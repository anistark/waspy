# fastapi.testclient.TestClient: requests made to the application in the
# same program, as Starlette's client makes them (a JSON body, query
# parameters by name), each answered by the application's routes.

from typing import Any, Optional

import fastapi
from urllib.parse import quote


class TestClient:
    def __init__(self, app: fastapi.FastAPI) -> None:
        self.app: fastapi.FastAPI = app

    def _url(self, url: str, params: Any) -> str:
        if params is None:
            return url
        given: dict[str, Any] = params
        pairs: list[str] = []
        for k in given:
            pairs.append(quote(k, safe="") + "=" + quote(str(given[k]), safe=""))
        if len(pairs) == 0:
            return url
        return url + ("&" if "?" in url else "?") + "&".join(pairs)

    def request(self, method: str, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> fastapi.Response:
        body: Optional[str] = content
        is_json = False
        if json is not None:
            body = fastapi._jsonable(json)
            is_json = True
        if headers is not None:
            given: dict[str, Any] = headers
            for k in given:
                if k.lower() == "content-type" and "json" in str(given[k]):
                    is_json = True
        return self.app.handle(method, self._url(url, params), body, is_json)

    def get(self, url: str, params: Any = None, headers: Any = None) -> fastapi.Response:
        return self.request("GET", url, None, None, params, headers)

    def delete(self, url: str, params: Any = None, headers: Any = None) -> fastapi.Response:
        return self.request("DELETE", url, None, None, params, headers)

    def post(self, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> fastapi.Response:
        return self.request("POST", url, json, content, params, headers)

    def put(self, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> fastapi.Response:
        return self.request("PUT", url, json, content, params, headers)

    def patch(self, url: str, json: Any = None, content: Optional[str] = None, params: Any = None, headers: Any = None) -> fastapi.Response:
        return self.request("PATCH", url, json, content, params, headers)
