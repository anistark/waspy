"""Abstract base classes (v0.16.0): abc.ABC with @abstractmethod.

A class deriving from ABC that still has an unimplemented abstract method
cannot be instantiated (rejected at compile time); a concrete subclass that
implements every abstract method instantiates and dispatches normally.
"""

from abc import ABC, abstractmethod


class Shape(ABC):
    def __init__(self, sides: int):
        self.sides = sides

    @abstractmethod
    def area(self) -> int:
        pass

    def describe(self) -> int:
        """A concrete method on the ABC, inherited by every subclass."""
        return self.sides


class Square(Shape):
    def __init__(self, edge: int):
        super().__init__(4)
        self.edge = edge

    def area(self) -> int:
        return self.edge * self.edge


class Triangle(Shape):
    def __init__(self, base: int, height: int):
        super().__init__(3)
        self.base = base
        self.height = height

    def area(self) -> int:
        return self.base * self.height // 2


def concrete_subclass_area() -> int:
    """A concrete subclass instantiates and implements the abstract method: 36."""
    s = Square(6)
    return s.area()


def inherited_concrete_method() -> int:
    """The ABC's concrete method works on subclass instances: 4 + 3 = 7."""
    s = Square(2)
    t = Triangle(6, 5)
    return s.describe() + t.describe()


def abstract_method_dispatch() -> int:
    """Each subclass supplies its own implementation: 4 + 15 = 19."""
    s = Square(2)
    t = Triangle(6, 5)
    return s.area() + t.area()


def isinstance_of_abstract_base() -> int:
    """isinstance works against the abstract base: both are Shapes."""
    s = Square(2)
    t = Triangle(6, 5)
    result = 0
    if isinstance(s, Shape):
        result = result + 1
    if isinstance(t, Shape):
        result = result + 10
    if isinstance(s, Triangle):
        result = result + 100
    return result
