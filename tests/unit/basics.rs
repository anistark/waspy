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
