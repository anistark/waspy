

# ---------------------------------------------------------------------------
# Runtime checks, appended to examples/oop_polymorphism.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# Every value below is what CPython answers for the same source. Virtual
# dispatch compiles to `call_indirect` through a funcref table, so these run
# the table under Node and the `wasmtime` CLI, optimized and unoptimized,
# rather than only under the in-process wasmi harness. A
# `runtime_check_negative_` function must answer 0.


def runtime_check_render_base() -> int:
    if render_base() == "none -> nobody":
        return 1
    return 0


def runtime_check_render_override() -> int:
    if render_email() != "email -> a@b.c":
        return 0
    if render_sms() != "sms -> 555":
        return 0
    return 1


def runtime_check_render_inherited_override() -> int:
    if render_inherited_override() == "sms -> 555":
        return 1
    return 0


def runtime_check_total_cost() -> int:
    diff = total_cost() - 0.29
    if diff < 0.0:
        diff = 0.0 - diff
    if diff < 0.000000001:
        return 1
    return 0


def runtime_check_priority_cost() -> int:
    diff = priority_cost() - 0.21
    if diff < 0.0:
        diff = 0.0 - diff
    if diff < 0.000000001:
        return 1
    return 0


def runtime_check_negative_render_override() -> int:
    if render_email() == "sms -> 555":
        return 1
    return 0
