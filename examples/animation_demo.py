"""An animation loop from Python, as Pyodide runs one: requestAnimationFrame
with a Python callback that moves a box across a canvas a step per frame,
the frame times the browser passes it, cancelAnimationFrame, and asyncio
waiting until the loop is done. The browser gate runs it against Pyodide in
Chromium."""

import asyncio

from js import cancelAnimationFrame, document, performance, requestAnimationFrame
from pyodide.ffi import create_once_callable, create_proxy

STEPS = 6
WIDTH = 10

canvas = document.createElement("canvas")
ctx = canvas.getContext("2d")
times: list[float] = []
xs: list[int] = []
done = asyncio.Event()


def frame(t) -> None:
    """Draw the box one step on, and ask for the next frame until done."""
    times.append(float(t))
    x = len(xs) * WIDTH
    ctx.clearRect(0, 0, canvas.width, canvas.height)
    ctx.fillRect(x, 0, WIDTH, WIDTH)
    xs.append(x)
    if len(xs) < STEPS:
        requestAnimationFrame(ticker)
    else:
        done.set()


ticker = create_proxy(frame)


def cancelled(t) -> None:
    print("a cancelled frame ran")


def filled(x: int) -> bool:
    alpha: int = ctx.getImageData(x, 5, 1, 1).data[3]
    return alpha == 255


async def main() -> None:
    canvas.width = STEPS * WIDTH
    canvas.height = WIDTH
    start = float(performance.now())
    requestAnimationFrame(ticker)
    pending = requestAnimationFrame(create_once_callable(cancelled))
    cancelAnimationFrame(pending)
    await done.wait()
    ticker.destroy()
    print("frames", len(times), "at", xs)
    gaps: list[float] = [times[k + 1] - times[k] for k in range(len(times) - 1)]
    print("each frame later than the last", all(g > 0 for g in gaps))
    print("frames after the start", times[0] > start - 1000)
    print("box at the end", filled(STEPS * WIDTH - 5), "and not at the start", filled(5))
    await asyncio.sleep(0.05)
    print("done")


if __name__ == "__main__":
    asyncio.run(main())
