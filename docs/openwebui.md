# OpenWebUI Registration Guide

[日本語](openwebui.ja.md) | **English**

This guide targets OpenWebUI `v0.11.0` and the included standard Manifold Pipe.

## 1. Start the proxy where OpenWebUI can reach it

If OpenWebUI runs in Docker and the proxy runs on the host, do not use `127.0.0.1` in OpenWebUI.
Inside the container, `127.0.0.1` means the container itself. Use the host's reachable address, for example:

```text
http://192.168.0.120:4040
```

The URL in the Pipe is the proxy base URL without `/v1`. The Pipe adds `/v1/models` and the API path itself.

## 2. Configure the proxy API key

The current Proxy requires an API key, including on loopback. Put a key in the proxy configuration:

```toml
[security]
api_key = "YOUR_PROXY_API_KEY"
```

Use your own long random value in a real deployment. The same value is entered in OpenWebUI's Pipe
valves as `PROXY_API_KEY`. If you use `api_key_env`, export the value in the proxy process environment.

## 3. Install and configure the Pipe

1. In OpenWebUI, open the administrator area and the Functions/Pipes management screen.
2. Create or import the included `openwebui/codex_hoshikage_pipe.py` as a Pipe.
3. Open that Pipe's configuration/valves.
4. Set:

   ```text
   PROXY_BASE_URL = http://192.168.0.120:4040
   PROXY_API_KEY = YOUR_PROXY_API_KEY
   REQUEST_TIMEOUT_SECONDS = 120
   HEALTHCHECK_TIMEOUT_SECONDS = 2
   REASONING_EFFORT = low
   ```

   Replace the address and key with your values.
   Set `REASONING_EFFORT` to `low`, `medium`, or `high` to change reasoning for ChatGPT models.
   Hoshikage and Ollama models use their provider default.
5. Save the Pipe and enable its manifold models.

Before every normal request, the Pipe checks the Proxy's `/readyz` endpoint. If the Proxy is stopped
or Codex is not ready, the Pipe stops quickly and reports that state instead of waiting for the full
request timeout. It does not silently send the request to another model.

The Pipe requests `/v1/models` and presents model IDs as `Codex / provider / provider/model`.
Model discovery can take longer than the Proxy liveness check because the Proxy refreshes
Provider catalogs on demand. The Pipe therefore uses a separate 20-second model-list timeout.
After changing Provider availability, save/update the Pipe in OpenWebUI (or reload the Function)
and refresh the browser so OpenWebUI calls `pipes()` again and rebuilds its manifold model list.
For chat requests, the Pipe also asks OpenWebUI v0.11.0 to refresh its internal model cache before
forwarding the turn. This keeps the server-side cache current even when the browser has not yet
reloaded the model picker.
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

If the Proxy restarts while the Pipe still holds an old Response ID, the Codex thread may no longer
exist. The Pipe detects `thread_not_found`, discards the stale ID, and retries once using OpenWebUI's
visible conversation history to create a replacement thread.

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
- **Only some models appear**: save/update or reload the Pipe, then refresh the browser. Check that
  the provider is enabled and that its catalog endpoint responds. Hoshikage models without tool-calling capability are intentionally filtered from Codex
  agent use.
- **A turn fails with `tool_calling_not_supported`**: select a model whose provider catalog reports
  tool support.
- **`thread_not_found` after a Proxy restart**: the Pipe should recover automatically from the visible
  OpenWebUI history. If recovery still fails, reload the Pipe once to clear its in-memory mapping.

## Pipe 0.5.1 approval fixes and v2 status

The Pipe reconciles pending approvals every 0.5 seconds, presents overlapping requests sequentially with operation details, and validates the Turn/Thread when posting a decision. Resolved or expired requests are discarded. Monitor failures surface as stream errors; decisions with unknown outcomes are never automatically retried.

Run `python -m unittest discover -s tests -p "test_openwebui_pipe.py"` with httpx and pydantic installed. After `cargo build --bins`, this also tests the actual Proxy with a fake App Server over HTTP. Browser confirmation remains a separate acceptance check.

Pipe 0.5.1 still uses `/v1/responses` and the v1 control extensions. Enabling Proxy v2 does not migrate the Pipe automatically. V2 adoption requires durable chat/branch mapping and request keys, explicit stop handling distinct from disconnection, and artifact authorization/presentation. On 2026-09-11 OpenWebUI was upgraded from 0.6.36 to 0.11.3 and the registered Pipe was updated to 0.5.1. Its Proxy URL is now http://192.168.0.120:4040; the key is stored in Valves rather than source code. The previous Function row was backed up privately before a targeted SQLite transaction; the installed loader checks source changes before reusing its cached module.

Five local tests passed, including an actual Proxy/fake App Server approval round trip. Four approval regression tests also passed against the registered source inside the OpenWebUI container, followed by model discovery and a real Codex response (`PIPE_DEPLOY_OK`). Browser dialog rendering remains unverified.

## Pipe 0.6.0 image support

Multimodal message parts are preserved. Uploaded images are read through OpenWebUI file authorization and converted to data URLs (10 MiB per image, 15 MiB total input). Failed image reads abort the request instead of silently sending text only.

Completed Responses expose PNGs from Codex image-generation tools through the documented generated-image extension. The Pipe saves the bytes as files owned by the OpenWebUI user and renders Markdown image links without exposing the Proxy key. This does not collect arbitrary workspace files or implement the general v2 artifact menu.
