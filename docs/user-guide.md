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
| `security.allowed_cwds` | Existing canonical directory roots Codex may use |
| `security.api_key` / `api_key_env` | Client authentication; required for non-loopback |
| `defaults.model` | Public model ID used when a request omits `model` |
| `approval.timeout_seconds` | Approval expiry interval |
| `approval.auto_approve_workspace` | Automatically accepts operations Codex reports inside the requested workspace; default `true` |
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

The endpoint asks Codex App Server's standard `thread/read` method for the current
thread and Turn state. It reports `inProgress`, `completed`, `interrupted`, or
`failed`, together with the latest Codex error when available. It also includes the
last event observed by the Proxy, which helps distinguish a live long-running Turn
from a Turn whose event stream has stopped.

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

The MVP supports `model`, `input`, `previous_response_id`, `stream`, `metadata`, and ChatGPT-only
`reasoning`. Standard events include response creation, text deltas, tool calls/results where present,
usage, completion, cancellation, and errors. Codex-specific events are not forced into standard fields.

Use the returned response ID to continue a durable Responses conversation:

```json
{
  "model": "chatgpt/gpt-5.6-luna",
  "previous_response_id": "resp_123",
  "input": "Continue."
}
```

After a proxy restart, continuation works only while the Codex-side thread remains available. Otherwise
the proxy returns `thread_not_found`; it does not reconstruct a thread from conversation text.

## Chat Completions API

OpenWebUI uses this endpoint:

```sh
curl -N -H "Authorization: Bearer $PROXY_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"ollama/gemma4:e4b","messages":[{"role":"user","content":"Say OK."}],"stream":true}' \
  http://127.0.0.1:4040/v1/chat/completions
```

This is an OpenAI-compatible subset, not a promise to implement every OpenAI field. The supported MVP
surface is text messages, `model`, `stream`, and `metadata` plus the proxy's tool/approval flow. Check
the configured model capability before relying on tools, multimodal input, `tool_choice`, or other
advanced fields.

## Errors and approvals

- `401 invalid_api_key`: client authentication failed.
- `404 model_not_found`: the public model ID is not registered.
- `409 approval_required`: a client without approval capability reached a tool approval request. The
  proxy declines/cancels the Codex-side request and releases the turn; it does not wait for a timeout.
- `409 thread_not_found`: a requested durable Responses thread is unavailable.
- `400 unsupported_parameter`: a provider-specific option was sent to the wrong provider.
- `turn_failed` or a failed completion: the response includes the Codex failure detail when available.

Approval remains pending until accepted, declined, cancelled, or expired. The provider permit is held
through the turn, including approval waiting, by design in the MVP. A client disconnect cancels the turn.
When `approval.auto_approve_workspace = true`, operations reported by Codex as running in the requested
workspace are accepted automatically. This is intended for trusted local work and does not make the
Proxy safe for untrusted users; keep the cwd allowlist narrow. Operations outside the workspace still
use the interactive approval flow.

## Security and operations

- Bind to loopback unless remote access is required.
- For remote access, use an API key and place TLS at a reverse proxy.
- Keep CORS disabled unless you have a specific trusted browser deployment.
- Keep allowed working-directory roots narrow and existing.
- Event Journal files are metadata-oriented and should be rotated and retained according to local policy;
  command output and file content are size-limited/redacted when recorded.
- Do not expose Codex execution to untrusted users. The client API key is not a substitute for approval
  or filesystem policy.
