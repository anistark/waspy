# A client for Anthropic's Messages API in the shape of the `anthropic`
# SDK's AsyncAnthropic (`client.messages.create(...)`), written over the
# httpx client here, so over the host's fetch: a program run in JavaScript.
# The request is the API's JSON (POST /v1/messages with the `x-api-key` and
# `anthropic-version` headers), and the response's content blocks, stop
# reason, and usage are read back as the SDK's attributes. An error answers
# the SDK's exception for its status, with its message.

import os
from typing import Any, Optional

import httpx


API_VERSION = "2026-10-21"


class APIError(Exception):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message: str = message
        self.body: Any = None


class APIConnectionError(APIError):
    pass


class APIStatusError(APIError):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.status_code: int = 0
        self.type: str = ""
        self.request_id: str = ""


class BadRequestError(APIStatusError):
    pass


class AuthenticationError(APIStatusError):
    pass


class PermissionDeniedError(APIStatusError):
    pass


class NotFoundError(APIStatusError):
    pass


class ConflictError(APIStatusError):
    pass


class UnprocessableEntityError(APIStatusError):
    pass


class RateLimitError(APIStatusError):
    pass


class InternalServerError(APIStatusError):
    pass


def _status_error(status: int, message: str) -> APIStatusError:
    if status == 400:
        return BadRequestError(message)
    if status == 401:
        return AuthenticationError(message)
    if status == 403:
        return PermissionDeniedError(message)
    if status == 404:
        return NotFoundError(message)
    if status == 409:
        return ConflictError(message)
    if status == 422:
        return UnprocessableEntityError(message)
    if status == 429:
        return RateLimitError(message)
    if status >= 500:
        return InternalServerError(message)
    return APIStatusError(message)


class Usage:
    def __init__(self, input_tokens: int, output_tokens: int) -> None:
        self.input_tokens: int = input_tokens
        self.output_tokens: int = output_tokens

    def __repr__(self) -> str:
        return "Usage(input_tokens=" + str(self.input_tokens) + ", output_tokens=" + str(self.output_tokens) + ")"


class ContentBlock:
    # One block of a response: `type` says which of the fields it carries
    # ("text": text; "thinking": thinking; "tool_use": id, name, input).
    def __init__(self, type: str) -> None:
        self.type: str = type
        self.text: str = ""
        self.thinking: str = ""
        self.id: str = ""
        self.name: str = ""
        self.input: Any = None

    def __repr__(self) -> str:
        if self.type == "text":
            return "TextBlock(text=" + repr(self.text) + ", type='text')"
        if self.type == "tool_use":
            return "ToolUseBlock(id=" + repr(self.id) + ", name=" + repr(self.name) + ", type='tool_use')"
        return "ContentBlock(type=" + repr(self.type) + ")"


class Message:
    def __init__(self, id: str, model: str, role: str, stop_reason: Optional[str], content: list[ContentBlock], usage: Usage) -> None:
        self.id: str = id
        self.type: str = "message"
        self.model: str = model
        self.role: str = role
        self.stop_reason: Optional[str] = stop_reason
        self.content: list[ContentBlock] = content
        self.usage: Usage = usage


def _text(d: dict[str, Any], key: str) -> str:
    if key in d:
        v = d[key]
        if isinstance(v, str):
            s: str = v
            return s
    return ""


def _int(d: dict[str, Any], key: str) -> int:
    if key in d:
        v = d[key]
        if isinstance(v, int):
            n: int = v
            return n
    return 0


def _message(data: Any) -> Message:
    d: dict[str, Any] = data
    blocks: list[ContentBlock] = []
    if "content" in d:
        items: list[Any] = d["content"]
        for item in items:
            b: dict[str, Any] = item
            block = ContentBlock(_text(b, "type"))
            block.text = _text(b, "text")
            block.thinking = _text(b, "thinking")
            block.id = _text(b, "id")
            block.name = _text(b, "name")
            if "input" in b:
                block.input = b["input"]
            blocks.append(block)
    stop: Optional[str] = None
    if "stop_reason" in d and d["stop_reason"] is not None:
        stop = _text(d, "stop_reason")
    usage = Usage(0, 0)
    if "usage" in d:
        u: dict[str, Any] = d["usage"]
        usage = Usage(_int(u, "input_tokens"), _int(u, "output_tokens"))
    return Message(_text(d, "id"), _text(d, "model"), _text(d, "role"), stop, blocks, usage)


class _Messages:
    def __init__(self, base_url: str, api_key: str) -> None:
        self.base_url: str = base_url
        self.api_key: str = api_key

    async def create(self, model: str, max_tokens: int, messages: Any, system: Any = None, tools: Any = None, tool_choice: Any = None, thinking: Any = None, output_config: Any = None, stop_sequences: Any = None, metadata: Any = None) -> Message:
        payload: dict[str, Any] = {}
        payload["model"] = model
        payload["max_tokens"] = max_tokens
        payload["messages"] = messages
        if system is not None:
            payload["system"] = system
        if tools is not None:
            payload["tools"] = tools
        if tool_choice is not None:
            payload["tool_choice"] = tool_choice
        if thinking is not None:
            payload["thinking"] = thinking
        if output_config is not None:
            payload["output_config"] = output_config
        if stop_sequences is not None:
            payload["stop_sequences"] = stop_sequences
        if metadata is not None:
            payload["metadata"] = metadata
        headers: dict[str, Any] = {}
        headers["x-api-key"] = self.api_key
        headers["anthropic-version"] = API_VERSION
        client = httpx.AsyncClient()
        try:
            r = await client.post(self.base_url + "/v1/messages", json=payload, headers=headers)
        except httpx.RequestError as e:
            raise APIConnectionError("Connection error.")
        if r.is_success:
            return _message(r.json())
        body: Any = None
        error_type = ""
        try:
            body = r.json()
        except ValueError:
            body = r.text
        if isinstance(body, dict):
            d: dict[str, Any] = body
            if "error" in d and isinstance(d["error"], dict):
                inner: dict[str, Any] = d["error"]
                error_type = _text(inner, "type")
        error = _status_error(r.status_code, "Error code: " + str(r.status_code) + " - " + str(body))
        error.status_code = r.status_code
        error.body = body
        error.type = error_type
        if "request-id" in r.headers:
            error.request_id = r.headers["request-id"]
        raise error


class AsyncAnthropic:
    def __init__(self, api_key: Optional[str] = None, base_url: Optional[str] = None) -> None:
        key = ""
        if api_key is not None:
            key = str(api_key)
        elif "ANTHROPIC_API_KEY" in os.environ:
            key = os.environ["ANTHROPIC_API_KEY"]
        url = "https://api.anthropic.com"
        if base_url is not None:
            url = str(base_url)
        elif "ANTHROPIC_BASE_URL" in os.environ:
            url = os.environ["ANTHROPIC_BASE_URL"]
        self.api_key: str = key
        self.base_url: str = url
        self.messages: _Messages = _Messages(url, key)
