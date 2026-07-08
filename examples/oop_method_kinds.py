"""Method kinds (v0.15.0): @staticmethod, @classmethod, and @property.

A static method takes no implicit argument and is callable on the class or an
instance. A class method receives the class implicitly and works as a factory
via `cls(...)`. A property compiles attribute reads to its getter and
assignments to its setter, instead of direct field access.
"""


class Counter:
    def __init__(self, start: int):
        self.count = start

    @staticmethod
    def add(a: int, b: int) -> int:
        return a + b

    @classmethod
    def create(cls, start: int) -> "Counter":
        return cls(start)

    def increment(self) -> int:
        self.count += 1
        return self.count


class Temperature:
    def __init__(self, celsius: float):
        self._celsius = celsius

    @property
    def celsius(self) -> float:
        return self._celsius

    @celsius.setter
    def celsius(self, value: float):
        self._celsius = value

    @property
    def fahrenheit(self) -> float:
        return self._celsius * 9.0 / 5.0 + 32.0


def static_method_on_class() -> int:
    """Called on the class, no instance involved: 19 + 23 = 42."""
    return Counter.add(19, 23)


def static_method_on_instance() -> int:
    """Called on an instance, which is ignored: 40 + 2 = 42."""
    c = Counter(0)
    return c.add(40, 2)


def classmethod_factory() -> int:
    """`cls(...)` inside the classmethod constructs the class: 41 + 1 = 42."""
    c = Counter.create(41)
    return c.increment()


def classmethod_on_instance() -> int:
    """A classmethod called via an instance still builds a fresh one: 5 + 10 = 15."""
    a = Counter(5)
    b = a.create(10)
    return a.count + b.count


def property_getter() -> float:
    """`t.celsius` invokes the getter method: 25.0."""
    t = Temperature(25.0)
    return t.celsius


def property_setter() -> float:
    """`t.celsius = v` invokes the setter method: 21.5."""
    t = Temperature(0.0)
    t.celsius = 21.5
    return t.celsius


def computed_property() -> float:
    """A getter with no backing field of its own: 100C -> 212.0F."""
    t = Temperature(100.0)
    return t.fahrenheit


def property_augmented_assignment() -> float:
    """`t.celsius += d` reads via the getter and writes via the setter: 21.5."""
    t = Temperature(20.0)
    t.celsius += 1.5
    return t.celsius
