# OpenWebUI Manifold Pipe

For the end-user setup flow, see the bilingual [OpenWebUI Registration Guide](../docs/openwebui.md).

`codex_hoshikage_pipe.py` is the official OpenWebUI v0.11.0 integration path
for interactive Approval.

## Installation

1. In OpenWebUI, create a new Pipe/Function and paste the contents of
   `codex_hoshikage_pipe.py`.
2. Set `PROXY_BASE_URL` to the Proxy URL without `/v1`, for example
   `http://192.168.0.220:4040`.
3. Set `PROXY_API_KEY` when the Proxy requires API key authentication.
4. Leave `HEALTHCHECK_TIMEOUT_SECONDS` at its default of `2` seconds unless your network is unusually slow.
5. Enable the generated manifold models.

The current Pipe uses the Proxy Responses API and keeps a context-continuation mapping in memory.
Its default logical conversation ID is `openwebui_id_001`, so threads for the same OpenWebUI user share
the same Codex context while the Pipe process remains alive. User IDs are included in the internal key,
so different OpenWebUI users do not share context. You can change `CONVERSATION_ID` in the Pipe valves
when you want to start a separate experiment.

When the selected model changes, the Pipe starts a new Codex thread and seeds it with the conversation
history supplied by OpenWebUI. The external conversation ID remains the same, and subsequent messages
continue on the newly selected model. This is an MVP experiment: the mapping is lost when OpenWebUI
reloads the Pipe, and Responses-based interactive Approval still requires the Proxy to expose its
Responses stream Turn ID.

The Pipe reads `GET /v1/models`, sends requests to
`POST /v1/chat/completions`, and watches
`GET /v1/codex/turns/{turn_id}/events/stream` for Approval requests.
OpenWebUI prefixes Pipe model IDs with its Function ID; the Pipe removes that
namespace before sending the Public Model ID to the Proxy.

The Approval dialog uses OpenWebUI's `__event_call__` confirmation event.
OpenWebUI v0.11.0 provides an OK/Cancel dialog rather than a four-button
decision selector. OK maps to `accept` (or `accept_for_session` when that is
the only accepted decision); Cancel maps to `decline` when available, or
`cancel` otherwise. The Proxy Approval API still supports all four Wire
Decision values: `accept`, `accept_for_session`, `decline`, and `cancel`.

When the Proxy approval timeout expires, the Proxy rejects the Codex operation
and ends the Approval/Turn. OpenWebUI v0.11.0 does not provide a standard event
for a Pipe to close an already displayed `__event_call__` confirmation dialog,
so the dialog may remain visible until the user dismisses it. This is a known
limitation of the standard Pipe integration; the operation is already rejected
and a later decision from the stale dialog is not accepted by the Proxy.

This integration targets OpenWebUI v0.11.0 and requires its asynchronous Pipe
support and documented `__event_call__` event interface. If package
installation from Pipe frontmatter is disabled, install `httpx` in the
OpenWebUI environment or enable its function dependency installation setting.
