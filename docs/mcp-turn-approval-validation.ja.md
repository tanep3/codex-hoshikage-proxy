# MCP単一Run限定許可 0.3：実装・検証記録

2026-09-16。基準は双方合意済みの[接続契約0.3](mcp-turn-approval-api.ja.md)。接続レビュー完了後に実装を再開した。**Proxy実装と下記の自動試験・隔離実Codex試験を実施。総合受入と常駐有効化は未完了。**

## 実装範囲

- approval_contextをResponseに固定し、利用者・会話・RunとProxy側のResponse／Turn／入力世代／設定世代を照合。
- 安定item IDで対応付けた操作詳細のGET、表示トークン付き単発返信、明示ターン許可、許可一覧、冪等取消。
- 単発Allowの非昇格、危険ツール・秘匿・未評価ツールの除外、内部フォームの非自動回答。
- 送信意思を保存してから上流へ返信。初回返信pending時の後続要求は確定を待ち、結果不明を再送しない。
- Steer前の入力世代更新、停止・取消・終端・期限・設定変更・再起動での適用禁止。
- 生引数は上限付きメモリで保持し期限に破棄。公開GETは既知秘密の伏字とJSON Pointerを返す。生引数を許可監査DBへ保存しない。
- 未回答16件／履歴256件／許可16件と応答サイズ上限を分離。全件一覧を黙って切り捨てない。

## 確認結果

| 対象 | 結果・範囲 |
| --- | --- |
| HTTPの5呼出し | 模擬Codexで、単発なら手動5回・grantなし。明示ターン許可なら手動1回・適用数5・終端失効 |
| 実Codex 0.153.4 / gpt-5.6-luna | 独立CODEX_HOMEと固定文字列だけを返すローカルMCPで5呼出し完了。明示許可1回、適用数5、Turn終端失効を確認 |
| 実browser_evaluateの対応付け | 先行実証でitem ID・引数・質問IDを照合。定数コードの要求をCancelで終了した。実ブラウザー操作成功とは扱わない |
| 設定・入力世代 | 設定変更→復帰でも旧トークンを拒否。Steer後の旧表示も拒否 |
| 競合 | 初回返信pendingを待つ。取消後の適用と遅延書込み確認で許可を復活させない |
| 同一性 | scope各要素の差分、他Threadの終了、同一call IDの引数差替え、満杯キャッシュ／上限超過の差替えを試験 |
| 永続化失敗 | SQLite triggerでSteerの状態保存失敗を注入し、世代更新のロールバックを確認。実HTTP Steerでも保存失敗時503・上流送信0件、正常時は世代更新・旧許可失効・上流送信1件を確認 |
| 互換性 | 機能無効のcapabilityと503応答、従来単発返信の継続。既存v1・v2自動試験も実施 |
| 並列・停止競合 | 独立2 Response間の許可非共有と停止分離。stop／取消と許可適用を別スレッドで競合させ、保存順に一致し、その後は適用不能 |
| 復元・通信障害 | 実行中の正式backup拒否、終端後に残った旧許可記録を含む正式復元で世代更新・失効。イベント欠落と模擬上流切断でも失効し、同一返信キーの照合で再送なし |
| 許可期限 | 壁時計後退でTTLが延長する問題を修正。単調時計を併用し、後退・前進後の後退・TTL境界・高頻度照会を制御時刻で確認。ホスト時計は変更していない |
| 引数キャッシュ容量 | 62,000 bytesの引数を256件保持し、先頭・末尾の対応を保全、257件目を拒否。単発測定2,960msであり性能SLOではない |
| 実ブラウザー比較 | 公開ランキングでbrowser_findを5回実行。単発5確認に対しターン許可1確認、5件取得・終端失効・次発言の再確認を確認。ページ遷移の単発確認1回は別計上 |
| 取消API | resource=mcp_grant、取消時の件数、同一キー再送とoperations/by-keyによる同一結果照会 |

主な試験ファイル：`tests/v2_mcp_grants.rs`、`tests/v2_http.rs`、`tests/v2_interactions.rs`、`tests/live_mcp_turn_grants.rs`、`tests/live_mcp_browser_grants.rs`。フィクスチャは `scripts/fixtures/turn_approval_mcp.py`。フィクスチャのfunction値をコード実行することはない。

実施コマンド：

```sh
cargo test --all-targets
cargo test --test v2_mcp_grants --test v2_http --test v2_interactions
cargo clippy --all-targets -- -D warnings
```

実モデル試験は明示実行のみ。既存の認証ファイルのパスを `CODEX_TEST_AUTH` に設定して実行する。秘密値をコマンドラインや記録へ書かない。

```sh
cargo test --test live_mcp_turn_grants -- --ignored --nocapture
```

## 2026-09-16：Proxy単独追加受入の結果

事前に記載した追加受入計画に従い、A03の独立Response、A06のHTTP Steerと保存障害、A07の正式復元・イベント欠落・上流切断、A08の容量と許可期限、A11の実ブラウザー比較を追加した。期限の修正は、先にシステム設計書の「期限境界の補完」へ単調時計の扱いを記載してから実装した。実行中のbackupは契約どおり拒否されるため、正式復元試験では終端済みResponseに残る旧許可記録を対象とした。

- `cargo test --all-targets`：成功。明示実行用のignored試験は別扱い。
- `tests/v2_mcp_grants.rs`：14件成功、`tests/v2_http.rs`：18件成功、`tests/v2_interactions.rs`：6件成功。
- 許可時計の回帰試験：成功。実ディスク枯渇やホスト時計変更は実施していない。
- `cargo clippy --all-targets -- -D warnings`、fmt、差分チェック：成功。

### 実ブラウザーの比較条件と限界

Codex 0.153.4 / gpt-5.6-lunaと登録済みplaywrightの接続先を使い、独立CODEX_HOME・隔離ストアで実施。常駐設定は変更していない。専用のbrowser_findは公開定義がtext／regexによる現在ページの検索であり、任意JavaScriptや保存先filenameを受け取らない。候補のbrowser_snapshotはfilenameを受け取り得るため、今回は許可対象に採用しなかった。

試験用カタログはbrowser_navigateとbrowser_findだけを公開し、Proxyの許可対象はbrowser_findだけとした。価格.comのノートパソコン人気売れ筋ランキングを固定URLで開き、上位5件を順に検索する同一指示を両条件で与えた。navigateはURL一致を照合して毎回個別確認する。

| 条件 | browser_find完了数 | 読取りの明示確認数 | navigateを含む明示確認総数 |
| --- | ---: | ---: | ---: |
| 今回だけ許可 | 5 | 5 | 6 |
| 明示ターン許可 | 5 | 1 | 2 |

両条件で回答に上位5商品名を含むことを検証。ターン許可は適用数5で終端失効し、同じCodex Threadへの次のユーザー発言では新たな確認が必要だった。最後の再実行は83.44秒で成功した。承認は試験ハーネスがAPIで返したもので、実Discordのクリックやカード数の測定ではない。

初回は別MCP接続のブラウザーがabout:blankで、準備側のページを参照できず失敗した。各試験接続自身が固定URLへnavigateする手順へ修正して再検証した。既存利用者のタブを操作せず、準備用タブと試験プロセスは終了した。

再実行には既存認証ファイルのパスをCODEX_TEST_AUTH、playwright接続設定を含むTOMLのパスをCODEX_TEST_MCP_CONFIGへ設定する。元ファイルは変更しない。外部サイトとモデルを利用し、ランキング期待値は2026-09-16時点なので再実行時にはページとの一致を確認する。

```sh
cargo test --test live_mcp_browser_grants -- --ignored --nocapture
```

この結果は専用読取りツールを指定した比較であり、全ツール公開時にモデルが自発的にこの経路を選ぶ保証ではない。browser_evaluateは常に個別確認を維持する。恒久的なallowlistへの追加や、他のツールの一括許可は行っていない。

## 残る総合受入と接続準備

Gatewayの実装・Mock受入と2026-09-16 18:49 JSTの常駐反映は、Gatewayの[実装記録](../../codex-hoshikage-gateway/docs/implementation-status.ja.md)で確認した。「Gateway実装待ち」は解消した。Proxyの今回の変更は常駐未反映、自動許可も未有効化であり、既存Proxy readyだけでは新機能の接続成功を意味しない。

- A01/A03/A06/A07：Proxy単独の上記制御試験は成功。実Gatewayを通す並列Run・Steer・stop・取消・通信切断／再起動で、UIと永続照会結果まで一致することを結合確認する。
- A08：上限・大引数キャッシュ・許可時計の境界は確認済み。実運用の同時大応答・長時間負荷・保存障害をすべて網羅した保証とはしない。
- A09/A10：実Discordの本人限定画面、原文操作内容、単発とターン許可の区別、公開カード・DB・ログへの追加漏洩防止、旧クライアントとの実接続互換性を確認する。上流の機能切替はプロセス全体へ影響するため、既存クライアントも対象とする。
- A11：実ブラウザー専用読取りの確認削減と次発言の再確認は上記条件で成功。実Discordでのカード集約・確認回数、通常公開カタログでのモデル選択経路は未確認。

結合試験時は対象Proxy版と設定世代、capabilityの両profile、許可対象のMCP server／toolを記録する。今回の実証に基づく候補はplaywright.browser_findのみ。evaluate／unsafeを許可対象へ追加しない。隔離試験での有効化と常駐自動許可の有効化を区別し、常駐有効化は総合受入と配備判断を経て行う。

### 2026-09-16 20:31 JST：接続試験用の常駐反映

利用者の明示指示でProxyを常駐反映し、本機能をON、対象をplaywright.browser_findに設定した。上記の未反映・未有効化は配備前時点の記録。現在は結合試験可能で、実Discord受入は未完了。[配備記録](server-operations.md)を参照。
