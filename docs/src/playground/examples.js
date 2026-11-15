export const EXAMPLES = [
    {
        id: 'fibonacci',
        name: 'Fibonacci & primes',
        code: `def fib(n: int) -> int:
    """Recursive Fibonacci."""
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


def fib_iter(n: int) -> int:
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a


def is_prime(n: int) -> bool:
    if n < 2:
        return False
    i = 2
    while i * i <= n:
        if n % i == 0:
            return False
        i += 1
    return True
`,
    },
    {
        id: 'strings',
        name: 'Strings',
        code: `from typing import List


def normalize(word: str) -> str:
    return word.strip(".,!?").lower()


def words(text: str) -> List[str]:
    out: List[str] = []
    for raw in text.split():
        out.append(normalize(raw))
    return out


def slug() -> str:
    return "-".join(words("  Python, to WebAssembly!  "))


def count_vowels() -> int:
    total = 0
    for ch in "WebAssembly from Python".lower():
        if ch in "aeiou":
            total += 1
    return total


def badge(n: int) -> str:
    label = "items"
    if n == 1:
        label = "item"
    return f"{n} {label}".upper().center(12)
`,
    },
    {
        id: 'classes',
        name: 'Classes & inheritance',
        code: `class Shape:
    def area(self) -> float:
        return 0.0

    def name(self) -> str:
        return "shape"

    def describe(self) -> str:
        return f"{self.name()} with area {self.area():.2f}"


class Circle(Shape):
    def __init__(self, r: float):
        self.r = r

    def area(self) -> float:
        return 3.14159 * self.r * self.r

    def name(self) -> str:
        return "circle"


class Square(Shape):
    def __init__(self, side: float):
        self.side = side

    def area(self) -> float:
        return self.side * self.side

    def name(self) -> str:
        return "square"


def describe_circle(r: float) -> str:
    return Circle(r).describe()


def describe_square(side: float) -> str:
    return Square(side).describe()
`,
    },
    {
        id: 'collections',
        name: 'Collections',
        code: `from typing import Dict, List


def squares(n: int) -> int:
    return sum([i * i for i in range(1, n + 1)])


def word_count() -> int:
    counts: Dict[str, int] = {}
    for w in "the quick fox and the lazy dog and the cat".split():
        counts[w] = counts.get(w, 0) + 1
    return counts["the"]


def median(a: int, b: int, c: int) -> int:
    xs: List[int] = sorted([a, b, c])
    return xs[1]


def unique_count() -> int:
    return len({x % 7 for x in range(100)})
`,
    },
    {
        id: 'exceptions',
        name: 'Exceptions',
        code: `class InsufficientFunds(Exception):
    pass


class Account:
    def __init__(self, balance: int):
        self.balance = balance

    def withdraw(self, amount: int) -> int:
        if amount > self.balance:
            raise InsufficientFunds("not enough money")
        self.balance -= amount
        return self.balance


def try_withdraw(amount: int) -> str:
    acct = Account(100)
    try:
        left = acct.withdraw(amount)
        return f"ok, {left} left"
    except InsufficientFunds:
        return "declined"


def parse_or_default(fallback: int) -> int:
    try:
        return int("42x")
    except ValueError:
        return fallback
`,
    },
    {
        id: 'generators',
        name: 'Generators',
        code: `def countdown(n: int):
    while n > 0:
        yield n
        n -= 1


def evens(limit: int):
    for i in range(limit):
        if i % 2 == 0:
            yield i


def total_countdown(n: int) -> int:
    total = 0
    for x in countdown(n):
        total += x
    return total


def sum_evens(limit: int) -> int:
    total = 0
    for x in evens(limit):
        total += x
    return total
`,
    },
    {
        id: 'canvas',
        name: 'Canvas: a bar chart',
        // Imports `js`: runs with the page through the loader, and is
        // checked against Pyodide by the browser gate.
        web: true,
        code: `import math

from js import document
from pyodide.ffi import JsProxy

SCORES = [("ada", 7), ("grace", 12), ("alan", 9), ("edsger", 5)]
COLORS = ["#e4572e", "#29335c", "#f3a712", "#669bbc"]


def stage() -> JsProxy:
    """The playground's stage, or the page's body anywhere else."""
    host = document.getElementById("stage")
    return host if host else document.body


def main() -> None:
    canvas = document.createElement("canvas")
    canvas.width = 320
    canvas.height = 160
    stage().appendChild(canvas)
    ctx = canvas.getContext("2d")
    ctx.fillStyle = "#fdf6e3"
    ctx.fillRect(0, 0, 320, 160)
    ctx.font = "12px sans-serif"
    for i in range(len(SCORES)):
        name, score = SCORES[i]
        x = 20 + i * 75
        ctx.fillStyle = COLORS[i]
        ctx.fillRect(x, 140 - score * 10, 50, score * 10)
        ctx.fillText(name, x, 155)
    ctx.beginPath()
    ctx.arc(290, 30, 18, 0, 2 * math.pi)
    ctx.fillStyle = "rgba(220, 50, 47, 0.8)"
    ctx.fill()
    px = ctx.getImageData(45, 130, 1, 1).data
    r: int = px[0]
    g: int = px[1]
    b: int = px[2]
    print(f"drew {len(SCORES)} bars; the first is #{r:02x}{g:02x}{b:02x}")
    print("the circle's style:", ctx.fillStyle)


if __name__ == "__main__":
    main()
`,
    },
    {
        id: 'animation',
        name: 'Animation frames',
        // Imports `js`: runs with the page through the loader, and is
        // checked against Pyodide by the browser gate.
        web: true,
        code: `import asyncio

from js import document, requestAnimationFrame
from pyodide.ffi import JsProxy, create_proxy

FRAMES = 90
SIZE = 16
WIDTH = 320
HEIGHT = 80


def stage() -> JsProxy:
    """The playground's stage, or the page's body anywhere else."""
    host = document.getElementById("stage")
    return host if host else document.body


canvas = document.createElement("canvas")
ctx = canvas.getContext("2d")
times: list[float] = []
bounces: list[int] = []
done = asyncio.Event()


def frame(t) -> None:
    """Move the ball one step, bouncing off the walls, and ask for more."""
    times.append(float(t))
    n = len(times)
    span = WIDTH - SIZE
    lap = (n * 4) % (2 * span)
    x = lap if lap <= span else 2 * span - lap
    if lap == span or lap == 0:
        bounces.append(n)
    ctx.fillStyle = "#fdf6e3"
    ctx.fillRect(0, 0, WIDTH, HEIGHT)
    ctx.fillStyle = "#cb4b16"
    ctx.fillRect(x, 32, SIZE, SIZE)
    if n < FRAMES:
        requestAnimationFrame(ticker)
    else:
        done.set()


ticker = create_proxy(frame)


async def main() -> None:
    canvas.width = WIDTH
    canvas.height = HEIGHT
    stage().appendChild(canvas)
    requestAnimationFrame(ticker)
    await done.wait()
    ticker.destroy()
    steady = all(times[k] < times[k + 1] for k in range(len(times) - 1))
    print(f"{len(times)} frames, each later than the last: {steady}")
    print("bounced at frames", bounces)


if __name__ == "__main__":
    asyncio.run(main())
`,
    },
    {
        id: 'pixels',
        name: 'Image pixels',
        // Imports `js`: runs with the page through the loader, and is
        // checked against Pyodide by the browser gate.
        web: true,
        code: `from js import document
from pyodide.ffi import JsProxy

W = 160
H = 80


def stage() -> JsProxy:
    """The playground's stage, or the page's body anywhere else."""
    host = document.getElementById("stage")
    return host if host else document.body


def grayscale_right_half(src: bytes) -> bytes:
    """Each pixel of the right half as its luma, by ITU-R 601's weights."""
    out: list[int] = []
    for k in range(0, len(src), 4):
        r, g, b, a = src[k], src[k + 1], src[k + 2], src[k + 3]
        if (k // 4) % W >= W // 2:
            y = (r * 299 + g * 587 + b * 114) // 1000
            r, g, b = y, y, y
        out += [r, g, b, a]
    return bytes(out)


def main() -> None:
    canvas = document.createElement("canvas")
    canvas.width = W
    canvas.height = H
    stage().appendChild(canvas)
    ctx = canvas.getContext("2d")
    gradient = ctx.createLinearGradient(0, 0, W, H)
    gradient.addColorStop(0, "#d33682")
    gradient.addColorStop(1, "#2aa198")
    ctx.fillStyle = gradient
    ctx.fillRect(0, 0, W, H)
    ctx.fillStyle = "#b58900"
    ctx.fillRect(30, 20, 100, 40)
    image = ctx.getImageData(0, 0, W, H)
    pixels = image.data.to_bytes()
    image.data.assign(grayscale_right_half(pixels))
    ctx.putImageData(image, 0, 0)
    after = ctx.getImageData(0, 0, W, H).data.to_bytes()
    left = (40 * W + 40) * 4
    right = (40 * W + 120) * 4
    print("read", len(pixels), "bytes in one copy")
    print("left half kept:", [after[left + k] for k in range(3)])
    print("right half gray:", [after[right + k] for k in range(3)])


if __name__ == "__main__":
    main()
`,
    },
    {
        id: 'webgl',
        name: 'WebGL: a triangle',
        // Imports `js`: runs with the page through the loader, and is
        // checked against Pyodide by the browser gate.
        web: true,
        code: `from js import Float32Array, Uint8Array, document
from pyodide.ffi import JsProxy, to_js

VERTEX = """
attribute vec2 position;
attribute vec3 color;
varying vec3 shade;
void main() {
    shade = color;
    gl_Position = vec4(position, 0.0, 1.0);
}
"""

FRAGMENT = """
precision mediump float;
varying vec3 shade;
void main() {
    gl_FragColor = vec4(shade, 1.0);
}
"""

# x, y, r, g, b for each corner of the triangle.
CORNERS = [0.0, 0.8, 1.0, 0.2, 0.2, -0.8, -0.8, 0.2, 1.0, 0.2, 0.8, -0.8, 0.2, 0.4, 1.0]


def stage() -> JsProxy:
    """The playground's stage, or the page's body anywhere else."""
    host = document.getElementById("stage")
    return host if host else document.body


def shader(gl: JsProxy, kind: int, source: str) -> JsProxy:
    s = gl.createShader(kind)
    gl.shaderSource(s, source)
    gl.compileShader(s)
    ok: bool = gl.getShaderParameter(s, gl.COMPILE_STATUS)
    if not ok:
        log: str = gl.getShaderInfoLog(s)
        raise RuntimeError(log)
    return s


def main() -> None:
    canvas = document.createElement("canvas")
    canvas.width = 240
    canvas.height = 160
    stage().appendChild(canvas)
    gl = canvas.getContext("webgl", preserveDrawingBuffer=True)
    program = gl.createProgram()
    gl.attachShader(program, shader(gl, gl.VERTEX_SHADER, VERTEX))
    gl.attachShader(program, shader(gl, gl.FRAGMENT_SHADER, FRAGMENT))
    gl.linkProgram(program)
    gl.useProgram(program)
    gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer())
    gl.bufferData(gl.ARRAY_BUFFER, Float32Array.new(to_js(CORNERS)), gl.STATIC_DRAW)
    for name, size, offset in [("position", 2, 0), ("color", 3, 2)]:
        where: int = gl.getAttribLocation(program, name)
        gl.enableVertexAttribArray(where)
        gl.vertexAttribPointer(where, size, gl.FLOAT, False, 20, offset * 4)
    gl.clearColor(0.99, 0.96, 0.89, 1.0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    gl.drawArrays(gl.TRIANGLES, 0, 3)
    out = Uint8Array.new(4)
    gl.readPixels(120, 80, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out)
    center: list[int] = list(out.to_bytes())
    gl.readPixels(2, 2, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out)
    corner: list[int] = list(out.to_bytes())
    print("a triangle in", gl.drawingBufferWidth, "by", gl.drawingBufferHeight)
    print("the background is cleared:", corner)
    print("the center is drawn:", center != corner, "no errors:", gl.getError() == 0)


if __name__ == "__main__":
    main()
`,
    },
    {
        id: 'refused',
        name: 'A refused construct',
        code: `# Waspy gives Python's answer or refuses to compile.
# It never guesses: eval() has no compiled meaning,
# so this program is rejected with a located error.


def double(x: int) -> int:
    return x * 2


def dynamic(x: int) -> int:
    return eval("x + 1")
`,
    },
];
