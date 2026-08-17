"""
title: Codex Hoshikage Proxy
author: Codex Hoshikage Proxy
version: 0.2.0
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
        CONVERSATION_ID: str = Field(
            default="openwebui_id_001",
            description=(
                "Logical Codex conversation ID shared by OpenWebUI threads "
                "for the context-continuation PoC"
            ),
        )

    def __init__(self) -> None:
        self.type = "manifold"
        self.valves = self.Valves()
        # This is intentionally an in-memory PoC state.  A later iteration can
        # persist it, but keeping it local makes the OpenWebUI-only experiment
        # easy to reset by reloading the Pipe.
        self._response_ids: dict[str, tuple[str, str]] = {}

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
        __chat_id__: Optional[str] = None,
        __metadata__: Optional[dict[str, Any]] = None,
        __user__: Optional[dict[str, Any]] = None,
        __event_emitter__: Any = None,
        __event_call__: Any = None,
        __task__: Optional[str] = None,
        **_: Any,
    ) -> AsyncGenerator[str, None]:
        """Stream a Responses API result and preserve a logical conversation."""
        import httpx

        # OpenWebUI invokes Pipes for internal tasks such as title generation.
        # Those calls must not advance the user's Codex conversation.
        if __task__ is not None:
            return

        payload = dict(body)
        requested_model = payload.get("model")
        proxy_model = requested_model
        if isinstance(requested_model, str):
            proxy_model = self._proxy_model_id(requested_model)
            payload["model"] = proxy_model

        source_metadata = __metadata__ or payload.get("metadata") or {}
        # The proxy's metadata contract is string-to-string.  OpenWebUI's
        # reserved metadata also contains lists and nested dictionaries.
        metadata = {
            str(key): value
            for key, value in source_metadata.items()
            if isinstance(value, str)
        }
        if __event_call__ is not None:
            metadata["codex.approval_capability"] = "interactive"
        else:
            metadata.pop("codex.approval_capability", None)
        conversation_id = self._conversation_id(__chat_id__, __user__)
        metadata["codex.openwebui_chat_id"] = conversation_id
        payload["metadata"] = metadata
        payload["input"] = self._responses_input(
            body,
            metadata,
            conversation_id,
            proxy_model if isinstance(proxy_model, str) else "",
        )
        payload.pop("messages", None)
        previous_response_id = self._previous_response_id(
            conversation_id,
            proxy_model if isinstance(proxy_model, str) else "",
        )
        if previous_response_id:
            payload["previous_response_id"] = previous_response_id
        else:
            payload.pop("previous_response_id", None)
        payload["stream"] = True

        timeout = httpx.Timeout(self.valves.REQUEST_TIMEOUT_SECONDS)
        async with httpx.AsyncClient(timeout=timeout) as client:
            async with client.stream(
                "POST",
                f"{self._base_url()}/v1/responses",
                headers=self._headers(),
                json=payload,
            ) as response:
                if response.is_error:
                    detail = (await response.aread()).decode(errors="replace")
                    raise RuntimeError(
                        f"Proxy returned {response.status_code} for {response.url}: {detail}"
                    )
                response_id: Optional[str] = None
                turn_id = response.headers.get("x-codex-turn-id")
                approval_task = None
                event_name: Optional[str] = None
                if turn_id and __event_call__ is not None:
                    approval_task = asyncio.create_task(
                        self._watch_approvals(client, turn_id, __event_call__)
                    )
                try:
                    async for line in response.aiter_lines():
                        if line.startswith("event:"):
                            event_name = line[6:].strip()
                            continue
                        if not line.startswith("data:"):
                            continue
                        data = line[5:].strip()
                        if data == "[DONE]":
                            break
                        try:
                            event = json.loads(data)
                        except json.JSONDecodeError:
                            continue
                        event_type = event_name
                        event_name = None
                        if event_type == "response.created":
                            candidate = event.get("id")
                            if isinstance(candidate, str):
                                response_id = candidate
                        elif event_type == "response.output_text.delta":
                            delta = event.get("delta")
                            if isinstance(delta, str) and delta:
                                yield delta
                        elif event_type == "response.failed":
                            raise RuntimeError(str(event.get("error") or event))
                        elif event_type == "response.completed":
                            if response_id is None:
                                candidate = event.get("id")
                                if isinstance(candidate, str):
                                    response_id = candidate
                            if response_id is not None:
                                self._response_ids[conversation_id] = (
                                    proxy_model if isinstance(proxy_model, str) else "",
                                    response_id,
                                )
                finally:
                    if approval_task is not None:
                        approval_task.cancel()
                        await asyncio.gather(approval_task, return_exceptions=True)

    def _conversation_id(
        self, chat_id: Optional[str], user: Optional[dict[str, Any]]
    ) -> str:
        configured = self.valves.CONVERSATION_ID.strip()
        logical_id = configured or chat_id or "openwebui-default"
        user_id = user.get("id") if isinstance(user, dict) else None
        if isinstance(user_id, str) and user_id:
            return f"{user_id}:{logical_id}"
        return logical_id

    def _previous_response_id(self, conversation_id: str, model_id: str) -> Optional[str]:
        current = self._response_ids.get(conversation_id)
        if current is None or current[0] != model_id:
            return None
        return current[1]

    def _responses_input(
        self,
        body: dict[str, Any],
        metadata: dict[str, Any],
        conversation_id: str,
        model_id: str,
    ) -> list[dict[str, str]]:
        """Use the latest prompt normally, or full history after a model switch."""
        previous = self._previous_response_id(conversation_id, model_id)
        if previous:
            prompt = metadata.get("user_prompt")
            if not isinstance(prompt, str) or not prompt:
                prompt = self._last_user_content(body.get("messages", []))
            return [{"type": "text", "text": prompt}]

        messages = body.get("messages") or []
        result: list[dict[str, str]] = []
        for message in messages:
            if not isinstance(message, dict):
                continue
            role = str(message.get("role") or "user")
            content = message.get("content")
            if isinstance(content, str) and content:
                result.append({"type": "text", "text": f"[{role}]\n{content}"})
        if result:
            return result
        prompt = metadata.get("user_prompt")
        return [{"type": "text", "text": prompt if isinstance(prompt, str) else ""}]

    @staticmethod
    def _last_user_content(messages: Any) -> str:
        if not isinstance(messages, list):
            return ""
        for message in reversed(messages):
            if isinstance(message, dict) and message.get("role") == "user":
                content = message.get("content")
                if isinstance(content, str):
                    return content
        return ""

    @staticmethod
    def _proxy_model_id(model_id: str) -> str:
        """Remove OpenWebUI's Function namespace before forwarding the model."""
        marker = "codex/"
        if marker not in model_id:
            return model_id
        return model_id.split(marker, 1)[1]

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
            lines = response.aiter_lines()
            next_line = asyncio.create_task(lines.__anext__())
            approval_call = None
            approval_id = None
            event_name = None
            try:
                while True:
                    pending = {next_line}
                    if approval_call is not None:
                        pending.add(approval_call)
                    done, _ = await asyncio.wait(
                        pending, return_when=asyncio.FIRST_COMPLETED
                    )
                    if approval_call is not None and approval_call in done:
                        answer = approval_call.result()
                        approval_call = None
                        decision = self._normalize_decision(answer, decisions)
                        approval_response = await client.post(
                            f"{self._base_url()}/v1/codex/approvals/{approval_id}",
                            headers=self._headers(),
                            json={"decision": decision},
                        )
                        approval_response.raise_for_status()
                        event_name = None
                        continue
                    if next_line not in done:
                        continue
                    try:
                        line = next_line.result()
                    except StopAsyncIteration:
                        break
                    next_line = asyncio.create_task(lines.__anext__())
                    if line.startswith("event:"):
                        event_name = line[6:].strip()
                        continue
                    if not line.startswith("data:"):
                        continue
                    try:
                        event = json.loads(line[5:].strip())
                    except json.JSONDecodeError:
                        continue
                    if event_name == "approval_requested":
                        requested_id = event.get("approval_id")
                        if not isinstance(requested_id, str) or approval_call is not None:
                            continue
                        approval_id = requested_id
                        decisions = event.get("availableDecisions") or []
                        approval_call = asyncio.create_task(
                            event_call(
                                {
                                    "type": "confirmation",
                                    "data": {
                                        "title": "Codex approval required",
                                        "message": (
                                            "Approve this operation? Available Codex decisions: "
                                            + ", ".join(str(value) for value in decisions)
                                        ),
                                    },
                                }
                            )
                        )
                    elif event_name == "approval_resolved":
                        if event.get("approval_id") == approval_id and approval_call:
                            approval_call.cancel()
                            await asyncio.gather(approval_call, return_exceptions=True)
                            approval_call = None
                            approval_id = None
                    event_name = None
            finally:
                next_line.cancel()
                if approval_call is not None:
                    approval_call.cancel()
                await asyncio.gather(
                    next_line,
                    *( [approval_call] if approval_call is not None else [] ),
                    return_exceptions=True,
                )

    @staticmethod
    def _normalize_decision(answer: Any, available: list[Any]) -> str:
        value = answer.get("value") if isinstance(answer, dict) else answer
        allowed = {str(item) for item in available}
        if isinstance(value, bool):
            if value:
                if "accept" in allowed:
                    return "accept"
                if "accept_for_session" in allowed:
                    return "accept_for_session"
            else:
                if "decline" in allowed:
                    return "decline"
                if "cancel" in allowed:
                    return "cancel"
            return "cancel"
        value = str(value or "cancel").strip()
        aliases = {"approve": "accept", "deny": "decline"}
        value = aliases.get(value, value)
        return value if value in allowed else "cancel"
