//! Error-quality coverage: unsupported Python syntax is rejected
//! up front by the parser's validation pass with a located, actionable
//! message — instead of failing deep in codegen with an AST debug dump, or
//! (worse) compiling to silently wrong code.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{dedent, try_compile};

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
            "x = 1\nnonlocal x\n\ndef f() -> int:\n    return 0\n",
            "nonlocal declaration not allowed at module level",
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

/// Star parameters on a method, whose calls are not bound by name, are
/// refused naming the method; positional-only parameters are refused rather
/// than dropped.
#[test]
fn star_parameters_on_methods_are_rejected() {
    let err = compile_error(
        "class C:\n    def m(self, *xs: int) -> int:\n        return len(xs)\n\ndef f() -> int:\n    return C().m(1)\n",
    );
    assert!(
        err.contains("*xs") && err.contains("C.m") && err.contains("not on the method"),
        "unexpected: {err}"
    );

    let err = compile_error("def g(a: int, /, b: int) -> int:\n    return a + b\n");
    assert!(err.contains("positional-only"), "unexpected: {err}");
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

/// A class keyword other than `metaclass=Name` is refused at the class
/// definition, and so is a metaclass that would give the class attributes
/// under names known only at run time.
#[test]
fn class_keywords_and_namespaces_are_rejected() {
    let err = compile_error("class A(flag=True):\n    pass\n");
    assert!(
        err.contains("class keywords other than 'metaclass=Name'"),
        "unexpected: {err}"
    );
    let err = compile_error(
        "class M(type):\n    def __new__(mcs, name, bases, ns):\n        ns['x'] = 1\n        \
         return super().__new__(mcs, name, bases, ns)\n\nclass A(metaclass=M):\n    pass\n",
    );
    assert!(
        err.contains("uses 'ns', the class namespace"),
        "unexpected: {err}"
    );
}

/// `min()`/`max()` of what has no order, or of nothing, fail loudly: an
/// unordered element type and a dict (not supported yet) at compile time, an
/// empty iterable without a default with CPython's ValueError.
#[test]
fn min_max_that_cannot_answer_are_rejected() {
    let err = compile_error("def f() -> int:\n    xs = [{1: 2}]\n    return 0 if min(xs) else 1\n");
    assert!(err.contains("min() cannot order"), "unexpected: {err}");
    // A dict walks its keys, as in CPython.
    assert_eq!(
        harness::call_i32(
            "def f() -> int:\n    d = {3: 1, 7: 0}\n    return max(d)\n",
            "f"
        ),
        7
    );
    let err = compile_error("def f() -> int:\n    return min(5)\n");
    assert!(
        err.contains("'int' object is not iterable"),
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

/// `dict.get(key)` answers None for a missing key: an instance value's null
/// pointer, or a boxed optional number. A value with no distinct None (a
/// str here) is refused, and so is a default whose type the values do not
/// share.
#[test]
fn dict_get_that_cannot_answer_is_refused() {
    let err = compile_error(
        "from typing import Dict\n\ndef f() -> int:\n    d: Dict[str, str] = {}\n    \
         return len(d.get('a'))\n",
    );
    assert!(
        err.contains("dict.get(key) answers None for a missing key"),
        "{err}"
    );
    // Returned where an int is declared, a found value is that int, and a
    // missing key's None, which an int cannot hold, traps.
    let src = "from typing import Dict\n\ndef f() -> int:\n    d: Dict[str, int] = {'a': 4}\n    \
               return d.get('a')\n\ndef g() -> int:\n    d: Dict[str, int] = {}\n    \
               return d.get('a')\n";
    assert_eq!(harness::call_i32(src, "f"), 4);
    assert!(harness::call_i32_traps(src, "g"));
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

/// A user-written decorator is applied by specializing its wrapper for the
/// function it decorates, so a wrapper whose answer would depend on how each
/// call is written, or whose names the function's parameters would capture,
/// is refused; so is a return whose type its function's annotation would
/// misread, which is what an unannotated wrapper takes on.
#[test]
fn decorators_that_cannot_be_specialized_are_refused() {
    let deco = |wrapper: &str| {
        format!(
            "def d(fn):\n    def w(*args):\n        {wrapper}\n    return w\n\n\
             @d\ndef k(a: int) -> int:\n    return a\n\n\
             def f() -> int:\n    return k(1)\n"
        )
    };
    let cases: &[(&str, &str)] = &[
        (
            "return len(args)",
            "uses 'args' other than passing it on to the function",
        ),
        (
            "return fn(*args) + a",
            "uses the name 'a', which is also a parameter of 'k'",
        ),
        (
            "return str(fn(*args))",
            "a 'str' is returned from a function declared to return 'int'",
        ),
    ];
    for (wrapper, expected) in cases {
        let err = compile_error(&deco(wrapper));
        assert!(err.contains(expected), "{wrapper}: {err}");
    }
    let err = compile_error("@nothere\ndef k(a: int) -> int:\n    return a\n");
    assert!(
        err.contains("decorator '@nothere' on function 'k' is not supported"),
        "{err}"
    );
}

/// `open()` with its mode in a variable opened the file for reading whatever
/// the mode said; it is refused.
#[test]
fn open_mode_must_be_a_literal() {
    let err = try_compile(&dedent(
        r#"
        def f() -> None:
            m = "w"
            fh = open("out.txt", m)
            fh.write("x")
            fh.close()
        "#,
    ))
    .expect_err("a mode in a variable");
    assert!(
        err.contains("open()'s mode must be written as a string literal"),
        "{err}"
    );
}
