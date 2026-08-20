# Closures: lambdas with full variable capture (#43). A lambda is lifted to a
# real WASM function; the closure value is a heap environment holding a pointer
# to each captured variable's cell plus the dispatch-table slot, and calls go
# through call_indirect. Capturing the cell rather than the value is what makes
# a closure see the variable's *current* contents, the way Python's do. Each
# function returns an i32 checked by the integration tests.

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


def reads_the_current_value() -> int:
    """Python closures capture the variable, not a snapshot of it: reassigning
    after the closure is made changes what the closure sees."""
    v = 1
    g = lambda: v
    v = 9
    return g()


def sees_updates_between_calls() -> int:
    """The same closure, called either side of an update, answers differently:
    11 the first time and 22 the second."""
    v = 11
    g = lambda: v
    first = g()
    v = 22
    return first + g()


def loop_closures_share_the_loop_variable() -> int:
    """Closures made in a loop share the one loop variable, so after the loop
    they all see its final value (2), not the value it had at their creation."""
    fs = []
    for i in range(3):
        fs.append(lambda: i)
    first = fs[0]
    last = fs[2]
    return first() * 10 + last()
