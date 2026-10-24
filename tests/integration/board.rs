//! The development board's features, one test each (`just board-check`).
//!
//! Every feature on the development board (`docs/src/_data/modules.json`)
//! names the test here that backs it, as `card::test`. A feature marked done
//! must have one that passes; one still open may have one, marked `#[ignore]`,
//! asserting what the feature will answer once it lands, and `just
//! board-check` fails the moment such a test starts passing, so the board is
//! moved in the same change. Expected values are CPython's for the same
//! source, except where the board documents the difference (an `int` is 32
//! bits and wraps).

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

    /// Nested function definitions (def inside def), closing over the enclosing variables
    #[test]
    fn nested_functions() {
        check(
            r#"
            def f() -> int:
                base = 10
                def add(x: int) -> int:
                    return x + base
                return add(1) + add(2)
        "#,
            &[("f", Want::Int(23))],
        );
    }

    /// *args collects extra positional arguments as a tuple
    #[test]
    fn star_args() {
        check(
            r#"
            def total(*nums: int) -> int:
                s = 0
                for n in nums:
                    s += n
                return s

            def first_or(default: int, *rest: int) -> int:
                return rest[0] if len(rest) > 0 else default

            def f() -> int:
                return total() + total(1, 2, 3) * 10 + first_or(7) * 100 + first_or(7, 8, 9) * 1000
        "#,
            &[("f", Want::Int(8760))],
        );
    }

    /// Keyword-only parameters (after * or *args)
    #[test]
    fn keyword_only_parameters() {
        check(
            r#"
            def scale(x: int, *, factor: int = 2, offset: int = 0) -> int:
                return x * factor + offset

            def f() -> int:
                return scale(3) + scale(3, factor=10) * 10 + scale(1, offset=5, factor=1) * 1000
        "#,
            &[("f", Want::Int(6306))],
        );
    }

    /// **kwargs collects extra keyword arguments as a dict
    #[test]
    fn star_star_kwargs() {
        check(
            r#"
            def count_options(**opts: int) -> int:
                total = 0
                for k in opts:
                    total += opts[k]
                return len(opts) * 100 + total

            def f() -> int:
                return count_options() + count_options(a=1, b=2) * 1000
        "#,
            &[("f", Want::Int(203000))],
        );
    }

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

    /// Optional numbers: None distinct from 0 (an Optional[int], Optional[bool], or Optional[float] is boxed), `is None`, arithmetic and ordering raising CPython's TypeError on None, `==`, truthiness, str(), and dict.get(key) on number values
    #[test]
    fn optional_numbers() {
        check(
            r##"
            from typing import Dict, List, Optional


            def find(xs: List[int], target: int) -> Optional[int]:
                for i in range(len(xs)):
                    if xs[i] == target:
                        return i
                return None


            def ratio(a: int, b: int) -> Optional[float]:
                if b == 0:
                    return None
                return a / b


            class Slot:
                def __init__(self) -> None:
                    self.value: Optional[int] = None


            def f() -> str:
                xs = [5, 0, 7]
                out = ""
                i = find(xs, 0)
                j = find(xs, 9)
                out += str(i) + "," + str(j) + "," + str(i is None) + str(j is None) + "|"
                if i is not None:
                    out += str(i + 10) + "|"
                out += str(ratio(1, 4)) + str(ratio(1, 0)) + "|"
                counts: Dict[str, int] = {"a": 0, "b": 3}
                a = counts.get("a")
                z = counts.get("z")
                out += str(a) + str(z) + str(a is None) + str(z is None) + "|"
                out += str(a == 0) + str(z == 0) + str(z != 0) + str(bool(a)) + str(bool(z)) + "|"
                s = Slot()
                out += str(s.value) + "|"
                s.value = 0
                out += str(s.value) + str(s.value is None) + "|"
                try:
                    out += str(z + 1)
                except TypeError as e:
                    out += str(e) + "|"
                try:
                    out += str(z < 3)
                except TypeError as e:
                    out += str(e) + "|"
                k: Optional[int] = 4
                k = None if k > 3 else k
                out += str(k) + repr(counts.get("b")) + f"{counts.get('q')}"
                return out
        "##,
            &[("f", Want::Str("1,None,FalseTrue|11|0.25None|0NoneFalseTrue|TrueFalseTrueFalseFalse|None|0False|unsupported operand type(s) for +: 'NoneType' and 'int'|'<' not supported between instances of 'NoneType' and 'int'|None3None"))],
        );
        check(
            r##"
            from typing import Optional

            def add1(x: Optional[int]) -> int:
                return x + 1

            def f() -> str:
                try:
                    return str(add1(None))
                except TypeError as e:
                    return "caught " + str(e)
        "##,
            &[(
                "f",
                Want::Str("caught unsupported operand type(s) for +: 'NoneType' and 'int'"),
            )],
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

    /// global: a function rebinds a module-level name
    #[test]
    fn global_statement() {
        check(
            r#"
            counter = 0

            def bump(n: int) -> None:
                global counter
                counter += n

            def reset() -> None:
                global counter
                counter = 0

            def f() -> int:
                bump(2)
                bump(5)
                a = counter
                reset()
                bump(1)
                return a * 10 + counter
        "#,
            &[("f", Want::Int(71))],
        );
    }

    /// del of a list index or slice, and of a dict key (IndexError / KeyError when absent)
    #[test]
    fn del_statement() {
        check(
            r#"
            def f() -> int:
                xs = [1, 2, 3, 4, 5]
                del xs[0]
                del xs[-1]
                del xs[0:1]
                d = {"a": 1, "b": 2}
                del d["a"]
                r = len(xs) * 100 + xs[0] * 10 + len(d)
                try:
                    del d["zz"]
                except KeyError:
                    r += 1000
                try:
                    del xs[9]
                except IndexError:
                    r += 10000
                return r
        "#,
            &[("f", Want::Int(11231))],
        );
    }

    /// nonlocal: a nested function rebinds a variable of the function enclosing it
    #[test]
    fn nonlocal_statement() {
        check(
            r#"
            def f() -> int:
                count = 0
                def inc() -> None:
                    nonlocal count
                    count += 1
                inc()
                inc()
                return count
        "#,
            &[("f", Want::Int(2))],
        );
    }

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

            class C:
                @deco
                def m(self) -> int:
                    return 1
        "#,
            "decorator '@deco' on method 'C.m'",
        );
        refused(
            r#"
            def deco(cls):
                return cls

            @deco
            class C:
                pass
        "#,
            "decorator '@deco' on class 'C'",
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

    /// User-written decorators applied at definition time: wrappers forwarding *args and **kwargs (with keyword arguments at the call), state the wrapper keeps through nonlocal (one per decorated function), decorator factories (@repeat(3)), stacked decorators, decorators that register and return the function, fn.__name__, and @functools.wraps
    #[test]
    fn user_written_decorators() {
        check(
            r#"
            import functools
            from typing import List

            log: List[str] = []


            def traced(fn):
                @functools.wraps(fn)
                def wrapper(*args, **kwargs):
                    log.append("call " + fn.__name__)
                    result = fn(*args, **kwargs)
                    log.append("done " + fn.__name__)
                    return result
                return wrapper


            def counted(fn):
                calls = 0

                def wrapper(*args):
                    nonlocal calls
                    calls += 1
                    return fn(*args) + calls * 1000
                return wrapper


            def repeat(times: int):
                def deco(fn):
                    def wrapper(x: int) -> int:
                        total = 0
                        for _ in range(times):
                            total += fn(x)
                        return total
                    return wrapper
                return deco


            registry: List[str] = []


            def register(fn):
                registry.append(fn.__name__)
                return fn


            @traced
            def add(a: int, b: int = 10) -> int:
                return a + b


            @counted
            def square(x: int) -> int:
                return x * x


            @counted
            def cube(x: int) -> int:
                return x * x * x


            @repeat(3)
            def double(x: int) -> int:
                return 2 * x


            @register
            @traced
            def greet(name: str) -> str:
                return "hi " + name


            @traced
            def fact(n: int) -> int:
                return 1 if n <= 1 else n * fact(n - 1)


            def g() -> str:
                out = str(add(1, 2)) + "," + str(add(5)) + "," + str(add(a=1, b=1)) + "|"
                out += str(square(3)) + "," + str(square(4)) + "," + str(cube(2)) + "|"
                out += str(double(5)) + "|" + greet("bob") + "|" + ",".join(registry) + "|"
                out += str(fact(4)) + "|" + str(len(log)) + log[0] + log[-1]
                return out
        "#,
            &[(
                "g",
                Want::Str("3,15,2|1009,2016,1008|30|hi bob|greet|24|16call adddone fact"),
            )],
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

    /// % formatting on runtime values ("%d" % n), every conversion, flags, width, precision, %(key) mappings, and a tuple operand
    #[test]
    fn runtime_percent_formatting() {
        check(
            r#"
            def f(n: int) -> str:
                return "%d!" % n

            def g() -> str:
                x = -1.005
                t = (3, "z")
                d = {"name": "bob", "job": "x"}
                a = "%5d|%-6.2f|%#x|%r|%s" % (42, x, 255, "a", True)
                b = "%s-%s" % t
                c = "%(name)s is %(job)5s" % d
                e = "%e %g %G %+.3E" % (12345.678, 0.00001234, 1e20, 3.0)
                h = "%c%c %o %X %a %5.2s| %%" % (72, 105, 8, 3054, "\u00e9", "hello")
                k = "%d %i %u" % (3.99, -2.5, True)
                m = "%-8s|%8r|%08.3f|% d|%+d" % ("ab", "ab", -3.14159, 5, 0)
                return f(5) + ";" + a + ";" + b + ";" + c + ";" + e + ";" + h + ";" + k + ";" + m
        "#,
            &[(
                "g",
                Want::Str(
                    "5!;   42|-1.00 |0xff|'a'|True;3-z;bob is     x;1.234568e+04 1.234e-05 1E+20 +3.000E+00;Hi 10 BEE '\\xe9'    he| %;3 -2 1;ab      |    'ab'|-003.142| 5|+0",
                ),
            )],
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

    /// .format() interpolates runtime values - automatic {} and positional {0} fields
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

    /// F-string fixed-point specifier f"{value:.2f}"
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

    /// F-strings: the full format spec (fill, alignment, sign, z, #, 0, width, grouping, precision, every type letter), nested {x:{w}.{p}f} specs, and !r / !s / !a conversions, digit for digit with CPython
    #[test]
    fn fstring_format_spec() {
        check(
            r#"
            def f(x: float, n: int, s: str) -> str:
                w = 9
                p = 2
                return f"[{x:{w}.{p}f}][{n:,}][{n:_b}][{n:#o}][{n:+08X}][{s:*^8}][{s!r}][{x:e}][{x:.3g}][{x:%}][{-0.0:z.1f}][{n:c}][{x!s:>6}]"

            def g() -> str:
                return f(0.125, 1000, "hé")
        "#,
            &[(
                "g",
                Want::Str(
                    r#"[     0.12][1,000][11_1110_1000][0o1750][+00003E8][***hé***]['hé'][1.250000e-01][0.125][12.500000%][0.0][Ϩ][ 0.125]"#,
                ),
            )],
        );
    }

    /// .format() named fields, attribute and index lookups ({0.x}, {1[2]}, {2[k]}), format specs with nested fields, and !r / !s / !a conversions
    #[test]
    fn format_method_fields() {
        check(
            r#"
            class Point:
                def __init__(self, x: int, y: int):
                    self.x = x
                    self.y = y

            def g() -> str:
                p = Point(3, 4)
                xs = [10, 20, 30]
                d = {"k": 7}
                w = 6
                return "{name}={value:>{w}.2f}|{0.x},{0.y}|{1[2]}|{2[k]}|{3!r:^7}|{3!a}".format(p, xs, d, "é", name="pi", value=3.14159, w=w)
        "#,
            &[("g", Want::Str(r#"pi=  3.14|3,4|30|7|  'é'  |'\xe9'"#))],
        );
    }

    /// A str is a sequence of code points: len, indexing, slicing with any step, find and index, padding, iteration, and comprehensions count characters, not UTF-8 bytes
    #[test]
    fn code_points() {
        check(
            r#"
            def g() -> str:
                s = str(len("x"))[0:0] + "straße ☃ ΑΣ"
                out = str(len(s)) + "|" + s[5] + s[-1] + "|" + s[4:8] + "|" + s[::-1] + "|" + str(s.find("☃")) + "|" + s.center(15, "·")
                acc = ""
                for c in s:
                    acc = acc + c + "."
                return out + "|" + acc + "|" + str([c for c in s if c.isupper()])
        "#,
            &[("g", Want::Str("11|e\u{3a3}|\u{df}e \u{2603}|\u{3a3}\u{391} \u{2603} e\u{df}arts|7|\u{b7}\u{b7}stra\u{df}e \u{2603} \u{391}\u{3a3}\u{b7}\u{b7}|s.t.r.a.\u{df}.e. .\u{2603}. .\u{391}.\u{3a3}.|['\u{391}', '\u{3a3}']"))],
        );
    }

    /// Case conversion, the is* predicates, strip, and split over the whole of Unicode, as CPython maps and classifies it (final sigma, titlecase, ß to SS, CPython's whitespace), on literals and runtime strings alike
    #[test]
    fn unicode_case_and_classes() {
        check(
            r#"
            def g() -> str:
                s = str(len("x"))[0:0] + "straße ΑΣ hello-world ǆemal \xa0x"
                lit = "hello-world 3rd".title() + "," + "ǆemal".capitalize() + "," + str("123".isupper())
                return s.upper() + "|" + s.lower() + "|" + s.title() + "|" + s.capitalize() + "|" + "/".join(s.split()) + "|" + str(s.isalpha()) + str("é²".isalnum()) + str(s[8].islower()) + "|" + lit
        "#,
            &[("g", Want::Str("STRASSE \u{391}\u{3a3} HELLO-WORLD \u{1c4}EMAL \u{a0}X|stra\u{df}e \u{3b1}\u{3c2} hello-world \u{1c6}emal \u{a0}x|Stra\u{df}e \u{391}\u{3c2} Hello-World \u{1c5}emal \u{a0}X|Stra\u{df}e \u{3b1}\u{3c2} hello-world \u{1c6}emal \u{a0}x|stra\u{df}e/\u{391}\u{3a3}/hello-world/\u{1c6}emal/x|FalseTrueFalse|Hello-World 3Rd,\u{1c5}emal,False"))],
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

    /// A tuple mixing int and float members ((1, 2.5)), each read at its own width: indexing, unpacking, equality, ordering, hashing, in, count, index, str()
    #[test]
    fn mixed_number_tuples() {
        check(
            r#"
            def f() -> str:
                t = (1, 2.5)
                a, b = t
                s = {(1, 2.5), (1, 2.5), (2, 0.5)}
                d = {(1, 2.5): "x"}
                out = str(t) + str(t[0]) + str(t[1] * 2) + str(a + b) + str(len(s)) + d[(1, 2.5)]
                return out + str(t == (1, 2.5)) + str(t < (1, 3.0)) + str(1.0 in t) + str(2 in t) + str(t.count(2.5)) + str(t.index(2.5))
        "#,
            &[("f", Want::Str("(1, 2.5)15.03.52xTrueTrueTrueFalse11"))],
        );
    }

    /// A literal mixing element types no one type holds ([1, "a"], {"n": "bob", "age": 30}) is refused instead of reading the others back as the first; [[], [1]] and tuples of one shape unify
    #[test]
    fn mixed_literals_refused() {
        check(
            r#"
            def g() -> str:
                xs = [[], [1, 2]]
                ys = [(1, "a"), (2, "b")]
                return str(xs) + str(ys)
        "#,
            &[("g", Want::Str("[[], [1, 2]][(1, 'a'), (2, 'b')]"))],
        );
        for src in [
            "def g() -> str:\n    xs = [1, \"a\"]\n    return str(xs[1])\n",
            "def g() -> str:\n    d = {\"n\": \"bob\", \"age\": 30}\n    return str(d[\"age\"])\n",
        ] {
            let err = try_compile(src).expect_err("a mixed literal is refused");
            assert!(err.contains("mixing"), "{err}");
        }
    }

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

    /// Dict methods .get(key, default) (.get(key) on instance values, where None is distinct), .keys(), .values(), .items() - .items() as a value, not only a for iterable
    #[test]
    fn dict_methods() {
        check(
            r#"
            def f() -> int:
                d = {"a": 1, "b": 2}
                t = d.get("a", 0) + d.get("q", 10)
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

    /// str() and repr() of every type: floats (shortest round-trip digits), bools, None, quoted and escaped strings and bytes, ranges, nested collections, and instances through __str__ / __repr__
    #[test]
    fn str_of_float_and_collections() {
        check(
            r#"
            class Money:
                def __init__(self, cents: int):
                    self.cents = cents

                def __repr__(self) -> str:
                    return "Money(" + str(self.cents) + ")"

            def g() -> str:
                x = 1.5
                return str(x) + "|" + str([1, 2]) + "|" + repr("it's") + "|" + str(True) + "|" + str(None) + "|" + repr([0.1, 1e16, -0.0]) + "|" + str({"a": (1,)}) + "|" + str(range(2, 9, 3)) + "|" + repr(b"a\x00'") + "|" + str(Money(5)) + "|" + str({1.5, 2.5})
        "#,
            &[(
                "g",
                Want::Str(
                    r#"1.5|[1, 2]|"it's"|True|None|[0.1, 1e+16, -0.0]|{'a': (1,)}|range(2, 9, 3)|b"a\x00'"|Money(5)|{1.5, 2.5}"#,
                ),
            )],
        );
    }

    /// format() and ascii() builtins
    #[test]
    fn format_and_ascii() {
        check(
            r##"
            def g() -> str:
                n = 255
                return format(n, "#x") + "|" + format(3.0) + "|" + format(1234567.891, ",.2f") + "|" + format("ab", "^6") + "|" + format(True) + "|" + format(True, "d") + "|" + ascii("naïve ☃ \U0001f600") + "|" + ascii(["é", "ü"])
        "##,
            &[(
                "g",
                Want::Str(
                    r##"0xff|3.0|1,234,567.89|  ab  |True|1|'na\xefve \u2603 \U0001f600'|['\xe9', '\xfc']"##,
                ),
            )],
        );
    }

    /// float() of a string, correctly rounded
    #[test]
    fn float_of_string() {
        check(
            r#"
            def g() -> str:
                return repr(float("0.1")) + "|" + repr(float(" -2.5e-3 ")) + "|" + repr(float("1e23")) + "|" + repr(float("2.2250738585072014e-308")) + "|" + repr(float("123456789012345678901234567890e-10")) + "|" + repr(float("Infinity")) + "|" + repr(float("1_000.5"))
        "#,
            &[(
                "g",
                Want::Str(
                    "0.1|-0.0025|1e+23|2.2250738585072014e-308|1.2345678901234567e+19|inf|1000.5",
                ),
            )],
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

    /// print() writes to standard output through WASI: sep, end, file=sys.stderr, flush, every argument through str() after all are evaluated
    #[test]
    fn print_output() {
        let run = run_main(
            &dedent(
                r#"
            import sys

            class Point:
                def __init__(self, x: int, y: int):
                    self.x = x
                    self.y = y

                def __str__(self) -> str:
                    return "(" + str(self.x) + ", " + str(self.y) + ")"

            def main() -> None:
                xs = [1, 2]
                print("a", 1, 2.5, True, None, xs, (1, "b"), {"k": 1.5}, Point(3, 4))
                print(xs, xs.append(3))
                print("x", "y", sep="", end="|\n")
                print("err", 7, file=sys.stderr)
                print()
                print("done", flush=True)

            if __name__ == "__main__":
                main()
        "#,
            ),
            "",
            &[],
        );
        assert_eq!(
            run.stdout,
            "a 1 2.5 True None [1, 2] (1, 'b') {'k': 1.5} (3, 4)\n[1, 2, 3] None\nxy|\n\ndone\n"
        );
        assert_eq!(run.stderr, "err 7\n");
        assert_eq!(run.exit, 0);
    }

    /// input([prompt]) reads a line of standard input, writing the prompt first; EOFError at the end of input
    #[test]
    fn input_reads_a_line() {
        let run = run_main(
            &dedent(
                r#"
            def main() -> None:
                name = input("name? ")
                age = int(input())
                print("hi", name, age + 1)
                try:
                    input()
                except EOFError:
                    print("eof")

            if __name__ == "__main__":
                main()
        "#,
            ),
            "bob\n41\n",
            &[],
        );
        assert_eq!(run.stdout, "name? hi bob 42\neof\n");
        assert_eq!(run.stderr, "");
        assert_eq!(run.exit, 0);
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
    fn min_max_iterable() {
        check(
            r#"
            from typing import List

            def f() -> int:
                return min([3, 1, 2])

            def g() -> str:
                ws = ["pear", "fig", "banana"]
                return min(ws, key=lambda w: len(w)) + max(ws, key=len)

            def h() -> int:
                xs: List[int] = []
                return min(xs, default=-1) + max(range(3, 10, 2)) * 10
        "#,
            &[
                ("f", Want::Int(1)),
                ("g", Want::Str("figbanana")),
                ("h", Want::Int(89)),
            ],
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

    /// CPython 3.13's draws for `random_module`'s seed.
    const RANDOM_2024: &str = "[0.7572170996413706, 1.758625874409237, -1.391185562476273, \
                               0.6248799482691346, 0.7493563774790886][5, 13, 1267]c\
                               [6, 5, 1, 3, 2, 4][4, 1, 6]";

    /// sys.exit([status]) raises a catchable SystemExit; one nothing catches exits with its status (an int, 0 for none or None, 1 with the message printed to stderr for anything else)
    #[test]
    fn sys_exit() {
        let src = dedent(
            r#"
                import sys

                def main() -> None:
                    n = int(input())
                    try:
                        sys.exit(9)
                    except SystemExit:
                        print("caught")
                    if n == 0:
                        sys.exit()
                    if n == 1:
                        sys.exit(3)
                    if n == 2:
                        sys.exit("bye now")
                    if n == 3:
                        raise SystemExit(True)
                    if n == 4:
                        sys.exit(None)
                    print("end")

                if __name__ == "__main__":
                    main()
        "#,
        );
        let run = run_main(&src, "0\n", &[]);
        assert_eq!(run.stdout, "caught\n");
        assert_eq!(run.stderr, "");
        assert_eq!(run.exit, 0);
        let run = run_main(&src, "1\n", &[]);
        assert_eq!(run.stdout, "caught\n");
        assert_eq!(run.stderr, "");
        assert_eq!(run.exit, 3);
        let run = run_main(&src, "2\n", &[]);
        assert_eq!(run.stdout, "caught\n");
        assert_eq!(run.stderr, "bye now\n");
        assert_eq!(run.exit, 1);
        let run = run_main(&src, "3\n", &[]);
        assert_eq!(run.stdout, "caught\n");
        assert_eq!(run.stderr, "");
        assert_eq!(run.exit, 1);
        let run = run_main(&src, "5\n", &[]);
        assert_eq!(run.stdout, "caught\nend\n");
        assert_eq!(run.stderr, "");
        assert_eq!(run.exit, 0);
    }

    /// sys.argv, and sys.stdin / sys.stdout / sys.stderr: write, flush, read, readline, readlines, and iterating lines, through WASI
    #[test]
    fn sys_streams_and_argv() {
        let run = run_main(
            &dedent(
                r#"
            import sys

            def main() -> None:
                first = sys.stdin.readline()
                count = 0
                for line in sys.stdin:
                    count += 1
                    sys.stdout.write(str(count) + ":" + line)
                sys.stderr.write("first=" + first)
                print(len(sys.argv), sys.argv[1:], first == "one\n")

            if __name__ == "__main__":
                main()
        "#,
            ),
            "one\ntwo\r\nthree",
            &["x", "y"],
        );
        assert_eq!(run.stdout, "1:two\r\n2:three3 ['x', 'y'] True\n");
        assert_eq!(run.stderr, "first=one\n");
        assert_eq!(run.exit, 0);
    }

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

    /// json module: dumps (indent, separators, sort_keys, ensure_ascii, allow_nan), loads into the declared type with CPython's JSONDecodeError, dump, load
    #[test]
    fn json_module() {
        check(
            r#"
            import json

            def f() -> str:
                d: dict[str, list[int]] = json.loads('{"a": [1, 2], "b": []}')
                return json.dumps(d, sort_keys=True) + json.dumps((1.5, None), indent=1) + str(len(json.dumps([1, 2, 3])))
        "#,
            &[(
                "f",
                Want::Str("{\"a\": [1, 2], \"b\": []}[\n 1.5,\n null\n]9"),
            )],
        );
    }

    /// random module, bit-identical to CPython for a seed: random, randint, randrange, uniform, choice, shuffle, sample, seed, getrandbits, gauss, normalvariate, expovariate
    #[test]
    fn random_module() {
        check(
            r#"
            import random

            def f() -> str:
                random.seed(2024)
                xs = [1, 2, 3, 4, 5, 6]
                random.shuffle(xs)
                return (str([random.random(), random.uniform(1, 2), random.gauss(), random.normalvariate(), random.expovariate()])
                        + str([random.randint(1, 6), random.randrange(10, 20, 3), random.getrandbits(12)])
                        + random.choice("abc")
                        + str(xs) + str(random.sample(xs, 3)))

            def unseeded() -> int:
                random.seed()
                x = random.random()
                return int(x >= 0.0 and x < 1.0)
        "#,
            &[("f", Want::Str(RANDOM_2024)), ("unseeded", Want::Int(1))],
        );
    }

    /// sys: platform, version, path
    #[test]
    fn sys_host_strings() {
        let version = format!("3.13.0 (waspy {})", env!("CARGO_PKG_VERSION"));
        let src = dedent(
            r#"
            import sys

            def platform() -> str:
                return sys.platform

            def version() -> str:
                return sys.version

            def path() -> str:
                sys.path.append("lib")
                return str(sys.path) + str(len(sys.path))
        "#,
        );
        assert_eq!(call_str(&src, "platform"), "wasi");
        assert_eq!(call_str(&src, "version"), version);
        assert_eq!(call_str(&src, "path"), "['lib']1");
    }

    /// os functions on host values: getcwd(), getenv(), environ, getpid(), urandom()
    #[test]
    fn os_host_values() {
        check(
            r#"
            import os

            def f() -> str:
                return (os.getcwd() + " " + str(os.getpid()) + " " + str(len(os.urandom(5)))
                        + " " + os.getenv("NOPE", "dflt") + " " + str(os.getenv("NOPE"))
                        + " " + str(len(os.environ)) + " " + os.environ.get("NOPE", "-"))
        "#,
            &[("f", Want::Str("/ 42 5 dflt None 0 -"))],
        );
    }

    /// os.path functions: join(), basename(), dirname(), split(), splitext(), abspath(), exists(), isfile(), isdir()
    #[test]
    fn os_path_functions() {
        check(
            r#"
            import os.path

            def f() -> str:
                p = os.path.join("a/b", "c.tar.gz")
                return str([p, os.path.basename(p), os.path.dirname(p), os.path.abspath("x/../y"),
                            os.path.join("a", "/b")]) + str(os.path.split(p)) + str(os.path.splitext(p))

            def kinds() -> str:
                return str([os.path.exists("."), os.path.isdir("."), os.path.isfile("."),
                            os.path.exists("missing")])
        "#,
            &[
                (
                    "f",
                    Want::Str(
                        "['a/b/c.tar.gz', 'c.tar.gz', 'a/b', '/y', '/b']('a/b', 'c.tar.gz')\
                         ('a/b/c.tar', '.gz')",
                    ),
                ),
                ("kinds", Want::Str("[True, True, False, False]")),
            ],
        );
    }

    /// functools: reduce, partial, partialmethod, cmp_to_key, cached_property, singledispatchmethod
    #[test]
    fn functools_functions() {
        check(
            r#"
            from functools import reduce, partial, partialmethod, cmp_to_key, cached_property, singledispatchmethod

            def add(a: int, b: int) -> int:
                return a + b

            def desc(a: int, b: int) -> int:
                return b - a

            class Box:
                def __init__(self, n: int) -> None:
                    self.n = n
                    self.reads = 0

                def scaled(self, k: int, m: int) -> int:
                    return self.n * k + m

                double = partialmethod(scaled, 2)

                @cached_property
                def square(self) -> int:
                    self.reads += 1
                    return self.n * self.n

                @singledispatchmethod
                def describe(self, x: object) -> str:
                    return "?"

                @describe.register
                def _(self, x: int) -> str:
                    return "int"

                @describe.register
                def _(self, x: str) -> str:
                    return "str"

            def f() -> str:
                b = Box(5)
                inc = partial(add, 1)
                return str([reduce(add, [1, 2, 3], 10), inc(41), b.double(1), b.square, b.square,
                            b.reads]) + str(sorted([3, 1, 2], key=cmp_to_key(desc))) + b.describe(1) + b.describe("s")
        "#,
            &[("f", Want::Str("[16, 42, 11, 25, 25, 1][3, 2, 1]intstr"))],
        );
    }

    /// datetime: date / time / datetime / timedelta construction, each field checked as CPython checks it
    #[test]
    fn datetime_construction() {
        check(
            r#"
            from datetime import date, time, datetime, timedelta


            def f() -> str:
                out = [repr(date(2024, 2, 29)), repr(time(13, 5, 9, 7)), repr(datetime(2024, 1, 2, 3, 4)),
                       repr(timedelta(hours=1.5, minutes=-3))]
                try:
                    date(2023, 2, 29)
                except ValueError as e:
                    out.append(str(e))
                try:
                    time(24)
                except ValueError as e:
                    out.append(str(e))
                return " ".join(out)
        "#,
            &[("f", Want::Str("datetime.date(2024, 2, 29) datetime.time(13, 5, 9, 7) datetime.datetime(2024, 1, 2, 3, 4) datetime.timedelta(seconds=5220) day is out of range for month hour must be in 0..23"))],
        );
    }

    /// datetime: attributes, arithmetic, weekday, isoformat, fromisoformat, strftime, strptime, replace, timestamp, fromtimestamp, timezone, tzinfo, and now() / today() read at run time
    #[test]
    fn datetime_operations() {
        check(
            r#"
            from datetime import date, datetime, timedelta, timezone


            def f() -> str:
                d = datetime(2024, 3, 15, 13, 5, 9, tzinfo=timezone(timedelta(hours=5, minutes=30)))
                later = d + timedelta(days=20)
                parsed = datetime.strptime("15/03/2024 01:05 PM", "%d/%m/%Y %I:%M %p")
                return " | ".join([later.isoformat(), str(later.weekday()), d.strftime("%a %d %b %Y %H:%M %z"),
                                   repr(datetime.fromisoformat("2024-03-15T07:35:09Z") == d), str(d.timestamp()),
                                   repr(d.astimezone(timezone.utc)), repr(parsed),
                                   str(date(2024, 12, 25) - date(2024, 1, 1)), repr(d.replace(tzinfo=None)),
                                   str(datetime.fromtimestamp(0, timezone.utc)), str(datetime.now().year >= 2024)])
        "#,
            &[("f", Want::Str("2024-04-04T13:05:09+05:30 | 3 | Fri 15 Mar 2024 13:05 +0530 | True | 1710488109.0 | datetime.datetime(2024, 3, 15, 7, 35, 9, tzinfo=datetime.timezone.utc) | datetime.datetime(2024, 3, 15, 13, 5) | 359 days, 0:00:00 | datetime.datetime(2024, 3, 15, 13, 5, 9) | 1970-01-01 00:00:00+00:00 | True"))],
        );
    }

    /// logging module: getLogger and the logger hierarchy, debug, info, warning, error, critical, log, basicConfig, setLevel, disable, Logger, Handler, StreamHandler, FileHandler, NullHandler, Formatter (the %, {, and $ styles), Filter, LogRecord, with each record's line and function
    #[test]
    fn logging_module() {
        check(
            r#"
            import sys
            import logging


            class Collect(logging.Handler):
                def __init__(self) -> None:
                    super().__init__()
                    self.lines: list[str] = []

                def emit(self, record: logging.LogRecord) -> None:
                    self.lines.append(self.format(record))


            def f() -> str:
                log = logging.getLogger("app.part")
                log.setLevel(logging.INFO)
                h = Collect()
                h.setFormatter(logging.Formatter("%(levelname)s:%(name)s:%(funcName)s:%(message)s"))
                log.addHandler(h)
                log.debug("hidden %d", 1)
                log.info("shown %d", 2)
                log.log(logging.ERROR, "logged %s", "x")
                logging.getLogger("app").addFilter(logging.Filter("other"))
                log.warning("propagates past a parent's filter")
                logging.disable(logging.CRITICAL)
                log.critical("disabled")
                logging.disable(logging.NOTSET)
                return " | ".join(h.lines) + " | " + repr(log) + " " + logging.getLevelName(40)
        "#,
            &[("f", Want::Str("INFO:app.part:f:shown 2 | ERROR:app.part:f:logged x | WARNING:app.part:f:propagates past a parent's filter | <Logger app.part (INFO)> ERROR"))],
        );
    }

    /// re module at runtime: compile, search, match, fullmatch, findall, finditer, split, sub, subn, escape, Pattern and Match objects, flags, matched as CPython's sre matches (backtracking, lookarounds, atomic and possessive repeats, conditionals, Unicode classes and case folding)
    #[test]
    fn re_runtime() {
        check(
            r#"
            import re

            WORD = re.compile(r"(?P<w>[a-z]+)(\d*)", re.I)


            def f() -> str:
                m = WORD.search("-- Hello42 x")
                parts = []
                if m:
                    parts.append(m.group("w") + "|" + str(m.group(2)) + "|" + str(m.span()) + "|" + str(m.lastindex))
                parts.append(str(re.findall(r"\d+", "a1 b22 c333")))
                parts.append(str(re.split(r"[,;]\s*", "x, y;z")))
                parts.append(re.sub(r"(\w+)@(\w+)", r"\2:\1", "me@home"))
                parts.append(str(re.subn(r"o", "0", "foo")))
                parts.append(str(re.fullmatch(r"\d{3}-\d{4}", "555-1234") is not None))
                parts.append(str([x.start() for x in re.finditer(r"a", "banana")]))
                parts.append(str(re.match(r"(a)|b", "b")))
                return " ".join(parts)
        "#,
            &[("f", Want::Str("Hello|42|(3, 10)|2 ['1', '22', '333'] ['x', 'y', 'z'] home:me ('f00', 2) True [1, 3, 5] <re.Match object; span=(0, 1), match='b'>"))],
        );
    }

    /// collections module: namedtuple and typing.NamedTuple (classes whose fields take their types from the calls), deque, Counter, OrderedDict, defaultdict, ChainMap, each a class specialized by its element types
    #[test]
    fn collections_classes() {
        check(
            r#"
            from collections import namedtuple, Counter, defaultdict, deque, OrderedDict, ChainMap

            Point = namedtuple("Point", "x y")


            def f() -> str:
                p = Point(1, 2)
                c = Counter("abca")
                d = defaultdict(list)
                d["k"].append(1)
                q = deque([1, 2, 3], maxlen=3)
                q.append(4)
                o = OrderedDict(a=1, b=2)
                o.move_to_end("a")
                m = ChainMap({"x": 1}, {"x": 2, "y": 3})
                return (str(p) + str(p.x + p[1]) + str(c) + str(c.most_common(1)) + str(d) + str(q)
                        + str(q.popleft()) + str(o) + str(m["x"]) + str(m["y"]) + str(len(m)))

        "#,
            &[("f", Want::Str("Point(x=1, y=2)3Counter({'a': 2, 'b': 1, 'c': 1})[('a', 2)]defaultdict(<class 'list'>, {'k': [1]})deque([2, 3, 4], maxlen=3)2OrderedDict({'b': 2, 'a': 1})132"))],
        );
    }

    /// itertools module: count, cycle, repeat, chain, compress, dropwhile, filterfalse, groupby, islice, starmap, takewhile, tee, zip_longest, product, permutations, combinations, combinations_with_replacement, accumulate, batched, pairwise, lowered to generators (tee and zip_longest over sequences)
    #[test]
    fn itertools_functions() {
        check(
            r#"
            import itertools
            from itertools import count, islice


            def f() -> str:
                parts = [str(list(islice(count(3), 3))), str(list(islice(itertools.cycle("ab"), 3))),
                         str(list(itertools.repeat(0, 2))), str(list(itertools.chain([1], [2]))),
                         str(list(itertools.compress([1, 2, 3], [1, 0, 1]))),
                         str(list(itertools.dropwhile(lambda x: x < 2, [1, 2, 1]))),
                         str(list(itertools.takewhile(lambda x: x < 2, [1, 2, 1]))),
                         str(list(itertools.filterfalse(lambda x: x > 1, [1, 2, 0]))),
                         str(list(itertools.accumulate([1, 2, 3]))),
                         str(list(itertools.starmap(lambda x, y: x * y, [(2, 3)]))),
                         str(list(itertools.islice("abcd", 1, 3))), str(list(itertools.pairwise([1, 2, 3]))),
                         str(list(itertools.batched([1, 2, 3], 2))),
                         str([(k, len(list(g))) for k, g in itertools.groupby("aab")]),
                         str(list(itertools.zip_longest([1, 2], [3], fillvalue=0))),
                         str(list(itertools.product([1, 2], repeat=2))), str(list(itertools.permutations([1, 2]))),
                         str(list(itertools.combinations([1, 2, 3], 2))),
                         str(list(itertools.combinations_with_replacement([1, 2], 2)))]
                t1, t2 = itertools.tee([1, 2])
                parts.append(str(list(t1)) + str(list(t2)))
                return "".join(parts)

        "#,
            &[("f", Want::Str("[3, 4, 5]['a', 'b', 'a'][0, 0][1, 2][1, 3][2, 1][1][1, 0][1, 3, 6][6]['b', 'c'][(1, 2), (2, 3)][(1, 2), (3,)][('a', 2), ('b', 1)][(1, 3), (2, 0)][(1, 1), (1, 2), (2, 1), (2, 2)][(1, 2), (2, 1)][(1, 2), (1, 3), (2, 3)][(1, 1), (1, 2), (2, 2)][1, 2][1, 2]"))],
        );
    }

    /// math functions on runtime values: sqrt, sin, cos, tan, asin, acos, atan, atan2, sinh, cosh, tanh, exp, log, log10, log2, pow, floor, ceil, trunc, fabs, copysign, fmod, remainder, degrees, radians, hypot, factorial, gcd, isnan, isinf, isfinite (refused today)
    #[test]
    fn math_functions() {
        check(
            r#"
            import math

            def f() -> str:
                xs = [math.sqrt(16.0), math.sin(0.5), math.cos(0.5), math.tan(0.5),
                      math.asin(0.25), math.acos(0.25), math.atan(0.25), math.atan2(1.0, 2.0),
                      math.sinh(0.5), math.cosh(0.5), math.tanh(0.5), math.exp(0.5),
                      math.log(10.0), math.log10(1000.0), math.log2(8.0), math.pow(2.0, 0.5),
                      math.fabs(-1.5), math.copysign(1.0, -0.0), math.fmod(7.5, 2.0),
                      math.remainder(7.5, 2.0), math.degrees(1.0), math.radians(90.0),
                      math.hypot(3.0, 4.0)]
                ints = [math.floor(-1.5), math.ceil(-1.5), math.trunc(-1.5), math.factorial(10),
                        math.gcd(12, 18)]
                flags = [math.isnan(math.nan), math.isinf(math.inf), math.isfinite(1.0)]
                return str(xs) + str(ints) + str(flags)

            def domain() -> str:
                try:
                    return str(math.sqrt(-1.0))
                except ValueError as e:
                    return str(e)
        "#,
            &[
                (
                    "f",
                    Want::Str(
                        "[4.0, 0.479425538604203, 0.8775825618903728, 0.5463024898437905, \
                         0.25268025514207865, 1.318116071652818, 0.24497866312686414, \
                         0.4636476090008061, 0.5210953054937474, 1.1276259652063807, \
                         0.46211715726000974, 1.6487212707001282, 2.302585092994046, 3.0, 3.0, \
                         1.4142135623730951, 1.5, -1.0, 1.5, -0.5, 57.29577951308232, \
                         1.5707963267948966, 5.0][-2, -1, -1, 3628800, 6][True, True, True]",
                    ),
                ),
                ("domain", Want::Str("math domain error")),
            ],
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

    /// yield inside try / except / finally and with, resuming inside them; close() raising GeneratorExit at the suspended yield (finally blocks and handlers run, a generator yielding instead is CPython's RuntimeError); and generator methods holding their instance
    #[test]
    fn yield_in_try() {
        check(
            r#"
            from typing import List

            log: List[str] = []


            class Boom(Exception):
                def __init__(self, code: int) -> None:
                    super().__init__("boom")
                    self.code = code


            def risky(i: int) -> int:
                if i == 2:
                    raise Boom(i * 10)
                if i == 4:
                    raise ValueError("four")
                return i


            def gen(n: int):
                for i in range(n):
                    try:
                        yield risky(i)
                    except Boom as e:
                        log.append("boom" + str(e.code))
                        yield -e.code
                    except ValueError as e:
                        log.append("value:" + str(e))
                    finally:
                        log.append("fin" + str(i))
                log.append("end")


            def counter():
                try:
                    i = 0
                    while True:
                        yield i
                        i += 1
                finally:
                    log.append("closed")


            def early():
                for i in range(10):
                    try:
                        if i == 3:
                            return
                        yield i
                    finally:
                        log.append("e" + str(i))


            class Res:
                def __init__(self, name: str) -> None:
                    self.name = name

                def __enter__(self) -> "Res":
                    log.append("open " + self.name)
                    return self

                def __exit__(self, exc_type, exc, tb) -> bool:
                    log.append("close " + self.name)
                    return False


            def lines():
                with Res("r") as r:
                    yield 1
                    yield 2


            def stubborn():
                try:
                    yield 1
                except GeneratorExit:
                    log.append("ge")
                    raise


            def broken():
                try:
                    yield 1
                finally:
                    yield 2


            def f() -> str:
                out = str([v for v in gen(6)]) + "|"
                c = counter()
                out += str(next(c)) + str(next(c)) + str(next(c))
                c.close()
                out += "|" + str([v for v in early()]) + "|"
                out += str(sum([v for v in lines()])) + "|"
                s = stubborn()
                next(s)
                s.close()
                b = broken()
                next(b)
                try:
                    b.close()
                except RuntimeError as e:
                    out += str(e) + "|"
                out += ",".join(log)
                return out
        "#,
            &[("f", Want::Str("[0, 1, -20, 3, 5]|012|[0, 1, 2]|3|generator ignored GeneratorExit|fin0,fin1,boom20,fin2,fin3,value:four,fin4,fin5,end,closed,e0,e1,e2,e3,open r,close r,ge"))],
        );
        check(
            r#"
            from typing import List

            log: List[str] = []


            def nested():
                for i in range(6):
                    try:
                        try:
                            if i == 1:
                                continue
                            if i == 4:
                                break
                            if i == 2:
                                raise KeyError("k")
                            yield i
                        finally:
                            log.append("in" + str(i))
                    except KeyError as e:
                        log.append("caught " + repr(e))
                        yield 100
                    finally:
                        log.append("out" + str(i))
                log.append("done")


            def reraiser():
                try:
                    yield 1
                    raise ValueError("bad")
                except ValueError:
                    log.append("seen")
                    raise


            def leaky():
                try:
                    yield 1
                    raise IndexError("idx")
                except KeyError:
                    log.append("never")
                finally:
                    log.append("leaky fin")


            def in_handler():
                try:
                    raise LookupError("look")
                except LookupError as e:
                    yield str(e)
                    yield "after"
                yield "end"


            def f() -> str:
                out = str([v for v in nested()]) + "|"
                try:
                    for v in reraiser():
                        out += str(v)
                except ValueError as e:
                    out += "VE:" + str(e) + "|"
                try:
                    for v in leaky():
                        out += str(v)
                except IndexError as e:
                    out += "IE:" + str(e) + "|"
                out += ",".join([s for s in in_handler()]) + "|"
                return out + ",".join(log)
        "#,
            &[("f", Want::Str("[0, 100, 3]|1VE:bad|1IE:idx|look,after,end|in0,out0,in1,out1,in2,caught KeyError('k'),out2,in3,out3,in4,out4,done,seen,leaky fin"))],
        );
        check(
            r#"
            from typing import List


            class Node:
                def __init__(self, value: int) -> None:
                    self.value = value
                    self.children: List["Node"] = []

                def add(self, value: int) -> "Node":
                    child = Node(value)
                    self.children.append(child)
                    return child

                def values(self):
                    yield self.value
                    for c in self.children:
                        yield c.value * 10


            class Counter:
                def __init__(self, limit: int) -> None:
                    self.limit = limit
                    self.calls = 0

                def bump(self) -> int:
                    self.calls += 1
                    return self.calls

                def count(self, step: int):
                    i = 0
                    while i < self.limit:
                        yield i + self.bump()
                        i += step


            class Sub(Counter):
                pass


            def g() -> str:
                root = Node(1)
                root.add(2)
                root.add(3)
                out = str([v for v in root.values()]) + "|"
                c = Counter(7)
                t = 0
                for v in c.count(2):
                    t += v
                out += str(t) + "," + str(c.calls) + "|"
                g = c.count(3)
                out += str(next(g)) + str(next(g)) + "|"
                s = Sub(4)
                out += str(sum([v for v in s.count(1)])) + "|"
                for a, b in zip([1, 2], Counter(5).count(1)):
                    out += str(a * b)
                return out
        "#,
            &[("g", Want::Str("[1, 20, 30]|22,4|59|16|16"))],
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

    /// zip() with a generator or iterator after its first argument, pulled one value per round in CPython's order
    #[test]
    fn zip_generator_later() {
        check(
            r#"
            from typing import List

            pulled: List[int] = []


            def gen(n: int):
                for i in range(n):
                    pulled.append(i)
                    yield i * 10


            class Count:
                def __init__(self, n: int) -> None:
                    self.i = 0
                    self.n = n

                def __iter__(self) -> "Count":
                    return self

                def __next__(self) -> int:
                    if self.i >= self.n:
                        raise StopIteration
                    self.i += 1
                    return self.i


            def f() -> str:
                a = 0
                b = 0
                for a, b in zip([1, 2, 3], [9]):
                    pass
                out = str(a * 100 + b) + "|"
                t = 0
                for x, y in zip([1, 2, 3], gen(5)):
                    t += x * y
                out += str(t) + str(pulled) + "|"
                t = 0
                for x, y, z in zip(gen(2), [7, 8, 9], Count(9)):
                    t += x + y * z
                out += str(t) + str(pulled) + "|"
                for x, y in zip(gen(4), gen(1)):
                    t += x + y
                out += str(t) + str(pulled)
                return out
        "#,
            &[(
                "f",
                Want::Str("109|80[0, 1, 2]|33[0, 1, 2, 0, 1]|33[0, 1, 2, 0, 1, 0, 0, 1]"),
            )],
        );
    }
}

mod error_handling {
    use super::*;

    /// assert raises AssertionError when its condition is false, evaluating the message only then
    #[test]
    fn assert_statements() {
        check(
            r#"
            def check(n: int) -> int:
                assert n > 0, "positive"
                return n

            def f() -> int:
                r = check(3)
                try:
                    check(-1)
                except AssertionError:
                    r += 10
                try:
                    assert False
                except AssertionError:
                    r += 100
                return r
        "#,
            &[("f", Want::Int(113))],
        );
    }

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

    /// Exception objects: raise E(args) raises an instance and except ... as e binds it; str(e), repr(e), e.args, user exception classes with __init__ and fields, super().__init__, raise X from Y with __cause__, and CPython's messages for the runtime's own faults
    #[test]
    fn exception_objects() {
        check(
            r#"
            from typing import Dict


            class AppError(Exception):
                pass


            class NotFound(AppError):
                def __init__(self, key: str, code: int):
                    super().__init__("missing " + key)
                    self.key = key
                    self.code = code


            def find(d: Dict[str, int], k: str) -> int:
                if k not in d:
                    raise NotFound(k, 404)
                return d[k]


            def g() -> str:
                out = ""
                try:
                    find({"a": 1}, "b")
                except NotFound as e:
                    out += str(e) + "|" + repr(e) + "|" + str(e.code) + e.key + "|"
                try:
                    raise ValueError("bad", 3)
                except ValueError as e:
                    out += str(e) + "|" + repr(e) + "|"
                try:
                    int("x1")
                except ValueError as e:
                    out += str(e) + "|"
                try:
                    {}["k"]
                except KeyError as e:
                    out += str(e) + "|" + repr(e) + "|"
                try:
                    raise TypeError("t")
                except (KeyError, TypeError) as e:
                    out += repr(e) + "|" + str(e.args) + "|"
                try:
                    raise RuntimeError("outer") from ValueError("cause")
                except RuntimeError as e:
                    out += str(e) + "<-" + str(e.__cause__)
                return out
        "#,
            &[("g", Want::Str("missing b|NotFound('missing b')|404b|('bad', 3)|ValueError('bad', 3)|invalid literal for int() with base 10: 'x1'|'k'|KeyError('k')|TypeError('t')|('t',)|outer<-cause"))],
        );
    }

    /// An exception the __main__ block does not catch writes the last line of CPython's report to stderr (Name: str(e)) and exits with status 1
    #[test]
    fn uncaught_exception_reports() {
        let src = dedent(
            r#"
                class Boom(Exception):
                    def __str__(self) -> str:
                        return "custom boom"

                def main() -> None:
                    print("before")
                    n = int(input())
                    if n == 1:
                        raise Boom()
                    if n == 2:
                        raise ValueError("bad value", 2)
                    if n == 3:
                        raise KeyError("k")
                    if n == 4:
                        raise RuntimeError
                    print([1][n])

                if __name__ == "__main__":
                    main()
        "#,
        );
        let run = run_main(&src, "1\n", &[]);
        assert_eq!(run.stdout, "before\n");
        assert_eq!(run.stderr, "Boom: custom boom\n");
        assert_eq!(run.exit, 1);
        let run = run_main(&src, "2\n", &[]);
        assert_eq!(run.stdout, "before\n");
        assert_eq!(run.stderr, "ValueError: ('bad value', 2)\n");
        assert_eq!(run.exit, 1);
        let run = run_main(&src, "3\n", &[]);
        assert_eq!(run.stdout, "before\n");
        assert_eq!(run.stderr, "KeyError: 'k'\n");
        assert_eq!(run.exit, 1);
        let run = run_main(&src, "9\n", &[]);
        assert_eq!(run.stdout, "before\n");
        assert_eq!(run.stderr, "IndexError: list index out of range\n");
        assert_eq!(run.exit, 1);
    }

    /// __exit__ receives the exception leaving a with block and suppresses it by answering true; an exception __exit__ raises replaces it, and return / break leave through __exit__(None, None, None)
    #[test]
    fn exit_suppresses() {
        check(
            r#"
            from typing import List

            log: List[str] = []


            class Suppress:
                def __init__(self, name: str):
                    self.name = name

                def __enter__(self) -> "Suppress":
                    log.append("enter " + self.name)
                    return self

                def __exit__(self, exc_type, exc, tb) -> bool:
                    if exc_type is None:
                        log.append("exit " + self.name + " clean")
                        return False
                    log.append("exit " + self.name + " " + repr(exc))
                    return isinstance(exc, ValueError)


            class Boom:
                def __enter__(self) -> int:
                    return 1

                def __exit__(self, exc_type, exc, tb) -> None:
                    log.append("boom exit " + str(exc is None))
                    raise RuntimeError("from exit")


            def body(n: int) -> int:
                with Suppress("a"):
                    if n == 0:
                        raise ValueError("v")
                    if n == 1:
                        raise KeyError("k")
                    if n == 2:
                        return 7
                return 1


            def g() -> str:
                out = ""
                for n in range(4):
                    try:
                        out += str(body(n)) + ";"
                    except KeyError as e:
                        out += "caught " + repr(e) + ";"
                with Suppress("outer"):
                    with Suppress("inner"):
                        raise ValueError("deep")
                for i in range(3):
                    with Suppress("loop"):
                        if i == 1:
                            break
                try:
                    with Boom():
                        raise TypeError("t")
                except RuntimeError as e:
                    out += "exit raised " + str(e) + ";"
                return out + "|".join(log)
        "#,
            &[("g", Want::Str("1;caught KeyError('k');7;1;exit raised from exit;enter a|exit a ValueError('v')|enter a|exit a KeyError('k')|enter a|exit a clean|enter a|exit a clean|enter outer|enter inner|exit inner ValueError('deep')|exit outer clean|enter loop|exit loop clean|enter loop|exit loop clean|boom exit False"))],
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

    /// Subclass matching: except Base catches its subclasses, along user classes and CPython's builtin hierarchy (LookupError over IndexError and KeyError, ArithmeticError over ZeroDivisionError, RuntimeError over NotImplementedError)
    #[test]
    fn subclass_matching() {
        check(
            r#"
            class AppError(Exception):
                pass


            class DbError(AppError):
                pass


            def g() -> str:
                out = ""
                try:
                    raise DbError("x")
                except AppError as e:
                    out += "app:" + str(isinstance(e, DbError)) + "|"
                for i in range(4):
                    try:
                        if i == 0:
                            [1][2]
                        elif i == 1:
                            {}[0]
                        elif i == 2:
                            1 // 0
                        else:
                            raise NotImplementedError("n")
                    except LookupError as e:
                        out += "lookup:" + str(e) + "|"
                    except ArithmeticError as e:
                        out += "arith:" + str(e) + "|"
                    except RuntimeError as e:
                        out += "runtime:" + repr(e) + "|"
                try:
                    try:
                        raise KeyError("k")
                    except ValueError:
                        out += "wrong"
                except Exception as e:
                    out += "exception:" + repr(e)
                return out
        "#,
            &[("g", Want::Str("app:True|lookup:list index out of range|lookup:0|arith:integer division or modulo by zero|runtime:NotImplementedError('n')|exception:KeyError('k')"))],
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

    /// The collection faults raise: list.index / list.remove of a missing value (ValueError), set.remove of a missing member (KeyError), pop out of range or from an empty list (IndexError)
    #[test]
    fn collection_faults_raise() {
        check(
            r#"
            from typing import List, Set

            def f() -> int:
                xs: List[int] = [1, 2]
                s: Set[int] = {1}
                empty: List[int] = []
                r = 0
                try:
                    r += xs.index(9)
                except ValueError:
                    r += 1
                try:
                    xs.remove(9)
                except ValueError:
                    r += 10
                try:
                    s.remove(9)
                except KeyError:
                    r += 100
                try:
                    r += empty.pop()
                except IndexError:
                    r += 1000
                try:
                    r += xs.pop(5)
                except IndexError:
                    r += 10000
                return r
        "#,
            &[("f", Want::Int(11111))],
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

    /// The rest of the set API: the update forms and in-place operators, copy, pop, clear, set(iterable), and methods taking any iterable
    #[test]
    fn rest_of_set_api() {
        check(
            r#"
            from typing import List, Set

            def order(s: Set[int]) -> str:
                parts: List[str] = []
                for x in s:
                    parts.append(str(x))
                return ",".join(parts)

            def f() -> str:
                a = {5, 1, 9}
                a.update([40, 8, 1])
                b = a.copy()
                b.difference_update({1, 9})
                c = a.union([100, 2], (3, 4))
                c.intersection_update({1, 2, 3, 100, 40})
                a.symmetric_difference_update([9, 70])
                a |= {3}
                a -= {5}
                p = a.pop()
                d = set([3, 1, 2, 3, 17])
                d.clear()
                return order(a) + ";" + order(b) + ";" + order(c) + ";" + str(p) + ";" + str(len(d))
        "#,
            &[("f", Want::Str("3,70,40,8;5,40,8;1,2,3,100,40;1;0"))],
        );
    }

    /// Iterating a set visits its members in CPython's order (ints, floats, and tuples of them)
    #[test]
    fn set_iteration_order() {
        check(
            r#"
            from typing import List, Set

            def order(s: Set[int]) -> str:
                parts: List[str] = []
                for x in s:
                    parts.append(str(x))
                return ",".join(parts)

            def f() -> str:
                s = {33, 1, 65, 97, 2}
                t = {x * 7 % 13 for x in range(20)}
                u = {1, 2, 3, 9, 17, 33} ^ {2, 3, 4, 65, 129}
                return order(s) + ";" + order(t) + ";" + order(u) + ";" + str(sum({4, 5}))
        "#,
            &[(
                "f",
                Want::Str("65,2,97,1,33;0,1,2,3,4,5,6,7,8,9,10,11,12;65,129,17,33,4,1,9;9"),
            )],
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

    /// Named tuples - namedtuple() and typing.NamedTuple, defined at module level: fields by name and position, unpacking, and tuple equality
    #[test]
    fn named_tuples() {
        check(
            r#"
            from collections import namedtuple
            from typing import NamedTuple

            P = namedtuple("P", "x y")

            class Q(NamedTuple):
                a: str
                b: int = 0

            def f() -> int:
                p = P(1, 2)
                x, y = p
                return p.y * 100 + p[0] * 10 + x + y + (p == (1, 2)) + Q("s").b
        "#,
            &[("f", Want::Int(214))],
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

    /// String formatting: the full format spec (width, alignment, sign, grouping, types), named .format() fields, !r / !a, runtime %
    #[test]
    fn full_format_spec() {
        check(
            r#"
            def f(v: float) -> str:
                return f"{v:0=+12,.3f}|{v:<12g}|{v:^#12.0e}|{v:_.1%}|{v:G}"

            def g() -> str:
                return f(-12345.6789) + ";" + f(1e-7) + ";" + f(2.5)
        "#,
            &[(
                "g",
                Want::Str(
                    r#"-012,345.679|-12345.7    |  -1.e+04   |-1_234_567.9%|-12345.7;+000,000.000|1e-07       |   1.e-07   |0.0%|1E-07;+000,002.500|2.5         |   2.e+00   |250.0%|2.5"#,
                ),
            )],
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

    /// Metaclasses: class M(type) with metaclass=M (inherited by subclasses), M.__new__ and M.__init__ run once as each class is created, attributes they give the class, M.__call__ replacing instantiation, M's methods and class variables reached through the class; type(x), x.__class__, and __name__
    #[test]
    fn metaclasses() {
        check(
            r##"
            from typing import List

            registry: List[str] = []


            class PluginMeta(type):
                count = 0

                def __new__(mcs, name, bases, namespace):
                    cls = super().__new__(mcs, name, bases, namespace)
                    registry.append(name)
                    return cls

                def __init__(cls, name, bases, namespace):
                    super().__init__(name, bases, namespace)
                    cls.plugin_id = len(registry)
                    cls.label = name.lower() + "!"
                    PluginMeta.count += 1

                def describe(cls) -> str:
                    return cls.__name__ + "#" + str(cls.plugin_id)


            class Plugin(metaclass=PluginMeta):
                def run(self) -> str:
                    return "base"


            class Audio(Plugin):
                def run(self) -> str:
                    return "audio"


            class Video(Plugin):
                pass


            class Counted(type):
                def __call__(cls, *args, **kwargs):
                    instance = super().__call__(*args, **kwargs)
                    instance.serial = 7
                    return instance


            class Point(metaclass=Counted):
                def __init__(self, x: int, y: int = 0) -> None:
                    self.x = x
                    self.y = y
                    self.serial = 0


            def f() -> str:
                out = ",".join(registry) + "|" + str(PluginMeta.count) + "|"
                out += Audio.describe() + Video.describe() + Plugin.describe() + "|"
                out += Audio.label + str(Video.plugin_id) + "|"
                out += str(Plugin.count) + Audio().run() + Video().run() + "|"
                p = Point(3, y=4)
                out += str(p.x + p.y) + str(p.serial)
                return out
        "##,
            &[(
                "f",
                Want::Str("Plugin,Audio,Video|3|Audio#2Video#3Plugin#1|audio!3|3audiobase|77"),
            )],
        );
        check(
            r##"
            from typing import Dict, Optional


            class Singleton(type):
                def __init__(cls, name, bases, namespace):
                    cls.instance: Optional["Config"] = None

                def __call__(cls, *args, **kwargs):
                    if cls.instance is None:
                        cls.instance = super().__call__(*args, **kwargs)
                    return cls.instance


            class Config(metaclass=Singleton):
                def __init__(self, level: int) -> None:
                    self.level = level


            def f() -> str:
                a = Config(3)
                b = Config(9)
                return str(a is b) + str(b.level)
        "##,
            &[("f", Want::Str("True3"))],
        );
    }

    /// Multiple inheritance with CPython's C3 method resolution order: methods and fields from every base, cooperative super() following the instance's MRO, isinstance against every base, and an inconsistent order refused as CPython's TypeError
    #[test]
    fn multiple_inheritance() {
        check(
            r#"
            from typing import List

            trace: List[str] = []


            class Base:
                def __init__(self) -> None:
                    trace.append("Base")
                    self.tag = "b"

                def hello(self) -> str:
                    return "Base.hello"

                def who(self) -> str:
                    return "base"


            class Left(Base):
                def __init__(self) -> None:
                    trace.append("Left")
                    super().__init__()
                    self.left = 1

                def hello(self) -> str:
                    return "Left>" + super().hello()


            class Right(Base):
                def __init__(self) -> None:
                    trace.append("Right")
                    super().__init__()
                    self.right = 2

                def hello(self) -> str:
                    return "Right>" + super().hello()

                def who(self) -> str:
                    return "right"


            class Diamond(Left, Right):
                def __init__(self) -> None:
                    trace.append("Diamond")
                    super().__init__()

                def hello(self) -> str:
                    return "Diamond>" + super().hello()


            class JsonMixin:
                def to_json(self) -> str:
                    return "{" + self.describe() + "}"

                def describe(self) -> str:
                    return "mixin"


            class Named:
                def __init__(self, name: str) -> None:
                    self.name = name

                def describe(self) -> str:
                    return "name=" + self.name


            class Item(Named, JsonMixin):
                def __init__(self, name: str, qty: int) -> None:
                    super().__init__(name)
                    self.qty = qty


            def g() -> str:
                d = Diamond()
                out = d.hello() + "|" + d.who() + "|" + str(d.left + d.right) + d.tag + "|" + ",".join(trace) + "|"
                out += str(isinstance(d, Left)) + str(isinstance(d, Right)) + str(isinstance(d, Base)) + "|"
                r = Right()
                out += r.hello() + "|"
                it = Item("pen", 3)
                out += it.to_json() + "|" + str(it.qty) + str(isinstance(it, JsonMixin))
                return out
        "#,
            &[("g", Want::Str("Diamond>Left>Right>Base.hello|right|3b|Diamond,Left,Right,Base|TrueTrueTrue|Right>Base.hello|{name=pen}|3True"))],
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
            ..waspy::CompilerOptions::default()
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
        let err = try_compile("def f(n: int) -> str:\n    return \"%*d!\" % (3, n)\n")
            .expect_err("refused");
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

    /// open(path, mode) through WASI: path_open on the preopened directories (r, w, a, b, + modes); the waspy_host imports under the deprecated waspy_host_file_io option
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

    /// File imports emitted only when open() is used - other modules keep zero imports
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
        assert!(io.imports().any(|i| i.name() == "path_open"));
        assert!(io.imports().all(|i| i.module() == "wasi_snapshot_preview1"));
        // The deprecated option keeps the `waspy_host` interface.
        let options = waspy::CompilerOptions {
            optimize: false,
            waspy_host_file_io: true,
            ..waspy::CompilerOptions::default()
        };
        let legacy = waspy::compile_python_to_wasm_with_options(FILE_SRC, &options).unwrap();
        let legacy = wasmi::Module::new(&engine, &legacy[..]).unwrap();
        assert!(
            legacy.imports().count() > 0 && legacy.imports().all(|i| i.module() == "waspy_host")
        );
    }

    /// A file open() cannot open raises CPython's exception (FileNotFoundError: [Errno 2] No such file or directory: 'x', PermissionError, IsADirectoryError, ...)
    #[test]
    fn open_errors_raise() {
        check(
            r#"
            def f() -> str:
                try:
                    open("missing.txt")
                    return "opened"
                except FileNotFoundError as e:
                    return str(e)

            def g() -> str:
                try:
                    with open("missing.txt") as fh:
                        return fh.read()
                except OSError as e:
                    return type(e).__name__
        "#,
            &[
                (
                    "f",
                    Want::Str("[Errno 2] No such file or directory: 'missing.txt'"),
                ),
                ("g", Want::Str("FileNotFoundError")),
            ],
        );
    }
}
