[日本語](README.ja.md) | **English**

# Codex Hoshikage Proxy

## Use Codex from any OpenAI-compatible client

You already have an app that can talk to the OpenAI API?
Point it at this proxy and use Codex from the same kind of interface.

That means you can use Codex from OpenWebUI, scripts, and other OpenAI-compatible tools while
choosing the model you want: ChatGPT/Codex subscription models, Hoshikage, or Ollama.

## Why is this useful?

- **One API endpoint.** Your client only needs to know the proxy. You do not have to reconfigure it
  every time you switch providers.
- **Use your Codex subscription.** Sign in to Codex with ChatGPT and use the Codex models available
  to your account. No OpenAI Platform API key is needed for that login method.
- **Mix local and cloud models.** Try Hoshikage or Ollama locally, then switch to a ChatGPT model
  with a model selector.
- **Keep Codex's useful powers.** Codex can work with files and run commands, with approval prompts
  and an allowed-directory list to keep things under control.
- **Works with OpenWebUI.** The included Pipe turns the proxy into a model source for OpenWebUI.

## What can you do with it?

Pick a model from the unified list and send a normal OpenAI-style request:

```text
chatgpt/gpt-5.6-luna
hoshikage/unsloth-gemma4-12b-qat-thinking-off
ollama/gemma4:e4b
```

The proxy provides:

- OpenAI-compatible `GET /v1/models`.
- Responses API at `POST /v1/responses`.
- Chat Completions API at `POST /v1/chat/completions`.
- Streaming text deltas, completion/failure events, and turn interruption on disconnect.
- Image inputs (URLs/data URLs) and JSON Schema output constraints.
- Codex file and shell operations, approval handling, and working-directory allowlists.
- Independent reasoning-effort selection for ChatGPT models.
- Durable request lookup, duplicate prevention, Turn steer/interrupt, and conversation model changes
  within one provider. See the [Control API v1 contract (Japanese)](docs/control-api.ja.md).

In short: it is a bridge between familiar OpenAI-compatible clients and Codex.

## Getting started

1. Install Codex CLI and this proxy.
2. Copy the example configuration to `~/.config/codex-hoshikage-proxy/config.toml`.
3. Choose the providers and directories you want to allow.
4. Sign in to Codex if you want ChatGPT subscription models.
5. Run the proxy as a user-level systemd service.

Start here:

- [Installation guide](docs/installation.md)
- [User and API guide](docs/user-guide.md)
- [OpenWebUI registration guide](docs/openwebui.md)

The compatibility target starts at Codex CLI/App Server `0.147.0`. This round of live validation used
`0.153.4` with `gpt-5.6-luna`. Historical OpenWebUI `v0.11.0` acceptance notes are available; the new
image and structured-output checks used the proxy API directly.

Live checks also cover control request lookup, duplicate prevention, steer, interrupt, and a
`gpt-5.6-luna` to `gpt-5.6-terra` change in the same thread with inheritance after restart.

## Validation and remaining limits

Live checks cover ordinary responses, conversation continuation, images with `detail=high`, JSON Schema,
streaming, disconnect interruption, approval cancellation/expiry, and errors after an App Server crash.

- This model misidentified an image color with `detail=low`, including when the proxy was bypassed.
  Use `high` for images in this configuration for now.
- Client-defined tool-call/result and full usage conversion remain incomplete. Opt-in v2 clients can use the [interaction relay API](docs/interaction-api.ja.md) for user input, MCP elicitation, and permission requests; Gateway UI integration and acceptance are pending. Unsupported client tool specifications are rejected before execution; unsupported interactions receive an error and trigger interruption of the affected Turn.
- App Server failure causes the proxy to exit with an error. The bundled systemd service restarts it
  after five seconds; interrupted requests are not automatically replayed.
- Saved final answers can be retrieved through extension API v2. Final-output retrieval in v1 and historical SSE replay remain unsupported. Control APIs use a shared
  operator scope without per-user isolation.
- Long-running/high-load operation and compatibility with every model/client have not been validated.

See the [2026-09-13 hardening and acceptance record (Japanese)](docs/proxy-hardening-2026-09-13.ja.md) for the latest changes and deployment status.

See [coverage](docs/app-server-coverage.md), [live validation](docs/live-codex-validation.md), and
[development checks](docs/development.md).

## License

Copyright (c) 2026 Tane Channel Technology. Licensed under the [MIT License](LICENSE).

MCP approval API 0.6 separates complete operation display and one-shot approval from explicitly selected execution policies. See the [API contract](docs/mcp-approval-api-v06.ja.md), [user/operator guide](docs/mcp-approval-v06-guide.ja.md), and [implementation and acceptance record](docs/mcp-approval-v06-implementation.ja.md) (Japanese). The OpenAI-compatible `/v1` endpoints remain available.
