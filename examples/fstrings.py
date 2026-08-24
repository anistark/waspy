"""F-string formatting.

Each placeholder renders its value with str() and the pieces are concatenated,
so an f-string is exactly the '+' chain you would write by hand.

Not supported yet, and rejected at compile time rather than rendered wrong:
a format specifier (f"{x:.2f}"), the !r and !a conversions, and a placeholder
holding a value str() cannot render (a float, a bool, or a collection).
"""


def greet(name: str) -> str:
    return f"Hello, {name}!"


def label(count: int) -> str:
    return f"{count} items"


def summary(a: int, b: int) -> str:
    return f"a={a} b={b} sum={a + b}"


def escaped(n: int) -> str:
    # Doubled braces are a literal brace, as in Python.
    return f"{{{n}}}"


def constants() -> str:
    # Constant placeholders fold into the literal at compile time, so a float
    # or a bool can be interpolated as long as it is written as a plain literal.
    return f"{1} {2.5} {True}"


def joined(n: int) -> str:
    out = ""
    for i in range(n):
        out = out + f"{i},"
    return out


class Item:
    def __init__(self, name: str, qty: int):
        self.name = name
        self.qty = qty

    def line(self) -> str:
        return f"{self.name} x{self.qty}"


def item_line() -> str:
    it: Item = Item("bolt", 12)
    return it.line()


def greet_len() -> int:
    return len(greet("world"))


def label_len(count: int) -> int:
    return len(label(count))


def summary_len(a: int, b: int) -> int:
    return len(summary(a, b))


def joined_len(n: int) -> int:
    return len(joined(n))
