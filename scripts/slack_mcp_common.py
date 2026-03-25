#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import platform
import re
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEFAULT_MCP_SERVER_URL = "https://mcp.slack.com/mcp"
DEFAULT_PROTOCOL_VERSION = "2025-06-18"
APP_NAME = "slackcli"
LEGACY_APP_NAME = "slack-cli"


class HttpRequestError(RuntimeError):
    def __init__(self, status: int, body: str, headers: dict[str, str]) -> None:
        super().__init__(f"HTTP {status}: {body}")
        self.status = status
        self.body = body
        self.headers = headers


def _preferred_config_dir(app_name: str) -> Path:
    override = os.environ.get("SLACKCLI_CONFIG_DIR") or os.environ.get("SLACK_CLI_CONFIG_DIR")
    if override:
        return Path(override)

    if xdg_home := os.environ.get("XDG_CONFIG_HOME"):
        return Path(xdg_home) / app_name

    system = platform.system()
    if system == "Darwin":
        return Path.home() / "Library" / "Application Support" / f"com.slack.{app_name}"
    if system == "Windows":
        appdata = os.environ.get("APPDATA")
        if appdata:
            return Path(appdata) / "Slack" / app_name

    return Path.home() / ".config" / app_name


def default_config_dir() -> Path:
    preferred = _preferred_config_dir(APP_NAME)
    legacy = _preferred_config_dir(LEGACY_APP_NAME)
    if not preferred.exists() and legacy.exists():
        return legacy
    return preferred


def default_config_path() -> Path:
    return default_config_dir() / "config.toml"


def default_credentials_path() -> Path:
    return default_config_dir() / "credentials.toml"


def load_token(config_path: Path | None = None, credentials_path: Path | None = None) -> str:
    if direct := os.environ.get("SLACK_MCP_ACCESS_TOKEN"):
        return direct
    if direct := os.environ.get("SLACK_TOKEN"):
        return direct
    if direct := os.environ.get("SLACKCLI_TOKEN"):
        return direct

    resolved_config_path = config_path or default_config_path()
    resolved_credentials_path = credentials_path or default_credentials_path()
    if not resolved_config_path.exists():
        raise FileNotFoundError(
            f"config file not found at {resolved_config_path}; run `slackcli auth login` first"
        )
    if not resolved_credentials_path.exists():
        raise FileNotFoundError(
            f"credentials file not found at {resolved_credentials_path}; run `slackcli auth login` first"
        )

    config = tomllib.loads(resolved_config_path.read_text(encoding="utf-8"))
    credentials = tomllib.loads(resolved_credentials_path.read_text(encoding="utf-8"))
    profile_name = os.environ.get("SLACKCLI_PROFILE") or config.get("active_profile")
    if not isinstance(profile_name, str) or not profile_name:
        raise RuntimeError(
            "no active profile configured; set SLACKCLI_PROFILE, SLACK_TOKEN, or run `slackcli auth login`"
        )

    profile = credentials.get("profiles", {}).get(profile_name, {})
    token = profile.get("token")
    if not isinstance(token, str) or not token:
        raise RuntimeError(
            f"no persisted token found for profile `{profile_name}`; run `slackcli auth login` again"
        )
    return token


def _http_request(
    url: str,
    *,
    method: str = "GET",
    headers: dict[str, str] | None = None,
    json_body: dict[str, Any] | list[Any] | None = None,
    timeout: float = 60.0,
) -> tuple[int, dict[str, str], str]:
    request_headers = {
        "User-Agent": "slackcli-mcp-bench/1.0",
        "Accept": "application/json",
        **dict(headers or {}),
    }
    body: bytes | None = None

    if json_body is not None:
        body = json.dumps(json_body).encode("utf-8")
        request_headers.setdefault("Content-Type", "application/json")
        request_headers.setdefault("Accept", "application/json, text/event-stream")

    request = urllib.request.Request(url, data=body, method=method, headers=request_headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            content_type = response.headers.get("Content-Type", "")
            if "text/event-stream" in content_type:
                chunks: list[str] = []
                saw_data = False
                while True:
                    line = response.readline()
                    if not line:
                        break
                    decoded = line.decode("utf-8", errors="replace")
                    chunks.append(decoded)
                    if decoded.startswith("data:"):
                        saw_data = True
                    if saw_data and decoded.strip() == "":
                        break
                text = "".join(chunks)
            else:
                text = response.read().decode("utf-8")
            return response.status, dict(response.headers.items()), text
    except urllib.error.HTTPError as error:
        text = error.read().decode("utf-8", errors="replace")
        raise HttpRequestError(error.code, text, dict(error.headers.items())) from error


def _extract_json_rpc_message(response_text: str) -> dict[str, Any] | None:
    text = response_text.strip()
    if not text:
        return None
    if text.startswith("{"):
        return json.loads(text)

    message: dict[str, Any] | None = None
    chunks = [chunk.strip() for chunk in text.split("\n\n") if chunk.strip()]
    for chunk in chunks:
        data_lines = []
        for line in chunk.splitlines():
            if line.startswith("data:"):
                data_lines.append(line[5:].strip())
        if not data_lines:
            continue
        candidate = "\n".join(data_lines)
        if candidate and candidate != "[DONE]":
            try:
                parsed = json.loads(candidate)
            except json.JSONDecodeError:
                continue
            if isinstance(parsed, dict):
                message = parsed
    return message


def _normalize_key(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "", value.lower())


TOOL_ALIASES: dict[str, dict[str, Any]] = {
    "search_messages": {
        "name_candidates": (
            "slack_search_public_and_private",
            "search_messages",
            "search-messages",
            "messages_search",
            "messages-search",
        ),
        "keyword_sets": (
            ("search", "message"),
            ("messages", "search"),
        ),
        "argument_candidates": {
            "query": ("query", "q", "search_query"),
            "limit": ("limit", "count", "page_size", "max_results"),
        },
    },
    "read_channel": {
        "name_candidates": (
            "read_channel",
            "read-channel",
            "channel_history",
            "channel-history",
            "read_conversation",
        ),
        "keyword_sets": (
            ("read", "channel"),
            ("channel", "history"),
            ("read", "conversation"),
        ),
        "argument_candidates": {
            "channel_id": (
                "channel_id",
                "channel",
                "conversation_id",
                "channelId",
                "conversationId",
            ),
            "limit": ("limit", "count", "page_size", "max_results"),
        },
    },
    "read_thread": {
        "name_candidates": (
            "read_thread",
            "read-thread",
            "thread_history",
            "thread-history",
            "read_replies",
        ),
        "keyword_sets": (
            ("read", "thread"),
            ("thread", "history"),
            ("thread", "reply"),
        ),
        "argument_candidates": {
            "channel_id": (
                "channel_id",
                "channel",
                "conversation_id",
                "channelId",
                "conversationId",
            ),
            "thread_ts": (
                "thread_ts",
                "thread",
                "threadId",
                "threadTs",
                "message_ts",
                "messageTs",
                "ts",
            ),
            "limit": ("limit", "count", "page_size", "max_results"),
        },
    },
    "send_message": {
        "name_candidates": (
            "send_message",
            "send-message",
            "post_message",
            "post-message",
            "create_message",
        ),
        "keyword_sets": (
            ("send", "message"),
            ("post", "message"),
            ("write", "message"),
        ),
        "argument_candidates": {
            "channel_id": (
                "channel_id",
                "channel",
                "conversation_id",
                "channelId",
                "conversationId",
            ),
            "text": ("text", "message", "content", "body"),
            "thread_ts": (
                "thread_ts",
                "thread",
                "threadId",
                "threadTs",
                "message_ts",
                "messageTs",
                "ts",
            ),
        },
    },
    "read_user_profile": {
        "name_candidates": (
            "read_user_profile",
            "read-user-profile",
            "user_profile",
            "user-profile",
        ),
        "keyword_sets": (
            ("read", "user", "profile"),
            ("user", "profile"),
        ),
        "argument_candidates": {
            "user_id": (
                "user_id",
                "user",
                "userId",
            ),
        },
    },
}


@dataclass
class SlackMcpClient:
    server_url: str
    access_token: str
    protocol_version: str = DEFAULT_PROTOCOL_VERSION
    session_id: str | None = None
    _next_id: int = 1

    def initialize(self) -> dict[str, Any]:
        payload = {
            "jsonrpc": "2.0",
            "id": self._consume_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": self.protocol_version,
                "capabilities": {},
                "clientInfo": {
                    "name": "slackcli-bench",
                    "version": "0.1.0",
                },
            },
        }
        message = self._post(payload)
        self.notify("notifications/initialized", {})
        return message

    def list_tools(self) -> list[dict[str, Any]]:
        tools: list[dict[str, Any]] = []
        cursor: str | None = None
        while True:
            params: dict[str, Any] = {}
            if cursor:
                params["cursor"] = cursor
            message = self.rpc("tools/list", params)
            result = message.get("result", {})
            tools.extend(result.get("tools", []))
            cursor = result.get("nextCursor")
            if not cursor:
                return tools

    def call_tool(self, name: str, arguments: dict[str, Any] | None = None) -> dict[str, Any]:
        return self.rpc(
            "tools/call",
            {
                "name": name,
                "arguments": arguments or {},
            },
        )

    def rpc(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        payload = {
            "jsonrpc": "2.0",
            "id": self._consume_id(),
            "method": method,
            "params": params or {},
        }
        message = self._post(payload)
        if "error" in message:
            raise RuntimeError(json.dumps(message["error"], sort_keys=True))
        return message

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        payload = {
            "jsonrpc": "2.0",
            "method": method,
            "params": params or {},
        }
        self._post(payload, expect_response=False)

    def _consume_id(self) -> int:
        current = self._next_id
        self._next_id += 1
        return current

    def _post(self, payload: dict[str, Any], *, expect_response: bool = True) -> dict[str, Any]:
        headers = {
            "Authorization": f"Bearer {self.access_token}",
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
        }
        if self.session_id:
            headers["Mcp-Session-Id"] = self.session_id

        try:
            _, response_headers, text = _http_request(
                self.server_url,
                method="POST",
                headers=headers,
                json_body=payload,
                timeout=60.0,
            )
        except HttpRequestError as error:
            message = _extract_json_rpc_message(error.body)
            if message and "error" in message:
                raise RuntimeError(json.dumps(message["error"], sort_keys=True)) from error
            raise

        if session_id := response_headers.get("Mcp-Session-Id") or response_headers.get("mcp-session-id"):
            self.session_id = session_id
        if not expect_response:
            return {}
        message = _extract_json_rpc_message(text)
        if message is None:
            raise RuntimeError(f"MCP server returned an empty response for payload: {payload}")
        return message


def resolve_tool(alias: str, tools: list[dict[str, Any]]) -> dict[str, Any]:
    spec = TOOL_ALIASES.get(alias)
    if spec is None:
        raise KeyError(f"unknown Slack MCP benchmark alias: {alias}")

    exact_candidates = tuple(_normalize_key(name) for name in spec["name_candidates"])
    scored: list[tuple[int, dict[str, Any]]] = []

    for tool in tools:
        if not isinstance(tool, dict):
            continue
        name = tool.get("name")
        if not isinstance(name, str):
            continue
        description = tool.get("description")
        combined = f"{name} {description}" if isinstance(description, str) else name
        normalized_name = _normalize_key(name)
        normalized_combined = _normalize_key(combined)

        if normalized_name in exact_candidates:
            return tool
        if normalized_name.startswith("slack") and normalized_name[len("slack") :] in exact_candidates:
            return tool

        score = 0
        for keyword_set in spec["keyword_sets"]:
            normalized_keywords = tuple(_normalize_key(keyword) for keyword in keyword_set)
            if all(keyword in normalized_combined for keyword in normalized_keywords):
                score += 10
            elif all(keyword in normalized_name for keyword in normalized_keywords):
                score += 6
        if score > 0:
            scored.append((score, tool))

    if not scored:
        available = ", ".join(
            sorted(
                tool.get("name")
                for tool in tools
                if isinstance(tool, dict) and isinstance(tool.get("name"), str)
            )
        )
        raise RuntimeError(
            f"could not find an MCP tool for alias `{alias}`. Available tools: {available}"
        )

    scored.sort(key=lambda item: (-item[0], str(item[1].get("name"))))
    return scored[0][1]


def map_tool_arguments(alias: str, tool: dict[str, Any], logical_arguments: dict[str, Any]) -> dict[str, Any]:
    spec = TOOL_ALIASES[alias]
    schema = tool.get("inputSchema")
    properties = schema.get("properties", {}) if isinstance(schema, dict) else {}
    normalized_properties = {
        _normalize_key(name): name for name in properties.keys() if isinstance(name, str)
    }
    mapped: dict[str, Any] = {}

    for logical_key, value in logical_arguments.items():
        candidates = spec["argument_candidates"].get(logical_key, (logical_key,))

        property_name = None
        for candidate in candidates:
            if candidate in properties:
                property_name = candidate
                break
            normalized_candidate = _normalize_key(candidate)
            if normalized_candidate in normalized_properties:
                property_name = normalized_properties[normalized_candidate]
                break

        if property_name is None:
            property_name = logical_key
        mapped[property_name] = value

    return mapped
