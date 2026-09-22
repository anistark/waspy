"""functools decorators: @singledispatch and @total_ordering.

`@singledispatch` picks an implementation by the type of the call's first
argument. Dispatch is decided at compile time from the argument's static
type, so the argument has to have one (a literal, an annotated parameter, or
a value the compiler can type). `@total_ordering` fills in the ordering
methods a class leaves out from the one it defines, and ordering between
instances calls the class's own `__lt__`/`__gt__`/... the way CPython does.

`@lru_cache` and `@cache` are accepted and change nothing: memoization is a
speed concern a compiled module does not observe. Other functools names
(`partial`, `reduce`, `cached_property`, `cmp_to_key`) are refused at
compile time rather than accepted and ignored.
"""

from functools import singledispatch, total_ordering


@singledispatch
def describe(value: int) -> str:
    return "int"


@describe.register
def _(value: str) -> str:
    return "str:" + value


@describe.register(float)
def _(value) -> str:
    return "float"


@describe.register
def _(value: bool) -> str:
    return "bool"


def dispatch_by_type() -> str:
    word = "x"
    return describe(5) + " " + describe(word) + " " + describe(2.5) + " " + describe(True)


@total_ordering
class Version:
    def __init__(self, major: int, minor: int):
        self.major = major
        self.minor = minor

    def __eq__(self, other) -> bool:
        return self.major == other.major and self.minor == other.minor

    def __lt__(self, other) -> bool:
        if self.major != other.major:
            return self.major < other.major
        return self.minor < other.minor


def ordering_from_lt() -> int:
    a = Version(1, 4)
    b = Version(2, 0)
    c = Version(1, 4)
    hits = 0
    if a < b:
        hits += 1
    if b > a:
        hits += 10
    if a <= c:
        hits += 100
    if a >= c:
        hits += 1000
    if a >= b:
        hits += 10000
    return hits


def newest(versions_major: int) -> int:
    best = Version(0, 0)
    for minor in range(versions_major):
        candidate = Version(minor % 2, minor)
        if candidate > best:
            best = candidate
    return best.major * 10 + best.minor
