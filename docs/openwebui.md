# OpenWebUI Registration Guide

[日本語](openwebui.ja.md) | **English**

This guide targets OpenWebUI `v0.11.0` and the included standard Manifold Pipe.

## 1. Start the proxy where OpenWebUI can reach it

If OpenWebUI runs in Docker and the proxy runs on the host, do not use `127.0.0.1` in OpenWebUI.
Inside the container, `127.0.0.1` means the container itself. Use the host's reachable address, for example:

```text
http://192.168.0.220:4040
```

The URL in the Pipe is the proxy base URL without `/v1`. The Pipe adds `/v1/models` and the API path itself.

## 2. Configure the proxy API key

For a non-loopback listener, put a key in the proxy configuration:

```toml
[security]
api_key = "tane-codex-proxy-local-key"
```

Use your own long random value in a real deployment. The same value is entered in OpenWebUI's Pipe
valves as `PROXY_API_KEY`. If you use `api_key_env`, export the value in the proxy process environment.

## 3. Install and configure the Pipe

1. In OpenWebUI, open the administrator area and the Functions/Pipes management screen.
2. Create or import the included `openwebui/codex_hoshikage_pipe.py` as a Pipe.
3. Open that Pipe's configuration/valves.
4. Set:

   ```text
   PROXY_BASE_URL = http://192.168.0.220:4040
   PROXY_API_KEY = tane-codex-proxy-local-key
   REQUEST_TIMEOUT_SECONDS = 120
   ```

   Replace the address and key with your values.
5. Save the Pipe and enable its manifold models.

The Pipe requests `/v1/models` and presents model IDs as `Codex / provider / provider/model`.
Refresh the Pipe's model list after changing provider configuration.

## 4. Context-continuation experiment

The current Pipe calls the Proxy's Responses API internally. Its default logical conversation ID is:

```text
openwebui_id_001
```

This ID is a Pipe-side conversation key; it is not a Codex thread ID. Internally, the Pipe scopes it by
OpenWebUI user ID. While the Pipe process remains alive, threads for the same user share the same latest
Responses context, while different users remain isolated.

When you change the selected model, the Pipe starts a new Codex thread and sends the conversation
history supplied by OpenWebUI to the new model. The logical conversation ID remains the same, so the
conversation can continue across the model change. The mapping is currently in memory and is lost when
OpenWebUI reloads the Pipe. This is intentionally a first-stage experiment.

## 5. Approval behavior

The Pipe uses OpenWebUI's standard `__event_call__` approval event. The standard UI currently offers
two buttons, even though the proxy domain supports `accept`, `accept_for_session`, `decline`, and
`cancel`. The Pipe maps the available two-button interaction to the appropriate Codex decision and
passes the resulting decision to the proxy.

Approval is still enforced by Codex and the proxy. Never treat the two-button UI as a replacement for
the server-side approval state.

If the approval timeout expires, the proxy expires and cleans up the request, but OpenWebUI's standard
dialog may remain visible because the standard event API has no reliable server-side close event. A later
click is rejected as stale. This is a known OpenWebUI standard Pipe limitation and is an operational
constraint until the UI event path supports explicit dialog closure.

Reloading or disconnecting while approval is pending cancels the Codex turn through the Pipe disconnect
path. A manually cancelled approval also ends the turn and releases the provider permit.

## 6. Troubleshooting

- **NetworkProblem**: confirm the URL is reachable from the OpenWebUI container. Use the host LAN IP,
  not `127.0.0.1`, and ensure port `4040` is listening.
- **401**: the Pipe key and proxy `security.api_key` must match exactly.
- **404 on `/v1/chat/completions`**: the Pipe is pointing at an old process or the wrong port; restart
  the current proxy and use the base URL without `/v1`.
- **Only some models appear**: refresh the Pipe and check that the provider is enabled and its model
  is registered. Hoshikage models without tool-calling capability are intentionally filtered from Codex
  agent use.
- **A turn fails with `tool_calling_not_supported`**: select a model whose provider catalog reports
  tool support.
