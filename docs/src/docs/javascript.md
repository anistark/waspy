---
title: JavaScript and async
eyebrow: Running
description: "Coroutines and asyncio, JavaScript values from Python as Pyodide hands them over, the loader, Promises, and modules loaded by name."
---

## Coroutines and asyncio

`async def` makes a coroutine, and `await` runs one, as CPython does: an `await` of a coroutine runs it inline, and one of a task or a future waits for it to finish. An `await` may stand anywhere CPython evaluates it unconditionally (an argument, an operand, the value of an assignment or a `return`, a loop's or an `if`'s condition), and what CPython evaluates before it in the statement is computed first, so the order is CPython's. Coroutine methods work too.

`asyncio` is CPython 3.13's event loop, futures, and tasks, compiled with the program: `run`, `create_task`, `ensure_future`, `gather` (awaited directly, with `*aws` or a fixed list, its results unpacked when their types differ, and `return_exceptions=True` for coroutines that resolve to None, listing each one's exception or None), `sleep`, `wait_for` (awaited directly, `timeout=None`, zero, and a deadline that cancels what it waits on into `TimeoutError`), a task's `result`, `exception`, `done`, `cancelled`, `cancel`, and `get_name`, and `CancelledError`, `TimeoutError`, and `InvalidStateError`. Which task runs when, and so what each `print` interleaves with, is CPython's: tasks start in the order they are made, a `sleep(0)` lets every other ready task run once, timers fall due in deadline order, and `asyncio.run()` cancels the tasks still pending when its coroutine finishes, running their cleanup before it returns.

`async with asyncio.TaskGroup() as tg:` runs the tasks `tg.create_task(coro)` makes and waits for all of them when the block ends; the first to fail cancels the rest, and the group raises `ExceptionGroup` holding what failed (`eg.exceptions`, `eg.message`, and `str(eg)` as CPython words them), which `except ExceptionGroup as eg:` catches. `except*` is not supported.

`asyncio.timeout(delay)` (or `timeout(None)`, no deadline) is the context manager `wait_for` runs under, for a block: `async with asyncio.timeout(0.5):` cancels the block at the deadline and raises `TimeoutError` out of it, and its `expired()` says whether it did. `Lock`, `Event`, `Semaphore`, and `BoundedSemaphore` are CPython 3.13's, waking their waiters in the same order, with `async with` for a lock or a semaphore. `Queue`, `LifoQueue`, and `PriorityQueue` hold one type of item, named where the queue is made (`q: asyncio.Queue[int] = asyncio.Queue(maxsize=10)`, or a field `self.q: asyncio.Queue[str] = asyncio.Queue()`): `put`, `get`, `join`, `put_nowait`, `get_nowait`, `task_done`, `qsize`, `empty`, `full`, `maxsize`, and `shutdown`, with `QueueEmpty`, `QueueFull`, and `QueueShutDown`.

```python
async def worker(name: str, q: asyncio.Queue[int]) -> None:
    while True:
        n = await q.get()
        print(name, n * n)
        q.task_done()

async def main() -> None:
    q: asyncio.Queue[int] = asyncio.Queue()
    for n in range(4):
        q.put_nowait(n)
    workers = [asyncio.create_task(worker(w, q)) for w in ["a", "b"]]
    await q.join()                  # a 0, a 1, a 4, a 9: "a" never waits
    for w in workers:
        w.cancel()
```

```python
import asyncio

async def fetch(n: int) -> int:
    await asyncio.sleep(0.01 * n)
    return n * n

async def main() -> None:
    print(await asyncio.gather(fetch(3), fetch(1), fetch(2)))   # [9, 1, 4]

if __name__ == "__main__":
    asyncio.run(main())
```

`async with` and `async for` follow CPython's protocols. `async with m as x:` awaits `m.__aenter__()` and binds what it answers, then awaits `m.__aexit__(...)` however the body ends: falling off the end, a `return`, `break`, or `continue`, or an exception, which a true answer from `__aexit__` suppresses. `async for x in it:` calls `it.__aiter__()` once and awaits `__anext__()` each round until it raises `StopAsyncIteration`. An `__aiter__` or `__aenter__` that answers `self` may leave its result unannotated; as with `__exit__`, the exception's class is handed to `__aexit__` as the exception itself, so that parameter may only be compared with None.

```python
class Ticker:
    def __init__(self, n: int) -> None:
        self.i, self.n = 0, n

    def __aiter__(self):
        return self

    async def __anext__(self) -> int:
        if self.i == self.n:
            raise StopAsyncIteration
        self.i += 1
        await asyncio.sleep(0.01)
        return self.i

async def main() -> None:
    async for tick in Ticker(3):
        print(tick)             # 1, 2, 3
```

Under WASI the loop waits for its next timer through `poll_oneoff` and runs to completion; in a page, an exported coroutine runs between JavaScript's turns (below). What is not supported is refused when the program is compiled: asynchronous generators and comprehensions, `asyncio`'s other functions and classes, a coroutine that is called and never awaited, and an `await` CPython might not evaluate or would evaluate repeatedly (the right of `and`/`or`, an arm of a conditional expression, a comprehension). A coroutine that returns a value declares its type (`-> int`).

## JavaScript values

A program that imports `js` (or `pyodide.ffi`) reaches JavaScript through the [loader](#the-loader), with Pyodide's names and conversions, so code written for Pyodide or PyScript compiles where it can:

```python
from js import JSON, Math, Object
from pyodide.ffi import to_js

data = JSON.parse('{"team": "otters", "scores": [7, 12, 9]}')
team: str = data.team              # a JS string, stored as a str
best: int = Math.max(7, 12, 9)     # a JS number, stored as an int
for s in data.scores:              # JavaScript's iteration protocol
    print(s)
scores: list[int] = data.scores.to_py()
print(JSON.stringify(to_js({"best": best})))
```

A JavaScript value is a `JsProxy` whose attributes, calls, and items resolve by name when the program runs: `obj.x`, `obj.x = v`, `obj.f(...)`, `f(...)` of a function held in a variable, `Ctor.new(...)` for `new`, `arr[i]` (negative indexes count from the end), `len()`, and `for x in obj`. As in Pyodide, a number is an `int` when it is a safe integer and a `float` otherwise, a string a `str`, a boolean a `bool`, `undefined` None, and `null` Pyodide's `jsnull`. Types are static here, so a value stays a proxy until it is used, and is converted there: stored where a type is declared, printed or given to `str()`, `repr()`, `int()`, `float()`, or `bool()`, compared with `==` or `is None`, or tested for truth (an empty array or string is false, an object true). Arithmetic on a proxy is refused; store it where its type is declared first.

`.to_py()` converts a whole JS array or plain object (or a Map with string keys) into the list, tuple, or dict the place declares (`xs: list[int] = arr.to_py()`), and `to_js(x)` a list, tuple, or dict into a JS array or object, both through JSON. A Python value passed to JavaScript (an argument, a property) is an int, float, str, bool, None, or a proxy; Pyodide would pass a list or an instance as a proxy of the Python object, which has no counterpart here, so that is refused with a hint to pass `to_js(x)`.

A JavaScript exception is `pyodide.ffi.JsException`, whose text is JavaScript's `String(error)` (`SyntaxError: ...`), and a missing attribute is `AttributeError`, a missing item `IndexError`. Where Pyodide would keep a value whatever the annotation says, a typed place cannot hold another kind, so storing a JS string where an int is declared, or an integer-valued number where a float is, raises `TypeError` (`int(x)`, `float(x)`, and `str(x)` convert explicitly). The values Python lets go of are given back to JavaScript when the collector frees them.

## The loader

`waspy::js_loader()` is the JavaScript side of a compiled module, an ES module that is the same for every program. Write it next to the module (the `advanced_compiler` example's `--js` flag does) and instantiate through it:

```js
import { instantiate } from './waspy-loader.js';

const { exports } = await instantiate(bytes, {
    wasi,        // the WASI host, when the program prints, reads, or opens files
    globals,     // what `js` names (globalThis by default)
    modules,     // modules a program loads by name (below)
});
exports.greet('Ada', 2);            // a str and an int in, a str out
await exports.fetch_report(7);      // an `async def`: a Promise
```

A program with a `__main__` block runs through the `start` the same call answers (`await start()`), which is what lets it suspend on a promise (below).

The loader wraps each export by the signature the module lists (its `waspy.exports` custom section), so it takes and answers JavaScript values: numbers, strings, booleans, undefined for None, arrays and plain objects for lists, tuples, and dicts, and any value for a `JsProxy` parameter. A Python exception escaping one is thrown as a `PythonError` whose message is the last line of CPython's report (`ValueError: negative`). A program that uses the bridge gets the wrappers; for one that does not (a plugin a page calls with strings, say), set `CompilerOptions::js_exports`.

## Promises

An `await` of a JavaScript value (a promise, or any thenable) waits for it to settle, as Pyodide's does: it answers the value, and a rejection raises `JsException`. An exported `async def` answers a Promise: its coroutine runs as a task on the module's event loop, which the loader runs between JavaScript's turns (a timer for `asyncio.sleep`, a promise's settling for an `await`), and the Promise resolves with what the coroutine returns, or rejects with the `PythonError` it raised.

```python
import asyncio
from pyodide.ffi import JsProxy

async def double_later(p: JsProxy) -> int:
    v: int = await p
    await asyncio.sleep(0.01)
    return v * 2
```

```js
await exports.double_later(Promise.resolve(21));   // 42
```

A program's `__main__` block can await promises too. Run it through the loader's `start()`, and where the browser has JavaScript Promise Integration (JSPI: `WebAssembly.Suspending` and `WebAssembly.promising`), a promise awaited inside `asyncio.run()` suspends the program until it settles, as Pyodide's does, so promises and `asyncio` timers interleave in the order they settle:

```python
import asyncio
import js

async def main() -> None:
    later = js.eval("(ms, v) => new Promise((r) => setTimeout(() => r(v), ms))")
    first, second = await asyncio.gather(later(50, "slow"), later(10, "fast"))
    print(first, second)    # slow fast

if __name__ == "__main__":
    asyncio.run(main())
```

```js
const { start } = await instantiate(bytes, { wasi });
await start();
```

Without JSPI (or inside a call JavaScript made into the module, which cannot suspend), such an `await` raises `RuntimeError`; export the coroutine and await it from JavaScript instead.

## Modules loaded by name

`importlib.import_module(name)` with a name known only when the program runs loads a separately compiled module the host holds, through the loader's `modules`, and answers it as a JavaScript value whose methods are its exports:

```python
import importlib

def area_of(plugin: str) -> int:
    m = importlib.import_module(plugin)
    a: int = m.area(3)
    return a
```

```js
const geometry = await instantiate(geometryBytes);   // compiled with js_exports
const app = await instantiate(appBytes, { modules: { geometry: geometry.exports } });
app.exports.area_of('geo' + 'metry');
```

A name the host has no module for raises CPython's `ModuleNotFoundError: No module named 'x'`. A constant name is the import it names and is linked at compile time, as before; `__import__` of a computed name is refused.
