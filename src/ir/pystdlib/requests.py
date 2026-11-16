# requests, over WASI preview 2's outgoing HTTP: a program run under a WASI
# runtime with HTTP (`wasmtime run -S http`) sends each request through the
# host, synchronously. The request is requests 2.x's (query parameters,
# headers, a JSON, form, or text body), and so are the response's
# attributes and errors. The host call is `__waspy_http` (`compiler::http`),
# which answers the status, the headers, and the body in one str.

import json
from typing import Any, Optional

from urllib.parse import quote_plus, urljoin, urlsplit


_PHRASES = {
    100: "Continue", 101: "Switching Protocols",
    200: "OK", 201: "Created", 202: "Accepted", 203: "Non-Authoritative Information",
    204: "No Content", 205: "Reset Content", 206: "Partial Content",
    301: "Moved Permanently", 302: "Found", 303: "See Other", 304: "Not Modified",
    307: "Temporary Redirect", 308: "Permanent Redirect",
    400: "Bad Request", 401: "Unauthorized", 402: "Payment Required", 403: "Forbidden",
    404: "Not Found", 405: "Method Not Allowed", 406: "Not Acceptable", 408: "Request Timeout",
    409: "Conflict", 410: "Gone", 411: "Length Required", 412: "Precondition Failed",
    413: "Content Too Large", 414: "URI Too Long", 415: "Unsupported Media Type",
    418: "I'm a Teapot", 422: "Unprocessable Content", 425: "Too Early",
    426: "Upgrade Required", 428: "Precondition Required", 429: "Too Many Requests",
    431: "Request Header Fields Too Large", 451: "Unavailable For Legal Reasons",
    500: "Internal Server Error", 501: "Not Implemented", 502: "Bad Gateway",
    503: "Service Unavailable", 504: "Gateway Timeout", 505: "HTTP Version Not Supported",
}

# The `error-code` cases of `wasi:http/types`, in order.
_ERROR_CODES = [
    "DNS-timeout", "DNS-error", "destination-not-found", "destination-unavailable",
    "destination-IP-prohibited", "destination-IP-unroutable", "connection-refused",
    "connection-terminated", "connection-timeout", "connection-read-timeout",
    "connection-write-timeout", "connection-limit-reached", "TLS-protocol-error",
    "TLS-certificate-error", "TLS-alert-received", "HTTP-request-denied",
    "HTTP-request-length-required", "HTTP-request-body-size", "HTTP-request-method-invalid",
    "HTTP-request-URI-invalid", "HTTP-request-URI-too-long", "HTTP-request-header-section-size",
    "HTTP-request-header-size", "HTTP-request-trailer-section-size", "HTTP-request-trailer-size",
    "HTTP-response-incomplete", "HTTP-response-header-section-size", "HTTP-response-header-size",
    "HTTP-response-body-size", "HTTP-response-trailer-section-size", "HTTP-response-trailer-size",
    "HTTP-response-transfer-coding", "HTTP-response-content-coding", "HTTP-response-timeout",
    "HTTP-upgrade-failed", "HTTP-protocol-error", "loop-detected", "configuration-error",
    "internal-error",
]


class RequestException(OSError):
    pass


class ConnectionError(RequestException):
    pass


class Timeout(RequestException):
    pass


class HTTPError(RequestException):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.response: Response = Response(0, {}, "", "")


class InvalidURL(RequestException):
    pass


class MissingSchema(RequestException):
    pass


class InvalidSchema(RequestException):
    pass


class InvalidHeader(RequestException):
    pass


class TooManyRedirects(RequestException):
    pass


class CaseInsensitiveDict:
    def __init__(self) -> None:
        self._store: dict[str, str] = {}
        self._names: dict[str, str] = {}

    def __setitem__(self, key: str, value: str) -> None:
        self._store[key.lower()] = value
        self._names[key.lower()] = key

    def __getitem__(self, key: str) -> str:
        return self._store[key.lower()]

    def __contains__(self, key: str) -> bool:
        return key.lower() in self._store

    def __len__(self) -> int:
        return len(self._store)

    def get(self, key: str, default: Optional[str] = None) -> Optional[str]:
        if key.lower() in self._store:
            return self._store[key.lower()]
        return default

    def keys(self) -> list[str]:
        return [self._names[k] for k in self._store]

    def items(self) -> list[tuple[str, str]]:
        return [(self._names[k], self._store[k]) for k in self._store]

    def __repr__(self) -> str:
        parts: list[str] = []
        for k in self._store:
            parts.append(repr(self._names[k]) + ": " + repr(self._store[k]))
        return "{" + ", ".join(parts) + "}"


class Response:
    def __init__(self, status_code: int, headers: dict[str, str], text: str, url: str) -> None:
        self.status_code: int = status_code
        self.headers: CaseInsensitiveDict = CaseInsensitiveDict()
        for k in headers:
            self.headers[k] = headers[k]
        self.text: str = text
        self.url: str = url
        self.reason: str = _PHRASES[status_code] if status_code in _PHRASES else ""
        self.ok: bool = status_code < 400
        self.encoding: str = "utf-8"
        self.history: list[Response] = []
        self.is_redirect: bool = status_code in [301, 302, 303, 307, 308] and "location" in self.headers

    @property
    def content(self) -> bytes:
        return self.text.encode()

    def json(self) -> Any:
        return json.loads(self.text)

    def raise_for_status(self) -> None:
        kind = ""
        code = self.status_code
        if 400 <= code < 500:
            kind = "Client Error"
        elif 500 <= code < 600:
            kind = "Server Error"
        if kind != "":
            error = HTTPError(str(code) + " " + kind + ": " + self.reason + " for url: " + self.url)
            error.response = self
            raise error

    def __repr__(self) -> str:
        return "<Response [" + str(self.status_code) + "]>"


def _form(values: Any) -> str:
    given: dict[str, Any] = values
    pairs: list[str] = []
    for k in given:
        pairs.append(quote_plus(k) + "=" + quote_plus(str(given[k])))
    return "&".join(pairs)


def _send(method: str, url: str, params: Any, data: Any, body_json: Any, headers: Any) -> Response:
    parts = urlsplit(url)
    if parts.scheme == "":
        raise MissingSchema("Invalid URL '" + url + "': No scheme supplied. Perhaps you meant https://" + url + "?")
    if parts.scheme != "http" and parts.scheme != "https":
        raise InvalidSchema("No connection adapters were found for '" + url + "'")
    if parts.netloc == "":
        raise InvalidURL("Invalid URL '" + url + "': No host supplied")
    query = parts.query
    if params is not None:
        extra = _form(params)
        if extra != "":
            query = query + "&" + extra if query != "" else extra
    target = parts.path if parts.path != "" else "/"
    if query != "":
        target = target + "?" + query
    full = parts.scheme + "://" + parts.netloc + target
    sent: dict[str, str] = {}
    sent["User-Agent"] = "python-requests/2.34.2"
    sent["Accept"] = "*/*"
    body = ""
    if body_json is not None:
        body = json.dumps(body_json)
        sent["Content-Type"] = "application/json"
    elif data is not None:
        if isinstance(data, str):
            text: str = data
            body = text
        else:
            body = _form(data)
            sent["Content-Type"] = "application/x-www-form-urlencoded"
    # requests sends a body's length, and an empty one's for a method that
    # carries one.
    if body != "" or method.upper() in ["POST", "PUT", "PATCH"]:
        sent["Content-Length"] = str(len(body.encode()))
    if headers is not None:
        given: dict[str, Any] = headers
        for k in given:
            v = str(given[k])
            if "\n" in v or "\r" in v or "\n" in k:
                raise InvalidHeader("Invalid leading whitespace, reserved character(s), or return character(s) in header value: " + repr(v))
            sent[k] = v
    blob = ""
    for k in sent:
        blob += k + "\n" + sent[k] + "\n"
    answer: str = __waspy_http(method.upper(), parts.scheme, parts.netloc, target, blob, body)
    if answer.startswith("E"):
        code = int(answer[3:])
        cause = _ERROR_CODES[code] if code < len(_ERROR_CODES) else "the request could not be sent"
        port = str(parts.port) if parts.port is not None else ("443" if parts.scheme == "https" else "80")
        pool = "HTTPSConnectionPool" if parts.scheme == "https" else "HTTPConnectionPool"
        message = pool + "(host='" + str(parts.hostname) + "', port=" + port + "): Max retries exceeded with url: " + target + " (Caused by " + cause + ")"
        if cause.endswith("timeout"):
            raise Timeout(message)
        raise ConnectionError(message)
    status = int(answer[1:4])
    got: dict[str, str] = {}
    at = 5
    while True:
        end = answer.find("\n", at)
        if end == at:
            at = end + 1
            break
        name = answer[at:end]
        stop = answer.find("\n", end + 1)
        value = answer[end + 1:stop]
        if name in got:
            got[name] = got[name] + ", " + value
        else:
            got[name] = value
        at = stop + 1
    return Response(status, got, answer[at:], full)


def _follow(method: str, url: str, params: Any, data: Any, body_json: Any, headers: Any, allow_redirects: bool) -> Response:
    # requests follows a redirect's Location (up to 30), as a GET for a 303
    # and for a POST answered 301 or 302, without the body.
    r = _send(method, url, params, data, body_json, headers)
    history: list[Response] = []
    while allow_redirects and r.is_redirect:
        if len(history) >= 30:
            raise TooManyRedirects("Exceeded 30 redirects.")
        history.append(r)
        location = str(r.headers.get("location"))
        code = r.status_code
        if code == 303 and method.upper() != "HEAD":
            method = "GET"
        if (code == 301 or code == 302) and method.upper() == "POST":
            method = "GET"
        if method.upper() == "GET":
            data = None
            body_json = None
        r = _send(method, urljoin(r.url, location), None, data, body_json, headers)
    r.history = history
    return r


def request(method: str, url: str, params: Any = None, data: Any = None, json: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = True) -> Response:
    return _follow(method, url, params, data, json, headers, allow_redirects)


def get(url: str, params: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = True) -> Response:
    return _follow("GET", url, params, None, None, headers, allow_redirects)


def head(url: str, params: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = False) -> Response:
    return _follow("HEAD", url, params, None, None, headers, allow_redirects)


def delete(url: str, params: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = True) -> Response:
    return _follow("DELETE", url, params, None, None, headers, allow_redirects)


def post(url: str, data: Any = None, json: Any = None, params: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = True) -> Response:
    return _follow("POST", url, params, data, json, headers, allow_redirects)


def put(url: str, data: Any = None, json: Any = None, params: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = True) -> Response:
    return _follow("PUT", url, params, data, json, headers, allow_redirects)


def patch(url: str, data: Any = None, json: Any = None, params: Any = None, headers: Any = None, timeout: Any = None, allow_redirects: bool = True) -> Response:
    return _follow("PATCH", url, params, data, json, headers, allow_redirects)
