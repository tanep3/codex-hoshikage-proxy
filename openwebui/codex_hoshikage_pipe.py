"""
title: Codex Hoshikage Proxy
author: Codex Hoshikage Proxy
version: 0.1.0
requirements: httpx

OpenWebUI Manifold Pipe for Codex Hoshikage Proxy.

Install this file as an OpenWebUI Function/Manifold Pipe.  The Pipe talks to
the Proxy only through its OpenAI-compatible API and the documented Codex
extension endpoints.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any, AsyncGenerator, Optional

from pydantic import BaseModel, Field


class Pipe:
    """Expose each Proxy model as an OpenWebUI manifold model."""

    class Valves(BaseModel):
        PROXY_BASE_URL: str = Field(
            default="http://127.0.0.1:4040",
            description="Codex Hoshikage Proxy base URL",
        )
        PROXY_API_KEY: str = Field(
            default="",
            description="Optional Proxy API key",
        )
        REQUEST_TIMEOUT_SECONDS: float = Field(default=120.0, ge=1.0)

    def __init__(self) -> None:
        self.type = "manifold"
        self.valves = self.Valves()

    def _headers(self) -> dict[str, str]:
        headers = {"content-type": "application/json"}
        if self.valves.PROXY_API_KEY:
            headers["authorization"] = f"Bearer {self.valves.PROXY_API_KEY}"
        return headers

    def _base_url(self) -> str:
        return self.valves.PROXY_BASE_URL.rstrip("/")

    def pipes(self) -> list[dict[str, str]]:
        """Return the current Proxy model catalog for OpenWebUI selection."""
        import httpx

        try:
            response = httpx.get(
                f"{self._base_url()}/v1/models",
                headers=self._headers(),
                timeout=self.valves.REQUEST_TIMEOUT_SECONDS,
            )
            response.raise_for_status()
            models = response.json().get("data", [])
        except Exception:
            return []

        result: list[dict[str, str]] = []
        for model in models:
            model_id = model.get("id")
            if not isinstance(model_id, str) or not model_id:
                continue
            owner = model.get("owned_by", "codex")
            result.append(
                {
                    "id": f"codex/{model_id}",
                    "name": f"Codex / {owner} / {model_id}",
                }
            )
        return result

    async def pipe(
        self,
        body: dict[str, Any],
        __event_emitter__: Any = None,
        __event_call__: Any = None,
        __task__: Optional[str] = None,
        **_: Any,
    ) -> AsyncGenerator[str, None]:
        """Stream a Chat Completions response and bridge Approval requests."""
        import httpx

        payload = dict(body)
        requested_model = payload.get("model")
        if isinstance(requested_model, str) and requested_model.startswith("codex/"):
            payload["model"] = requested_model.removeprefix("codex/")
        metadata = dict(payload.get("metadata") or {})
        if __event_call__ is not None:
            metadata["codex.approval_capability"] = "interactive"
        else:
            metadata.pop("codex.approval_capability", None)
        payload["metadata"] = metadata
        payload["stream"] = True

        timeout = httpx.Timeout(self.valves.REQUEST_TIMEOUT_SECONDS)
        async with httpx.AsyncClient(timeout=timeout) as client:
            async with client.stream(
                "POST",
                f"{self._base_url()}/v1/chat/completions",
                headers=self._headers(),
                json=payload,
            ) as response:
                response.raise_for_status()
                turn_id = response.headers.get("x-codex-turn-id")
                approval_task = None
                if turn_id and __event_call__ is not None:
                    approval_task = asyncio.create_task(
                        self._watch_approvals(client, turn_id, __event_call__)
                    )
                try:
                    async for line in response.aiter_lines():
                        if not line.startswith("data:"):
                            continue
                        data = line[5:].strip()
                        if data == "[DONE]":
                            break
                        try:
                            chunk = json.loads(data)
                        except json.JSONDecodeError:
                            continue
                        error = chunk.get("error")
                        if error:
                            raise RuntimeError(str(error))
                        for choice in chunk.get("choices", []):
                            delta = choice.get("delta", {}).get("content")
                            if isinstance(delta, str) and delta:
                                yield delta
                finally:
                    if approval_task is not None:
                        approval_task.cancel()
                        await asyncio.gather(approval_task, return_exceptions=True)

    async def _watch_approvals(
        self,
        client: Any,
        turn_id: str,
        event_call: Any,
    ) -> None:
        async with client.stream(
            "GET",
            f"{self._base_url()}/v1/codex/turns/{turn_id}/events/stream",
            headers=self._headers(),
        ) as response:
            response.raise_for_status()
            event_name = None
            async for line in response.aiter_lines():
                if line.startswith("event:"):
                    event_name = line[6:].strip()
                    continue
                if not line.startswith("data:"):
                    continue
                if event_name != "approval_requested":
                    continue
                try:
                    event = json.loads(line[5:].strip())
                except json.JSONDecodeError:
                    continue
                approval_id = event.get("approval_id")
                if not isinstance(approval_id, str):
                    continue
                decisions = event.get("availableDecisions") or []
                answer = await event_call(
                    {
                        "type": "input",
                        "data": {
                            "title": "Codex approval required",
                            "message": (
                                "Choose one of: "
                                + ", ".join(str(value) for value in decisions)
                            ),
                            "placeholder": "accept / accept_for_session / decline / cancel",
                        },
                    }
                )
                decision = self._normalize_decision(answer, decisions)
                await client.post(
                    f"{self._base_url()}/v1/codex/approvals/{approval_id}",
                    headers=self._headers(),
                    json={"decision": decision},
                )
                event_name = None

    @staticmethod
    def _normalize_decision(answer: Any, available: list[Any]) -> str:
        value = answer.get("value") if isinstance(answer, dict) else answer
        value = str(value or "cancel").strip()
        aliases = {"approve": "accept", "deny": "decline"}
        value = aliases.get(value, value)
        allowed = {str(item) for item in available}
        return value if value in allowed else "cancel"
