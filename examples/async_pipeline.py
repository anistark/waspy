"""A small asyncio pipeline: producers and consumers sharing work through
tasks, with a timeout and a cancellation, the order CPython's event loop
gives each step."""

import asyncio
from typing import List


async def fetch(item: int) -> int:
    # A pretend download: larger items take longer.
    await asyncio.sleep(0.01 * (item % 4))
    return item * item


async def worker(name: str, items: List[int]) -> int:
    total = 0
    for item in items:
        value = await fetch(item)
        print(f"{name} fetched {item} -> {value}")
        total += value
    return total


async def stalled() -> int:
    try:
        await asyncio.sleep(10)
        return 0
    finally:
        print("stalled worker cleaned up")


async def heartbeat(beats: int) -> None:
    for beat in range(beats):
        print("beat", beat)
        await asyncio.sleep(0)


async def main() -> int:
    beat = asyncio.create_task(heartbeat(3))
    totals = await asyncio.gather(
        worker("a", [1, 2, 3]),
        worker("b", [4, 5]),
        worker("c", [6]),
    )
    print("totals", totals)
    await beat
    try:
        await asyncio.wait_for(stalled(), timeout=0.05)
    except TimeoutError:
        print("gave up on the stalled worker")
    first, rest = await asyncio.gather(fetch(7), worker("d", [8, 9]))
    print("first", first, "rest", rest)
    return sum(totals) + first + rest


if __name__ == "__main__":
    print("grand total", asyncio.run(main()))
