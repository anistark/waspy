# enum: a class deriving from Enum, IntEnum, or StrEnum is written out where
# the program defines it (ir::pystdlib): an Enum's members are instances
# with a name and a value, an IntEnum's ints, a StrEnum's strs.


class Enum:
    pass


class IntEnum:
    pass


class StrEnum:
    pass


def auto() -> int:
    raise NotImplementedError("enum.auto() is a member's value where an Enum class defines it")
