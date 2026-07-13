# Closures: lambdas with full variable capture (#43). A lambda is lifted to a
# real WASM function; the closure value is a heap environment holding the
# captured variables (copied at creation) plus the dispatch-table slot, and
# calls go through call_indirect. Each function returns an i32 checked by the
# integration tests.

square = lambda x: x * x


def make_adder(n: int):
    return lambda x: x + n


def returned_closure_reads_capture() -> int:
    add5 = make_adder(5)
    return add5(3)  # 8


def closure_captures_local() -> int:
    base = 100
    f = lambda x: x + base
    return f(23)  # 123


def closures_capture_independently() -> int:
    a2 = make_adder(2)
    a7 = make_adder(7)
    return a2(1) * 100 + a7(1)  # 308


def lambda_without_capture() -> int:
    double = lambda x: x * 2
    return double(21)  # 42


def lambda_with_two_params() -> int:
    mul = lambda a, b: a * b
    return mul(6, 7)  # 42


def apply(f, x: int) -> int:
    return f(x)


def closure_passed_as_argument() -> int:
    return apply(make_adder(10), 5)  # 15


def make_const(v: int):
    return lambda: v


def zero_argument_closure() -> int:
    c = make_const(99)
    return c()  # 99


def nested_lambda_captures_param() -> int:
    add = lambda x: lambda y: x + y
    add3 = add(3)
    return add3(4)  # 7


def module_level_lambda() -> int:
    return square(6)  # 36


def helper(v: int) -> int:
    return v + 1


def lambda_calls_module_function() -> int:
    f = lambda x: helper(x) * 2
    return f(4)  # 10


def make_off(n: int):
    return lambda x: x + n


def closures_built_in_comprehension() -> int:
    fs = [make_off(i) for i in range(3)]
    f0 = fs[0]
    f2 = fs[2]
    return f0(10) + f2(10)  # 22
