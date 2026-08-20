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
//! 5. Module-level statements other than plain definitions (a loop, a bare
//!    call, an augmented or unpacking assignment, a write through a subscript
//!    or attribute) were compiled away, so later reads saw the value from
//!    before the statement and the program returned a wrong answer.
//!
//! Each test asserts the runtime result, so a regression is a failing value
//! rather than a module that merely compiles.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{call_f64, call_i32, try_compile};

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

/// Set mutation used to emit a module that failed WASM validation (an
/// unbalanced `drop`), then one that compiled and trapped. It is implemented
/// now, and the memory-safety part of it is the rehash: growing past the
/// literal's capacity must move the table without losing members or writing
/// over the collection that sits next to it in linear memory.
#[test]
fn set_growth_rehashes_without_touching_its_neighbour() {
    let src = "def f() -> int:\n\
               \x20   a = {1}\n\
               \x20   b = [100, 200]\n\
               \x20   i = 0\n\
               \x20   while i < 40:\n\
               \x20       a.add(i)\n\
               \x20       i = i + 1\n\
               \x20   return len(a) * 1000 + b[0] + b[1]\n";
    assert_eq!(call_i32(src, "f"), 40300);
}

/// A tombstone left by `remove` must not be mistaken for the member that was
/// there: the value stays in the bucket, so only the state word distinguishes
/// them. Cycling add/remove also fills the table with tombstones, which is what
/// forces the rehash that compacts them away; without it the insert probe
/// would never find an empty bucket.
#[test]
fn removed_members_stay_removed_through_repeated_churn() {
    let src = "def f() -> int:\n\
               \x20   s = {0}\n\
               \x20   i = 1\n\
               \x20   while i < 200:\n\
               \x20       s.add(i)\n\
               \x20       s.remove(i)\n\
               \x20       i = i + 1\n\
               \x20   n = len(s) * 10\n\
               \x20   if 150 in s:\n\
               \x20       n = n + 1\n\
               \x20   if 0 in s:\n\
               \x20       n = n + 2\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 12);
}

/// A set held in an instance field grows through the field, so the class keeps
/// seeing the reallocated table (the same write-back lists and dicts use).
#[test]
fn set_fields_grow_through_the_field() {
    let src = "class Bag:\n\
               \x20   def __init__(self):\n\
               \x20       self.seen = {0}\n\
               \x20   def put(self, v: int):\n\
               \x20       self.seen.add(v)\n\
               \x20   def size(self) -> int:\n\
               \x20       return len(self.seen)\n\
               \n\
               def f() -> int:\n\
               \x20   b = Bag()\n\
               \x20   i = 1\n\
               \x20   while i < 30:\n\
               \x20       b.put(i)\n\
               \x20       i = i + 1\n\
               \x20   return b.size()\n";
    assert_eq!(call_i32(src, "f"), 30);
}

/// Float members hash and compare at f64 width through mutation too.
#[test]
fn float_set_members_survive_mutation() {
    let src = "def f() -> int:\n\
               \x20   s = {1.5, 2.5}\n\
               \x20   s.add(3.5)\n\
               \x20   s.remove(1.5)\n\
               \x20   n = len(s) * 10\n\
               \x20   if 3.5 in s:\n\
               \x20       n = n + 1\n\
               \x20   if 1.5 in s:\n\
               \x20       n = n + 2\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 21);
}

/// Python raises `KeyError` when `remove` misses. A compiled module has no
/// exception value to raise, so it traps: loud beats quietly doing nothing.
/// `discard` is the method that ignores a miss, and does.
#[test]
fn removing_a_missing_member_traps_but_discarding_does_not() {
    let miss = "def f() -> int:\n\
                \x20   s = {1}\n\
                \x20   s.remove(5)\n\
                \x20   return len(s)\n";
    let wasm = try_compile(miss).expect("compiles");
    let (instance, mut store) = harness::instantiate_wasm(&wasm);
    let call = instance
        .get_typed_func::<(), i32>(&store, "f")
        .expect("exported f");
    assert!(
        call.call(&mut store, ()).is_err(),
        "remove() of a missing member must trap"
    );

    let discard = "def f() -> int:\n\
                   \x20   s = {1}\n\
                   \x20   s.discard(5)\n\
                   \x20   return len(s)\n";
    assert_eq!(call_i32(discard, "f"), 1);
}

/// A method a set does not have is still a compile error through the codegen
/// error channel, naming the method and the function it appears in.
#[test]
fn unsupported_set_methods_are_a_compile_error() {
    let src = "def f() -> int:\n\
               \x20   s = {1}\n\
               \x20   s.union({2})\n\
               \x20   return len(s)\n";
    let err = try_compile(src).expect_err("union() is not implemented");
    assert!(
        err.contains("'union' is not supported"),
        "unexpected: {err}"
    );
    assert!(
        err.contains("in function 'f'"),
        "the error must name the function it is in: {err}"
    );
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

/// Module-level statements the compiler cannot execute are rejected up front
/// instead of being silently dropped. Each of these compiled "successfully"
/// and then returned the pre-statement value (or, for tuple unpacking,
/// garbage), which is exactly what the correctness rule forbids.
#[test]
fn silently_dropped_module_level_statements_are_rejected() {
    let cases: &[(&str, &str)] = &[
        (
            "TOTAL = 0\nfor i in range(4):\n    TOTAL = TOTAL + i\n\ndef get() -> int:\n    return TOTAL\n",
            "'for' loops at module level",
        ),
        (
            "N = 0\nwhile N < 3:\n    N = N + 1\n\ndef get() -> int:\n    return N\n",
            "'while' loops at module level",
        ),
        (
            "FLAG = 1\nVALUE = 0\nif FLAG:\n    VALUE = 10\n\ndef get() -> int:\n    return VALUE\n",
            "'if' statements at module level",
        ),
        (
            "V = 0\ntry:\n    V = 5\nexcept ValueError:\n    V = 1\n\ndef get() -> int:\n    return V\n",
            "'try' blocks at module level",
        ),
        (
            "ITEMS = [1, 2]\nITEMS.append(3)\n\ndef get() -> int:\n    return len(ITEMS)\n",
            "call statements at module level",
        ),
        (
            "TOTAL = 1\nTOTAL += 2\n\ndef get() -> int:\n    return TOTAL\n",
            "augmented assignments at module level",
        ),
        (
            "D = {1: 0}\nD[1] = 9\n\ndef get() -> int:\n    return D[1]\n",
            "assignments to anything but a plain name at module level",
        ),
        (
            "A, B = 1, 2\n\ndef get() -> int:\n    return A + B\n",
            "assignments to anything but a plain name at module level",
        ),
    ];

    for (source, expected) in cases {
        let err = try_compile(source).expect_err("must be rejected, not silently dropped");
        assert!(
            err.contains(expected),
            "expected {expected:?} in the error, got: {err}"
        );
        assert!(err.contains("line"), "the error must be located: {err}");
        assert!(
            err.contains("Hint:"),
            "the error must say what to do instead: {err}"
        );
    }
}

/// The module-level shapes that do work keep working: a constant, a call, an
/// instantiation, an expression over an earlier definition, and the
/// entry-point guard (which is a marker, not executable module code).
#[test]
fn supported_module_level_definitions_still_run() {
    assert_eq!(
        call_i32(
            "def helper() -> int:\n    return 7\n\nVALUE = helper()\n\ndef get() -> int:\n    return VALUE\n",
            "get"
        ),
        7
    );
    assert_eq!(
        call_i32(
            "class C:\n\
             \x20   def __init__(self):\n\
             \x20       self.n = 5\n\
             \x20   def value(self) -> int:\n\
             \x20       return self.n\n\
             \n\
             SHARED = C()\n\
             \n\
             def get() -> int:\n\
             \x20   return SHARED.value()\n",
            "get"
        ),
        5
    );
    assert_eq!(
        call_i32(
            "def base() -> int:\n    return 3\n\nA = base()\nB = A * 2\n\ndef get() -> int:\n    return B\n",
            "get"
        ),
        6
    );
    assert_eq!(
        call_i32(
            "PI = 3\n\ndef main() -> int:\n    return PI\n\nif __name__ == \"__main__\":\n    main()\n",
            "main"
        ),
        3
    );
}

/// A guarded import (`try: import x except ImportError: import y`) is still
/// allowed at module level: it is a supported shape with no runtime effect to
/// drop, and rejecting every module-level `try` would have broken it.
#[test]
fn guarded_imports_stay_allowed_at_module_level() {
    let src = "try:\n\
               \x20   import ujson\n\
               except ImportError:\n\
               \x20   import json\n\
               \n\
               def get() -> int:\n\
               \x20   return 1\n";
    assert_eq!(call_i32(src, "get"), 1);
}

/// A literal mixing floats with ints has no single slot width: the collection
/// reads every slot at one width, so one of the two element types used to come
/// back as garbage. Codegen reports it as a compile error now, for each literal
/// kind, naming the function it appears in.
#[test]
fn mixed_width_literals_are_a_compile_error() {
    let cases: &[(&str, &str)] = &[
        (
            "def f() -> float:\n    xs = [1, 2.5]\n    return xs[1]\n",
            "list literal mixing float and int",
        ),
        (
            "def f() -> float:\n    t = (1, 2.5)\n    return t[1]\n",
            "tuple literal mixing float and int",
        ),
        (
            "def f() -> float:\n    d = {1: 1.5, 2: 2}\n    return d[1]\n",
            "dict literal's values mixing float and int",
        ),
        (
            "def f() -> int:\n    d = {1: 5, 2.5: 6}\n    return d[1]\n",
            "dict literal's keys mixing float and int",
        ),
        (
            "def f() -> int:\n    s = {1, 2.5}\n    return len(s)\n",
            "set literal mixing float and int",
        ),
    ];

    for (source, expected) in cases {
        let err = try_compile(source).expect_err("a mixed-width literal must be rejected");
        assert!(err.contains(expected), "expected {expected:?}, got: {err}");
        assert!(
            err.contains("in function 'f'"),
            "the error must name the function: {err}"
        );
    }

    // Uniform literals are unaffected, including bools next to ints (Python
    // treats bool as an int, and both live in the slot's low word).
    assert_eq!(
        call_f64(
            "def f() -> float:\n    xs = [1.5, 2.5]\n    return xs[1]\n",
            "f"
        ),
        2.5
    );
    assert_eq!(
        call_i32(
            "def f() -> int:\n    xs = [1, True]\n    return xs[0] + xs[1]\n",
            "f"
        ),
        2
    );
}

// ---------------------------------------------------------------------------
// Exception semantics.
//
// `raise` used to set a per-function flag that the end of the enclosing `try`
// body read to pick a handler, so the rest of the try body still ran, an
// unmatched handler swallowed the exception, and nothing crossed a call
// boundary. It transfers control now: the pending exception lives in a module
// global, a `raise` branches to the enclosing `try`'s dispatch or out of the
// function, and callers check after any call that can raise.
//
// Each test states what CPython does.
// ---------------------------------------------------------------------------

/// CPython never runs the statements after a `raise`.
#[test]
fn raise_skips_the_rest_of_the_try_body() {
    let src = "def f() -> int:\n\
               \x20   n = 0\n\
               \x20   try:\n\
               \x20       raise ValueError(\"x\")\n\
               \x20       n = n + 100\n\
               \x20   except ValueError:\n\
               \x20       n = n + 21\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 21);
}

/// CPython leaves the loop at the `raise`, so the counter stops at 1.
#[test]
fn raise_leaves_the_loop_it_is_raised_in() {
    let src = "def f() -> int:\n\
               \x20   n = 0\n\
               \x20   try:\n\
               \x20       i = 0\n\
               \x20       while i < 5:\n\
               \x20           n = n + 1\n\
               \x20           raise ValueError(\"x\")\n\
               \x20           i = i + 1\n\
               \x20   except ValueError:\n\
               \x20       pass\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 1);
}

/// CPython propagates an exception out of the callee into the caller's handler.
#[test]
fn raise_propagates_out_of_a_call() {
    let src = "def boom() -> int:\n\
               \x20   raise ValueError(\"x\")\n\
               \x20   return 99\n\
               \n\
               def f() -> int:\n\
               \x20   try:\n\
               \x20       n = boom()\n\
               \x20   except ValueError:\n\
               \x20       return 5\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 5);
}

/// A handler for a different type does not catch, so the exception leaves `f`.
/// Nothing in a compiled module can carry it out, so the honest outcome is a
/// trap; today the raise is swallowed and `f` returns 9.
#[test]
fn an_unmatched_handler_does_not_swallow_the_exception() {
    let src = "def f() -> int:\n\
               \x20   try:\n\
               \x20       raise ValueError(\"x\")\n\
               \x20   except TypeError:\n\
               \x20       return 3\n\
               \x20   return 9\n";
    let wasm = try_compile(src).expect("compiles");
    let (instance, mut store) = harness::instantiate_wasm(&wasm);
    let call = instance
        .get_typed_func::<(), i32>(&store, "f")
        .expect("exported f");
    assert!(
        call.call(&mut store, ()).is_err(),
        "an uncaught exception must not return a value"
    );
}

/// An uncaught `raise` with no `try` at all: CPython terminates the program.
#[test]
fn an_uncaught_raise_does_not_fall_through() {
    let src = "def f() -> int:\n\
               \x20   raise ValueError(\"x\")\n\
               \x20   return 7\n";
    let wasm = try_compile(src).expect("compiles");
    let (instance, mut store) = harness::instantiate_wasm(&wasm);
    let call = instance
        .get_typed_func::<(), i32>(&store, "f")
        .expect("exported f");
    assert!(
        call.call(&mut store, ()).is_err(),
        "an uncaught raise must not return 7"
    );
}

/// An exception crosses as many frames as it needs to, and the frames it
/// passes through run their `finally` on the way.
#[test]
fn exceptions_unwind_through_several_frames() {
    let src = "class C:\n\
               \x20   def __init__(self):\n\
               \x20       self.n = 0\n\
               \x20   def add(self, v: int):\n\
               \x20       self.n = self.n + v\n\
               \n\
               def inner(c: C) -> int:\n\
               \x20   try:\n\
               \x20       raise ValueError(\"x\")\n\
               \x20   finally:\n\
               \x20       c.add(7)\n\
               \x20   return 1\n\
               \n\
               def middle(c: C) -> int:\n\
               \x20   n = inner(c)\n\
               \x20   return n + 100\n\
               \n\
               def f() -> int:\n\
               \x20   c = C()\n\
               \x20   try:\n\
               \x20       n = middle(c)\n\
               \x20   except ValueError:\n\
               \x20       return c.n\n\
               \x20   return -1\n";
    assert_eq!(call_i32(src, "f"), 7);
}

/// The innermost `try` whose handler type matches catches; one that does not
/// match passes the exception outward instead of swallowing it.
#[test]
fn the_matching_handler_catches_not_the_nearest() {
    let inner_matches = "def f() -> int:\n\
                         \x20   try:\n\
                         \x20       try:\n\
                         \x20           raise ValueError(\"x\")\n\
                         \x20       except ValueError:\n\
                         \x20           return 1\n\
                         \x20   except ValueError:\n\
                         \x20       return 5\n\
                         \x20   return 9\n";
    assert_eq!(call_i32(inner_matches, "f"), 1);

    let inner_misses = "def f() -> int:\n\
                        \x20   try:\n\
                        \x20       try:\n\
                        \x20           raise ValueError(\"x\")\n\
                        \x20       except TypeError:\n\
                        \x20           return 1\n\
                        \x20   except ValueError:\n\
                        \x20       return 5\n\
                        \x20   return 9\n";
    assert_eq!(call_i32(inner_misses, "f"), 5);
}

/// An exception raised inside a handler belongs to the enclosing `try`, not to
/// the one whose handler is running.
#[test]
fn a_handler_that_raises_is_not_caught_by_its_own_try() {
    let src = "def g() -> int:\n\
               \x20   try:\n\
               \x20       raise ValueError(\"x\")\n\
               \x20   except ValueError:\n\
               \x20       raise TypeError(\"y\")\n\
               \x20   return 1\n\
               \n\
               def f() -> int:\n\
               \x20   try:\n\
               \x20       n = g()\n\
               \x20   except TypeError:\n\
               \x20       return 5\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 5);
}

/// Once a handler has caught, the pending exception is cleared: the function
/// carries on, and a later call is not mistaken for still unwinding.
#[test]
fn catching_clears_the_pending_exception() {
    let src = "def g(x: int) -> int:\n\
               \x20   if x == 0:\n\
               \x20       raise ValueError(\"x\")\n\
               \x20   return x\n\
               \n\
               def f() -> int:\n\
               \x20   n = 0\n\
               \x20   try:\n\
               \x20       n = g(0)\n\
               \x20   except ValueError:\n\
               \x20       n = 2\n\
               \x20   return n + g(10)\n";
    assert_eq!(call_i32(src, "f"), 12);
}

/// `__exit__` runs when an exception leaves a `with` block, including one
/// raised inside a function the body called, and nested managers each run.
#[test]
fn context_managers_exit_on_the_exception_path() {
    let tracker = "class Tracker:\n\
                   \x20   def __init__(self):\n\
                   \x20       self.exits = 0\n\
                   \x20   def __enter__(self) -> int:\n\
                   \x20       return 1\n\
                   \x20   def __exit__(self, a: int, b: int, c: int):\n\
                   \x20       self.exits = self.exits + 1\n\
                   \n";

    let direct = format!(
        "{tracker}def g(t: Tracker) -> int:\n\
         \x20   with t as v:\n\
         \x20       raise ValueError(\"x\")\n\
         \x20   return 0\n\
         \n\
         def f() -> int:\n\
         \x20   t = Tracker()\n\
         \x20   try:\n\
         \x20       n = g(t)\n\
         \x20   except ValueError:\n\
         \x20       return t.exits\n\
         \x20   return -1\n"
    );
    assert_eq!(call_i32(&direct, "f"), 1);

    let from_a_call = format!(
        "{tracker}def boom() -> int:\n\
         \x20   raise ValueError(\"x\")\n\
         \x20   return 0\n\
         \n\
         def g(t: Tracker) -> int:\n\
         \x20   with t as v:\n\
         \x20       n = boom()\n\
         \x20   return 0\n\
         \n\
         def f() -> int:\n\
         \x20   t = Tracker()\n\
         \x20   try:\n\
         \x20       n = g(t)\n\
         \x20   except ValueError:\n\
         \x20       return t.exits\n\
         \x20   return -1\n"
    );
    assert_eq!(call_i32(&from_a_call, "f"), 1);

    let nested = format!(
        "{tracker}def g(t: Tracker) -> int:\n\
         \x20   with t as a:\n\
         \x20       with t as b:\n\
         \x20           raise ValueError(\"x\")\n\
         \x20   return 0\n\
         \n\
         def f() -> int:\n\
         \x20   t = Tracker()\n\
         \x20   try:\n\
         \x20       n = g(t)\n\
         \x20   except ValueError:\n\
         \x20       return t.exits\n\
         \x20   return -1\n"
    );
    assert_eq!(call_i32(&nested, "f"), 2);
}
