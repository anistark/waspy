"""Exceptions: raising, catching, propagating, and cleaning up.

`raise` transfers control. It records the exception's type in a module global
and leaves the block: to the enclosing `try`'s handlers, or out of the function
and into whatever the caller does next. A handler whose type matches catches it
and clears it; one that does not match lets it keep travelling. An exception
nothing catches unwinds out of the program and traps, rather than letting a
function return as though nothing happened.

There are no exception *objects* here: `raise ValueError("...")` records the
type, and the message is not carried.
"""


def divide(a: float, b: float) -> float:
    """
    Divides a by b and raises ValueError if division by zero occurs.
    Returns the result as float.
    """
    try:
        return a / b
    except ZeroDivisionError:
        raise ValueError("Cannot divide by zero")
    except ValueError:
        raise
    finally:
        print("Execution completed.")


class Ledger:
    """Records what ran, so the tests can see which paths were taken."""

    def __init__(self):
        self.cleanups = 0
        self.steps = 0

    def cleanup(self):
        self.cleanups = self.cleanups + 1

    def step(self):
        self.steps = self.steps + 1


def check_positive(n: int) -> int:
    """Raise unless the number is positive."""
    if n <= 0:
        raise ValueError("not positive")
    return n


def caught_here() -> int:
    """A raise in the try body reaches the matching handler."""
    try:
        n = check_positive(-1)
    except ValueError:
        return 5
    return n


def raise_skips_the_rest() -> int:
    """The statements after a raise never run: the counter stops at 1."""
    ledger = Ledger()
    try:
        ledger.step()
        raise ValueError("stop")
        ledger.step()
    except ValueError:
        return ledger.steps
    return -1


def raise_leaves_the_loop() -> int:
    """A raise inside a loop leaves the loop, it does not finish iterating."""
    ledger = Ledger()
    try:
        i = 0
        while i < 5:
            ledger.step()
            raise ValueError("stop")
            i = i + 1
    except ValueError:
        return ledger.steps
    return -1


def unwinds_through_a_call(ledger: Ledger) -> int:
    """Called by `propagates_with_cleanup`: raises with a finally to run."""
    try:
        n = check_positive(0)
    finally:
        ledger.cleanup()
    return n


def propagates_with_cleanup() -> int:
    """An exception travels out of the callee into this handler, and the
    callee's `finally` runs on the way past."""
    ledger = Ledger()
    try:
        n = unwinds_through_a_call(ledger)
    except ValueError:
        return ledger.cleanups
    return -1


def unmatched_handler_passes_it_on() -> int:
    """The inner handler is for another type, so the outer one catches."""
    try:
        try:
            raise ValueError("x")
        except TypeError:
            return 1
    except ValueError:
        return 5
    return 9


def catching_resumes_normally() -> int:
    """After a handler runs, the function carries on: 5 + 10."""
    n = 0
    try:
        raise ValueError("x")
    except ValueError:
        n = 5
    n = n + 10
    return n

def exception_base_catches_any() -> int:
    """`except Exception:` catches anything, the way every exception being a
    subclass of it makes it in Python."""
    try:
        raise ValueError("x")
    except Exception:
        return 5
    return 9


def tuple_of_types() -> int:
    """`except (A, B):` catches either of the types it names."""
    try:
        raise TypeError("x")
    except (ValueError, TypeError):
        return 5
    return 9


def tuple_of_types_passes_others_on() -> int:
    """...and passes on the ones it does not name."""
    try:
        try:
            raise KeyError("x")
        except (ValueError, TypeError):
            return 1
    except KeyError:
        return 5
    return 9
