"""Image processing from Python on a canvas's pixels, as Pyodide does it:
read an ImageData's bytes in one copy with to_bytes(), compute new pixels in
Python (an inverted copy, then a grayscale one), write them back in one copy
with assign() and putImageData(), and set single pixels by item. The
browser gate runs it against Pyodide in Chromium."""

from js import Float32Array, ImageData, Uint8ClampedArray, document
from pyodide.ffi import JsProxy, to_js

W = 4
H = 2


def checker(ctx: JsProxy) -> None:
    """Orange and blue squares, a pixel each."""
    for y in range(H):
        for x in range(W):
            ctx.fillStyle = "#ff8000" if (x + y) % 2 == 0 else "#0040c0"
            ctx.fillRect(x, y, 1, 1)


def invert(src: bytes) -> bytes:
    out: list[int] = []
    for k in range(0, len(src), 4):
        out += [255 - src[k], 255 - src[k + 1], 255 - src[k + 2], src[k + 3]]
    return bytes(out)


def grayscale(src: bytes) -> bytes:
    """Each pixel's luma, by ITU-R 601's weights in whole numbers."""
    out: list[int] = []
    for k in range(0, len(src), 4):
        g = (src[k] * 299 + src[k + 1] * 587 + src[k + 2] * 114) // 1000
        out += [g, g, g, src[k + 3]]
    return bytes(out)


def first(ctx: JsProxy, n: int) -> list[int]:
    data = ctx.getImageData(0, 0, W, H).data.to_bytes()
    return [data[k] for k in range(n)]


def main() -> None:
    canvas = document.createElement("canvas")
    canvas.width = W
    canvas.height = H
    ctx = canvas.getContext("2d")
    checker(ctx)
    pixels = ctx.getImageData(0, 0, W, H).data.to_bytes()
    print("read", len(pixels), "bytes:", [pixels[k] for k in range(8)])

    inverted = ctx.createImageData(W, H)
    inverted.data.assign(invert(pixels))
    ctx.putImageData(inverted, 0, 0)
    print("inverted:", first(ctx, 8))

    gray = ImageData.new(W, H)
    gray.data.assign(grayscale(ctx.getImageData(0, 0, W, H).data.to_bytes()))
    ctx.putImageData(gray, 0, 0)
    print("grayscale:", first(ctx, 8))

    one = ctx.getImageData(0, 0, 1, 1)
    one.data[0] = 300
    one.data[1] += 10
    one.data[2] = -4
    one.data[3] = 127.5
    ctx.putImageData(one, 2, 1)
    print("one pixel:", one.data.to_bytes()[0], one.data[1], one.data[2], one.data[3])

    copy = Uint8ClampedArray.new(to_js(bytes([1, 2, 3, 4])))
    print("from bytes:", len(copy), copy[3])
    try:
        gray.data.assign(bytes([1, 2, 3]))
    except ValueError as e:
        print("ValueError:", e)
    try:
        Float32Array.new(1).assign(bytes([0, 0, 128, 63]))
    except ValueError as e:
        print("ValueError:", e)


if __name__ == "__main__":
    main()
