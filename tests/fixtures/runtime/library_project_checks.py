

# ---------------------------------------------------------------------------
# Runtime checks, appended to a copy of examples/library_project/main.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# See shopping_cart_checks.py for why these are exported functions rather than
# host-side assertions. Every value is what CPython answers for the same
# source. The copy is what gets the suffix, so examples/library_project/ stays
# the program a reader sees.


def runtime_check_catalog_size() -> int:
    if catalog_size() == 3:
        return 1
    return 0


def runtime_check_total_copies() -> int:
    if total_copies() == 6:
        return 1
    return 0


def runtime_check_gibson_titles() -> int:
    if gibson_titles() == 2:
        return 1
    return 0


def runtime_check_oldest() -> int:
    if oldest() == 1965:
        return 1
    return 0


def runtime_check_borrow_flow() -> int:
    if borrow_flow() == 4:
        return 1
    return 0


def runtime_check_first_line() -> int:
    if first_line() == "Dune by Herbert (1965)":
        return 1
    return 0


def runtime_check_shared_class_roundtrip() -> int:
    if shared_class_roundtrip() == "Solaris by Lem: 3 of 4":
        return 1
    return 0


def runtime_check_negative_first_line() -> int:
    if first_line() == "Dune by Herbert":
        return 1
    return 0
