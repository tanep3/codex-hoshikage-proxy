# API 0.6：確認待ち時間の不具合修正

2026-09-17。Gatewayの `proxy-mcp-v06-review-delay-fix.ja.md` へのProxy回答。

## 原因と責任

Proxyの不具合。30秒TTL後のカタログ再取得が250ms以内に終わらない場合、表示処理が更新中を意味未評価・公開可否未分類として扱っていた。private_requiredのdiagnostic、表示証拠と、unavailableの期限フィールドにも接続型との不整合があった。即時押下・ready中心の前回試験では、人間が考える時間と非ready分岐の確認が不足していた。

## 修正契約

[API 0.6第14節](mcp-approval-api-v06.ja.md)を参照。新しいprofileやGateway専用の処理は追加しない。

- 更新中：unavailable / catalog_loading、retryable=true、retry_after_ms=2000、actions.retry=true。既存表示証拠を変更しない。
- 取得失敗：unavailable / catalog_failed。同じく再照会可能。個人情報の判定に置き換えない。
- 同一定義への正常更新：既存の表示ID・fingerprint・page tokenを維持する。表示判定には一つのカタログsnapshotを使用し、判定途中のTTL境界で表示が変わることも防ぐ。
- 許可POST自身でも再取得を開始する。更新中／失敗は409 catalog_loading／catalog_failedで許可未送信。拒否と既に受付済みの照会は更新不要。
- private_requiredは秘密入力を含まない入口1ページを返し、表示ID・fingerprint・tokenとreason/diagnosticを整合させる。入口からの許可は不可。
- unavailableはpresentation_id／presentation_fingerprint／page／expires_atを明示的nullで返す。

## Gatewayへの接続事項

更新中のGETを2秒以降に再照会して待機表示を更新できる。利用者に理由不明の連打を求めない。許可POSTは自動再送しない。内容・権限変更は最新の画面を明示的に再確認する。単発許可への自動変更、AIの再実行、引数の省略で補わない。

Discordでの画面操作を含む統合試験は、利用者指定どおりGateway担当。Proxyの型照合や実Codex試験を、実Discordの統合試験完了とは扱わない。

## 検証結果

- 35秒／90秒×単発／依頼中許可の実時間HTTP試験4通りに成功（カタログ応答を900ms遅延）。更新中のunavailableと、正常更新後の元表示との完全一致を確認。
- 許可POSTだけで再取得を開始する経路、取得失敗、定義変更、停止との競合、旧証拠から許可未送信、カタログ取得失敗中の拒否に成功。
- 実Codex／gpt-5.6-lunaと隔離した読み取り専用MCPで、35秒後の依頼中許可（5呼出し）、次の依頼で90秒後の単発許可（1呼出し）に成功。証拠：`/tmp/v06-live-43088c99-8bec-4b7b-a341-a6f48b2c4966`。
- 現行Gatewayの `mcp_v06::Presentation::parse` と `render_page` を直接使い、初めからprivate_required、catalog_loading、catalog_failed、定義変更後private_required、実Codexのready 2件を検証。全て成功。取得不能時のexpires_atはキー欠落でなく明示nullであることも確認。
- 全target回帰試験、修正後のv06 HTTP試験、Clippy `-D warnings`、fmt、releaseビルドに成功。試験fixtureの停止時JSON-RPC errorも正しく扱うよう修正。

[非ready応答の実測例](mcp-approval/review-delay-examples.json)。テスト用の識別子を含み、実利用者の秘密入力は含まない。実Discordの再接続受入はGateway側で実施する。

## 常駐反映

2026-09-17 20:52 JST、修正コミット `5b17f9c` を反映済み。停止前後に活動依頼がないことを確認し、バックアップを取得した。認証・設定・保存回答・API互換性、実Codex回答保存と重複抑止を確認。詳細は[運用記録](server-operations.md)。
