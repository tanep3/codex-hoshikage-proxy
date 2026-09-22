# API Reference

[日本語](api-reference.ja.md) | **English**

This is the user-facing reference for every current Codex Hoshikage Proxy endpoint. It explains what each endpoint does, what to send, and what it returns.

For detailed concurrency and recovery rules, also see the [execution control contract](control-api.ja.md). For Codex App Server coverage, see the [coverage matrix](app-server-coverage.md).

## 1. Choose an API

| Goal | API |
| --- | --- |
| Send text or images from an OpenAI-compatible application | `POST /v1/responses` or `POST /v1/chat/completions` |
| Continue conversations and use status, interrupt, steer, and interactive approvals | `POST /v1/responses` plus `/v1/codex/*` |
| Use Codex App Server capabilities without translating them to OpenAI shapes | WebSocket `GET /codex` |
| Monitor process and runtime readiness | `GET /healthz` and `GET /readyz` |

`/v1` is an **OpenAI-compatible subset**. The Proxy does not guess how to translate every Codex App Server feature into OpenAI objects. Clients that need App Server threads, turns, items, notifications, approvals, MCP, or experimental APIs should use `/codex`.

## 2. Common conventions

### Base URL

The examples use a Proxy listening on port 4040 on the same machine.

```sh
export PROXY_BASE_URL=http://127.0.0.1:4040
export PROXY_API_KEY='your configured API key'
```

Replace `127.0.0.1` with the Proxy host address for LAN access. Use a TLS-terminating reverse proxy before exposing it over the internet.

### Authentication

When the Proxy has an API key, **every HTTP endpoint and the `/codex` WebSocket Upgrade** requires:

```http
Authorization: Bearer <API key>
```

This includes `/healthz` and `/readyz`. Authentication failure returns HTTP `401`:

```json
{
  "error": {
    "message": "API key is missing or invalid",
    "type": "invalid_request_error",
    "code": "invalid_api_key"
  }
}
```

All clients sharing one API key are one trusted operator from the Proxy's perspective. There is no per-user ownership boundary or ACL.

### JSON errors

HTTP errors normally use this shape:

```json
{
  "error": {
    "message": "human-readable detail",
    "type": "invalid_request_error",
    "code": "machine_readable_code"
  }
}
```

Check both the HTTP status and `error.code`.

### Request body size

The maximum HTTP request body is 16 MiB, including image data URLs.

## 3. Endpoint index

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/healthz` | Process liveness |
| `GET` | `/readyz` | Codex runtime and control-store readiness |
| `GET` | `/v1/models` | Currently available public models |
| `POST` | `/v1/responses` | Responses-compatible execution, continuation, and streaming |
| `POST` | `/v1/chat/completions` | Chat Completions-compatible one-shot execution |
| `GET` | `/v1/codex/capabilities` | Extension capabilities, limits, and provider status |
| `GET` | `/v1/codex/requests/{request_id}` | Execution record for an Idempotency-Key |
| `GET` | `/v1/codex/responses/{response_id}` | Response execution record and continuation eligibility |
| `GET` | `/v1/codex/turns/{turn_id}/status` | Current Codex turn status |
| `POST` | `/v1/codex/turns/{turn_id}/interrupt` | Request turn interruption |
| `POST` | `/v1/codex/turns/{turn_id}/steer` | Add input to an active turn |
| `GET` | `/v1/codex/turns/{turn_id}/events/stream` | Status, approval, and upstream event monitoring SSE |
| `GET` | `/v1/codex/turns/{turn_id}/approvals` | Pending approvals for a turn |
| `GET` | `/v1/codex/approvals/{approval_id}` | Approval details and state |
| `POST` | `/v1/codex/approvals/{approval_id}` | Submit an approval decision |
| `GET` | `/v1/codex/responses/{response_id}/images` | List generated PNG images |
| `GET` | `/v1/codex/responses/{response_id}/images/{filename}` | Download a generated PNG |
| `GET` | `/codex` | Codex App Server JSON-RPC over WebSocket |

The removed `/v2/codex` API returns `404`.

## 4. Health checks

### `GET /healthz`

Confirms that the HTTP process can answer requests. It does not prove that an upstream provider is usable.

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/healthz"
```

```json
{"status":"ok"}
```

### `GET /readyz`

Returns `200` when the shared Codex App Server is ready and the durable control store is readable.

```json
{"status":"ready"}
```

Otherwise it returns `503`:

```json
{"status":"not_ready"}
```

Readiness does not guarantee provider authentication or model availability. Use `/v1/codex/capabilities` and `/v1/models` for those checks.

## 5. Models

### `GET /v1/models`

Returns models currently publishable from configuration and dynamic provider catalogs.

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  "$PROXY_BASE_URL/v1/models"
```

```json
{
  "object": "list",
  "data": [{
    "id": "chatgpt/gpt-5.6-luna",
    "object": "model",
    "created": 0,
    "owned_by": "chatgpt"
  }]
}
```

Public IDs use `provider/model`, for example `chatgpt/...`, `hoshikage/...`, or `ollama/...`. Models can disappear while a provider is unauthenticated, unavailable, or lacks required capabilities.

## 6. Responses-compatible API

### `POST /v1/responses`

Starts a new conversation or continues a previously successful Response. New clients that need the control API should prefer this endpoint.

#### Minimal request

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"chatgpt/gpt-5.6-luna","input":"Reply with OK."}' \
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

Successful responses also include available identifiers in these headers:

```text
x-response-id: resp_...
x-codex-thread-id: ...
x-codex-turn-id: ...
```

#### Supported fields

| Field | Type | Meaning |
| --- | --- | --- |
| `model` | optional string | Public model ID. Uses the configured default for a new conversation or the latest conversation model for a continuation |
| `input` | required string or array | Text, images, or role-bearing messages |
| `previous_response_id` | optional string | Continue a successful Response's conversation |
| `stream` | optional boolean | Return SSE when `true`; default `false` |
| `metadata` | optional object | String-valued metadata, including the `codex.*` options below |
| `reasoning.effort` | optional string | Reasoning level for a supporting ChatGPT model |
| `text.format` | optional object | `text` or `json_schema` |

Proxy metadata extensions:

| Key | Value | Effect |
| --- | --- | --- |
| `codex.cwd` | allowed absolute path | Working directory for a new conversation; cannot change during continuation |
| `codex.approval_capability` | `interactive` | Declares that the client can use the approval endpoints |
| `codex.auto_approve_workspace` | `false` | Suppresses workspace auto-approval for this conversation. `true` cannot exceed the server setting |

Metadata values are JSON strings, not booleans.

#### Inputs and images

A plain string is accepted:

```json
{"input":"Explain this repository."}
```

Messages and images are also accepted:

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "input": [{
    "role": "user",
    "content": [
      {"type":"input_text","text":"Describe this image."},
      {"type":"input_image","image_url":"https://example.com/image.png","detail":"high"}
    ]
  }]
}
```

Supported roles are `system`, `developer`, `user`, and `assistant`. Roles are converted to text labels such as `[system]`; they are not independent App Server system messages.

Images must be HTTP(S) URLs or `data:image/...` URLs. `detail` accepts `auto`, `low`, `high`, and `original`. The Proxy rejects `file://`, local server paths, and OpenAI Files API IDs.

#### JSON Schema output

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "input": "Return a JSON answer.",
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

The Proxy passes `schema` to Codex and returns the result as a JSON string. It does not validate the final output again. `name` and `strict` do not create an additional Proxy guarantee.

#### Conversation continuation

```json
{
  "previous_response_id": "resp_...",
  "input": "Continue the explanation."
}
```

Only successful Responses are continuable. A model may change within the same provider. Cross-provider changes, workspace changes, and concurrent runs on one thread return `409`. Continuation appends a turn to the same thread; it does not branch or rewind history.

#### Duplicate-execution protection

Set `Idempotency-Key` to a 1–128 character identifier containing only ASCII letters, digits, `-`, `_`, or `.`.

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: req-8d529a' \
  -d '{"model":"chatgpt/gpt-5.6-luna","input":"Investigate.","stream":true}' \
  "$PROXY_BASE_URL/v1/responses"
```

Reposting the same key with the same JSON body does not start a second turn. It returns execution metadata as JSON, not the prior answer or SSE. Reusing the key with a different body returns `409 request_conflict`. After an ambiguous transport failure, query the request record before considering another execution.

#### Streaming

`stream:true` returns `text/event-stream` with these principal events:

| Event | Meaning |
| --- | --- |
| `response.created` | Response ID, model, and `in_progress` state |
| `response.output_text.delta` | Incremental text in `delta` |
| `codex.turn.status` | Long-running, status-probe, or approval-wait heartbeat |
| `response.completed` | Successful completion |
| `response.failed` | Failure code or upstream error |

```text
event: response.output_text.delta
data: {"id":"resp_...","output_index":0,"item_id":"msg_1","delta":"OK"}

event: response.completed
data: {"id":"resp_...","object":"response","status":"completed","model":"chatgpt/gpt-5.6-luna"}
```

Responses streaming ends after `response.completed` or `response.failed`; it does not send `[DONE]`. Disconnecting the generation stream requests interruption of that turn.

#### Unsupported fields

Client-defined `tools`, `functions`, active `tool_choice`/`function_call`, `parallel_tool_calls:true`, and client tool-call history return `400 unsupported_parameter`. No-op values such as empty arrays, `none`, and `false` are accepted.

The endpoint does not provide complete OpenAI tool-call/result objects, usage, or later retrieval of the final output.

## 7. Chat Completions-compatible API

### `POST /v1/chat/completions`

This endpoint serves existing Chat Completions clients. Every request uses a new ephemeral thread; it does not support `previous_response_id`. Use Responses for continuation and reliable control.

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"ollama/gemma4:e4b","messages":[{"role":"user","content":"Reply with OK."}]}' \
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

Supported fields are `model`, `messages`, `stream`, string-valued `metadata`, ChatGPT `reasoning_effort`, and `response_format`. Message roles, text, images, and client-defined tool restrictions match Responses.

JSON Schema uses the Chat Completions shape:

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

With `stream:true`, the Proxy sends OpenAI-style `chat.completion.chunk` objects as `data:` and ends with `data: [DONE]`. The response includes `x-codex-turn-id`. Disconnecting requests turn interruption.

## 8. Capabilities and provider status

### `GET /v1/codex/capabilities`

Use this endpoint to discover extension features and limitations instead of hardcoding assumptions. It also refreshes the model catalog.

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

Provider status may be `available`, `authentication_required`, or `temporarily_unavailable`. Treat `reason` as extensible diagnostic data.

## 9. Execution records and turn status

### `GET /v1/codex/requests/{request_id}`

Returns the durable execution record associated with an `Idempotency-Key`.

### `GET /v1/codex/responses/{response_id}`

Returns the execution record for a Response and adds `continuable`.

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

`phase` records the Proxy dispatch boundary and may be `received`, `dispatching`, `started`, `rejected`, `finished`, or `unknown`. Use the turn status endpoint for current Codex state. Execution records do not contain prompts or final answers.

### `GET /v1/codex/turns/{turn_id}/status`

Queries Codex App Server with `thread/read` and returns both current state and the Proxy's stored observation.

`status` is normally `inProgress`, `completed`, `failed`, `interrupted`, or `unknown` when it cannot be established. The response also includes `turn`, `thread_status`, `pending_approvals`, and `runtime_query_error`. Never infer success, failure, or cancellation from `unknown`.

## 10. Interrupt and steer

### `POST /v1/codex/turns/{turn_id}/interrupt`

Send an empty-body POST to request interruption.

```json
{"turn_id":"...","result":"accepted","status":"not_yet_confirmed"}
```

HTTP `202 accepted` confirms only that Codex accepted the request. Query turn status for the outcome. A terminal turn returns `200 already_terminal`; a turn that cannot be confirmed active returns `409 turn_state_unknown`. Interruption does not roll back file changes.

### `POST /v1/codex/turns/{turn_id}/steer`

Adds input to the same active turn.

```json
{
  "expected_turn_id": "<same ID as the URL>",
  "input": "Run tests first."
}
```

Success is `202` with `{"turn_id":"...","result":"accepted"}`. The URL and `expected_turn_id` must match. Terminal, unknown, interrupting, approval-waiting, or input-waiting turns return `409 turn_not_steerable`. Steer has no idempotency key; do not automatically resend after a lost response.

## 11. Turn monitoring events

### `GET /v1/codex/turns/{turn_id}/events/stream`

This monitoring SSE does not own the execution. It first sends:

1. `codex.events.reset`, indicating a new connection without history replay
2. `codex.turn.snapshot`, containing current status and pending approvals
3. Current `approval_requested` events

It then forwards matching upstream notifications and Proxy approval events under their original `kind` or `method` name. `codex.events.gap` reports missed events. SSE IDs contain a connection UUID and sequence number, but `Last-Event-ID` does not replay history. Treat the new snapshot as authoritative after reconnecting.

Disconnecting this monitor does not interrupt the turn. This differs from disconnecting the generation stream returned by `POST /v1/responses`.

## 12. Interactive approvals

V1 can relay Codex `commandExecution` and `fileChange` approvals. It cannot answer user-input requests, MCP elicitation, or permission-specific approvals. Use `/codex` for the complete App Server server-request surface.

For interactive approval, use Responses with `stream:true` and:

```json
{
  "metadata": {
    "codex.approval_capability": "interactive",
    "codex.auto_approve_workspace": "false"
  }
}
```

### `GET /v1/codex/turns/{turn_id}/approvals`

Returns current pending approvals as `{"turn_id":"...","data":[...]}`. Obtain `approval_id` from these records; do not guess IDs.

### `GET /v1/codex/approvals/{approval_id}`

Returns upstream details, valid decisions, expiry, and reply state:

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

`details` varies by approval kind. Only display choices from `available_decisions`.

### `POST /v1/codex/approvals/{approval_id}`

```json
{
  "decision": "accept",
  "expected_turn_id": "...",
  "expected_thread_id": "..."
}
```

Choose `decision` from the current `available_decisions`. The expected IDs are optional but recommended to prevent stale or cross-turn decisions. Expired, duplicate, or unavailable decisions return `409 approval_rejected`.

If a V1 client does not declare interactive approval capability and Codex requests approval, the Proxy rejects the request, releases the turn, and returns `approval_required`.

## 13. Codex-generated images

These extensions retrieve PNGs written by Codex image tools into the dedicated Codex home. **The Proxy must have an API key configured.**

### `GET /v1/codex/responses/{response_id}/images`

Lists PNG files generated during a completed Response:

```json
{
  "data": [
    {"name":"image.png","content_type":"image/png","size_bytes":123456}
  ]
}
```

Limits are 16 images per execution and 10 MiB per image. No images returns an empty array. An unfinished Response returns `409 response_not_finished`.

### `GET /v1/codex/responses/{response_id}/images/{filename}`

Returns PNG bytes with `Content-Type: image/png`, `Cache-Control: private, no-store`, and a SHA-256 `ETag`. Arbitrary paths, other threads, out-of-window files, symlinks, and hard links are inaccessible.

Principal image-specific errors are `401 api_key_required`, `404 response_not_found` / `generated_images_unavailable` / `image_not_found`, `409 response_not_finished` / `source_changed`, `413 too_many_images`, `415 invalid_image`, and `403 source_access_denied`.

## 14. Codex Native API

### WebSocket `GET /codex`

This endpoint exposes the Codex App Server protocol faithfully. Each WebSocket connection starts one dedicated `codex app-server` over stdio. One WebSocket text frame maps to one App Server JSONL message in each direction.

```text
ws://127.0.0.1:4040/codex
```

Include `Authorization: Bearer <API key>` in the WebSocket Upgrade. The browser's built-in `WebSocket` API cannot set an arbitrary Authorization header, so use a server-side client or an authenticated backend.

Initialize the connection according to the installed App Server protocol:

The current App Server wire format follows JSON-RPC 2.0 but omits the `"jsonrpc":"2.0"` member. The Proxy neither adds nor removes that member, so follow the schema generated by the installed Codex version.

```json
{"method":"initialize","id":0,"params":{"clientInfo":{"name":"my_client","title":"My Client","version":"1.0.0"}}}
```

After receiving the initialize result:

```json
{"method":"initialized","params":{}}
```

Then start a thread and turn:

```json
{"method":"thread/start","id":1,"params":{"model":"gpt-5.6-luna"}}
```

```json
{"method":"turn/start","id":2,"params":{"threadId":"<thread id>","input":[{"type":"text","text":"Explain this repository."}]}}
```

Notifications and server requests arrive on the same connection. Reply to a server request with the same `id` and a `result` or `error`. The Proxy does not reinterpret methods, params, results, errors, or unknown fields.

Available capabilities are those of the installed Codex version and may include thread creation, resume, fork, list, read, and archive; turn start, steer, and interrupt; item and progress events; approval and input requests; and model, configuration, and MCP APIs. The [official OpenAI Codex App Server reference](https://developers.openai.com/codex/app-server) is authoritative. Generate version-matched schemas from the installed binary with:

```sh
codex app-server generate-json-schema --out ./schemas
codex app-server generate-ts --out ./schemas
```

Experimental methods and fields require `initialize.params.capabilities.experimentalApi: true`.

`/codex` does not provide `/v1` conversion, Response IDs, the `/v1` execution ledger, product-specific UI or policy, proxy answers to server requests, or automatic replay after disconnect.

Defaults are 16 simultaneous connections and 8 MiB per message. Binary input closes with code `1003`, invalid JSON with `1007`, oversized messages with `1009`, and child startup, stdio, or abnormal exit with `1011`. An exhausted connection limit returns HTTP `503 codex_native_connection_limit` before Upgrade.

Disconnecting ends the dedicated App Server. Resume a persisted thread from a new connection with the formal `thread/resume` method.

## 15. Common error codes

| HTTP / surface | Code | Meaning / action |
| ---: | --- | --- |
| 400 | `invalid_request_error` | Check required fields, JSON types, and input shape |
| 400 | `unsupported_parameter` | Remove unsupported subset fields or use `/codex` |
| 400 | `invalid_request_id` | Use a valid 1–128 character Idempotency-Key |
| 400 | `invalid_cwd` | Use an existing, allowed working directory |
| 401 | `invalid_api_key` | Check Bearer authentication |
| 401 | `provider_authentication_required` | Reauthenticate the provider |
| 404 | `model_not_found` | Select a public ID from `/v1/models` |
| 404 | `thread_not_found` | Check stored state, identity, and Codex home |
| 404 | `turn_not_found` | Use a turn recorded by this Proxy |
| 404 | `request_not_found` | No record; this does not prove the request was never sent |
| 409 | `request_conflict` | Never reuse an Idempotency-Key for a different body |
| 409 | `response_not_continuable` | Continue from a successful Response |
| 409 | `thread_busy` | Wait until the earlier turn is confirmed terminal |
| 409 | `turn_state_unknown` | Recheck state; do not infer success or cancellation |
| 409 | `turn_not_steerable` | Steer only a running turn that is not waiting for approval |
| 409 | `approval_required` | Implement interactive approval or use `/codex` |
| 409 | `unsupported_interaction` | V1 cannot answer this server request; use `/codex` |
| 409 | `approval_rejected` | Refresh expiry, state, and available decisions |
| 502 | `runtime_error` | Codex App Server or transport failure |
| 503 | `provider_unavailable` | Check provider and catalog health |
| 503 | `control_store_unavailable` | Do not automatically rerun; restore the durable store |
| 503 | `codex_native_unavailable` | Check that the `/codex` App Server can be started |
| 503 | `codex_native_connection_limit` | Reduce `/codex` connections |
| 504 | `runtime_timeout` | A non-streaming request timed out without events; check turn status and do not automatically rerun |

After streaming has started, an idle timeout is reported inside a `response.failed` event as `runtime_idle_timeout`; it cannot change the established HTTP status.

After execution begins, a transport failure or `unknown` state proves neither failure nor non-execution. Query with the request, Response, and turn IDs before deciding what to do.

## 16. OpenAI APIs not implemented

This Proxy does not reimplement the entire OpenAI API. It does not currently provide:

- OpenAI Files, Embeddings, Audio, Fine-tuning, Batch, and similar endpoints
- Image generation as the OpenAI Images API
- Round-trip client-defined function/tool calling
- Complete OpenAI-compatible usage accounting
- Retrieval of final V1 output or replay of old SSE events
- Arbitrary Codex server-request relay through V1

Use `/codex` when an application needs a Codex App Server capability directly. Product-specific display, authorization, and storage remain the consuming application's responsibility.
