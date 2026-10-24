---
title: Numbers
eyebrow: Language
description: "How int and float behave, and the two places waspy deliberately answers differently from CPython."
---

`int` is a 32-bit two's-complement integer, and `float` is an IEEE-754 double.

This is a deliberate narrowing of Python's semantics, and part of the definition of the subset waspy accepts rather than an accident of the backend. CPython's `int` is arbitrary precision: it grows to hold whatever it is given. Here a value that leaves the range -2147483648 to 2147483647 wraps around, so

```python
1000000 * 1000000   # CPython: 1000000000000    waspy: -727379968
2 ** 40             # CPython: 1099511627776    waspy: 0
2147483647 + 1      # CPython: 2147483648       waspy: -2147483648
```

A program whose integer values stay inside that range gets Python's answers. One that leaves it gets two's-complement wraparound, quietly, the way it would in C or Rust's release profile.

The alternative is to check every add, subtract, multiply, and power for overflow and trap. That makes the divergence loud, but it costs instructions on the hottest path in any program and rejects code that deliberately relies on wrapping (hashes, checksums, PRNGs). A 64-bit integer would only move the boundary rather than remove it: arbitrary precision needs heap-allocated digits and an allocator call on every operation, which is a different project.

Everywhere else, waspy holds itself to the opposite standard: a construct either produces Python's answer or fails loudly, never a quietly different one. Integer width is one of two documented exceptions, so if your program's arithmetic can exceed 32 bits, it is not in the supported subset.

The other is a NaN inside a collection. Floats are unboxed values here, not objects, so there is no identity for CPython's "same object, therefore equal" shortcut to find: with `x = float("nan")`, `(x,) == (x,)` and `[x] == [x]` are False, where CPython answers True. `x == x` is False in both.

Float division by zero and integer division or modulo by zero raise `ZeroDivisionError` rather than producing `inf` or trapping. `**` on floats is CPython's: `2.0 ** 0.5` and every special value give its answer, zero to a negative power raises `ZeroDivisionError`, and a result too large raises `OverflowError`. A negative float to a fractional power traps, since CPython's answer is a complex number. On ints, `**` multiplies (and wraps), and a negative integer exponent traps because Python's answer is a float an int result cannot hold. `float()` and `int()` of a string take ASCII digits and whitespace only: CPython also accepts other Unicode decimal digits and spaces, which trap here rather than raise. `round(x, n)` needs `0 <= n <= 22`.

## The math module

CPython's `math` functions call the platform's C library, so CPython itself can answer differently in the last bit on different platforms: `math.tan(0.1)` is `0.10033467208545054` on macOS and with glibc, and `0.10033467208545055` with musl. waspy's reference platform is CPython on WASI, which links musl's libm, and its `math` is a port of that libm, checked bit for bit against it. So every result is CPython-on-WASI's, and most agree with CPython elsewhere; the ones that do not are a last-bit rounding of the same value. `sqrt`, `floor`, `ceil`, `trunc`, `fabs`, `copysign`, `fmod`, and `remainder` are exact everywhere, and `hypot` is CPython's own algorithm. `**` on floats uses the same `pow`.
