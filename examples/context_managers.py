"""Context managers: `with` over a user class implementing the protocol.

A `with` statement calls `__enter__` on the way in, binds whatever it returns
to the `as` name, and calls `__exit__` on the way out, including when the body
returns early. `with open(...) as f:` is handled separately (see file_io.py).
"""


class Resource:
    """Tracks how many times it was entered and exited."""

    def __init__(self, size: int):
        self.size = size
        self.entered = 0
        self.exited = 0

    def __enter__(self) -> int:
        self.entered = self.entered + 1
        return self.size

    def __exit__(self, exc_type: int, exc_value: int, traceback: int) -> int:
        self.exited = self.exited + 1
        return 0

    def state(self) -> int:
        return self.entered * 10 + self.exited


class Order:
    """Records the order cleanups run in: each one appends a digit, so 12 means
    the `1` cleanup ran before the `2` cleanup."""

    def __init__(self):
        self.order = 0

    def mark(self, digit: int):
        self.order = self.order * 10 + digit

    def __enter__(self) -> int:
        return 1

    def __exit__(self, exc_type: int, exc_value: int, traceback: int) -> int:
        self.order = self.order * 10 + 2
        return 0


class Scaled(Resource):
    """A subclass inherits the protocol from its base."""

    def doubled(self) -> int:
        return self.size * 2


def enter_and_exit() -> int:
    """Both halves of the protocol run: entered once, exited once."""
    r = Resource(4)
    with r as size:
        pass
    return r.state()


def binds_enter_result() -> int:
    """The `as` name is what __enter__ returned, not the manager itself."""
    with Resource(21) as size:
        return size * 2


def exit_runs_before_return() -> int:
    """An early return from the body still runs __exit__ first."""
    r = Resource(1)
    with r as size:
        return r.exited * 100 + size
    return -1


def exit_ran_afterwards() -> int:
    """After the block, the manager has recorded its exit."""
    r = Resource(1)
    with r as size:
        pass
    return r.exited


def nested_blocks() -> int:
    """Nested managers each run their own protocol."""
    outer = Resource(1)
    inner = Resource(2)
    with outer as a:
        with inner as b:
            pass
    return outer.state() * 100 + inner.state()


def repeated_in_a_loop() -> int:
    """Re-entering the same manager accumulates on each pass."""
    r = Resource(3)
    i = 0
    while i < 4:
        with r as size:
            pass
        i = i + 1
    return r.state()


def inherited_protocol() -> int:
    """A subclass of a context manager is one too."""
    s = Scaled(5)
    with s as size:
        pass
    return s.doubled() * 100 + s.state()


def exit_runs_before_break() -> int:
    """A `break` leaving the body runs __exit__ first, exactly as a return
    does. The loop would run three times; the first pass leaves it."""
    r = Resource(1)
    i = 0
    while i < 3:
        with r as size:
            break
        i = i + 1
    return r.state()


def exit_runs_before_continue() -> int:
    """A `continue` leaving the body runs __exit__ too, once per pass."""
    r = Resource(1)
    i = 0
    while i < 3:
        i = i + 1
        with r as size:
            continue
    return r.state()


def inner_loop_break_stays_inside() -> int:
    """A `break` that belongs to a loop written inside the body binds to that
    loop, so __exit__ runs once, on the way out of the block."""
    r = Resource(1)
    with r as size:
        i = 0
        while i < 3:
            break
    return r.state()


def finally_runs_before_break() -> int:
    """`finally` runs on the way out through a `break`, not only on the
    ordinary path."""
    r = Resource(1)
    i = 0
    while i < 3:
        try:
            break
        finally:
            r.exited = r.exited + 1
    return r.exited


def finally_runs_before_return() -> int:
    """`finally` runs on the way out through a `return`."""
    r = Resource(1)
    n = finally_returning(r)
    return r.exited * 10 + n


def finally_returning(r: Resource) -> int:
    try:
        return 3
    finally:
        r.exited = r.exited + 1


def cleanup_order_finally_then_exit() -> int:
    """Cleanups run innermost first: the inner `finally` before the enclosing
    block's __exit__."""
    r = Order()
    n = order_try_in_with(r)
    return r.order


def order_try_in_with(r: Order) -> int:
    with r as size:
        try:
            return 0
        finally:
            r.mark(1)


def cleanup_order_exit_then_finally() -> int:
    """The other nesting: __exit__ before the enclosing `finally`."""
    r = Order()
    n = order_with_in_try(r)
    return r.order


def order_with_in_try(r: Order) -> int:
    try:
        with r as size:
            return 0
    finally:
        r.mark(1)


def main() -> int:
    print("context managers")
    return enter_and_exit()
