
# AsyncAnthropic, over httpx's AsyncClient (the host's fetch).

import httpx


class _Messages:
    def __init__(self, base_url: str, api_key: str) -> None:
        self.base_url: str = base_url
        self.api_key: str = api_key

    async def create(self, model: str, max_tokens: int, messages: Any, system: Any = None, tools: Any = None, tool_choice: Any = None, thinking: Any = None, output_config: Any = None, stop_sequences: Any = None, metadata: Any = None) -> Message:
        payload = _payload(model, max_tokens, messages, system, tools, tool_choice, thinking, output_config, stop_sequences, metadata)
        client = httpx.AsyncClient()
        try:
            r = await client.post(self.base_url + "/v1/messages", json=payload, headers=_headers(self.api_key))
        except httpx.RequestError as e:
            raise APIConnectionError("Connection error.")
        if r.is_success:
            return _message(r.json())
        request_id: Optional[str] = None
        if "request-id" in r.headers:
            request_id = r.headers["request-id"]
        raise _failure(r.status_code, r.text, request_id)


class AsyncAnthropic:
    def __init__(self, api_key: Optional[str] = None, base_url: Optional[str] = None) -> None:
        settings = _settings(api_key, base_url)
        self.api_key: str = settings[0]
        self.base_url: str = settings[1]
        self.messages: _Messages = _Messages(settings[1], settings[0])
