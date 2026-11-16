//! Packages from outside the standard library, in the shape their own code
//! expects (`pydantic`, `fastapi`, `httpx`, `anthropic`; `ir::shims`).
//!
//! Expected values are what the real packages answer for the same source:
//! pydantic 2.14 and FastAPI 0.163 under CPython 3.13, and httpx 0.28 for
//! the same responses (served there by `httpx.MockTransport`, here by the
//! harness's `fetch`).

#[path = "../utils/harness.rs"]
mod harness;

#[allow(unused_imports)]
use harness::*;

/// Run a program's `__main__` block and compare its standard output.
fn prints(source: &str, want: &str) {
    let run = run_main(&dedent(source), "", &[]);
    assert_eq!(run.stderr, "", "stderr");
    assert_eq!(run.stdout, want);
}

/// A model's constructor, repr, str, dump, JSON, comparison, inheritance, and methods, and pydantic's lax validation of a dict and of JSON text with its errors, as pydantic 2.14 prints them.
#[test]
fn pydantic_models_validate_dump_and_compare() {
    prints(
        r##"
        from typing import Optional

        from pydantic import BaseModel, ValidationError


        class Address(BaseModel):
            city: str
            zip: str = "00000"


        class User(BaseModel):
            name: str
            age: int
            score: float = 0.0
            admin: bool = False
            tags: list[str] = []
            nick: Optional[str] = None
            address: Optional[Address] = None

            def label(self) -> str:
                return self.name.upper() + "#" + str(self.age)


        class Admin(User):
            level: int = 1


        def main() -> None:
            u = User(name="ada", age=36)
            print(u)
            print(repr(u))
            print(u.model_dump())
            print(u.model_dump_json())
            v = User.model_validate({"name": "bo", "age": "41", "score": 3, "tags": ["x", "y"], "address": {"city": "Oslo"}})
            print(repr(v))
            print(v.address.city, v.age + 1, v.score, v.label())
            w = User.model_validate_json('{"name": "cy", "age": 7.0, "admin": "yes", "nick": "c"}')
            print(w.model_dump_json())
            print(u == User(name="ada", age=36), u == v, isinstance(u, BaseModel))
            a = Admin.model_validate({"name": "root", "age": 1, "level": "3"})
            print(a, a.label())
            for bad in [{"name": 5, "age": "x"}, {"age": 3}, {"name": "a", "age": 1, "tags": ["a", 2]}, {"name": "a", "age": 1.5}, {"name": "a", "age": 1, "admin": "maybe"}, {"name": "a", "age": 1, "address": {"zip": "1"}}, {"name": "a", "age": 1, "score": "abc"}]:
                try:
                    User.model_validate(bad)
                except ValidationError as e:
                    print(e.error_count())
                    print(e)
            u.age = 40
            print(u.age)
            try:
                User.model_validate([1, 2])
            except ValueError as e:
                print(type(e).__name__, e)


        if __name__ == "__main__":
            main()
"##,
        r#"name='ada' age=36 score=0.0 admin=False tags=[] nick=None address=None
User(name='ada', age=36, score=0.0, admin=False, tags=[], nick=None, address=None)
{'name': 'ada', 'age': 36, 'score': 0.0, 'admin': False, 'tags': [], 'nick': None, 'address': None}
{"name":"ada","age":36,"score":0.0,"admin":false,"tags":[],"nick":null,"address":null}
User(name='bo', age=41, score=3.0, admin=False, tags=['x', 'y'], nick=None, address=Address(city='Oslo', zip='00000'))
Oslo 42 3.0 BO#41
{"name":"cy","age":7,"score":0.0,"admin":true,"tags":[],"nick":"c","address":null}
True False True
name='root' age=1 score=0.0 admin=False tags=[] nick=None address=None level=3 ROOT#1
2
2 validation errors for User
name
  Input should be a valid string [type=string_type, input_value=5, input_type=int]
    For further information visit https://errors.pydantic.dev/2.14/v/string_type
age
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='x', input_type=str]
    For further information visit https://errors.pydantic.dev/2.14/v/int_parsing
1
1 validation error for User
name
  Field required [type=missing, input_value={'age': 3}, input_type=dict]
    For further information visit https://errors.pydantic.dev/2.14/v/missing
1
1 validation error for User
tags.1
  Input should be a valid string [type=string_type, input_value=2, input_type=int]
    For further information visit https://errors.pydantic.dev/2.14/v/string_type
1
1 validation error for User
age
  Input should be a valid integer, got a number with a fractional part [type=int_from_float, input_value=1.5, input_type=float]
    For further information visit https://errors.pydantic.dev/2.14/v/int_from_float
1
1 validation error for User
admin
  Input should be a valid boolean, unable to interpret input [type=bool_parsing, input_value='maybe', input_type=str]
    For further information visit https://errors.pydantic.dev/2.14/v/bool_parsing
1
1 validation error for User
address.city
  Field required [type=missing, input_value={'zip': '1'}, input_type=dict]
    For further information visit https://errors.pydantic.dev/2.14/v/missing
1
1 validation error for User
score
  Input should be a valid number, unable to parse string as a number [type=float_parsing, input_value='abc', input_type=str]
    For further information visit https://errors.pydantic.dev/2.14/v/float_parsing
40
ValidationError 1 validation error for User
  Input should be a valid dictionary or instance of User [type=model_type, input_value=[1, 2], input_type=list]
    For further information visit https://errors.pydantic.dev/2.14/v/model_type
"#,
    );
}

/// Each lax conversion pydantic 2.14 makes from a JSON-shaped value into int, float, bool, str, list, and dict fields, a long input's shortened repr, and models nested in lists, dicts, and optional fields.
#[test]
fn pydantic_lax_conversions_and_nested_models() {
    prints(
        r#"
        from typing import Any, Optional

        from pydantic import BaseModel, ValidationError


        class M(BaseModel):
            i: int = 0
            f: float = 0.0
            b: bool = False
            s: str = ""
            l: list[int] = []
            d: dict[str, float] = {}


        class Box(BaseModel):
            items: list[M] = []
            named: dict[str, M] = {}
            best: Optional[M] = None


        def t(key: str, v: Any) -> None:
            data: dict[str, Any] = {}
            data[key] = v
            try:
                print(M.model_validate(data).model_dump())
            except ValidationError as e:
                for line in str(e).splitlines()[1:3]:
                    print(line)


        def main() -> None:
            for v in [" 5 ", "+7", "-3", "1_000", "5.0", "5.5", "1e3", 2.0, True, None, [0], "", "0x10", float("inf")]:
                t("i", v)
            for v in ["1.5", " 2 ", "1e3", True, "inf", "x", None, 3]:
                t("f", v)
            for v in ["true", "True", "yes", "on", "1", "0", "off", "f", "n", "no", "FALSE", 1, 0, 2, 1.0, 0.5, None]:
                t("b", v)
            for v in [5, 1.5, True, None, ["a"]]:
                t("s", v)
            t("s", "x" * 60)
            t("i", "y" * 60)
            t("l", ["a", 1])
            t("d", {"a": 1, "b": "x"})
            t("d", [1])
            b = Box.model_validate({"items": [{"i": "3"}, {"b": "yes"}], "named": {"k": {"s": "v"}}, "best": {"f": 2}})
            print(b)
            print(b.model_dump_json())
            try:
                Box.model_validate({"items": [{"i": "z"}, 5], "named": {"k": {"l": ["q"]}}, "best": "no"})
            except ValueError as e:
                print(e)
            print(b == Box.model_validate_json(b.model_dump_json()))


        if __name__ == "__main__":
            main()
"#,
        r#"{'i': 5, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 7, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': -3, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 1000, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 5, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
i
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='5.5', input_type=str]
i
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='1e3', input_type=str]
{'i': 2, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 1, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
i
  Input should be a valid integer [type=int_type, input_value=None, input_type=NoneType]
i
  Input should be a valid integer [type=int_type, input_value=[0], input_type=list]
i
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='', input_type=str]
i
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='0x10', input_type=str]
i
  Input should be a finite number [type=finite_number, input_value=inf, input_type=float]
{'i': 0, 'f': 1.5, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 2.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 1000.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 1.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': inf, 'b': False, 's': '', 'l': [], 'd': {}}
f
  Input should be a valid number, unable to parse string as a number [type=float_parsing, input_value='x', input_type=str]
f
  Input should be a valid number [type=float_type, input_value=None, input_type=NoneType]
{'i': 0, 'f': 3.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
{'i': 0, 'f': 0.0, 'b': False, 's': '', 'l': [], 'd': {}}
b
  Input should be a valid boolean, unable to interpret input [type=bool_parsing, input_value=2, input_type=int]
{'i': 0, 'f': 0.0, 'b': True, 's': '', 'l': [], 'd': {}}
b
  Input should be a valid boolean [type=bool_type, input_value=0.5, input_type=float]
b
  Input should be a valid boolean [type=bool_type, input_value=None, input_type=NoneType]
s
  Input should be a valid string [type=string_type, input_value=5, input_type=int]
s
  Input should be a valid string [type=string_type, input_value=1.5, input_type=float]
s
  Input should be a valid string [type=string_type, input_value=True, input_type=bool]
s
  Input should be a valid string [type=string_type, input_value=None, input_type=NoneType]
s
  Input should be a valid string [type=string_type, input_value=['a'], input_type=list]
{'i': 0, 'f': 0.0, 'b': False, 's': 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', 'l': [], 'd': {}}
i
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='yyyyyyyyyyyyyyyyyyyyyyyy...yyyyyyyyyyyyyyyyyyyyyyy', input_type=str]
l.0
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='a', input_type=str]
d.b
  Input should be a valid number, unable to parse string as a number [type=float_parsing, input_value='x', input_type=str]
d
  Input should be a valid dictionary [type=dict_type, input_value=[1], input_type=list]
items=[M(i=3, f=0.0, b=False, s='', l=[], d={}), M(i=0, f=0.0, b=True, s='', l=[], d={})] named={'k': M(i=0, f=0.0, b=False, s='v', l=[], d={})} best=M(i=0, f=2.0, b=False, s='', l=[], d={})
{"items":[{"i":3,"f":0.0,"b":false,"s":"","l":[],"d":{}},{"i":0,"f":0.0,"b":true,"s":"","l":[],"d":{}}],"named":{"k":{"i":0,"f":0.0,"b":false,"s":"v","l":[],"d":{}}},"best":{"i":0,"f":2.0,"b":false,"s":"","l":[],"d":{}}}
4 validation errors for Box
items.0.i
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='z', input_type=str]
    For further information visit https://errors.pydantic.dev/2.14/v/int_parsing
items.1
  Input should be a valid dictionary or instance of M [type=model_type, input_value=5, input_type=int]
    For further information visit https://errors.pydantic.dev/2.14/v/model_type
named.k.l.0
  Input should be a valid integer, unable to parse string as an integer [type=int_parsing, input_value='q', input_type=str]
    For further information visit https://errors.pydantic.dev/2.14/v/int_parsing
best
  Input should be a valid dictionary or instance of M [type=model_type, input_value='no', input_type=str]
    For further information visit https://errors.pydantic.dev/2.14/v/model_type
True
"#,
    );
}

/// An application's routes driven through the test client: path, query, and body parameters read by their annotations, a model returned, HTTPException, and FastAPI 0.163's 404, 405, and 422 bodies.
#[test]
fn fastapi_routes_answer_as_fastapi_does() {
    prints(
        r#"
        from typing import Optional
        from fastapi import FastAPI, HTTPException
        from fastapi.testclient import TestClient
        from pydantic import BaseModel

        app = FastAPI()


        class Item(BaseModel):
            name: str
            price: float
            tags: list[str] = []


        items: dict[int, Item] = {}


        @app.get("/")
        def root():
            return {"message": "hello"}


        @app.get("/items/{item_id}")
        def read_item(item_id: int, q: Optional[str] = None):
            if item_id not in items:
                raise HTTPException(status_code=404, detail="Item not found")
            return {"id": item_id, "name": items[item_id].name, "q": q}


        @app.post("/items/{item_id}", status_code=201)
        def create_item(item_id: int, item: Item) -> Item:
            items[item_id] = item
            return item


        @app.get("/search")
        def search(term: str, limit: int = 10):
            return [term] * min(limit, 3)


        @app.delete("/items/{item_id}")
        def delete_item(item_id: int):
            del items[item_id]
            return {"deleted": item_id}


        @app.get("/all")
        def all_items() -> list[Item]:
            return [items[k] for k in items]


        def main() -> None:
            client = TestClient(app)
            r = client.get("/")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/items/1")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.post("/items/1", json={"name": "pen", "price": 2})
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/items/1?q=blue")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/items/abc")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.post("/items/2", json={"name": "x"})
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.post("/items/2")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/search?term=a&limit=2")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/search", params={"term": "z z"})
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/search")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/nope")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.put("/items/1")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/all")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.delete("/items/1")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.get("/search?term=b&limit=x")
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.post("/items/3", json={"name": "q", "price": "cheap", "tags": "no"})
            print(r.status_code, r.headers["content-type"], r.text)
            r = client.post("/items/3", json={"name": "q", "price": "cheap"})
            print(r.json()["detail"][0]["loc"])
            r = client.post("/items/9", content="{bad")
            print(r.status_code, r.text)
            print(str(HTTPException(status_code=418)), HTTPException(status_code=404, detail="x").detail)


        if __name__ == "__main__":
            main()
"#,
        r#"200 application/json {"message":"hello"}
404 application/json {"detail":"Item not found"}
201 application/json {"name":"pen","price":2.0,"tags":[]}
200 application/json {"id":1,"name":"pen","q":"blue"}
422 application/json {"detail":[{"type":"int_parsing","loc":["path","item_id"],"msg":"Input should be a valid integer, unable to parse string as an integer","input":"abc"}]}
422 application/json {"detail":[{"type":"missing","loc":["body","price"],"msg":"Field required","input":{"name":"x"}}]}
422 application/json {"detail":[{"type":"missing","loc":["body"],"msg":"Field required","input":null}]}
200 application/json ["a","a"]
200 application/json ["z z","z z","z z"]
422 application/json {"detail":[{"type":"missing","loc":["query","term"],"msg":"Field required","input":null}]}
404 application/json {"detail":"Not Found"}
405 application/json {"detail":"Method Not Allowed"}
200 application/json [{"name":"pen","price":2.0,"tags":[]}]
200 application/json {"deleted":1}
422 application/json {"detail":[{"type":"int_parsing","loc":["query","limit"],"msg":"Input should be a valid integer, unable to parse string as an integer","input":"x"}]}
422 application/json {"detail":[{"type":"float_parsing","loc":["body","price"],"msg":"Input should be a valid number, unable to parse string as a number","input":"cheap"},{"type":"list_type","loc":["body","tags"],"msg":"Input should be a valid list","input":"no"}]}
['body', 'price']
422 {"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":"{bad"}]}
418: I'm a Teapot x
"#,
    );
}

/// A request body that is not a dict, a raw body sent without a JSON content type, and a nested model's errors, as FastAPI validates a body from attributes.
#[test]
fn fastapi_bodies_that_are_not_models() {
    prints(
        r#"
        from typing import Optional
        from fastapi import FastAPI
        from fastapi.testclient import TestClient
        from pydantic import BaseModel
        app = FastAPI()
        class A(BaseModel):
            city: str
        class P(BaseModel):
            name: str
            home: Optional[A] = None
        @app.post("/p")
        def make(p: P) -> P:
            return p
        def main() -> None:
            c = TestClient(app)
            for r in [c.post("/p", json=[1]), c.post("/p", json={"name": "a", "home": 5}), c.post("/p", json={"name": "a", "home": {}}), c.post("/p", content="x"), c.post("/p", content='{"name":"b"}'), c.post("/p", content='{"name":"b"}', headers={"content-type": "application/json"}), c.post("/p", json="s")]:
                print(r.status_code, r.text)


        if __name__ == "__main__":
            main()
"#,
        r#"422 {"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":[1]}]}
422 {"detail":[{"type":"model_attributes_type","loc":["body","home"],"msg":"Input should be a valid dictionary or object to extract fields from","input":5}]}
422 {"detail":[{"type":"missing","loc":["body","home","city"],"msg":"Field required","input":{}}]}
422 {"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":"x"}]}
422 {"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":"{\"name\":\"b\"}"}]}
200 {"name":"b","home":null}
422 {"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":"s"}]}
"#,
    );
}

/// httpx's AsyncClient over the host's fetch: a GET's status, reason, and
/// JSON, a POST of a JSON body (compact, as httpx writes it) and of text
/// with a header, `raise_for_status()`'s HTTPStatusError with httpx's
/// message, and a request that fails raising ConnectError. httpx 0.28's
/// answers for the same responses (the harness's echo writes its keys
/// sorted).
#[test]
fn httpx_async_client_over_fetch() {
    prints(
        r##"
        import asyncio

        import httpx


        async def main() -> None:
            async with httpx.AsyncClient(base_url="https://api.test") as client:
                r = await client.get("/scores")
                print(r.status_code, r.reason_phrase, r.is_success, r.json())
                r = await client.post("/__echo", json={"q": "é", "n": [1, 2]})
                print(r.json())
                r = await client.post("/__echo", content="hello", headers={"Content-Type": "text/plain"})
                print(r.json())
                r = await client.get("/missing")
                print(r.status_code, r.is_success, r.is_error, r.text)
                try:
                    r.raise_for_status()
                except httpx.HTTPStatusError as e:
                    print(type(e).__name__, e)
                    print(e.response.status_code)
                try:
                    await client.get("https://offline.test/")
                except httpx.RequestError as e:
                    print(type(e).__name__, e)


        if __name__ == "__main__":
            asyncio.run(main())
        "##,
        "200 OK True {'ada': 7, 'bo': 12}\n{'body': '{\"q\":\"é\",\"n\":[1,2]}', 'method': 'POST', 'type': 'application/json'}\n{'body': 'hello', 'method': 'POST', 'type': 'text/plain'}\n404 False True nothing here\nHTTPStatusError Client error '404 Not Found' for url 'https://api.test/missing'\nFor more information check: https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/404\n404\nConnectError Failed to fetch\n",
    );
}

/// The Messages API client: a request with the API's headers and JSON body
/// (the harness's stand-in answers with what it was sent), the response's
/// content blocks, stop reason, and usage read back, a 401 raising
/// AuthenticationError with the SDK's message, and a failed request raising
/// APIConnectionError.
#[test]
fn anthropic_messages_client() {
    prints(
        r##"
        import asyncio

        import anthropic


        async def main() -> None:
            client = anthropic.AsyncAnthropic(api_key="test-key", base_url="https://api.test")
            message = await client.messages.create(
                model="claude-opus-5-5",
                max_tokens=1024,
                system="Be brief.",
                messages=[{"role": "user", "content": "Hello there"}],
            )
            for block in message.content:
                if block.type == "text":
                    print(block.text)
            print([b.type for b in message.content])
            print(message.stop_reason, message.usage.input_tokens, message.usage.output_tokens, message.model)
            bad = anthropic.AsyncAnthropic(api_key="wrong", base_url="https://api.test")
            try:
                await bad.messages.create(model="claude-opus-5-5", max_tokens=16, messages=[{"role": "user", "content": "x"}])
            except anthropic.APIStatusError as e:
                print(type(e).__name__, e.status_code, e.type)
                print(e)
            try:
                await anthropic.AsyncAnthropic(api_key="k", base_url="https://offline.test").messages.create(model="m", max_tokens=1, messages=[])
            except anthropic.APIConnectionError as e:
                print(type(e).__name__, e)


        if __name__ == "__main__":
            asyncio.run(main())
        "##,
        "heard: Hello there (system: Be brief., max_tokens: 1024)\n['thinking', 'text']\nend_turn 11 7 claude-opus-5-5\nAuthenticationError 401 authentication_error\nError code: 401 - {'error': {'message': 'invalid x-api-key', 'type': 'authentication_error'}, 'type': 'error'}\nAPIConnectionError Connection error.\n",
    );
}
