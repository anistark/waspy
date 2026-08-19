//! Unit-level coverage for the basic language operations (good-first-issue
//! item 4): arithmetic, comparisons, boolean logic, bitwise operators, and
//! type conversions. Each test compiles a minimal snippet and asserts the
//! computed value, so a codegen regression in a single operator fails a
//! single, obvious test.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{call_f64, call_i32, call_i32_2};

/// The integer binary operators produce Python's results.
#[test]
fn integer_arithmetic() {
    let src = "def f(a: int, b: int) -> int:\n    return a + b\n";
    assert_eq!(call_i32_2(src, "f", 17, 25), 42);
    let src = "def f(a: int, b: int) -> int:\n    return a - b\n";
    assert_eq!(call_i32_2(src, "f", 17, 25), -8);
    let src = "def f(a: int, b: int) -> int:\n    return a * b\n";
    assert_eq!(call_i32_2(src, "f", -6, 7), -42);
    let src = "def f(a: int, b: int) -> int:\n    return a // b\n";
    assert_eq!(call_i32_2(src, "f", 42, 5), 8);
    let src = "def f(a: int, b: int) -> int:\n    return a % b\n";
    assert_eq!(call_i32_2(src, "f", 42, 5), 2);
}

/// Float arithmetic runs at f64 width; the chosen values are exactly
/// representable, so equality is exact.
#[test]
fn float_arithmetic() {
    let src = "def f() -> float:\n    return 1.5 + 2.25\n";
    assert_eq!(call_f64(src, "f"), 3.75);
    let src = "def f() -> float:\n    return 10.5 - 0.25\n";
    assert_eq!(call_f64(src, "f"), 10.25);
    let src = "def f() -> float:\n    return 2.5 * 4.0\n";
    assert_eq!(call_f64(src, "f"), 10.0);
    let src = "def f() -> float:\n    return 10.0 / 4.0\n";
    assert_eq!(call_f64(src, "f"), 2.5);
}

/// Every comparison operator over ints answers both directions.
#[test]
fn integer_comparisons() {
    for (op, lt, eq, gt) in [
        ("<", 1, 0, 0),
        ("<=", 1, 1, 0),
        (">", 0, 0, 1),
        (">=", 0, 1, 1),
        ("==", 0, 1, 0),
        ("!=", 1, 0, 1),
    ] {
        let src = format!("def f(a: int, b: int) -> bool:\n    return a {op} b\n");
        assert_eq!(call_i32_2(&src, "f", 1, 2), lt, "1 {op} 2");
        assert_eq!(call_i32_2(&src, "f", 2, 2), eq, "2 {op} 2");
        assert_eq!(call_i32_2(&src, "f", 3, 2), gt, "3 {op} 2");
    }
}

/// and/or short-circuit and `not` inverts.
#[test]
fn boolean_logic() {
    let src = "def f(a: int, b: int) -> int:\n    if (a > 0) and (b > 0):\n        return 1\n    return 0\n";
    assert_eq!(call_i32_2(src, "f", 1, 1), 1);
    assert_eq!(call_i32_2(src, "f", 1, 0), 0);
    let src = "def f(a: int, b: int) -> int:\n    if (a > 0) or (b > 0):\n        return 1\n    return 0\n";
    assert_eq!(call_i32_2(src, "f", 0, 1), 1);
    assert_eq!(call_i32_2(src, "f", 0, 0), 0);
    let src =
        "def f(a: int, b: int) -> int:\n    if not (a > b):\n        return 1\n    return 0\n";
    assert_eq!(call_i32_2(src, "f", 1, 2), 1);
    assert_eq!(call_i32_2(src, "f", 2, 1), 0);
}

/// The bitwise operators and shifts.
#[test]
fn bitwise_operators() {
    let src = "def f(a: int, b: int) -> int:\n    return a & b\n";
    assert_eq!(call_i32_2(src, "f", 0b1100, 0b1010), 0b1000);
    let src = "def f(a: int, b: int) -> int:\n    return a | b\n";
    assert_eq!(call_i32_2(src, "f", 0b1100, 0b1010), 0b1110);
    let src = "def f(a: int, b: int) -> int:\n    return a ^ b\n";
    assert_eq!(call_i32_2(src, "f", 0b1100, 0b1010), 0b0110);
    let src = "def f(a: int, b: int) -> int:\n    return a << b\n";
    assert_eq!(call_i32_2(src, "f", 3, 4), 48);
    let src = "def f(a: int, b: int) -> int:\n    return a >> b\n";
    assert_eq!(call_i32_2(src, "f", 48, 4), 3);
}

/// Unary negation over ints and floats.
#[test]
fn unary_negation() {
    let src = "def f(a: int, b: int) -> int:\n    return -a + b\n";
    assert_eq!(call_i32_2(src, "f", 7, 0), -7);
    let src = "def f() -> float:\n    x = 2.5\n    return -x\n";
    assert_eq!(call_f64(src, "f"), -2.5);
}

/// int() truncates toward zero; float() widens; bool() tests truthiness.
#[test]
fn type_conversions() {
    let src = "def f() -> int:\n    return int(3.7)\n";
    assert_eq!(call_i32(src, "f"), 3);
    let src = "def f() -> float:\n    return float(5)\n";
    assert_eq!(call_f64(src, "f"), 5.0);
    let src = "def f() -> int:\n    if bool(7):\n        return 1\n    return 0\n";
    assert_eq!(call_i32(src, "f"), 1);
    let src = "def f() -> int:\n    if bool(0):\n        return 1\n    return 0\n";
    assert_eq!(call_i32(src, "f"), 0);
}

/// Mixed int/float expressions widen the int operand to f64.
#[test]
fn mixed_arithmetic_widens() {
    let src = "def f() -> float:\n    a = 2\n    b = 1.25\n    return a + b\n";
    assert_eq!(call_f64(src, "f"), 3.25);
    let src = "def f() -> float:\n    return 3 * 0.5\n";
    assert_eq!(call_f64(src, "f"), 1.5);
}

/// Augmented assignment updates in place for each operator kind.
#[test]
fn augmented_assignment() {
    let src = "def f(a: int, b: int) -> int:\n    x = a\n    x += b\n    x *= 2\n    x -= 1\n    return x\n";
    // (10 + 5) * 2 - 1 = 29.
    assert_eq!(call_i32_2(src, "f", 10, 5), 29);
    let src = "def f() -> float:\n    x = 8.0\n    x /= 2.0\n    return x\n";
    assert_eq!(call_f64(src, "f"), 4.0);
}

/// Operator precedence and parentheses group as in Python.
#[test]
fn precedence_and_grouping() {
    let src = "def f(a: int, b: int) -> int:\n    return a + b * 2\n";
    assert_eq!(call_i32_2(src, "f", 1, 3), 7);
    let src = "def f(a: int, b: int) -> int:\n    return (a + b) * 2\n";
    assert_eq!(call_i32_2(src, "f", 1, 3), 8);
}

// ---------------------------------------------------------------------------
// Arithmetic helpers: division by zero, power, and float modulo.
//
// The power and float-modulo helpers used to write to WASM locals 0, 1, and 2
// outright, which are the function's first parameters: `a ** b` clobbered `a`,
// and in a function whose first locals were not the width the helper assumed,
// the module failed to validate. Float modulo also subtracted the wrong way
// round, and float power returned from the *enclosing function* for its
// special cases and answered the base itself for a fractional exponent.
// ---------------------------------------------------------------------------

/// Division by zero raises ZeroDivisionError instead of trapping (integers) or
/// answering inf (floats), and it is catchable and propagates out of calls.
#[test]
fn division_by_zero_raises() {
    let cases: &[&str] = &[
        "def f() -> int:\n    n = 0\n    try:\n        return 10 // n\n    except ZeroDivisionError:\n        return 5\n",
        "def f() -> int:\n    n = 0\n    try:\n        return 10 % n\n    except ZeroDivisionError:\n        return 5\n",
        "def f() -> int:\n    n = 0\n    try:\n        return 10 / n\n    except ZeroDivisionError:\n        return 5\n",
    ];
    for src in cases {
        assert_eq!(call_i32(src, "f"), 5, "in: {src}");
    }

    let float_div = "def f() -> float:\n\
                     \x20   d = 0.0\n\
                     \x20   try:\n\
                     \x20       return 1.5 / d\n\
                     \x20   except ZeroDivisionError:\n\
                     \x20       return 5.0\n";
    assert_eq!(call_f64(float_div, "f"), 5.0);

    let float_mod = "def f() -> float:\n\
                     \x20   d = 0.0\n\
                     \x20   try:\n\
                     \x20       return 1.5 % d\n\
                     \x20   except ZeroDivisionError:\n\
                     \x20       return 5.0\n";
    assert_eq!(call_f64(float_mod, "f"), 5.0);

    let through_a_call = "def half(n: int, d: int) -> int:\n\
                          \x20   return n // d\n\
                          \n\
                          def f() -> int:\n\
                          \x20   try:\n\
                          \x20       return half(10, 0)\n\
                          \x20   except ZeroDivisionError:\n\
                          \x20       return 5\n";
    assert_eq!(call_i32(through_a_call, "f"), 5);

    // A nonzero literal divisor needs no guard, and still divides.
    assert_eq!(call_i32("def f() -> int:\n    return 10 // 2\n", "f"), 5);
    assert_eq!(
        call_i32("def f() -> int:\n    n = 3\n    return 10 % n\n", "f"),
        1
    );
}

/// `**` computes, and leaves its operands alone: reading a parameter after
/// raising it to a power gives the parameter, not the result.
#[test]
fn power_does_not_clobber_its_operands() {
    let integer = "def g(a: int, b: int) -> int:\n\
                   \x20   p = a ** b\n\
                   \x20   return p + a\n\
                   \n\
                   def f() -> int:\n\
                   \x20   return g(2, 3)\n";
    assert_eq!(call_i32(integer, "f"), 10);

    let float_power = "def g(a: float, b: float) -> float:\n\
                       \x20   p = a ** b\n\
                       \x20   return p + a\n\
                       \n\
                       def f() -> float:\n\
                       \x20   return g(2.0, 3.0)\n";
    assert_eq!(call_f64(float_power, "f"), 10.0);

    // Exponent 0 is 1, and a negative float exponent is the reciprocal.
    assert_eq!(
        call_i32("def f() -> int:\n    n = 0\n    return 5 ** n\n", "f"),
        1
    );
    assert_eq!(
        call_f64(
            "def f() -> float:\n    b = 0.0 - 2.0\n    return 2.0 ** b\n",
            "f"
        ),
        0.25
    );
}

/// Float modulo follows Python's sign convention and reads back the way round
/// it should: `3.5 % 2.0` is 1.5, and `-1.5 % 2.0` is 0.5.
#[test]
fn float_modulo_matches_python() {
    assert_eq!(
        call_f64("def f() -> float:\n    d = 2.0\n    return 3.5 % d\n", "f"),
        1.5
    );
    let negative = "def g(a: float, b: float) -> float:\n\
                    \x20   return a % b\n\
                    \n\
                    def f() -> float:\n\
                    \x20   return g(0.0 - 1.5, 2.0)\n";
    assert_eq!(call_f64(negative, "f"), 0.5);
}

/// `str()` renders an integer's digits at runtime. The IR converter used to
/// erase the call and leave the argument in its place, so `str(123)` *was* the
/// integer 123: `len(str(n))` answered 0, comparing the result to a literal
/// never matched, and concatenating it produced the wrong string. Anything it
/// cannot render (a bool would come out "1" rather than Python's "True", a
/// float needs a formatter this runtime lacks) is a compile error rather than
/// an empty string.
#[test]
fn str_of_an_int_renders_its_digits() {
    assert_eq!(
        call_i32("def f() -> int:\n    return len(str(123))\n", "f"),
        3
    );
    assert_eq!(
        call_i32(
            "def f() -> int:\n    a = 1000000\n    return len(str(a))\n",
            "f"
        ),
        7
    );
    // The '-' counts, and i32::MIN renders through the unsigned magnitude.
    assert_eq!(
        call_i32(
            "def f() -> int:\n    a = 0 - 123\n    return len(str(a))\n",
            "f"
        ),
        4
    );
    // The rendered digits compare equal to the same literal.
    let compared = "def f() -> int:\n\
                    \x20   s = str(123)\n\
                    \x20   if s == \"123\":\n\
                    \x20       return 1\n\
                    \x20   return 0\n";
    assert_eq!(call_i32(compared, "f"), 1);
    // And concatenate: "x=" + "12".
    assert_eq!(
        call_i32("def f() -> int:\n    return len(\"x=\" + str(12))\n", "f"),
        4
    );
    // A string passes straight through.
    assert_eq!(
        call_i32("def f() -> int:\n    return len(str(\"abc\"))\n", "f"),
        3
    );

    let of_a_bool = harness::try_compile("def f() -> int:\n    return len(str(True))\n")
        .expect_err("str() of a bool is not supported");
    assert!(
        of_a_bool.contains("str() of bool"),
        "unexpected: {of_a_bool}"
    );
}

/// `int` is a 32-bit two's-complement integer, so arithmetic that leaves its
/// range wraps rather than growing the way CPython's arbitrary-precision `int`
/// does. This is the one documented place where a compiled program answers
/// differently without saying so (see the "Numbers" section of `README.md`),
/// and these assertions are here to keep it deliberate: if the representation
/// ever changes, they should be updated on purpose, not discovered.
#[test]
fn integers_are_32_bit_and_wrap() {
    // Inside the range, Python's answers.
    assert_eq!(
        call_i32(
            "def f() -> int:\n    a = 1000000\n    b = 1000\n    return a * b\n",
            "f"
        ),
        1_000_000_000
    );
    assert_eq!(
        call_i32("def f() -> int:\n    return 2147483647\n", "f"),
        i32::MAX
    );

    // Outside it, two's-complement wraparound. CPython answers 1000000000000,
    // 2147483648, and 1099511627776 for these three.
    assert_eq!(
        call_i32(
            "def f() -> int:\n    a = 1000000\n    b = 1000000\n    return a * b\n",
            "f"
        ),
        1_000_000_000_000i64 as i32
    );
    assert_eq!(
        call_i32(
            "def f() -> int:\n    a = 2147483647\n    return a + 1\n",
            "f"
        ),
        i32::MIN
    );
    assert_eq!(
        call_i32("def f() -> int:\n    n = 40\n    return 2 ** n\n", "f"),
        0
    );
}
