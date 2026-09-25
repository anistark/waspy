"""An order ledger: stock, orders, discounts, and a report.

The sixth end-to-end program, and the one the correctness rule was re-signed
on. It was written as ordinary Python, without reference to what waspy
supports, and compiled unchanged. It found no silent defect: its three stops
were loud refusals (an annotated field, `self.items[sku].qty += n`, and
`round()`), each since implemented rather than worked around.

The shape: a dataclass held in a dict keyed by string, a custom exception
hierarchy caught by subclass, an overridden `discount()` reached through an
inherited `total()`, float money rounded half-to-even the way CPython rounds,
and a formatted report.
"""

from dataclasses import dataclass
from typing import Dict, List


class LedgerError(Exception):
    pass


class OutOfStock(LedgerError):
    pass


class UnknownItem(LedgerError):
    pass


@dataclass
class Item:
    sku: str
    price: float
    qty: int


class Inventory:
    def __init__(self):
        self.items: Dict[str, Item] = {}

    def add(self, sku: str, price: float, qty: int):
        if sku in self.items:
            self.items[sku].qty += qty
        else:
            self.items[sku] = Item(sku, price, qty)

    def take(self, sku: str, qty: int) -> float:
        if sku not in self.items:
            raise UnknownItem()
        item = self.items[sku]
        if item.qty < qty:
            raise OutOfStock()
        item.qty -= qty
        return item.price * qty

    def value(self) -> float:
        total = 0.0
        for sku in self.items:
            item = self.items[sku]
            total += item.price * item.qty
        return total


class Order:
    def __init__(self, customer: str):
        self.customer = customer
        self.lines: List[str] = []
        self.amount = 0.0

    def discount(self) -> float:
        return 0.0

    def total(self) -> float:
        return round(self.amount * (1.0 - self.discount()), 2)


class LoyalOrder(Order):
    def discount(self) -> float:
        if self.amount >= 100.0:
            return 0.1
        return 0.05


def place(inv: Inventory, order: Order, wanted: List[tuple]) -> int:
    failed = 0
    for sku, qty in wanted:
        try:
            order.amount += inv.take(sku, qty)
            order.lines.append(sku)
        except OutOfStock:
            failed += 1
        except UnknownItem:
            failed += 10
    return failed


def stocked() -> Inventory:
    inv = Inventory()
    inv.add("apple", 0.5, 100)
    inv.add("bread", 2.25, 10)
    inv.add("cheese", 7.8, 5)
    inv.add("apple", 0.5, 20)
    return inv


def scenario() -> str:
    inv = stocked()
    a = Order("ann")
    b = LoyalOrder("bob")
    fa = place(inv, a, [("apple", 30), ("cheese", 2), ("durian", 1)])
    fb = place(inv, b, [("bread", 8), ("cheese", 4), ("apple", 90), ("bread", 1)])
    lines = []
    for order in [a, b]:
        lines.append(f"{order.customer}: {len(order.lines)} lines, {order.total():.2f}")
    lines.append(f"failed {fa} {fb}")
    lines.append(f"stock {inv.value():.2f}")
    return "; ".join(lines)


def remaining_apples() -> int:
    inv = stocked()
    place(inv, Order("x"), [("apple", 45), ("apple", 45)])
    return inv.items["apple"].qty


def loyal_threshold() -> int:
    small = LoyalOrder("s")
    small.amount = 99.99
    big = LoyalOrder("b")
    big.amount = 100.0
    return int(small.total() * 100) * 100000 + int(big.total() * 100)


if __name__ == "__main__":
    print(scenario())
    print(remaining_apples(), loyal_threshold())
