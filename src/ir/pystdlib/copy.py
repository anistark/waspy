# copy: copy() and deepcopy() are written where the program calls them, by
# the type of what they copy (compiler::dunders).


def copy(x: int) -> int:
    raise NotImplementedError("copy.copy() is written where it is called")


def deepcopy(x: int) -> int:
    raise NotImplementedError("copy.deepcopy() is written where it is called")
