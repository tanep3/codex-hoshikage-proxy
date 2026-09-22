# APIリファレンス

**日本語** | [English](api-reference.md)

この文書は、Codex Hoshikage Proxyが公開する現行APIの利用者向けリファレンスです。各URLで何ができるか、何を送るか、何が返るかをまとめています。

高度な競合・復旧規則は[実行制御APIの契約](control-api.ja.md)、Codex App Serverとの対応範囲は[対応表](app-server-coverage.md)も参照してください。

## 1. APIの選び方

| 目的 | 使用するAPI |
| --- | --- |
| OpenAI対応アプリからテキスト／画像を送り、回答を受け取る | `POST /v1/responses` または `POST /v1/chat/completions` |
| 会話を継続し、状態照会・中断・Steer・対話承認を使う | `POST /v1/responses` と `/v1/codex/*` |
| Codex App Serverの全機能を、そのまま扱う | WebSocket `GET /codex` |
| 生存・準備状態を監視する | `GET /healthz`、`GET /readyz` |

`/v1`はOpenAI APIの**互換サブセット**です。Codex App Serverの機能をOpenAI形式へ推測変換しません。App ServerのThread、Turn、Item、通知、承認、MCP、実験的APIなどを直接扱うクライアントは`/codex`を使用してください。

## 2. 共通仕様

### ベースURL

以下では、Proxyを同じPCの4040番ポートで動かす例を使います。

```sh
export PROXY_BASE_URL=http://127.0.0.1:4040
export PROXY_API_KEY='設定したAPIキー'
```

LANから接続する場合は、`127.0.0.1`をProxyホストのIPアドレスへ置き換えます。インターネットへ公開する場合はTLSを終端するリバースプロキシを使用してください。

### 認証

API Keyを設定したProxyでは、**すべてのHTTP APIと`/codex`のWebSocket Upgrade**に次のヘッダーが必要です。`/healthz`と`/readyz`も例外ではありません。

```http
Authorization: Bearer <API key>
```

認証失敗はHTTP `401`です。

```json
{
  "error": {
    "message": "API key is missing or invalid",
    "type": "invalid_request_error",
    "code": "invalid_api_key"
  }
}
```

単一のAPI Keyを共有する利用者は、Proxy上では同じ運用主体です。利用者ごとの所有権分離やACLはありません。

### JSONエラー

Upgrade前と通常HTTP APIのエラーは、原則として次の形式です。

```json
{
  "error": {
    "message": "人が読める説明",
    "type": "invalid_request_error",
    "code": "機械判定用コード"
  }
}
```

クライアントはHTTP状態と`error.code`の両方を確認してください。同じHTTP状態でも原因は異なります。

### HTTP本文サイズ

受信するHTTP本文の上限は16 MiBです。画像のdata URLもこの本文上限に含まれます。

## 3. エンドポイント一覧

| Method | Path | できること |
| --- | --- | --- |
| `GET` | `/healthz` | Proxyプロセスの生存確認 |
| `GET` | `/readyz` | Codex実行系と制御ストアの準備確認 |
| `GET` | `/v1/models` | 現在利用可能な公開モデル一覧 |
| `POST` | `/v1/responses` | Responses互換の実行、会話継続、ストリーミング |
| `POST` | `/v1/chat/completions` | Chat Completions互換の単発実行 |
| `GET` | `/v1/codex/capabilities` | 拡張APIの機能と制限、Provider状態 |
| `GET` | `/v1/codex/requests/{request_id}` | Idempotency-Keyに対応する実行記録 |
| `GET` | `/v1/codex/responses/{response_id}` | Responseの実行記録と継続可否 |
| `GET` | `/v1/codex/turns/{turn_id}/status` | Codexへ照会したTurnの現在状態 |
| `POST` | `/v1/codex/turns/{turn_id}/interrupt` | Turnの中断要求 |
| `POST` | `/v1/codex/turns/{turn_id}/steer` | 実行中Turnへの追加指示 |
| `GET` | `/v1/codex/turns/{turn_id}/events/stream` | 状態・承認・上流イベントの監視SSE |
| `GET` | `/v1/codex/turns/{turn_id}/approvals` | Turnの未処理承認一覧 |
| `GET` | `/v1/codex/approvals/{approval_id}` | 承認内容と状態の取得 |
| `POST` | `/v1/codex/approvals/{approval_id}` | 承認への回答 |
| `GET` | `/v1/codex/responses/{response_id}/images` | Codexが生成したPNG一覧 |
| `GET` | `/v1/codex/responses/{response_id}/images/{filename}` | 生成PNGの取得 |
| `GET` | `/codex` | Codex App Server JSON-RPCのWebSocket接続 |

旧`/v2/codex`は廃止済みであり、`404`を返します。

## 4. ヘルスチェック

### `GET /healthz`

HTTPサーバーが要求へ応答できることを確認します。上流Providerが利用可能という意味ではありません。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/healthz"
```

```json
{"status":"ok"}
```

### `GET /readyz`

共有Codex App ServerがReadyで、永続制御ストアを読める場合は`200`を返します。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/readyz"
```

準備完了:

```json
{"status":"ready"}
```

準備未完了はHTTP `503`です。

```json
{"status":"not_ready"}
```

`/readyz`は各Providerへの認証や個々のモデルの利用可否を保証しません。Provider状態は`/v1/codex/capabilities`、モデルは`/v1/models`で確認します。

## 5. モデル

### `GET /v1/models`

設定と各Providerの動的カタログから、現在公開できるモデルをOpenAI互換形式で返します。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/models"
```

```json
{
  "object": "list",
  "data": [
    {
      "id": "chatgpt/gpt-5.6-luna",
      "object": "model",
      "created": 0,
      "owned_by": "chatgpt"
    }
  ]
}
```

公開IDは`provider/model`です。設定により`chatgpt/...`、`hoshikage/...`、`ollama/...`などが返ります。認証切れ、停止中、能力不足のProvider／モデルは一覧から外れることがあります。

## 6. Responses互換API

### `POST /v1/responses`

新しい会話を開始するか、以前成功したResponseの会話を継続します。制御APIを使う新規クライアントには、このエンドポイントを推奨します。

#### 最小リクエスト

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"chatgpt/gpt-5.6-luna","input":"短くOKと答えて"}' \
  "$PROXY_BASE_URL/v1/responses"
```

```json
{
  "id": "resp_...",
  "object": "response",
  "model": "chatgpt/gpt-5.6-luna",
  "output": [{
    "id": "msg_1",
    "type": "message",
    "role": "assistant",
    "content": [{"type":"output_text","text":"OK"}],
    "status": "completed"
  }],
  "status": "completed"
}
```

成功応答には、利用可能な識別子を次のヘッダーでも返します。

```text
x-response-id: resp_...
x-codex-thread-id: ...
x-codex-turn-id: ...
```

#### 対応フィールド

| Field | Type | 説明 |
| --- | --- | --- |
| `model` | string、省略可 | `/v1/models`の公開ID。新規会話では設定の既定モデル、継続では会話の最新モデルを使用 |
| `input` | stringまたはarray、必須 | テキスト、画像、または`role`付きメッセージ |
| `previous_response_id` | string、省略可 | 成功したResponseの会話を継続 |
| `stream` | boolean、省略可 | `true`でSSE。既定`false` |
| `metadata` | object、省略可 | 値が文字列のメタデータ。下記の`codex.*`設定を使用可能 |
| `reasoning.effort` | string、省略可 | 対応するChatGPTモデルの推論レベル |
| `text.format` | object、省略可 | `text`または`json_schema` |

`metadata`で使えるProxy拡張:

| Key | Value | 効果 |
| --- | --- | --- |
| `codex.cwd` | 許可済み絶対パス | 新規会話の作業ディレクトリ。継続中は変更不可 |
| `codex.approval_capability` | `interactive` | クライアントが承認APIを扱えることを宣言 |
| `codex.auto_approve_workspace` | `false` | この会話ではワークスペース内の自動承認も抑制。`true`はサーバー設定以上に許可しない |

`metadata`の値はすべてJSON文字列です。booleanではありません。

#### 入力形式

文字列:

```json
{"input":"このリポジトリを説明して"}
```

メッセージと画像:

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "input": [{
    "role": "user",
    "content": [
      {"type":"input_text","text":"この画像を説明して"},
      {"type":"input_image","image_url":"https://example.com/image.png","detail":"high"}
    ]
  }]
}
```

対応するroleは`system`、`developer`、`user`、`assistant`です。Proxyはroleを`[system]`などのラベル付きテキストへ変換します。独立したApp Server systemメッセージにはなりません。

画像はHTTP(S) URLか`data:image/...` URLを指定します。`detail`は`auto`、`low`、`high`、`original`です。`file://`、Proxyホスト上のローカルパス、OpenAI Files APIのIDは受け付けません。

#### JSON Schema出力

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "input": "対象をJSONで説明して",
  "text": {
    "format": {
      "type": "json_schema",
      "schema": {
        "type": "object",
        "properties": {"answer":{"type":"string"}},
        "required": ["answer"],
        "additionalProperties": false
      }
    }
  }
}
```

Proxyは`schema`をCodexへ渡し、結果をJSON文字列として返します。結果の再検証はしません。`name`と`strict`はProxy独自の保証にはなりません。

#### 会話継続

```json
{
  "previous_response_id": "resp_...",
  "input": "続きを説明して"
}
```

成功完了したResponseだけを継続できます。同じProvider内なら`model`を変更できます。Providerをまたぐ変更、作業ディレクトリの変更、同じThreadでの並行実行は`409`です。これは履歴の分岐や巻戻しではなく、同じThreadへの次のTurnです。

#### 重複実行防止

`Idempotency-Key`ヘッダーへ、1〜128文字のASCII英数字と`-_.`だけからなる要求IDを指定できます。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: req-8d529a' \
  -d '{"model":"chatgpt/gpt-5.6-luna","input":"調査して","stream":true}' \
  "$PROXY_BASE_URL/v1/responses"
```

同じキーと同じJSON本文の再送は新しいTurnを開始せず、実行メタデータをJSONで返します。回答本文やSSEは再配信しません。同じキーで本文が異なる場合は`409 request_conflict`です。通信結果が不明な場合は、再送より先に要求照会APIを使ってください。

#### ストリーミング

`stream:true`は`text/event-stream`を返します。主なイベントは次のとおりです。

| Event | 内容 |
| --- | --- |
| `response.created` | Response ID、モデル、`in_progress`状態 |
| `response.output_text.delta` | `delta`に回答の増分テキスト |
| `codex.turn.status` | 長時間処理、状態確認、承認待ちのheartbeat |
| `response.completed` | 正常終了 |
| `response.failed` | 失敗コードまたは上流エラー |

例:

```text
event: response.output_text.delta
data: {"id":"resp_...","output_index":0,"item_id":"msg_1","delta":"OK"}

event: response.completed
data: {"id":"resp_...","object":"response","status":"completed","model":"chatgpt/gpt-5.6-luna"}
```

ResponsesのSSEは`response.completed`または`response.failed`で終了し、`[DONE]`は送りません。生成SSEの切断は対象Turnへの中断要求を発生させます。

#### 非対応フィールド

クライアント定義の`tools`、`functions`、有効な`tool_choice`／`function_call`、`parallel_tool_calls:true`、ツール呼出し履歴は`400 unsupported_parameter`です。空配列、`none`、`false`のような動作しない指定だけは受理します。

Usage、OpenAI形式のツール呼出し／結果、最終回答の後日再取得は提供しません。

## 7. Chat Completions互換API

### `POST /v1/chat/completions`

既存のOpenAI Chat Completionsクライアント向けです。各要求は新しい一時Threadで実行され、`previous_response_id`による継続はありません。会話継続と確実な制御が必要ならResponses APIを使用してください。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{
    "model":"ollama/gemma4:e4b",
    "messages":[{"role":"user","content":"短くOKと答えて"}]
  }' \
  "$PROXY_BASE_URL/v1/chat/completions"
```

```json
{
  "id": "chatcmpl_1",
  "object": "chat.completion",
  "created": 1789080000,
  "model": "ollama/gemma4:e4b",
  "choices": [{
    "index": 0,
    "message": {"role":"assistant","content":"OK"},
    "finish_reason": "stop"
  }]
}
```

対応フィールドは`model`、`messages`、`stream`、文字列値の`metadata`、ChatGPT向け`reasoning_effort`、`response_format`です。メッセージのrole・テキスト・画像の規則とクライアント定義ツールの制限はResponses APIと同じです。

JSON SchemaはOpenAI形式で指定します。

```json
{
  "response_format": {
    "type": "json_schema",
    "json_schema": {
      "name": "answer",
      "strict": true,
      "schema": {"type":"object","properties":{"answer":{"type":"string"}}}
    }
  }
}
```

`stream:true`ではOpenAI形式の`chat.completion.chunk`を`data:`へ送り、最後に`data: [DONE]`を送ります。HTTP応答ヘッダー`x-codex-turn-id`も返します。切断時は対象Turnへ中断を要求します。

## 8. CapabilityとProvider状態

### `GET /v1/codex/capabilities`

クライアントがハードコードせず、現在の拡張API機能と制限を判断するためのエンドポイントです。呼出し時にモデルカタログも更新します。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/codex/capabilities"
```

主な応答:

```json
{
  "contract_version": "1.0",
  "responses": true,
  "streaming": true,
  "conversation_resume": true,
  "conversation_model_change": true,
  "request_lookup": true,
  "turn_status": true,
  "turn_events": true,
  "turn_interrupt": true,
  "turn_steer": true,
  "interactive_approval": true,
  "output_retrieval": false,
  "limits": {
    "auth_scope": "shared_operator",
    "event_reconnect": "snapshot_only",
    "event_history_replay": false,
    "steer_idempotency": false,
    "disconnect_interrupts": true,
    "approval_kinds": ["commandExecution", "fileChange"],
    "user_input": false,
    "mcp_elicitation": false,
    "permissions_approval": false,
    "model_change_scope": "same_provider",
    "continuation": "successful_response_only"
  },
  "providers": {
    "chatgpt": {
      "status": "available",
      "reason": null,
      "checked_at": "2026-09-23T12:00:00Z"
    }
  }
}
```

Providerの`status`は`available`、`authentication_required`、`temporarily_unavailable`などです。`reason`は診断用であり、将来の値追加を許容してください。

## 9. 実行記録と状態照会

### `GET /v1/codex/requests/{request_id}`

`Idempotency-Key`に対応する永続実行記録を返します。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/codex/requests/req-8d529a"
```

### `GET /v1/codex/responses/{response_id}`

Response IDに対応する実行記録を返します。この応答だけは`continuable`も含みます。

```json
{
  "response_id": "resp_...",
  "sequence": 12,
  "client_request_id": "req-8d529a",
  "phase": "finished",
  "thread_id": "...",
  "turn_id": "...",
  "model_id": "chatgpt/gpt-5.6-luna",
  "cwd": "/workspace/project",
  "last_observed_status": "completed",
  "last_observed_at_ms": 1789080000000,
  "started_at_ms": 1789079900000,
  "suppress_auto_approval": false,
  "interrupt_state": null,
  "output_retrieval": "unavailable",
  "continuable": true
}
```

`phase`はProxyの送信境界記録です。`received`、`dispatching`、`started`、`rejected`、`finished`、`unknown`があります。現在のCodex状態を知るにはTurn状態APIを使います。実行記録にプロンプトや最終回答は保存されません。

### `GET /v1/codex/turns/{turn_id}/status`

Codex App Serverへ`thread/read`を行い、現在状態とProxyの保存済み観測値を返します。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/codex/turns/$TURN_ID/status"
```

`status`は通常`inProgress`、`completed`、`failed`、`interrupted`、照会不能時は`unknown`です。`turn`、`thread_status`、`pending_approvals`、`runtime_query_error`も含みます。`unknown`を成功・失敗・停止済みのいずれかへ推測しないでください。

## 10. Turnの中断とSteer

### `POST /v1/codex/turns/{turn_id}/interrupt`

本文なしで、中断をCodexへ要求します。

```sh
curl -X POST -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/codex/turns/$TURN_ID/interrupt"
```

`202 accepted`はCodexが要求を受け取ったことだけを示します。

```json
{"turn_id":"...","result":"accepted","status":"not_yet_confirmed"}
```

実際に中断したかはTurn状態APIで確認します。既に終端なら`200 already_terminal`、能動Turnと確認できなければ`409 turn_state_unknown`です。ファイル変更の巻戻しは行いません。

### `POST /v1/codex/turns/{turn_id}/steer`

実行中の同じTurnへ追加指示を送ります。

```sh
curl -X POST -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d "{\"expected_turn_id\":\"$TURN_ID\",\"input\":\"先にテストを実行して\"}" \
  "$PROXY_BASE_URL/v1/codex/turns/$TURN_ID/steer"
```

```json
{"turn_id":"...","result":"accepted"}
```

URLと`expected_turn_id`は一致必須です。終端、不明、中断要求中、承認／入力待ちでは`409 turn_not_steerable`です。SteerにはIdempotency-Keyがありません。応答を受け取れなかった場合は、同じ指示を自動再送しないでください。

## 11. Turnイベントの監視

### `GET /v1/codex/turns/{turn_id}/events/stream`

実行そのものを所有しない、監視用SSEです。

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/codex/turns/$TURN_ID/events/stream"
```

接続直後に次を送ります。

1. `codex.events.reset`: 新しい監視接続であり、履歴再生ではないことを通知
2. `codex.turn.snapshot`: 現在状態、Turn情報、未処理承認
3. 現在の`approval_requested`

以後、該当Turnの上流通知とProxy承認イベントを、元の`kind`または`method`名で転送します。受信欠落は`codex.events.gap`です。SSEの`id`は接続UUIDと連番ですが、`Last-Event-ID`による履歴再生はありません。再接続後は新しいsnapshotを正本にしてください。

監視SSEの切断はTurnを中断しません。`POST /v1/responses`の生成SSE切断とは動作が異なります。

## 12. 対話承認

V1が対話中継できる承認は、Codexの`commandExecution`と`fileChange`です。追加質問、MCP elicitation、権限専用承認などはV1で回答できません。Codex App Serverの全server requestを扱う場合は`/codex`を使います。

対話承認を行う要求では、Responsesを`stream:true`にし、次を指定してください。

```json
{
  "metadata": {
    "codex.approval_capability": "interactive",
    "codex.auto_approve_workspace": "false"
  }
}
```

### `GET /v1/codex/turns/{turn_id}/approvals`

未処理承認を一覧します。

```json
{
  "turn_id": "...",
  "data": [{
    "kind": "approval_requested",
    "approval_id": "...",
    "threadId": "...",
    "turnId": "..."
  }]
}
```

### `GET /v1/codex/approvals/{approval_id}`

判断に必要な上流詳細、利用可能な判断、期限、送信状態を返します。

```json
{
  "id": "...",
  "state": "pending",
  "available_decisions": ["accept", "accept_for_session", "decline", "cancel"],
  "details": {"threadId":"...","turnId":"..."},
  "expires_at_ms": 1789080000000,
  "reply_status": "not_sent"
}
```

`details`はCodexが送った要求を保持するため、承認種別によって項目が異なります。クライアントは`available_decisions`に含まれる値だけを提示してください。

### `POST /v1/codex/approvals/{approval_id}`

```sh
curl -X POST -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d "{\"decision\":\"accept\",\"expected_turn_id\":\"$TURN_ID\",\"expected_thread_id\":\"$THREAD_ID\"}" \
  "$PROXY_BASE_URL/v1/codex/approvals/$APPROVAL_ID"
```

`decision`は取得した`available_decisions`から選びます。`expected_turn_id`と`expected_thread_id`は任意ですが、古い画面や別Turnの誤操作を防ぐため指定を推奨します。期限切れ、重複判断、利用不能な選択肢は`409 approval_rejected`です。

承認機能を宣言しないV1クライアントで承認要求が発生した場合、Proxyは要求を拒否してTurnを解放し、`approval_required`を返します。

## 13. Codex生成画像

この拡張は、Codexの画像ツールが専用Codexホームへ保存したPNGを、対応するResponseから安全に取得するためのものです。**ProxyにAPI Keyが設定されている必要があります。**

### `GET /v1/codex/responses/{response_id}/images`

完了済みResponseの実行期間内に生成されたPNGを一覧します。

```json
{
  "data": [
    {"name":"image.png","content_type":"image/png","size_bytes":123456}
  ]
}
```

1実行あたり16枚、1枚あたり10 MiBまでです。対象が0枚なら空配列です。Responseが未完了なら`409 response_not_finished`です。

### `GET /v1/codex/responses/{response_id}/images/{filename}`

PNGバイト列を返します。

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/codex/responses/$RESPONSE_ID/images/$FILENAME" \
  -o image.png
```

応答は`Content-Type: image/png`、`Cache-Control: private, no-store`、SHA-256の`ETag`を含みます。任意パス、別Thread、実行期間外の画像、シンボリックリンク／ハードリンクは取得できません。

画像API固有の主なエラーは、`401 api_key_required`、`404 response_not_found`／`generated_images_unavailable`／`image_not_found`、`409 response_not_finished`／`source_changed`、`413 too_many_images`、`415 invalid_image`、`403 source_access_denied`です。

## 14. Codex Native API

### WebSocket `GET /codex`

Codex App Serverのプロトコルを忠実に利用するエンドポイントです。一接続につき一つの専用`codex app-server`をstdioで起動し、WebSocketのテキストフレームとApp ServerのJSONLメッセージを一対一で転送します。

接続URL:

```text
ws://127.0.0.1:4040/codex
```

WebSocket Upgradeにも`Authorization: Bearer <API key>`を付けます。ブラウザー標準の`WebSocket` APIは任意のAuthorizationヘッダーを付けられないため、直接接続には適しません。サーバー側クライアントか、認証を扱うバックエンドを使用してください。

接続後は、使用中のCodex App Server仕様どおりに次の順で送ります。

Codex App Serverの現在のwire形式はJSON-RPC 2.0に基づきますが、`"jsonrpc":"2.0"`メンバーを省略します。Proxyはこのメンバーを追加・削除しないため、インストールしたCodexのSchemaに従ってください。

```json
{"method":"initialize","id":0,"params":{"clientInfo":{"name":"my_client","title":"My Client","version":"1.0.0"}}}
```

初期化応答を受信した後:

```json
{"method":"initialized","params":{}}
```

新しい会話とTurn:

```json
{"method":"thread/start","id":1,"params":{"model":"gpt-5.6-luna"}}
```

```json
{"method":"turn/start","id":2,"params":{"threadId":"<thread id>","input":[{"type":"text","text":"このリポジトリを説明して"}]}}
```

App Serverからの通知とserver requestも同じ接続に届きます。server requestには同じ`id`を使った`result`または`error`でクライアントが回答します。Proxyはmethod、params、result、error、未知フィールドを解釈して別の契約へ変換しません。

利用できる機能には、Codexバージョンが提供するThreadの開始・再開・分岐・一覧・読取り・アーカイブ、Turnの開始・Steer・中断、Itemと進捗通知、承認や入力要求、モデル・設定・MCP関連APIなどがあります。正式なメソッドとSchemaは[OpenAI公式 Codex App Serverリファレンス](https://developers.openai.com/codex/app-server)を正本とし、実際にインストールしたCodexから次のコマンドでも生成できます。

```sh
codex app-server generate-json-schema --out ./schemas
codex app-server generate-ts --out ./schemas
```

Schemaは生成したCodexバージョン固有です。実験的method／fieldを使う場合は`initialize.params.capabilities.experimentalApi: true`が必要です。

`/codex`は次を行いません。

- `/v1`形式への変換、Response IDの発行、実行台帳への保存
- Gateway、Discord、OpenWebUI向けのUIや業務ルール
- 承認、入力、MCP elicitationへの代理回答
- 切断後の未完了要求の自動再送

接続上限の既定値は16、1メッセージの既定上限は8 MiBです。バイナリ入力はclose code `1003`、JSON不正は`1007`、サイズ超過は`1009`、App Server起動・stdio・異常終了は`1011`で閉じます。接続数上限はUpgrade前にHTTP `503 codex_native_connection_limit`です。

WebSocket切断時は専用App Serverを終了します。永続化済みThreadは新しい接続で正式な`thread/resume`を使用して復元してください。

## 15. 主なエラーコード

| HTTP／経路 | Code | 意味／対処 |
| ---: | --- | --- |
| 400 | `invalid_request_error` | JSON型、必須項目、入力形式を確認 |
| 400 | `unsupported_parameter` | この互換サブセットで扱わない指定を除くか`/codex`を使用 |
| 400 | `invalid_request_id` | Idempotency-Keyを1〜128文字の指定文字へ修正 |
| 400 | `invalid_cwd` | 許可済みで実在する作業ディレクトリを指定 |
| 401 | `invalid_api_key` | Bearer認証を確認 |
| 401 | `provider_authentication_required` | 対象Providerへ再ログイン |
| 404 | `model_not_found` | `/v1/models`から公開IDを選択 |
| 404 | `thread_not_found` | 会話の保存・認証・Codexホームを確認 |
| 404 | `turn_not_found` | このProxyが記録したTurn IDか確認 |
| 404 | `request_not_found` | 記録なし。要求が未送信だった証明にはならない |
| 409 | `request_conflict` | 同じIdempotency-Keyを異なる本文へ再利用しない |
| 409 | `response_not_continuable` | 成功したResponseから継続 |
| 409 | `thread_busy` | 先行Turnの終端を確認してから次を開始 |
| 409 | `turn_state_unknown` | 成功や停止済みと推測せず状態を再確認 |
| 409 | `turn_not_steerable` | 実行中で承認待ちでないTurnだけSteer可能 |
| 409 | `approval_required` | 対話承認を実装するか`/codex`を使用 |
| 409 | `unsupported_interaction` | V1外のserver request。`/codex`を使用 |
| 409 | `approval_rejected` | 期限、現在状態、選択肢を再取得 |
| 502 | `runtime_error` | Codex App Serverのエラーまたは通信障害 |
| 503 | `provider_unavailable` | Providerの稼働・カタログ状態を確認 |
| 503 | `control_store_unavailable` | 自動再実行せず、永続ストアを復旧 |
| 503 | `codex_native_unavailable` | `/codex`用App Serverを起動できる設定か確認 |
| 503 | `codex_native_connection_limit` | `/codex`接続数を減らす |
| 504 | `runtime_timeout` | 非stream要求が無通信で時間切れ。Turn状態を確認し、自動再実行しない |

stream開始後の無通信タイムアウトは、HTTP状態の変更ではなく`response.failed`の`runtime_idle_timeout`として通知されます。

実行開始後の通信断や`unknown`は、失敗とも未実行とも断定できません。要求ID、Response ID、Turn IDを使って照会し、同じ処理を無条件に再実行しないでください。

## 16. 実装していないOpenAI API

このProxyはOpenAI API全体を再実装するものではありません。現時点で、少なくとも次は提供しません。

- Files、Embeddings、Audio、Fine-tuning、BatchなどのOpenAIエンドポイント
- OpenAI Images APIとしての画像生成
- クライアント定義Function／Tool callingの往復
- Usageの完全なOpenAI互換集計
- V1最終回答の再取得、SSE履歴再生
- V1での任意のCodex server request中継

Codex App Serverが持つ機能を直接必要とする場合は`/codex`を使い、製品固有の表示・認可・保存は利用側アプリケーションで実装します。
