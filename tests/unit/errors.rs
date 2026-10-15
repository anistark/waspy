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
            "def f() -> int:\n    def g() -> int:\n        nonlocal x\n        return x\n    x = 1\n    return g()\n",
            "'nonlocal' statement is not supported",
        ),
        (
            "def f() -> int:\n    x = 1\n    del x\n    return 0\n",
            "'del' of a name or an attribute is not supported",
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
}

/// A call to one of the module's own functions is checked against its
/// signature, as CPython's TypeError would be: a surplus positional argument
/// was dropped (`f(1, 2, 3)` ran `f(1, 2)`), and a missing one produced an
/// invalid module.
#[test]
fn calls_are_checked_against_the_signature() {
    let def = "def f(a: int, b: int = 2, *, k: int = 0) -> int:\n    return a + b + k\n\n\
               def g(*, name: str) -> int:\n    return len(name)\n\n";
    let cases: &[(&str, &str)] = &[
        (
            "f(1, 2, 3)",
            "f() takes 2 positional argument(s) but 3 were given",
        ),
        ("f()", "f() missing required argument: 'a'"),
        ("g()", "g() missing required argument: 'name'"),
        (
            "g('abc')",
            "g() takes 0 positional argument(s) but 1 were given",
        ),
    ];
    for (call, expected) in cases {
        let src = format!("{def}def h() -> int:\n    return {call}\n");
        let err = compile_error(&src);
        assert!(err.contains(expected), "{call}: {err}");
    }
    let err = compile_error("class C:\n    def m(self, *, k: int = 1) -> int:\n        return k\n");
    assert!(
        err.contains("keyword-only parameters are supported on functions and __init__"),
        "{err}"
    );
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

/// An expression the converter does not lower is refused by name, not with a
/// dump of its AST.
#[test]
fn unlowered_expression_is_refused_by_name() {
    let err = compile_error("def f() -> int:\n    return (y := 3) + y\n");
    assert!(
        err.contains("the NamedExpr expression is not supported"),
        "got: {err}"
    );
    assert!(!err.contains("ExprNamedExpr"), "got: {err}");
}

/// A conditional expression's value is read as one type, so arms whose types
/// would read differently are refused rather than one being converted: CPython
/// keeps `1 if c else 2.5` an int when `c` holds, and `True if c else 0` a bool.
#[test]
fn conditional_expression_arms_must_share_a_type() {
    let cases: &[(&str, &str)] = &[
        ("x = 1 if c else 2.5", "(int and float)"),
        ("x = True if c else 0", "(bool and int)"),
        ("x = 'a' if c else None", "(str and None)"),
        ("x = A() if c else B()", "(A and B)"),
    ];
    for (line, types) in cases {
        let src = format!(
            "class A:\n    pass\n\nclass B:\n    pass\n\n\
             def f(c: int) -> int:\n    {line}\n    return 0\n"
        );
        let err = compile_error(&src);
        assert!(
            err.contains("arms of this conditional expression have different types")
                && err.contains(types),
            "{line}: {err}"
        );
    }
}

/// `list.extend()` of something other than a list or a uniformly typed tuple
/// is refused; it dropped the argument and extended by nothing.
#[test]
fn list_extend_of_another_iterable_is_refused() {
    for (arg, ty) in [
        ("range(3)", "'range'"),
        ("{4, 5}", "'Set[int]'"),
        ("'bc'", "'str'"),
    ] {
        let src = format!(
            "from typing import List\n\ndef f() -> int:\n    xs: List[int] = [1]\n    \
             xs.extend({arg})\n    return len(xs)\n"
        );
        let err = compile_error(&src);
        assert!(
            err.contains("list.extend() takes a list or a tuple of one element type")
                && err.contains(ty),
            "{arg}: {err}"
        );
    }
}

/// `dict.get(key)` answers None for a missing key, which only an instance
/// value has a distinct word for; on other values it is refused (it answered
/// 0), and so is a default whose type the values do not share.
#[test]
fn dict_get_that_cannot_answer_is_refused() {
    let err = compile_error(
        "from typing import Dict\n\ndef f() -> int:\n    d: Dict[str, int] = {}\n    \
         return d.get('a')\n",
    );
    assert!(
        err.contains("dict.get(key) answers None for a missing key"),
        "{err}"
    );
    let err = compile_error(
        "from typing import Dict\n\ndef f() -> int:\n    d: Dict[str, int] = {}\n    \
         x = d.get('a', 'none')\n    return 0\n",
    );
    assert!(err.contains("dict.get()'s default is a 'str'"), "{err}");
}

/// `del` of an item works on lists and dicts; a tuple or a str has no item
/// deletion, as in CPython, and a stepped slice is not supported yet.
#[test]
fn del_of_an_immutable_item_is_refused() {
    let cases: &[(&str, &str)] = &[
        (
            "x = (1, 2)\n    del x[0]",
            "'tuple' object doesn't support item deletion",
        ),
        (
            "s = 'ab'\n    del s[0]",
            "'str' object doesn't support item deletion",
        ),
        (
            "xs = [1, 2, 3]\n    del xs[::2]",
            "'del' of a stepped slice is not supported",
        ),
    ];
    for (body, expected) in cases {
        let err = compile_error(&format!("def f() -> int:\n    {body}\n    return 0\n"));
        assert!(err.contains(expected), "{body}: {err}");
    }
}

/// `global` rebinds a module definition of the same type; one that names no
/// definition, changes the type, or is bound by a loop is refused.
#[test]
fn global_that_cannot_rebind_is_refused() {
    let cases: &[(&str, &str)] = &[
        (
            "global missing\n    missing = 1",
            "'global missing' names no module-level definition",
        ),
        (
            "global n\n    n = 'x'",
            "the global 'n' is typed 'int' and cannot be rebound to a 'str'",
        ),
        (
            "global n\n    for n in range(3):\n        pass",
            "the global 'n' is bound by a loop",
        ),
    ];
    for (body, expected) in cases {
        let err = compile_error(&format!(
            "n = 0\n\ndef f() -> int:\n    {body}\n    return 0\n"
        ));
        assert!(err.contains(expected), "{body}: {err}");
    }
}
