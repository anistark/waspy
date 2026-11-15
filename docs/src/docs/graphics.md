---
title: Graphics and the canvas
eyebrow: Running
description: "Drawing from Python on a canvas through its 2D context and WebGL, animation loops through requestAnimationFrame, and image pixels through Pyodide's buffer methods."
---

Graphics go through the [JavaScript bridge](/docs/javascript/): a canvas, its context, and what they answer are JavaScript values, with Pyodide's names and conversions, so drawing code written for Pyodide or PyScript compiles where it can. A program that draws runs in a page through the [loader](/docs/javascript/#the-loader). The [playground](/playground/) runs one too: its Canvas, Animation frames, Image pixels, and WebGL examples draw in the Run tab's stage, and each is checked against Pyodide by the browser gate.

## The 2D canvas

A canvas's 2D context is a JavaScript value like any other: set its styles, fill and stroke shapes, build paths, transform inside `save()` and `restore()`, and read pixels back with `getImageData()`.

```python
import math
from js import document

canvas = document.createElement("canvas")
canvas.width = 160
canvas.height = 100
document.body.appendChild(canvas)
ctx = canvas.getContext("2d")
ctx.fillStyle = "#29335c"
ctx.fillRect(10, 10, 40, 80)
ctx.beginPath()
ctx.arc(100, 50, 20, 0, 2 * math.pi)
ctx.fillStyle = "rgba(0, 0, 255, 0.5)"
ctx.fill()
px = ctx.getImageData(30, 50, 1, 1).data
print(px[0], px[1], px[2], ctx.fillStyle)   # 41 51 92 rgba(0, 0, 255, 0.5)
```

Each property and call crosses the bridge once, and a crossing costs no more than it does in Pyodide, so a drawing loop needs no batching. What the context answers is converted where it is used, as any JavaScript value is: a number read back is an `int` when it is whole, so store one that may be whole where a float is declared through `float(x)`. `examples/canvas_demo.py` (bars, a frame, a circle, a path, transforms, a gradient, and the pixels each leaves) is checked against Pyodide in Chromium by the browser gate.

## Animation frames

`requestAnimationFrame` takes a Python function through `pyodide.ffi.create_proxy` (see [Events](/docs/javascript/#events)), and calls it with the frame's time in milliseconds; a frame that asks for the next keeps the loop going, and `cancelAnimationFrame` drops one not yet run. A program's `asyncio.run()` waits for the loop with an `asyncio.Event` the last frame sets: where the browser has JSPI the event loop yields to the page between frames, and each frame wakes it (see [Promises](/docs/javascript/#promises)).

```python
import asyncio
from js import document, requestAnimationFrame
from pyodide.ffi import create_proxy

canvas = document.createElement("canvas")
ctx = canvas.getContext("2d")
times: list[float] = []
done = asyncio.Event()

def frame(t) -> None:
    times.append(float(t))
    ctx.clearRect(0, 0, 300, 150)
    ctx.fillRect(len(times) * 10, 0, 10, 10)
    if len(times) < 60:
        requestAnimationFrame(ticker)
    else:
        done.set()

ticker = create_proxy(frame)

async def main() -> None:
    requestAnimationFrame(ticker)
    await done.wait()
    ticker.destroy()
    print(f"{len(times)} frames in {times[-1] - times[0]:.0f} ms")

if __name__ == "__main__":
    asyncio.run(main())
```

A frame's time, like `performance.now()`, is a JavaScript number that is sometimes whole, which is an `int` as Pyodide hands it over, so read it with `float(t)` where a float is wanted: storing it where `float` is declared raises `TypeError` when it happens to be whole. `examples/animation_demo.py` is checked against Pyodide in Chromium by the browser gate.

## Image pixels

An `ImageData`'s pixels are a `Uint8ClampedArray`, four bytes a pixel (red, green, blue, alpha). Pyodide's buffer methods move them in one copy each way, straight between JavaScript and the module's memory: `data.to_bytes()` answers them as `bytes`, and `data.assign(b)` writes bytes of the same length back. Process them in Python in between, and put the image back with `putImageData()`.

```python
from js import document

def invert(src: bytes) -> bytes:
    out: list[int] = []
    for k in range(0, len(src), 4):
        out += [255 - src[k], 255 - src[k + 1], 255 - src[k + 2], src[k + 3]]
    return bytes(out)

canvas = document.createElement("canvas")
ctx = canvas.getContext("2d")
ctx.fillStyle = "#ff8000"
ctx.fillRect(0, 0, 300, 150)
img = ctx.getImageData(0, 0, 300, 150)
img.data.assign(invert(img.data.to_bytes()))
ctx.putImageData(img, 0, 0)
print(list(ctx.getImageData(0, 0, 1, 1).data.to_bytes()))   # [0, 127, 255, 255]
```

A single pixel is an item: `img.data[k] = v` stores as the typed array does (a `Uint8ClampedArray` clamps 300 to 255 and rounds 127.5 to 128), and `img.data[k]` reads one back, each a crossing of the bridge, so a whole image goes through `to_bytes()` and `assign()`. `assign()` raises Pyodide's `ValueError` for bytes of another length, or for a typed array that does not hold bytes. `to_js(b)` makes a `Uint8Array` of bytes, which `Uint8ClampedArray.new(...)` or `ImageData.new(...)` take. `assign_to()` and `to_memoryview()` hand over a writable buffer, which needs a `bytearray` or `memoryview`, and are refused. `examples/image_demo.py` (an inverted copy, a grayscale one, single pixels, and the errors) is checked against Pyodide in Chromium by the browser gate.

## WebGL

A canvas's `webgl` context is a JavaScript value too, so a WebGL program reads as it does in Pyodide: compile and link shaders, upload vertex and index data through typed arrays, set uniforms, and draw. A list of numbers becomes a typed array through `to_js()`, as in Pyodide (`Float32Array.new(to_js(corners))`), and pixels come back with `readPixels()` into a `Uint8Array`, whose `to_bytes()` hands them over in one copy.

```python
from js import Float32Array, Uint8Array, document
from pyodide.ffi import to_js

canvas = document.createElement("canvas")
gl = canvas.getContext("webgl", preserveDrawingBuffer=True)
buffer = gl.createBuffer()
gl.bindBuffer(gl.ARRAY_BUFFER, buffer)
gl.bufferData(gl.ARRAY_BUFFER, Float32Array.new(to_js([-1.0, -1.0, 1.0, -1.0, 0.0, 1.0])), gl.STATIC_DRAW)
size: int = gl.getBufferParameter(gl.ARRAY_BUFFER, gl.BUFFER_SIZE)
gl.clearColor(0.2, 0.4, 0.6, 1.0)
gl.clear(gl.COLOR_BUFFER_BIT)
out = Uint8Array.new(4)
gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out)
print(size, list(out.to_bytes()))     # 24 [51, 102, 153, 255]
```

What a query answers is converted where it is stored, so give a status or a size its type (`ok: bool = gl.getShaderParameter(shader, gl.COMPILE_STATUS)`, `where: int = gl.getAttribLocation(program, "position")`). Keyword arguments to `getContext()` are its options object, as Pyodide passes them. `examples/webgl_demo.py` (shaders with a varying and a uniform, a square of two triangles drawn through an index buffer, and the colors read back at its corners) is checked against Pyodide in Chromium by the browser gate.
