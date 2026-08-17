[日本語](README.ja.md) | **English**

# Codex Hoshikage Proxy

Codex Hoshikage Proxy lets you use Codex-powered AI through one OpenAI-compatible endpoint.
It connects Codex App Server with Hoshikage, ChatGPT/Codex subscription models, and Ollama,
then exposes a common model list and API for applications such as OpenWebUI.

## What can it do?

- Provide `GET /v1/models`, `POST /v1/responses`, and `POST /v1/chat/completions`.
- Select a provider and model with one public model ID such as
  `hoshikage/unsloth-gemma4-12b-qat-thinking-off` or `chatgpt/gpt-5.6-luna`.
- Stream text, tool calls, usage, completion status, errors, and cancellation through
  OpenAI-compatible responses.
- Use Codex's local agent capabilities, including shell and file operations, with approval
  and working-directory controls.
- Connect ChatGPT/Codex subscription authentication without requiring an OpenAI API key.
- Combine local Hoshikage and Ollama models with ChatGPT models behind the same endpoint.
- Register the endpoint in OpenWebUI through the included Pipe.

## Why use it?

You do not need to configure every client separately for every provider. Configure the proxy
once, choose a model from the unified list, and keep the same client workflow when switching
between local and subscription-backed models. The proxy also gives you explicit control over
which directories Codex may access and whether a tool operation needs approval.

## Current status

This is an MVP release intended for self-hosted use. It targets Codex CLI/App Server `0.147.0`
or later. OpenWebUI integration targets OpenWebUI `v0.11.0` and its standard Pipe events.
The standard OpenWebUI approval dialog currently has a two-button limitation; see the
[OpenWebUI guide](docs/openwebui.md).

## Quick start

1. Install Codex CLI and, if needed, Hoshikage or Ollama.
2. Copy [`config.example.toml`](config.example.toml) to
   `~/.config/codex-hoshikage-proxy/config.toml` and edit the provider and directory settings.
3. Authenticate Codex if you will use ChatGPT models.
4. Start the proxy and call `/v1/models`.

See the [installation guide](docs/installation.md), [user and API guide](docs/user-guide.md),
and [OpenWebUI registration guide](docs/openwebui.md).

## Documentation

- [Installation guide](docs/installation.md) — prerequisites, configuration, authentication,
  and startup.
- [User and API guide](docs/user-guide.md) — environment settings, model selection, APIs,
  errors, and operations.
- [OpenWebUI registration guide](docs/openwebui.md) — OpenWebUI `v0.11.0` setup.
- [Internal requirements](docs/codex-hoshikage-proxy-requirements.md) and
  [system design](docs/codex-hoshikage-proxy-system-design.md) — project design references.

## License and author

Copyright (c) 2026 Tane Channel Technology. This project is licensed under the MIT License;
see [`LICENSE`](LICENSE).

