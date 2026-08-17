# Installation Guide

[日本語](installation.ja.md) | **English**

## 1. Prerequisites

- Linux or another environment where Codex CLI can run.
- Rust and Cargo.
- Codex CLI/App Server `0.147.0` or later.
- Hoshikage for Hoshikage models, and/or Ollama for Ollama models.
- OpenWebUI `v0.11.0` only if you want the included integration.

Install or update Codex, then verify:

```sh
codex --version
```

The proxy starts `codex app-server --listen stdio://` as a child process. It does not replace
Codex authentication and it does not create Codex credentials.

## 2. Install and configure

From the repository root:

```sh
cargo install --path .
mkdir -p "$HOME/.config/codex-hoshikage-proxy"
cp config.example.toml "$HOME/.config/codex-hoshikage-proxy/config.toml"
```

This installs the proxy as `~/.cargo/bin/codex-hoshikage-proxy`. It is a long-running service, so
the normal way to run it is a user-level systemd service, not `cargo run`.

Edit `~/.config/codex-hoshikage-proxy/config.toml`:

- Set `server.host` and `server.port`.
- Set `server.default_cwd` to an existing directory.
- Replace `security.allowed_cwds` with the directories you actually permit. The paths are examples,
  not mandatory defaults. Every allowed root and the default directory must already exist; the proxy
  does not create them.
- Enable and configure the providers you want.
- Set `defaults.model` to one configured public model ID.

For a LAN or other non-loopback listener, configure an API key:

```toml
[security]
api_key = "replace-with-a-long-random-secret"
allowed_cwds = ["${HOME}/work"]
```

You may instead use `api_key_env = "PROXY_API_KEY"`. Non-loopback listening is rejected without a
non-empty API key. TLS is intended to be terminated by a reverse proxy and CORS is disabled by default.

## 3. ChatGPT/Codex subscription authentication

The ChatGPT provider uses the Codex App Server's OpenAI authentication. This is separate from the
proxy's client-facing API key.

For a normal machine with a browser, authenticate the dedicated Proxy Codex home interactively:

```sh
export CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home"
codex login
```

For a headless or remote machine, use device-code login:

```sh
CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home" codex login --device-auth
```

Open the displayed URL on a browser, sign in with the ChatGPT account that has the required
subscription/workspace access, and enter the one-time code. If device login is unavailable, complete
browser login on a machine that can do so and copy the credential cache securely.

The proxy's `CODEX_HOME` is a dedicated management area. The proxy may generate its `config.toml`,
but it never generates `auth.json`; authentication is performed by Codex. Treat `auth.json` as a
password and never commit or share it.

ChatGPT sign-in uses subscription/workspace access. API-key sign-in is a different option and uses
OpenAI Platform usage-based billing; it is not the ChatGPT Plus allowance. The official authentication
details are in [OpenAI's Codex authentication guide](https://learn.chatgpt.com/docs/auth).

Enable a ChatGPT model in the proxy configuration, for example:

```toml
[providers.chatgpt]
codex_id = "openai"
enabled = true
max_concurrent_turns = 4

[models."chatgpt/gpt-5.6-luna"]
provider = "chatgpt"
upstream_model = "gpt-5.6-luna"
display_name = "GPT-5.6 Luna"
reasoning_efforts = ["low", "medium", "high"]
default_reasoning_effort = "medium"
```

Reasoning effort is exposed for ChatGPT models only. Other providers use their Codex-side default;
the proxy recommends `medium` when a default is required.

## 4. Hoshikage and Ollama

For local Hoshikage:

```toml
[providers.hoshikage]
codex_id = "hoshikage"
enabled = true
max_concurrent_turns = 1
base_url = "http://127.0.0.1:3030/v1"
```

For a Hoshikage endpoint on another host, set `auth_env_key` and export the token before starting the
proxy. Hoshikage models are discovered from its model APIs. Models that do not advertise tool calling
are not dynamically exposed for Codex tool use.

For Ollama, enable the provider and define the public model IDs. Ollama is expected to be available at
its standard local endpoint.

## 5. Authenticate and start the service

If you use ChatGPT models, complete the Codex login in the dedicated Proxy Codex home before starting
the service. The service uses the same home automatically:

```sh
CODEX_HOME="$HOME/.config/codex-hoshikage-proxy/codex-home" codex login --device-auth
```

Install the service unit:

```sh
mkdir -p "$HOME/.config/systemd/user"
cp contrib/systemd/codex-hoshikage-proxy.service \
  "$HOME/.config/systemd/user/codex-hoshikage-proxy.service"
systemctl --user daemon-reload
systemctl --user enable --now codex-hoshikage-proxy.service
```

Check the service:

```sh
systemctl --user status codex-hoshikage-proxy.service
journalctl --user -u codex-hoshikage-proxy.service -f
```

The included unit expects the proxy binary at `~/.cargo/bin/codex-hoshikage-proxy` and includes the
usual Volta path (`~/.volta/bin`) for Codex CLI. If Codex is installed elsewhere, edit the unit's
`Environment=PATH=...` line. To keep the service alive after logging out, enable lingering once:

```sh
loginctl enable-linger "$USER"
```

To stop or disable it:

```sh
systemctl --user disable --now codex-hoshikage-proxy.service
```

## 6. Verify the API

```sh
curl -H "Authorization: Bearer replace-with-a-long-random-secret" \
  http://127.0.0.1:4040/v1/models
```

Omit the `Authorization` header for a loopback-only listener when no API key is configured. A normal
response contains model IDs in `provider/model` form.

## 7. Next steps

- Use the [user and API guide](user-guide.md) for configuration and requests.
- Use the [OpenWebUI guide](openwebui.md) to register the Pipe.
