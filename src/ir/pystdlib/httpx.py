# httpx's AsyncClient, over the host's fetch (a program run in JavaScript,
# through the bridge): requests with query parameters, headers, and a JSON
# or text body, and responses with httpx 0.28's attributes and errors. A
# program run under WASI has no fetch, and there is no synchronous client.

import json
from typing import Any, Optional

from pyodide.ffi import to_js
from pyodide.http import AbortError, pyfetch
from urllib.parse import quote


_PHRASES = {
    200: "OK", 201: "Created", 202: "Accepted", 204: "No Content",
    301: "Moved Permanently", 302: "Found", 304: "Not Modified",
    400: "Bad Request", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found",
    405: "Method Not Allowed", 409: "Conflict", 422: "Unprocessable Entity",
    429: "Too Many Requests", 500: "Internal Server Error", 502: "Bad Gateway",
    503: "Service Unavailable", 504: "Gateway Timeout", 529: "",
}


class HTTPError(Exception):
    pass


class RequestError(HTTPError):
    pass


class TransportError(RequestError):
    pass


class NetworkError(TransportError):
    pass


class ConnectError(NetworkError):
    pass


class Response:
    def __init__(self, status_code: int, reason_phrase: str, text: str, headers: dict[str, str], url: str) -> None:
        self.status_code: int = status_code
        self.reason_phrase: str = reason_phrase
        self.text: str = text
        self.headers: dict[str, str] = headers
        self.url: str = url
        self.is_success: bool = 200 <= status_code < 300
        self.is_error: bool = status_code >= 400

    def json(self) -> Any:
        return json.loads(self.text)

    def raise_for_status(self) -> "Response":
        if self.is_success:
            return self
        kind = "Informational response"
        code = self.status_code
        if 300 <= code < 400:
            kind = "Redirect response"
        elif 400 <= code < 500:
            kind = "Client error"
        elif code >= 500:
            kind = "Server error"
        message = kind + " '" + str(self.status_code) + " " + self.reason_phrase + "' for url '" + self.url + "'"
        message += "\nFor more information check: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/" + str(self.status_code)
        error = HTTPStatusError(message)
        error.response = self
        raise error


class HTTPStatusError(HTTPError):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.response: Response = Response(0, "", "", {}, "")


def _encode(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _query(url: str, params: Any) -> str:
    if params is None:
        return url
    given: dict[str, Any] = params
    pairs: list[str] = []
    for k in given:
        pairs.append(quote(k, safe="") + "=" + quote(str(given[k]), safe=""))
    if len(pairs) == 0:
        return url
    return url + ("&" if "?" in url else "?") + "&".join(pairs)


class AsyncClient:
    def __init__(self, base_url: str = "", headers: Any = None, timeout: float = 5.0) -> None:
        self.base_url: str = base_url
        self.headers: dict[str, str] = {}
        if headers is not None:
            given: dict[str, Any] = headers
            for k in given:
                self.headers[k] = str(given[k])
        self.is_closed: bool = False

    async def __aenter__(self) -> "AsyncClient":
        return self

    async def __aexit__(self, exc_type, exc, tb) -> None:
        self.is_closed = True

    async def aclose(self) -> None:
        self.is_closed = True

    async def request(self, method: str, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> Response:
        target = _query(url if url.startswith("http") else self.base_url + url, params)
        sent: dict[str, str] = {}
        for k in self.headers:
            sent[k] = self.headers[k]
        body = ""
        has_body = False
        if json is not None:
            body = _encode(json)
            has_body = True
            sent["Content-Type"] = "application/json"
        elif content is not None:
            body = str(content)
            has_body = True
        if headers is not None:
            given: dict[str, Any] = headers
            for k in given:
                sent[k] = str(given[k])
        try:
            if has_body:
                r = await pyfetch(target, method=method, headers=to_js(sent), body=body)
            else:
                r = await pyfetch(target, method=method, headers=to_js(sent))
        except AbortError as e:
            raise ConnectError(str(e))
        text = await r.text()
        phrase: str = r.status_text
        if phrase == "" and r.status in _PHRASES:
            phrase = _PHRASES[r.status]
        return Response(r.status, phrase, text, r.headers, target)

    async def get(self, url: str, params: Any = None, headers: Any = None) -> Response:
        return await self.request("GET", url, None, None, params, headers)

    async def delete(self, url: str, params: Any = None, headers: Any = None) -> Response:
        return await self.request("DELETE", url, None, None, params, headers)

    async def post(self, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> Response:
        return await self.request("POST", url, json, content, params, headers)

    async def put(self, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> Response:
        return await self.request("PUT", url, json, content, params, headers)

    async def patch(self, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> Response:
        return await self.request("PATCH", url, json, content, params, headers)
