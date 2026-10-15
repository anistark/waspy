"""Conditional expressions: `a if cond else b`.

Only the arm the condition picks is evaluated, and the condition is tested
for truth the way `if` tests it. Both arms must read as one type: two classes
share their nearest common base, a class and None make an Optional, and an int
widens only where a float is expected. Arms that would read differently (an
int and a float, a bool and an int, a str and None) are refused.
"""

from typing import List


def sign(n: int) -> int:
    return 1 if n > 0 else -1 if n < 0 else 0


def plural(count: int) -> str:
    return f"{count} file{'' if count == 1 else 's'}"


def clamp(x: float, low: float, high: float) -> float:
    return low if x < low else high if x > high else x


def describe(scores: List[int]) -> str:
    return "none" if not scores else "high" if max(scores[0], scores[-1]) > 90 else "low"


def sign_total() -> int:
    total = 0
    for n in [-5, 0, 7, 3]:
        total += sign(n)
    return total


def labels() -> str:
    return plural(1) + ", " + plural(4)


def clamped() -> float:
    return clamp(-2.0, 0.0, 1.0) + clamp(0.25, 0.0, 1.0) + clamp(9.0, 0.0, 1.0)


def descriptions() -> str:
    return describe([]) + " " + describe([95, 10]) + " " + describe([40, 50])
