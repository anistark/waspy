

# ---------------------------------------------------------------------------
# Runtime checks, appended to examples/expression_evaluator.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# Every value below is what CPython answers for the same source. A
# `runtime_check_negative_` function must answer 0.


def runtime_check_program() -> int:
    if program() == 14:
        return 1
    return 0


def runtime_check_precedence() -> int:
    if precedence() == 9:
        return 1
    return 0


def runtime_check_shown() -> int:
    if shown() == "(1 + (2 * (x - 3)))":
        return 1
    return 0


def runtime_check_token_count() -> int:
    if token_count() == 9:
        return 1
    return 0


def runtime_check_each_error_raises() -> int:
    for bad in ["1 / 0", "q + 1", "(1 + 2", "3 $ 4", "4 +"]:
        raised = 0
        try:
            run([bad])
        except CalcError:
            raised = 1
        if raised == 0:
            return 0
    return 1


def runtime_check_valid_input_does_not_raise() -> int:
    try:
        if run(["10 / 3"]) != 3:
            return 0
    except CalcError:
        return 0
    return 1


def runtime_check_negative_program() -> int:
    if program() == 15:
        return 1
    return 0
