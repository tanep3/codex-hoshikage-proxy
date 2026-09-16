# MCPインライン承認 0.4：実装・検証記録

2026-09-16。[合意契約0.4](mcp-inline-approval-api.ja.md)とGatewayのR-01解消・接続レビュー完了を確認し、利用者の実装開始指示を受けて着手。文書完成・接続合意後にコードを変更した。**2026-09-17 04:42 JSTに常駐反映済み。実Discord結合受入は未完了。**

## 実装

- Response受付のapproval_presentationとcapability、専用presentation GET。
- 検索語・regex・HTTP(S) URL・タブ一覧の公開renderer。元のoperation.argumentsはrequester_onlyを維持。
- 上流catalogの入力schemaと設定世代を照合。同名の他サーバーや未知定義は推測しない。catalog失敗・変更→復帰でも旧表示を復活させない。
- 認証候補・userinfo・署名URL・秘密形式・曖昧なencoding・実行コード・未知引数・長文を補足へ。URL/検索語に秘密がある場合は対象値全体を公開しない。
- 公開表示・action・audience・scope・期限に拘束したランダム256bit token。本文の識別はプロセス専用鍵のHMAC-SHA256。DBに本文・実引数を追加保存しない。
- 同内容の再GETは同じ表示版、変更時は旧版失効、最大4版。期限は元callとinteraction以内で更新しない。
- 表示照合から返信意思保存までをDB transactionで順序付け、上流callキャッシュの変更も同じ承認処理のロックで直列化。上流送信はロック解放後に実施。既存キーの結果照会は失効後も再送しない。
- 0.3互換、復元世代、workspace認可、停止・Steer・再起動・上流接続喪失による失効を継承。上流で常時許可されるツールへ承認要求を新設しない。

## 検証範囲

`tests/v2_presentations.rs` は通常の検索・regex・URL・list、秘密混在、未知schema・キー、Unicode長文、別call／別scope、表示版上限、定義変更→復帰、同一callの引数変更、保存障害、上流喪失、DB再open、期限を対象とする。

`tests/v2_http.rs` はHTTPのResponse宣言→presentation→不正token拒否→明示ターン許可→5呼出し完了→失効後の同一キー／operation照会を追加。単発の非昇格・停止・Steer・返信結果不明などの既存回帰試験も維持する。

実Codexのcatalog試験：独立CODEX_HOMEに既存playwrightの接続設定をコピーし、元設定の承認方針を変更せず、thread/startとmcpServerStatus/listで3ツールの定義を確認。アダプタが実schemaを受け付けることを確認した。**AI Turn・ブラウザー操作・Discord投稿は行っていない。** この試験の表示生成には隔離フィクスチャのcallを使い、実呼出しの承認完了とは報告しない。

実catalog試験は成功（3.60秒）。一時homeではPATH helper生成とプロジェクト信頼設定に関するCodex警告が出たが、catalog取得・照合は完了。常駐設定・利用者のブラウザータブは操作していない。

```sh
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
# CODEX_TEST_AUTHとCODEX_TEST_MCP_CONFIGには既存ファイルのパスだけを設定する。
cargo test --test v2_presentations live_catalog_matches_evaluated_browser_renderers -- --ignored --nocapture
```

## 自動試験の結果

- 全target回帰試験：成功。明示実行用のignoredは別扱い。
- 最終差分後の関連試験54件：成功（HTTP 20、interaction 6、turn grant 14、presentation 14）。新規catalog実試験は通常実行ではignored、上記の明示実行で別途成功。
- 最終補完は、workspace認可のtransaction内再確認、表示台帳の破損を未登録として上書きしない処理、機能OFF後も受付済み要求の同一キー照会を維持する処理。
- 最初のHTTP試験で受付フィールドの許容リストへの追加漏れを検出し、修正後にHTTP 20件が成功。無効な表示tokenでは上流送信なし、その後の正しい承認で5件だけ送信することを確認した。
- Clippy全target（警告をエラー化）・fmt・差分チェック：成功。実Discordの表示・押下確認は含めない。

## 残る結合受入

- Gatewayの0.4実装と接続し、検索・URLの最初のカードからの本人による実操作、2択／3択、補足、取消と次発言の失効を確認する。
- Discordの未確定配信・重複押下・古いカード・別会話・Gateway再起動と照合復旧。Proxy側のHTTP試験で代替しない。
- 実上流callの動作と表示内容が一致すること。特定のサイト／タブへの固定を保証する機能ではない。
- 任意の自由文中の未識別・独自形式の秘密まで完全検出できるとは保証しない。公開を許可した操作対象は元会話の閲覧者にも見える。

常駐反映・新機能の接続受入完了・コミット／pushはこの実装記録だけでは宣言しない。

## 常駐反映

利用者指示でbc28861を配備し、新capability有効・新Response宣言受付・既存API互換を確認。[配備記録](server-operations.md)参照。Gateway完成の連絡を受領し、実Discord結合試験へ進める状態。試験投稿やAI実行は今回行っていない。
