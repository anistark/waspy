# contextlib: suppress, written as the try statement it means where a `with`
# uses it (ir::pystdlib).


def suppress(*exceptions: int) -> int:
    raise NotImplementedError("contextlib.suppress is written as a try statement where a with statement uses it")
