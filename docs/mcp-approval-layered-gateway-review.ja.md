# 責務分離後のGateway開発文書 — Proxy側レビュー

2026-09-17。判定：**責務分担・開発工程はGo。方向性の差し戻し事項なし。** 具体API 0.6の接続合意、DB詳細設計完成、製品実装・実機受入を意味しない。

## 対象

- Gateway [内部設計1.0](../../codex-hoshikage-gateway/docs/mcp-approval-layered-gateway-design.ja.md)
- Gateway [要件2.8](../../codex-hoshikage-gateway/docs/requirements.ja.md)：責務、MCP承認、LR-G01〜06、旧仕様の適用範囲
- Gateway [システム設計1.8](../../codex-hoshikage-gateway/docs/system-design.ja.md)：承認・保存・復旧、責務分離後の内部構成
- Proxy [責務再整理](mcp-approval-layering-revision.ja.md)、[API 0.6レビュー案](mcp-approval-api-v06.ja.md)

Gateway側の「具体契約待ち」は、本文の論理型・DB項目を未確定として扱う意味で読む。API 0.6を既に受け入れたとは扱わない。旧0.5の記述は冒頭の適用範囲指定と後継節により、後継の既定仕様ではなく互換・履歴として区別されている。

## 確認結果

| 項目 | Gatewayの根拠 | 判定 |
| --- | --- | --- |
| 表示先／単発／依頼中許可の独立 | 内部設計§1、ExecutionBinding／ApprovalEligibility、LR-G01〜03 | 一致。意味未評価を単発不可へ一括変換しない |
| 実引数の忠実性と説明の分離 | 内部設計§2〜3、LR-G02 | 一致。全構造・型・値、未指定とnull、数値の精度、未評価説明を区別 |
| 公開可否と実行禁止の独立 | 内部設計§1・3 | 一致。本人向け表示は承認でも安全性評価でもない |
| policyの明示選択と非波及 | 内部設計§4、LR-G03〜05 | 一致。表示指定からの自動選択なし、未選択実行への禁止漏れなし |
| 実効設定・会話継続の所有 | 内部設計§4、LR-G04〜05 | 一致。Proxyが担当し、Gatewayが新会話やpolicy省略再送で代用しない |
| 拒否の独立、配信・復旧 | 内部設計§5、UI-08・10・12 | 一致。許可用の全ページ検証を拒否へ要求しない |
| DBの版別解釈 | 内部設計§2・4、システム設計の後継構成 | 一致。旧行を新profileへ変換せず、未選択の世代を捏造しない |
| 着手条件 | 内部設計§6、LR-G06 | 一致。342件完全解析待ちを外し、機能単位の設計・契約・受入を先行 |

GatewayへProxyの意味解析や実行制御を移す記述、ProxyへDiscord固有の本人確認・UI処理を押し込む新たな要求は確認しなかった。今回の範囲で、修正必須の方針不整合はない。

## 0.6接続レビュー後に具体化する項目

以下は差し戻しではなく、内部設計§7が明示的に保留している項目への対応表。0.6が修正された場合は合意した最終版に合わせる。

1. **完全表示のデータ経路**（内部設計§2〜3、0.6§5・8）
   DisplaySnapshotはProxyのpresentation.fields／全ページを正本とする。Gatewayがoperation.argumentsを再解釈して別の公開説明を作らない。arguments_delivery=presentation_pagesのarguments=nullは取得失敗・引数なしではない。表示データ内で保持された型や階層を、Gatewayが再構成して値を丸めない。本文はメモリ内のみとし、DBには表示証跡を保存する。
2. **要求と実効結果**（内部設計§2・4、0.6§2〜4・7）
   PolicySelectionとEffectivePolicyに、selection、binding_id、generation、preparing/ready/failed/closedを対応付ける。202を適用完了としない。未選択のgeneration=null、scopeのnullable世代、許可時expected_policy_binding_idを版別に保存・照合する。拒否はこの追加照合に依存させない。
3. **運用者による強制選択の扱い**（内部設計§2・7、0.6末尾補足）
   Gatewayの論理型が将来の強制規則を見込むことは問題ない。ただし初期0.6は、未指定実行へ運用者が新policyを強制選択する機能を含めない。現在の実装で未知の強制規則を推測したり、未選択を強制選択済みと表示したりしない。将来追加する際に契約をレビューする。
4. **初期選択・マイグレーション**（内部設計§4・6、0.6§2・8・10）
   初期選択はevaluated-turn-notion-guard/version=1という0.6案を照合する。profile=source-conversation-v3とpolicy選択を別保存。未適用schema9案は後継の一つのmigrationへ整理し、既存schema8履歴は保持する。汎用表示・単発の利用可否を依頼中許可enabledだけで止めない。

## レビューの範囲と次工程

文書レビューのみ。Gatewayのコード・DB・サービスは変更していない。受入ケースを実行した記録でもない。Gateway側がAPI 0.6をレビュー中であることを尊重し、本記録をそのレビューの代わりにしない。

双方の具体契約の照合後、Gatewayは型・DB・表示／返信・復旧、Proxyは実行binding・汎用表示・policy設定の分離を詳細化する。文書が完成した機能単位で実装へ進む。
