//! The development board's features, one test each (`just board-check`).
//!
//! Every feature on `docs/modules/index.html` names the test here that backs
//! it, as `card::test`. A feature marked done must have one that passes; one
//! still open may have one, marked `#[ignore]`, asserting what the feature will
//! answer once it lands, and `just board-check` fails the moment such a test
//! starts passing, so the board is moved in the same change. Expected values
//! are CPython's for the same source, except where the board documents the
//! difference (an `int` is 32 bits and wraps).

#[path = "../utils/harness.rs"]
mod harness;

#[allow(unused_imports)]
use harness::*;

/// What calling an exported function must answer.
#[allow(dead_code)]
enum Want {
    Int(i32),
    Float(f64),
    Str(&'static str),
    Traps,
}

/// Compile `source` (indented in the test, dedented here) and check each call.
fn check(source: &str, wants: &[(&str, Want)]) {
    let src = dedent(source);
    if let Err(e) = try_compile(&src) {
        panic!("compile: {e}");
    }
    for (func, want) in wants {
        match want {
            Want::Int(v) => assert_eq!(call_i32(&src, func), *v, "{func}()"),
            Want::Float(v) => assert_eq!(call_f64(&src, func), *v, "{func}()"),
            Want::Str(v) => assert_eq!(call_str(&src, func), *v, "{func}()"),
            Want::Traps => assert!(call_i32_traps(&src, func), "{func}() must trap"),
        }
    }
}

/// `source` is refused, and the refusal names `needle`.
#[allow(dead_code)]
fn refused(source: &str, needle: &str) {
    let err = try_compile(&dedent(source)).expect_err("this construct must be refused");
    assert!(err.contains(needle), "expected `{needle}` in: {err}");
}

#[allow(dead_code)]
fn compile_with(source: &str, optimize: bool) -> Vec<u8> {
    let options = waspy::CompilerOptions {
        optimize,
        ..waspy::CompilerOptions::default()
    };
    waspy::compile_python_to_wasm_with_options(source, &options).expect("compiles")
}

#[allow(dead_code)]
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Write `files` into a fresh directory for a test that compiles from disk.
#[allow(dead_code)]
fn project(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("waspy-board-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("write");
    }
    dir
}

mod arithmetic {
    use super::*;

    /// All operators (+, -, *, /, %, //, **)
    #[test]
    fn all_operators() {
        check(
            r#"
            def f() -> int:
                return 7 + 3 * 2 - 8 // 3 + 7 % 4 + 2 ** 5

            def g() -> float:
                return 7 / 2 + 1.5 * 2.0 - 0.25
        "#,
            &[("f", Want::Int(46)), ("g", Want::Float(6.25))],
        );
    }

    /// / is true division and yields a float, // is floor division (Python 3 semantics)
    #[test]
    fn true_and_floor_division() {
        check(
            r#"
            def f() -> float:
                return 7 / 2

            def g() -> int:
                return 7 // 2 + (-7) // 2
        "#,
            &[("f", Want::Float(3.5)), ("g", Want::Int(-1))],
        );
    }

    /// Type coercion (int/float)
    #[test]
    fn int_float_coercion() {
        check(
            r#"
            def f() -> float:
                n = 3
                return n * 1.5 + 2
        "#,
            &[("f", Want::Float(6.5))],
        );
    }

    /// Bitwise operations (&, |, ^, <<, >>, ~)
    #[test]
    fn bitwise_operations() {
        check(
            r#"
            def f() -> int:
                return (12 & 10) * 1000 + (12 | 3) * 100 + (12 ^ 5) + (1 << 4) + (64 >> 2) + ~5
        "#,
            &[("f", Want::Int(9535))],
        );
    }

    /// Unary operations (-, +, not)
    #[test]
    fn unary_operations() {
        check(
            r#"
            def f() -> int:
                a = 5
                r = -a + +a
                if not a == 4:
                    r += 10
                return r

            def g() -> float:
                x = 2.5
                return -x
        "#,
            &[("f", Want::Int(10)), ("g", Want::Float(-2.5))],
        );
    }

    /// Integer // and % floor as Python does (they truncated for mixed-sign operands)
    #[test]
    fn floored_division_and_modulo() {
        check(
            r#"
            def f() -> int:
                return (-7 // 2) * 100 + (-7 % 3) * 10 + (7 % -3)
        "#,
            &[("f", Want::Int(-382))],
        );
    }

    /// Augmented assignment shares the plain operators (string +=, /=, float %=, floored %= and //=)
    #[test]
    fn augmented_assignment_operators() {
        check(
            r##"
            def f() -> str:
                line = "a"
                line += "#"
                return line

            def g() -> float:
                x = 7.0
                x /= 2
                x %= 2.0
                return x

            def h() -> int:
                n = -7
                n //= 2
                m = -7
                m %= 3
                return n * 10 + m
        "##,
            &[
                ("f", Want::Str("a#")),
                ("g", Want::Float(1.5)),
                ("h", Want::Int(-38)),
            ],
        );
    }
}

mod control_flow {
    use super::*;

    /// if/elif/else statements
    #[test]
    fn if_elif_else() {
        check(
            r#"
            def grade(n: int) -> str:
                if n >= 90:
                    return "A"
                elif n >= 80:
                    return "B"
                else:
                    return "C"

            def f() -> str:
                return grade(95) + grade(85) + grade(10)
        "#,
            &[("f", Want::Str("ABC"))],
        );
    }

    /// while loops
    #[test]
    fn while_loops() {
        check(
            r#"
            def f() -> int:
                i = 0
                total = 0
                while i < 10:
                    total += i
                    i += 1
                return total
        "#,
            &[("f", Want::Int(45))],
        );
    }

    /// for loops with list/string iteration
    #[test]
    fn for_over_lists_and_strings() {
        check(
            r#"
            def f() -> int:
                t = 0
                for x in [3, 4, 5]:
                    t += x
                for ch in "abc":
                    t += len(ch) * 10
                return t
        "#,
            &[("f", Want::Int(42))],
        );
    }

    /// break and continue in loops
    #[test]
    fn break_and_continue() {
        check(
            r#"
            def f() -> int:
                t = 0
                for i in range(10):
                    if i == 7:
                        break
                    if i % 2 == 0:
                        continue
                    t += i
                return t
        "#,
            &[("f", Want::Int(9))],
        );
    }

    /// try/except/finally blocks with real exception propagation (see EXCEPTION HANDLING)
    #[test]
    fn try_except_finally() {
        check(
            r#"
            def f() -> int:
                r = 0
                try:
                    r += 1
                    raise ValueError("x")
                except ValueError:
                    r += 10
                finally:
                    r += 100
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// with statements over user context managers (__enter__/__exit__, nested, inherited, exit before return / break / continue)
    #[test]
    fn with_context_managers() {
        check(
            r#"
            class Log:
                def __init__(self):
                    self.events = 0

            class Ctx:
                def __init__(self, log: Log):
                    self.log = log

                def __enter__(self) -> int:
                    self.log.events += 1
                    return 5

                def __exit__(self, a, b, c) -> bool:
                    self.log.events += 10
                    return False

            def f() -> int:
                log = Log()
                with Ctx(log) as v:
                    with Ctx(log):
                        pass
                return log.events * 10 + v

            def g() -> int:
                log = Log()
                for i in range(3):
                    with Ctx(log):
                        if i == 1:
                            break
                return log.events
        "#,
            &[("f", Want::Int(225)), ("g", Want::Int(22))],
        );
    }

    /// Comparison operations (==, !=, <, <=, >, >=)
    #[test]
    fn comparisons() {
        check(
            r#"
            def f() -> int:
                a = 3
                b = 5
                r = 0
                if a == 3:
                    r += 1
                if a != b:
                    r += 2
                if a < b:
                    r += 4
                if a <= 3:
                    r += 8
                if b > a:
                    r += 16
                if b >= 6:
                    r += 32
                return r
        "#,
            &[("f", Want::Int(31))],
        );
    }

    /// Boolean logic (and, or, not) with short-circuit
    #[test]
    fn boolean_short_circuit() {
        check(
            r#"
            from typing import List

            def boom(xs: List[int]) -> bool:
                return xs[5] == 0

            def f() -> int:
                xs: List[int] = []
                r = 0
                if False and boom(xs):
                    r += 1
                if True or boom(xs):
                    r += 10
                if not (1 > 2):
                    r += 100
                return r
        "#,
            &[("f", Want::Int(110))],
        );
    }

    /// else clauses on for, while, and try
    #[test]
    fn else_clauses() {
        check(
            r#"
            def f() -> int:
                r = 0
                for i in range(3):
                    if i == 5:
                        break
                else:
                    r += 1
                while r < 0:
                    pass
                else:
                    r += 10
                try:
                    r += 0
                except ValueError:
                    r = -1
                else:
                    r += 100
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// Conditional expressions (a if cond else b)
    #[test]
    #[ignore = "open on the board"]
    fn conditional_expressions() {
        check(
            r#"
            def pick(n: int) -> str:
                return "big" if n > 10 else "small"

            def f() -> int:
                return len(pick(50)) * 10 + len(pick(1))
        "#,
            &[("f", Want::Int(35))],
        );
    }
}

mod functions {
    use super::*;

    /// Function definitions with parameters
    #[test]
    fn definitions_with_parameters() {
        check(
            r#"
            def area(w: int, h: int) -> int:
                return w * h

            def f() -> int:
                return area(3, 4)
        "#,
            &[("f", Want::Int(12))],
        );
    }

    /// Type annotations (params & return types)
    #[test]
    fn annotated_signatures() {
        check(
            r#"
            def scale(x: float, k: int) -> float:
                return x * k

            def f() -> float:
                return scale(1.25, 4)
        "#,
            &[("f", Want::Float(5.0))],
        );
    }

    /// Function calls between compiled functions
    #[test]
    fn calls_between_functions() {
        check(
            r#"
            def fact(n: int) -> int:
                if n <= 1:
                    return 1
                return n * fact(n - 1)

            def f() -> int:
                return fact(6)
        "#,
            &[("f", Want::Int(720))],
        );
    }

    /// Multiple functions per module
    #[test]
    fn multiple_functions() {
        check(
            r#"
            def a() -> int:
                return 1

            def b() -> int:
                return a() + 1

            def c() -> int:
                return b() * 10
        "#,
            &[
                ("a", Want::Int(1)),
                ("b", Want::Int(2)),
                ("c", Want::Int(20)),
            ],
        );
    }

    /// Keyword arguments in calls to functions and constructors, placed by name with defaults filled in
    #[test]
    fn keyword_arguments() {
        check(
            r#"
            def f(a: int, b: int = 1, c: int = 2) -> int:
                return a * 100 + b * 10 + c

            def g() -> int:
                return f(1, c=7) + f(c=1, a=2, b=3)
        "#,
            &[("g", Want::Int(348))],
        );
    }

    /// An unannotated parameter or return takes ints, bools, and None; a string, float, collection, or instance through one is refused
    #[test]
    fn untyped_parameters_take_words() {
        refused(
            r#"
            def ln(s):
                return len(s)

            def f() -> int:
                return ln("abc")
        "#,
            "argument 1 of ln()",
        );
    }
}

mod type_system {
    use super::*;

    /// Basic types: int (32-bit two's complement, wraps on overflow rather than growing like CPython's), float (IEEE-754 double), bool
    #[test]
    fn basic_types_and_int_width() {
        check(
            r#"
            def big() -> int:
                n = 2147483647
                return n + 1

            def fl() -> float:
                return 0.1 + 0.2

            def bo() -> bool:
                return 3 > 2
        "#,
            &[
                ("big", Want::Int(-2147483648)),
                ("fl", Want::Float(0.30000000000000004)),
                ("bo", Want::Int(1)),
            ],
        );
    }

    /// Generic types (List[T], Dict[K,V], Tuple[T,...])
    #[test]
    fn generic_types() {
        check(
            r#"
            from typing import Dict, List, Tuple

            def f() -> int:
                xs: List[str] = ["a", "bb"]
                d: Dict[str, int] = {"k": 3}
                t: Tuple[int, str] = (4, "x")
                return len(xs[1]) * 100 + d["k"] * 10 + t[0]
        "#,
            &[("f", Want::Int(234))],
        );
    }

    /// Union/Optional types
    #[test]
    fn union_and_optional() {
        check(
            r#"
            from typing import Optional, Union

            def label(x: Optional[str]) -> str:
                if x is None:
                    return "none"
                return x

            def pick(flag: bool) -> Union[int, str]:
                return 3

            def g() -> str:
                return label(None) + label("a")
        "#,
            &[("g", Want::Str("nonea"))],
        );
    }

    /// Custom class type annotations
    #[test]
    fn class_annotations() {
        check(
            r#"
            class P:
                def __init__(self, x: int):
                    self.x = x

            def get(p: P) -> int:
                return p.x

            def f() -> int:
                return get(P(7))
        "#,
            &[("f", Want::Int(7))],
        );
    }

    /// Unannotated locals typed from the value: collection element types, c = C() as an instance, a method call's declared return type
    #[test]
    fn inferred_local_types() {
        check(
            r#"
            class C:
                def __init__(self):
                    self.v = 4

                def half(self) -> float:
                    return self.v / 2

            def f() -> float:
                xs = [1.5, 2.5]
                c = C()
                h = c.half()
                return xs[1] + h
        "#,
            &[("f", Want::Float(4.5))],
        );
    }
}

mod variables {
    use super::*;

    /// Variable declarations and assignments
    #[test]
    fn assignments() {
        check(
            r#"
            def f() -> int:
                a = 1
                b: int = 2
                a = a + b
                return a
        "#,
            &[("f", Want::Int(3))],
        );
    }

    /// Augmented assignment (+=, -=, *=, etc.)
    #[test]
    fn augmented_assignment() {
        check(
            r#"
            def f() -> int:
                n = 10
                n += 5
                n -= 3
                n *= 2
                return n
        "#,
            &[("f", Want::Int(24))],
        );
    }

    /// Attribute assignment (obj.attr = value)
    #[test]
    fn attribute_assignment() {
        check(
            r#"
            class C:
                def __init__(self):
                    self.v = 0

            def f() -> int:
                c = C()
                c.v = 9
                return c.v
        "#,
            &[("f", Want::Int(9))],
        );
    }

    /// Type inference from usage
    #[test]
    fn type_inference_from_usage() {
        check(
            r#"
            def f() -> float:
                total = 0.0
                for x in [1, 2, 3]:
                    total += x / 2
                return total
        "#,
            &[("f", Want::Float(3.0))],
        );
    }

    /// Module-level definitions evaluated once at instantiation and shared (start function + globals)
    #[test]
    fn module_level_definitions() {
        check(
            r#"
            from typing import List

            ITEMS: List[int] = [1, 2]

            def add(v: int) -> int:
                ITEMS.append(v)
                return len(ITEMS)

            def f() -> int:
                add(5)
                return add(6)
        "#,
            &[("f", Want::Int(4))],
        );
    }

    /// Class-level variables shared by the class
    #[test]
    fn class_variables() {
        check(
            r#"
            class Counter:
                step = 3

                def __init__(self):
                    self.n = Counter.step

            def f() -> int:
                return Counter().n + Counter.step
        "#,
            &[("f", Want::Int(6))],
        );
    }

    /// Tuple unpacking types its targets; a wrong-length value raises ValueError
    #[test]
    fn tuple_unpacking() {
        check(
            r#"
            def f() -> str:
                a, b = 1, "xy"
                return b * a

            def g() -> int:
                a, b = [1, 2, 3]
                return a
        "#,
            &[("f", Want::Str("xy")), ("g", Want::Traps)],
        );
    }

    /// Augmented assignment through subscripts and computed objects, each target evaluated once
    #[test]
    fn augmented_assignment_targets() {
        check(
            r#"
            from typing import Dict, List

            def f() -> int:
                xs: List[int] = [1, 2]
                d: Dict[str, int] = {"a": 1}
                xs[1] += 5
                d["a"] *= 4
                return xs[1] * 10 + d["a"]
        "#,
            &[("f", Want::Int(74))],
        );
    }

    /// Annotated fields: self.items: Dict[str, Item] = {}
    #[test]
    fn annotated_fields() {
        check(
            r#"
            from typing import Dict

            class Inv:
                def __init__(self):
                    self.items: Dict[str, int] = {}

                def add(self, k: str, n: int) -> None:
                    self.items[k] = n

            def f() -> int:
                inv = Inv()
                inv.add("a" + "b", 3)
                return inv.items["ab"]
        "#,
            &[("f", Want::Int(3))],
        );
    }

    /// A module-level statement that would not run (a loop, a call, an augmented assignment) is refused
    #[test]
    fn module_statements_that_would_not_run() {
        refused(
            r#"
            n = 0
            for i in range(3):
                n += i

            def f() -> int:
                return n
        "#,
            "at module level",
        );
    }
}

mod decorators {
    use super::*;

    /// Standard decorators applied: @staticmethod, @classmethod, @property / @x.setter, @abstractmethod, @dataclass, and the functools decorators (see their cards)
    #[test]
    fn standard_decorators() {
        check(
            r#"
            from abc import ABC, abstractmethod
            from dataclasses import dataclass
            from functools import lru_cache

            class Shape(ABC):
                @abstractmethod
                def area(self) -> int:
                    pass

            class Sq(Shape):
                def __init__(self, s: int):
                    self._s = s

                def area(self) -> int:
                    return self._s * self._s

                @property
                def side(self) -> int:
                    return self._s

                @side.setter
                def side(self, v: int) -> None:
                    self._s = v

                @staticmethod
                def unit() -> int:
                    return 1

                @classmethod
                def make(cls, s: int) -> "Sq":
                    return cls(s)

            @dataclass
            class Pt:
                x: int
                y: int

            @lru_cache
            def sq(n: int) -> int:
                return n * n

            def f() -> int:
                s = Sq.make(3)
                s.side = 4
                p = Pt(1, 2)
                r = s.area() * 100 + Sq.unit() * 10 + sq(2)
                if p == Pt(1, 2):
                    r += 1000
                return r
        "#,
            &[("f", Want::Int(2614))],
        );
    }

    /// Unimplemented decorators are refused (functions, methods, and classes) instead of being silently dropped
    #[test]
    fn unimplemented_decorators_refused() {
        refused(
            r#"
            def deco(fn):
                return fn

            @deco
            def f() -> int:
                return 1
        "#,
            "decorator '@deco'",
        );
    }

    /// Decorators CPython does not define (@memoize, @timer, @debug, @pure, @wasm_export, ...) are refused, as CPython raises NameError
    #[test]
    fn undefined_decorators_refused() {
        refused(
            r#"
            @memoize
            def f() -> int:
                return 1
        "#,
            "decorator '@memoize'",
        );
    }

    /// User-written decorators: a function that takes a function and returns one, applied at definition time
    #[test]
    #[ignore = "open on the board"]
    fn user_written_decorators() {
        check(
            r#"
            def twice(fn):
                def wrapper(x: int) -> int:
                    return fn(fn(x))
                return wrapper

            @twice
            def inc(x: int) -> int:
                return x + 1

            def f() -> int:
                return inc(1)
        "#,
            &[("f", Want::Int(3))],
        );
    }
}

mod strings {
    use super::*;

    /// String literals, indexing, slicing, concatenation
    #[test]
    fn literals_indexing_slicing() {
        check(
            r#"
            def f() -> str:
                s = "hello"
                return s[1] + s[-1] + s[1:3] + s[::-1][:2] + "!"
        "#,
            &[("f", Want::Str("eoelol!"))],
        );
    }

    /// Case conversion (upper, lower, capitalize, title)
    #[test]
    fn case_conversion() {
        check(
            r#"
            def f() -> str:
                s = "hello World"
                return s.upper() + s.lower() + s.capitalize() + s.title()
        "#,
            &[(
                "f",
                Want::Str("HELLO WORLDhello worldHello worldHello World"),
            )],
        );
    }

    /// Test methods (isdigit, isalpha, isspace, etc.) - compile-time optimized
    #[test]
    fn predicates() {
        check(
            r#"
            def f() -> int:
                r = 0
                if "123".isdigit():
                    r += 1
                if "abc".isalpha():
                    r += 10
                if " \t".isspace():
                    r += 100
                if not "a1".isalpha():
                    r += 1000
                return r
        "#,
            &[("f", Want::Int(1111))],
        );
    }

    /// Search methods (.find, .count, .startswith, .endswith)
    #[test]
    fn search_methods() {
        check(
            r#"
            def f() -> int:
                s = "banana"
                r = s.find("an") * 1000 + s.count("a") * 100
                if s.startswith("ba"):
                    r += 10
                if s.endswith("na"):
                    r += 1
                return r
        "#,
            &[("f", Want::Int(1311))],
        );
    }

    /// Transform methods (.split, .join, .replace)
    #[test]
    fn split_join_replace() {
        check(
            r#"
            def f() -> str:
                parts = "a,b,c".split(",")
                return "-".join(parts).replace("b", "B")
        "#,
            &[("f", Want::Str("a-B-c"))],
        );
    }

    /// A method on a literal receiver answers what the same method on a variable does, .split(sep) included
    #[test]
    fn literal_receiver_matches_runtime() {
        check(
            r#"
            def f() -> int:
                s = "a b  c"
                return len("a b  c".split(" ")) * 10 + len(s.split(" "))
        "#,
            &[("f", Want::Int(44))],
        );
    }

    /// Layout methods (.ljust, .rjust, .center)
    #[test]
    fn layout_methods() {
        check(
            r#"
            def f() -> str:
                return "ab".ljust(4, ".") + "|" + "ab".rjust(4) + "|" + "ab".center(6, "*")
        "#,
            &[("f", Want::Str("ab..|  ab|**ab**"))],
        );
    }

    /// % formatting - compile-time for constants: %s %r %d %i %x %X %o %f with flags, width, and precision (rendered as written, or in decimal for %x, before; anything else is refused)
    #[test]
    fn constant_percent_formatting() {
        check(
            r#"
            def f() -> str:
                return "%5d|%-6.2f|%#x|%r|%s" % (42, -1.005, 255, "a", True)
        "#,
            &[("f", Want::Str("   42|-1.00 |0xff|'a'|True"))],
        );
    }

    /// % formatting on runtime values ("%d" % n)
    #[test]
    #[ignore = "open on the board"]
    fn runtime_percent_formatting() {
        check(
            r#"
            def f(n: int) -> str:
                return "%d!" % n

            def g() -> str:
                return f(5)
        "#,
            &[("g", Want::Str("5!"))],
        );
    }

    /// Repetition: s * n and n * s
    #[test]
    fn repetition() {
        check(
            r#"
            def f() -> str:
                n = 2
                return "ab" * 3 + n * "c"
        "#,
            &[("f", Want::Str("abababcc"))],
        );
    }

    /// Every string method works on a runtime receiver, not only a literal (case, trim, search, split/join, replace, layout, predicates)
    #[test]
    fn runtime_receivers() {
        check(
            r#"
            def f(w: str) -> str:
                return w.strip().upper().replace("B", "x").center(7, "-")

            def g() -> str:
                return f("  ab ")
        "#,
            &[("g", Want::Str("---Ax--"))],
        );
    }

    /// .format() interpolates runtime values - automatic {} and positional {0} fields (named fields, specifiers, and conversions rejected)
    #[test]
    fn format_method() {
        check(
            r#"
            def f(n: int) -> str:
                return "{} of {}".format(n, 10) + "{1}{0}{1}".format(n, "-")

            def g() -> str:
                return f(3)
        "#,
            &[("g", Want::Str("3 of 10-3-"))],
        );
    }

    /// Strings compare by content everywhere, so a key built at runtime matches an equal one (dict keys, in over lists and sets)
    #[test]
    fn content_comparison() {
        check(
            r#"
            from typing import Dict

            def f() -> int:
                d: Dict[str, int] = {}
                for w in "a b a".split(" "):
                    d[w] = d.get(w, 0) + 1
                k = "a" + ""
                r = d[k] * 10
                if k in ["x", "a"]:
                    r += 1
                return r
        "#,
            &[("f", Want::Int(21))],
        );
    }

    /// sub in text searches the string
    #[test]
    fn substring_in() {
        check(
            r#"
            def f() -> int:
                t = "hello world"
                r = 0
                if "lo w" in t:
                    r += 1
                if "xyz" not in t:
                    r += 10
                return r
        "#,
            &[("f", Want::Int(11))],
        );
    }

    /// F-strings - constant placeholders folded into the literal
    #[test]
    fn fstring_constants() {
        check(
            r#"
            def f() -> str:
                return f"{1} {True} {2.5} {'x'}"
        "#,
            &[("f", Want::Str("1 True 2.5 x"))],
        );
    }

    /// F-strings - dynamic interpolation (every placeholder rendered and concatenated)
    #[test]
    fn fstring_runtime() {
        check(
            r#"
            def f(n: int, s: str) -> str:
                return f"{s}: {n} items ({n * 2})"

            def g() -> str:
                return f(3, "box")
        "#,
            &[("g", Want::Str("box: 3 items (6)"))],
        );
    }

    /// F-string fixed-point specifier f"{value:.2f}" (every other specifier still refused)
    #[test]
    fn fstring_fixed_point() {
        check(
            r#"
            def f(x: float) -> str:
                return f"{x:.2f}|{x:.0f}"

            def g() -> str:
                return f(2.675)
        "#,
            &[("g", Want::Str("2.67|3"))],
        );
    }

    /// String ordering (<, <=, >, >=) and identity (is)
    #[test]
    fn string_ordering() {
        check(
            r#"
            def f() -> int:
                a = "apple"
                b = "b" + "anana"
                r = 0
                if a < b:
                    r += 1
                if b >= "banana":
                    r += 10
                if a is a:
                    r += 100
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// s[i] is a real one-character string wherever it goes
    #[test]
    fn indexed_character_is_a_string() {
        check(
            r#"
            from typing import List

            def f() -> str:
                s = "xyz"
                out: List[str] = []
                out.append(s[1])
                return out[0] + s[2].upper()
        "#,
            &[("f", Want::Str("yZ"))],
        );
    }

    /// for ch in s iterates characters
    #[test]
    fn iterate_characters() {
        check(
            r#"
            def f() -> str:
                out = ""
                for ch in "abc":
                    out = ch + out
                return out
        "#,
            &[("f", Want::Str("cba"))],
        );
    }
}

mod collections {
    use super::*;

    /// List literals with memory allocation [1, 2, 3]
    #[test]
    fn list_literals() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2, 3]
                return len(xs) * 10 + xs[2]
        "#,
            &[("f", Want::Int(33))],
        );
    }

    /// List indexing (read & write) list[i] and list[i] = value
    #[test]
    fn list_indexing() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2, 3]
                xs[0] = 9
                xs[-1] = 7
                return xs[0] * 10 + xs[2]
        "#,
            &[("f", Want::Int(97))],
        );
    }

    /// List methods .append(), .pop([i]), .clear(), .insert(i, v)
    #[test]
    fn list_methods() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2]
                xs.append(3)
                xs.insert(0, 0)
                last = xs.pop()
                first = xs.pop(0)
                n = len(xs)
                xs.clear()
                return last * 100 + first * 10 + n + len(xs)
        "#,
            &[("f", Want::Int(302))],
        );
    }

    /// .remove(v) drops the first equal element; a missing value raises a catchable ValueError
    #[test]
    fn list_remove() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2, 1]
                xs.remove(1)
                try:
                    xs.remove(7)
                except ValueError:
                    return len(xs) * 10 + xs[0]
                return 0
        "#,
            &[("f", Want::Int(22))],
        );
    }

    /// .pop(i) removes the element at i and .index(v) answers its position; a missing value or position traps, as Python raises
    #[test]
    fn pop_index_and_index_of() {
        check(
            r#"
            def f() -> int:
                xs = [5, 6, 7]
                v = xs.pop(1)
                return v * 10 + xs.index(7)

            def g() -> int:
                xs = [1]
                return xs.index(9)
        "#,
            &[("f", Want::Int(61)), ("g", Want::Traps)],
        );
    }

    /// A method called with the wrong number of arguments is a compile error, not a silently ignored one
    #[test]
    fn method_arity_checked() {
        refused(
            r#"
            def f() -> int:
                xs = [1]
                xs.append(1, 2)
                return 1
        "#,
            "append",
        );
    }

    /// List search methods .index(v) & .count(v) with linear search
    #[test]
    fn list_search() {
        check(
            r#"
            def f() -> int:
                xs = [3, 1, 3, 2]
                return xs.index(2) * 10 + xs.count(3)
        "#,
            &[("f", Want::Int(32))],
        );
    }

    /// Dict literals with memory allocation {'key': value}
    #[test]
    fn dict_literals() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1, "b": 2}
                return len(d) * 10 + d["b"]
        "#,
            &[("f", Want::Int(22))],
        );
    }

    /// Dict indexing (read & write) dict[key] and dict[key] = value
    #[test]
    fn dict_indexing() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1}
                d["a"] = 5
                d["z"] = 2
                return d["a"] * 10 + d["z"]

            def g() -> int:
                d = {"a": 1}
                try:
                    return d["q"]
                except KeyError:
                    return -1
        "#,
            &[("f", Want::Int(52)), ("g", Want::Int(-1))],
        );
    }

    /// Runtime growth: lists reallocate on append/extend/insert, dicts on a new key
    #[test]
    fn runtime_growth() {
        check(
            r#"
            from typing import Dict, List

            def f() -> int:
                xs: List[int] = []
                d: Dict[int, int] = {}
                for i in range(100):
                    xs.append(i)
                    d[i] = i * 2
                xs.extend([1, 2])
                return len(xs) * 1000 + d[99]
        "#,
            &[("f", Want::Int(102198))],
        );
    }

    /// Iterating a list of instances binds a typed element (for it in items: it.field)
    #[test]
    fn iterate_instances() {
        check(
            r#"
            class It:
                def __init__(self, w: int):
                    self.w = w

            def f() -> int:
                items = [It(2), It(3)]
                t = 0
                for it in items:
                    t += it.w
                return t
        "#,
            &[("f", Want::Int(5))],
        );
    }

    /// Set mutation .add() / .remove() / .discard(), rehashing when the table fills
    #[test]
    fn set_mutation() {
        check(
            r#"
            def f() -> int:
                s = {1}
                for i in range(20):
                    s.add(i)
                s.remove(5)
                s.discard(99)
                return len(s)
        "#,
            &[("f", Want::Int(19))],
        );
    }

    /// List methods .sort([reverse]) and .reverse(), in place
    #[test]
    fn sort_and_reverse() {
        check(
            r#"
            def f() -> int:
                xs = [3, 1, 2]
                xs.sort()
                a = xs[0]
                xs.sort(reverse=True)
                b = xs[0]
                xs.reverse()
                return a * 100 + b * 10 + xs[0]
        "#,
            &[("f", Want::Int(131))],
        );
    }

    /// List slicing xs[a:b] returns a fresh list, negative indices and clamping included (a step other than 1 is rejected)
    #[test]
    fn list_slicing() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2, 3, 4, 5]
                ys = xs[1:-1]
                ys[0] = 9
                return len(ys) * 100 + xs[1] * 10 + len(xs[7:])
        "#,
            &[("f", Want::Int(320))],
        );
    }

    /// Dict methods .get(key[, default]), .keys(), .values(), .items() - .items() as a value, not only a for iterable
    #[test]
    fn dict_methods() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1, "b": 2}
                t = d.get("a") + d.get("q", 10)
                for k in d.keys():
                    t += len(k)
                for v in d.values():
                    t += v * 100
                items = d.items()
                return t + len(items) * 1000
        "#,
            &[("f", Want::Int(2313))],
        );
    }

    /// in searches dicts and tuples as well as lists and sets; anything unsearchable is a compile error
    #[test]
    fn in_dicts_and_tuples() {
        check(
            r#"
            def f() -> int:
                r = 0
                if "a" in {"a": 1}:
                    r += 1
                if 3 in (1, 3):
                    r += 10
                return r
        "#,
            &[("f", Want::Int(11))],
        );
    }

    /// Truthiness follows Python: an empty list, dict, set, or tuple is False
    #[test]
    fn collection_truthiness() {
        check(
            r#"
            from typing import Dict, List

            def f() -> int:
                xs: List[int] = []
                d: Dict[int, int] = {}
                r = 0
                if not xs:
                    r += 1
                if not d:
                    r += 10
                if [0]:
                    r += 100
                if not ():
                    r += 1000
                return r
        "#,
            &[("f", Want::Int(1111))],
        );
    }

    /// A method the compiler does not implement is a compile error rather than a silent no-op
    #[test]
    fn unknown_methods_refused() {
        refused(
            r#"
            def f() -> int:
                xs = [1]
                xs.frobnicate()
                return 1
        "#,
            "frobnicate",
        );
    }

    /// Value equality and ordering for tuples and lists (==, !=, <, <=, >, >=)
    #[test]
    fn sequence_equality_and_ordering() {
        check(
            r#"
            def f() -> int:
                r = 0
                if [1, 2] == [1, 2]:
                    r += 1
                if (1, "b") < (1, "c"):
                    r += 10
                if [1, 2] < [1, 2, 0]:
                    r += 100
                if (2, 1) != (2, 1):
                    r += 1000
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// Tuples as set members and dict keys; sets hash strings by content
    #[test]
    fn tuples_as_keys() {
        check(
            r#"
            def f() -> int:
                s = {(1, 2), (1, 2), (2, 1)}
                d = {(1, "a"): 5}
                w = "a" + ""
                return len(s) * 10 + d[(1, w)]
        "#,
            &[("f", Want::Int(25))],
        );
    }

    /// Every evaluation of a collection literal is a new object
    #[test]
    fn fresh_literals() {
        check(
            r#"
            from typing import List

            def make() -> List[int]:
                return [0]

            def f() -> int:
                a = make()
                b = make()
                a.append(1)
                return len(a) * 10 + len(b)
        "#,
            &[("f", Want::Int(21))],
        );
    }

    /// A tuple subscript (d[(1, 2)]) is an index, not a slice
    #[test]
    fn tuple_subscript() {
        check(
            r#"
            def f() -> int:
                d = {(1, 2): 7}
                return d[1, 2]
        "#,
            &[("f", Want::Int(7))],
        );
    }

    /// Collection writes convert the value to the element type, or are refused (append, insert, item assignment, dict values, set.add)
    #[test]
    fn collection_writes_convert() {
        check(
            r#"
            from typing import List

            def f() -> float:
                xs: List[float] = []
                xs.append(3)
                xs.insert(0, 1)
                return xs[0] + xs[1] / 2
        "#,
            &[("f", Want::Float(2.5))],
        );
    }

    /// Sequence repetition: [0] * n and (a,) * n (a tuple's count must be a constant, since its length is part of its type)
    #[test]
    fn sequence_repetition() {
        check(
            r#"
            def f() -> int:
                xs = [0] * 4
                t = (1, 2) * 2
                return len(xs) * 10 + len(t)
        "#,
            &[("f", Want::Int(44))],
        );
    }

    /// Concatenation: xs + ys for two lists or two tuples
    #[test]
    fn concatenation() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2] + [3]
                t = (1,) + (2, 3)
                return len(xs) * 10 + t[2]
        "#,
            &[("f", Want::Int(33))],
        );
    }

    /// Dict merge: d1 | d2, in CPython's order
    #[test]
    fn dict_merge() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1, "b": 2} | {"c": 3, "a": 9}
                t = 0
                for k in d:
                    t = t * 10 + d[k]
                return t
        "#,
            &[("f", Want::Int(923))],
        );
    }

    /// The rest of the dict API: |=, update, pop, popitem, setdefault, copy, clear, fromkeys, del d[k]
    #[test]
    #[ignore = "open on the board"]
    fn rest_of_dict_api() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1}
                d.update({"b": 2})
                v = d.pop("a")
                return len(d) * 10 + v
        "#,
            &[("f", Want::Int(11))],
        );
    }
}

mod classes {
    use super::*;

    /// Class definitions with full WASM compilation
    #[test]
    fn class_definitions() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> int:
                a = Account("ann", 5)
                return a.deposit(3)
        "#,
            &[("f", Want::Int(8))],
        );
    }

    /// Instance method definitions with implicit self parameter
    #[test]
    fn methods_with_self() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> int:
                a = Account("ann", 1)
                a.deposit(2)
                return a.deposit(3)
        "#,
            &[("f", Want::Int(6))],
        );
    }

    /// Object instantiation via constructor calls (ClassName(args))
    #[test]
    fn instantiation() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> str:
                return Account("bob", 0).owner
        "#,
            &[("f", Want::Str("bob"))],
        );
    }

    /// Automatic __init__ method invocation during construction
    #[test]
    fn init_runs() {
        check(
            r#"
            class C:
                def __init__(self):
                    self.ready = 7

            def f() -> int:
                return C().ready
        "#,
            &[("f", Want::Int(7))],
        );
    }

    /// Method calls with proper dispatch (obj.method())
    #[test]
    fn method_dispatch() {
        check(
            r#"
            class A:
                def who(self) -> int:
                    return 1

            class B:
                def who(self) -> int:
                    return 2

            def f() -> int:
                return A().who() * 10 + B().who()
        "#,
            &[("f", Want::Int(12))],
        );
    }

    /// Instance attribute access getter (obj.attr)
    #[test]
    fn attribute_read() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> int:
                return Account("x", 42).balance
        "#,
            &[("f", Want::Int(42))],
        );
    }

    /// Instance attribute assignment setter (obj.attr = value)
    #[test]
    fn attribute_write() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> int:
                a = Account("x", 1)
                a.balance = 99
                return a.balance
        "#,
            &[("f", Want::Int(99))],
        );
    }

    /// Per-instance field storage with calculated memory offsets
    #[test]
    fn field_layout() {
        check(
            r#"
            class P:
                def __init__(self, a: int, b: float, c: str):
                    self.a = a
                    self.b = b
                    self.c = c

            def f() -> float:
                p = P(1, 2.5, "xyz")
                return p.a + p.b + len(p.c)
        "#,
            &[("f", Want::Float(6.5))],
        );
    }

    /// Heap-allocated instances: a distinct pointer per instantiation
    #[test]
    fn distinct_instances() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> int:
                a = Account("a", 1)
                b = Account("b", 1)
                r = 0
                if a is not b:
                    r += 1
                return r
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// Multiple live instances of one class with independent state
    #[test]
    fn independent_instances() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def f() -> int:
                a = Account("a", 1)
                b = Account("b", 10)
                a.deposit(5)
                return a.balance * 100 + b.balance
        "#,
            &[("f", Want::Int(610))],
        );
    }

    /// Instances as first-class values (arguments, factory returns)
    #[test]
    fn instances_as_values() {
        check(
            r#"
            class Account:
                def __init__(self, owner: str, balance: int):
                    self.owner = owner
                    self.balance = balance

                def deposit(self, n: int) -> int:
                    self.balance += n
                    return self.balance

            def make(n: int) -> Account:
                return Account("m", n)

            def total(a: Account, b: Account) -> int:
                return a.balance + b.balance

            def f() -> int:
                return total(make(2), make(3))
        "#,
            &[("f", Want::Int(5))],
        );
    }

    /// Single inheritance with super().__init__ / super().method()
    #[test]
    fn single_inheritance() {
        check(
            r#"
            class Base:
                def __init__(self, v: int):
                    self.v = v

                def show(self) -> int:
                    return self.v

            class Child(Base):
                def __init__(self, v: int):
                    super().__init__(v * 2)

                def show(self) -> int:
                    return super().show() + 1

            def f() -> int:
                return Child(5).show()
        "#,
            &[("f", Want::Int(11))],
        );
    }

    /// isinstance / issubclass over the class hierarchy
    #[test]
    fn isinstance_hierarchy() {
        check(
            r#"
            class A:
                pass

            class B(A):
                pass

            def f() -> int:
                r = 0
                if isinstance(B(), A):
                    r += 1
                if not isinstance(A(), B):
                    r += 10
                if issubclass(B, A):
                    r += 100
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// @dataclass with generated __init__ / __eq__ / __repr__
    #[test]
    fn dataclasses() {
        check(
            r#"
            from dataclasses import dataclass

            @dataclass
            class Pt:
                x: int
                y: int = 4

            def f() -> int:
                a = Pt(1)
                r = a.x * 10 + a.y
                if a == Pt(1, 4):
                    r += 100
                return r
        "#,
            &[("f", Want::Int(114))],
        );
    }

    /// Abstract base classes (abc.ABC + @abstractmethod)
    #[test]
    fn abstract_base_classes() {
        refused(
            r#"
            from abc import ABC, abstractmethod

            class Shape(ABC):
                @abstractmethod
                def area(self) -> int:
                    pass

            def f() -> int:
                s = Shape()
                return 1
        "#,
            "abstract",
        );
    }

    /// Collection fields hold instances: self.items = [] filled by append keeps its element class
    #[test]
    fn collection_fields_hold_instances() {
        check(
            r#"
            class Item:
                def __init__(self, w: int):
                    self.w = w

            class Bag:
                def __init__(self):
                    self.items = []

                def add(self, w: int) -> None:
                    item = Item(w)
                    self.items.append(item)

            def f() -> int:
                b = Bag()
                b.add(2)
                b.add(5)
                t = 0
                for it in b.items:
                    t += it.w
                return t
        "#,
            &[("f", Want::Int(7))],
        );
    }

    /// Collection fields grow through the field, so self.items.append(v) is visible to later reads
    #[test]
    fn collection_fields_grow() {
        check(
            r#"
            class Q:
                def __init__(self):
                    self.xs = [0]

                def push(self, v: int) -> None:
                    self.xs.append(v)

            def f() -> int:
                q = Q()
                for i in range(10):
                    q.push(i)
                return len(q.xs) * 10 + q.xs[10]
        "#,
            &[("f", Want::Int(119))],
        );
    }

    /// Rich comparisons on instances: <, <=, >, >= call __lt__/__le__/__gt__/__ge__, with reflection
    #[test]
    fn rich_comparisons() {
        check(
            r#"
            class V:
                def __init__(self, n: int):
                    self.n = n

                def __lt__(self, other) -> bool:
                    return self.n < other.n

            def f() -> int:
                a = V(1)
                b = V(2)
                r = 0
                if a < b:
                    r += 1
                if b > a:
                    r += 10
                return r
        "#,
            &[("f", Want::Int(11))],
        );
    }

    /// Virtual dispatch: a base method calling self.method() reaches the subclass override (methods, @property getters, __eq__)
    #[test]
    fn virtual_dispatch() {
        check(
            r#"
            class Media:
                def kind(self) -> str:
                    return "media"

                def describe(self) -> str:
                    return self.kind() + "!"

            class Video(Media):
                def kind(self) -> str:
                    return "video"

            def f() -> str:
                return Video().describe()
        "#,
            &[("f", Want::Str("video!"))],
        );
    }

    /// Instances of sibling classes in one list, dict, local, or field dispatch on their own class (typed as the nearest common base)
    #[test]
    fn sibling_instances() {
        check(
            r#"
            class Shape:
                def area(self) -> int:
                    return 0

            class Sq(Shape):
                def area(self) -> int:
                    return 4

            class Tri(Shape):
                def area(self) -> int:
                    return 3

            def f() -> int:
                t = 0
                for s in [Sq(), Tri(), Sq()]:
                    t = t * 10 + s.area()
                return t
        "#,
            &[("f", Want::Int(434))],
        );
    }
}

mod builtins {
    use super::*;

    /// Type conversions (int(), float(), str(), bool())
    #[test]
    fn type_conversions() {
        check(
            r#"
            def f() -> str:
                return str(int(3.9)) + str(int("42")) + str(int(bool(5))) + str(int(float(2)))
        "#,
            &[("f", Want::Str("34212"))],
        );
    }

    /// str() of a float, bool, or collection, and float() of a string (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn str_of_float_and_collections() {
        check(
            r#"
            def f() -> str:
                x = 1.5
                return str(x) + str([1, 2])
        "#,
            &[("f", Want::Str("1.5[1, 2]"))],
        );
    }

    /// len() - strings, lists, and dicts
    #[test]
    fn len_of_collections() {
        check(
            r#"
            def f() -> int:
                return len("abc") * 100 + len([1, 2]) * 10 + len({"a": 1})
        "#,
            &[("f", Want::Int(321))],
        );
    }

    /// len() of a range, and len() of anything without a length refused (TypeError in CPython)
    #[test]
    fn len_of_range() {
        check(
            r#"
            def f() -> int:
                return len(range(2, 10, 3)) * 10 + len(range(10, 2, -3))
        "#,
            &[("f", Want::Int(33))],
        );
    }

    /// isinstance() against builtin types (int, float, str, bool, list, dict, set, tuple, bytes) and tuples of types
    #[test]
    fn isinstance_builtin_types() {
        check(
            r#"
            def f() -> int:
                r = 0
                if isinstance(3, int):
                    r += 1
                if isinstance(True, int):
                    r += 10
                if isinstance("a", (int, str)):
                    r += 100
                if isinstance([1], dict):
                    r += 1000
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// bool(x) is the truth value (it passed x through, so bool(2) + 1 was 3)
    #[test]
    fn bool_truth_value() {
        check(
            r#"
            def f() -> int:
                return bool(2) + bool("") * 10 + bool([1]) * 100
        "#,
            &[("f", Want::Int(101))],
        );
    }

    /// print() evaluates its arguments but writes nothing: output needs a host interface
    #[test]
    #[ignore = "open on the board"]
    fn print_output() {
        // print() writes through the host interface once it has one, so a module
        // that prints imports what it writes with.
        let wasm = compile_with(
            "def f() -> int:\n    print(\"hello\")\n    return 1\n",
            false,
        );
        let module = wasmi::Module::new(&wasmi::Engine::default(), &wasm[..]).unwrap();
        assert!(module.imports().count() > 0, "print() reaches no host");
    }

    /// min() & max() - multiple arguments, all ints, all floats, or all strings, keeping the first of equals as CPython does
    #[test]
    fn min_max_arguments() {
        check(
            r#"
            def f() -> float:
                return min(2.5, 1.5) * 10 + max(1.0, 3.0)

            def g() -> str:
                return min("b", "a") + max("x", "y")
        "#,
            &[("f", Want::Float(18.0)), ("g", Want::Str("ay"))],
        );
    }

    /// min() & max() over a single iterable, with key= and default=
    #[test]
    #[ignore = "open on the board"]
    fn min_max_iterable() {
        check(
            r#"
            def f() -> int:
                return min([3, 1, 2])
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// sum() over lists/tuples with optional start value - real element loop
    #[test]
    fn sum_with_start() {
        check(
            r#"
            def f() -> int:
                return sum([1, 2, 3]) + sum((4, 5), 10)
        "#,
            &[("f", Want::Int(25))],
        );
    }

    /// sorted(iterable[, key][, reverse]) - stable, over ints, floats, strings, and tuples (a sequence or key whose type cannot be determined is refused, not guessed)
    #[test]
    fn sorted_builtin() {
        check(
            r#"
            from typing import List, Tuple

            def f() -> str:
                words: List[str] = ["pear", "fig", "apple"]
                nums: List[int] = [3, -1, 2]
                by_neg = sorted(nums, key=lambda x: -x)
                return "".join(sorted(words)) + str(by_neg[0])

            def g() -> int:
                pairs: List[Tuple[int, int]] = [(2, 1), (1, 9), (2, 0)]
                out = sorted(pairs, reverse=True)
                return out[0][1] * 10 + out[2][0]
        "#,
            &[("f", Want::Str("applefigpear3")), ("g", Want::Int(11))],
        );
    }

    /// int() parses a string, raising ValueError on bad input
    #[test]
    fn int_parses_strings() {
        check(
            r#"
            def f() -> int:
                a = int(" -42 ")
                try:
                    int("x1")
                    return 0
                except ValueError:
                    return a
        "#,
            &[("f", Want::Int(-42))],
        );
    }

    /// round(x[, ndigits]) exact, half-to-even, matching CPython
    #[test]
    fn round_half_even() {
        check(
            r#"
            def f() -> int:
                return round(2.5) * 100 + round(3.5) * 10 + round(-0.5)

            def g() -> float:
                return round(2.675, 2)
        "#,
            &[("f", Want::Int(240)), ("g", Want::Float(2.67))],
        );
    }

    /// abs()
    #[test]
    fn abs_builtin() {
        check(
            r#"
            def f() -> float:
                return abs(-3) + abs(-1.5)
        "#,
            &[("f", Want::Float(4.5))],
        );
    }
}

mod runtime_features {
    use super::*;

    /// Set storage with de-duplication & membership (in / not in)
    #[test]
    fn set_dedup_and_membership() {
        check(
            r#"
            def f() -> int:
                s = {1, 2, 2, 3}
                r = len(s) * 10
                if 2 in s:
                    r += 1
                if 9 not in s:
                    r += 100
                return r
        "#,
            &[("f", Want::Int(131))],
        );
    }

    /// Growable collections - lists and dicts reallocate instead of overflowing their region
    #[test]
    fn growable_collections() {
        check(
            r#"
            def f() -> int:
                xs = [1]
                for i in range(1000):
                    xs.append(i)
                return len(xs)
        "#,
            &[("f", Want::Int(1001))],
        );
    }

    /// Dict index assignment with update or append (dict[key] = value)
    #[test]
    fn dict_update_or_append() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1}
                d["a"] = 2
                d["b"] = 3
                return len(d) * 10 + d["a"]
        "#,
            &[("f", Want::Int(22))],
        );
    }

    /// Generator State Management - execution context suspension/resumption
    #[test]
    fn generator_state() {
        check(
            r#"
            def counter(n: int):
                i = 0
                while i < n:
                    yield i * i
                    i += 1

            def f() -> int:
                g = counter(4)
                a = next(g)
                b = next(g)
                return a + b * 10 + next(g) * 100
        "#,
            &[("f", Want::Int(410))],
        );
    }

    /// Dynamic Module Loading - runtime module loading and execution
    #[test]
    #[ignore = "open on the board"]
    fn dynamic_module_loading() {
        check(
            r#"
            import importlib

            def f() -> int:
                m = importlib.import_module("math")
                return 1
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// Closure Variable Capture Analysis - detecting and capturing closure variables
    #[test]
    fn closure_capture() {
        check(
            r#"
            def f() -> int:
                k = 3
                add = lambda x: x + k
                return add(4)
        "#,
            &[("f", Want::Int(7))],
        );
    }

    /// String Transformation in WASM - full character-by-char transformations
    #[test]
    fn string_transformation() {
        check(
            r#"
            def f(s: str) -> str:
                return s.upper()[::-1] + s.title() + s.replace("b", "XY").lower()

            def g() -> str:
                w = "ab" + "c d"
                return f(w)
        "#,
            &[("g", Want::Str("D CBAAbc Daxyc d"))],
        );
    }
}

mod stdlib {
    use super::*;

    /// Module constants: math (pi, e, tau, inf, nan), os (name, which is posix as on every CPython WebAssembly host, sep, pathsep, linesep, devnull, curdir, pardir, extsep), re flags, sys.maxsize (2**31 - 1 under 32-bit int), logging levels, datetime.MINYEAR / MAXYEAR
    #[test]
    fn module_constants() {
        check(
            r#"
            import math
            import os
            import re
            import sys
            import logging
            import datetime

            def f() -> float:
                return math.pi + math.e + math.tau

            def g() -> str:
                return os.name + os.sep + os.pathsep + os.linesep.strip() + os.curdir

            def h() -> int:
                return re.IGNORECASE + logging.WARNING + datetime.MAXYEAR + datetime.MINYEAR
        "#,
            &[
                ("f", Want::Float(12.143059789228424)),
                ("g", Want::Str("posix/:.")),
                ("h", Want::Int(10032)),
            ],
        );
    }

    /// re module, compile-time folding: sub and escape over constant arguments, where the pattern is one CPython and the compiler read alike (anything else refused)
    #[test]
    fn re_constant_folds() {
        check(
            r#"
            import re

            def f() -> str:
                return re.sub(r"(\w+)@(\w+)", r"\2 at \1", "me@host") + re.escape("a.b")
        "#,
            &[("f", Want::Str("host at mea\\.b"))],
        );
    }

    /// functools: @singledispatch with @f.register (static dispatch on the first argument's type), @total_ordering (derives the missing ordering methods)
    #[test]
    fn singledispatch_and_total_ordering() {
        check(
            r#"
            from functools import singledispatch, total_ordering

            @singledispatch
            def kind(x) -> int:
                return 0

            @kind.register
            def _(x: int) -> int:
                return 1

            @kind.register
            def _(x: str) -> int:
                return 2

            @total_ordering
            class V:
                def __init__(self, n: int):
                    self.n = n

                def __eq__(self, other) -> bool:
                    return self.n == other.n

                def __lt__(self, other) -> bool:
                    return self.n < other.n

            def f() -> int:
                r = kind(3) * 10 + kind("a")
                if V(2) >= V(1):
                    r += 100
                return r
        "#,
            &[("f", Want::Int(112))],
        );
    }

    /// functools: @lru_cache, @cache, @wraps (accepted, no observable effect)
    #[test]
    fn caching_decorators() {
        check(
            r#"
            from functools import cache, lru_cache

            @lru_cache(maxsize=None)
            def fib(n: int) -> int:
                if n < 2:
                    return n
                return fib(n - 1) + fib(n - 2)

            @cache
            def sq(n: int) -> int:
                return n * n

            def f() -> int:
                return fib(20) + sq(3)
        "#,
            &[("f", Want::Int(6774))],
        );
    }

    /// json module: dumps, loads, dump, load, JSONEncoder, JSONDecoder (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn json_module() {
        check(
            r#"
            import json

            def f() -> int:
                return len(json.dumps([1, 2, 3]))
        "#,
            &[("f", Want::Int(9))],
        );
    }

    /// math functions on runtime values: sqrt, sin, cos, tan, asin, acos, atan, atan2, sinh, cosh, tanh, exp, log, log10, log2, pow, floor, ceil, trunc, fabs, copysign, fmod, remainder, degrees, radians, hypot, factorial, gcd, isnan, isinf, isfinite (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn math_functions() {
        check(
            r#"
            import math

            def f(x: float) -> float:
                return math.sqrt(x)

            def g() -> float:
                return f(16.0)
        "#,
            &[("g", Want::Float(4.0))],
        );
    }
}

mod comprehensions {
    use super::*;

    /// List comprehensions ([x for x in list])
    #[test]
    fn list_comprehension() {
        check(
            r#"
            def f() -> int:
                ys = [x * 2 for x in [1, 2, 3]]
                return ys[2] * 10 + len(ys)
        "#,
            &[("f", Want::Int(63))],
        );
    }

    /// List comprehension filters ([x for x in list if condition])
    #[test]
    fn comprehension_filters() {
        check(
            r#"
            def f() -> int:
                return len([x for x in range(10) if x % 3 == 0])
        "#,
            &[("f", Want::Int(4))],
        );
    }

    /// Dict comprehensions ({k: v for k, v in items})
    #[test]
    fn dict_comprehension() {
        check(
            r#"
            def f() -> int:
                d = {k: v * 10 for k, v in [("a", 1), ("b", 2)]}
                return d["b"]
        "#,
            &[("f", Want::Int(20))],
        );
    }

    /// Set comprehensions ({x for x in list}) with dedup at construction
    #[test]
    fn set_comprehension() {
        check(
            r#"
            def f() -> int:
                return len({x % 3 for x in range(10)})
        "#,
            &[("f", Want::Int(3))],
        );
    }

    /// Multiple generators & nesting ([x for row in m for x in row])
    #[test]
    fn nested_comprehension() {
        check(
            r#"
            def f() -> int:
                m = [[1, 2], [3, 4]]
                flat = [x for row in m for x in row]
                return flat[3] * 10 + len(flat)
        "#,
            &[("f", Want::Int(44))],
        );
    }

    /// Generator expressions (x for x in list), materialized eagerly as lists
    #[test]
    fn generator_expressions() {
        check(
            r#"
            def f() -> int:
                return sum(x * x for x in [1, 2, 3])
        "#,
            &[("f", Want::Int(14))],
        );
    }

    /// Comprehensions over generators and iterator classes (in the first for clause)
    #[test]
    fn comprehension_over_generator() {
        check(
            r#"
            def gen(n: int):
                for i in range(n):
                    yield i * 2

            def f() -> int:
                ys = [v + 1 for v in gen(4)]
                return ys[3] * 10 + len(ys)
        "#,
            &[("f", Want::Int(74))],
        );
    }
}

mod functional {
    use super::*;

    /// Lambda functions (lambda x: x + 1) compiled to real functions
    #[test]
    fn lambdas() {
        check(
            r#"
            def f() -> int:
                double = lambda x: x * 2
                return double(21)
        "#,
            &[("f", Want::Int(42))],
        );
    }

    /// Closures capture the variable, not its value: a captured variable lives in a cell the enclosing function and every closure share
    #[test]
    fn closures_capture_variables() {
        check(
            r#"
            def f() -> int:
                k = 1
                get = lambda: k
                k = 5
                return get()
        "#,
            &[("f", Want::Int(5))],
        );
    }

    /// Closures capturing a float (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn float_captures() {
        check(
            r#"
            def f() -> float:
                k = 1.5
                g = lambda x: x * k
                return g(2)
        "#,
            &[("f", Want::Float(3.0))],
        );
    }

    /// Lambda parameters typed from their call sites, so indexing, len(), or a method call inside a lambda works (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn typed_lambda_parameters() {
        check(
            r#"
            def f() -> int:
                first = lambda kv: kv[1]
                return first((1, 5))
        "#,
            &[("f", Want::Int(5))],
        );
    }

    /// First-class closures: returned, passed as arguments, stored in collections, nested
    #[test]
    fn first_class_closures() {
        check(
            r#"
            def adder(n: int):
                return lambda x: x + n

            def apply(fn, v: int) -> int:
                return fn(v)

            def f() -> int:
                fs = [adder(1), adder(10)]
                first = fs[0]
                return apply(fs[1], 5) + first(1)
        "#,
            &[("f", Want::Int(17))],
        );
    }

    /// Higher-order functions (passing functions as arguments)
    #[test]
    fn higher_order_functions() {
        check(
            r#"
            def twice(fn, v: int) -> int:
                return fn(fn(v))

            def f() -> int:
                return twice(lambda x: x * 3, 2)
        "#,
            &[("f", Want::Int(18))],
        );
    }

    /// Callable type tracking for function objects
    #[test]
    fn callable_tracking() {
        check(
            r#"
            from typing import Callable

            def run(fn: Callable[[int], int], v: int) -> int:
                return fn(v)

            def f() -> int:
                return run(lambda x: x - 1, 10)
        "#,
            &[("f", Want::Int(9))],
        );
    }
}

mod generators {
    use super::*;

    /// Generator functions - yield suspends and resumes with state preserved
    #[test]
    fn generator_functions() {
        check(
            r#"
            def fib():
                a, b = 0, 1
                while True:
                    yield a
                    a, b = b, a + b

            def f() -> int:
                t = 0
                g = fib()
                for i in range(10):
                    t += next(g)
                return t
        "#,
            &[("f", Want::Int(88))],
        );
    }

    /// yield from delegation (ranges, lists, other generators)
    #[test]
    fn yield_from() {
        check(
            r#"
            def inner():
                yield 1
                yield 2

            def outer():
                yield from inner()
                yield from [3, 4]
                yield from range(5, 7)

            def f() -> int:
                t = 0
                for v in outer():
                    t = t * 10 + v
                return t
        "#,
            &[("f", Want::Int(123456))],
        );
    }

    /// next(), send() with x = yield resume values, and close()
    #[test]
    fn next_send_close() {
        check(
            r#"
            def acc():
                total = 0
                while True:
                    x = yield total
                    total += x

            def f() -> int:
                g = acc()
                next(g)
                g.send(5)
                r = g.send(10)
                g.close()
                return r
        "#,
            &[("f", Want::Int(15))],
        );
    }

    /// Iterator protocol (__iter__, __next__) on user classes with StopIteration
    #[test]
    fn iterator_protocol() {
        check(
            r#"
            class Count:
                def __init__(self, n: int):
                    self.i = 0
                    self.n = n

                def __iter__(self) -> "Count":
                    return self

                def __next__(self) -> int:
                    if self.i >= self.n:
                        raise StopIteration
                    self.i += 1
                    return self.i

            def f() -> int:
                t = 0
                for v in Count(4):
                    t += v
                return t
        "#,
            &[("f", Want::Int(10))],
        );
    }

    /// Generator type tracking (Generator[T])
    #[test]
    fn generator_type_tracking() {
        check(
            r#"
            from typing import Generator

            def halves(n: int) -> Generator[float, None, None]:
                for i in range(n):
                    yield i / 2

            def f() -> float:
                t = 0.0
                for v in halves(4):
                    t += v
                return t
        "#,
            &[("f", Want::Float(3.0))],
        );
    }

    /// range() function implementation - all variants
    #[test]
    fn range_variants() {
        check(
            r#"
            def f() -> int:
                t = 0
                for i in range(3):
                    t += i
                for i in range(2, 5):
                    t += i * 10
                for i in range(10, 0, -4):
                    t += i * 100
                return t
        "#,
            &[("f", Want::Int(1893))],
        );
    }

    /// for loop iteration over ranges with step support
    #[test]
    fn range_steps() {
        check(
            r#"
            def f() -> int:
                t = 0
                for i in range(1, 20, 6):
                    t = t * 100 + i
                return t
        "#,
            &[("f", Want::Int(1071319))],
        );
    }

    /// Tuple targets in for loops (for a, b in pairs, star targets)
    #[test]
    fn tuple_targets() {
        check(
            r#"
            def f() -> int:
                t = 0
                for a, b in [(1, 2), (3, 4)]:
                    t += a * b
                for first, *rest in [[1, 2, 3]]:
                    t += first * 100 + len(rest) * 10
                return t
        "#,
            &[("f", Want::Int(134))],
        );
    }

    /// enumerate(), zip(), and dict.items()/.keys()/.values() in for loops
    #[test]
    fn enumerate_zip_items() {
        check(
            r#"
            def f() -> int:
                t = 0
                for i, x in enumerate([5, 6]):
                    t += i * x
                for a, b in zip([1, 2], [10, 20, 30]):
                    t += a * b
                for k, v in {"a": 1, "bb": 2}.items():
                    t += len(k) * v * 100
                return t
        "#,
            &[("f", Want::Int(556))],
        );
    }

    /// yield inside try / with, generator methods, and GeneratorExit / finally on close() (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn yield_in_try() {
        check(
            r#"
            def gen():
                try:
                    yield 1
                finally:
                    pass

            def f() -> int:
                t = 0
                for v in gen():
                    t += v
                return t
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// len() of a generator is refused, as CPython raises TypeError
    #[test]
    fn len_of_generator_refused() {
        refused(
            r#"
            def gen():
                yield 1

            def f() -> int:
                return len(gen())
        "#,
            "has no len()",
        );
    }

    /// zip() with a generator after its first argument (refused today)
    #[test]
    #[ignore = "open on the board"]
    fn zip_generator_later() {
        check(
            r#"
            def gen():
                yield 1
                yield 2

            def f() -> int:
                t = 0
                for a, b in zip([1, 2], gen()):
                    t += a * b
                return t
        "#,
            &[("f", Want::Int(5))],
        );
    }
}

mod error_handling {
    use super::*;

    /// try / except / finally blocks
    #[test]
    fn try_except_finally_blocks() {
        check(
            r#"
            def f(n: int) -> int:
                r = 0
                try:
                    r = 10 // n
                except ZeroDivisionError:
                    r = -1
                finally:
                    r += 100
                return r

            def g() -> int:
                return f(0) * 1000 + f(5)
        "#,
            &[("g", Want::Int(99102))],
        );
    }

    /// raise transfers control: it leaves the block, the try body, and the loop it is raised in
    #[test]
    fn raise_transfers_control() {
        check(
            r#"
            def f() -> int:
                r = 0
                for i in range(5):
                    try:
                        if i == 2:
                            raise ValueError("stop")
                        r += 1
                    except ValueError:
                        r += 100
                        break
                return r
        "#,
            &[("f", Want::Int(102))],
        );
    }

    /// Propagation across calls: an exception travels out of a callee into the caller's handler
    #[test]
    fn propagation_across_calls() {
        check(
            r#"
            def inner(n: int) -> int:
                if n < 0:
                    raise ValueError("neg")
                return n

            def middle(n: int) -> int:
                return inner(n) + 1

            def f() -> int:
                try:
                    return middle(-1)
                except ValueError:
                    return 42
        "#,
            &[("f", Want::Int(42))],
        );
    }

    /// Handler matching by type, except (A, B) for either, except Exception for any; unmatched handlers pass it on
    #[test]
    fn handler_matching() {
        check(
            r#"
            def classify(n: int) -> int:
                try:
                    if n == 0:
                        raise KeyError("k")
                    if n == 1:
                        raise IndexError("i")
                    raise TypeError("t")
                except (KeyError, IndexError):
                    return 1
                except Exception:
                    return 2

            def f() -> int:
                return classify(0) * 100 + classify(1) * 10 + classify(2)
        "#,
            &[("f", Want::Int(112))],
        );
    }

    /// finally and __exit__ run while an exception propagates, and on return / break / continue
    #[test]
    fn finally_runs_on_every_path() {
        check(
            r#"
            from typing import List

            def work(log: List[int]) -> int:
                for i in range(3):
                    try:
                        if i == 1:
                            continue
                        if i == 2:
                            return 9
                    finally:
                        log.append(i)
                return 0

            def f() -> int:
                log: List[int] = []
                r = work(log)
                return r * 1000 + len(log) * 100 + log[2]
        "#,
            &[("f", Want::Int(9302))],
        );
    }

    /// An uncaught exception unwinds out of the program and traps
    #[test]
    fn uncaught_exception_traps() {
        check(
            r#"
            def f() -> int:
                raise RuntimeError("boom")
        "#,
            &[("f", Want::Traps)],
        );
    }

    /// Exception objects: the message in raise ValueError("...") is kept and except ... as e binds the exception (e can only be re-raised today; any other use is refused)
    #[test]
    #[ignore = "open on the board"]
    fn exception_objects() {
        check(
            r#"
            def f() -> int:
                try:
                    raise ValueError("boom")
                except ValueError as e:
                    return len(str(e))
        "#,
            &[("f", Want::Int(4))],
        );
    }

    /// raise and raise e inside a handler re-raise the caught exception's type
    #[test]
    fn reraise_keeps_type() {
        check(
            r#"
            def f() -> int:
                try:
                    try:
                        raise ValueError("a")
                    except ValueError as e:
                        raise e
                except TypeError:
                    return 1
                except ValueError:
                    return 2
                return 0
        "#,
            &[("f", Want::Int(2))],
        );
    }

    /// Subclass matching: except MyBase does not catch a subclass of it (Exception / BaseException catch everything)
    #[test]
    #[ignore = "open on the board"]
    fn subclass_matching() {
        check(
            r#"
            class AppError(Exception):
                pass

            class DbError(AppError):
                pass

            def f() -> int:
                try:
                    raise DbError("x")
                except AppError:
                    return 1
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// Runtime faults raise catchable exceptions: IndexError for an out-of-range index, KeyError for a missing key, ZeroDivisionError
    #[test]
    fn runtime_faults_catchable() {
        check(
            r#"
            def f() -> int:
                xs = [1]
                d = {"a": 1}
                n = 0
                r = 0
                try:
                    r += xs[5]
                except IndexError:
                    r += 1
                try:
                    r += d["z"]
                except KeyError:
                    r += 10
                try:
                    r += 1 // n
                except ZeroDivisionError:
                    r += 100
                return r
        "#,
            &[("f", Want::Int(111))],
        );
    }

    /// int() of a bad string raises a catchable ValueError
    #[test]
    fn int_bad_string_raises() {
        check(
            r#"
            def f() -> int:
                try:
                    return int("4x")
                except ValueError:
                    return -7
        "#,
            &[("f", Want::Int(-7))],
        );
    }
}

mod advanced_types {
    use super::*;

    /// Bytes type & binary data handling
    #[test]
    fn bytes_values() {
        check(
            r#"
            def f() -> int:
                b = b"hi"
                return b[0] + len(b)
        "#,
            &[("f", Want::Int(106))],
        );
    }

    /// Bytes literals, indexing, slicing, concatenation
    #[test]
    fn bytes_operations() {
        check(
            r#"
            def f() -> int:
                b = b"abc" + b"de"
                return len(b[1:4]) * 1000 + b[-1]
        "#,
            &[("f", Want::Int(3101))],
        );
    }

    /// Set type ({1, 2, 3}) & literals
    #[test]
    fn set_literals() {
        check(
            r#"
            def f() -> int:
                s = {3, 1, 3}
                return len(s)
        "#,
            &[("f", Want::Int(2))],
        );
    }

    /// Empty sets with type annotations
    #[test]
    fn empty_annotated_sets() {
        check(
            r#"
            from typing import Set

            def f() -> int:
                s: Set[str] = set()
                s.add("a")
                s.add("a")
                return len(s)
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// Set mutation: add, remove, discard
    #[test]
    fn set_add_remove_discard() {
        check(
            r#"
            def f() -> int:
                s = {1, 2}
                s.add(3)
                s.remove(1)
                s.discard(7)
                r = len(s) * 10
                if 3 in s:
                    r += 1
                return r
        "#,
            &[("f", Want::Int(21))],
        );
    }

    /// Set operations: union, intersection, difference, symmetric difference as methods and operators, subset and equality tests (a method takes a set argument)
    #[test]
    fn set_operations() {
        check(
            r#"
            def f() -> int:
                a = {1, 2, 3}
                b = {2, 3, 4}
                r = len(a | b) * 1000 + len(a & b) * 100 + len(a ^ b) * 10 + len(a - b)
                if a.issubset(a | b):
                    r += 10000
                return r
        "#,
            &[("f", Want::Int(14221))],
        );
    }

    /// Tuple literals with variable expressions (a, b, c)
    #[test]
    fn tuple_literals() {
        check(
            r#"
            def f() -> int:
                a = 1
                b = 2
                t = (a, b, a + b)
                return t[2]
        "#,
            &[("f", Want::Int(3))],
        );
    }

    /// Tuple indexing with type tracking
    #[test]
    fn tuple_indexing() {
        check(
            r#"
            def f() -> float:
                t = (2.5, "abc")
                u = (1, "xy", False)
                return t[0] + len(t[1]) + u[0] + len(u[1])
        "#,
            &[("f", Want::Float(8.5))],
        );
    }

    /// Heterogeneous tuples with mixed types
    #[test]
    fn heterogeneous_tuples() {
        check(
            r#"
            from typing import Tuple

            def pair() -> Tuple[str, float]:
                return ("x", 0.5)

            def f() -> float:
                p = pair()
                return len(p[0]) + p[1]
        "#,
            &[("f", Want::Float(1.5))],
        );
    }

    /// Tuple unpacking & assignment (a, b = (1, 2))
    #[test]
    fn tuple_unpack_assign() {
        check(
            r#"
            def f() -> int:
                a, b = (1, 2)
                a, b = b, a
                return a * 10 + b
        "#,
            &[("f", Want::Int(21))],
        );
    }

    /// Extended (starred) unpacking (a, *b, c = xs)
    #[test]
    fn starred_unpacking() {
        check(
            r#"
            def f() -> int:
                a, *b, c = [1, 2, 3, 4]
                return a * 100 + len(b) * 10 + c
        "#,
            &[("f", Want::Int(124))],
        );
    }

    /// Tuple .count()
    #[test]
    fn tuple_count() {
        check(
            r#"
            def f() -> int:
                return (1, 2, 1).count(1)
        "#,
            &[("f", Want::Int(2))],
        );
    }

    /// Tuple .index() (a missing value traps where CPython raises ValueError)
    #[test]
    fn tuple_index() {
        check(
            r#"
            def f() -> int:
                return (4, 5, 6).index(6)

            def g() -> int:
                return (4, 5).index(9)
        "#,
            &[("f", Want::Int(2)), ("g", Want::Traps)],
        );
    }

    /// Named tuples - namedtuple() factory (refused today, see STANDARD LIBRARY)
    #[test]
    #[ignore = "open on the board"]
    fn named_tuples() {
        check(
            r#"
            from collections import namedtuple

            def f() -> int:
                P = namedtuple("P", "x y")
                return P(1, 2).y
        "#,
            &[("f", Want::Int(2))],
        );
    }

    /// String formatting: f-strings, .format() with automatic and positional fields, .2f-style precision
    #[test]
    fn string_formatting() {
        check(
            r#"
            def f(x: float, n: int) -> str:
                return f"{x:.2f}" + "{}-{}".format(n, n + 1) + "{0}{0}".format(n)

            def g() -> str:
                return f(3.14159, 7)
        "#,
            &[("g", Want::Str("3.147-877"))],
        );
    }
}

mod advanced_o_o_p {
    use super::*;

    /// Property decorators (@property, @<name>.setter)
    #[test]
    fn properties() {
        check(
            r#"
            class T:
                def __init__(self):
                    self._c = 0

                @property
                def c(self) -> int:
                    return self._c

                @c.setter
                def c(self, v: int) -> None:
                    self._c = v * 2

            def f() -> int:
                t = T()
                t.c = 5
                return t.c
        "#,
            &[("f", Want::Int(10))],
        );
    }

    /// Static & class methods (@staticmethod, @classmethod with cls factories)
    #[test]
    fn static_and_class_methods() {
        check(
            r#"
            class P:
                def __init__(self, v: int):
                    self.v = v

                @staticmethod
                def double(x: int) -> int:
                    return x * 2

                @classmethod
                def origin(cls) -> "P":
                    return cls(0)

            def f() -> int:
                return P.double(4) + P.origin().v
        "#,
            &[("f", Want::Int(8))],
        );
    }

    /// Multiple inheritance & method resolution
    #[test]
    #[ignore = "open on the board"]
    fn multiple_inheritance() {
        check(
            r#"
            class A:
                def who(self) -> int:
                    return 1

            class B:
                def who(self) -> int:
                    return 2

            class C(A, B):
                pass

            def f() -> int:
                return C().who()
        "#,
            &[("f", Want::Int(1))],
        );
    }
}

mod memory {
    use super::*;

    /// Bump allocator with runtime memory growth
    #[test]
    fn memory_growth() {
        check(
            r#"
            def f() -> int:
                t = 0
                for i in range(200):
                    xs = [i] * 100
                    t += len(xs)
                return t
        "#,
            &[("f", Want::Int(20000))],
        );
    }

    /// Collections carry a capacity and reallocate instead of overflowing their region
    #[test]
    fn collection_capacity() {
        check(
            r#"
            def f() -> int:
                a = [1, 2]
                b = [100, 200]
                for i in range(50):
                    a.append(i)
                return b[0] + b[1] + len(a)
        "#,
            &[("f", Want::Int(352))],
        );
    }

    /// Pointer identity across growth (a grown collection visible through every alias)
    #[test]
    fn identity_across_growth() {
        check(
            r#"
            def f() -> int:
                a = [1]
                alias = a
                for i in range(100):
                    a.append(i)
                return len(alias)
        "#,
            &[("f", Want::Int(101))],
        );
    }
}

mod imports {
    use super::*;

    /// Import statement parsing & IR generation
    #[test]
    fn import_statements() {
        check(
            r#"
            import math

            def f() -> float:
                return math.pi
        "#,
            &[("f", Want::Float(std::f64::consts::PI))],
        );
    }

    /// From-import with named imports
    #[test]
    fn from_imports() {
        check(
            r#"
            from math import pi, e

            def f() -> float:
                return pi + e
        "#,
            &[("f", Want::Float(5.859874482048838))],
        );
    }

    /// Conditional imports in try/except blocks
    #[test]
    fn conditional_imports() {
        check(
            r#"
            try:
                import math
            except ImportError:
                import cmath

            def f() -> float:
                return math.tau
        "#,
            &[("f", Want::Float(std::f64::consts::TAU))],
        );
    }

    /// Dynamic imports (__import__, importlib) (refused at compile time today)
    #[test]
    #[ignore = "open on the board"]
    fn dynamic_imports() {
        check(
            r#"
            def f() -> int:
                m = __import__("math")
                return 1
        "#,
            &[("f", Want::Int(1))],
        );
    }

    /// Star imports detection (from X import *)
    #[test]
    fn star_imports_refused() {
        let err = try_compile("from math import *\n\ndef f() -> int:\n    return 1\n")
            .expect_err("refused");
        assert!(err.contains("import *"), "{err}");
    }

    /// Module execution and loading
    #[test]
    fn module_execution() {
        let dir = project("modexec", &[
            ("config.py", "from typing import List\n\nLIMITS: List[int] = [3, 4]\nTOTAL = 3 + 4\n"),
            ("main.py", "import config\n\ndef f() -> int:\n    return config.TOTAL * 10 + len(config.LIMITS)\n"),
        ]);
        let (instance, mut store) = instantiate_file(&dir.join("main.py"));
        assert_eq!(call_instance_i32(&instance, &mut store, "f"), 72);
    }

    /// User-defined module files - import mod / from mod import f resolved from sibling .py files and statically linked
    #[test]
    fn user_module_files() {
        let dir = project(
            "usermods",
            &[
                (
                    "geometry.py",
                    "def area(w: int, h: int) -> int:\n    return w * h\n",
                ),
                (
                    "main.py",
                    "from geometry import area\n\ndef f() -> int:\n    return area(3, 5)\n",
                ),
            ],
        );
        let (instance, mut store) = instantiate_file(&dir.join("main.py"));
        assert_eq!(call_instance_i32(&instance, &mut store, "f"), 15);
    }

    /// Namespace access on user modules (mod.f(), mod.CONST, mod.ClassName()) and aliases (import mod as m, from mod import f as g)
    #[test]
    fn module_namespaces_and_aliases() {
        let dir = project("namespaces", &[
            ("shapes.py", "SIDES = 4\n\nclass Sq:\n    def __init__(self, s: int):\n        self.s = s\n\ndef perim(n: int) -> int:\n    return n * SIDES\n"),
            ("main.py", "import shapes as sh\nfrom shapes import perim as p\n\ndef f() -> int:\n    return sh.perim(2) * 100 + p(1) * 10 + sh.Sq(sh.SIDES).s\n"),
        ]);
        let (instance, mut store) = instantiate_file(&dir.join("main.py"));
        assert_eq!(call_instance_i32(&instance, &mut store, "f"), 844);
    }

    /// Module caching - a module imported through several paths compiles once
    #[test]
    fn module_caching() {
        let dir = project("diamond", &[
            ("base.py", "def one() -> int:\n    return 1\n"),
            ("left.py", "from base import one\n\ndef l() -> int:\n    return one() + 10\n"),
            ("right.py", "from base import one\n\ndef r() -> int:\n    return one() + 100\n"),
            ("main.py", "from left import l\nfrom right import r\n\ndef f() -> int:\n    return l() + r()\n"),
        ]);
        let (instance, mut store) = instantiate_file(&dir.join("main.py"));
        assert_eq!(call_instance_i32(&instance, &mut store, "f"), 112);
    }
}

mod project_mgmt {
    use super::*;

    /// Multi-file compilation to single WASM
    #[test]
    fn multi_file_compilation() {
        let wasm = try_compile_multi(&[
            (
                "shapes.py",
                "def square(n: int) -> int:\n    return n * n\n",
            ),
            (
                "main.py",
                "from shapes import square\n\ndef area() -> int:\n    return square(6)\n",
            ),
        ])
        .expect("two files merge into one module");
        let (instance, mut store) = instantiate_wasm(&wasm);
        assert_eq!(call_instance_i32(&instance, &mut store, "area"), 36);
    }

    /// Dependency analysis & circular detection
    #[test]
    fn dependency_order_and_cycles() {
        let dir = project(
            "cycle",
            &[
                ("a.py", "import b\n\ndef fa() -> int:\n    return 1\n"),
                ("b.py", "import a\n\ndef fb() -> int:\n    return 2\n"),
                ("c.py", "import a\n\ndef fc() -> int:\n    return 3\n"),
            ],
        );
        let project =
            waspy::analysis::project::PythonProject::from_directory(&dir).expect("analysed");
        assert!(project.dependencies["c"].contains("a"), "c depends on a");
        let ordered = project
            .get_ordered_files()
            .expect("a cycle is ordered, not an error");
        assert_eq!(ordered.len(), 3);
    }

    /// Entry point detection (__main__.py)
    #[test]
    fn entry_point_detection() {
        let info = waspy::ir::detect_entry_points(
            "def main() -> int:\n    return 0\n",
            Some(std::path::Path::new("pkg/__main__.py")),
        )
        .expect("detected")
        .expect("a __main__.py is an entry point");
        assert_eq!(info.main_function_name, "main");
        let guarded =
            "def main() -> int:\n    return 0\n\nif __name__ == \"__main__\":\n    main()\n";
        assert!(waspy::ir::detect_entry_points(guarded, None)
            .expect("detected")
            .is_some());
    }

    /// Config parsing (setup.py, pyproject.toml)
    #[test]
    fn config_parsing() {
        let config =
            waspy::core::config::load_project_config(examples_dir().join("calculator_project"))
                .expect("setup.py parses");
        assert_eq!(config.name, "calculator_project");
        assert_eq!(config.version, "1.0.0");
        let dir = project(
            "pyproject",
            &[(
                "pyproject.toml",
                "[project]\nname = \"widgets\"\nversion = \"2.3.4\"\n",
            )],
        );
        let config = waspy::core::config::load_project_config(&dir).expect("pyproject.toml parses");
        assert_eq!(
            (config.name.as_str(), config.version.as_str()),
            ("widgets", "2.3.4")
        );
    }
}

mod optimization {
    use super::*;

    /// WebAssembly optimization (Binaryen)
    #[test]
    fn binaryen_optimization() {
        let src = "def fact(n: int) -> int:\n    r = 1\n    for i in range(2, n + 1):\n        r *= i\n    return r\n\ndef f() -> int:\n    return fact(10)\n";
        let plain = compile_with(src, false);
        let optimized = compile_with(src, true);
        assert!(
            optimized.len() <= plain.len(),
            "optimization does not grow the module"
        );
        for wasm in [plain, optimized] {
            let (instance, mut store) = instantiate_wasm(&wasm);
            assert_eq!(call_instance_i32(&instance, &mut store, "f"), 3628800);
        }
    }

    /// Compiler options & configuration (reviewed API: optimize + verbosity)
    #[test]
    fn compiler_options() {
        let options = waspy::CompilerOptions {
            optimize: false,
            verbosity: waspy::Verbosity::Quiet,
        };
        let wasm =
            waspy::compile_python_to_wasm_with_options("def f() -> int:\n    return 3\n", &options)
                .expect("compiles");
        let (instance, mut store) = instantiate_wasm(&wasm);
        assert_eq!(call_instance_i32(&instance, &mut store, "f"), 3);
    }

    /// Error handling with located messages (line/column, function, hints)
    #[test]
    fn located_errors() {
        let err = try_compile("def f() -> int:\n    x = 1\n    return x +\n")
            .expect_err("a syntax error");
        assert!(err.contains("line 3"), "names the line: {err}");
        let err =
            try_compile("def f() -> int:\n    return undefined_name(1)\n").expect_err("refused");
        assert!(err.contains("in function 'f'"), "names the function: {err}");
        let err =
            try_compile("def f(n: int) -> str:\n    return \"%d!\" % n\n").expect_err("refused");
        assert!(err.contains("Hint:"), "carries a hint: {err}");
    }

    /// A call to an undefined function, or an attribute read through an untyped value, is a compile error instead of answering 0
    #[test]
    fn undefined_names_refused() {
        let err = try_compile("def f() -> int:\n    return nope(1)\n").expect_err("refused");
        assert!(err.contains("nope"), "{err}");
        let err = try_compile("def f(x) -> int:\n    return x.v\n").expect_err("refused");
        assert!(err.contains("cannot read attribute 'v'"), "{err}");
    }

    /// Early validation - unsupported syntax rejected before codegen
    #[test]
    fn early_validation() {
        let err = try_compile("async def f():\n    return 1\n").expect_err("refused");
        assert!(err.contains("async functions are not supported"), "{err}");
    }

    /// A parse or lowering failure fails the build naming the file, rather than skipping it with a warning
    #[test]
    fn failures_name_the_file() {
        let err = try_compile_multi(&[
            ("good.py", "def g() -> int:\n    return 1\n"),
            ("broken.py", "def b(:\n"),
        ])
        .expect_err("one broken file fails the build");
        assert!(err.contains("broken.py"), "{err}");
    }

    /// Integration test suite - every example compiled, run, and asserted in CI
    #[test]
    fn every_example_compiles() {
        for path in example_python_files() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if MULTI_FILE_ONLY.contains(&name.as_str()) {
                continue;
            }
            let wasm = waspy::compile_python_file_with_options(
                &path,
                &waspy::CompilerOptions {
                    optimize: false,
                    ..waspy::CompilerOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
            try_instantiate(&wasm).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    /// End-to-end programs asserted under Node and wasmtime, optimized and unoptimized, as a CI gate
    #[test]
    fn end_to_end_programs_have_checkers() {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime");
        let checkers: Vec<_> = std::fs::read_dir(&fixtures).expect("fixtures").collect();
        assert!(
            checkers.len() >= 6,
            "every end-to-end program has a runtime checker"
        );
        let justfile = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("justfile"),
        )
        .unwrap();
        assert!(
            justfile.contains("verify_runtime_node.mjs")
                && justfile.contains("verify_runtime_wasmtime.py")
        );
    }

    /// Comment preservation - python.comments custom section, survives optimization
    #[test]
    fn comment_preservation() {
        let src = "# keep-this-comment\ndef f() -> int:\n    return 1  # and this one\n";
        for optimize in [false, true] {
            let wasm = compile_with(src, optimize);
            assert!(
                contains(&wasm, waspy::compiler::COMMENTS_SECTION_NAME.as_bytes()),
                "section kept"
            );
            assert!(
                contains(&wasm, b"keep-this-comment"),
                "comment text kept (optimize: {optimize})"
            );
        }
    }

    /// Memory layout: equal string and bytes literals share one blob in the data segment
    #[test]
    fn literals_share_one_blob() {
        let src = "def a() -> str:\n    return \"a-distinct-literal\"\n\ndef b() -> str:\n    return \"a-distinct-literal\"\n";
        let wasm = compile_with(src, false);
        let count = wasm
            .windows(18)
            .filter(|w| *w == b"a-distinct-literal")
            .count();
        assert_eq!(count, 1, "the literal is stored once");
        assert_eq!(call_str(src, "a"), call_str(src, "b"));
    }
}

mod file_io {
    use super::*;

    const FILE_SRC: &str = r#"def write() -> int:
    f = open("notes.txt", "w")
    n = f.write("hello")
    f.close()
    return n

def append() -> int:
    with open("notes.txt", "a") as f:
        f.write("!!")
        f.flush()
    return 0

def read_all() -> int:
    with open("notes.txt") as f:
        return len(f.read())

def read_some() -> int:
    f = open("notes.txt", "r")
    s = f.read(3)
    f.close()
    return len(s)
"#;

    /// open(path, mode) via the documented waspy_host import interface (r, w, a, b, + modes folded to flags)
    #[test]
    fn open_with_modes() {
        let (instance, mut store) = instantiate_with_host_fs(FILE_SRC);
        assert_eq!(call_host_fs_i32(&instance, &mut store, "write"), 5);
        call_host_fs_i32(&instance, &mut store, "append");
        assert_eq!(store.data().files["notes.txt"], b"hello!!".to_vec());
    }

    /// read() / read(n) returning strings (64 KiB default cap per call)
    #[test]
    fn read_and_read_n() {
        let (instance, mut store) = instantiate_with_host_fs(FILE_SRC);
        call_host_fs_i32(&instance, &mut store, "write");
        assert_eq!(call_host_fs_i32(&instance, &mut store, "read_all"), 5);
        assert_eq!(call_host_fs_i32(&instance, &mut store, "read_some"), 3);
    }

    /// write(s) returning bytes written; close() and flush()
    #[test]
    fn write_close_flush() {
        let (instance, mut store) = instantiate_with_host_fs(FILE_SRC);
        assert_eq!(call_host_fs_i32(&instance, &mut store, "write"), 5);
        call_host_fs_i32(&instance, &mut store, "append");
        assert_eq!(call_host_fs_i32(&instance, &mut store, "read_all"), 7);
    }

    /// with open(...) as f: context-manager form (desugared to open/body/close)
    #[test]
    fn with_open() {
        let (instance, mut store) = instantiate_with_host_fs(FILE_SRC);
        call_host_fs_i32(&instance, &mut store, "write");
        call_host_fs_i32(&instance, &mut store, "append");
        assert_eq!(call_host_fs_i32(&instance, &mut store, "read_all"), 7);
    }

    /// Import section emitted only when open() is used - other modules keep zero imports
    #[test]
    fn imports_only_with_open() {
        let engine = wasmi::Engine::default();
        let plain = wasmi::Module::new(
            &engine,
            &compile_with("def f() -> int:\n    return 1\n", false)[..],
        )
        .unwrap();
        assert_eq!(plain.imports().count(), 0);
        let io = wasmi::Module::new(&engine, &compile_with(FILE_SRC, false)[..]).unwrap();
        assert!(io.imports().count() > 0 && io.imports().all(|i| i.module() == "waspy_host"));
    }
}
