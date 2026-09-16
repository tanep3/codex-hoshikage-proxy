# v2対話要求の取得・回答API

2026-09-13、追加契約0.1。**Proxy実装済み・受入検証中。GatewayのUI対応・結合受入は未完了。** 2026-09-13 08:40 JSTに[常駐反映済み](server-operations.md)。基本契約の `contract_version: 2.0` を維持する後方互換の拡張。OpenAI互換v1の要求形式は変更しない。

## 責務と提供判定

Proxyは対話要求を受信し、保存済みResponse・会話・ワーク・Thread・Turnへ帰属を確定し、質問・期限・回答検証・送信意思・送信状態を管理する。GatewayはDiscordでの表示、操作者の本人確認・権限判定、操作キーの永続保存を担当する。ProxyのAPIキーは共有運用者権限であり、Discord利用者間の分離はGatewayが実施する。

`GET /v2/codex/capabilities`：

```json
{
  "features":{"interaction_relay":true},
  "interaction_kinds":["user_input","mcp_form","mcp_url","permissions"],
  "interaction_limits":{
    "max_count":16,"max_bytes":65536,"timeout_seconds":600,
    "schema_profile":"flat-primitives-v1","permission_profile":"whole-category-v1"
  }
}
```

このCapabilityは表示できるGatewayが既に配備されているという意味ではない。Gatewayは対応した種類だけを**実行要求ごとに**宣言する。

```http
POST /v2/codex/conversations/conv_example/responses
Authorization: Bearer <operator-key>
X-Proxy-Instance-Id: pxy_example
X-Proxy-Recovery-Generation: gen_example
Idempotency-Key: discord-request-123
Content-Type: application/json

{"input":"作業の依頼","interaction_capabilities":["user_input","mcp_form"]}
```

省略時は空配列。未知の種別・重複・文字列などの不正な指定は400。通常のコマンド／ファイル承認には既存の `metadata["codex.approval_capability"]` を使う。未宣言または検証不能な対話要求は `unsupported_interaction` と対象Turnの停止で扱い、質問や許可を捏造しない。

## 要求一覧と詳細

- `GET /v2/codex/responses/{response_id}/interactions`
- `GET /v2/codex/interactions/{interaction_id}`

基本契約と同じBearer認証・instance・復元世代・ワークのアクセス制御を適用する。要求IDはProxyが確定し、上流RPC IDをクライアントへ公開しない。最大16件の一覧は取得時点のスナップショットであり、ページングしない。取得直後に状態が変わり得るため、回答時にrevisionと実行状態を再検証する。

```json
{
  "response_id":"resp_example",
  "data":[{
    "interaction_id":"int_example",
    "response_id":"resp_example",
    "conversation_id":"conv_example",
    "workspace_id":"ws_example",
    "kind":"user_input",
    "state":"pending",
    "revision":1,
    "created_at":"2026-09-13T01:00:00Z",
    "expires_at":"2026-09-13T01:10:00Z",
    "reply_status":"not_sent",
    "error":null,
    "request":{"itemId":"item_example","questions":[{
      "id":"color","header":"色","question":"どの色にしますか？",
      "isOther":true,"isSecret":false,"options":[{"label":"青","description":"青を使用"}]
    }]}
  }]
}
```

Gatewayは生成中に定期照会する。推奨間隔は1秒。既存Responseイベントの `response.interactions_changed` は `{response_id, revision}` のみを通知する補助であり、欠落・再起動後は必ずGETへ戻る。公開イベントに質問や回答を入れない。回答待ちはモデルのidle timeoutとは区別するが、下記の有限期限は維持する。

## 回答

```http
POST /v2/codex/interactions/int_example/reply
Authorization: Bearer <operator-key>
X-Proxy-Instance-Id: pxy_example
X-Proxy-Recovery-Generation: gen_example
Idempotency-Key: discord-answer-456
Content-Type: application/json

{"expected_revision":1,"response":{"answers":{"color":{"answers":["青"]}}}}
```

応答は202と既存形式のoperationを返す。operationの `resource.type` は `interaction`。`state: succeeded` は上流接続への書込み完了を意味し、AI実行成功や上流の処理成功を意味しない。詳細はGETで照会する。タイムアウト・書込み成否不明はoperationと対話の状態を `unknown` にし、自動再送しない。

同じ操作キー・同じ本文の再送は元operationを返す。異なる本文・別対象へのキー流用は409 `idempotency_conflict`。別キーで二重に回答した場合も、最初の送信意思だけを受理する。HTTP接続が切れても受理した回答処理を破棄しない。送信開始後にProxyが停止した場合、その回答は再起動後に再送しない。

### user_input

`answers` は全質問IDへの回答マップ。質問は1〜3件、回答は各IDにつき非空文字列1件、1件最大8192 UTF-8 bytes。質問に選択肢があり `isOther` がtrueでなければ、選択肢のlabelとの一致を要求する。存在しないID、必須回答不足、配列の複数回答、未知フィールドは拒否する。選択肢は最大32件。秘密回答はGatewayでも非公開入力として扱う。回答せず実行を止める場合は既存の要求ID停止APIを使う。

### mcp_form / mcp_url

```json
{"action":"accept","content":{"approved":true}}
```

フォームの `request` には `serverName`・`message`・`requestedSchema` がある。対応profileは平坦なobjectで、最大32プロパティ、string／integer／number／boolean、required、enum（最大64件）、enumNames、minimum／maximum、minLength／maxLength、title／description／default。未知キーワード、リモート参照、ネスト、配列、pattern、format、複合schema、OpenAI拡張formは未対応として要求を拒否する。未知プロパティの回答も拒否する。数値は±(2^53−1)以内、文字列は8192 UTF-8 bytes以内。文字数制約はUnicodeスカラー値数で検証する。defaultは自動回答として使用しない。

URL確認には `serverName`・`message`・`url`・`elicitationId` がある。http/httpsのURLだけを扱い、資格情報を埋め込んだURLは拒否する。ProxyはURLを取得しない。Gatewayは利用者へ確認内容・接続先を表示する。URL確認のacceptは `content:null`。両モードとも辞退・取消は `{"action":"decline","content":null}` または `{"action":"cancel","content":null}`。

### permissions

```json
{"permissions":{"network":{"enabled":true}},"scope":"turn"}
```

`request.permissions` のnetwork・fileSystemを、それぞれ**カテゴリ全体のまま許可するか省略するか**を選ぶ。空objectは権限を与えない応答。要求外の追加・カテゴリ内部の書換えは拒否する。特にfileSystemのdenyを削除したり、globの範囲を変えたりしない。scope省略はturn、sessionは明示指定のみ。`strictAutoReview` はtrueの指定だけを受理する。

fileSystemは旧read/write絶対パス配列、およびentries（read/write/deny、path/glob_pattern/special）を扱う。specialはroot/minimal/project_roots/tmpdir/slash_tmp、project_rootsのsubpathを保持する。未知のspecialは拒否する。read/write・entriesはそれぞれ最大64件、globScanMaxDepthは1〜256。Gatewayはカテゴリ内のすべての権限・拒否条件を確認画面に表示できる場合に限りpermissions対応を宣言する。作業ディレクトリだけを見て自動許可しない。

## 状態・期限・競合・復旧

| 状態 | 意味 |
| --- | --- |
| pending | 永続保存済み、回答待ち |
| sending | 回答の送信意思を永続保存済み、結果未確定 |
| submitted | 接続への書込み完了。上流処理の成功保証ではない |
| resolved | 上流で要求が解消、または対象Turnが終了。回答による解消とは限らない |
| expired | 回答期限切れ、対象Responseの停止意思を保存 |
| cancelled | 停止・接続消失・実行状態の変化で回答不可 |
| unknown | 送信・解消結果が不明。自動再送しない |

`reply_status` はnot_sent／written／unknownであり、実行結果とは独立する。状態変更でrevisionを進める。結果不明の間も既存のUNKNOWN占有契約を維持する。

回答期限はProxyで要求を保存してから600秒。自動選択は行わず、期限切れ時は停止意思を保存する。期限判定と停止処理は最大5秒周期の保守処理または照会時に進める。上流が先に要求を解消した場合はその通知を優先する。`autoResolutionMs` を回答の自動選択許可には使用しない。

停止と回答はSQLiteのtransactionで送信意思の受付順を確定する。停止意思が先なら回答を拒否する。回答意思が先なら、その送信と後続の停止が競合し得るため、Gatewayは停止完了と偽らずResponseの実際の状態を照会する。上流イベントを取り逃した場合は、中継を宣言した進行中Responseに停止意思と `interaction_events_lost` を保存する。

再起動時にpendingはcancelled、sending/submittedはunknownとし、旧接続のRPC IDを再利用しない。正式復元後は基本契約の世代照合と占有復旧を行い、元のAI要求も回答も再送しない。

要求本文はpendingの間だけ取得可能で、回答意思・期限切れ・解消・取消・復旧の確定時に削除する。回答本文をDBや通常ログに永続保存しない。監査・重複防止用に対象ID、時刻、状態、revision、内容のfingerprintを保持する。これらの小さな照会記録・操作キーは既存台帳と同じく明示的な状態廃棄まで保持する。運用バックアップには過去のpending本文が含まれ得るため、バックアップのアクセス制限・保持手順も適用する。

## 主なエラー

- 400 `invalid_interaction_response`：型・回答・Capability宣言が不正。
- 409 `interaction_closed`：回答期限、停止、解消、再起動などにより回答不可。
- 409 `interaction_binding_pending`：上流Turn開始応答との対応確認前。GETで状態確認後、同じ操作キーで再要求できる。
- 409 `revision_conflict`：表示時のrevisionと不一致。自動で新しいrevisionに差し替えて許可しない。
- 409 `idempotency_conflict`：同じキーの内容・対象不一致。
- 認証、ワーク権限、復元世代、ストア障害は基本契約の401/403/409/428/503。

上流の要求が検証対象外・件数超過の場合は、要求を無検証で公開せず、対話不能のエラーと停止で処理する。

参照：[公式App Server仕様](https://learn.chatgpt.com/docs/app-server)、[v2基本契約](workspace-artifact-api-v2.ja.md)、[実装・受入記録](proxy-hardening-2026-09-13.ja.md)。

## Gateway実装と受入

- 対応済みの表示・回答UIがある種類だけを実行時に宣言する。通常の実行承認とは別のID・回答スキーマを使用する。
- MCPではツール実行自体の承認が、`_meta.codex_approval_kind: mcp_tool_call`、空のobjectスキーマとして届く場合がある。その後にツール本来の確認フォームが届く。各要求を個別に表示し、ユーザーが許可した対象だけに回答する。空フォームを自動承認しない。永続許可の追加メタデータはこのAPIでは受け付けない。
- 回答ボタンは本人・権限・有効期限を照合し、同じ操作キーを保存する。通信切断時は同じキーでoperationを照会・再取得し、新しいキーで回答し直さない。
- `/stop`、二重クリック、別ユーザーの操作、期限切れ、上流側での解消、Proxy/Gateway再起動、復元世代の不一致を受入試験に含める。秘密の質問・回答を公開Discordメッセージへ出さない。

Proxy側の試験は `tests/v2_http.rs`、`tests/v2_interactions.rs`、隔離した実Codex用の `scripts/live_v2_interactions.py` を参照。実モデル試験はローカルのテストMCPだけを用い、MCP実行承認とフォーム回答の二段階、同一キーのoperation再取得、最終出力を確認した。質問・権限・URLは模擬上流・検証器で確認しており、実クライアントUIの受入とは区別する。

回答送信中のHTTP切断は、Linux上で上流パイプの書込みを止めたままTCP RSTを発生させる試験も実施済み。切断後の送信継続・最終完了・同一キー再取得・上流への回答1回を確認した。[受入記録](proxy-hardening-2026-09-13.ja.md)を参照。

## 文書先行の追加契約（未配備）

[MCP操作詳細・単一Run限定許可0.3](mcp-turn-approval-api.ja.md)に、本人限定詳細、表示版の照合、明示的なRun限定許可を定義する。この追加契約はProxy提示版で、現行常駐の利用可能機能ではない。有効化後のinteraction一覧上限は履歴256件・未回答16件に分かれる。現在の16件制限を黙って変更せず、追加capabilityで判定する。

## 公開カード承認の次期契約

操作内容と承認を依頼元の会話にまとめる拡張は[0.4接続レビュー案](mcp-inline-approval-api.ja.md)で定義する。現行0.3のrequester_only引数を公開表示へ転用しない。新capability・presentation API・返信照合フィールドを実装した。このサーバーは2026-09-17に常駐反映済み。稼働先のcapabilityを確認して接続する。[検証記録](mcp-inline-approval-validation.ja.md)を参照。
