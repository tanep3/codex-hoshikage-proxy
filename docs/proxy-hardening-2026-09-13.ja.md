# 2026-09-13 Proxy追加修正・受入記録

状態：実装・ローカル検証完了。常駐サービスには未反映。本書の変更は、2026-09-13に確認済みのDiscord画像表示に対する追加の安定化である。

## 修正内容

- 質問・MCP elicitationなどのserver requestを無視して待機する経路を修正。権限専用承認へ通常承認の `decision` を送る誤った処理も修正した。未対応メソッドには同じRPC IDで `-32601` を返し、`unsupported_interaction` を対象Thread/Turnに通知する。回答や権限許可は捏造しない。
- v1のResponses／Chat Completions、ストリーム／非ストリームの各経路で対話不能を通知し、対象Turnの停止を要求する。v2は停止意思・理由を永続化し、実際の終端状態を確認してから占有を解放する。停止結果不明を停止成功と扱わず、同一要求を自動再実行しない。
- `serverRequest/resolved` を受け取った通常承認は失効させ、古いUIからの許可を拒否する。Thread IDとRPC IDを両方照合する。
- コマンドの作業ディレクトリがワーク内でも、`networkApprovalContext`、`additionalPermissions`、`proposedExecpolicyAmendment` がある場合はワーク内自動承認を適用しない。
- クライアント定義ツールの指定や呼出し履歴を黙って無視する経路を修正。未対応の指定はCodex起動前に400 `unsupported_parameter`で拒否する。これはクライアント定義Function Callingの実装完了を意味しない。
- 要件・設計書の「SQLite未実装」と、英日READMEの範囲を限定しない「最終出力を再取得できない」を修正。現行v2と旧v1の提供範囲を区別した。

参照：[公式App Server仕様](https://learn.chatgpt.com/docs/app-server)、Codex CLI 0.153.4から生成した `ToolRequestUserInputResponse`、`PermissionsRequestApprovalResponse`、`McpServerElicitationRequestResponse` のJSON Schema。質問・権限・MCPの応答形状を通常承認と共通化しない。

## 検証内容

- `tests/http_integration.rs`：4種類の未対応server request × Responses／Chat × stream／non-streamの16通りで、idle timeoutを待たずエラーが返る。上流へは許可や回答ではなくJSON-RPCエラーを返す。クライアントツール指定は実行前に拒否する。
- `tests/runtime_integration.rs`：上流で解消された承認への古い回答を拒否する。
- `tests/v2_http.rs`：対話不能で停止意思が記録され、終端確認後に占有を解放し、同じ要求キーの再送が同じResponseを指す。
- `tests/v2_crash.rs`：受付保存後、上流送信意思保存後、成果物予約後、本体・manifest公開後、実ファイル書込失敗後の5境界で、隔離プロセスを強制終了する。未commitのSQLite変更のrollback、ID・復元世代維持、要求キーの重複防止、元ファイル再コピー禁止、公開済み成果物の復旧を確認する。書込失敗は子プロセスだけの `RLIMIT_FSIZE` で発生させ、共有ディスクを埋めない。これはENOSPCやfsync失敗そのものの試験ではない。
- `tests/v2_store.rs`：配信枠を使い切った際の429、本文読取途中の切断による枠解放、Range再開による同一バイト列の復元、原本更新の不変成果物への非伝播、読取参照の解放を確認する。HTTPボディ層の試験であり、別PC・低速LANの実通信試験ではない。

実行結果：

- `cargo test --locked --all-targets --quiet`：123件成功、失敗0件、3件ignored。ignoredの内訳は容量測定、私有画像fixture、強制終了試験の子プロセス専用fixture。最後のfixtureは親試験から5回明示起動して確認済み。
- `cargo clippy --locked --all-targets -- -D warnings`：成功。
- `cargo fmt --all -- --check`、`git diff --check`、変更したMarkdownの相対リンク検査：成功。
- PIPEの8件も成功。通常のシステムPythonにはhttpxがなかったため、`uv run --no-project --cache-dir /tmp/hoshikage-test-uv-cache --with httpx --with pydantic --with pillow python -m unittest discover -s tests -p test_openwebui_pipe.py -q` の隔離環境で実施。模擬App Serverを使った実Proxy HTTP承認往復を含む。稼働中のOpenWebUI・Gateway・Proxyは変更していない。

上記の初回修正では新たな有料モデル実行、実Discord投稿、常駐サービス再起動は実施していない。続く対話中継の実モデル試験は下記を参照。

## 残る作業

- Gateway側の質問・MCP・権限専用承認UIと認可の連携。Proxy側は[追加API契約](interaction-api.ja.md)を実装済み。実行要求ごとの対応宣言が必要で、未宣言の既存クライアントの動作は維持する。
- クライアント定義Function Callingと、正確なリクエスト単位usageの変換。Codex内部のツール実行を外部クライアントへの実行依頼に誤変換せず、Thread累計や最後のモデル呼出しの値をTurn全体の使用量と推測しない。
- ディスク型ファイルシステム／実デバイス固有の障害、長時間の混合負荷、別ホストの切断・Range再開、Gatewayと連動する正式復元・配信結果不明の結合試験。
- 容量・保持既定値の最終合意、他のモデル／Codex版の受入。

v2全体の `acceptance_pending` は維持する。既存の正常系画像表示の受入を取り消すものではない。

## 同日追加：v2対話中継

質問、MCPフォーム／URL確認、権限要求を対応宣言したv2クライアントへ中継する。要求はResponse・Thread・Turnへ紐付け、回答期限・revision・操作キーを照合する。送信意思を先に保存し、送信結果不明の回答を再送しない。回答本文は永続化せず、解消した要求本文も破棄する。停止・期限切れ・上流での解消・イベント欠落・再起動で古いUIを失効させる。

模擬上流とSQLite試験では、回答スキーマ、権限の拡大禁止、二重回答の競合、同一キーの再取得、旧復元世代の拒否、停止後・期限切れの回答拒否、モデルidle timeoutとの分離、再起動後の送信結果不明の維持を確認した。

隔離Proxyと実Codex（gpt-5.6-luna）、テスト専用のローカルMCPで、MCPツール実行承認→フォーム回答→最終出力 `ELICITATION_ACCEPTED` まで成功した。最初の2回は試験側がフォーム前のツール実行承認を想定しておらず中止し、二段階を明示的に照合する試験へ修正後に成功した。これは質問・URL・権限要求すべての実モデル受入を意味しない。

常駐Proxy・Gateway・OpenWebUIは変更していない。対話UIのクライアント対応と配備後の結合受入が必要。基本契約2.0と `acceptance_pending` を維持する。

追加後の自動試験：Rust全対象132件成功・3件ignored、その後追加した開始前受付／期限切れrevisionの回帰試験を含む対話6件も成功（現在の成功対象は計133件）。Clippyの警告0、fmt・差分空白検査成功。PIPE既存8件も成功。HTTP切断直後の回答送信継続は、下記の実TCP切断試験でも確認した。


## 同日追加：回答送信中の実TCP切断

`tests/v2_http.rs::tcp_disconnect_during_answer_write_does_not_lose_or_resend_reply` を追加。Linuxの模擬App Serverの標準入力パイプを4096 bytesに制限し、8192 bytesの回答を送ることで上流書込みを確実に待たせる。永続状態が `sending` になってからHTTPクライアントをTCP RSTで切断し、切断後も書込み待ちであることを確認した上で上流の読取りを再開する。

回答は1回だけ上流に届き、実行がfinished、対話がresolved、送信がwrittenになる。同じ操作キーでの再要求はsucceededの元operationを返し、再送しない。単なるHTTPボディ切断やレスポンス受信後の切断とは別に、送信処理中の切断を検証した。隔離したloopback通信と模擬上流の試験であり、実Discord・別ホストの不安定回線・実Codexの障害試験ではない。常駐サービスは変更していない。

この追加後の `cargo test --locked --all-targets --quiet` は134件成功・失敗0件・3件ignored。Clippy（`-D warnings`）、fmt、差分空白検査も成功。


## 同日追加：保存同期失敗を復旧成功と誤認する不具合

障害注入で、manifestのfsyncが失敗し続けていても、復旧時のハッシュ一致だけで成果物がreadyになる不具合を再現した。修正前の `sync_manifest` 条件では、取得不可を期待する試験が実際にreadyとなって失敗した。ページキャッシュから読めることだけでは保存完了の根拠にならない。

`src/v2/recovery.rs` を修正し、保存済みバイト列の検証後、本体・manifest・保存先ディレクトリの同期がすべて成功してから公開する。同期失敗後に残るunknown成果物は、manifestがある場合に限り次回起動でも同じ保存物から復旧する。内容の検証後に起きた同期エラーは恒久的な破損と区別する。復旧成功時は操作に残った過去のエラーも解消する。回答保存も同じ復旧処理を通る。

`tests/v2_io_faults.rs` はLinuxの試験用子プロセスに限ってwriteのENOSPC／fsyncのEIOを注入する。stagingへのwrite、stagingのfsync、manifestのfsync、rename後の保存先ディレクトリfsyncの4条件を確認。注入された呼出しに実際に到達したことも検査する。保存時に成功を返さず、障害継続中のストア再起動でも公開せず、障害解消後は原本を再取得せず同じID・保存バイト列へ復旧する。既にreadyだった成果物も一時的な同期障害後に復旧できる。既存の強制終了・保存物検証試験も成功した。

これは実際にファイルシステムを満杯にした試験や電源断試験ではない。SQLite障害、実ディスク容量枯渇、別ホストの不安定回線、Gateway障害結合は引き続き未完了。常駐サービスは変更していない。

今回の全対象試験は135件成功・失敗0件・4件ignored。新しいignoredは親試験から4条件で明示起動する障害注入fixture。Clippy（`-D warnings`）、fmt、差分空白検査、変更Markdownの相対リンク検査も成功した。


## 同日追加：実容量不足とSQLite障害

`python3 scripts/test_v2_full_fs.py` で、専用のユーザー・マウント名前空間に容量16 MiBのtmpfsを作り、実際のwriteでENOSPCになるまで埋めた。成果物保存が成功と誤認されないこと、SQLiteが実際にSQLITE_FULLを返すことを確認した。空きを戻してストアを再起動しても、インスタンスID・復元世代・要求キーは維持され、DBのintegrity_checkはok。未commitの操作と大容量INSERTはrollbackされ、送信意思まで記録した実行はUNKNOWNとなって再送されない。失敗した成果物の同一キー再要求でも変更後の原本を再コピーしない。

試験環境はメモリ512 MiB・CPU時間30秒・実行時間60秒・ファイルシステム16 MiBに制限した。子のマウント名前空間が親と異なること、実際のファイルシステム種別・容量を検査してから実行し、終了後に親の一時ディレクトリが空であることを確認。常駐サービス・本番DB・ホストのマウントは変更していない。これはtmpfsの実容量不足であり、ext4ディスクイメージや物理ディスクを埋める試験ではない。

さらに `tests/v2_io_faults.rs` の注入条件を6件に拡張し、SQLite WALへのpwrite／同期のEIOを確認した。操作キーと関連レコードの更新が一部だけ残らないこと、再起動後のintegrity_check、保存済みID、UNKNOWNの再送禁止を検査した。同期エラーでcommit結果が不明になる可能性は保持し、「エラーなら必ず未commit」とは仮定しない。注入対象は試験プロセス・試験用WALのみ。

今回、新たな本体コードの不具合は見つからなかった。追加試験と再実行手順を保存した。長時間混合負荷、Gatewayの障害結合、ディスク型ファイルシステム固有の障害・電源断は引き続き別の受入項目とする。

今回の検証結果：通常の全対象試験135件成功・失敗0件・5件ignored。容量不足の専用試験1件（成果物／SQLiteの2条件）はスクリプトから明示実行して成功。6条件の障害注入fixtureも親試験経由で成功。Clippy（`-D warnings`）、fmt、差分空白検査も成功した。
