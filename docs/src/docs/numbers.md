---
title: Numbers
eyebrow: Language
description: "How int and float behave, and where waspy deliberately answers differently from CPython."
---

`int` is a 32-bit two's-complement integer, and `float` is an IEEE-754 double.

CPython's `int` is arbitrary precision: it grows to hold whatever it is given. Here an int holds -2147483648 to 2147483647, and an operation whose result leaves that range raises `OverflowError` instead of wrapping around:

```python
1000000 * 1000000   # CPython: 1000000000000    waspy: OverflowError
2 ** 40             # CPython: 1099511627776    waspy: OverflowError
2147483647 + 1      # CPython: 2147483648       waspy: OverflowError
```

A program whose integer values stay inside that range gets Python's answers, and one that leaves it stops with an error it can catch (`except OverflowError:`), never a quietly different number. Every int `+`, `-`, `*`, `**`, `<<`, unary `-`, `//` (the least int divided by -1), `abs()`, `sum()`, and `int()` of a string or a float is checked. The check is a 64-bit operation and a compare, which costs a few instructions on the hottest path in a program, and keeps the rule the rest of the compiler holds to: a construct produces Python's answer or fails loudly.

Shifts follow CPython too: `x >> 40` is 0 (or -1 for a negative `x`), `1 << 31` raises `OverflowError`, and a negative shift count raises `ValueError`. The bitwise operators `&`, `|`, `^`, and `~` cannot leave the range.

Code that relies on wrapping (a hash, a checksum, a pseudo-random generator) keeps its values small explicitly, as portable Python does: `h = (h * 31 + c) % 1000003` never leaves the range, since `h` stays below 1000003, and a 32-bit hash can be computed in 16-bit halves. A 64-bit integer would only move the boundary rather than remove it: arbitrary precision needs heap-allocated digits and an allocator call on every operation, which is a different project.

Float values are not affected: `1e300 * 1e300` is `inf` in both, and `float ** float` raises `OverflowError` where CPython does.

One answer that differs from CPython is a NaN inside a collection. Floats are unboxed values here, not objects, so there is no identity for CPython's "same object, therefore equal" shortcut to find: with `x = float("nan")`, `(x,) == (x,)` and `[x] == [x]` are False, where CPython answers True. `x == x` is False in both.

Float division by zero and integer division or modulo by zero raise `ZeroDivisionError` rather than producing `inf` or trapping. `**` on floats is CPython's: `2.0 ** 0.5` and every special value give its answer, zero to a negative power raises `ZeroDivisionError`, and a result too large raises `OverflowError`. A negative float to a fractional power traps, since CPython's answer is a complex number. On ints, `**` multiplies (raising `OverflowError` past 32 bits), and a negative integer exponent traps because Python's answer is a float an int result cannot hold. `float()` and `int()` of a string take ASCII digits and whitespace only: CPython also accepts other Unicode decimal digits and spaces, which trap here rather than raise. `round(x, n)` needs `0 <= n <= 22`.

## The math module

CPython's `math` functions call the platform's C library, so CPython itself can answer differently in the last bit on different platforms: `math.tan(0.1)` is `0.10033467208545054` on macOS and with glibc, and `0.10033467208545055` with musl. waspy's reference platform is CPython on WASI, which links musl's libm, and its `math` is a port of that libm, checked bit for bit against it. So every result is CPython-on-WASI's, and most agree with CPython elsewhere; the ones that do not are a last-bit rounding of the same value. `sqrt`, `floor`, `ceil`, `trunc`, `fabs`, `copysign`, `fmod`, and `remainder` are exact everywhere, and `hypot` is CPython's own algorithm. `**` on floats uses the same `pow`.
