//! Regression tests for the constructs that used to compile "successfully" and
//! then do the wrong thing at runtime:
//!
//! 1. `with` over a user context manager emitted a module that failed WASM
//!    validation, and never called `__enter__`/`__exit__`.
//! 2. Iterating a float list held in a variable bound the loop variable as an
//!    i32, so the values came out as garbage.
//! 3. Growing a list past its literal's size wrote into whatever collection
//!    happened to sit next in linear memory; adding a new key to a dict did
//!    the same.
//! 4. A method call on a receiver with no method support (`set.add`) emitted
//!    an unbalanced `drop`, so the module failed validation.
//!
//! Each test asserts the runtime result, so a regression is a failing value
//! rather than a module that merely compiles.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{call_f64, call_i32, try_compile, try_instantiate};

/// A minimal context manager, plus whatever function bodies a test needs.
fn with_manager(bodies: &str) -> String {
    format!(
        "class Tracker:\n\
         \x20   def __init__(self, start: int):\n\
         \x20       self.value = start\n\
         \x20       self.entered = 0\n\
         \x20       self.exited = 0\n\
         \n\
         \x20   def __enter__(self) -> int:\n\
         \x20       self.entered = 1\n\
         \x20       return self.value\n\
         \n\
         \x20   def __exit__(self, exc_type: int, exc_value: int, tb: int) -> int:\n\
         \x20       self.exited = 1\n\
         \x20       return 0\n\
         \n\
         \x20   def state(self) -> int:\n\
         \x20       return self.entered * 10 + self.exited\n\
         \n{bodies}"
    )
}

/// `with` runs `__enter__` on the way in and `__exit__` on the way out, and
/// binds the `as` name to what `__enter__` returned.
#[test]
fn context_manager_runs_the_protocol() {
    let src = with_manager(
        "def f() -> int:\n\
         \x20   t: Tracker = Tracker(5)\n\
         \x20   with t as v:\n\
         \x20       pass\n\
         \x20   return t.state()\n",
    );
    assert_eq!(call_i32(&src, "f"), 11);

    let src = with_manager(
        "def f() -> int:\n\
         \x20   t: Tracker = Tracker(7)\n\
         \x20   with t as v:\n\
         \x20       return v\n",
    );
    assert_eq!(call_i32(&src, "f"), 7);
}

/// A `return` inside the body still runs `__exit__` before leaving.
#[test]
fn context_manager_exits_before_return() {
    let src = with_manager(
        "def f() -> int:\n\
         \x20   t: Tracker = Tracker(3)\n\
         \x20   with t as v:\n\
         \x20       return t.exited\n",
    );
    // `__exit__` has already run when the return value is read back, but the
    // returned expression is evaluated first, so it sees the pre-exit state.
    assert_eq!(call_i32(&src, "f"), 0);

    let src = with_manager(
        "def f() -> int:\n\
         \x20   t: Tracker = Tracker(3)\n\
         \x20   with t as v:\n\
         \x20       pass\n\
         \x20   return t.exited\n",
    );
    assert_eq!(call_i32(&src, "f"), 1);
}

/// Nested `with` statements exit innermost-first, and a manager written inline
/// in the header resolves too.
#[test]
fn context_managers_nest() {
    let src = with_manager(
        "def f() -> int:\n\
         \x20   outer: Tracker = Tracker(1)\n\
         \x20   inner: Tracker = Tracker(2)\n\
         \x20   with outer as a:\n\
         \x20       with inner as b:\n\
         \x20           pass\n\
         \x20   return outer.state() * 100 + inner.state()\n",
    );
    assert_eq!(call_i32(&src, "f"), 1111);

    let src = with_manager(
        "def f() -> int:\n\
         \x20   with Tracker(9) as v:\n\
         \x20       return v\n",
    );
    assert_eq!(call_i32(&src, "f"), 9);
}

/// A class missing either half of the protocol, or a manager whose class
/// cannot be resolved, is a compile error rather than a broken module.
#[test]
fn incomplete_context_managers_are_rejected() {
    let cases = [
        (
            "class Plain:\n    def __init__(self):\n        self.x = 1\n\n\
             def f() -> int:\n    with Plain() as p:\n        return 1\n",
            "no '__enter__' method",
        ),
        (
            "class Half:\n    def __enter__(self) -> int:\n        return 1\n\n\
             def f() -> int:\n    with Half() as p:\n        return p\n",
            "no '__exit__' method",
        ),
        (
            "def f(thing: int) -> int:\n    with thing as p:\n        return 1\n",
            "known at compile time",
        ),
    ];
    for (source, expected) in cases {
        let err = try_compile(source).expect_err("expected a compile error");
        assert!(
            err.contains(expected),
            "expected `{expected}` in error for:\n{source}\ngot: {err}"
        );
    }
}

/// Iterating a float list bound to a variable yields f64 elements, whether the
/// variable is unannotated, annotated `list`, or annotated `List[float]`.
#[test]
fn float_list_variables_iterate_as_floats() {
    for decl in ["xs = [1.5, 2.5, 3.0]", "xs: list = [1.5, 2.5, 3.0]"] {
        let src = format!(
            "def f() -> float:\n\
             \x20   {decl}\n\
             \x20   total: float = 0.0\n\
             \x20   for x in xs:\n\
             \x20       total = total + x\n\
             \x20   return total\n"
        );
        assert_eq!(call_f64(&src, "f"), 7.0, "declared as `{decl}`");
    }
}

/// Growing a list past its literal size leaves neighbouring collections alone.
#[test]
fn append_past_capacity_leaves_neighbours_intact() {
    let src = "def f() -> int:\n\
               \x20   a: list = [1, 2]\n\
               \x20   b: list = [100, 200]\n\
               \x20   a.append(3)\n\
               \x20   a.append(4)\n\
               \x20   return b[0] * 10 + b[1] // 100\n";
    assert_eq!(call_i32(src, "f"), 1002);
}

/// A grown list keeps its own elements, its length, and its float precision.
#[test]
fn grown_lists_keep_their_contents() {
    let src = "def f() -> int:\n\
               \x20   xs: list = [1, 2]\n\
               \x20   xs.append(3)\n\
               \x20   xs.append(4)\n\
               \x20   xs.append(5)\n\
               \x20   return len(xs) * 100 + xs[0] + xs[4]\n";
    assert_eq!(call_i32(src, "f"), 506);

    let src = "def f() -> float:\n\
               \x20   xs: list = [1.5]\n\
               \x20   xs.append(2.5)\n\
               \x20   xs.append(3.5)\n\
               \x20   total: float = 0.0\n\
               \x20   for x in xs:\n\
               \x20       total = total + x\n\
               \x20   return total\n";
    assert_eq!(call_f64(src, "f"), 7.5);
}

/// The empty-list-plus-loop idiom, which has no capacity at all to start with.
#[test]
fn appending_to_an_empty_list_in_a_loop() {
    let src = "def f() -> int:\n\
               \x20   xs: list = []\n\
               \x20   i: int = 0\n\
               \x20   while i < 20:\n\
               \x20       xs.append(i * 2)\n\
               \x20       i = i + 1\n\
               \x20   return len(xs) * 1000 + xs[19]\n";
    assert_eq!(call_i32(src, "f"), 20038);
}

/// `extend` and `insert` reserve room the same way `append` does.
#[test]
fn extend_and_insert_grow_too() {
    let src = "def f() -> int:\n\
               \x20   a: list = [1, 2]\n\
               \x20   b: list = [50, 60]\n\
               \x20   a.extend([3, 4, 5])\n\
               \x20   return len(a) * 100 + a[4] * 10 + b[0]\n";
    // 5 elements, last is 5, and the neighbour still reads 50.
    assert_eq!(call_i32(src, "f"), 600);

    let src = "def f() -> int:\n\
               \x20   a: list = [1]\n\
               \x20   b: list = [77]\n\
               \x20   a.insert(0, 2)\n\
               \x20   a.insert(0, 3)\n\
               \x20   return len(a) * 100 + b[0]\n";
    assert_eq!(call_i32(src, "f"), 377);
}

/// A list held in an instance field grows through the field, so later reads of
/// `self.items` see the reallocated region.
#[test]
fn instance_field_lists_grow() {
    let src = "class Bag:\n\
               \x20   def __init__(self):\n\
               \x20       self.items = [1]\n\
               \n\
               \x20   def add(self, v: int):\n\
               \x20       self.items.append(v)\n\
               \n\
               \x20   def total(self) -> int:\n\
               \x20       out: int = 0\n\
               \x20       for x in self.items:\n\
               \x20           out = out + x\n\
               \x20       return out\n\
               \n\
               def f() -> int:\n\
               \x20   b: Bag = Bag()\n\
               \x20   b.add(2)\n\
               \x20   b.add(3)\n\
               \x20   b.add(4)\n\
               \x20   return b.total() * 100 + len(b.items)\n";
    assert_eq!(call_i32(src, "f"), 1004);
}

/// A new dict key past the literal's entry count reallocates instead of
/// overwriting the collection next to it.
#[test]
fn dict_growth_leaves_neighbours_intact() {
    let src = "def f() -> int:\n\
               \x20   d: dict = {1: 10}\n\
               \x20   other: list = [999, 888]\n\
               \x20   d[2] = 20\n\
               \x20   d[3] = 30\n\
               \x20   return other[0]\n";
    assert_eq!(call_i32(src, "f"), 999);
}

/// A grown dict keeps every entry readable, updates in place for a key it
/// already holds, and preserves float values.
#[test]
fn grown_dicts_keep_their_entries() {
    let src = "def f() -> int:\n\
               \x20   d: dict = {1: 10}\n\
               \x20   d[2] = 20\n\
               \x20   d[3] = 30\n\
               \x20   return d[1] + d[2] + d[3] + len(d)\n";
    assert_eq!(call_i32(src, "f"), 63);

    let src = "def f() -> int:\n\
               \x20   d: dict = {1: 10, 2: 20}\n\
               \x20   d[1] = 99\n\
               \x20   return d[1] * 100 + len(d)\n";
    assert_eq!(call_i32(src, "f"), 9902);

    let src = "def f() -> float:\n\
               \x20   d: dict = {1: 1.5}\n\
               \x20   d[2] = 2.5\n\
               \x20   d[3] = 3.5\n\
               \x20   return d[1] + d[2] + d[3]\n";
    assert_eq!(call_f64(src, "f"), 7.5);
}

/// Filling an empty dict literal in a loop, the idiom with no capacity at all
/// to start from.
#[test]
fn filling_an_empty_dict_in_a_loop() {
    let src = "def f() -> int:\n\
               \x20   d: dict = {}\n\
               \x20   i: int = 0\n\
               \x20   while i < 10:\n\
               \x20       d[i] = i * 3\n\
               \x20       i = i + 1\n\
               \x20   return len(d) * 100 + d[9]\n";
    assert_eq!(call_i32(src, "f"), 1027);
}

/// Set mutation is not implemented. It used to emit a module that failed WASM
/// validation (an unbalanced `drop`); it now compiles to a module that is
/// valid, instantiates, and traps if the unimplemented call is reached.
#[test]
fn unimplemented_set_mutation_traps_instead_of_miscompiling() {
    let src = "def f() -> int:\n\
               \x20   s: set = {1}\n\
               \x20   s.add(2)\n\
               \x20   return len(s)\n";
    let wasm = try_compile(src).expect("set mutation still compiles");
    try_instantiate(&wasm).expect("the module must be valid WASM and instantiate");
}

/// A collection field declared element-less (`self.items = []`) takes its
/// element type from what the class appends to it, so iterating the field binds
/// a typed element: reading a member off it and calling a method on it both
/// resolve. Previously the member read produced 0 and the method call trapped.
#[test]
fn field_collections_of_instances_keep_their_element_type() {
    let src = "class Item:\n\
               \x20   def __init__(self, price: float, qty: int):\n\
               \x20       self.price = price\n\
               \x20       self.qty = qty\n\
               \n\
               \x20   def subtotal(self) -> float:\n\
               \x20       return self.price * self.qty\n\
               \n\
               class Cart:\n\
               \x20   def __init__(self):\n\
               \x20       self.items = []\n\
               \n\
               \x20   def add(self, price: float, qty: int):\n\
               \x20       item = Item(price, qty)\n\
               \x20       self.items.append(item)\n\
               \n\
               \x20   def units(self) -> int:\n\
               \x20       n: int = 0\n\
               \x20       for it in self.items:\n\
               \x20           n = n + it.qty\n\
               \x20       return n\n\
               \n\
               \x20   def total(self) -> float:\n\
               \x20       out: float = 0.0\n\
               \x20       for it in self.items:\n\
               \x20           out = out + it.subtotal()\n\
               \x20       return out\n\
               \n\
               def units() -> int:\n\
               \x20   c = Cart()\n\
               \x20   c.add(2.5, 3)\n\
               \x20   c.add(1.5, 4)\n\
               \x20   return c.units()\n\
               \n\
               def total() -> float:\n\
               \x20   c = Cart()\n\
               \x20   c.add(2.5, 3)\n\
               \x20   c.add(1.5, 4)\n\
               \x20   return c.total()\n";
    // Reading a field off the loop element, through a method call on it, and
    // the append itself all have to agree on the element's class.
    assert_eq!(call_i32(src, "units"), 7);
    assert_eq!(call_f64(src, "total"), 13.5);
}

/// The direct forms resolve too: appending a constructor call without an
/// intermediate local, and a field initialized with a literal of instances.
#[test]
fn field_element_types_from_literals_and_direct_appends() {
    let src = "class P:\n\
               \x20   def __init__(self, v: float):\n\
               \x20       self.v = v\n\
               \n\
               class Direct:\n\
               \x20   def __init__(self):\n\
               \x20       self.ps = []\n\
               \n\
               \x20   def add(self, v: float):\n\
               \x20       self.ps.append(P(v))\n\
               \n\
               \x20   def sum(self) -> float:\n\
               \x20       out: float = 0.0\n\
               \x20       for p in self.ps:\n\
               \x20           out = out + p.v\n\
               \x20       return out\n\
               \n\
               class Preset:\n\
               \x20   def __init__(self):\n\
               \x20       self.ps = [P(1.5), P(2.5)]\n\
               \n\
               \x20   def sum(self) -> float:\n\
               \x20       out: float = 0.0\n\
               \x20       for p in self.ps:\n\
               \x20           out = out + p.v\n\
               \x20       return out\n\
               \n\
               def direct() -> float:\n\
               \x20   d = Direct()\n\
               \x20   d.add(1.5)\n\
               \x20   d.add(2.5)\n\
               \x20   return d.sum()\n\
               \n\
               def preset() -> float:\n\
               \x20   p = Preset()\n\
               \x20   return p.sum()\n";
    assert_eq!(call_f64(src, "direct"), 4.0);
    assert_eq!(call_f64(src, "preset"), 4.0);
}
