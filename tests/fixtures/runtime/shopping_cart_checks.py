

# ---------------------------------------------------------------------------
# Runtime checks, appended to examples/shopping_cart.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# Every value below is what CPython answers for the same source, so these
# mirror the assertions in tests/integration/coverage.rs. They are written as
# exported functions returning 1 or 0 rather than as host-side assertions so
# that a runtime with no way to read the module's memory (the `wasmtime` CLI)
# checks exactly what Node checks. A `runtime_check_negative_` function must
# answer 0: it proves the comparison inside a checker can fail, so a checker
# answering 1 means something.


def runtime_check_checkout() -> int:
    total = checkout()
    diff = total - 94.0215
    if diff < 0.0:
        diff = 0.0 - diff
    if diff < 0.000000001:
        return 1
    return 0


def runtime_check_item_count() -> int:
    if item_count() == 205:
        return 1
    return 0


def runtime_check_empty_total() -> int:
    if empty_cart_total() == 0.0:
        return 1
    return 0


def runtime_check_discount_tiers() -> int:
    if discount_rate(120.0) != 0.10:
        return 0
    if discount_rate(100.0) != 0.10:
        return 0
    if discount_rate(99.99) != 0.05:
        return 0
    if discount_rate(50.0) != 0.05:
        return 0
    if discount_rate(49.99) != 0.0:
        return 0
    if discount_rate(0.0) != 0.0:
        return 0
    return 1


def runtime_check_negative_checkout() -> int:
    if checkout() == 98.97:
        return 1
    return 0
