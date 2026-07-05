"""Single inheritance and method resolution (v0.14.0).

A subclass lays its base's fields out as a prefix and appends its own, so a
base method reading `self.x` works unchanged on a subclass instance.
Inherited methods dispatch to the base's compiled function; overrides
replace it. `super().__init__(...)` and `super().method(...)` call up one
level. Every instance carries its class id in a tag word, so `isinstance`
answers correctly at runtime even when the static type is a base class.
"""


class Animal:
    def __init__(self, legs: int):
        self.legs = legs
        self.energy = 10

    def speak(self) -> int:
        return 1

    def leg_count(self) -> int:
        return self.legs


class Dog(Animal):
    def __init__(self, tricks: int):
        super().__init__(4)
        self.tricks = tricks

    def speak(self) -> int:
        return 2

    def speak_like_parent(self) -> int:
        return super().speak()


class Puppy(Dog):
    def __init__(self):
        super().__init__(0)


def override_wins() -> int:
    """Dog overrides speak: Animal says 1, Dog says 2 -> 12."""
    a = Animal(2)
    d = Dog(0)
    return a.speak() * 10 + d.speak()


def inherited_method_on_subclass() -> int:
    """leg_count is defined only on Animal; on a Dog it reads the legs
    field set by the chained super().__init__(4)."""
    d = Dog(0)
    return d.leg_count()


def super_init_chains() -> int:
    """Dog.__init__ -> Animal.__init__ sets legs and energy; Dog adds its
    own field after the base prefix: 4 + 10 + 3 = 17."""
    d = Dog(3)
    return d.legs + d.energy + d.tricks


def super_method_call() -> int:
    """super().speak() inside Dog reaches Animal's implementation."""
    d = Dog(0)
    return d.speak_like_parent()


def two_level_chain() -> int:
    """Puppy -> Dog -> Animal construction chain: 4 legs + 10 energy."""
    p = Puppy()
    return p.legs + p.energy


def isinstance_across_hierarchy() -> int:
    """A Puppy is a Puppy, a Dog, and an Animal (1 + 10 + 100); an Animal
    is not a Dog (no +1000). Total: 111."""
    p = Puppy()
    a = Animal(2)
    total = 0
    if isinstance(p, Puppy):
        total = total + 1
    if isinstance(p, Dog):
        total = total + 10
    if isinstance(p, Animal):
        total = total + 100
    if isinstance(a, Dog):
        total = total + 1000
    return total


def issubclass_checks() -> int:
    """issubclass folds at compile time: Puppy<=Animal (1), Dog<=Dog (10),
    but not Animal<=Puppy (no +100). Total: 11."""
    total = 0
    if issubclass(Puppy, Animal):
        total = total + 1
    if issubclass(Dog, Dog):
        total = total + 10
    if issubclass(Animal, Puppy):
        total = total + 100
    return total


def make_animal(kind: int) -> Animal:
    if kind == 1:
        return Dog(0)
    return Animal(2)


def runtime_type_check() -> int:
    """The static type is Animal, but the instance tag knows it's a Dog."""
    a = make_animal(1)
    if isinstance(a, Dog):
        return 1
    return 0
