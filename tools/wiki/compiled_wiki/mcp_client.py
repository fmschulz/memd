from __future__ import annotations

import json
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any


class McpClientError(RuntimeError):
    """Raised when the MCP server returns an invalid or failed response."""


@dataclass
class McpHttpClient:
    url: str
    timeout: float = 30.0
    protocol_version: str = "2025-11-25"
    client_name: str = "memd-wiki"
    client_version: str = "0.1.0"
    _initialized: bool = field(default=False, init=False, repr=False)
    _request_id: int = field(default=0, init=False, repr=False)

    def initialize(self) -> None:
        if self._initialized:
            return
        payload = {
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": self.protocol_version,
                "capabilities": {},
                "clientInfo": {
                    "name": self.client_name,
                    "version": self.client_version,
                },
            },
        }
        response = self._post(payload)
        result = response.get("result", {})
        if result.get("protocolVersion") != self.protocol_version:
            raise McpClientError(
                f"unexpected protocol version: {result.get('protocolVersion')!r}"
            )
        self._initialized = True

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        self.initialize()
        payload = {
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        response = self._post(payload)
        result = response.get("result")
        if not isinstance(result, dict):
            raise McpClientError(f"missing MCP result for tool {name}")
        content = result.get("content")
        if not isinstance(content, list) or not content:
            raise McpClientError(f"missing tool content for {name}")

        parsed_items: list[Any] = []
        for item in content:
            if not isinstance(item, dict):
                continue
            text = item.get("text")
            if not isinstance(text, str):
                continue
            parsed_items.append(self._parse_text_payload(text))

        if not parsed_items:
            raise McpClientError(f"tool {name} returned no text payload")
        if len(parsed_items) == 1:
            return parsed_items[0]
        return parsed_items

    def _next_id(self) -> int:
        self._request_id += 1
        return self._request_id

    def _post(self, payload: dict[str, Any]) -> dict[str, Any]:
        body = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            self.url,
            data=body,
            headers={
                "Accept": "application/json, text/event-stream",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read().decode("utf-8")
        except urllib.error.HTTPError as exc:
            error_body = exc.read().decode("utf-8", errors="replace")
            raise McpClientError(
                f"HTTP {exc.code} calling MCP: {error_body}"
            ) from exc
        except urllib.error.URLError as exc:
            raise McpClientError(f"failed to reach MCP server at {self.url}") from exc

        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError as exc:
            raise McpClientError(f"invalid JSON from MCP server: {raw[:200]!r}") from exc

        if "error" in parsed:
            raise McpClientError(f"MCP error: {parsed['error']}")
        if not isinstance(parsed, dict):
            raise McpClientError("MCP response was not a JSON object")
        return parsed

    @staticmethod
    def _parse_text_payload(text: str) -> Any:
        stripped = text.strip()
        if not stripped:
            return ""
        try:
            return json.loads(stripped)
        except json.JSONDecodeError:
            return stripped
