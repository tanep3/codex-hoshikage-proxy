# User and API Guide

[日本語](user-guide.ja.md) | **English**

## Configuration

The default configuration file is:

```text
~/.config/codex-hoshikage-proxy/config.toml
```

Set `CODEX_HOSHIKAGE_PROXY_CONFIG` to use another file. The proxy configuration is declarative and
the proxy-owned Codex `config.toml` is regenerated from it. Do not hand-edit generated Codex config.

Important settings:

| Setting | Meaning |
| --- | --- |
| `server.host`, `server.port` | Listener address |
| `server.default_cwd` | Existing default working directory |
| `server.turn_idle_timeout_seconds` | Maximum silence between Codex App Server events for one Turn; default `600`. This is not a total task limit. |
| `server.turn_stall_detection_seconds` | Silence interval before the Proxy probes Codex for a possible stalled Turn; default `180`. |
| `server.turn_stall_confirmation_count` | Consecutive no-progress probes required before `turn_stalled`; default `3`. |
| `server.turn_heartbeat_seconds` | SSE heartbeat interval during a running Turn; default `30`. |
| `security.allowed_cwds` | Existing canonical directory roots Codex may use |
| `security.api_key` / `api_key_env` | Client authentication; required for non-loopback |
| `defaults.model` | Default for new conversations and Chat when `model` is omitted; Responses continuations inherit the latest selection in the thread |
| `approval.timeout_seconds` | Approval expiry interval |
| `approval.auto_approve_workspace` | Automatically accepts operations Codex reports inside the requested workspace; default `true` |
| `codex.sandbox.mode` | Codex sandbox mode used for new threads; `workspace-write` is the default |
| `codex.sandbox.writable_roots` | Additional existing absolute directories Codex may write in workspace-write mode |
| `codex.sandbox.network_access` | Allows outbound network access from workspace-write commands; default `false` |
| provider `enabled` | Enables a provider |
| provider `max_concurrent_turns` | Provider concurrency limit |
| `models."provider/model"` | Public-to-upstream model mapping |

Provider and model are one selection in the public model ID. Reasoning effort is independent of that
selection, but is currently accepted only for `chatgpt/...` models. Supported values follow the Codex
model configuration. For non-ChatGPT providers, omit reasoning or use the provider's default.

## Unified model list

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  http://127.0.0.1:4040/v1/models
```

The result is an OpenAI-compatible list. Entries use IDs such as:

```text
chatgpt/gpt-5.6-luna
hoshikage/unsloth-gemma4-12b-qat-thinking-off
ollama/gemma4:e4b
```

## Turn status diagnostics

The Proxy exposes a diagnostic endpoint for an active Codex Turn:

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  http://127.0.0.1:4040/v1/codex/turns/{turn_id}/status
```

The endpoint queries Codex App Server with `thread/read` and `includeTurns=true`.
`status` is `inProgress`, `completed`, `interrupted`, `failed`, or `unknown` when the
current state cannot be queried. Persisted observations are returned separately as
`last_observed_status` and `last_observed_at_ms`. The compatibility field
`last_event_at_ms` also means this status observation time, not the last progress
event time. Do not interpret `unknown` as completion or failure.

`server.turn_idle_timeout_seconds` is an inactivity timeout: it limits how long the
Proxy waits without receiving any Codex App Server event. It is not a total task
duration limit. When it expires, the Proxy interrupts the Codex Turn and returns
`runtime_idle_timeout`. The default is 600 seconds.

Before that hard limit, `server.turn_stall_detection_seconds` makes the Proxy query
Codex with `thread/read`. Approval waits are kept alive. If Codex still reports an
active Turn without progress events, the Proxy interrupts it and returns
`turn_stalled`.

Hoshikage discovery combines its ordinary model list with detailed capability information. A model
whose detailed metadata says `tools: false` is not exposed as a dynamic Codex model, because Codex
agent turns require tool calling. The proxy does not guess tool support from a model name.
Hoshikage and Ollama catalog requests time out after five seconds. If either service is stopped or
unavailable, the proxy continues starting and omits that provider's dynamic models.
The catalog is refreshed whenever `/v1/models` is requested or a model is used, so a provider that
comes back online is detected automatically on the next request.

## Responses API

Non-streaming:

```sh
curl -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"chatgpt/gpt-5.6-luna","input":"Say OK."}' \
  http://127.0.0.1:4040/v1/responses
```

Streaming:

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"hoshikage/unsloth-gemma4-12b-qat-thinking-off","input":"Say OK.","stream":true}' \
  http://127.0.0.1:4040/v1/responses
```

The proxy supports `model`, `input`, `previous_response_id`, `stream`, `metadata`, `text.format`, and
ChatGPT-only `reasoning`. Standard output currently focuses on response creation, text deltas, and
completion/failure. Full OpenAI tool-call/result and usage conversion remains incomplete. Approvals
use the Codex extension API.

Use the returned response ID to continue a durable Responses conversation:

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "previous_response_id": "resp_123",
  "input": "Continue."
}
```

After a proxy restart, the proxy uses `thread/resume` to reload the persisted Codex thread when available. Otherwise
the proxy returns `thread_not_found`; it does not reconstruct a thread from conversation text.

Only successful Responses can be continued. Specify `model` to change models within the same
provider for the next Turn while keeping thread history. If omitted, the model is inherited from
the latest accepted Turn start in that thread, even when referencing an older Response.
Cross-provider changes and concurrent execution in one thread return 409. The bundled OpenWebUI
Pipe still starts a new thread when its model selection changes.

## Chat Completions API

Use this endpoint for ordinary OpenAI-compatible Chat clients. The bundled OpenWebUI Pipe uses the Responses API.

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"ollama/gemma4:e4b","messages":[{"role":"user","content":"Say OK."}],"stream":true}' \
  http://127.0.0.1:4040/v1/chat/completions
```

This is an OpenAI-compatible subset, not a promise to implement every OpenAI field. The supported MVP
surface includes text/image messages, `model`, `stream`, `metadata`, `response_format`, ChatGPT-only
`reasoning_effort`, and the proxy approval flow. This does not provide full compatibility with
client-defined tool calling or every advanced OpenAI field.

## Errors and approvals

- `401 invalid_api_key`: client authentication failed.
- `404 model_not_found`: the public model ID is not registered.
- `409 approval_required`: a client without approval capability reached a tool approval request. The
  proxy declines/cancels the Codex-side request and releases the turn; it does not wait for a timeout.
- `404 thread_not_found`: a requested durable Responses thread is unavailable.
- `400 unsupported_parameter`: a provider-specific option was sent to the wrong provider.
- `turn_failed` or a failed completion: the response includes the Codex failure detail when available.

Approval remains pending until accepted, declined, cancelled, or expired. The provider permit is held
through the turn, including approval waiting, by design in the MVP. A client disconnect cancels the turn.
When `approval.auto_approve_workspace = true`, operations reported by Codex as running in the requested
workspace are accepted automatically. This is intended for trusted local work and does not make the
Proxy safe for untrusted users; keep the cwd allowlist narrow. Operations outside the workspace still
use the interactive approval flow. All reported paths must be inside the workspace; requests with
any outside path or no known target paths are not automatically accepted.

The sandbox settings are separate from Proxy approval. `codex.sandbox.writable_roots` is written into
the generated Codex configuration; it must contain existing absolute paths. Workspace auto-approval
uses only structured paths supplied by Codex (`cwd`, `path`, `file_path`, `filePath`, `target_path`,
`targetPath`, `grantRoot`, `paths`, `fileChanges`, and paths in `commandActions`), including file
change targets reported by `item/started`. It never trusts a workspace path merely because it appears inside a shell command.
Set `network_access = true` only for trusted local skills that need outbound network access.

## Security and operations

- Bind to loopback unless remote access is required.
- For remote access, use an API key and place TLS at a reverse proxy.
- Keep CORS disabled unless you have a specific trusted browser deployment.
- Keep allowed working-directory roots narrow and existing.
- Event Journal and execution records have no automatic rotation or expiry. Monitor disk usage.
- Preserve `state/responses/mappings.jsonl` and `executions.jsonl` during updates and recovery; they
  support conversation continuation and duplicate prevention. Only one Proxy may own a state directory.
- The execution ledger stores metadata, not prompts or final output for later retrieval.
- Do not expose Codex execution to untrusted users. The client API key is not a substitute for approval
  or filesystem policy.

## Images, structured output, and model pagination

Responses accepts string input, arrays of text/image items, and messages with `role` and `content`.
Chat Completions accepts string content or arrays of text and image parts. Message roles retain the
existing `[role]` text projection; this does not create separate App Server system instructions.

- Responses image: `{"type":"input_image","image_url":"https://example.com/image.png","detail":"high"}`.
- Chat image: `{"type":"image_url","image_url":{"url":"https://example.com/image.png","detail":"high"}}`.
- Image URLs may use HTTP(S) or `data:image/...`. Local paths, `file://`, and Files API IDs are unsupported.
  `detail` accepts `auto`, `low`, `high`, or `original`. Actual image support depends on the model and Codex environment.
- Responses structured output: `text.format: {"type":"json_schema","name":"answer","schema":{...},"strict":true}`.
- Chat structured output: `response_format: {"type":"json_schema","json_schema":{"name":"answer","schema":{...},"strict":true}}`.
  The schema is forwarded as `turn/start.outputSchema`; the final answer remains a JSON string. The proxy does
  not independently validate generated JSON. `name` and `strict` are not separate Codex options.
  Output formats other than `text` and `json_schema` return HTTP 400.
- ChatGPT Chat Completions accepts `reasoning_effort`, validated against the model's advertised choices.
- ChatGPT catalogs follow `nextCursor` until exhausted. Repeated cursors, more than 100 pages, and the
  five-second total deadline fail catalog discovery.

See the [App Server coverage notes](app-server-coverage.md) for remaining gaps.

Live validation with Codex 0.153.4 and gpt-5.6-luna reproduced a color error with `detail=low`,
including when bypassing this proxy. The same image was recognized correctly with `detail=high`.
See the [live validation report](live-codex-validation.md).

## Execution control

The [Control API v1 contract (Japanese)](control-api.ja.md) covers start identity, Idempotency-Key, status lookup, steer, interrupt, approval suppression and conversation model changes. Model changes within one provider apply to the next turn while preserving thread history.

Use `Idempotency-Key` on Responses for request-ID lookup. Repeating the same key and body returns
execution metadata, not replayed output or SSE. Observer SSE reconnects provide snapshots, not
historical event replay. Disconnecting generation interrupts the Turn; disconnecting observation does not.
Control APIs assume a shared operator API key and do not isolate individual users.
