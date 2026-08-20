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

/// A hand-built module whose only function reads local 0 while declaring no
/// parameters and no locals: the exact class of defect (an invalid local
/// index) that used to reach Binaryen and abort the process.
fn module_with_invalid_local_index() -> Vec<u8> {
    let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // Type section: one signature, () -> i32.
    wasm.extend_from_slice(&[0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f]);
    // Function section: function 0 has that signature.
    wasm.extend_from_slice(&[0x03, 0x02, 0x01, 0x00]);
    // Export section: function 0 is exported as "boom".
    wasm.extend_from_slice(&[0x07, 0x08, 0x01, 0x04, b'b', b'o', b'o', b'm', 0x00, 0x00]);
    // Code section: no locals, then `local.get 0; end`.
    wasm.extend_from_slice(&[0x0a, 0x06, 0x01, 0x04, 0x00, 0x20, 0x00, 0x0b]);
    wasm
}

/// Invalid generated WebAssembly is reported as a compile error naming the
/// offending function, instead of being handed to Binaryen (which aborts the
/// whole process with no file, line, or function to go on).
#[test]
fn invalid_generated_wasm_is_a_located_compile_error() {
    let err = waspy::compiler::validate_wasm(&module_with_invalid_local_index())
        .expect_err("an invalid local index must be rejected");
    let err = err.to_string();
    assert!(
        err.contains("failed WebAssembly validation"),
        "unexpected: {err}"
    );
    assert!(
        err.contains("function 'boom'"),
        "the error must name the function it happened in: {err}"
    );
    assert!(
        err.contains("byte offset"),
        "the error must carry the offset: {err}"
    );
    assert!(
        err.contains("code generation bug"),
        "the error must say this is not the user's fault: {err}"
    );
}

/// The validation pass runs on every compilation, so anything the compiler
/// reports as successful is a module a runtime will accept.
#[test]
fn compiled_modules_pass_validation() {
    let wasm = harness::compile(
        "class Counter:\n\
         \x20   def __init__(self):\n\
         \x20       self.items = []\n\
         \x20   def add(self, v: int):\n\
         \x20       self.items.append(v)\n\
         \x20   def total(self) -> int:\n\
         \x20       return sum(self.items)\n\
         \n\
         def run() -> int:\n\
         \x20   c = Counter()\n\
         \x20   for i in range(5):\n\
         \x20       c.add(i)\n\
         \x20   return c.total()\n",
    );
    waspy::compiler::validate_wasm(&wasm).expect("compiled module must validate");
}
