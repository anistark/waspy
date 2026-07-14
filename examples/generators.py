# Generators and the iterator protocol (v0.18.0)
# - generator functions suspend at `yield` and resume on the next request
# - `for` drives generators, `yield from` delegates, next()/send()/close() work
# - user classes implementing __iter__/__next__ iterate in `for` loops


def count_up(n: int):
    """Suspend inside a while loop; resume after each yield."""
    i = 0
    while i < n:
        yield i
        i = i + 1


def squares(n: int):
    """Suspend inside a for-over-range loop."""
    for i in range(n):
        yield i * i


def first_evens(limit: int):
    """Conditional yields plus an early return ending iteration."""
    for i in range(100):
        if i >= limit:
            return
        if i % 2 == 0:
            yield i


def chained():
    """Delegate to a range and another generator with yield from."""
    yield from range(3)
    yield from count_up(3)


def doubled(n: int):
    """A generator consuming another generator."""
    for x in count_up(n):
        yield x * 2


def echo():
    """Yield expressions: each send() resumes with the sent value."""
    total = 0
    while True:
        got = yield total
        total = total + got


def halves(n: int):
    """Float yields make the generator produce f64 values."""
    i = 0
    while i < n:
        yield 0.5
        i = i + 1


class Countdown:
    """User iterator: __iter__/__next__ with StopIteration ending the loop."""

    def __init__(self, start: int):
        self.current = start

    def __iter__(self) -> "Countdown":
        return self

    def __next__(self) -> int:
        if self.current <= 0:
            raise StopIteration
        value = self.current
        self.current = self.current - 1
        return value


# --- entry points (no-arg, i32) exercised by the integration tests ---


def sum_count_up() -> int:
    """0 + 1 + 2 + 3 + 4 = 10."""
    total = 0
    for x in count_up(5):
        total = total + x
    return total


def sum_squares() -> int:
    """0 + 1 + 4 + 9 = 14."""
    total = 0
    for s in squares(4):
        total = total + s
    return total


def sum_first_evens() -> int:
    """0 + 2 + 4 = 6 (return stops iteration at 5)."""
    total = 0
    for e in first_evens(6):
        total = total + e
    return total


def sum_chained() -> int:
    """(0 + 1 + 2) + (0 + 1 + 2) = 6."""
    total = 0
    for v in chained():
        total = total + v
    return total


def sum_doubled() -> int:
    """2 * (0 + 1 + 2 + 3) = 12."""
    total = 0
    for d in doubled(4):
        total = total + d
    return total


def manual_next() -> int:
    """Drive a generator by hand: 0, then 1, then 2 -> 100*0 + 10*1 + 2."""
    g = count_up(9)
    a = next(g)
    b = next(g)
    c = next(g)
    return a * 100 + b * 10 + c


def send_accumulates() -> int:
    """Prime with next(), then send 5 and 7: the echo total reaches 12."""
    g = echo()
    next(g)
    g.send(5)
    return g.send(7)


def closed_generator_stops() -> int:
    """close() exhausts the generator, so the loop body never runs."""
    g = count_up(10)
    g.close()
    total = 0
    for x in g:
        total = total + x
    return total


def countdown_sum() -> int:
    """User iterator in a for loop: 4 + 3 + 2 + 1 = 10."""
    total = 0
    for v in Countdown(4):
        total = total + v
    return total


def generator_break_early() -> int:
    """break leaves the drive loop; 0 + 1 + 2 + 3 = 6, stopping at 4."""
    total = 0
    for x in count_up(100):
        if x >= 4:
            break
        total = total + x
    return total


def two_generators_independent() -> int:
    """Two live instances of one generator advance independently."""
    a = count_up(10)
    b = count_up(10)
    x = next(a)
    y = next(a)
    z = next(b)
    return x * 100 + y * 10 + z


def sum_halves() -> float:
    """0.5 + 0.5 + 0.5 = 1.5 through f64 yields."""
    total = 0.0
    for h in halves(3):
        total = total + h
    return total
