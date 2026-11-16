
# Anthropic, over requests (WASI preview 2's outgoing HTTP).

import requests


class _SyncMessages:
    def __init__(self, base_url: str, api_key: str) -> None:
        self.base_url: str = base_url
        self.api_key: str = api_key

    def create(self, model: str, max_tokens: int, messages: Any, system: Any = None, tools: Any = None, tool_choice: Any = None, thinking: Any = None, output_config: Any = None, stop_sequences: Any = None, metadata: Any = None) -> Message:
        payload = _payload(model, max_tokens, messages, system, tools, tool_choice, thinking, output_config, stop_sequences, metadata)
        try:
            r = requests.post(self.base_url + "/v1/messages", json=payload, headers=_headers(self.api_key))
        except requests.RequestException as e:
            raise APIConnectionError("Connection error.")
        if r.ok:
            return _message(r.json())
        raise _failure(r.status_code, r.text, r.headers.get("request-id"))


class Anthropic:
    def __init__(self, api_key: Optional[str] = None, base_url: Optional[str] = None) -> None:
        settings = _settings(api_key, base_url)
        self.api_key: str = settings[0]
        self.base_url: str = settings[1]
        self.messages: _SyncMessages = _SyncMessages(settings[1], settings[0])
