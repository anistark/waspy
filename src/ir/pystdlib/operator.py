# operator, the functions over the operators (Lib/operator.py).


def add(a, b):
    return a + b


def sub(a, b):
    return a - b


def mul(a, b):
    return a * b


def truediv(a, b):
    return a / b


def floordiv(a, b):
    return a // b


def mod(a, b):
    return a % b


def pow(a, b):
    return a ** b


def neg(a):
    return -a


def pos(a):
    return +a


def abs(a):
    return a if a >= 0 else -a


def not_(a) -> bool:
    return not a


def truth(a) -> bool:
    return bool(a)


def eq(a, b) -> bool:
    return a == b


def ne(a, b) -> bool:
    return a != b


def lt(a, b) -> bool:
    return a < b


def le(a, b) -> bool:
    return a <= b


def gt(a, b) -> bool:
    return a > b


def ge(a, b) -> bool:
    return a >= b


def and_(a, b):
    return a & b


def or_(a, b):
    return a | b


def xor(a, b):
    return a ^ b


def concat(a, b):
    return a + b


def contains(a, b) -> bool:
    return b in a


def getitem(a, b):
    return a[b]


def countOf(a, b) -> int:
    count = 0
    for item in a:
        if item == b:
            count += 1
    return count


def indexOf(a, b) -> int:
    i = 0
    for item in a:
        if item == b:
            return i
        i += 1
    raise ValueError("sequence.index(x): x not in sequence")


# itemgetter, attrgetter, and methodcaller are written as the lambdas they make
# where the program calls them (ir::pystdlib).


def itemgetter(*items: int) -> int:
    raise NotImplementedError("operator.itemgetter is written as a lambda where it is called")


def attrgetter(*attrs: str) -> int:
    raise NotImplementedError("operator.attrgetter takes attribute names written in the program")


def methodcaller(name: str) -> int:
    raise NotImplementedError("operator.methodcaller takes a method name written in the program")
