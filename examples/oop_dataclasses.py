"""Dataclasses (v0.16.0): @dataclass generates __init__, __eq__, and __repr__.

The constructor takes one parameter per annotated field (defaults included),
`==`/`!=` between two instances compare field values via the generated
__eq__, and __repr__ renders `Name(field=value, ...)` at runtime.
"""

from dataclasses import dataclass


@dataclass
class Point:
    x: int
    y: int


@dataclass
class Rect:
    width: int
    height: int
    color: int = 7


@dataclass
class Label:
    text: str
    size: int = 12


def construct_with_all_args() -> int:
    """The generated __init__ assigns each field: 3 + 4 = 7."""
    p = Point(3, 4)
    return p.x + p.y


def construct_with_default() -> int:
    """An omitted trailing argument takes the field default: 2*3 + 7 = 13."""
    r = Rect(2, 3)
    return r.width * r.height + r.color


def default_can_be_overridden() -> int:
    """Passing the defaulted argument overrides it: 2*3 + 9 = 15."""
    r = Rect(2, 3, 9)
    return r.width * r.height + r.color


def equal_by_value() -> int:
    """Two instances with equal fields compare equal (not by pointer)."""
    a = Point(1, 2)
    b = Point(1, 2)
    if a == b:
        return 1
    return 0


def unequal_by_value() -> int:
    """A differing field makes instances unequal, and != inverts __eq__."""
    a = Point(1, 2)
    b = Point(1, 3)
    result = 0
    if a != b:
        result = result + 1
    if a == b:
        result = result + 10
    return result


def repr_round_trips() -> int:
    """__repr__ renders the class name and each field's runtime value."""
    p = Point(1, -23)
    s = p.__repr__()
    if s == "Point(x=1, y=-23)":
        return 1
    return 0


def repr_quotes_strings() -> int:
    """String fields are quoted like Python's repr."""
    label = Label("hi")
    s = label.__repr__()
    if s == "Label(text='hi', size=12)":
        return 1
    return 0


def string_field_round_trips() -> int:
    """A str field stores its offset word and reads back byte-for-byte."""
    label = Label("waspy", 3)
    if label.text == "waspy":
        return label.size
    return 0
