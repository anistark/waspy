"""A shopping-cart domain model, written as ordinary Python.

This is a whole program rather than a feature demo: nothing here was chosen to
show off a compiler feature. It is the first of the end-to-end programs the
0.16.0 readiness work is built around, and it composes features that each
already worked alone: a class holding a list of other instances, a method
calling another object's method, float money arithmetic, and a plain rules
function over the result.

Composition is where the interesting defects live. The first time this program
was compiled it hit three in a row (a Binaryen process abort, a silently wrong
0.0, and a trap), none of which any single-feature test had caught.
"""


class Item:
    def __init__(self, name: str, price: float, qty: int):
        self.name = name
        self.price = price
        self.qty = qty

    def subtotal(self) -> float:
        return self.price * self.qty


class Cart:
    def __init__(self):
        self.items = []
        self.count = 0

    def add(self, name: str, price: float, qty: int):
        item = Item(name, price, qty)
        self.items.append(item)
        self.count = self.count + 1

    def total(self) -> float:
        out: float = 0.0
        for it in self.items:
            out = out + it.subtotal()
        return out

    def units(self) -> int:
        n: int = 0
        for it in self.items:
            n = n + it.qty
        return n


def discount_rate(total: float) -> float:
    if total >= 100.0:
        return 0.10
    if total >= 50.0:
        return 0.05
    return 0.0


def checkout() -> float:
    cart = Cart()
    cart.add("widget", 9.99, 3)
    cart.add("gadget", 24.50, 2)
    cart.add("doohickey", 5.00, 4)
    subtotal = cart.total()
    rate = discount_rate(subtotal)
    return subtotal - subtotal * rate


def item_count() -> int:
    cart = Cart()
    cart.add("a", 1.0, 2)
    cart.add("b", 2.0, 3)
    return cart.count * 100 + cart.units()


def empty_cart_total() -> float:
    cart = Cart()
    return cart.total()
