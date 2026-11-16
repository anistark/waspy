# The builtins written in Python: their calls are made calls of these where
# the program does not define the name itself (ir::pystdlib).

_DIGITS = "0123456789abcdefghijklmnopqrstuvwxyz"


def _digits(n: int, base: int) -> str:
    # n >= 0; the least int's magnitude is not an int here, so it is spelled
    # from its digits one step short.
    if n == 0:
        return "0"
    out: list[str] = []
    while n > 0:
        out.append(_DIGITS[n % base])
        n //= base
    out.reverse()
    return "".join(out)


def _magnitude(x: int, base: int) -> str:
    if x >= 0:
        return _digits(x, base)
    if x == -2147483647 - 1:
        # -(x // base) fits where -x does not
        q = -(x // base)
        r = -(x % base)
        if r < 0:
            q -= 1
            r += base
        return _digits(q, base) + _DIGITS[r]
    return _digits(-x, base)


def hex(x: int) -> str:
    if x < 0:
        return "-0x" + _magnitude(x, 16)
    return "0x" + _magnitude(x, 16)


def oct(x: int) -> str:
    if x < 0:
        return "-0o" + _magnitude(x, 8)
    return "0o" + _magnitude(x, 8)


def bin(x: int) -> str:
    if x < 0:
        return "-0b" + _magnitude(x, 2)
    return "0b" + _magnitude(x, 2)


def divmod(a, b):
    return (a // b, a % b)


def _mulmod(a: int, b: int, m: int) -> int:
    # (a * b) % m for 0 <= a, b < m, without an intermediate past 32 bits.
    if m <= 46340:
        return (a * b) % m
    result = 0
    while b > 0:
        if b & 1:
            result = result + a
            if result >= m:
                result -= m
        a = a + a
        if a >= m:
            a -= m
        b >>= 1
    return result


def pow3(base: int, exp: int, mod: int) -> int:
    if mod == 0:
        raise ValueError("pow() 3rd argument cannot be 0")
    m = mod if mod > 0 else -mod
    if m == 1:
        return 0
    b = base % m
    if exp < 0:
        # the inverse of base modulo m (CPython 3.8+)
        old_r = b
        r = m
        old_s = 1
        s = 0
        while r != 0:
            q = old_r // r
            old_r, r = r, old_r - q * r
            old_s, s = s, old_s - q * s
        if old_r != 1:
            raise ValueError("base is not invertible for the given modulus")
        b = old_s % m
        exp = -exp
    result = 1
    while exp > 0:
        if exp & 1:
            result = _mulmod(result, b, m)
        b = _mulmod(b, b, m)
        exp >>= 1
    if mod < 0 and result != 0:
        result -= m
    return result


def int_base(text: str, base: int) -> int:
    if base != 0 and (base < 2 or base > 36):
        raise ValueError("int() base must be >= 2 and <= 36, or 0")
    s = text.strip()
    bad = "invalid literal for int() with base " + str(base) + ": " + repr(text)
    negative = False
    if s.startswith("-") or s.startswith("+"):
        negative = s[0] == "-"
        s = s[1:]
    low = s.lower()
    b = base
    prefixed = False
    if len(low) >= 2 and low[0] == "0" and low[1] in "xob":
        p = {"x": 16, "o": 8, "b": 2}[low[1]]
        if b == 0 or b == p:
            b = p
            s = s[2:]
            low = low[2:]
            prefixed = True
    if b == 0:
        b = 10
        if len(low) > 1 and low[0] == "0" and low.strip("0_") != "":
            raise ValueError(bad)
    if low == "" or low.startswith("_") and not prefixed or low.endswith("_") or "__" in low:
        raise ValueError(bad)
    value = 0
    for c in low:
        if c == "_":
            continue
        d = _DIGITS.find(c)
        if d < 0 or d >= b:
            raise ValueError(bad)
        value = value * b - d if negative else value * b + d
    return value
