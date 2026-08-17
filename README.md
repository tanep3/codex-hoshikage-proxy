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
- Streaming text, tool calls, usage, completion status, errors, and cancellation.
- Codex file and shell operations, approval handling, and working-directory allowlists.
- Independent reasoning-effort selection for ChatGPT models.

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

This project targets Codex CLI/App Server `0.147.0` or later and OpenWebUI `v0.11.0`.

## License

Copyright (c) 2026 Tane Channel Technology. Licensed under the [MIT License](LICENSE).

