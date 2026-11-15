---
title: Graphics and the canvas
eyebrow: Running
description: "Drawing from Python on a canvas through its 2D context, as Pyodide does."
---

Graphics go through the [JavaScript bridge](/docs/javascript/): a canvas, its context, and what they answer are JavaScript values, with Pyodide's names and conversions, so drawing code written for Pyodide or PyScript compiles where it can. A program that draws runs in a page through the [loader](/docs/javascript/#the-loader).

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
