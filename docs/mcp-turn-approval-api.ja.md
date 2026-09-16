# MCP操作詳細・単一Run限定許可 API

2026-09-16 / 接続合意版0.3（2026-09-16 Gatewayレビュー完了）。

本書は実装が満たすべき接続契約であり、現行常駐サービスの実装済み機能一覧ではない。基本方針は双方合意済み。0.3の具体契約はGatewayが受け入れ、前回5指摘の解消と関連文書の整合を確認した。文書の整合確認・提示・接続上の指摘解消という実装開始条件は充足した。先行コードは未完了であり、0.3に照合して実装を再開できる。常駐での自動許可は未有効化。

基本契約は[API v2](workspace-artifact-api-v2.ja.md)。本書の全相対APIパスの接頭辞は `/v2/codex`。認証、instance／復元世代ヘッダー、操作キー、workspace認可、復元保留は基本契約を継承する。例のID・日時は説明用で、実在の秘密や利用者IDを含まない。

## 対応範囲

設定 `v2.mcp_turn_approval_enabled` は既定false。Codex 0.153.4専用アダプタを使用する。上流の `features.tool_call_mcp_elicitation=false` により、MCP呼出し承認を `item/tool/requestUserInput` のitemIdで受け、先行する `item/started` のmcpToolCallとThread・Turn・item IDを一致させる。固定の承認question IDと単発Allow選択肢も検証する。文面解析・到着順による推測は行わない。MCP内部フォームは従来のmcp_formを維持する。

Gatewayへはこのネイティブ承認を既存の空mcp_formに変換し、従来の単発accept/decline/cancelを互換提供する。上流への回答は元の質問IDへのAllow/Cancelに厳密に変換する。Allow for sessionや永続許可は送信しない。対応付け不足時はターン許可を提供しない。安全なMCP承認変換ができない要求は元のuser_input対話として扱い、user_input未宣言のクライアントには従来どおりunsupported_interactionで停止する。「必ず単発mcp_formへ戻せる」という保証ではない。確実な対応付けがあるがallowlist外・高危険度の呼出しは、ターン許可不可の単発mcp_formへ変換できる。featureはプロセス全体に影響するため、既定false、専用の検証環境で有効にして互換受入後に配備する。

## 実行の識別

`POST /v2/codex/conversations/{conversation_id}/responses` に省略可能な `approval_context` を追加する。

```json
{"input":"依頼","interaction_capabilities":["mcp_form"],"approval_context":{"principal_id":"opaque-user","channel_id":"opaque-channel-or-thread","run_id":"unique-run"}}
```

各値は空でない最大128 UTF-8 bytesの文字列。Gatewayが認証済み本人・会話から生成する。ProxyのAPIキーは信頼されたGateway/運用者の資格であり、任意クライアントの自己申告を本人認証とは扱わない。Gatewayは別利用者にこのAPIキーを渡さない。

1 Responseを1 Runとする。許可は上記3値、Proxy instance／復元世代、conversation／workspace／Response／Turn、入力世代、サーバー・ツール・設定世代に拘束する。同じrun_idを再使用してもResponseが異なれば流用不可。contextは受付後変更不可。旧クライアントで省略した場合は単発許可のみ。

## 操作詳細

`GET /v2/codex/interactions/{interaction_id}/operation`。既存Bearer・instance／復元世代・workspaceの認可を適用する。

返す主要フィールド：`interaction_id`、`response_id`、`turn_id`、`call_id`、`server`、`tool`、`binding_status`（verified/unavailable）、`unavailable_reason`、`config_generation`、`input_generation`、`scope_fingerprint`、`revision`、`turn_grant_eligible`、`ineligible_reason`、`arguments`、`redacted_paths`、`disclosure`。

`arguments` は実イベント由来で、モデル要約ではない。既知の秘密キーを伏せるが、任意コード内の秘密を完全に識別できる保証はない。`disclosure: requester_only` としてGatewayは本人限定の表示に使用し、公開チャンネル・ログへ転記しない。上限超過・情報欠落・秘匿が判断を妨げる場合は自動許可対象外。生の引数はProxyの許可監査DBへ保存せず、終了・再起動後は取得不能として返す。終了後の取得不能を、操作がなかったという意味にしない。

`browser_run_code_unsafe` は常に対象外。`browser_evaluate` は任意コードの意味・通信・変更を一般的に保証できないため、この契約版では操作コードを個別表示する。連続閲覧には、運用者が評価した専用のsnapshot等の読み取りツールを選択する。単なるツール名やreadOnlyHintだけでコード実行を安全認定しない。対象ツールは運用者設定 `v2.mcp_turn_grant_tools` のサーバー別allowlistで限定する。許可範囲は同一ツールの引数変更を含む。サイト限定等を保証するAPIではない。

## 明示的な許可作成・単発回答

単発回答は従来どおり `POST /interactions/{id}/reply`。追加指定なしのacceptは常に1件だけで、ターン許可へ昇格しない。

ターン許可は同じreplyに明示的な `grant_scope: "turn_tool"` と `expected_scope_fingerprint` を追加する。表示したrevision、pending状態、context、適格性を照合する。既存のIdempotency-Key必須。作成したgrant IDはinteractionに返し、次項の一覧で照会できる。

```json
{"expected_revision":1,"response":{"action":"accept","content":{}},"grant_scope":"turn_tool","expected_scope_fingerprint":"opaque-fingerprint"}
```

許可状態：pending（現在の回答結果待ち）→active（書込み確認済み）／suspended（結果不明）、active→revoked／expired。元の回答が結果不明なら後続に適用しない。許可レコードと現在のinteraction.reply_statusを別々に照会する。

## 照会・取消

- `GET /v2/codex/responses/{response_id}/mcp-grants`：許可一覧。grant_id、scope、state、reason、created_at、expires_at、application_count。
- `POST /v2/codex/mcp-grants/{grant_id}/revoke`：Idempotency-Key必須。新たな適用を禁止する。既にsending/submitted/unknownの適用を取り消したと返さない。`in_flight_or_unknown_count` を返す。実行停止は既存stop APIを使用する。

停止/cancelの受理、Run終端、Steer送信前、設定変更、再起動、復元世代変更、イベント欠落で失効する。Steerは同じTurnでも入力世代を進める。失敗したSteerでも旧許可を復活させない。承認ボタン操作では入力世代を進めない。

## 競合と制限

許可適用の送信意思保存と取消・stopをDBトランザクションで順序付ける。取消が先なら自動送信しない。送信意思が先なら既に取り消せない可能性を明示する。通信結果不明の応答を再送しない。

TTLは最大10分・Run終端まで。1 Responseの許可数は16、対話・適用監査履歴は合計256件、未回答対話は16件。上限時は黙って承認せず既存の安全な停止経路へ移る。監査保存不能も自動承認不可。監査は生の引数を含めず、後述の明示的な状態廃止まで保持する。許可の有効期限と監査保存期間は異なる。

主なエラー：409 `revision_conflict` / `scope_conflict` / `interaction_closed`、422 `turn_grant_ineligible`、503 `turn_approval_disabled`。既存の認証・復元世代・容量・結果不明の契約も適用する。

## 受入

実MCPの呼出しID・引数の対応付け、同一ツール5回（単発なら5確認／ターン許可なら1確認）、別principal/channel/run/Response/Turn/tool/server/generationの分離、単発非昇格、危険ツール除外、内部フォーム非自動回答、Steer・stop・取消の競合、再起動・復元・イベント欠落・回答結果不明、説明表示の情報不足・秘匿を試験する。実Discordでの画面表示・カード集約はGateway結合受入で確認する。模擬MCPの成功を実ブラウザー受入と扱わない。

## 正確なJSON契約

### Capability

```json
{
  "mcp_operation_details":{"enabled":true,"profile":"native-item-id-v1","max_argument_bytes":65536,"disclosure":"requester_only"},
  "mcp_turn_approval":{"enabled":true,"profile":"native-item-id-v1","max_grants":16,"ttl_seconds":600,"max_records":256}
}
```

上記は `GET /v2/codex/capabilities` のトップレベルへ追加するオブジェクト（`features` 配下ではない）。キー自体がなければ未対応、存在してenabled=falseなら実装済み・運用無効。両機能を別々に確認し、ターン許可ボタンはさらに個々のoperation.turn_grant_eligibleがtrueの場合だけ表示する。操作詳細無効時のGETは503 `operation_details_disabled`、ターン許可無効時の明示作成は503 `turn_approval_disabled`。

### 操作詳細：成功例（全フィールド必須、nullを明示）

```json
{
 "interaction_id":"int_example","response_id":"resp_example","turn_id":"turn_example",
 "call_id":"call_example","server":"example","tool":"read_test",
 "binding_status":"verified","unavailable_reason":null,
 "config_generation":"generation-opaque","input_generation":0,
 "scope":{"instance_id":"pxy_example","recovery_generation":"gen_example","context":{"principal_id":"opaque-user","channel_id":"opaque-channel","run_id":"run-example"},"response_id":"resp_example","conversation_id":"conv_example","workspace_id":"ws_example","turn_id":"turn_example","input_generation":0,"config_generation":"generation-opaque","server":"example","tool":"read_test"},
 "scope_fingerprint":"opaque-display-binding","revision":1,
 "turn_grant_eligible":true,"ineligible_reason":null,
 "arguments":{"query":"example"},"redacted_paths":[],"disclosure":"requester_only"
}
```

scope_fingerprintは秘密引数の公開ハッシュではなく、安定call ID・実引数の不変スナップショット・設定世代・入力世代・許可範囲に対応付けた不透明トークン。同一call IDの内容が変化した場合は対応付けを無効化し、以前の表示に対する回答を409で拒否する。revisionはinteraction revisionと同一。返した詳細を更新した場合に古い表示のまま許可することはない。

Gatewayの本人限定詳細画面から行う単発許可には `expected_scope_fingerprint` を必ず付ける。ProxyのAPI上は後方互換のため省略可能。`grant_scope` を付けなければ単発のまま。旧クライアントの従来bodyは引き続き受け付けるが、操作詳細を閲覧したと推定しない。

対応付け不能例：上記のcall_id/server/tool/config_generation/input_generation/scope/scope_fingerprint/argumentsはnull、binding_statusはunavailable、unavailable_reasonはstable_call_id_unavailable、turn_grant_eligibleはfalse、ineligible_reasonはbinding_unavailable、redacted_pathsは[]。interaction_id/response_id/turn_id/revision/disclosureは維持する。再起動・終端・期限切れで生引数を失った場合はarguments=null、unavailable_reason=operation_details_expired、turn_grant_eligible=false。高危険度はineligible_reason=high_risk_tool、許可リスト外はtool_not_allowlisted、contextなしはapproval_context_required、秘匿ありはredacted_arguments、設定変更はconfig_changed、入力世代変更はinput_changed。

### replyと許可一覧

replyのoperationは従来どおりresource.type=interaction、resource.id=interaction ID。許可が作成された場合、GET interactionに `grant_id` が追加される。単発のみならこのフィールドはない。interaction.reply_statusとgrant.stateを別々に照合する。

```json
{"operation_id":"op_reply","state":"succeeded","resource":{"type":"interaction","id":"int_example"}}
```

これはoperationの主要フィールド例。既存契約の時刻等も返す。succeededは書込み確認であり、ツール実行成功ではない。

```json
{"response_id":"resp_example","data":[{"grant_id":"grant_example","scope":{"instance_id":"pxy_example","recovery_generation":"gen_example","context":{"principal_id":"opaque-user","channel_id":"opaque-channel","run_id":"run-example"},"response_id":"resp_example","conversation_id":"conv_example","workspace_id":"ws_example","turn_id":"turn_example","input_generation":0,"config_generation":"generation-opaque","server":"example","tool":"read_test"},"state":"active","created_at":"2026-09-16T00:00:00Z","expires_at":"2026-09-16T00:10:00Z","application_count":1,"initial_interaction_id":"int_example"}]}
```

stateはpending/active/suspended/revoked/expired。reasonは失効時に付く（operator_revoked、scope_ended、run_ended）。suspendedは返信結果不明。application_countは送信意思を記録した件数であり、実行成功件数ではない。一覧は全件、paginationなし。grantは最大16件、interaction一覧は機能有効時に最大256件まで返すため、Gatewayの「一覧<=16」検証を変更する。切捨てはしない。未回答上限16件と履歴上限256件を区別する。

### revoke

要求bodyは `{}`。202でoperationを返し、resource.type=mcp_grant、resource.id=grant ID。`in_flight_or_unknown_count` は取消時点でsending/submitted/unknownの適用件数。受理後のHTTP切断では既存のoperation照会・同一キー同一bodyの再照会を使う。別キーで再送しない。grant一覧でもrevokedを確認できる。

時刻は既存v2と同じRFC3339 UTC形式。引数最大65536 bytes、context各128 UTF-8 bytes、既存interaction全体の65536 bytes上限とID検証を維持する。引数はメモリだけで最大10分保持し、終端で破棄する。監査メタデータと冪等操作記録の保持は基本契約の `until_explicit_state_retirement` とする。時間経過による自動削除は行わず、Proxy状態の明示廃止まで照会可能とする。通常再起動・正式復元で廃止しない。容量不足では新規受付を拒否し、保存済み監査を黙って消さない。個々の許可は最大10分で失効するため、監査の保持が許可の延長を意味することはない。

### Steerと実利用の受入

既存 `POST /v1/codex/turns/{turn_id}/steer` 内で入力世代更新と旧許可失効を先に永続化し、成功した場合のみ上流へ送信する。新APIへの移行は不要。失効保存失敗ならSteerは送らない。

商品ランキング閲覧の代替候補はplaywrightのbrowser_snapshot等、コード実行を伴わない専用操作。実ツール定義とモデルの選択を検証してから運用者allowlistへ追加する。実作業での確認回数比較・実Discordカード集約は未完了の受入項目として保持し、browser_evaluateの危険度除外だけで改善完了とはしない。

## 0.3補完：型・境界・完全な応答例

### Wireの型とサイズ

JSONの追加フィールドをクライアントは無視できること。以下の必須フィールドの欠落・型違い・未知のstate/profileは、許可ボタンを出さず照合エラーとする。IDを数値に変換しない。

| フィールド | 型・制約 |
| --- | --- |
| capability.enabled | boolean、必須 |
| capability.profile | string、`native-item-id-v1`、必須 |
| capabilityの件数・秒数・bytes | 0以上のinteger、必須。下記固定上限を返す |
| interaction_id / response_id | 空でないstring、必須 |
| operation.turn_id | stringまたはnull、必須。未対応付け時はnull可 |
| operation.call_id / server / tool / config_generation / scope_fingerprint | stringまたはnull、必須 |
| operation.input_generation | 0以上のintegerまたはnull、必須 |
| operation.revision | 1以上のinteger、必須。interaction.revisionと同じ |
| operation.scope | 下記scope objectまたはnull、必須 |
| operation.arguments | JSON objectまたはnull、必須。nullは空引数を意味しない |
| operation.binding_status | verified / unavailable、必須 |
| operation.unavailable_reason / ineligible_reason | 下記列挙stringまたはnull、必須 |
| operation.turn_grant_eligible | boolean、必須 |
| operation.redacted_paths | string配列、必須。JSON Pointer表記。伏字箇所なしは[] |
| operation.disclosure | requester_only、必須 |
| grant.scope | object、必須。各キーは成功例と同一。contextの3値は非null |
| grant.reason | revoked/expiredでは必須、pending/active/suspendedでは省略可 |
| grant.created_at / expires_at | RFC3339 UTC string、必須。秒の小数部を許容 |
| grant.application_count | 0以上のinteger、必須。送信意思保存件数 |
| grant.initial_interaction_id | string、必須 |

scopeのcontextなしはoperationでは `context:null` とし、許可作成不可。scope内のinstance_id / recovery_generation / response_id / conversation_id / workspace_id / turn_id / config_generation / server / toolは非空string、input_generationは非負integer。Turn未対応付けならscope全体をnullとする。scopeには引数を含めない。同一ツールの別引数を含む許可であることを表示する。scope_fingerprintは呼出しごとに異なり、scopeが等しい別呼出しでも再利用できない。

| 上限 | 値・超過時 |
| --- | --- |
| 新規context各値 | 128 UTF-8 bytes、超過は400 invalid_approval_context |
| 本拡張のID・サーバー・ツール・理由などの単一文字列 | 8192 UTF-8 bytes以下。任意引数内のstringは引数全体上限による |
| 生引数を含む照合元MCP item | JSON UTF-8で65536 bytes、超過は詳細取得不可・自動許可不可 |
| 返信body / 上流対話request | JSON UTF-8で65536 bytes。引数上限とは別に適用 |
| GET operation / 単一interaction | JSON UTF-8で262144 bytes以下。切り詰めてverifiedにしない |
| grant一覧 | 1 Response最大16件、全件で1048576 bytes以下 |
| interaction一覧 | 有効時1 Response最大256件、全件で67108864 bytes以下 |
| 未回答対話 | 1 Responseのpending＋sending最大16件 |
| メモリ上の詳細 | プロセス合計256呼出し、各最大65536 bytes、最長10分 |

応答サイズ上限は非圧縮UTF-8 JSONに対する値。上限を満たさないデータは新規登録しない。正常応答を途中で打ち切らず、応答不能は503を返す。一覧にpaginationや暗黙の省略はない。interaction_limits.max_count=16は従来互換の対話制限値であり、mcp_turn_approval.enabled=trueの場合の履歴全件上限はmcp_turn_approval.max_records=256を使う。無効・旧Proxyでは従来の履歴上限16を維持する。Gatewayはcapabilityを取得してから一覧上限を選ぶ。

### 無効・対応付け不能・期限切れ

機能無効時のcapability追加部分の完全形：

```json
{
 "mcp_operation_details":{"enabled":false,"profile":"native-item-id-v1","max_argument_bytes":65536,"disclosure":"requester_only"},
 "mcp_turn_approval":{"enabled":false,"profile":"native-item-id-v1","max_grants":16,"ttl_seconds":600,"max_records":256}
}
```

現版は同じ運用スイッチを使うが、Gatewayは2機能を独立判定する。未対応版は両キーが存在しない。無効時のGET operationはHTTP 503、次の完全bodyを返す。

```json
{"error":{"code":"operation_details_disabled","message":"operation_details_disabled","retry":{"action":"none"}}}
```

有効だが呼出し対応付け不能の場合はHTTP 200：

```json
{
 "interaction_id":"int_example","response_id":"resp_example","turn_id":"turn_example",
 "call_id":null,"server":null,"tool":null,"binding_status":"unavailable",
 "unavailable_reason":"stable_call_id_unavailable","config_generation":null,"input_generation":null,
 "scope":null,"scope_fingerprint":null,"revision":1,"turn_grant_eligible":false,
 "ineligible_reason":"binding_unavailable","arguments":null,"redacted_paths":[],"disclosure":"requester_only"
}
```

対応付け済みでも終端・再起動・期限切れ・上限による破棄後は生引数を返さない。scope等の監査メタデータは維持する。binding_status=verifiedは「過去に対応付けできた」意味であり、現在の許可可能性を表さない。arguments=null、unavailable_reason=operation_details_expired、turn_grant_eligible=falseにする。

取得不能理由はstable_call_id_unavailable / operation_details_expired。不適格理由はbinding_unavailable / high_risk_tool / redacted_arguments / approval_context_required / tool_not_allowlisted / config_changed / input_changed。詳細期限切れはunavailable_reasonを優先表示する。既知の秘密を伏せた場合はredacted_pathsを返し、ターン許可は不可。翻訳・要約はGatewayの別表示であり、argumentsを書き換えたものを実引数と称さない。

### 返信と状態照会の完全例

詳細確認後の単発返信。ターン許可には昇格しない。

```json
{"expected_revision":1,"expected_scope_fingerprint":"opaque-display-binding","response":{"action":"accept","content":{}}}
```

HTTP 202の書込み確認済みreply応答の完全例：

```json
{"operation_id":"op_reply","kind":"interaction.reply","state":"succeeded","created_at":"2026-09-16T00:00:00Z","error":null,"resource":{"type":"interaction","id":"int_example"}}
```

grant_scope付き返信の後、GET `/interactions/int_example` の完全例（上流解決前）：

```json
{
 "interaction_id":"int_example","response_id":"resp_example","conversation_id":"conv_example","workspace_id":"ws_example",
 "kind":"mcp_form","state":"submitted","revision":3,"created_at":"2026-09-16T00:00:00Z","expires_at":"2026-09-16T00:10:00Z",
 "request":null,"reply_status":"written","resolution_reason":"interaction_reply_written","error":null,"grant_id":"grant_example",
 "operation":{
  "binding_status":"verified","call_id":"call_example","server":"example","tool":"read_test","config_generation":"generation-opaque",
  "turn_grant_eligible":true,"ineligible_reason":null,"disclosure":"requester_only","redacted_paths":[],
  "scope":{"instance_id":"pxy_example","recovery_generation":"gen_example","context":{"principal_id":"opaque-user","channel_id":"opaque-channel","run_id":"run-example"},"response_id":"resp_example","conversation_id":"conv_example","workspace_id":"ws_example","turn_id":"turn_example","input_generation":0,"config_generation":"generation-opaque","server":"example","tool":"read_test"},
  "scope_fingerprint":"opaque-display-binding"
 }
}
```

interaction.operationは受理時メタデータであり現在の許可ボタン判定に使わない。生引数はGET operationでのみ取得する。revisionは非同期解決でさらに増えるので上記3を固定値として期待しない。単発のみならgrant_idは省略。ターン許可作成時はgrant一覧でstateを別途照合する。自動適用された後続interactionにも使用したgrant_idを保存する。

### revokeの完全例と復旧

POST `/mcp-grants/grant_example/revoke`、`Idempotency-Key: revoke-example`、bodyは `{}`。

HTTP 202：

```json
{"operation_id":"op_revoke","kind":"mcp_grant.revoke","state":"succeeded","created_at":"2026-09-16T00:01:00Z","error":null,"resource":{"type":"mcp_grant","id":"grant_example"},"in_flight_or_unknown_count":1}
```

応答を失った場合は `GET /operations/by-key/revoke-example`、または既知なら `GET /operations/op_revoke` で同じoperationを取得する。未登録404なら元のキー・元のbodyで再送できる。照会503/通信不能なら結果不明のまま待つ。異なるbodyで同一キーを使うと409 idempotency_conflict。別キーでの再作成や元のツール実行の再送で回復しない。

取消前に送信意思を保存したinteractionのうち、取消時点でsending/submitted/unknownのものを数える。resolvedはこの数に含めないが、成功・無作用を保証するものではない。取消応答はスナップショットで、後から0に更新されない。新規適用を禁止できたことと、既実行を戻せたことを区別する。既にrevoked/expiredの取消も冪等に成功し、許可は復活しない。

### エラーとクライアント動作

共通bodyは上記503例と同じ形で、codeとmessageに次表のコードを入れる。新規コードのretry.actionはnone。既存のinteraction_binding_pendingはpoll_operationを維持する。429ではRetry-Afterを返すが、上限超過を自動承認で回避しない。

| HTTP | code | 動作 |
| --- | --- | --- |
| 400 | invalid_approval_context / invalid_interaction_response | 要求修正。上流へ送信していない |
| 404 | resource_not_found / operation_not_found | 基本契約の対象／操作キー照会として処理 |
| 409 | revision_conflict / scope_conflict / operation_binding_expired | 古い画面を閉じて再照会。新しい版へ無条件で押下を移さない |
| 409 | interaction_closed / interaction_binding_pending / grant_inactive | 実際の対象状態を再照会。許可成功と表示しない |
| 409 | idempotency_conflict | 元のキーとbodyを照合。別キーの自動再試行は禁止 |
| 422 | turn_grant_ineligible | ターン許可不可。取得可能なら個別詳細確認へ |
| 429 | turn_grant_limit / interaction_limit_exceeded | 新規許可不可。対話上限時は安全な停止経路へ |
| 503 | operation_details_disabled / turn_approval_disabled | capability再取得。該当機能のボタンを隠す |
| 503 | mcp_config_unavailable | 世代確認不能。自動許可しない |

認証・workspace失効・世代不一致・復元保留・永続化失敗の既存エラーは基本契約のまま。書込み試行後の結果不明は成功や未実行に置き換えずoperation.state=unknown／interaction.reply_status=unknownで照会する。

## 状態遷移・世代・競合の規範

| 事象 | 許可状態と効力 |
| --- | --- |
| 明示ターン許可＋表示照合成功 | 現在の返信意思とpending許可を同一トランザクションで保存 |
| 上流への返信書込み確認 | pending→active。ただし先に失効・取消されていれば復活しない |
| 返信結果不明 | pending/active→suspended。後続には適用しない。自動再送・自動再活性化なし |
| 後続の同一scope呼出し | active・期限・世代・停止状態を送信意思保存時に再照合し、適用数を増加 |
| 取消 | pending/active/suspended→revoked、reason=operator_revoked |
| Run終端／接続喪失／イベント欠落 | 非終端許可→expired、reason=run_ended |
| TTL、stop/cancel、Steer、設定変更、再起動、復元 | 非終端許可→expired、reason=scope_ended |

「即時失効」は失効受付の永続化が完了した後、新しい適用意思を保存できないことを意味する。先行送信意思の通信結果まで撤回したとは保証しない。照会では実効状態へ更新して返す。停止意思・入力世代・許可の照合に失敗したら送信しない。pending許可への後続呼出しは待機し、最初の返信がactiveになってから再照合する。suspendedやexpiredへの移行後に待機呼出しを勝手に許可しない。

既存Steer経路は `/v1/codex/turns/{turn_id}/steer`。対象Thread・Turnを確定し、対象Runの入力世代更新と旧許可失効を永続化してから上流Steerを送る。保存失敗では送らない。送信失敗・結果不明でも世代を巻き戻さず、元の許可を復活させない。古い入力世代の操作詳細からの返信は単発でも拒否する。

config_generationは秘密設定のハッシュを公開せず、不透明な世代IDとする。継承元と生成後の有効MCP設定、接続先、ツールの許可方針が変われば更新する。変更を元へ戻しても古い世代を再使用しない。Proxy再起動・上流接続の再構築でも旧許可を失効させる。設定確認不能なら適用を止める。実行中のMCPサーバー内部実装の無通知変更を検知できる保証はない。運用者はサーバー／定義更新時に設定再読込・接続再構築で世代を更新し、未評価定義へ既存許可を流用しない。

scope_fingerprintは表示スナップショットのトークンであり、設定／入力世代が変わっても文字列自体を必ず変更する方式ではない。照合対象世代の不一致・対応付け失効によって古いトークンを拒否する。生引数を同一トークンのまま差し替えない。revisionだけで引数同一性を代用しない。

## 互換性・受入と実証の境界

機能を有効にするfeatureはCodex App Serverプロセス全体に作用する。Runごとに切り替える設計ではない。旧クライアントへの影響がないと仮定せず、次表を有効化前に隔離環境で試験する。

| 上流／クライアント | 必須挙動 |
| --- | --- |
| ID対応付け成功、mcp_form宣言、allowlist外／危険ツール | 単発mcp_form＋取得可能な詳細、ターン許可なし |
| ID対応付け失敗、user_input宣言 | 元のuser_inputの個別対話。MCP許可へ推測変換しない |
| ID対応付け失敗、mcp_formのみ宣言 | unsupported_interactionとして対象実行を停止。単発画面が出ると保証しない |
| 内部フォーム／URL認証／秘密入力／permissions | 既存kind・capability検証。ターン許可の適用なし |
| v1/OpenAI互換クライアント、並列Run | 標準API契約を変えず、未対応承認は既存の明示的エラー。別Runの許可は使用不可 |

Gatewayに一般user_inputの実装を必須化しない。未対応宣言を尊重した停止を受け入れる。これを避ける必要があるならGatewayで通常質問として実装する別変更が必要であり、MCP専用の空フォームへ自動変換して済ませない。

実証済みなのはCodex 0.153.4（上流ソースcommit `3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`）と実playwrightのbrowser_evaluateで、`item/started`のID・引数と`item/tool/requestUserInput.itemId`を一致確認できた点。定数 `() => 2 + 2` の呼出し承認をCancelで終了した照合試験であり、ブラウザー操作実行や自動許可・Discord受入の成功ではない。

### 必須受入項目（実装後に実施、現在は完了扱いにしない）

| ID | 試験・合格条件 |
| --- | --- |
| A01 | 実上流のThread/Turn/item IDと引数照合。欠落・重複変更・順序逆転で推測しない |
| A02 | 今回だけAllowを5件押してもgrant作成なし。明示ターン許可なら同一専用ツール5件でクリック1回 |
| A03 | principal/channel/run/Response/conversation/workspace/Turn/server/tool/instance/復元世代が1要素でも異なれば適用不可。並列Runで確認 |
| A04 | 高危険度コード、秘匿、上限超過、内部フォーム・認証・権限要求は自動許可なし |
| A05 | 単発・ターン許可の両方で、表示後の引数変更・設定変更・Steer・TTLに対して旧押下を拒否 |
| A06 | 取消・stop・Steerと送信意思保存の前後を制御して試験。失効保存失敗時は上流送信なし |
| A07 | 初回返信と次要求の競合、返信結果不明、再起動、正式復元、イベント欠落で許可が復活せず、返信再送なし |
| A08 | 未回答16件と履歴256件、許可16件、サイズ上限、保持期限、監査保存不能、完全な一覧を確認 |
| A09 | 本人限定画面で実コードを確認でき、公開カード・DB・ログに引数を追加漏洩しない。モデル要約を区別 |
| A10 | capability無効・未対応・不明profileと上記互換性表を確認。許可対象外クライアントに偽の成功なし |
| A11 | 以下の商品ランキング実作業比較と実Discordのカード集約・次発言での再確認 |

A11の具体候補はplaywright.browser_snapshot。まず実際の公開定義・実装・権限・引数を運用者が評価し、対象バージョンと設定世代を固定する。ページ遷移やクリックまでsnapshotの許可に含めない。navigate/click等を使う場合はそれぞれ別ツールとして評価・個別承認する。evaluate/unsafeへの置換を自動承認しない。

同じ商品ランキング閲覧依頼・モデル・ツール公開設定で、(a)現行個別承認、(b)明示した専用ツール経路を比較する。モデルに公開するツール定義と閲覧指示を記録し、実際の選択ツール列、各ツールの確認回数、ランキングの取得内容、追加発言後の確認、残ったDiscordカード数を記録する。snapshotが一覧抽出に不足してevaluateを選ぶ場合は個別コード確認へ戻す。必要なら専用抽出ツールの追加を別設計として提示する。ツール名をallowlistへ追加しただけでは代替策完成としない。現時点でbrowser_evaluateの5回確認問題が解消したとは主張しない。

## Gatewayレビュー対応表・引渡し条件

| Gatewayレビュー項目 | 0.3の回答 |
| --- | --- |
| 1 Capability／JSON／上限 | トップレベルの正確なキー、型、無効と欠落、完全body、nullable、全件上限を規定 |
| 2 fallback／互換性 | 安全変換可とuser_input未対応停止を分離。プロセス全体への影響を受入条件化 |
| 3 本人限定表示と単発照合 | 詳細画面からはrevision＋fingerprint必須。実引数は本人限定、世代変化でも旧押下拒否 |
| 4 既存Steer経路 | v1経路で保存→送信。失敗・不明でも許可復活なし |
| 5 実利用 | snapshotの評価・モデル選択条件・ランキング比較・確認回数をA11へ具体化。受入は未完了 |

Gatewayは本書を基に要件・設計・DB・受入条件を照合できる。新APIが既に常駐で利用できると解釈しない。具体契約への差分指摘は文書へ先に反映し、双方の照合が終わってからこの版を基準に実装を再開する。実証未完了項目はリリースの判定条件であり、完了の証拠を文書の体裁で代用しない。

### 接続レビュー完了記録（2026-09-16）

Gatewayの `docs/mcp-turn-approval-api-review.ja.md` に0.3受入と5指摘解消が記録された。接続契約に実装開始を妨げる未決事項はない。Proxyの実装基準版を0.3とする。Gateway側も詳細設計を先行させる。A11の連続確認削減を含む実装・受入・常駐有効化は未完了であり、今回の合意に含めない。
