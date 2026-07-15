# User-defined module imports (#41).
#
# Compile this entry file with `just compile examples/user_modules_app/main.py`
# (or `waspy::compile_python_file`): `mathlib` and `shapes` are resolved to the
# sibling .py files and linked into the single output WASM module. A module
# reached through several import paths (mathlib here, imported both directly
# and by shapes) is compiled exactly once.

import mathlib
import shapes
from mathlib import add as plus


def combined() -> int:
    return mathlib.add(2, mathlib.mul(4, 10))


def aliased() -> int:
    return plus(20, 22)


def scaled() -> int:
    return mathlib.FACTOR * 6


def rect_area() -> int:
    r = shapes.Rect(6, 7)
    return r.area()
