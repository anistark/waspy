"""JavaScript promises awaited inside `asyncio.run()`: the program suspends
until each one settles, as Pyodide's does through JavaScript Promise
Integration, so timers and promises interleave in the order they settle,
and a rejected promise raises `JsException`."""

import asyncio

import js
from pyodide.ffi import JsException

# A JavaScript function answering a promise that resolves to `v` after `ms`
# milliseconds.
DELAY = "(ms, v) => new Promise((resolve) => setTimeout(() => resolve(v), ms))"


async def after(ms: int, value: str) -> str:
    later = js.eval(DELAY)
    text: str = await later(ms, value)
    print("settled", text)
    return text


async def ticker() -> int:
    ticks = 0
    for _ in range(3):
        await asyncio.sleep(0.05)
        ticks += 1
        print("tick", ticks)
    return ticks


async def main() -> None:
    n: int = await js.Promise.resolve(41)
    print("resolved", n + 1)
    slow, fast = await asyncio.gather(after(150, "slow"), after(10, "fast"))
    print("gathered", slow, fast)
    word, ticks = await asyncio.gather(after(75, "midway"), ticker())
    print("both", word, ticks)
    try:
        await js.Promise.reject(js.Error.new("nope"))
    except JsException as e:
        print("rejected", e)


if __name__ == "__main__":
    asyncio.run(main())
