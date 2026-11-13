"""Imports bound every way the compiler links them.

A standard-library module under another name, an import inside a function,
and `importlib.import_module` or `__import__` with a constant name. Every
module is linked into the program when it is compiled, so each of these names
the module it would load in CPython, and a name computed at run time is
refused.
"""

import importlib
import math as m
import os.path as osp

PARSER = "json"


def circle_area(r: float) -> float:
    return m.pi * r * r


def joined(a: str, b: str) -> str:
    return osp.join(a, b)


def word_count(text: str, word: str) -> int:
    from collections import Counter

    counts = Counter(text.split())
    return counts[word]


def encoded() -> str:
    json = importlib.import_module(PARSER)
    return json.dumps({"ok": [1, 2], "n": [3]})


def root_of(x: float) -> float:
    return __import__("math").sqrt(x)


if __name__ == "__main__":
    print(round(circle_area(2.0), 3), joined("logs", "app.txt"))
    print(word_count("the cat saw the dog", "the"), encoded(), root_of(49.0))
