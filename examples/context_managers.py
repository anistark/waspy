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


def main() -> int:
    print("context managers")
    return enter_and_exit()
