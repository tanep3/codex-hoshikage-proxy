# OpenWebUI Manifold Pipe

`codex_hoshikage_pipe.py` is the official OpenWebUI v0.11.0 integration path
for interactive Approval.

## Installation

1. In OpenWebUI, create a new Pipe/Function and paste the contents of
   `codex_hoshikage_pipe.py`.
2. Set `PROXY_BASE_URL` to the Proxy URL.
3. Set `PROXY_API_KEY` when the Proxy requires API key authentication.
4. Enable the generated manifold models.

The Pipe reads `GET /v1/models`, sends requests to
`POST /v1/chat/completions`, and watches
`GET /v1/codex/turns/{turn_id}/events/stream` for Approval requests.

The Approval dialog uses OpenWebUI's `__event_call__` input event and sends
the selected value unchanged from the Proxy's `availableDecisions` list.
Use `accept`, `accept_for_session`, `decline`, or `cancel` as appropriate.

This integration targets OpenWebUI v0.11.0 and requires its asynchronous Pipe
support and documented `__event_call__` event interface. If package
installation from Pipe frontmatter is disabled, install `httpx` in the
OpenWebUI environment or enable its function dependency installation setting.
