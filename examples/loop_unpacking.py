# Tuple targets in for loops, and the iterator-shaped builtins (v0.18.0)
# - for a, b in pairs (with star targets)
# - for i, x in enumerate(xs[, start])
# - for a, b, ... in zip(...)
# - for k, v in d.items() / k in d.keys() / v in d.values()
# All compose with generators: the desugared loops suspend at yield like any
# other loop.


def pairs_sum() -> int:
    """(1,2) (3,4) (5,6) as a*10 + b each: 12 + 34 + 56 = 102."""
    total = 0
    for a, b in [(1, 2), (3, 4), (5, 6)]:
        total = total + a * 10 + b
    return total


def star_target() -> int:
    """a, *rest, z binds the middle slice as a list: 124 + 528 = 652."""
    total = 0
    for a, *rest, z in [(1, 2, 3, 4), (5, 6, 7, 8)]:
        total = total + a * 100 + len(rest) * 10 + z
    return total


def enum_sum() -> int:
    """enumerate([10, 20, 30]): i*100 + x each -> 360."""
    xs = [10, 20, 30]
    total = 0
    for i, x in enumerate(xs):
        total = total + i * 100 + x
    return total


def enum_start() -> int:
    """enumerate(xs, 7) starts the counter at 7: 75 + 86 = 161."""
    total = 0
    for i, x in enumerate([5, 6], 7):
        total = total + i * 10 + x
    return total


def enum_range() -> int:
    """enumerate over a range: (0+10) + (1+11) + (2+12) = 36."""
    total = 0
    for i, x in enumerate(range(10, 13)):
        total = total + i + x
    return total


def zip_sum() -> int:
    """zip stops at the shorter sequence: 11 + 22 + 33 = 66."""
    xs = [1, 2, 3]
    ys = [10, 20, 30, 40]
    total = 0
    for a, b in zip(xs, ys):
        total = total + a + b
    return total


def zip3_sum() -> int:
    """Three-way zip, shortest wins: 111 + 222 = 333."""
    total = 0
    for a, b, c in zip([1, 2], [10, 20], [100, 200, 300]):
        total = total + a + b + c
    return total


def items_sum() -> int:
    """dict.items(): k*100 + v each -> 110 + 220 + 330 = 660."""
    d = {1: 10, 2: 20, 3: 30}
    total = 0
    for k, v in d.items():
        total = total + k * 100 + v
    return total


def keys_values_sum() -> int:
    """keys() and values() walk the same entries: (4+5)*100 + (7+8) = 915."""
    d = {4: 7, 5: 8}
    keys = 0
    for k in d.keys():
        keys = keys + k
    values = 0
    for v in d.values():
        values = values + v
    return keys * 100 + values


def indexed(n: int):
    """enumerate inside a generator suspends per pair."""
    for i, x in enumerate(range(n)):
        yield i + x * 10


def enum_in_generator() -> int:
    """0 + 11 + 22 = 33 through a suspended enumerate loop."""
    total = 0
    for v in indexed(3):
        total = total + v
    return total


def dict_pairs():
    """items() inside a generator: the dict lives in the state instance."""
    d = {1: 5, 2: 6}
    for k, v in d.items():
        yield k * 10 + v


def items_in_generator() -> int:
    """15 + 26 = 41 through a suspended items() loop."""
    total = 0
    for v in dict_pairs():
        total = total + v
    return total
