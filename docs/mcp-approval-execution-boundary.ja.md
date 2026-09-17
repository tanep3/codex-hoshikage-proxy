# MCP承認0.5 — 実行禁止を守る上流制御

> 2026-09-17 責務再整理中：[汎用基盤とポリシー拡張の改訂](mcp-approval-layering-revision.ja.md)を参照。全ツールの意味解析を汎用単発承認の前提とする記述、および追加禁止を標準API全体へ適用する記述は見直し対象。本書をそのまま新規実装へ使わない。既存0.5 wireの後継差分は未確定であり、合意済みとして扱わない。

2026-09-17。内部設計。対象版はCodex 0.153.4。製品への組込み・実モデル受入は未実施。

## 1. 確認した根拠

[公式設定仕様](https://learn.chatgpt.com/docs/config-file/config-reference)に、Appsと通常MCPのツール別approval_modeがある。設定キーの存在だけでは確実な停止を証明できないため、導入版と同じ公式ソースのtag rust-v0.153.4、commit 3d2ee51ca2d5db578f328aa75e20aa22c0197c9aを読み合わせた。

| ソース（codex-rs以下） | 確認した処理 |
| --- | --- |
| core/src/mcp_tool_call.rs: maybe_request_mcp_tool_approval | 承認要求より前に自動許可条件・承認不要条件・記憶済み許可を判定する |
| 同: requires_mcp_tool_approval_for_mode | promptは承認を必要とする。approveは不要 |
| 同: session_mcp_tool_approval_key | auto以外は記憶済み許可のキーを生成しない。promptは過去のsession/persistent許可を使わない |
| codex-mcp/src/mcp/mod.rs: mcp_permission_prompt_is_auto_approved | approve、またはneverと権限profileの組合せで承認要求を迂回し得る |
| connectors/src/app_tool_policy.rs | Appsのツール設定は正確なtool name、次にtitleで照合。managed設定は通常設定より優先 |
| core/src/connectors.rs: mcp_approvals_reviewer_from_layers | app/linkのreviewer、管理要件、モデル別自動審査要件が影響する |

Proxy現行のnative adapterはitem/tool/requestUserInputを入口とする。item/startedは通知であり実行を止める同期応答ではない。通知後のTurn停止だけで「実更新0回」を保証しない。

## 2. 制御対象と優先順位

本書は該当ポリシーを明示選択した実行だけに適用する。今回の必須対象は評価済みNotion接続のnotion.notion-update-page。更新方法を上流設定で区別できないため、このツール全体をProxyの検査経路に通し、command=update_contentだけ実行不可とする。他のcommandは各詳細設計の許可条件で判定する。ページ更新全体を無効にして部分置換対応完了とはしない。

ポリシー適用実行に割り当てるProxy専用の実効設定へツール別promptを重ねる。非選択実行と設定・runtimeを無条件に共有しない。正確なconnector IDとtool nameを検証済みカタログから結び付け、titleの曖昧一致を使わない。利用者のグローバル設定ファイルを変更しない。明示的なenabled=falseは維持し、制限を適用するために有効化しない。通常MCP経由の同機能を登録する場合は、その接続・定義も別に評価し、Apps用キーを流用しない。

実効条件はpromptだけでなく、approvalPolicy=on-request、対象のapprovals reviewer=user、native call ID付き承認経路が有効であること。app/linkの上書き・管理要件・モデル別自動審査・プロジェクト設定を含めて確認する。満たせない場合は当該runtimeで新しい実行を開始しない。古い安全な設定で動作していると推測しない。

この強制確認は「利用者に毎回ボタンを押させる」指定ではない。Proxyが毎回callを検証できる指定であり、通常操作の有効な依頼中許可はProxyが適用する。実行不可分岐には、単発許可・常時許可・以前のgrantより優先する規則を適用する。他ツールの常時許可設定を一括で書き換えない。

## 3. 起動・継続・設定更新

1. 元設定を読む→既存の継承処理→Proxy所有の制御設定を合成する。元設定と実効設定を別に保持する。
2. 設定・接続・policyの根拠から実効世代を作る。秘密値や生設定をAPIへ返さない。
3. 新規Threadだけでなくresume／次Turnの前にも実効条件を検証する。旧Threadに保存された条件を信頼して省略しない。
4. MCP接続設定に加えてAppsと承認制御設定の差分も検知する。現行McpRefreshのMCP_KEYSだけではApps差分が含まれないため、そのまま流用しない。
5. 上流反映の応答と読戻しが完了した後だけ実行する。反映結果不明は新規実行不可。動作中のTurnへ意味の違う設定を継ぎ足さず、次Turnへ反映する。停止・世代失効は即時の既存契約に従う。
6. 設定読戻しの値と実承認経路の証明は別。対象版・モデルの受入で要求が届くことを確認し、上流版変更は再評価する。

## 4. 拒否と表示

対応付けできたupdate_contentは有効な許可tokenを作らない。presentationは既定のunavailableと固定理由を返す。利用者の拒否／停止または既存の期限処理で上流待機を解消し、許可応答は送らない。Proxyの制限を利用者が拒否したという監査記録へすり替えない。

旧profileの単発返信、直接API返信、既存grantの後続適用でも、送信意思を保存する前に同じ禁止規則を評価する。表示profileは実行制限の有無を選ぶスイッチではない。標準OpenAI互換経路や他クライアントの非選択実行には対象上流設定を適用しない。禁止判定は実行に固定されたポリシーを根拠に行い、表示profileや返信経路の変更では外せない。対応付け不能なら許可を推測して返さない。

実行禁止の保証対象は、このProxyが管理するCodexから対象MCPツールを呼ぶ経路。別アプリからの直接更新、任意コードで直接Notion APIを呼ぶ行為まで防ぐネットワーク隔離の保証とは区別する。Proxyが禁止操作を別ツールへ自動変換・代行してはいけない。

## 5. 実装後の受入

| ID | 条件と期待値 |
| --- | --- |
| EB-01 | 元設定auto/approve、app全体・tool・linkの設定が混在しても、対象の実効値prompt/userを確認 |
| EB-02 | 同一Threadで上流に過去の許可がある場合も新callの承認要求がProxyへ届く |
| EB-03 | never・自動reviewer・管理設定との不整合ではTurn開始0回。設定保存成功だけで合格にしない |
| EB-04 | 更新方法だけ変えた連続callで、許可済み通常操作は重複確認なし、update_contentの上流許可0回 |
| EB-05 | 適用実行では旧画面・別返信経路で迂回不可。非選択実行では標準互換経路・他クライアントへ制限が漏れない |
| EB-06 | Apps設定変更、reload失敗・timeout、resume、再起動、モデル／上流版変更を確認 |
| EB-07 | 対象外ツールの常時許可、明示無効、グローバル設定の原本が変わらない |

NR-01〜07と組み合わせ、架空のMCPでの制御試験、実Codexの承認到達、専用テスト対象の更新有無を別々に記録する。本書のソース確認を実行試験成功として数えない。

責務整理合意後の接続先は[API 0.6レビュー案](mcp-approval-api-v06.ja.md)。本書の技術的知見は引き継ぐが、単発承認への意味評価必須条件・全実行への禁止波及・全342件の着手ゲートは後継要件に従って置き換える。
