---
title: Third-party packages
eyebrow: Language
description: "Why packages from PyPI are not installed, and the packages whose shape a compiled program can use: pydantic, FastAPI, httpx, and an Anthropic API client."
---

Waspy compiles a program's source to WebAssembly. There is no Python interpreter in the module, so a package cannot be installed with `pip` and imported as it is: most packages are large, many are partly written in C or Rust, and many lean on what only an interpreter has (classes made and inspected while the program runs, `eval`, monkey-patching). Compiling them would mean compiling all of that.

What the compiler has instead is a short list of packages written again for it, in the shape their own code expects, so a program that uses them compiles unchanged. Each is written in the Python subset and compiled with the program, as the [standard library](/docs/python-subset/) is, and what a package does with the program's own classes and functions (a model's validation, an application's routes) is written out when the program is compiled. Each answers as the real package does for what it covers, and refuses what it does not, by name.

| Package | What is covered | Where it runs |
|---|---|---|
| `pydantic` | `BaseModel` with typed fields, lax validation, dumps | Everywhere |
| `fastapi` | Routes on the module's functions, `HTTPException`, `TestClient` | Everywhere |
| `httpx` | `AsyncClient` | A program run in JavaScript |
| `anthropic` | `AsyncAnthropic().messages.create(...)` | A program run in JavaScript |

Any other package is an import of a module the program does not have, which is refused when compiling.

## pydantic

A model is a class deriving from `BaseModel` whose fields are annotated, as pydantic 2.14 reads them:

```python
from typing import Optional

from pydantic import BaseModel, ValidationError


class Address(BaseModel):
    city: str
    zip: str = "00000"


class User(BaseModel):
    name: str
    age: int
    tags: list[str] = []
    address: Optional[Address] = None


user = User.model_validate({"name": "bo", "age": "41", "address": {"city": "Oslo"}})
print(user)                    # name='bo' age=41 tags=[] address=Address(city='Oslo', zip='00000')
print(user.model_dump_json())  # {"name":"bo","age":41,"tags":[],"address":{"city":"Oslo","zip":"00000"}}

try:
    User.model_validate_json('{"name": 5}')
except ValidationError as e:
    print(e)
```

What a model has:

- Fields of `str`, `int`, `float`, `bool`, `Any`, another model, `list[...]` and `dict[str, ...]` of those, and `Optional[...]` of a scalar or a model, with defaults written out
- The constructor, by keyword, and `repr()`, `str()`, and `==` as pydantic's
- `model_validate(obj)` and `model_validate_json(text)`, with pydantic's lax conversions (`"41"` into an int, `"yes"` into a bool, `2` into a float) and its errors: `ValidationError` lists every failure with its location, message, type, and input, word for word as pydantic 2.14 prints it, and `error_count()`
- `model_dump()` (a dict of dynamic values) and `model_dump_json()` (compact JSON)
- A model deriving from another model, and methods of its own

Not supported, and refused by name: `Field()`, validators and computed fields (`@field_validator`, `@model_validator`, `@computed_field`), `model_config`, a model's own `__init__`, a collection held in an `Optional` (give it an empty default instead), and nested collections (`list[list[int]]`). `ValidationError.errors()` is not there. The constructor takes values of the fields' types: pydantic would turn `User(name="a", age=1.0)` into an int, which is refused here, as a float where an int is declared is everywhere.

## FastAPI

Routes are declared on the module's functions with `@app.get`, `@app.post`, `@app.put`, `@app.delete`, and `@app.patch`, and a handler's parameters are read from the request by their annotations, as FastAPI 0.163 reads them:

```python
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
from pydantic import BaseModel

app = FastAPI()


class Note(BaseModel):
    text: str


notes: dict[int, Note] = {}


@app.post("/notes/{note_id}", status_code=201)
def add(note_id: int, note: Note) -> Note:
    notes[note_id] = note
    return note


@app.get("/notes/{note_id}")
def read(note_id: int, upper: bool = False):
    if note_id not in notes:
        raise HTTPException(status_code=404, detail="no such note")
    text = notes[note_id].text
    return {"id": note_id, "text": text.upper() if upper else text}


client = TestClient(app)
print(client.post("/notes/1", json={"text": "hi"}).text)  # {"text":"hi"}
print(client.get("/notes/1?upper=true").json())           # {'id': 1, 'text': 'HI'}
print(client.get("/notes/x").status_code)                 # 422
```

- A parameter named in the path (`{note_id}`) is a path parameter, one annotated with a model is the JSON body, and any other is a query parameter, required unless it has a default. Each is converted as pydantic converts it, and a value that does not convert answers FastAPI's 422 body
- A handler answers a dict, a list, a str, a number, or a model (when its return annotation or `response_model=` names the model, or `list[...]` of one), written as FastAPI writes JSON; `status_code=` sets the status
- `HTTPException(status_code, detail)` answers `{"detail": ...}`; a path no route matches answers 404, and a path matched with another method 405, with FastAPI's bodies
- An `async def` handler is run with `asyncio.run()`
- `TestClient(app)` sends requests: `get`, `post`, `put`, `patch`, and `delete`, with `params=`, `json=`, `content=`, and `headers=`, answering a response with `status_code`, `text`, `json()`, and `headers`

A host serves the application through `app.handle(method, url, body)`, which answers the same response; it is this compiler's entry point for a host, not FastAPI's. Not supported: `APIRouter`, `Depends`, `Request`, background tasks, websockets, more than one application with routes, path converters (`{p:path}`), and a handler parameter of another type than a scalar, an `Optional` scalar, or a model.

## httpx

`httpx.AsyncClient` sends requests through the host's `fetch`, so it works in a program run in JavaScript ([JavaScript and async](/docs/javascript/)), not under WASI, which has no network:

```python
import asyncio

import httpx


async def main() -> None:
    async with httpx.AsyncClient(base_url="https://api.example.com") as client:
        r = await client.get("/scores", params={"limit": 10})
        r.raise_for_status()
        print(r.status_code, r.json())


asyncio.run(main())
```

`get`, `post`, `put`, `patch`, `delete`, and `request` take `params=`, `headers=`, and a `json=` or `content=` body. A response has `status_code`, `reason_phrase`, `text`, `json()`, `headers`, `url`, `is_success`, `is_error`, and `raise_for_status()`, which raises `HTTPStatusError` with httpx 0.28's message; a request that cannot be sent raises `ConnectError`. There is no synchronous client (`httpx.get`, `httpx.Client`).

## The Anthropic API

`anthropic.AsyncAnthropic` is a client for Anthropic's Messages API in the shape of the official SDK's async client, written over the `httpx` client above, so it runs where that does:

```python
import asyncio

import anthropic


async def main() -> None:
    client = anthropic.AsyncAnthropic()  # the key from ANTHROPIC_API_KEY
    message = await client.messages.create(
        model="claude-opus-5-5",
        max_tokens=1024,
        system="Answer in one sentence.",
        messages=[{"role": "user", "content": "What is WebAssembly?"}],
    )
    for block in message.content:
        if block.type == "text":
            print(block.text)
    print(message.stop_reason, message.usage.output_tokens)


asyncio.run(main())
```

- `messages.create()` sends `POST /v1/messages` with the `x-api-key` and `anthropic-version` headers, and takes `model`, `max_tokens`, `messages`, `system`, `tools`, `tool_choice`, `thinking`, `output_config`, `stop_sequences`, and `metadata`, sent as given
- The answer is a `Message` with `id`, `model`, `role`, `stop_reason`, `usage` (`input_tokens`, `output_tokens`), and `content`, a list of blocks whose `type` says which fields they carry: `text`, `thinking`, or a tool call's `id`, `name`, and `input`
- An error status raises the SDK's exception for it (`BadRequestError`, `AuthenticationError`, `PermissionDeniedError`, `NotFoundError`, `RateLimitError`, `InternalServerError`, all `APIStatusError`s with `status_code`, `type`, and `body`), with the SDK's message; a request that cannot be sent raises `APIConnectionError`
- `api_key=` and `base_url=` default to `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL`

There is no synchronous client and no streaming yet. A `messages` list built up in a variable is annotated `list[Any]` (a list of dicts of strs held in a variable cannot be passed where a dynamic value is, see [the Python subset](/docs/python-subset/)); written out in the call, it needs nothing.
