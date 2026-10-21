export const EXAMPLES = [
    {
        id: 'fibonacci',
        name: 'Fibonacci & primes',
        code: `def fib(n: int) -> int:
    """Recursive Fibonacci."""
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


def fib_iter(n: int) -> int:
    a, b = 0, 1
    for _ in range(n):
        a, b = b, a + b
    return a


def is_prime(n: int) -> bool:
    if n < 2:
        return False
    i = 2
    while i * i <= n:
        if n % i == 0:
            return False
        i += 1
    return True
`,
    },
    {
        id: 'strings',
        name: 'Strings',
        code: `from typing import List


def normalize(word: str) -> str:
    return word.strip(".,!?").lower()


def words(text: str) -> List[str]:
    out: List[str] = []
    for raw in text.split():
        out.append(normalize(raw))
    return out


def slug() -> str:
    return "-".join(words("  Python, to WebAssembly!  "))


def count_vowels() -> int:
    total = 0
    for ch in "WebAssembly from Python".lower():
        if ch in "aeiou":
            total += 1
    return total


def badge(n: int) -> str:
    label = "items"
    if n == 1:
        label = "item"
    return f"{n} {label}".upper().center(12)
`,
    },
    {
        id: 'classes',
        name: 'Classes & inheritance',
        code: `class Shape:
    def area(self) -> float:
        return 0.0

    def name(self) -> str:
        return "shape"

    def describe(self) -> str:
        return f"{self.name()} with area {self.area():.2f}"


class Circle(Shape):
    def __init__(self, r: float):
        self.r = r

    def area(self) -> float:
        return 3.14159 * self.r * self.r

    def name(self) -> str:
        return "circle"


class Square(Shape):
    def __init__(self, side: float):
        self.side = side

    def area(self) -> float:
        return self.side * self.side

    def name(self) -> str:
        return "square"


def describe_circle(r: float) -> str:
    return Circle(r).describe()


def describe_square(side: float) -> str:
    return Square(side).describe()
`,
    },
    {
        id: 'collections',
        name: 'Collections',
        code: `from typing import Dict, List


def squares(n: int) -> int:
    return sum([i * i for i in range(1, n + 1)])


def word_count() -> int:
    counts: Dict[str, int] = {}
    for w in "the quick fox and the lazy dog and the cat".split():
        counts[w] = counts.get(w, 0) + 1
    return counts["the"]


def median(a: int, b: int, c: int) -> int:
    xs: List[int] = sorted([a, b, c])
    return xs[1]


def unique_count() -> int:
    return len({x % 7 for x in range(100)})
`,
    },
    {
        id: 'exceptions',
        name: 'Exceptions',
        code: `class InsufficientFunds(Exception):
    pass


class Account:
    def __init__(self, balance: int):
        self.balance = balance

    def withdraw(self, amount: int) -> int:
        if amount > self.balance:
            raise InsufficientFunds("not enough money")
        self.balance -= amount
        return self.balance


def try_withdraw(amount: int) -> str:
    acct = Account(100)
    try:
        left = acct.withdraw(amount)
        return f"ok, {left} left"
    except InsufficientFunds:
        return "declined"


def parse_or_default(fallback: int) -> int:
    try:
        return int("42x")
    except ValueError:
        return fallback
`,
    },
    {
        id: 'generators',
        name: 'Generators',
        code: `def countdown(n: int):
    while n > 0:
        yield n
        n -= 1


def evens(limit: int):
    for i in range(limit):
        if i % 2 == 0:
            yield i


def total_countdown(n: int) -> int:
    total = 0
    for x in countdown(n):
        total += x
    return total


def sum_evens(limit: int) -> int:
    total = 0
    for x in evens(limit):
        total += x
    return total
`,
    },
    {
        id: 'refused',
        name: 'A refused construct',
        code: `# Waspy gives Python's answer or refuses to compile.
# It never guesses: eval() has no compiled meaning,
# so this program is rejected with a located error.


def double(x: int) -> int:
    return x * 2


def dynamic(x: int) -> int:
    return eval("x + 1")
`,
    },
];
