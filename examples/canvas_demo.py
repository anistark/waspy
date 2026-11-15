"""Drawing on a canvas from Python through its 2D context, as Pyodide does:
filled and stroked shapes, a path, a gradient, transforms inside save() and
restore(), and pixels read back with getImageData() to check what was drawn.
The browser gate runs it against Pyodide in Chromium."""

import math

from js import document
from pyodide.ffi import JsProxy

SCORES = [("ada", 7), ("grace", 12), ("alan", 9)]
COLORS = ["#e4572e", "#29335c", "#f3a712"]


def pixel(ctx: JsProxy, x: int, y: int) -> str:
    """The color at (x, y), as #rrggbb, with its alpha when not opaque."""
    data = ctx.getImageData(x, y, 1, 1).data
    r: int = data[0]
    g: int = data[1]
    b: int = data[2]
    a: int = data[3]
    text = f"#{r:02x}{g:02x}{b:02x}"
    return text if a == 255 else f"{text}/{a}"


def bars(ctx: JsProxy) -> None:
    """A bar per score, 10 pixels a point, on a white background."""
    ctx.fillStyle = "white"
    ctx.fillRect(0, 0, 160, 140)
    for i in range(len(SCORES)):
        name, score = SCORES[i]
        ctx.fillStyle = COLORS[i]
        ctx.fillRect(10 + i * 50, 130 - score * 10, 40, score * 10)
        print(name, pixel(ctx, 30 + i * 50, 125), ctx.fillStyle)
    print("above the bars", pixel(ctx, 30, 20))


def shapes(ctx: JsProxy) -> None:
    """A stroked frame, a filled circle, and a triangle path."""
    ctx.strokeStyle = "rgb(0, 128, 0)"
    ctx.lineWidth = 4
    ctx.strokeRect(2, 2, 156, 136)
    print("frame", pixel(ctx, 2, 70), ctx.strokeStyle, ctx.lineWidth)
    ctx.beginPath()
    ctx.arc(80, 40, 15, 0, 2 * math.pi)
    ctx.fillStyle = "rgba(0, 0, 255, 0.5)"
    ctx.fill()
    print("circle", pixel(ctx, 80, 40), ctx.fillStyle)
    print("inside", ctx.isPointInPath(80, 40), ctx.isPointInPath(80, 60))
    ctx.beginPath()
    ctx.moveTo(130, 10)
    ctx.lineTo(150, 40)
    ctx.lineTo(110, 40)
    ctx.closePath()
    ctx.fillStyle = "#000"
    ctx.fill()
    print("triangle", pixel(ctx, 130, 30), pixel(ctx, 112, 12))


def transformed(ctx: JsProxy) -> None:
    """translate() and scale() inside save() and restore()."""
    ctx.save()
    ctx.translate(100, 100)
    ctx.scale(2, 2)
    ctx.fillStyle = "#123456"
    ctx.fillRect(0, 0, 5, 5)
    t = ctx.getTransform()
    print("transform", t.a, t.d, t.e, t.f)
    ctx.restore()
    t = ctx.getTransform()
    print("restored", t.a, t.e, ctx.fillStyle)
    print("scaled square", pixel(ctx, 109, 109), pixel(ctx, 111, 111))


def gradient(ctx: JsProxy) -> None:
    """A horizontal gradient from black to white across the canvas."""
    g = ctx.createLinearGradient(0, 0, 160, 0)
    g.addColorStop(0, "black")
    g.addColorStop(1, "white")
    ctx.fillStyle = g
    ctx.fillRect(0, 0, 160, 10)
    row = ctx.getImageData(0, 5, 160, 1).data
    first: int = row[0]
    middle: int = row[80 * 4]
    last: int = row[159 * 4]
    print("gradient", first, middle, last, first < middle < last)


def main() -> None:
    canvas = document.createElement("canvas")
    canvas.width = 160
    canvas.height = 140
    document.body.appendChild(canvas)
    ctx = canvas.getContext("2d")
    print(canvas.width, canvas.height, ctx.canvas == canvas)
    bars(ctx)
    shapes(ctx)
    transformed(ctx)
    gradient(ctx)
    canvas.width = 160
    print("cleared", pixel(ctx, 30, 125))
    canvas.remove()


if __name__ == "__main__":
    main()
