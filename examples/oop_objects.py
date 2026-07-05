"""Heap-allocated object instances (v0.13.0 P0).

Each `ClassName(...)` calls the runtime allocator and returns a distinct
instance pointer, so multiple instances of one class coexist with
independent state. Methods operate on the `self` pointer they are passed.
"""


class Counter:
    def __init__(self, start: int):
        self.value = start

    def increment(self) -> int:
        self.value = self.value + 1
        return self.value

    def add(self, amount: int) -> int:
        self.value += amount
        return self.value


class Point:
    def __init__(self, x: float, y: float):
        self.x = x
        self.y = y

    def scale(self, factor: float) -> float:
        self.x = self.x * factor
        self.y = self.y * factor
        return self.x + self.y


def two_counters_independent() -> int:
    """Two live instances mutate independently: 12 * 100 + 103 = 1303."""
    a = Counter(10)
    b = Counter(100)
    a.increment()
    a.increment()
    b.add(3)
    return a.value * 100 + b.value


def fresh_instance_per_call() -> int:
    """Each instantiation starts from its own zeroed heap block: 1 + 1 = 2."""
    first = Counter(0)
    first.increment()
    second = Counter(0)
    second.increment()
    return first.value + second.value


def make_counter(start: int) -> Counter:
    """Factory: the instance pointer survives the return."""
    c = Counter(start)
    return c


def counter_from_factory() -> int:
    c = make_counter(41)
    c.increment()
    return c.value


def bump(c: Counter, times: int) -> int:
    """Instances pass as arguments; mutation is visible to the caller."""
    i = 0
    while i < times:
        c.increment()
        i = i + 1
    return c.value


def counter_as_argument() -> int:
    c = Counter(5)
    bump(c, 4)
    return c.value


def instances_in_a_list() -> int:
    """Instances are first-class: store pointers in a list, read them back."""
    a = Counter(7)
    b = Counter(30)
    pair = [a, b]
    first = pair[0]
    second = pair[1]
    first.add(5)
    return first.value + second.value


def instances_in_a_tuple() -> int:
    """Tuple slots hold instance pointers too: 11 + 31 = 42."""
    a = Counter(11)
    b = Counter(31)
    pair = (a, b)
    x = pair[0]
    y = pair[1]
    return x.value + y.value


def instances_in_a_dict() -> int:
    """Dict values hold instance pointers: 40 + 2 = 42."""
    a = Counter(40)
    b = Counter(2)
    d = {1: a, 2: b}
    x = d[1]
    y = d[2]
    return x.value + y.value


def float_fields_two_instances() -> float:
    """Float fields stay per-instance: p keeps 1.5+2.5 scaled by 2 = 8.0,
    q holds 10.0+20.0 = 30.0, so the total is 38.0."""
    p = Point(1.5, 2.5)
    q = Point(10.0, 20.0)
    p.scale(2.0)
    return p.x + p.y + q.x + q.y
