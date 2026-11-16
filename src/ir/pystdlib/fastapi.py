# FastAPI's application, the subset a compiled program can have: routes
# declared with `@app.get(...)` and its kin on functions of the module.
# `ir::shims` writes the function that matches a request to a route, reads
# its path and query parameters and its JSON body by the handler's
# annotations, and writes the response, from the program's own routes;
# what is here is what every application shares. Responses are FastAPI
# 0.163's: compact JSON, and its 404, 405, and 422 bodies.

import json
from typing import Any, Optional

import pydantic
from urllib.parse import parse_qsl, unquote


_PHRASES = {
    200: "OK", 201: "Created", 202: "Accepted", 204: "No Content",
    301: "Moved Permanently", 302: "Found", 304: "Not Modified",
    400: "Bad Request", 401: "Unauthorized", 402: "Payment Required", 403: "Forbidden",
    404: "Not Found", 405: "Method Not Allowed", 409: "Conflict", 410: "Gone",
    418: "I'm a Teapot", 422: "Unprocessable Content", 429: "Too Many Requests",
    500: "Internal Server Error", 501: "Not Implemented", 502: "Bad Gateway",
    503: "Service Unavailable", 504: "Gateway Timeout",
}


class HTTPException(Exception):
    def __init__(self, status_code: int, detail: Optional[str] = None) -> None:
        text = ""
        if detail is not None:
            text = str(detail)
        elif status_code in _PHRASES:
            text = _PHRASES[status_code]
        super().__init__(text)
        self.status_code: int = status_code
        self.detail: str = text

    def __str__(self) -> str:
        return str(self.status_code) + ": " + self.detail


class Response:
    def __init__(self, status_code: int, text: str) -> None:
        self.status_code: int = status_code
        self.text: str = text
        self.headers: dict[str, str] = {"content-type": "application/json"}
        self.is_success: bool = 200 <= status_code < 300

    def json(self) -> Any:
        return json.loads(self.text)


class FastAPI:
    def __init__(self, title: str = "FastAPI", version: str = "0.1.0") -> None:
        self.title: str = title
        self.version: str = version

    def handle(self, method: str, url: str, body: Optional[str] = None, json_body: bool = True) -> Response:
        return __py_fastapi__routes(method.upper(), url, body, json_body)


def _path(url: str) -> list[str]:
    i = url.find("?")
    path = url if i < 0 else url[:i]
    out: list[str] = []
    for part in path.split("/"):
        if part != "":
            out.append(unquote(part))
    return out


def _query(url: str) -> dict[str, str]:
    i = url.find("?")
    out: dict[str, str] = {}
    if i < 0:
        return out
    for pair in parse_qsl(url[i + 1:], keep_blank_values=True):
        out[pair[0]] = pair[1]
    return out


def _error(status: int, detail: str) -> Response:
    return Response(status, "{\"detail\":" + pydantic._json_str(detail) + "}")


def _invalid(errs: list[pydantic._Err]) -> Response:
    parts: list[str] = []
    for e in errs:
        parts.append(pydantic._err_json(e))
    return Response(422, "{\"detail\":[" + ",".join(parts) + "]}")


def _param(raw: str) -> Any:
    return raw


def _body(body: Optional[str], is_json: bool, errs: list[pydantic._Err]) -> Any:
    if body is None or body == "":
        errs.append(pydantic._missing(["body"], None))
        return None
    text: str = str(body)
    if not is_json:
        raw: Any = text
        return raw
    try:
        return json.loads(text)
    except json.JSONDecodeError as e:
        # "<msg>: line L column C (char P)", as CPython writes it.
        report = str(e)
        cut = report.rfind(": line ")
        at = report.rfind("(char ")
        pos = report[at + 6: len(report) - 1] if at >= 0 else "0"
        errs.append(pydantic._Err("json_invalid", ["body", "#" + pos], "JSON decode error", pydantic._empty(), report[:cut] if cut >= 0 else report))
        return None


def _jsonable(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
