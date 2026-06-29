//! Integration coverage for the bundled `examples/*.py`.
//!
//! Tiers:
//!   1. Every standalone example must compile to a valid, instantiable WASM
//!      module. This is the "invalid / unrunnable module" class of regression
//!      that the 0.10.0 correctness pass fixed; the sweep keeps it fixed and any
//!      new example is covered automatically.
//!   2. Multi-file compilation produces a valid module and cross-file calls run.
//!   3. Two codegen defects this harness surfaced — a `str`-typed function
//!      parameter compared with `==`, and `raise ExceptionType(arg)` — have
//!      dedicated regression tests.
//!   4. The 0.11.0 headline feature — `break` / `continue` — asserts concrete
//!      runtime results by calling the exported functions of
//!      `examples/loop_control.py`.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{
    call_i32, call_i32_2, example_python_files, instantiate_wasm, read_example, try_compile,
    try_compile_multi, try_instantiate, MULTI_FILE_ONLY,
};

/// Every standalone-compilable example compiles, validates, and instantiates.
/// Multi-file examples are excluded (and covered by their own test below).
/// Failures are collected so the report lists every broken example, not just
/// the first.
#[test]
fn all_examples_compile_and_instantiate() {
    let mut failures = Vec::new();
    for path in example_python_files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if MULTI_FILE_ONLY.contains(&name.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read example file");
        let result = try_compile(&source).and_then(|wasm| try_instantiate(&wasm));
        if let Err(err) = result {
            failures.push(format!("{name}: {err}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} example(s) failed to compile + instantiate:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// The multi-file demo compiles `basic_operations.py` + `calculator.py` into a
/// single module (calculator depends on the other file) and runs functions that
/// cross the file boundary. `complex_calculation(x, y)` computes
/// `(x + y) * (x - y)`, so `(5, 3) == 16`; `calculate_factorial(5) == 120`
/// exercises cross-file recursion.
#[test]
fn calculator_multi_file_compiles_and_runs() {
    let basic = read_example("basic_operations.py");
    let calculator = read_example("calculator.py");
    let wasm = try_compile_multi(&[
        ("basic_operations.py", &basic),
        ("calculator.py", &calculator),
    ])
    .expect("multi-file compilation");
    let (instance, mut store) = instantiate_wasm(&wasm);
    let complex = instance
        .get_typed_func::<(i32, i32), i32>(&store, "complex_calculation")
        .expect("exported complex_calculation");
    assert_eq!(complex.call(&mut store, (5, 3)).expect("call"), 16);
    let factorial = instance
        .get_typed_func::<i32, i32>(&store, "calculate_factorial")
        .expect("exported calculate_factorial");
    assert_eq!(factorial.call(&mut store, 5).expect("call"), 120);
}

/// Regression test for the `str`-parameter bug `calculator.py` surfaced: a
/// function with a `str` parameter compared via `==`, called with a string
/// literal. Both layers must work — the call narrows the string argument to its
/// offset word, and the callee recovers the length from the blob prefix to run
/// the byte-for-byte comparison. `classify` returns 1 for "add", 2 for "sub",
/// 0 otherwise; the no-arg entry points make it callable without host-side
/// string marshalling.
#[test]
fn str_parameter_equality_runs() {
    let src = "def classify(op: str) -> int:\n    if op == \"add\":\n        return 1\n    if op == \"sub\":\n        return 2\n    return 0\n\ndef check_add() -> int:\n    return classify(\"add\")\n\ndef check_sub() -> int:\n    return classify(\"sub\")\n\ndef check_other() -> int:\n    return classify(\"xyz\")\n";
    assert_eq!(call_i32(src, "check_add"), 1);
    assert_eq!(call_i32(src, "check_sub"), 2);
    assert_eq!(call_i32(src, "check_other"), 0);
}

/// Regression test for the `raise ExceptionType(arg)` bug `exceptions.py`
/// surfaced: raising a built-in exception constructed with an argument
/// (`raise ValueError("never")`) must not leave the argument on the stack. The
/// exception is resolved to its type code by name, so the module is valid and
/// instantiates. The `try` returns 7 before the (never-taken) handler.
#[test]
fn raise_with_argument_is_valid() {
    let src = "def guard() -> int:\n    try:\n        return 7\n    except ValueError:\n        raise ValueError(\"never\")\n    finally:\n        done = 1\n    return 0\n";
    assert_eq!(call_i32(src, "guard"), 7);
}

/// `break` exits the loop early: summing `range(100)` but breaking at `i == 5`
/// yields 0 + 1 + 2 + 3 + 4 = 10.
#[test]
fn break_exits_loop_early() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32(&src, "sum_until_five"), 10);
}

/// `continue` skips the rest of the body: summing the odd numbers below ten
/// yields 1 + 3 + 5 + 7 + 9 = 25.
#[test]
fn continue_skips_iteration() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32(&src, "sum_odds_below_ten"), 25);
}

/// `break` / `continue` inside a `while True` loop: the first multiple of 3
/// strictly greater than 10 is 12.
#[test]
fn break_continue_in_while_loop() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32_2(&src, "first_multiple_over", 10, 3), 12);
}

/// `break` exits only the innermost loop: the inner loop breaks at `j == 1`
/// after one increment, across three outer iterations, so the count is 3.
#[test]
fn break_exits_innermost_loop_only() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32(&src, "count_inner_breaks"), 3);
}

/// Statically nested list-of-lists: each inner literal occupies its own region,
/// so `grid[0][1] + grid[1][0]` reads 2 + 3 = 5 (Issue #14).
#[test]
fn nested_list_indexing() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "nested_grid"), 5);
}

/// A list literal built inside a loop that escapes must get a fresh region per
/// iteration, allocated from the runtime heap rather than the one compile-time
/// region every iteration would otherwise share. `grid[0][0]` stays 0 and
/// `grid[2][0]` is 2, so the result is 0*100 + 2 = 2; aliasing would give 202
/// (every row pointing at the last iteration's data). Issue #14.
#[test]
fn per_iteration_collection_does_not_alias() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "loop_escape"), 2);
}

/// Float dict values round-trip through the one-word slot (stored as f32 and
/// widened back to f64 on read): `d[1] + d[2]` = 3.5 + 7.5 = 11.0, truncated to
/// 11. Before this fix the dict store emitted an `i32.store` of an `f64`,
/// producing an invalid module.
#[test]
fn float_dict_values_round_trip() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "float_dict_sum"), 11);
}

/// Float set members de-duplicate by value: `{1.5, 1.5, 2.5}` has two distinct
/// members. The float is narrowed to its f32 bit pattern so the i32 dedup
/// search compares it correctly (and the module stays valid).
#[test]
fn float_set_members_dedup() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "float_set_size"), 2);
}
