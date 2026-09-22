"""Virtual dispatch (v0.17.0): a base method reaches the subclass's override.

A method call on an instance resolves against the object's *runtime* class, not
the class the variable is declared as, so the template-method pattern works: a
method written once in the base calls `self.something()` and each subclass
supplies its own. The same applies to `@property` getters and to `__eq__`
behind `==`.

Dispatch goes through a per-class table indexed by the class tag every instance
carries, so only a method some subclass actually overrides pays for it; every
other call is still a direct one. `super().method()` stays non-virtual, as in
Python, which is what lets an override extend its base rather than recurse.
"""

from typing import List


class Notification:
    def __init__(self, to: str):
        self.to = to

    def channel(self) -> str:
        return "none"

    def cost(self) -> float:
        return 0.0

    # Written once. Every subclass changes what it prints by overriding
    # channel(), not by overriding this.
    def render(self) -> str:
        return self.channel() + " -> " + self.to


class Email(Notification):
    def channel(self) -> str:
        return "email"

    def cost(self) -> float:
        return 0.01


class SMS(Notification):
    def channel(self) -> str:
        return "sms"

    def cost(self) -> float:
        return 0.07


class Priority(SMS):
    # Inherits SMS.channel(), and extends its cost through super().
    def cost(self) -> float:
        return super().cost() * 3.0


def render_email() -> str:
    return Email("a@b.c").render()


def render_sms() -> str:
    return SMS("555").render()


def render_inherited_override() -> str:
    # Priority defines no channel() of its own, so it reaches SMS's.
    return Priority("555").render()


def render_base() -> str:
    return Notification("nobody").render()


def total_cost() -> float:
    # One list, three runtime classes, one call site.
    queue: List[Notification] = [Email("a"), SMS("b"), Priority("c")]
    total = 0.0
    for item in queue:
        total = total + item.cost()
    return total


def cost_through_a_parameter(n: Notification) -> float:
    return n.cost()


def priority_cost() -> float:
    return cost_through_a_parameter(Priority("x"))


if __name__ == "__main__":
    print(render_base(), render_email(), render_sms(), render_inherited_override())
    print(total_cost(), priority_cost())
