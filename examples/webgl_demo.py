"""WebGL from Python, as Pyodide draws with it: compile and link shaders,
upload vertex and index data through typed arrays, draw a square of two
triangles with a color per corner and a tint in a uniform, and read pixels
back to check what was drawn. The browser gate runs it against Pyodide in
Chromium."""

from js import Float32Array, Uint8Array, Uint16Array, document
from pyodide.ffi import JsProxy, to_js

SIZE = 64

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
uniform float tint;
void main() {
    gl_FragColor = vec4(shade * tint, 1.0);
}
"""

# x, y, then r, g, b for each corner of a square half the canvas wide.
CORNERS = [
    -0.5, -0.5, 1.0, 0.0, 0.0,
    0.5, -0.5, 0.0, 1.0, 0.0,
    0.5, 0.5, 0.0, 0.0, 1.0,
    -0.5, 0.5, 1.0, 1.0, 1.0,
]
TRIANGLES = [0, 1, 2, 0, 2, 3]


def compile_shader(gl: JsProxy, kind: int, source: str) -> JsProxy:
    shader = gl.createShader(kind)
    gl.shaderSource(shader, source)
    gl.compileShader(shader)
    ok: bool = gl.getShaderParameter(shader, gl.COMPILE_STATUS)
    if not ok:
        log: str = gl.getShaderInfoLog(shader)
        raise RuntimeError(log)
    return shader


def link(gl: JsProxy) -> JsProxy:
    program = gl.createProgram()
    gl.attachShader(program, compile_shader(gl, gl.VERTEX_SHADER, VERTEX))
    gl.attachShader(program, compile_shader(gl, gl.FRAGMENT_SHADER, FRAGMENT))
    gl.linkProgram(program)
    ok: bool = gl.getProgramParameter(program, gl.LINK_STATUS)
    print("linked", ok)
    return program


def attribute(gl: JsProxy, program: JsProxy, name: str, size: int, offset: int) -> None:
    where: int = gl.getAttribLocation(program, name)
    gl.enableVertexAttribArray(where)
    gl.vertexAttribPointer(where, size, gl.FLOAT, False, 5 * 4, offset * 4)


def pixel(gl: JsProxy, x: int, y: int) -> list[int]:
    out = Uint8Array.new(4)
    gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, out)
    return list(out.to_bytes())


def main() -> None:
    canvas = document.createElement("canvas")
    canvas.width = SIZE
    canvas.height = SIZE
    gl = canvas.getContext("webgl", preserveDrawingBuffer=True)
    print("drawing buffer", gl.drawingBufferWidth, gl.drawingBufferHeight)
    program = link(gl)
    gl.useProgram(program)

    vertices = gl.createBuffer()
    gl.bindBuffer(gl.ARRAY_BUFFER, vertices)
    gl.bufferData(gl.ARRAY_BUFFER, Float32Array.new(to_js(CORNERS)), gl.STATIC_DRAW)
    indices = gl.createBuffer()
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, indices)
    gl.bufferData(gl.ELEMENT_ARRAY_BUFFER, Uint16Array.new(to_js(TRIANGLES)), gl.STATIC_DRAW)
    vertex_bytes: int = gl.getBufferParameter(gl.ARRAY_BUFFER, gl.BUFFER_SIZE)
    index_bytes: int = gl.getBufferParameter(gl.ELEMENT_ARRAY_BUFFER, gl.BUFFER_SIZE)
    print("uploaded", vertex_bytes, "bytes of corners,", index_bytes, "of indices")
    attribute(gl, program, "position", 2, 0)
    attribute(gl, program, "color", 3, 2)

    gl.viewport(0, 0, SIZE, SIZE)
    gl.clearColor(0.0, 0.0, 0.2, 1.0)
    gl.clear(gl.COLOR_BUFFER_BIT)
    print("cleared", pixel(gl, 1, 1))
    gl.uniform1f(gl.getUniformLocation(program, "tint"), 1.0)
    gl.drawElements(gl.TRIANGLES, len(TRIANGLES), gl.UNSIGNED_SHORT, 0)
    print("no errors", gl.getError() == gl.NO_ERROR)
    # Near each corner of the square, its own color; outside it, the clear.
    print("bottom left", pixel(gl, 17, 17), "bottom right", pixel(gl, 46, 17))
    print("top right", pixel(gl, 46, 46), "top left", pixel(gl, 17, 46))
    print("outside", pixel(gl, 4, 60))

    gl.uniform1f(gl.getUniformLocation(program, "tint"), 0.0)
    gl.drawElements(gl.TRIANGLES, len(TRIANGLES), gl.UNSIGNED_SHORT, 0)
    print("tinted to black", pixel(gl, 32, 32))


if __name__ == "__main__":
    main()
