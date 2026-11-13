//! Runtime coverage for the core-syntax constructs: nested functions and
//! `nonlocal`, star parameters, class objects, imports, and the cases around
//! them that have to be refused rather than answered wrong.
//!
//! Every expected value is what CPython answers for the same source.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{
    call_f64, call_i32, call_i32_traps, call_instance_i32, call_str, dedent, instantiate_wasm,
    read_str, run_main, try_compile, try_compile_multi, Run,
};

fn refused(source: &str, needle: &str) {
    let err = try_compile(&dedent(source)).expect_err("this construct must be refused");
    assert!(err.contains(needle), "expected `{needle}` in: {err}");
}

// ---------------------------------------------------------------------------
// Nested functions
// ---------------------------------------------------------------------------

/// A nested function keeps its annotations, so it takes and returns floats and
/// strings at their real types, and reads the enclosing variables it closes
/// over at theirs.
#[test]
fn nested_functions_are_typed() {
    let src = dedent(
        r#"
        def greet(name: str) -> str:
            prefix = "Hello, "
            def build(suffix: str) -> str:
                return prefix + name + suffix
            return build("!")

        def t_greet() -> str:
            return greet("Ann")

        def t_scale() -> float:
            k = 2.0
            def mul(x: float) -> float:
                return x * k
            return mul(2.5) + mul(1.0)

        def t_float_capture() -> float:
            rate = 1.5
            def apply(x: int) -> float:
                return x * rate
            return apply(4)

        def t_str_capture() -> int:
            word = "hello"
            def size() -> int:
                return len(word)
            return size()
        "#,
    );
    assert_eq!(call_str(&src, "t_greet"), "Hello, Ann!");
    assert_eq!(call_f64(&src, "t_scale"), 7.0);
    assert_eq!(call_f64(&src, "t_float_capture"), 6.0);
    assert_eq!(call_i32(&src, "t_str_capture"), 5);
}

/// Recursion, mutual recursion between siblings, keywords and defaults, a
/// second level of nesting, and a local that shadows an enclosing one.
#[test]
fn nested_functions_call_like_functions() {
    let src = dedent(
        r#"
        def fact(n: int) -> int:
            def go(k: int) -> int:
                if k <= 1:
                    return 1
                return k * go(k - 1)
            return go(n)

        def t_fact() -> int:
            return fact(5) + fact(1) * 1000

        def t_mutual() -> int:
            def is_even(n: int) -> bool:
                if n == 0:
                    return True
                return is_odd(n - 1)
            def is_odd(n: int) -> bool:
                if n == 0:
                    return False
                return is_even(n - 1)
            return int(is_even(10)) * 10 + int(is_odd(7))

        def t_kw() -> int:
            def area(w: int, h: int = 2) -> int:
                return w * h
            return area(3) + area(h=5, w=2) * 100

        def t_deep() -> int:
            x = 1
            def mid() -> int:
                def inner() -> int:
                    return x + 100
                return inner() + 10
            return mid()

        def t_shadow() -> int:
            x = 5
            def f() -> int:
                x = 100
                return x
            return f() + x
        "#,
    );
    assert_eq!(call_i32(&src, "t_fact"), 1120);
    assert_eq!(call_i32(&src, "t_mutual"), 11);
    assert_eq!(call_i32(&src, "t_kw"), 1006);
    assert_eq!(call_i32(&src, "t_deep"), 111);
    assert_eq!(call_i32(&src, "t_shadow"), 105);
}

/// A nested function is a value: returned, stored, and passed to a parameter
/// annotated with its signature, it is called at that signature, and each one
/// closes over the variables of the call that made it.
#[test]
fn nested_functions_are_values() {
    let src = dedent(
        r#"
        from typing import Callable

        def make_adder(n: int):
            def add(x: int) -> int:
                return x + n
            return add

        def t_adders() -> int:
            a = make_adder(3)
            b = make_adder(10)
            return a(1) * 100 + b(1)

        def t_escape_str() -> str:
            def make(p: str):
                def wrap(s: str) -> str:
                    return p + s + p
                return wrap
            w = make("*")
            return w("x")

        def apply(cb: Callable[[float], float]) -> float:
            return cb(2.0)

        def t_passed() -> float:
            def g(n: float) -> float:
                return n * 1.5
            return apply(g)

        def twice(cb, v: int) -> int:
            return cb(cb(v))

        def t_untyped_param() -> int:
            def g(n: int) -> int:
                return n * 3
            return twice(g, 2)
        "#,
    );
    assert_eq!(call_i32(&src, "t_adders"), 411);
    assert_eq!(call_str(&src, "t_escape_str"), "*x*");
    assert_eq!(call_f64(&src, "t_passed"), 3.0);
    assert_eq!(call_i32(&src, "t_untyped_param"), 18);
}

/// An exception raised in a nested function reaches the enclosing `try`,
/// whether the function is called by name or through a variable.
#[test]
fn nested_functions_raise() {
    let src = dedent(
        r#"
        def check(n: int) -> int:
            if n < 0:
                raise ValueError("neg")
            return n

        def direct() -> int:
            def g(n: int) -> int:
                return check(n)
            try:
                return g(-1)
            except ValueError:
                return 42

        def through_variable() -> int:
            def g(n: int) -> int:
                return check(n)
            h = g
            try:
                return h(-1)
            except ValueError:
                return 43
        "#,
    );
    assert_eq!(call_i32(&src, "direct"), 42);
    assert_eq!(call_i32(&src, "through_variable"), 43);
}

/// A method's nested function closes over `self`.
#[test]
fn nested_function_in_a_method() {
    let src = dedent(
        r#"
        class Acc:
            def __init__(self) -> None:
                self.total = 0
            def run(self) -> int:
                def bump(n: int) -> None:
                    self.total += n
                bump(3)
                bump(4)
                return self.total

        def f() -> int:
            return Acc().run()
        "#,
    );
    assert_eq!(call_i32(&src, "f"), 7);
}

// ---------------------------------------------------------------------------
// nonlocal
// ---------------------------------------------------------------------------

/// `nonlocal` rebinds the enclosing variable for every reader, including a
/// variable the nested function only assigns and one bound after the `def`.
#[test]
fn nonlocal_rebinds_the_enclosing_variable() {
    let src = dedent(
        r#"
        def counter() -> int:
            total = 0
            def add(n: int) -> None:
                nonlocal total
                total += n
            for i in range(5):
                add(i)
            return total

        def assign_only() -> int:
            x = 1
            def g() -> None:
                nonlocal x
                x = 7
            g()
            return x

        def bound_later() -> int:
            def g() -> int:
                nonlocal x
                return x
            x = 9
            return g()

        def strings() -> str:
            s = "a"
            def add(c: str) -> None:
                nonlocal s
                s = s + c
            add("b")
            add("c")
            return s
        "#,
    );
    assert_eq!(call_i32(&src, "counter"), 10);
    assert_eq!(call_i32(&src, "assign_only"), 7);
    assert_eq!(call_i32(&src, "bound_later"), 9);
    assert_eq!(call_str(&src, "strings"), "abc");
}

/// What a nested function cannot do here is refused, never answered wrong.
#[test]
fn nested_function_refusals() {
    // CPython raises UnboundLocalError.
    refused(
        r#"
        def f() -> int:
            r = g()
            def g() -> int:
                return 1
            return r
        "#,
        "before its 'def' runs",
    );
    // CPython: SyntaxError, no binding for nonlocal 'zz' found.
    refused(
        r#"
        def f() -> int:
            def g() -> None:
                nonlocal zz
                zz = 1
            return 0
        "#,
        "no binding for nonlocal 'zz'",
    );
    // A captured variable keeps one type; CPython answers 's'.
    refused(
        r#"
        def f() -> int:
            x = 1
            def g() -> None:
                nonlocal x
                x = "s"
            g()
            return x
        "#,
        "a closure reads it at one type",
    );
    // A str-returning function through an untyped parameter would come back
    // as an int.
    refused(
        r#"
        def h(cb) -> int:
            return cb(2)

        def f() -> int:
            def g(s: str) -> str:
                return s
            return h(g)
        "#,
        "a function is passed through a place with no type",
    );
    refused(
        r#"
        def f() -> int:
            def g():
                yield 1
            return 0
        "#,
        "nested generator",
    );
}

/// A sibling called before its `def` has no closure yet; CPython raises
/// NameError, and the call traps rather than reading a null environment.
#[test]
fn a_sibling_called_before_its_def_traps() {
    let src = dedent(
        r#"
        def f() -> int:
            def a() -> int:
                return b()
            r = a()
            def b() -> int:
                return 2
            return r
        "#,
    );
    assert!(call_i32_traps(&src, "f"));
}

// ---------------------------------------------------------------------------
// *args and **kwargs
// ---------------------------------------------------------------------------

/// `*args` is a tuple typed by its annotation, built at the member type, and
/// read like one: iteration, indexing, slicing, `len`, `in`, `count`,
/// `index`, equality with a tuple literal, unpacking, `isinstance`, and as a
/// dict key that a fixed tuple finds.
#[test]
fn star_args_is_a_tuple() {
    let src = dedent(
        r#"
        from typing import Dict, Tuple

        def fsum(*xs: float) -> float:
            s = 0.0
            for x in xs:
                s += x
            return s

        def t_float() -> float:
            return fsum(1.0, 2.5, 3.0)

        def names(*parts: str) -> str:
            return "-".join(parts)

        def t_names() -> str:
            return names("a", "b", "c")

        def t_tuple_ops() -> int:
            def inner(*ns: int) -> int:
                t = ns[1:]
                r = 0
                if ns == (1, 2, 3):
                    r += 1
                if 2 in ns:
                    r += 10
                r += ns.count(2) * 100
                r += ns.index(3) * 1000
                r += len(t) * 10000
                return r
            return inner(1, 2, 3)

        def t_isinstance() -> int:
            def inner(*ns: int) -> bool:
                return isinstance(ns, tuple)
            return int(inner(1))

        def t_unpack() -> int:
            def inner(*ns: int) -> int:
                a, b = ns
                return a * 10 + b
            return inner(3, 4)

        def t_unpack_wrong_length() -> int:
            def inner(*ns: int) -> int:
                a, b = ns
                return a
            try:
                return inner(1, 2, 3)
            except ValueError:
                return -5

        def key(*ns: int) -> Tuple[int, ...]:
            return ns

        def t_hash() -> int:
            d: Dict[Tuple[int, ...], int] = {}
            d[key(1, 2)] = 5
            return d[(1, 2)] if (1, 2) in d else -1
        "#,
    );
    assert_eq!(call_f64(&src, "t_float"), 6.5);
    assert_eq!(call_str(&src, "t_names"), "a-b-c");
    assert_eq!(call_i32(&src, "t_tuple_ops"), 22111);
    assert_eq!(call_i32(&src, "t_isinstance"), 1);
    assert_eq!(call_i32(&src, "t_unpack"), 34);
    assert_eq!(call_i32(&src, "t_unpack_wrong_length"), -5);
    assert_eq!(call_i32(&src, "t_hash"), 5);
}

/// `f(*xs)` passes a list or a tuple on as `*args`, and the parameters combine
/// in CPython's order: positional, `*args`, keyword-only, `**kwargs`.
#[test]
fn star_arguments_combine() {
    let src = dedent(
        r#"
        def total(*nums: int) -> int:
            s = 0
            for n in nums:
                s += n
            return s

        def fwd(*args: int) -> int:
            return total(*args) + len(args)

        def t_forward() -> int:
            xs = [4, 5, 6]
            return fwd(1, 2) * 100 + total(*xs)

        def mixed(a: int, *rest: int, scale: int = 1, **kw: int) -> int:
            return (a + sum(rest)) * scale + len(kw) * 1000 + kw.get("bonus", 0)

        def t_mixed() -> int:
            return mixed(1, 2, 3, scale=10, bonus=5, other=7)

        def floats(**kw: float) -> float:
            return kw["a"] + kw["b"]

        def t_kwargs_float() -> float:
            return floats(a=1.0, b=2.5)

        def t_kwargs_lookup() -> int:
            def g(**kw: int) -> int:
                return kw["a"] * 10 + kw.get("b", 7)
            return g(a=4)
        "#,
    );
    assert_eq!(call_i32(&src, "t_forward"), 515);
    assert_eq!(call_i32(&src, "t_mixed"), 2065);
    assert_eq!(call_f64(&src, "t_kwargs_float"), 3.5);
    assert_eq!(call_i32(&src, "t_kwargs_lookup"), 47);
}

/// What a star parameter cannot take is refused at compile time, with
/// CPython's TypeError where it raises one.
#[test]
fn star_parameter_refusals() {
    refused(
        r#"
        def g(*ns: int) -> int:
            ns.append(1)
            return 0

        def f() -> int:
            return g()
        "#,
        "'tuple' object has no attribute 'append'",
    );
    refused(
        r#"
        def g(*ns: int) -> int:
            ns[0] = 1
            return 0

        def f() -> int:
            return g(1)
        "#,
        "does not support item assignment",
    );
    refused(
        r#"
        def g(a: int, *rest: int) -> int:
            return a

        def f() -> int:
            return g(1, a=2)
        "#,
        "got multiple values for argument 'a'",
    );
    refused(
        r#"
        def g(*rest: int) -> int:
            return len(rest)

        def f() -> int:
            return g(rest=2)
        "#,
        "unexpected keyword argument 'rest'",
    );
    // An unannotated *args holds words only.
    refused(
        r#"
        def g(*xs) -> int:
            return len(xs)

        def f() -> int:
            return g("a", "b")
        "#,
        "a str is passed through a place with no type",
    );
    // A list of floats cannot be read as a tuple of ints.
    refused(
        r#"
        def g(*xs: int) -> int:
            return len(xs)

        def f() -> int:
            ys = [1.5]
            return g(*ys)
        "#,
        "where the parameter is a 'Tuple[int, ...]'",
    );
    refused(
        r#"
        def g(**kw: int) -> int:
            return len(kw)

        def f() -> int:
            return g(a="x")
        "#,
        "stores a 'str' into a collection whose elements are 'int'",
    );
}

// ---------------------------------------------------------------------------
// min() and max() over an iterable, and functions as values
// ---------------------------------------------------------------------------

/// `min`/`max` fold a list, tuple, or range, keep the first of equal
/// elements, compare keys where `key=` is given, answer `default` for an empty
/// iterable and raise ValueError without one.
#[test]
fn min_max_over_an_iterable() {
    let src = dedent(
        r#"
        from typing import List

        def ints() -> int:
            xs = [3, 1, 2]
            return min(xs) * 10 + max(xs)

        def floats() -> float:
            xs = [3.5, 1.25, 2.0]
            return min(xs) + max(xs) * 10

        def strings() -> str:
            ws = ["pear", "fig", "banana"]
            return min(ws) + max(ws)

        def keyed() -> str:
            ws = ["pear", "fig", "banana"]
            return min(ws, key=lambda w: len(w)) + max(ws, key=len)

        def ties() -> int:
            pairs = [(2, "b"), (1, "z"), (2, "a")]
            a = min(pairs)
            b = max(pairs, key=lambda p: p[0])
            return a[0] * 100 + b[0] * 10 + len(b[1])

        def several_keyed() -> str:
            return max("b", "aa", "c", key=lambda s: len(s))

        def ranges() -> int:
            return max(range(3, 10, 2)) * 10 + min(range(5))

        def defaulted() -> int:
            xs: List[int] = []
            return min(xs, default=-1)

        def empty() -> int:
            xs: List[int] = []
            try:
                return max(xs)
            except ValueError:
                return -2

        def by_abs() -> int:
            xs = [-3, 1, -2]
            return max(xs, key=abs)
        "#,
    );
    assert_eq!(call_i32(&src, "ints"), 13);
    assert_eq!(call_f64(&src, "floats"), 36.25);
    assert_eq!(call_str(&src, "strings"), "bananapear");
    assert_eq!(call_str(&src, "keyed"), "figbanana");
    assert_eq!(call_i32(&src, "ties"), 121);
    assert_eq!(call_str(&src, "several_keyed"), "aa");
    assert_eq!(call_i32(&src, "ranges"), 90);
    assert_eq!(call_i32(&src, "defaulted"), -1);
    assert_eq!(call_i32(&src, "empty"), -2);
    assert_eq!(call_i32(&src, "by_abs"), -3);
}

/// A module function or `len` named as a value is a function value, called
/// at its signature.
#[test]
fn functions_as_values() {
    let src = dedent(
        r#"
        def double(x: int) -> int:
            return x * 2

        def neg(v: int) -> int:
            return -v

        def apply(fn, v: int) -> int:
            return fn(v)

        def passed() -> int:
            return apply(double, 3)

        def stored() -> int:
            h = double
            return h(5)

        def sort_key() -> int:
            xs = [3, 1, 2]
            ys = sorted(xs, key=neg)
            return ys[0] * 100 + ys[1] * 10 + ys[2]

        def by_len() -> str:
            ws = ["pear", "fig", "banana"]
            return "-".join(sorted(ws, key=len))
        "#,
    );
    assert_eq!(call_i32(&src, "passed"), 6);
    assert_eq!(call_i32(&src, "stored"), 10);
    assert_eq!(call_i32(&src, "sort_key"), 321);
    assert_eq!(call_str(&src, "by_len"), "fig-pear-banana");
}

// ---------------------------------------------------------------------------
// Class objects
// ---------------------------------------------------------------------------

/// A class is a value: `type(x)` and `x.__class__` give an instance's own
/// class, compared by identity, named by `__name__`, shown as
/// `<class '__main__.A'>`, and read for its class variables (the subclass's
/// own when it defines one).
#[test]
fn class_objects() {
    let src = dedent(
        r#"
        from typing import Dict, List

        class Shape:
            sides = 0

        class Square(Shape):
            sides = 4

        class Tri(Shape):
            sides = 3

        def kind(s: Shape) -> str:
            c = type(s)
            if c is Square:
                return "sq"
            if c == Tri:
                return "tri"
            return "other"

        def f() -> str:
            shapes: List[Shape] = [Square(), Tri(), Shape()]
            out = ",".join([kind(s) for s in shapes]) + "|"
            out += ",".join([type(s).__name__ for s in shapes]) + "|"
            out += str(sum([type(s).sides for s in shapes])) + "|"
            counts: Dict[str, int] = {}
            for s in shapes:
                n = s.__class__.__name__
                counts[n] = counts.get(n, 0) + 1
            out += str(len(counts)) + "|"
            c = Square
            out += str(c) + repr(Tri) + f"{Shape}" + "|"
            return out + str(c is not Tri) + str(type(shapes[0]) != Square)
        "#,
    );
    assert_eq!(
        call_str(&src, "f"),
        "sq,tri,other|Square,Tri,Shape|7|3|<class '__main__.Square'><class '__main__.Tri'>\
         <class '__main__.Shape'>|TrueFalse"
    );
    refused(
        "def f() -> str:\n    return type(3).__name__\n",
        "type() of a 'int' is not supported",
    );
    refused(
        "class A:\n    pass\n\ndef f() -> int:\n    c = A\n    return len(c.__dict__)\n",
        "reading '__dict__' of the class object of 'A' is not supported",
    );
}

/// A class merged in from another file shows its module, as CPython's repr
/// does for an imported class.
#[test]
fn class_repr_names_its_module() {
    let wasm = try_compile_multi(&[
        (
            "main.py",
            "from shapes import Square\n\nclass Local:\n    pass\n\n\
             def f() -> str:\n    return repr(Square) + repr(Local)\n",
        ),
        ("shapes.py", "class Square:\n    pass\n"),
    ])
    .expect("compiles");
    let (instance, mut store) = instantiate_wasm(&wasm);
    let offset = call_instance_i32(&instance, &mut store, "f");
    assert_eq!(
        read_str(&instance, &store, offset),
        "<class 'shapes.Square'><class '__main__.Local'>"
    );
}

/// `__name__` is `"__main__"` in the entry file and the module's own name in
/// one it imports, so `logging.getLogger(__name__)` names each logger as
/// CPython does.
#[test]
fn name_is_the_modules() {
    let wasm = try_compile_multi(&[
        (
            "main.py",
            "from util import where\n\ndef f() -> str:\n    return __name__ + ' ' + where()\n",
        ),
        ("util.py", "def where() -> str:\n    return __name__\n"),
    ])
    .expect("compiles");
    let (instance, mut store) = instantiate_wasm(&wasm);
    let offset = call_instance_i32(&instance, &mut store, "f");
    assert_eq!(read_str(&instance, &store, offset), "__main__ util");
}

// ---------------------------------------------------------------------------
// Special methods
// ---------------------------------------------------------------------------

/// Operators, subscripts, `in`, `len()`, truth, `+=`, and calling an instance
/// go through the class's special methods: the left operand's own, or the
/// right one's reflected method. `len()` and truth check `__len__`'s answer.
#[test]
fn special_methods_implement_operators() {
    let run = run_main(
        &dedent(
            r#"
        class V:
            def __init__(self, x: int) -> None:
                self.x = x

            def __add__(self, o: "V") -> "V":
                return V(self.x + o.x)

            def __radd__(self, o: int) -> "V":
                return V(self.x + o)

            def __mul__(self, k: int) -> "V":
                return V(self.x * k)

            def __neg__(self) -> "V":
                return V(-self.x)

            def __len__(self) -> int:
                return self.x

            def __getitem__(self, i: int) -> int:
                return self.x * i

            def __contains__(self, i: int) -> bool:
                return i == self.x

            def __repr__(self) -> str:
                return f"V({self.x})"


        class Box:
            def __init__(self) -> None:
                self.d: dict[str, int] = {}

            def __setitem__(self, k: str, v: int) -> None:
                self.d[k] = v * 10

            def __getitem__(self, k: str) -> int:
                return self.d[k]

            def __bool__(self) -> bool:
                return len(self.d) > 0

            def __call__(self, k: str) -> int:
                return self.d[k] + 1


        class Neg:
            def __len__(self) -> int:
                return -1


        def main() -> None:
            a = V(2) + V(3)
            print(a, len(a), a[4], 5 in a, 4 in a, 6 not in a, -a, a * 3, 1 + a)
            a += V(10)
            print(a)
            a *= 2
            print(a)
            b = Box()
            print(bool(b), "yes" if b else "no")
            b["k"] = 4
            b["k"] += 1
            print(b["k"], b("k"), bool(b), not b)
            try:
                print(len(Neg()))
            except ValueError as e:
                print("ValueError", e)
            try:
                if Neg():
                    print("truthy")
            except ValueError as e:
                print("ValueError", e)


        if __name__ == "__main__":
            main()
    "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "V(5) 5 20 True False True V(-5) V(15) V(6)\nV(15)\nV(30)\nFalse no\n410 411 True False\nValueError __len__() should return >= 0\nValueError __len__() should return >= 0\n");
}

/// An operator no special method implements is still a TypeError at compile
/// time, and `__len__` declared to return something else is refused.
#[test]
fn special_methods_refusals() {
    refused(
        r#"
        class A:
            def __init__(self) -> None:
                self.x = 1

        def f() -> int:
            return (A() - A()).x
        "#,
        "unsupported operand type(s) for -",
    );
    refused(
        r#"
        class A:
            def __len__(self) -> str:
                return "x"

        def f() -> int:
            return len(A())
        "#,
        "__len__ is declared to return 'str'",
    );
}

/// Dicts compare by their entries whatever their order, and an empty literal
/// compares as the other side's type; comparing two dicts was refused.
#[test]
fn dict_equality() {
    let run = run_main(
        &dedent(
            r#"
            def main() -> None:
                a = {"x": 1, "y": 2}
                b = {"y": 2, "x": 1}
                c = {"x": 1, "y": 3}
                print(a == b, a != b, a == c, a == {"x": 1}, a == {}, {1.5: "a"} == {1.5: "a"})
                print([{"k": [1, 2]}] == [{"k": [1, 2]}], {"k": (1, "a")} == {"k": (1, "b")})


            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "True False False False False True\nTrue False\n"
    );
}

/// `reversed()` where it is walked: a loop, a comprehension, `list()`,
/// `sorted()`, `str.join`, over lists, strings, dicts (their keys), and
/// ranges.
#[test]
fn reversed_walks() {
    let run = run_main(
        &dedent(
            r#"
            def main() -> None:
                xs = [1, 2, 3]
                s = "abc"
                d = {"a": 1, "b": 2}
                for x in reversed(xs):
                    print(x)
                print(list(reversed(xs)), "".join(reversed(s)), [k for k in reversed(d)], sorted(reversed(xs)), list(reversed(range(4))))

            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "3\n2\n1\n[3, 2, 1] cba ['b', 'a'] [1, 2, 3] [3, 2, 1, 0]\n"
    );
}

/// Keyword arguments on a method of the program's classes, placed by name
/// (with the defaults of the ones left out), on an instance and through the
/// class; a builtin value's method still takes none.
#[test]
fn method_keyword_arguments() {
    let run = run_main(
        &dedent(
            r#"
            class Shape:
                def __init__(self, w: int) -> None:
                    self.w = w

                def scaled(self, by: int = 2, plus: int = 0) -> int:
                    return self.w * by + plus

                @classmethod
                def unit(cls, w: int = 1, h: int = 1) -> int:
                    return w * h


            def main() -> None:
                s = Shape(3)
                print(s.scaled(by=4), s.scaled(plus=1), s.scaled(5, plus=2), s.scaled(plus=1, by=3), Shape.unit(h=7))


            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "12 7 17 10 7\n");
    refused(
        r#"
        class A:
            def m(self, x: int) -> int:
                return x

        def f() -> int:
            return A().m(y=1)
        "#,
        "A.m() got an unexpected keyword argument 'y'",
    );
    refused(
        r#"
        def f() -> list[str]:
            return "a,b".split(sep=",")
        "#,
        "keyword argument 'sep' is not supported on 'split()'",
    );
}

/// Fields and methods read through an optional instance (a linked list's `next`): the instance's, after CPython's AttributeError for None.
#[test]
fn optional_instances_linked_list() {
    let run = run_main(
        &dedent(
            r#"
            from typing import Optional


            class Node:
                def __init__(self, value: int, next: Optional["Node"] = None) -> None:
                    self.value = value
                    self.next = next


            def total(head: Optional[Node]) -> int:
                t = 0
                while head is not None:
                    t += head.value
                    head = head.next
                return t


            def main() -> None:
                lst = Node(1, Node(2, Node(3)))
                print(total(lst), lst.next.value, lst.next.next.value)
                try:
                    print(lst.next.next.next.value)
                except AttributeError as e:
                    print("AttributeError:", e)


            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "6 2 3\nAttributeError: 'NoneType' object has no attribute 'value'\n"
    );
}

/// A method called on an optional field after a None check, and an optional stored into a plain field.
#[test]
fn optional_instance_methods() {
    let run = run_main(
        &dedent(
            r#"
            from typing import Optional


            class Z:
                def __init__(self, n: int) -> None:
                    self.n = n

                def get(self) -> int:
                    return self.n


            class H:
                def __init__(self, z: Optional[Z] = None) -> None:
                    self.z: Optional[Z] = z
                    self.plain: Z = Z(0)
                    if z is not None:
                        self.plain = z

                def value(self) -> int:
                    if self.z is None:
                        return -1
                    return self.z.get()


            def main() -> None:
                print(H().value(), H(Z(5)).value(), H(Z(7)).plain.n, H().z is None)


            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout, "-1 5 7 True\n");
}

// ---------------------------------------------------------------------------
// Loops over items(), keys(), and values()
// ---------------------------------------------------------------------------

/// `for k, v in x.items()` (and `.keys()` / `.values()`) calls the method
/// when `x` is an instance of a class that defines it, inherited, through a
/// parameter or a field, or as a generator; a dict, and the `collections`
/// classes that read as one, walk their entries in place. A user class was
/// lowered as a dict and refused with "has no len()", the `collections`
/// classes' loops were refused, and `for kv in d.items()` needed two targets.
#[test]
fn loops_over_views_call_the_method_or_walk_the_dict() {
    let run = run_main(
        &dedent(
            r#"
            from collections import ChainMap, Counter, OrderedDict, defaultdict
            from typing import Dict, Iterator, List, Tuple


            class Bag:
                def __init__(self) -> None:
                    self.names: List[str] = ["a", "bb"]
                    self.counts: List[int] = [3, 4]

                def items(self) -> List[Tuple[str, int]]:
                    return [(self.names[i], self.counts[i]) for i in range(len(self.names))]

                def keys(self) -> List[str]:
                    return self.names

                def total(self) -> int:
                    t = 0
                    for k, v in self.items():
                        t += v
                    return t


            class Sack(Bag):
                pass


            class Shelf:
                def __init__(self) -> None:
                    self.bag: Bag = Sack()

                def labels(self) -> str:
                    out = ""
                    for k in self.bag.keys():
                        out += k
                    return out


            class Ticker:
                def values(self) -> Iterator[int]:
                    yield 7
                    yield 8


            def from_param(b: Bag) -> str:
                out = ""
                for k, v in b.items():
                    if v == 4:
                        break
                    out += k * v
                return out


            def main() -> None:
                sack = Sack()
                print([kv for kv in sack.items()], sack.total(), from_param(Bag()), Shelf().labels())
                print(sum(x for x in Ticker().values()))
                t = 0
                for x in Ticker().values():
                    t += x
                print(t)
                d: Dict[str, int] = {"x": 1, "yy": 2}
                for kv in d.items():
                    print(kv)
                c: Counter[str] = Counter("abca")
                for k, v in c.items():
                    print(k, v)
                dd: defaultdict[str, List[int]] = defaultdict(list)
                dd["p"].append(1)
                for k in dd.keys():
                    print(k)
                od: OrderedDict[str, int] = OrderedDict()
                od["z"] = 9
                for v in od.values():
                    print(v)
                a: Dict[str, int] = {"x": 1, "y": 2}
                b: Dict[str, int] = {"y": 20, "z": 30}
                cm: ChainMap[str, int] = ChainMap(a, b)
                for k, v in cm.items():
                    print(k, v)


            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "[('a', 3), ('bb', 4)] 7 aaa abb\n15\n15\n('x', 1)\n('yy', 2)\na 2\nb 1\nc 1\np\n9\ny 2\nz 30\nx 1\n"
    );
}

/// A receiver whose class the loop cannot see (a method's result) is refused
/// with a hint, not walked as a dict.
#[test]
fn view_loop_over_an_unknown_instance_is_refused() {
    refused(
        r#"
        from typing import Dict, List, Tuple

        class Bag:
            def items(self) -> List[Tuple[str, int]]:
                return [("a", 1)]

        def total() -> int:
            shelf: Dict[str, Bag] = {"a": Bag()}
            t = 0
            for k, v in shelf["a"].items():
                t += v
            return t
        "#,
        "a for loop over the items(), keys(), or values() of a 'Bag' instance",
    );
}

/// A method's declared result types the instance it returns, so a loop over
/// `.items()` of an instance a chain of calls returns calls the method.
#[test]
fn view_loop_over_a_method_result_calls_the_method() {
    let src = r#"
        from typing import List, Tuple

        class Bag:
            def items(self) -> List[Tuple[str, int]]:
                return [("a", 1), ("b", 5)]

        class Shelf:
            def bag(self) -> Bag:
                return Bag()

        def total() -> int:
            t = 0
            for k, v in Shelf().bag().items():
                t += v
            return t
    "#;
    assert_eq!(call_i32(&dedent(src), "total"), 6);
}

/// What the scan types before code generation: `sorted()` of a dict, a
/// `Counter`, or a `defaultdict` is a list of its keys; a variable a lambda
/// captures keeps its type through its cell; a field read through a captured
/// instance is typed; and a `defaultdict` field annotated `Dict[...]` takes
/// its types from the annotation. Each was refused, the comprehension variables
/// over them untyped.
#[test]
fn sorted_cells_and_dict_annotated_collections_are_typed() {
    let run = run_main(
        &dedent(
            r#"
            from collections import Counter, defaultdict
            from typing import Dict, List


            class Board:
                def __init__(self) -> None:
                    self.by: Dict[str, List[int]] = defaultdict(list)
                    self.notes: List[str] = ["n1", "n2"]


            def ranked(scores: Dict[str, int]) -> List[str]:
                order = sorted(scores, key=lambda k: (-scores[k], k))
                return [k.upper() for k in order]


            def main() -> None:
                b = Board()
                b.by["x"].append(3)
                b.by["y"].append(1)
                b.by["x"].append(2)
                print(sorted(b.by), b.by["x"], len(b.by))
                print(ranked({"b": 2, "a": 2, "c": 5}))
                first = lambda: b
                print([n + "!" for n in b.notes], first().notes[0])
                c = Counter("abcab")
                print(", ".join(f"{k}={c[k]}" for k in sorted(c)))


            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "['x', 'y'] [3, 2] 2\n['C', 'A', 'B']\n['n1!', 'n2!'] n1\na=2, b=2, c=1\n"
    );
}

// ---------------------------------------------------------------------------
// Imports: aliases, imports in a function, and importlib
// ---------------------------------------------------------------------------

/// Run the `__main__` block of a program merged from several files.
fn run_files(files: &[(&str, &str)]) -> Run {
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|(name, text)| (name.to_string(), dedent(text)))
        .collect();
    let refs: Vec<(&str, &str)> = sources
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let wasm = try_compile_multi(&refs).expect("compiles");
    let (instance, mut store) = instantiate_wasm(&wasm);
    instance
        .get_typed_func::<(), ()>(&store, "_start")
        .expect("an exported _start")
        .call(&mut store, ())
        .expect("runs");
    let host = store.data();
    Run {
        stdout: String::from_utf8(host.stdout.clone()).expect("utf-8"),
        stderr: String::from_utf8(host.stderr.clone()).expect("utf-8"),
        exit: 0,
    }
}

/// A standard-library module imported under another name is read through
/// it: `import math as m` answered "name 'm' is not defined", and a
/// submodule's alias (`osp.sep`) and a base class through one
/// (`lh.BufferingHandler`) the same.
#[test]
fn module_aliases_read_through_the_module() {
    let run = run_main(
        &dedent(
            r#"
            import math as m
            import os.path as osp
            import collections as co
            import re as regex
            import logging
            import logging.handlers as lh
            from typing import List


            class Keep(lh.BufferingHandler):
                pass


            def words(text: str) -> List[str]:
                return regex.findall(r"[a-z]+", text)


            if __name__ == "__main__":
                print(m.floor(m.pi * 100), m.sqrt(2.0))
                print(osp.join("a", "b"), osp.sep, osp.basename("x/y.txt"))
                c = co.Counter(words("the cat and the hat"))
                print(c["the"], c.most_common(1))
                print(Keep(3).capacity, isinstance(Keep(1), logging.Handler))
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(
        run.stdout,
        "314 1.4142135623730951\na/b / y.txt\n2 [('the', 2)]\n3 True\n"
    );
}

/// An import in a function, aliased or not, `from` or not, binds there and in
/// the functions nested in it; so does one in the main block. Each was refused
/// as a dynamic import.
#[test]
fn imports_in_a_function_bind_there() {
    let run = run_main(
        &dedent(
            r#"
            def area() -> float:
                import math
                return math.pi * 2.0 ** 2


            def tally(text: str) -> int:
                from collections import Counter as C
                import json as j
                counts = C(text.split())
                return len(j.dumps(counts)) + counts["a"]


            def nested() -> int:
                import math as mm

                def inner(x: float) -> int:
                    return mm.floor(x)

                return inner(7.9)


            if __name__ == "__main__":
                import sys
                from itertools import accumulate as acc
                print(round(area(), 4), tally("a b a"), nested())
                print(list(acc([1, 2, 3])), len(sys.argv))
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stdout, "12.5664 18 7\n[1, 3, 6] 1\n");
}

/// `importlib.import_module` and `__import__` with a constant name, or a
/// module-level constant holding one, are the import they name, however
/// `importlib` was imported. `__import__("os.path")` answers the package
/// `os`, and `import_module("os.path")` the submodule, as in CPython.
#[test]
fn importlib_with_a_constant_name() {
    let run = run_main(
        &dedent(
            r#"
            import importlib
            import importlib as il
            from importlib import import_module

            NAME = "json"


            def first() -> int:
                m = importlib.import_module("math")
                return m.floor(m.e * 1000)


            def second() -> str:
                j = import_module(NAME)
                return j.dumps({"k": [1, 2]})


            def third() -> str:
                o = __import__("os.path")
                p = il.import_module("os.path")
                return o.path.join("a", "b") + p.sep + str(o.sep == p.sep)


            def fourth() -> float:
                return importlib.import_module("math").sqrt(81.0) + __import__("math").tau


            r = importlib.import_module("random")


            def fifth() -> int:
                r.seed(11)
                return r.randrange(1000)


            if __name__ == "__main__":
                print(first(), second(), third())
                print(fourth(), fifth())
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(
        run.stdout,
        "2718 {\"k\": [1, 2]} a/b/True\n15.283185307179586 463\n"
    );
}

/// A user module imported through importlib, `__import__`, or in a function
/// is linked like any other: at module level and in a function, by name and
/// as a value.
#[test]
fn dynamic_imports_of_user_modules() {
    let run = run_files(&[
        (
            "main.py",
            r#"
            import importlib

            g = importlib.import_module("geometry")


            def f() -> int:
                return g.area(3, 5) + g.SIDES


            def h() -> int:
                s = importlib.import_module("shapes")
                return s.perimeter(2, 3)


            def k() -> int:
                from geometry import Box
                return Box(2).volume()


            def m() -> int:
                return __import__("shapes").perimeter(1, 1)


            if __name__ == "__main__":
                print(f(), h(), k(), m())
            "#,
        ),
        (
            "geometry.py",
            r#"
            SIDES = 4


            def area(w: int, h: int) -> int:
                return w * h


            class Box:
                def __init__(self, w: int) -> None:
                    self.w = w

                def volume(self) -> int:
                    return self.w ** 3
            "#,
        ),
        (
            "shapes.py",
            r#"
            def perimeter(w: int, h: int) -> int:
                return 2 * (w + h)
            "#,
        ),
    ]);
    assert_eq!(run.stdout, "19 10 8 4\n");
}

/// Every file's top level runs where CPython runs it: in source order (a
/// class's variables at its class statement, a decorator at its `def`, a
/// metaclass after the class body), and an imported module at its first
/// import, however many files import it. A module first imported in a
/// function starts when that import runs, once, and an exception its top
/// level raises reaches the function's `try`. The entry's whole top level
/// used to run first, every class variable before every module variable, and
/// each imported module after it.
#[test]
fn top_levels_run_in_cpythons_order() {
    let run = run_files(&[
        (
            "main.py",
            r#"
            from log import mark

            M1 = mark("main start")
            import a
            import c
            M2 = mark("main end")


            def deco(f):
                mark("decorating " + f.__name__)
                return f


            @deco
            def g() -> int:
                return 1


            class Meta(type):
                def __init__(cls, name: str, bases: tuple, ns: dict) -> None:
                    mark("meta for " + name)


            class Q(metaclass=Meta):
                q = mark("Q body")


            M3 = mark("after Q")


            def lazy() -> int:
                import late1
                import late1 as again
                return late1.L1 + again.L1


            def risky() -> str:
                try:
                    import boom
                    return "imported"
                except ValueError as e:
                    return "caught " + str(e)


            if __name__ == "__main__":
                print("main block")
                print(lazy())
                print(lazy())
                print(risky())
            "#,
        ),
        (
            "log.py",
            r#"
            def mark(s: str) -> int:
                print(s)
                return 1
            "#,
        ),
        (
            "a.py",
            r#"
            from log import mark
            A1 = mark("a start")
            import b
            A2 = mark("a end")
            "#,
        ),
        (
            "b.py",
            r#"
            from log import mark
            B1 = mark("b")
            "#,
        ),
        (
            "c.py",
            r#"
            from log import mark
            import b
            C1 = mark("c, after b")

            class K:
                k = mark("c class K")

            C2 = mark("c end")
            "#,
        ),
        (
            "late1.py",
            r#"
            from log import mark
            import late2
            L1 = mark("late1")
            "#,
        ),
        (
            "late2.py",
            r#"
            from log import mark
            L2 = mark("late2")
            "#,
        ),
        (
            "boom.py",
            r#"
            def explode() -> int:
                raise ValueError("boom at import")

            X = explode()
            "#,
        ),
    ]);
    assert_eq!(
        run.stdout,
        "main start\na start\nb\na end\nc, after b\nc class K\nc end\nmain end\n\
         decorating g\nQ body\nmeta for Q\nafter Q\nmain block\nlate2\nlate1\n2\n2\n\
         caught boom at import\n"
    );
}

/// A module that starts late cannot start at a dynamic import used in place,
/// which leaves no statement to start it from, so that is refused with a
/// hint to name the import.
#[test]
fn a_late_import_used_in_place_is_refused() {
    let main = dedent(
        r#"
        import importlib

        def f() -> int:
            return importlib.import_module("noisy").VALUE

        if __name__ == "__main__":
            print(f())
        "#,
    );
    let noisy = dedent(
        r#"
        def make() -> int:
            print("noisy init")
            return 5

        VALUE = make()
        "#,
    );
    let err = try_compile_multi(&[("main.py", &main), ("noisy.py", &noisy)]).expect_err("refused");
    assert!(
        err.contains("the module 'noisy' is imported in place in function 'f'")
            && err.contains("assign the import to a name first"),
        "{err}"
    );
}

/// What a dynamic import or an import in a function cannot be compiled as is
/// refused: `__import__` of a name computed at run time (an
/// `importlib.import_module` of one loads through the host, on the
/// JavaScript bridge), the forms that take more than a name, a read before
/// the import, an import that only runs on some paths, a name both imported
/// and assigned, a name another function imported, and a module reached
/// through an alias where its own name is a local.
#[test]
fn imports_that_cannot_be_compiled_are_refused() {
    let loaded = try_compile(&dedent(
        r#"
        import importlib
        def f(name: str) -> int:
            m = importlib.import_module(name)
            return 1
        "#,
    ));
    assert!(
        loaded.is_ok(),
        "a run-time name loads through the host: {loaded:?}"
    );
    refused(
        r#"
        def f() -> int:
            return __import__("ma" + "th").floor(2.5)
        "#,
        "of a name computed at run time is not supported",
    );
    refused(
        r#"
        import importlib
        def f() -> int:
            m = importlib.import_module("math", "pkg")
            return 1
        "#,
        "with the module's name as its only argument",
    );
    refused(
        r#"
        import importlib
        def f() -> int:
            m = importlib.import_module(".math")
            return 1
        "#,
        "of the relative name '.math'",
    );
    refused(
        r#"
        def f() -> int:
            x = math.floor(2.5)
            import math
            return x
        "#,
        "read before the import that binds it",
    );
    refused(
        r#"
        def f(c: bool) -> int:
            if c:
                import math
            return math.floor(2.5)
        "#,
        "an import inside a block that may not run",
    );
    refused(
        r#"
        def f() -> int:
            import math as m
            m = 3
            return m
        "#,
        "'m' is bound by an import and also assigned",
    );
    refused(
        r#"
        def f() -> int:
            from math import floor
            return floor(2.5)

        def g() -> int:
            return floor(3.5)
        "#,
        "it is imported in function 'f', which binds it there only",
    );
    refused(
        r#"
        def f() -> int:
            return floor(3.5)

        if __name__ == "__main__":
            from math import floor
            print(f())
        "#,
        "name 'floor' is imported only in the 'if __name__ == \"__main__\":' block",
    );
    refused(
        r#"
        import math as m
        def f() -> int:
            math = 3
            return m.floor(2.5) + math
        "#,
        "but 'math' is bound to something else in this scope",
    );
    refused(
        r#"
        class C:
            import math
        "#,
        "an import in a class body is not supported",
    );
}
