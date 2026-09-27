

# ---------------------------------------------------------------------------
# Runtime checks, appended to examples/order_ledger.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# Every value below is what CPython answers for the same source. A
# `runtime_check_negative_` function must answer 0.


def runtime_check_scenario() -> int:
    expected = "ann: 2 lines, 30.60; bob: 3 lines, 61.99; failed 10 1; stock 25.65"
    if scenario() == expected:
        return 1
    return 0


def runtime_check_remaining_apples() -> int:
    if remaining_apples() == 30:
        return 1
    return 0


def runtime_check_loyal_threshold() -> int:
    if loyal_threshold() == 949909000:
        return 1
    return 0


def runtime_check_negative_remaining_apples() -> int:
    if remaining_apples() == 31:
        return 1
    return 0
