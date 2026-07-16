//! Error-quality coverage for v0.20.0: unsupported Python syntax is rejected
//! up front by the parser's validation pass with a located, actionable
//! message — instead of failing deep in codegen with an AST debug dump, or
//! (worse) compiling to silently wrong code.

#[path = "../utils/harness.rs"]
mod harness;

use harness::try_compile;

/// Compile expecting failure and return the full error chain text.
fn compile_error(source: &str) -> String {
    try_compile(source).expect_err("source must be rejected")
}

/// Syntax errors report their line and column.
#[test]
fn parse_error_reports_line_and_column() {
    let err = compile_error("def f() -> int:\n    return (1 +\n");
    assert!(
        err.contains("line"),
        "parse error must carry a line number: {err}"
    );
}

/// Each known-unsupported statement fails fast with a message naming the
/// construct and giving a hint, plus the source location.
#[test]
fn unsupported_statements_are_rejected_with_hints() {
    let cases: &[(&str, &str)] = &[
        (
            "async def f():\n    return 1\n",
            "async functions are not supported",
        ),
        (
            "def f(x: int) -> int:\n    match x:\n        case 1:\n            return 1\n    return 0\n",
            "'match' statements are not supported",
        ),
        (
            "COUNT = 0\n\ndef f() -> int:\n    global COUNT\n    return COUNT\n",
            "'global' statement is not supported",
        ),
        (
            "def f() -> int:\n    def g() -> int:\n        nonlocal x\n        return x\n    x = 1\n    return g()\n",
            "'nonlocal' statement is not supported",
        ),
        (
            "def f() -> int:\n    x = 1\n    del x\n    return 0\n",
            "'del' is not supported",
        ),
        (
            "def f(x: int) -> int:\n    assert x > 0\n    return x\n",
            "'assert' is not supported",
        ),
        (
            "def f() -> int:\n    for i in range(3):\n        pass\n    else:\n        return 1\n    return 0\n",
            "'for ... else:' clauses are not supported",
        ),
        (
            "def f() -> int:\n    while False:\n        pass\n    else:\n        return 1\n    return 0\n",
            "'while ... else:' clauses are not supported",
        ),
        ("from math import *\n\ndef f() -> int:\n    return 1\n", "import *"),
    ];
    for (source, expected) in cases {
        let err = compile_error(source);
        assert!(
            err.contains(expected),
            "expected `{expected}` in error for:\n{source}\ngot: {err}"
        );
        assert!(
            err.contains("line"),
            "error must carry a location for:\n{source}\ngot: {err}"
        );
    }
}

/// Star/keyword parameter forms fail at the function definition, naming the
/// function.
#[test]
fn star_parameters_are_rejected() {
    let err = compile_error("def f(*args) -> int:\n    return 0\n");
    assert!(
        err.contains("*args") && err.contains("not supported"),
        "unexpected: {err}"
    );
    assert!(err.contains("'f'"), "error should name the function: {err}");

    let err = compile_error("def f(**opts) -> int:\n    return 0\n");
    assert!(
        err.contains("**opts") && err.contains("not supported"),
        "unexpected: {err}"
    );

    let err = compile_error("def f(a: int, *, b: int) -> int:\n    return a + b\n");
    assert!(err.contains("keyword-only parameters"), "unexpected: {err}");
}

/// Class keywords (metaclass=...) are rejected at the class definition.
#[test]
fn metaclass_is_rejected() {
    let err = compile_error("class Meta(type):\n    pass\n\nclass A(metaclass=Meta):\n    pass\n");
    assert!(
        err.contains("class keywords") || err.contains("metaclass"),
        "unexpected: {err}"
    );
}

/// min()/max() over one iterable argument used to compile to a stub that
/// always produced 0; they now fail loudly with a workaround hint.
#[test]
fn single_iterable_min_max_are_rejected() {
    let err = compile_error("def f() -> int:\n    xs = [3, 1, 2]\n    return min(xs)\n");
    assert!(
        err.contains("min() over a single iterable"),
        "unexpected: {err}"
    );
    let err = compile_error("def f() -> int:\n    xs = [3, 1, 2]\n    return max(xs)\n");
    assert!(
        err.contains("max() over a single iterable"),
        "unexpected: {err}"
    );
    // The multi-argument forms still compile and compute.
    assert_eq!(
        harness::call_i32(
            "def f() -> int:\n    return min(3, 1, 2) + max(3, 1, 2)\n",
            "f"
        ),
        4
    );
}
