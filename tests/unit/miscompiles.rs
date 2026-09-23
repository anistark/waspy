//! Regression tests for the silent miscompiles the whole-program work turned
//! up: constructs that compiled "successfully" and then answered something
//! CPython does not.
//!
//! Every expected value here is what CPython answers for the same source, so a
//! failure is a divergence from the reference implementation rather than a
//! change in what the compiler happens to do. The defects fall into three
//! groups:
//!
//! 1. **Method dispatch.** A method the compiler did not implement was dropped
//!    on the floor (lists) or emitted an invalid module blamed on code
//!    generation (strings), and extra arguments to a method it did implement
//!    were ignored. `list.pop(i)` read its index and never used it, and
//!    `list.index(v)` branched past the position it found.
//! 2. **Constant folding.** A method folded on a literal receiver took a
//!    different path from the same method on a variable, so a feature test
//!    with a literal passed while real code answered wrong: `split(sep)` folded
//!    into a string spelling a list, and fixed-point formatting broke ties on
//!    the wrong digit.
//! 3. **Error surfacing.** A parse or lowering failure on the file and
//!    multi-file paths, which is how every user compiles, was logged as a
//!    warning and the file skipped. One bad function took its whole module
//!    down, and a build with a module missing reported success.
//!
//! `memory_safety.rs` holds the earlier round of the same exercise (growth,
//! aliasing, checked indexing, f-string interpolation); this file starts where
//! that one left off.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{
    call_f64, call_i32, call_i32_1, call_str, call_str_1, try_compile, try_compile_multi,
};

// ---------------------------------------------------------------------------
// A method the compiler does not implement
// ---------------------------------------------------------------------------

/// An unknown method on each receiver kind names the method, the receiver, and
/// the enclosing function. The *list* receiver used to discard the receiver and
/// push a 0, so `xs.frobnicate()` compiled and did nothing, which is how
/// `sort` and `reverse` came to silently no-op for a whole release. The
/// *string* receiver dropped its `(offset, length)` and pushed nothing, so the
/// module failed WebAssembly validation and the error blamed code generation
/// rather than naming the method.
#[test]
fn unknown_methods_name_the_method_and_the_receiver() {
    let list = "from typing import List\n\
                \n\
                def f() -> int:\n\
                \x20   xs: List[int] = [1]\n\
                \x20   xs.frobnicate()\n\
                \x20   return xs[0]\n";
    let err = try_compile(list).expect_err("an unknown list method must be rejected");
    assert!(
        err.contains("frobnicate") && err.contains("list") && err.contains("in function 'f'"),
        "expected the method, receiver, and function, got: {err}"
    );

    let string = "def f() -> int:\n\
                  \x20   w = \"abc\"\n\
                  \x20   return len(w.swapcase())\n";
    let err = try_compile(string).expect_err("an unknown string method must be rejected");
    assert!(
        err.contains("swapcase") && err.contains("str"),
        "expected the method and receiver, got: {err}"
    );
    assert!(
        !err.contains("code generation bug"),
        "an unsupported method is not a codegen bug: {err}"
    );

    let dict = "from typing import Dict\n\
                \n\
                def f() -> int:\n\
                \x20   d: Dict[str, int] = {\"a\": 1}\n\
                \x20   d.popitem()\n\
                \x20   return d[\"a\"]\n";
    let err = try_compile(dict).expect_err("an unknown dict method must be rejected");
    assert!(
        err.contains("popitem") && err.contains("dict"),
        "expected the method and receiver, got: {err}"
    );
}

/// The hint on an unknown list method lists what *is* supported, and it stays
/// in step with the implementation: `sort` and `reverse` shipped without being
/// added to it, so the error told a reader to use methods it had just refused
/// to admit existed.
#[test]
fn the_unknown_list_method_hint_names_every_supported_method() {
    let src = "from typing import List\n\
               \n\
               def f() -> int:\n\
               \x20   xs: List[int] = [1]\n\
               \x20   xs.frobnicate()\n\
               \x20   return xs[0]\n";
    let err = try_compile(src).expect_err("an unknown list method must be rejected");
    for method in [
        "append", "clear", "count", "extend", "index", "insert", "pop", "remove", "reverse", "sort",
    ] {
        assert!(err.contains(method), "hint omits `{method}`: {err}");
    }
}

/// Every list-method arm read the arguments it needed and ignored the rest, so
/// `xs.append(3, 4)` compiled successfully and appended only the 3, and
/// `xs.clear(9)` dropped its argument. CPython raises `TypeError` for both.
#[test]
fn extra_arguments_to_a_list_method_are_rejected() {
    let too_many = "from typing import List\n\
                    \n\
                    def f() -> int:\n\
                    \x20   xs: List[int] = [2, 1]\n\
                    \x20   xs.append(3, 4)\n\
                    \x20   return len(xs)\n";
    let err = try_compile(too_many).expect_err("two arguments to append must be rejected");
    assert!(
        err.contains("append") && err.contains("got 2"),
        "expected an arity error naming append, got: {err}"
    );

    let on_clear = "from typing import List\n\
                    \n\
                    def f() -> int:\n\
                    \x20   xs: List[int] = [2, 1]\n\
                    \x20   xs.clear(9)\n\
                    \x20   return len(xs)\n";
    let err = try_compile(on_clear).expect_err("an argument to clear must be rejected");
    assert!(
        err.contains("clear") && err.contains("got 1"),
        "expected an arity error naming clear, got: {err}"
    );

    let too_few = "from typing import List\n\
                   \n\
                   def f() -> int:\n\
                   \x20   xs: List[int] = [2, 1]\n\
                   \x20   xs.insert(0)\n\
                   \x20   return len(xs)\n";
    let err = try_compile(too_few).expect_err("one argument to insert must be rejected");
    assert!(
        err.contains("insert") && err.contains("got 1"),
        "expected an arity error naming insert, got: {err}"
    );
}

/// The arity check must not reject the calls Python accepts, including the
/// optional argument on `pop` and the `reverse` keyword on `sort` that lowering
/// rewrites into a positional one.
#[test]
fn every_supported_list_method_arity_still_compiles() {
    let src = "from typing import List\n\
               \n\
               def f() -> int:\n\
               \x20   xs: List[int] = [3, 1]\n\
               \x20   xs.append(2)\n\
               \x20   xs.insert(0, 9)\n\
               \x20   xs.sort()\n\
               \x20   xs.sort(reverse=True)\n\
               \x20   ys: List[int] = [5, 4]\n\
               \x20   ys.clear()\n\
               \x20   xs.pop()\n\
               \x20   xs.pop(0)\n\
               \x20   xs.reverse()\n\
               \x20   xs.extend(ys)\n\
               \x20   return len(xs) * 100 + xs.count(2) * 10 + xs.index(2)\n";
    // CPython: [3, 1] -> append/insert/sort/sort(reverse) -> [9, 3, 2, 1],
    // pop() -> [9, 3, 2], pop(0) -> [3, 2], reverse -> [2, 3], extend([]) ->
    // [2, 3]. len 2, count(2) 1, index(2) 0.
    assert_eq!(call_i32(src, "f"), 210);
}

// ---------------------------------------------------------------------------
// list.pop(i) ignored its index
// ---------------------------------------------------------------------------

/// `pop(i)` evaluated its index, stored it, used it for the load, and then
/// decremented the length without moving anything: the element it answered was
/// right and the list it left behind was wrong. `[9, 3, 2].pop(0)` answered 9
/// and left `[9, 3]` where CPython leaves `[3, 2]`. This is the same defect
/// `insert` had, in the other direction.
#[test]
fn pop_removes_the_element_at_its_index() {
    let src = "from typing import List\n\
               \n\
               def rest(i: int) -> int:\n\
               \x20   xs: List[int] = [9, 3, 2]\n\
               \x20   xs.pop(i)\n\
               \x20   return xs[0] * 10 + xs[1]\n\
               \n\
               def popped(i: int) -> int:\n\
               \x20   xs: List[int] = [9, 3, 2]\n\
               \x20   return xs.pop(i)\n";
    assert_eq!(call_i32_1(src, "rest", 0), 32); // [3, 2]
    assert_eq!(call_i32_1(src, "rest", 1), 92); // [9, 2]
    assert_eq!(call_i32_1(src, "rest", 2), 93); // [9, 3]
    assert_eq!(call_i32_1(src, "popped", 0), 9);
    assert_eq!(call_i32_1(src, "popped", 1), 3);
    assert_eq!(call_i32_1(src, "popped", 2), 2);
}

/// A bare `pop()` still takes the last element, and a negative index counts
/// from the end the way it does everywhere else in the subset.
#[test]
fn pop_defaults_to_the_last_element_and_accepts_a_negative_index() {
    let src = "from typing import List\n\
               \n\
               def last() -> int:\n\
               \x20   xs: List[int] = [9, 3, 2]\n\
               \x20   xs.pop()\n\
               \x20   return xs[0] * 10 + xs[1]\n\
               \n\
               def negative() -> int:\n\
               \x20   xs: List[int] = [9, 3, 2]\n\
               \x20   xs.pop(-2)\n\
               \x20   return xs[0] * 10 + xs[1]\n";
    assert_eq!(call_i32(src, "last"), 93); // [9, 3]
    assert_eq!(call_i32(src, "negative"), 92); // [9, 2]
}

/// The shift moves whole slots, so an element wider than a word relocates
/// intact: a float keeps its f64 bits and a string keeps its blob offset.
#[test]
fn pop_relocates_wide_elements() {
    let floats = "from typing import List\n\
                  \n\
                  def popped() -> float:\n\
                  \x20   xs: List[float] = [1.5, 2.5, 3.5]\n\
                  \x20   return xs.pop(0)\n\
                  \n\
                  def rest() -> float:\n\
                  \x20   xs: List[float] = [1.5, 2.5, 3.5]\n\
                  \x20   xs.pop(0)\n\
                  \x20   return xs[0] + xs[1]\n";
    assert_eq!(call_f64(floats, "popped"), 1.5);
    assert_eq!(call_f64(floats, "rest"), 6.0);

    let strings = "from typing import List\n\
                   \n\
                   def popped() -> str:\n\
                   \x20   xs: List[str] = [\"aa\", \"bb\", \"cc\"]\n\
                   \x20   return xs.pop(1)\n\
                   \n\
                   def rest() -> str:\n\
                   \x20   xs: List[str] = [\"aa\", \"bb\", \"cc\"]\n\
                   \x20   xs.pop(1)\n\
                   \x20   return xs[0] + xs[1]\n";
    assert_eq!(call_str(strings, "popped"), "bb");
    assert_eq!(call_str(strings, "rest"), "aacc");
}

// ---------------------------------------------------------------------------
// list.index(v) branched past the position it found
// ---------------------------------------------------------------------------

/// The search loop pushed the matching position and then `br`-ed out of the
/// enclosing block. A branch to a label whose result type is empty discards
/// whatever sits above it, so the position was thrown away and execution fell
/// through to the `-1` after the loop: `[2, 3].index(2)` answered -1 for a
/// value the list held, and reported success.
#[test]
fn index_answers_the_position_it_found() {
    let src = "from typing import List\n\
               \n\
               def first() -> int:\n\
               \x20   xs: List[int] = [2, 3]\n\
               \x20   return xs.index(2)\n\
               \n\
               def last() -> int:\n\
               \x20   xs: List[int] = [5, 7, 9]\n\
               \x20   return xs.index(9)\n\
               \n\
               def repeated() -> int:\n\
               \x20   xs: List[int] = [4, 8, 4]\n\
               \x20   return xs.index(4)\n";
    assert_eq!(call_i32(src, "first"), 0);
    assert_eq!(call_i32(src, "last"), 2);
    assert_eq!(call_i32(src, "repeated"), 0); // the *first* occurrence

    let strings = "from typing import List\n\
                   \n\
                   def f() -> int:\n\
                   \x20   xs: List[str] = [\"aa\", \"bb\"]\n\
                   \x20   return xs.index(\"bb\")\n";
    assert_eq!(call_i32(strings, "f"), 1);

    let floats = "from typing import List\n\
                  \n\
                  def f() -> int:\n\
                  \x20   xs: List[float] = [1.5, 2.5]\n\
                  \x20   return xs.index(2.5)\n";
    assert_eq!(call_i32(floats, "f"), 1);
}

/// `count` shares the scan and was always right, which is why nothing caught
/// `index`: the two look interchangeable from the outside.
#[test]
fn count_still_counts() {
    let src = "from typing import List\n\
               \n\
               def present() -> int:\n\
               \x20   xs: List[int] = [2, 3, 2]\n\
               \x20   return xs.count(2)\n\
               \n\
               def absent() -> int:\n\
               \x20   xs: List[int] = [2, 3]\n\
               \x20   return xs.count(8)\n";
    assert_eq!(call_i32(src, "present"), 2);
    assert_eq!(call_i32(src, "absent"), 0);
}

// ---------------------------------------------------------------------------
// A method folded on a literal took a different path from one on a variable
// ---------------------------------------------------------------------------

/// `split(sep)` on a *literal* receiver folded, for a constant separator, into
/// a string spelling the result the way Python's `repr` does: `"a b a"
/// .split(" ")` became the 15-byte `['a', 'b', 'a']`. `len()` answered 15
/// instead of 3, iterating it walked characters, and using one as a dict key
/// trapped. There is no list constant to fold into, so the runtime path is the
/// only correct one, and `s.split(sep)` on a variable always took it.
#[test]
fn split_on_a_literal_answers_what_split_on_a_variable_does() {
    let src = "def literal_with_sep() -> int:\n\
               \x20   return len(\"a b a\".split(\" \"))\n\
               \n\
               def literal_without_sep() -> int:\n\
               \x20   return len(\"a b a\".split())\n\
               \n\
               def variable_with_sep() -> int:\n\
               \x20   s = \"a b a\"\n\
               \x20   return len(s.split(\" \"))\n\
               \n\
               def first_part() -> str:\n\
               \x20   return \"a b a\".split(\" \")[0]\n";
    assert_eq!(call_i32(src, "literal_with_sep"), 3);
    assert_eq!(call_i32(src, "literal_without_sep"), 3);
    assert_eq!(call_i32(src, "variable_with_sep"), 3);
    assert_eq!(call_str(src, "first_part"), "a");
}

/// The shape that found it: counting the pieces of a literal into a dict. The
/// folded string made every character its own key, and the lookup trapped.
#[test]
fn a_literal_split_accumulates_into_a_dict() {
    let src = "from typing import Dict\n\
               \n\
               def f() -> int:\n\
               \x20   d: Dict[str, int] = {}\n\
               \x20   for w in \"a b a\".split(\" \"):\n\
               \x20       d[w] = d.get(w, 0) + 1\n\
               \x20   return len(d) * 100 + d[\"a\"]\n";
    assert_eq!(call_i32(src, "f"), 202); // two distinct keys, "a" seen twice
}

/// Fixed-point formatting rounds to nearest with ties to even, the way
/// IEEE-754 and CPython both do. At `.0f` there is no fraction digit whose
/// parity could break a tie, so the digit that decides it is the integer
/// part's last one: splitting the value and rounding the fraction alone lost
/// that, and `F64Nearest(0.5)` is 0 whatever sits to its left, so `3.5`
/// printed as `3` where CPython answers `4`. `2.5` and `4.5` were right only
/// because their integer part was already even.
#[test]
fn fixed_point_breaks_ties_toward_even() {
    for (value, expected) in [
        (0.5, "0"),
        (1.5, "2"),
        (2.5, "2"),
        (3.5, "4"),
        (4.5, "4"),
        (5.5, "6"),
        (-2.5, "-2"),
        (-3.5, "-4"),
        (3.2, "3"),
        (3.7, "4"),
    ] {
        let got = format_float(value, 0);
        assert_eq!(got, expected, "f\"{{{value}:.0f}}\" answered {got}");
    }
}

/// Render one float through `f"{x:.Nf}"`. The value is baked into the source
/// because the exported function takes no arguments: the harness's argument
/// helpers pass `i32`s.
fn format_float(value: f64, precision: u32) -> String {
    let src = format!(
        "def f() -> str:\n\
         \x20   x = {value}\n\
         \x20   return f\"{{x:.{precision}f}}\"\n"
    );
    call_str(&src, "f")
}

/// A precision above zero keeps the tie-break on the last fraction digit,
/// which is where it belongs, and a fraction that rounds up carries into the
/// integer part. These cases were already right and must stay that way.
#[test]
fn fixed_point_keeps_its_other_answers() {
    for (value, precision, expected) in [
        (1.25, 1, "1.2"),
        (1.35, 1, "1.4"),
        (1.999, 2, "2.00"),
        (3.4347826086956523, 2, "3.43"),
        (2.675, 2, "2.67"),
        (0.125, 2, "0.12"),
        (0.135, 2, "0.14"),
    ] {
        let got = format_float(value, precision);
        assert_eq!(
            got, expected,
            "f\"{{{value}:.{precision}f}}\" answered {got}"
        );
    }
}

// ---------------------------------------------------------------------------
// A parse or lowering failure was a warning, and the file was skipped
// ---------------------------------------------------------------------------

/// The worst of the lot, because it crosses a module boundary and answers a
/// number. One unsupported construct anywhere in an imported module made the
/// whole module fail to lower, and the failure was logged as a warning and the
/// file skipped: the build succeeded with `helper.py` missing, and `total()`,
/// which calls into it, answered 0 instead of 14. A typo in any imported
/// module reaches this.
#[test]
fn one_bad_function_does_not_drop_its_whole_module() {
    let helper = "from typing import List\n\
                  \n\
                  def rate() -> int:\n\
                  \x20   return 7\n\
                  \n\
                  def broken() -> int:\n\
                  \x20   xs: List[int] = [1]\n\
                  \x20   return sorted(xs, bogus=1)[0]\n";
    let main = "def total() -> int:\n\
                \x20   return rate() * 2\n";
    let err = try_compile_multi(&[("helper.py", helper), ("main.py", main)])
        .expect_err("a module that cannot be lowered must fail the build");
    assert!(
        err.contains("helper.py"),
        "the error must name the file that failed, got: {err}"
    );
    assert!(
        err.contains("bogus"),
        "the error must carry the real cause, got: {err}"
    );
}

/// The same on the single-file path, where the symptom was a misleading
/// message rather than a wrong answer: the located error with its hint was
/// logged as a warning and the caller got "No valid functions found in any of
/// the provided files".
#[test]
fn a_file_that_cannot_be_lowered_reports_why() {
    let src = "X = 1\n\
               for i in range(3):\n\
               \x20   X = X + i\n\
               \n\
               def f() -> int:\n\
               \x20   return X\n";
    let err = try_compile_multi(&[("only.py", src)])
        .expect_err("a module-level loop must fail the build");
    assert!(
        err.contains("only.py") && err.contains("module level"),
        "expected the file and the real cause, got: {err}"
    );
    assert!(
        !err.contains("No valid functions found"),
        "the real cause must not be replaced by the empty-module message: {err}"
    );
}

/// A syntax error is reported the same way, with the file named.
#[test]
fn a_file_that_cannot_be_parsed_reports_why() {
    let err = try_compile_multi(&[("broken.py", "def two( ->\n")])
        .expect_err("a syntax error must fail the build");
    assert!(
        err.contains("broken.py") && err.contains("parse"),
        "expected the file and a parse error, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Known-open defects, asserted as they behave today
// ---------------------------------------------------------------------------
//
// These are not fixed. They are pinned here so the behavior cannot drift
// unnoticed and so the fix has a test waiting for it. A known defect with a
// test is a roadmap item; a known defect without one is a future surprise.

/// A lambda's parameters carry no type (#115), so code generation reads one as
/// a bare word. That was not the loud failure the roadmap recorded it as:
/// `(lambda kv: kv[1])((1, 5))` answered 0 where CPython answers 5, and
/// `(lambda w: len(w))("abcd")` answered 1684234849, which is the four bytes
/// read as an integer. Both compiled and reported success.
///
/// Typing a lambda's parameters from its call sites is a typing pass of its
/// own, so until then the three uses that need the type are refused rather
/// than miscompiled.
#[test]
fn a_lambda_parameter_that_needs_a_type_is_refused() {
    let indexed = "from typing import List, Tuple\n\
                   \n\
                   def f() -> int:\n\
                   \x20   pairs: List[Tuple[int, int]] = [(1, 5)]\n\
                   \x20   g = lambda kv: kv[1]\n\
                   \x20   return g(pairs[0])\n";
    let err = try_compile(indexed).expect_err("indexing a lambda parameter must be rejected");
    assert!(
        err.contains("lambda parameter carries no type") && err.contains("'kv'"),
        "expected the parameter and the reason, got: {err}"
    );

    let measured = "def f() -> int:\n\
                    \x20   g = lambda w: len(w)\n\
                    \x20   return g(\"abcd\")\n";
    let err = try_compile(measured).expect_err("len() of a lambda parameter must be rejected");
    assert!(
        err.contains("len()") && err.contains("'w'"),
        "expected len() and the parameter, got: {err}"
    );

    let method = "def f() -> str:\n\
                  \x20   g = lambda w: w.upper()\n\
                  \x20   return g(\"ab\")\n";
    let err = try_compile(method).expect_err("a method on a lambda parameter must be rejected");
    assert!(
        err.contains(".upper()") && err.contains("'w'"),
        "expected the method and the parameter, got: {err}"
    );
}

/// The refusal above must stay narrow. Arithmetic and comparison on an untyped
/// parameter are fine, because an untyped word already behaves as the `i32`
/// they assume, so the lambdas that do work keep working: a plain expression,
/// a capture of an enclosing variable, and a `sorted()` key.
#[test]
fn lambdas_that_do_not_need_their_parameter_type_still_work() {
    let src = "from typing import List\n\
               \n\
               def arithmetic() -> int:\n\
               \x20   f = lambda a, b: a * b + 1\n\
               \x20   return f(3, 4)\n\
               \n\
               def capture() -> int:\n\
               \x20   n = 10\n\
               \x20   f = lambda v: v + n\n\
               \x20   return f(5)\n\
               \n\
               def sort_key() -> int:\n\
               \x20   xs: List[int] = [3, 1, 2]\n\
               \x20   ys = sorted(xs, key=lambda v: 0 - v)\n\
               \x20   return ys[0] * 100 + ys[1] * 10 + ys[2]\n";
    assert_eq!(call_i32(src, "arithmetic"), 13);
    assert_eq!(call_i32(src, "capture"), 15);
    assert_eq!(call_i32(src, "sort_key"), 321); // descending
}

/// #116 records a bare `list` or `dict` annotation losing its element type, so
/// that string keys from such a collection stop deduplicating. Both spellings
/// answer Python's 2 here, so whatever reaches the reported symptom is
/// narrower than the annotation alone. Kept as a differential test on both
/// forms: if either starts answering 3, the deduplication has regressed.
#[test]
fn keys_from_a_bare_and_a_parameterised_annotation_both_deduplicate() {
    let bare = "def make() -> list:\n\
                \x20   return [\"a\", \"b\", \"a\"]\n\
                \n\
                def f() -> int:\n\
                \x20   d = {}\n\
                \x20   for w in make():\n\
                \x20       d[w] = 1\n\
                \x20   return len(d)\n";
    assert_eq!(call_i32(bare, "f"), 2);

    let parameterised = "from typing import Dict, List\n\
                         \n\
                         def make() -> List[str]:\n\
                         \x20   return [\"a\", \"b\", \"a\"]\n\
                         \n\
                         def f() -> int:\n\
                         \x20   d: Dict[str, int] = {}\n\
                         \x20   for w in make():\n\
                         \x20       d[w] = 1\n\
                         \x20   return len(d)\n";
    assert_eq!(call_i32(parameterised, "f"), 2);
}

/// A base method calling `self.kind()` reaches the subclass's override, the
/// plain template-method pattern. Dispatch used to be static everywhere, so
/// `Media.describe()` reached `Media.kind()` on a `Video` and answered
/// "media:b" while reporting success. This test pinned that wrong answer until
/// the vtable landed.
#[test]
fn an_inherited_method_reaches_the_subclass_override() {
    let src = "class Media:\n\
               \x20   def __init__(self, name: str):\n\
               \x20       self.name = name\n\
               \n\
               \x20   def kind(self) -> str:\n\
               \x20       return \"media\"\n\
               \n\
               \x20   def describe(self) -> str:\n\
               \x20       return self.kind() + \":\" + self.name\n\
               \n\
               \n\
               class Video(Media):\n\
               \x20   def kind(self) -> str:\n\
               \x20       return \"video\"\n\
               \n\
               \n\
               def inherited() -> str:\n\
               \x20   v: Video = Video(\"b\")\n\
               \x20   return v.describe()\n\
               \n\
               def direct() -> str:\n\
               \x20   v: Video = Video(\"b\")\n\
               \x20   return v.kind()\n";
    assert_eq!(call_str(src, "direct"), "video");
    assert_eq!(call_str(src, "inherited"), "video:b");
}

// ---------------------------------------------------------------------------
// The earlier round: methods that were stubs on a runtime receiver
// ---------------------------------------------------------------------------
//
// Every string method was a no-op, a constant, or garbage when its receiver
// was a variable rather than a literal, while reporting success. A *constant*
// receiver folded correctly during lowering, which is exactly why no feature
// test caught any of it: the tests all used literals. Each test below reads
// through a variable on purpose.

/// The case and trim transforms. `"ALPHA".lower()` on a variable answered
/// `"ALPHA"`, and `strip` with an argument popped the receiver's own
/// `(offset, length)` pair and emitted a module that did not validate.
#[test]
fn runtime_string_transforms_transform() {
    let src = "def cases() -> str:\n\
               \x20   w = \"aLPha bEta\"\n\
               \x20   return w.upper() + \"|\" + w.lower() + \"|\" + w.capitalize() + \"|\" + w.title()\n\
               \n\
               def trims() -> str:\n\
               \x20   w = \"  hi  \"\n\
               \x20   p = \"..xy..\"\n\
               \x20   return \"[\" + w.strip() + \"][\" + w.lstrip() + \"][\" + w.rstrip() + \"][\" + p.strip(\".\") + \"][\" + p.lstrip(\".\") + \"][\" + p.rstrip(\".\") + \"]\"\n";
    assert_eq!(
        call_str(src, "cases"),
        "ALPHA BETA|alpha beta|Alpha beta|Alpha Beta"
    );
    assert_eq!(call_str(src, "trims"), "[hi][hi  ][  hi][xy][xy..][..xy]");
}

/// The search and predicate methods, which answered -1, 0, and false whatever
/// the receiver held.
#[test]
fn runtime_string_searches_and_predicates_answer() {
    let src = "def searches() -> int:\n\
               \x20   w = \"banana\"\n\
               \x20   n = w.find(\"na\") * 1000\n\
               \x20   n = n + w.count(\"na\") * 100\n\
               \x20   if w.startswith(\"ban\"):\n\
               \x20       n = n + 10\n\
               \x20   if w.endswith(\"na\"):\n\
               \x20       n = n + 1\n\
               \x20   return n\n\
               \n\
               def predicates() -> int:\n\
               \x20   d = \"123\"\n\
               \x20   a = \"abc\"\n\
               \x20   s = \"  \"\n\
               \x20   n = 0\n\
               \x20   if d.isdigit():\n\
               \x20       n = n + 1\n\
               \x20   if a.isalpha():\n\
               \x20       n = n + 2\n\
               \x20   if s.isspace():\n\
               \x20       n = n + 4\n\
               \x20   if a.isalnum():\n\
               \x20       n = n + 8\n\
               \x20   if a.islower():\n\
               \x20       n = n + 16\n\
               \x20   if d.isupper():\n\
               \x20       n = n + 32\n\
               \x20   return n\n";
    // "banana".find("na") is 2 and .count("na") is 2; both prefix tests hold.
    assert_eq!(call_i32(src, "searches"), 2211);
    // Every predicate but `isupper` on digits, which Python answers False for.
    assert_eq!(call_i32(src, "predicates"), 31);
}

/// `split`/`join`/`replace` and the layout methods. `",".join(parts)` answered
/// the separator alone, `split` returned a region whose length word was
/// garbage, and `replace`/`ljust`/`rjust`/`center` returned the input.
#[test]
fn runtime_string_split_join_replace_and_layout() {
    let src = "def splits() -> str:\n\
               \x20   w = \"a,b,c\"\n\
               \x20   return \"-\".join(w.split(\",\"))\n\
               \n\
               def replaced() -> str:\n\
               \x20   w = \"aXbXc\"\n\
               \x20   return w.replace(\"X\", \"--\")\n\
               \n\
               def layout() -> str:\n\
               \x20   w = \"ab\"\n\
               \x20   return \"[\" + w.ljust(5) + \"][\" + w.rjust(5) + \"][\" + w.center(6) + \"]\"\n";
    assert_eq!(call_str(src, "splits"), "a-b-c");
    assert_eq!(call_str(src, "replaced"), "a--b--c");
    assert_eq!(call_str(src, "layout"), "[ab   ][   ab][  ab  ]");
}

/// `.format()` returned the empty string. It lowers to the same concatenation
/// chain an f-string does now, and takes automatic and positional fields.
#[test]
fn str_format_interpolates_runtime_values() {
    let src = "def automatic(n: int) -> str:\n\
               \x20   return \"{} of {}\".format(n, 10)\n\
               \n\
               def positional(n: int) -> str:\n\
               \x20   return \"{0}-{0}\".format(n)\n";
    assert_eq!(call_str_1(src, "automatic", 3), "3 of 10");
    assert_eq!(call_str_1(src, "positional", 7), "7-7");
}

// ---------------------------------------------------------------------------
// The earlier round: containers
// ---------------------------------------------------------------------------

/// `in` answered a constant `False` for anything a dict, tuple, or string
/// actually held: only lists and sets were considered searchable.
#[test]
fn membership_searches_every_container() {
    let src = "from typing import Dict, List, Tuple\n\
               \n\
               def f() -> int:\n\
               \x20   d: Dict[str, int] = {\"a\": 1}\n\
               \x20   t: Tuple[int, int] = (4, 5)\n\
               \x20   xs: List[int] = [7]\n\
               \x20   text = \"hello\"\n\
               \x20   n = 0\n\
               \x20   if \"a\" in d:\n\
               \x20       n = n + 1\n\
               \x20   if \"z\" in d:\n\
               \x20       n = n + 2\n\
               \x20   if 5 in t:\n\
               \x20       n = n + 4\n\
               \x20   if 7 in xs:\n\
               \x20       n = n + 8\n\
               \x20   if \"ell\" in text:\n\
               \x20       n = n + 16\n\
               \x20   if \"zz\" in text:\n\
               \x20       n = n + 32\n\
               \x20   return n\n";
    // Present: the dict key, the tuple member, the list member, the substring.
    assert_eq!(call_i32(src, "f"), 29);
}

/// An empty collection answered `True`, because the test read the region
/// pointer, which is never null, rather than the count.
#[test]
fn empty_containers_are_falsy() {
    let src = "from typing import Dict, List\n\
               \n\
               def f() -> int:\n\
               \x20   xs: List[int] = []\n\
               \x20   d: Dict[str, int] = {}\n\
               \x20   n = 0\n\
               \x20   if xs:\n\
               \x20       n = n + 1\n\
               \x20   if d:\n\
               \x20       n = n + 2\n\
               \x20   if not xs:\n\
               \x20       n = n + 4\n\
               \x20   return n\n\
               \n\
               def non_empty() -> int:\n\
               \x20   xs: List[int] = [0]\n\
               \x20   if xs:\n\
               \x20       return 1\n\
               \x20   return 0\n";
    assert_eq!(call_i32(src, "f"), 4);
    // A list holding one falsy element is itself truthy, as in Python.
    assert_eq!(call_i32(src, "non_empty"), 1);
}

/// `for k in d` was implemented for lists and strings only, so a dict fell
/// through to a branch walking it at the wrong width: a three-key dict counted
/// 131072 iterations, and a lookup inside the loop trapped.
#[test]
fn iterating_a_dict_walks_its_keys() {
    let src = "from typing import Dict\n\
               \n\
               def f() -> int:\n\
               \x20   d: Dict[str, int] = {\"a\": 1, \"b\": 2, \"c\": 3}\n\
               \x20   n = 0\n\
               \x20   for k in d:\n\
               \x20       n = n + d[k]\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "f"), 6);
}

/// Dicts had no method arm at all, so `get`, `keys`, `values`, and `items`
/// were all reported unsupported. `.items()` is a value now, not only
/// something a `for` loop can desugar.
#[test]
fn dict_methods_answer() {
    let src = "from typing import Dict\n\
               \n\
               def gets() -> int:\n\
               \x20   d: Dict[str, int] = {\"a\": 1}\n\
               \x20   return d.get(\"a\", 0) * 100 + d.get(\"zz\", 7)\n\
               \n\
               def views() -> int:\n\
               \x20   d: Dict[str, int] = {\"a\": 1, \"b\": 2}\n\
               \x20   return len(d.keys()) * 100 + len(d.values()) * 10 + len(d.items())\n";
    // A present key answers its value; a missing one answers the default.
    assert_eq!(call_i32(src, "gets"), 107);
    assert_eq!(call_i32(src, "views"), 222);
}

/// A key built at runtime never matched an equal key already in the dict,
/// because a string slot holds only its blob offset and the slots were
/// compared as words: that is identity, not equality.
#[test]
fn dict_keys_compare_by_content() {
    let src = "from typing import Dict\n\
               \n\
               def f() -> int:\n\
               \x20   d: Dict[str, int] = {}\n\
               \x20   s = \"a b a\"\n\
               \x20   for w in s.split():\n\
               \x20       d[w] = d.get(w, 0) + 1\n\
               \x20   return len(d) * 100 + d[\"a\"]\n";
    assert_eq!(call_i32(src, "f"), 202);
}

/// `sorted()` fell through to the generic builtin path and answered something
/// unrelated to sorting: `sorted([3, 1, 2])[0]` was 2. It sorts a copy now, so
/// the original is untouched, as in Python.
#[test]
fn sorted_sorts_a_copy() {
    let src = "from typing import List\n\
               \n\
               def f() -> int:\n\
               \x20   xs: List[int] = [3, 1, 2]\n\
               \x20   ys = sorted(xs)\n\
               \x20   return ys[0] * 100 + xs[0]\n";
    // ys[0] is 1 and the original still starts with 3.
    assert_eq!(call_i32(src, "f"), 103);
}

/// Indexing a tuple always reported the *first* member's type, so
/// `pairs[0][1]` on a `(int, str)` came back typed `int` and concatenating two
/// of them compiled as integer addition.
#[test]
fn the_index_decides_a_tuple_members_type() {
    let src = "from typing import List, Tuple\n\
               \n\
               def f() -> str:\n\
               \x20   pairs: List[Tuple[int, str]] = [(1, \"a\"), (2, \"b\")]\n\
               \x20   return pairs[0][1] + pairs[1][1]\n";
    assert_eq!(call_str(src, "f"), "ab");
}

// ---------------------------------------------------------------------------
// The earlier round: typing and calls
// ---------------------------------------------------------------------------

/// Python 3's `/` is true division and yields a float. This emitted an integer
/// divide, so `7 / 2` answered 3 and truncated silently wherever the result
/// was used. `//` still floors.
#[test]
fn division_follows_python_3() {
    let src = "def true_division() -> float:\n\
               \x20   return 7 / 2\n\
               \n\
               def floor_division() -> int:\n\
               \x20   return 7 // 2\n\
               \n\
               def converted_on_return() -> int:\n\
               \x20   return 7 / 2\n";
    assert_eq!(call_f64(src, "true_division"), 3.5);
    assert_eq!(call_i32(src, "floor_division"), 3);
    // A `return` converts to the declared type, which is what lets `-> int`
    // accept a true-division result.
    assert_eq!(call_i32(src, "converted_on_return"), 3);
}

/// A string had no companion length local when it arrived as a parameter or
/// was bound by a loop or comprehension target, so reading one pushed a single
/// word where the rest of codegen expects two. `sum(len(w) for w in words)`
/// answered garbage, and a method on a loop variable was rejected.
#[test]
fn a_string_keeps_its_length_through_every_binding() {
    let src = "from typing import List\n\
               \n\
               def over_a_generator() -> int:\n\
               \x20   words: List[str] = [\"a\", \"bb\", \"ccc\"]\n\
               \x20   return sum(len(w) for w in words)\n\
               \n\
               def loop_target() -> str:\n\
               \x20   xs: List[str] = [\"ab\", \"cd\"]\n\
               \x20   out = \"\"\n\
               \x20   for w in xs:\n\
               \x20       out = out + w.upper()\n\
               \x20   return out\n\
               \n\
               def comprehension_target() -> str:\n\
               \x20   xs: List[str] = [\"ab\", \"cd\"]\n\
               \x20   ups: List[str] = [w.upper() for w in xs]\n\
               \x20   return ups[0] + ups[1]\n";
    assert_eq!(call_i32(src, "over_a_generator"), 6);
    assert_eq!(call_str(src, "loop_target"), "ABCD");
    assert_eq!(call_str(src, "comprehension_target"), "ABCD");
}

/// A comprehension over a call did not consult the callee's declared return
/// type, so a string member rendered as its pointer inside an f-string.
#[test]
fn a_comprehension_over_a_call_binds_typed_targets() {
    let src = "from typing import List, Tuple\n\
               \n\
               def pairs() -> List[Tuple[int, str]]:\n\
               \x20   out: List[Tuple[int, str]] = []\n\
               \x20   out.append((1, \"a\"))\n\
               \x20   out.append((2, \"b\"))\n\
               \x20   return out\n\
               \n\
               def f() -> str:\n\
               \x20   lines: List[str] = [f\"{w}:{c}\" for c, w in pairs()]\n\
               \x20   return lines[0] + \",\" + lines[1]\n";
    assert_eq!(call_str(src, "f"), "a:1,b:2");
}

/// Only a call's positional arguments were read, so a keyword argument reached
/// codegen as if it had never been written: `xs.sort(reverse=True)` arrived as
/// a bare `sort()` and would have sorted the wrong way round without saying
/// so. `sort`'s `reverse` is honoured; every other keyword is refused.
#[test]
fn keyword_arguments_are_honoured_or_refused() {
    let honoured = "from typing import List\n\
                    \n\
                    def f() -> int:\n\
                    \x20   xs: List[int] = [1, 3, 2]\n\
                    \x20   xs.sort(reverse=True)\n\
                    \x20   return xs[0] * 100 + xs[1] * 10 + xs[2]\n";
    assert_eq!(call_i32(honoured, "f"), 321);

    let refused = "from typing import List\n\
                   \n\
                   def f() -> int:\n\
                   \x20   xs: List[int] = [1]\n\
                   \x20   return sorted(xs, bogus=1)[0]\n";
    let err = try_compile(refused).expect_err("an unknown keyword must be rejected");
    assert!(
        err.contains("bogus"),
        "expected the keyword to be named, got: {err}"
    );
}

/// Merged modules share one flat namespace, so two files defining the same
/// function name cannot both be kept. The second used to be dropped with a
/// warning while the build reported success, and every call to either one
/// reached the first: `alpha.rate()` and `beta.rate()` both answered alpha's.
#[test]
fn two_modules_with_the_same_function_name_are_refused() {
    let alpha = "def rate() -> int:\n\
                 \x20   return 1\n";
    let beta = "def rate() -> int:\n\
                \x20   return 2\n";
    let err = try_compile_multi(&[("alpha.py", alpha), ("beta.py", beta)])
        .expect_err("a duplicate function name must fail the build");
    assert!(
        err.contains("rate") && err.contains("beta.py"),
        "expected the name and the second file, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Decorators that were accepted and never applied (#120)
// ---------------------------------------------------------------------------

/// `@functools.singledispatch` dispatches on the first argument's static type:
/// the exact registration first, then `int` for a `bool` (Python's `bool` is
/// an `int`), then the nearest base class with an arm, then the base
/// function. The decorator used to be dropped, so every call reached the
/// base implementation and `kind("hi")` answered `kind(5)`'s value.
#[test]
fn singledispatch_picks_the_registered_implementation() {
    let src = "from functools import singledispatch\n\
               \n\
               class A:\n\
               \x20   def __init__(self):\n\
               \x20       self.v = 0\n\
               \n\
               class B(A):\n\
               \x20   def __init__(self):\n\
               \x20       self.v = 1\n\
               \n\
               @singledispatch\n\
               def kind(x: int) -> int:\n\
               \x20   return 1\n\
               \n\
               @kind.register\n\
               def _(x: str) -> int:\n\
               \x20   return 2 + len(x)\n\
               \n\
               @kind.register(float)\n\
               def _(x) -> int:\n\
               \x20   return 7\n\
               \n\
               @kind.register\n\
               def _(x: A) -> int:\n\
               \x20   return 9\n\
               \n\
               def by_type() -> int:\n\
               \x20   s = \"hi\"\n\
               \x20   return kind(s) * 1000 + kind(5) * 100 + kind(2.5) * 10 + kind(True)\n\
               \n\
               def by_base_class() -> int:\n\
               \x20   b = B()\n\
               \x20   return kind(b)\n";
    assert_eq!(call_i32(src, "by_type"), 4171);
    assert_eq!(call_i32(src, "by_base_class"), 9);
}

/// A dispatch that cannot be decided statically is refused rather than sent
/// to the base function, and a registration the compiler cannot type is
/// refused at lowering.
#[test]
fn singledispatch_refuses_what_it_cannot_decide() {
    let untyped_call = "from functools import singledispatch\n\
                        \n\
                        @singledispatch\n\
                        def kind(x: int) -> int:\n\
                        \x20   return 1\n\
                        \n\
                        @kind.register\n\
                        def _(x: str) -> int:\n\
                        \x20   return 2\n\
                        \n\
                        def g(v):\n\
                        \x20   return kind(v)\n";
    let err = try_compile(untyped_call).expect_err("an untyped dispatch argument must be refused");
    assert!(
        err.contains("cannot dispatch 'kind'"),
        "expected the dispatch to be named, got: {err}"
    );

    let untyped_arm = "from functools import singledispatch\n\
                       \n\
                       @singledispatch\n\
                       def kind(x: int) -> int:\n\
                       \x20   return 1\n\
                       \n\
                       @kind.register\n\
                       def _(x) -> int:\n\
                       \x20   return 2\n";
    let err = try_compile(untyped_arm).expect_err("an untyped register() arm must be refused");
    assert!(
        err.contains("kind.register") && err.contains("annotate"),
        "expected the registration and a hint, got: {err}"
    );
}

/// `@functools.total_ordering` derives the three ordering methods the class
/// leaves out from the one it defines, spelled the way CPython spells them.
/// It used to be ignored, and `a > b` on the instances compared their heap
/// pointers, so the answer was allocation order.
#[test]
fn total_ordering_derives_the_missing_comparisons() {
    let from_lt = "from functools import total_ordering\n\
                   \n\
                   @total_ordering\n\
                   class N:\n\
                   \x20   def __init__(self, v: int):\n\
                   \x20       self.v = v\n\
                   \x20   def __eq__(self, other) -> bool:\n\
                   \x20       return self.v == other.v\n\
                   \x20   def __lt__(self, other) -> bool:\n\
                   \x20       return self.v < other.v\n\
                   \n\
                   def f() -> int:\n\
                   \x20   a = N(5)\n\
                   \x20   b = N(3)\n\
                   \x20   c = N(5)\n\
                   \x20   r = 0\n\
                   \x20   if a > b:\n\
                   \x20       r += 1\n\
                   \x20   if a >= c:\n\
                   \x20       r += 10\n\
                   \x20   if b <= a:\n\
                   \x20       r += 100\n\
                   \x20   if a <= b:\n\
                   \x20       r += 1000\n\
                   \x20   if c > a:\n\
                   \x20       r += 10000\n\
                   \x20   return r\n";
    assert_eq!(call_i32(from_lt, "f"), 111);

    // Rooted at `__ge__`, with the default identity `__eq__`.
    let from_ge = "from functools import total_ordering\n\
                   \n\
                   @total_ordering\n\
                   class N:\n\
                   \x20   def __init__(self, v: int):\n\
                   \x20       self.v = v\n\
                   \x20   def __ge__(self, other) -> bool:\n\
                   \x20       return self.v >= other.v\n\
                   \n\
                   def f() -> int:\n\
                   \x20   a = N(5)\n\
                   \x20   b = N(3)\n\
                   \x20   r = 0\n\
                   \x20   if a > b:\n\
                   \x20       r += 1\n\
                   \x20   if b < a:\n\
                   \x20       r += 10\n\
                   \x20   if a <= b:\n\
                   \x20       r += 100\n\
                   \x20   if a <= a:\n\
                   \x20       r += 1000\n\
                   \x20   return r\n";
    assert_eq!(call_i32(from_ge, "f"), 1011);

    let no_root = "from functools import total_ordering\n\
                   \n\
                   @total_ordering\n\
                   class N:\n\
                   \x20   def __init__(self, v: int):\n\
                   \x20       self.v = v\n";
    let err = try_compile(no_root).expect_err("total_ordering needs a root comparison");
    assert!(
        err.contains("__lt__, __le__, __gt__, __ge__"),
        "expected the required methods to be listed, got: {err}"
    );
}

/// Ordering between instances dispatches to the rich comparison method, the
/// left operand's own first and then the right operand's reflection, as
/// CPython does. A user-written `__lt__` used to be ignored entirely (the
/// instance pointers were compared), and a class with no ordering method at
/// all, which CPython refuses with `TypeError`, compared the same way.
#[test]
fn instance_ordering_dispatches_to_the_dunder_or_is_refused() {
    let src = "class N:\n\
               \x20   def __init__(self, v: int):\n\
               \x20       self.v = v\n\
               \x20   def __lt__(self, other) -> bool:\n\
               \x20       return self.v < other.v\n\
               \n\
               class M:\n\
               \x20   def __init__(self, v: int):\n\
               \x20       self.v = v\n\
               \x20   def __gt__(self, other: \"N\") -> bool:\n\
               \x20       return self.v > other.v\n\
               \n\
               def f() -> int:\n\
               \x20   a = N(5)\n\
               \x20   b = N(3)\n\
               \x20   m = M(9)\n\
               \x20   r = 0\n\
               \x20   if b < a:\n\
               \x20       r += 1\n\
               \x20   if a < m:\n\
               \x20       r += 10\n\
               \x20   if a < b:\n\
               \x20       r += 100\n\
               \x20   return r\n";
    assert_eq!(call_i32(src, "f"), 11);

    let none = "class N:\n\
                \x20   def __init__(self, v: int):\n\
                \x20       self.v = v\n\
                \n\
                def f() -> int:\n\
                \x20   if N(3) < N(5):\n\
                \x20       return 1\n\
                \x20   return 0\n";
    let err = try_compile(none).expect_err("ordering without a dunder must be refused");
    assert!(
        err.contains("'<' not supported between instances of 'N' and 'N'"),
        "expected CPython's message, got: {err}"
    );

    let scalar = "class N:\n\
                  \x20   def __init__(self, v: int):\n\
                  \x20       self.v = v\n\
                  \n\
                  def f() -> int:\n\
                  \x20   if N(3) < 5:\n\
                  \x20       return 1\n\
                  \x20   return 0\n";
    let err = try_compile(scalar).expect_err("ordering against a scalar must be refused");
    assert!(
        err.contains("'N' and 'int'"),
        "expected both operand types, got: {err}"
    );
}

/// The caching decorators change nothing a compiled module can observe, so
/// they are accepted; every other decorator the compiler does not implement
/// is refused, on functions, methods, and classes alike. Each of these used to
/// be dropped silently: `@cached_property` read as a plain method whose
/// attribute access answered 0.
#[test]
fn unimplemented_decorators_are_refused_and_caching_ones_accepted() {
    let cached = "from functools import lru_cache, cache\n\
                  \n\
                  @lru_cache(maxsize=None)\n\
                  def fib(n: int) -> int:\n\
                  \x20   if n < 2:\n\
                  \x20       return n\n\
                  \x20   return fib(n - 1) + fib(n - 2)\n\
                  \n\
                  @cache\n\
                  def twice(n: int) -> int:\n\
                  \x20   return n * 2\n\
                  \n\
                  def f() -> int:\n\
                  \x20   return twice(fib(10))\n";
    assert_eq!(call_i32(cached, "f"), 110);

    let cases: [(&str, &str); 3] = [
        (
            "def deco(fn):\n\
             \x20   return fn\n\
             \n\
             @deco\n\
             def g(x: int) -> int:\n\
             \x20   return x + 1\n",
            "'@deco' on function 'g'",
        ),
        (
            "from functools import cached_property\n\
             \n\
             class N:\n\
             \x20   def __init__(self, v: int):\n\
             \x20       self.v = v\n\
             \x20   @cached_property\n\
             \x20   def d(self) -> int:\n\
             \x20       return self.v * 2\n",
            "'@cached_property' on method 'N.d'",
        ),
        (
            "def deco(cls):\n\
             \x20   return cls\n\
             \n\
             @deco\n\
             class N:\n\
             \x20   def __init__(self, v: int):\n\
             \x20       self.v = v\n",
            "'@deco' on class 'N'",
        ),
    ];
    for (src, expected) in cases {
        let err = try_compile(src).expect_err("an unimplemented decorator must be refused");
        assert!(
            err.contains(expected),
            "expected {expected:?} in the error, got: {err}"
        );
    }
}

/// A call to a name that is neither a compiled function nor a builtin the
/// compiler implements pushed a 0 and reported success, so `reduce(add, xs)`,
/// `partial(add, 10)`, `divmod(7, 2)`, and a misspelled function name all
/// answered 0. It names the callee now. (`abs()` was on this list until it was
/// implemented.)
#[test]
fn a_call_to_an_unknown_function_is_refused() {
    let cases: [&str; 3] = [
        "from functools import reduce\n\
         \n\
         def add(a: int, b: int) -> int:\n\
         \x20   return a + b\n\
         \n\
         def f() -> int:\n\
         \x20   return reduce(add, [1, 2, 3])\n",
        "def f() -> int:\n\
         \x20   return divmod(7, 2)[0]\n",
        "def total(a: int) -> int:\n\
         \x20   return a\n\
         \n\
         def f() -> int:\n\
         \x20   return totl(3)\n",
    ];
    for (src, name) in cases.iter().zip(["reduce", "divmod", "totl"]) {
        let err = try_compile(src).expect_err("an unknown callee must be refused");
        assert!(
            err.contains(&format!("call to '{name}'")),
            "expected '{name}' to be named, got: {err}"
        );
    }
}

/// An attribute read through a value whose type is not known (an unannotated
/// parameter, most often) answered 0 and reported success, which is what
/// left `def __lt__(self, other): return self.v < other.v` comparing against
/// nothing. The rich comparison methods now type an unannotated `other` as
/// the class; everywhere else the read is refused with a hint, and a field
/// the class does not have is refused too.
#[test]
fn attribute_reads_through_untyped_values_are_refused() {
    let dunder = "class N:\n\
                  \x20   def __init__(self, v: int):\n\
                  \x20       self.v = v\n\
                  \x20   def __eq__(self, other) -> bool:\n\
                  \x20       return self.v == other.v\n\
                  \n\
                  def f() -> int:\n\
                  \x20   if N(4) == N(4):\n\
                  \x20       return 1\n\
                  \x20   return 0\n";
    assert_eq!(call_i32(dunder, "f"), 1);

    let untyped = "class N:\n\
                   \x20   def __init__(self, v: int):\n\
                   \x20       self.v = v\n\
                   \n\
                   def val(other) -> int:\n\
                   \x20   return other.v\n\
                   \n\
                   def f() -> int:\n\
                   \x20   return val(N(5))\n";
    let err = try_compile(untyped).expect_err("a read through an untyped value must be refused");
    assert!(
        err.contains("attribute 'v'") && err.contains("annotate"),
        "expected the attribute and a hint, got: {err}"
    );

    let missing = "class N:\n\
                   \x20   def __init__(self, v: int):\n\
                   \x20       self.v = v\n\
                   \n\
                   def f() -> int:\n\
                   \x20   return N(1).w\n";
    let err = try_compile(missing).expect_err("a missing field must be refused");
    assert!(
        err.contains("'N' has no attribute 'w'"),
        "expected the class and the attribute, got: {err}"
    );
}

/// `list()`, `dict()`, `set()`, and `tuple()` with no argument are the empty
/// literals. They used to fall through to the unknown-call path and answer a
/// null pointer typed as nothing in particular.
#[test]
fn empty_constructors_are_empty_literals() {
    let src = "def f() -> int:\n\
               \x20   xs = list()\n\
               \x20   d = dict()\n\
               \x20   s = set()\n\
               \x20   xs.append(4)\n\
               \x20   xs.append(5)\n\
               \x20   s.add(2)\n\
               \x20   d[1] = 5\n\
               \x20   return len(xs) * 100 + len(d) * 10 + len(s)\n";
    assert_eq!(call_i32(src, "f"), 211);
}

// ---------------------------------------------------------------------------
// List writes stored the value at its own width, not the element's (#123)
// ---------------------------------------------------------------------------

/// An int written into a collection whose elements are read as floats is
/// widened to the slot's width. Every write path stored whatever `emit_expr`
/// produced, so a 4-byte integer landed in an 8-byte float slot and the
/// element read back as 1.5e-323 (`append`, `extend`) or 1.0000000000000007
/// (`insert`, item assignment, which overwrote the slot's low half).
#[test]
fn writes_into_a_float_collection_widen_the_value() {
    let src = "from typing import Dict, List, Set\n\
               \n\
               def by_append() -> float:\n\
               \x20   xs: List[float] = [1.0, 2.0]\n\
               \x20   xs.append(3)\n\
               \x20   return xs[2]\n\
               \n\
               def by_insert() -> float:\n\
               \x20   xs: List[float] = [10.0]\n\
               \x20   xs.insert(0, 4)\n\
               \x20   return xs[0]\n\
               \n\
               def by_subscript() -> float:\n\
               \x20   xs: List[float] = [1.0, 2.0]\n\
               \x20   xs[0] = 3\n\
               \x20   return xs[0]\n\
               \n\
               def by_dict_value() -> float:\n\
               \x20   d: Dict[int, float] = {1: 1.0}\n\
               \x20   d[2] = 3\n\
               \x20   return d[2]\n\
               \n\
               def by_set_add() -> int:\n\
               \x20   s: Set[float] = {1.0}\n\
               \x20   s.add(3)\n\
               \x20   if 3.0 in s:\n\
               \x20       return 1\n\
               \x20   return 0\n";
    assert_eq!(call_f64(src, "by_append"), 3.0);
    assert_eq!(call_f64(src, "by_insert"), 4.0);
    assert_eq!(call_f64(src, "by_subscript"), 3.0);
    assert_eq!(call_f64(src, "by_dict_value"), 3.0);
    assert_eq!(call_i32(src, "by_set_add"), 1);
}

/// The widening reaches a value that arrives through a variable or a call, not
/// only a literal. The type hint alone does not: it is consumed by constants
/// and arithmetic, so an int that passed through a local came out an int
/// whatever the destination asked for. An unannotated local now takes `int`
/// from the value assigned to it (both live in the same i32 slot, so the local
/// layout is unchanged), and the conversion happens at the write.
#[test]
fn widening_reaches_values_that_arrive_through_a_variable() {
    let src = "from typing import List\n\
               \n\
               def plain(n: int) -> int:\n\
               \x20   return n\n\
               \n\
               def through_a_local() -> float:\n\
               \x20   n = 3\n\
               \x20   xs: List[float] = [1.0]\n\
               \x20   xs.append(n)\n\
               \x20   return xs[1]\n\
               \n\
               def through_a_call() -> float:\n\
               \x20   xs: List[float] = [1.0]\n\
               \x20   xs.append(plain(4))\n\
               \x20   return xs[1]\n\
               \n\
               def through_a_parameter(n: int) -> float:\n\
               \x20   xs: List[float] = [1.0]\n\
               \x20   xs[0] = n\n\
               \x20   return xs[0]\n";
    assert_eq!(call_f64(src, "through_a_local"), 3.0);
    assert_eq!(call_f64(src, "through_a_call"), 4.0);
    assert_eq!(call_i32_1(src, "plain", 5), 5);
}

/// A write the compiler cannot make good on is refused rather than stored at
/// the wrong width: a float into an int collection (CPython keeps the 2.5; an
/// int slot cannot, and truncating would lose it), a float into a collection
/// with no element type to read it back at, a value of unknown type into a
/// float collection, and an `extend` between lists read at different widths.
#[test]
fn writes_that_cannot_be_made_good_on_are_refused() {
    let cases: [(&str, &str); 4] = [
        (
            "from typing import List\n\
             \n\
             def f() -> int:\n\
             \x20   xs: List[int] = [1]\n\
             \x20   xs.append(2.5)\n\
             \x20   return xs[1]\n",
            "truncate",
        ),
        (
            "def f() -> float:\n\
             \x20   xs = []\n\
             \x20   xs.append(2.5)\n\
             \x20   return xs[0]\n",
            "no known element type",
        ),
        (
            "from typing import List\n\
             \n\
             def g():\n\
             \x20   return 3\n\
             \n\
             def f() -> float:\n\
             \x20   xs: List[float] = [1.0]\n\
             \x20   xs.append(g())\n\
             \x20   return xs[1]\n",
            "unknown type into a collection of floats",
        ),
        (
            "from typing import List\n\
             \n\
             def f() -> float:\n\
             \x20   xs: List[float] = [1.0]\n\
             \x20   ys: List[int] = [3]\n\
             \x20   xs.extend(ys)\n\
             \x20   return xs[1]\n",
            "different widths",
        ),
    ];
    for (src, expected) in cases {
        let err = try_compile(src).expect_err("a write at the wrong width must be refused");
        assert!(
            err.contains(expected),
            "expected {expected:?} in the error, got: {err}"
        );
    }
}

/// The paths that were already right stay right: same-width writes of every
/// element type, and an `extend` between two lists read at the same width.
#[test]
fn same_width_writes_are_unaffected() {
    let src = "from typing import List\n\
               \n\
               def ints() -> int:\n\
               \x20   xs: List[int] = []\n\
               \x20   xs.append(7)\n\
               \x20   xs.insert(0, 4)\n\
               \x20   xs[1] = 9\n\
               \x20   return xs[0] * 10 + xs[1]\n\
               \n\
               def floats() -> float:\n\
               \x20   xs: List[float] = []\n\
               \x20   xs.append(1.5)\n\
               \x20   xs.insert(0, 2.5)\n\
               \x20   xs[1] = 3.5\n\
               \x20   return xs[0] + xs[1]\n\
               \n\
               def strings() -> int:\n\
               \x20   xs: List[str] = []\n\
               \x20   xs.append(\"ab\")\n\
               \x20   return len(xs[0])\n\
               \n\
               def extended() -> float:\n\
               \x20   xs: List[float] = [1.0]\n\
               \x20   ys: List[float] = [3.0]\n\
               \x20   xs.extend(ys)\n\
               \x20   return xs[1]\n";
    assert_eq!(call_i32(src, "ints"), 49);
    assert_eq!(call_f64(src, "floats"), 6.0);
    assert_eq!(call_i32(src, "strings"), 2);
    assert_eq!(call_f64(src, "extended"), 3.0);
}

// ---------------------------------------------------------------------------
// Overrides reached through a base method (virtual dispatch)
// ---------------------------------------------------------------------------

/// The override is found through every route an instance can be reached by: a
/// `self` call inside a base method, a variable declared as the base, a
/// parameter declared as the base, and an element of a list of the base. All
/// four used to answer with the base's implementation.
#[test]
fn an_override_is_reached_through_every_route() {
    let src = "from typing import List\n\
               \n\
               class A:\n\
               \x20   def k(self) -> int:\n\
               \x20       return 1\n\
               \x20   def via_self(self) -> int:\n\
               \x20       return self.k()\n\
               \n\
               class B(A):\n\
               \x20   def k(self) -> int:\n\
               \x20       return 2\n\
               \n\
               class C(B):\n\
               \x20   pass\n\
               \n\
               def through_a_parameter(a: A) -> int:\n\
               \x20   return a.k()\n\
               \n\
               def by_self() -> int:\n\
               \x20   return A().via_self() * 100 + B().via_self() * 10 + C().via_self()\n\
               \n\
               def by_parameter() -> int:\n\
               \x20   a = through_a_parameter(A())\n\
               \x20   b = through_a_parameter(B())\n\
               \x20   c = through_a_parameter(C())\n\
               \x20   return a * 100 + b * 10 + c\n\
               \n\
               def by_variable() -> int:\n\
               \x20   v: A = B()\n\
               \x20   return v.k()\n\
               \n\
               def by_list_element() -> int:\n\
               \x20   xs: List[A] = [A(), B(), C()]\n\
               \x20   total = 0\n\
               \x20   for x in xs:\n\
               \x20       total = total * 10 + x.k()\n\
               \x20   return total\n";
    // C inherits B's override, so every route answers 1, 2, 2 in turn.
    assert_eq!(call_i32(src, "by_self"), 122);
    assert_eq!(call_i32(src, "by_parameter"), 122);
    assert_eq!(call_i32(src, "by_variable"), 2);
    assert_eq!(call_i32(src, "by_list_element"), 122);
}

/// A `@property` getter and `__eq__` dispatch on the runtime class too: both
/// are reached from a base method (or the `==` operator) holding the base's
/// static type, so both had the same defect.
#[test]
fn overridden_properties_and_eq_dispatch_on_the_runtime_class() {
    let property = "class A:\n\
                    \x20   def __init__(self, v: int):\n\
                    \x20       self.v = v\n\
                    \x20   @property\n\
                    \x20   def scale(self) -> int:\n\
                    \x20       return 10\n\
                    \x20   def scaled(self) -> int:\n\
                    \x20       return self.v * self.scale\n\
                    \n\
                    class B(A):\n\
                    \x20   @property\n\
                    \x20   def scale(self) -> int:\n\
                    \x20       return 20\n\
                    \n\
                    def f() -> int:\n\
                    \x20   return A(2).scaled() * 1000 + B(2).scaled()\n";
    assert_eq!(call_i32(property, "f"), 20040);

    let equality = "class A:\n\
                    \x20   def __init__(self, v: int):\n\
                    \x20       self.v = v\n\
                    \x20   def __eq__(self, other) -> bool:\n\
                    \x20       return False\n\
                    \n\
                    class B(A):\n\
                    \x20   def __eq__(self, other) -> bool:\n\
                    \x20       return self.v == other.v\n\
                    \n\
                    def compare(x: A, y: A) -> int:\n\
                    \x20   if x == y:\n\
                    \x20       return 1\n\
                    \x20   return 0\n\
                    \n\
                    def f() -> int:\n\
                    \x20   return compare(B(3), B(3)) * 10 + compare(A(3), A(3))\n";
    assert_eq!(call_i32(equality, "f"), 10);
}

/// What virtual dispatch must not change: `super().method()` is non-virtual in
/// Python, so an override extending its base does not recurse; a method
/// nothing overrides keeps its direct call; and an exception raised inside an
/// override still propagates, which the indirect call has to account for
/// itself.
#[test]
fn virtual_dispatch_leaves_the_other_paths_alone() {
    let supered = "class A:\n\
                   \x20   def m(self) -> str:\n\
                   \x20       return \"a\"\n\
                   \n\
                   class B(A):\n\
                   \x20   def m(self) -> str:\n\
                   \x20       return super().m() + \"b\"\n\
                   \n\
                   class C(B):\n\
                   \x20   def m(self) -> str:\n\
                   \x20       return super().m() + \"c\"\n\
                   \n\
                   def f() -> str:\n\
                   \x20   return C().m()\n";
    assert_eq!(call_str(supered, "f"), "abc");

    let not_overridden = "class A:\n\
                          \x20   def m(self) -> int:\n\
                          \x20       return 7\n\
                          \n\
                          class B(A):\n\
                          \x20   def other(self) -> int:\n\
                          \x20       return 1\n\
                          \n\
                          def f() -> int:\n\
                          \x20   return B().m() + A().m()\n";
    assert_eq!(call_i32(not_overridden, "f"), 14);

    let raising = "class A:\n\
                   \x20   def m(self) -> int:\n\
                   \x20       return 1\n\
                   \n\
                   class B(A):\n\
                   \x20   def m(self) -> int:\n\
                   \x20       raise ValueError\n\
                   \n\
                   def call(a: A) -> int:\n\
                   \x20   return a.m()\n\
                   \n\
                   def caught() -> int:\n\
                   \x20   try:\n\
                   \x20       return call(B())\n\
                   \x20   except ValueError:\n\
                   \x20       return 42\n\
                   \n\
                   def not_raised() -> int:\n\
                   \x20   try:\n\
                   \x20       return call(A())\n\
                   \x20   except ValueError:\n\
                   \x20       return 42\n";
    assert_eq!(call_i32(raising, "caught"), 42);
    assert_eq!(call_i32(raising, "not_raised"), 1);
}

/// Every implementation of an overridden name is reached through one indirect
/// call, which names a single WebAssembly signature, so an override that
/// changes the parameters or the return type is refused. Python allows it;
/// dispatching it here would trap at the call with nothing useful said.
#[test]
fn an_override_with_a_different_signature_is_refused() {
    let arity = "class A:\n\
                 \x20   def m(self) -> int:\n\
                 \x20       return 1\n\
                 \n\
                 class B(A):\n\
                 \x20   def m(self, k: int) -> int:\n\
                 \x20       return k\n\
                 \n\
                 def f() -> int:\n\
                 \x20   return A().m()\n";
    let err = try_compile(arity).expect_err("a different arity must be refused");
    assert!(
        err.contains("'B.m' overrides 'A.m'"),
        "expected both methods named, got: {err}"
    );

    let returns = "class A:\n\
                   \x20   def m(self) -> int:\n\
                   \x20       return 1\n\
                   \n\
                   class B(A):\n\
                   \x20   def m(self) -> float:\n\
                   \x20       return 2.0\n\
                   \n\
                   def f() -> int:\n\
                   \x20   return A().m()\n";
    let err = try_compile(returns).expect_err("a different return type must be refused");
    assert!(
        err.contains("same parameter and return types"),
        "expected the hint, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Program four: a single character of a string, item assignment, and int()
// ---------------------------------------------------------------------------

/// `s[i]` is a real one-character string wherever it goes. It used to be a
/// pointer into the source string, which is right while the value stays on the
/// stack; but a string is narrowed to its offset to fit a word (an argument, a
/// field, a list element, a return value) and its length is recovered from the
/// four bytes before that offset, which for an interior pointer are the
/// preceding characters. Every one of these answered something else, silently.
#[test]
fn a_string_index_is_a_real_string_wherever_it_goes() {
    let src = "from typing import List\n\
               \n\
               class Holder:\n\
               \x20   def __init__(self, t: str):\n\
               \x20       self.t = t\n\
               \n\
               def length(ch: str) -> int:\n\
               \x20   return len(ch)\n\
               \n\
               def is_digit(ch: str) -> int:\n\
               \x20   if ch.isdigit():\n\
               \x20       return 1\n\
               \x20   return 0\n\
               \n\
               def first(s: str) -> str:\n\
               \x20   return s[0]\n\
               \n\
               def as_argument() -> int:\n\
               \x20   s = \"a1\"\n\
               \x20   return length(s[1]) * 10 + is_digit(s[1])\n\
               \n\
               def in_a_field() -> int:\n\
               \x20   s = \"xy\"\n\
               \x20   return len(Holder(s[1]).t)\n\
               \n\
               def in_a_list() -> int:\n\
               \x20   s = \"ab\"\n\
               \x20   xs: List[str] = [s[0], s[1]]\n\
               \x20   return len(xs[0]) + len(xs[1])\n\
               \n\
               def returned() -> int:\n\
               \x20   return len(first(\"hello\"))\n";
    // Before: 1627389952-ish garbage, 2013265920, -905969662, and 5.
    assert_eq!(call_i32(src, "as_argument"), 11);
    assert_eq!(call_i32(src, "in_a_field"), 1);
    assert_eq!(call_i32(src, "in_a_list"), 2);
    assert_eq!(call_i32(src, "returned"), 1);
}

/// `for ch in s` binds each character as a one-character string. It shared the
/// collection loop, which reads a length and a data pointer from a collection
/// header that a string does not have, so it walked unrelated memory.
#[test]
fn iterating_a_string_binds_each_character() {
    let src = "from typing import List\n\
               \n\
               def is_digit(ch: str) -> int:\n\
               \x20   if ch.isdigit():\n\
               \x20       return 1\n\
               \x20   return 0\n\
               \n\
               def digits() -> int:\n\
               \x20   n = 0\n\
               \x20   for ch in \"a1b22\":\n\
               \x20       n += is_digit(ch)\n\
               \x20   return n\n\
               \n\
               def collected() -> int:\n\
               \x20   xs: List[str] = []\n\
               \x20   for ch in \"abc\":\n\
               \x20       xs.append(ch)\n\
               \x20   return len(xs) * 10 + len(xs[2])\n\
               \n\
               def broke_out() -> int:\n\
               \x20   n = 0\n\
               \x20   for ch in \"abcdef\":\n\
               \x20       if ch == \"d\":\n\
               \x20           break\n\
               \x20       n += 1\n\
               \x20   return n\n";
    assert_eq!(call_i32(src, "digits"), 3);
    assert_eq!(call_i32(src, "collected"), 31);
    assert_eq!(call_i32(src, "broke_out"), 3);
}

/// `c[k] = v` with a key or value that does real work. The container pointer
/// and the key sat in scratch locals while the key and the value were emitted,
/// and those expressions use the same scratch locals, so `xs[1] = ys[2]`
/// wrote through whatever the nested index left behind.
#[test]
fn item_assignment_survives_a_key_or_value_that_does_work() {
    let src = "from typing import Dict, List\n\
               \n\
               def value_is_an_index() -> int:\n\
               \x20   xs: List[int] = [0, 0, 0]\n\
               \x20   ys: List[int] = [5, 6, 7]\n\
               \x20   xs[1] = ys[2]\n\
               \x20   return xs[1]\n\
               \n\
               def key_is_an_index() -> int:\n\
               \x20   xs: List[int] = [0, 0, 0]\n\
               \x20   ys: List[int] = [5, 6, 2]\n\
               \x20   xs[ys[2]] = 9\n\
               \x20   return xs[2]\n\
               \n\
               def dict_keys_from_a_string() -> int:\n\
               \x20   d: Dict[str, int] = {}\n\
               \x20   s = \"abab\"\n\
               \x20   i = 0\n\
               \x20   while i < len(s):\n\
               \x20       d[s[i]] = i\n\
               \x20       i += 1\n\
               \x20   return len(d) * 10 + d[\"b\"]\n\
               \n\
               def dict_key_is_a_method_call() -> int:\n\
               \x20   d: Dict[str, int] = {}\n\
               \x20   s = \"ab\"\n\
               \x20   d[s.upper()] = 4\n\
               \x20   return d[\"AB\"]\n";
    assert_eq!(call_i32(src, "value_is_an_index"), 7);
    assert_eq!(call_i32(src, "key_is_an_index"), 9);
    assert_eq!(call_i32(src, "dict_keys_from_a_string"), 23);
    assert_eq!(call_i32(src, "dict_key_is_a_method_call"), 4);
}

/// `int(s)` parses a string the way CPython does, and raises a catchable
/// `ValueError` where CPython does. It used to answer the string's *length*
/// and leave its offset on the stack: `int("12")` was 2, and every argument
/// after it in a call shifted by one.
#[test]
fn int_of_a_string_parses_it() {
    // (source text, CPython's answer, or None for ValueError)
    let cases: [(&str, Option<i32>); 20] = [
        ("12", Some(12)),
        ("345", Some(345)),
        ("  42  ", Some(42)),
        ("-7", Some(-7)),
        ("+9", Some(9)),
        ("1_000", Some(1000)),
        ("0", Some(0)),
        ("007", Some(7)),
        ("\\t8\\n", Some(8)),
        ("", None),
        ("  ", None),
        ("abc", None),
        ("1_", None),
        ("_1", None),
        ("1__0", None),
        ("1.5", None),
        ("12a", None),
        ("-", None),
        ("+ 3", None),
        ("4 5", None),
    ];
    for (text, expected) in cases {
        let src = format!(
            "def f() -> int:\n\
             \x20   s = \"{text}\"\n\
             \x20   try:\n\
             \x20       return int(s)\n\
             \x20   except ValueError:\n\
             \x20       return -999\n"
        );
        assert_eq!(
            call_i32(&src, "f"),
            expected.unwrap_or(-999),
            "int({text:?})"
        );
    }

    // The result composes: before, the stranded offset shifted the call.
    let composed = "class Num:\n\
                    \x20   def __init__(self, v: int):\n\
                    \x20       self.v = v\n\
                    \n\
                    def f() -> int:\n\
                    \x20   return Num(int(\"12\")).v + int(\"30\")\n";
    assert_eq!(call_i32(composed, "f"), 42);

    // The ValueError propagates out of a call, so its callers check for it.
    let propagated = "def parse(s: str) -> int:\n\
                      \x20   return int(s)\n\
                      \n\
                      def f() -> int:\n\
                      \x20   try:\n\
                      \x20       return parse(\"x\")\n\
                      \x20   except ValueError:\n\
                      \x20       return 1\n";
    assert_eq!(call_i32(propagated, "f"), 1);
}

/// What `int()` and `float()` cannot do honestly is refused: `float()` of a
/// string needs correctly rounded parsing (it converted the string's length
/// before), and `int()` of a collection or instance is a TypeError in CPython
/// (it passed the pointer through as a number).
#[test]
fn conversions_that_cannot_be_made_good_on_are_refused() {
    let float_of_str = "def f() -> float:\n\
                        \x20   return float(\"2.5\")\n";
    let err = try_compile(float_of_str).expect_err("float() of a string must be refused");
    assert!(err.contains("float() of a string"), "got: {err}");

    let int_of_list = "from typing import List\n\
                       \n\
                       def f() -> int:\n\
                       \x20   xs: List[int] = [1]\n\
                       \x20   return int(xs)\n";
    let err = try_compile(int_of_list).expect_err("int() of a list must be refused");
    assert!(err.contains("TypeError"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Program five: arithmetic, augmented assignment, fields, and fresh objects
// ---------------------------------------------------------------------------

/// Python's `//` and `%` floor; WebAssembly's `div_s` and `rem_s` truncate
/// toward zero. The two agree when the operands share a sign and differ by one
/// step when they do not, so every program with a negative operand got a
/// different number. The 0.15.0 arithmetic sweep checked division by zero and
/// never a negative operand.
#[test]
fn floor_division_and_modulo_follow_python() {
    let src = "def f(a: int, b: int) -> int:\n\
               \x20   return (a // b) * 1000 + (a % b)\n";
    // (a, b, CPython's a // b, CPython's a % b)
    for (a, b, q, r) in [
        (-7, 2, -4, 1),
        (7, -2, -4, -1),
        (-7, -2, 3, -1),
        (7, 2, 3, 1),
        (-7, 3, -3, 2),
        (7, -3, -3, -2),
        (-6, 3, -2, 0),
    ] {
        assert_eq!(
            harness::call_i32_2(src, "f", a, b),
            q * 1000 + r,
            "{a} // {b} and {a} % {b}"
        );
    }
}

/// `x op= v` is `x = x op v` and shares its codegen. It carried a separate
/// copy of the arithmetic, which was wrong in its own ways: string `+=` added a
/// length to an offset, `/=` divided as integers, float `%=` subtracted the
/// wrong way round. A list `+=` extends in place, as Python's does, so another
/// name for the same list sees it.
#[test]
fn augmented_assignment_matches_the_plain_form() {
    let src = "from typing import List\n\
               \n\
               def string_plus() -> int:\n\
               \x20   line = \"\"\n\
               \x20   for ch in \"abc\":\n\
               \x20       line += ch\n\
               \x20   line += \"!\"\n\
               \x20   return len(line)\n\
               \n\
               def float_div() -> float:\n\
               \x20   x = 7.0\n\
               \x20   x /= 2\n\
               \x20   return x\n\
               \n\
               def float_mod() -> float:\n\
               \x20   x = -7.5\n\
               \x20   x %= 2.0\n\
               \x20   return x\n\
               \n\
               def int_floor_mod() -> int:\n\
               \x20   x = -7\n\
               \x20   x %= 3\n\
               \x20   y = -7\n\
               \x20   y //= 2\n\
               \x20   return x * 10 + y\n\
               \n\
               def list_extends_in_place() -> int:\n\
               \x20   xs: List[int] = [1]\n\
               \x20   ys = xs\n\
               \x20   xs += [2, 3]\n\
               \x20   return len(ys)\n";
    assert_eq!(call_i32(src, "string_plus"), 4);
    assert_eq!(call_f64(src, "float_div"), 3.5);
    assert_eq!(call_f64(src, "float_mod"), 0.5);
    // -7 % 3 is 2 and -7 // 2 is -4.
    assert_eq!(call_i32(src, "int_floor_mod"), 16);
    assert_eq!(call_i32(src, "list_extends_in_place"), 3);

    // An int local cannot become a float mid-function, so `/=` on one is
    // refused rather than truncated back into its slot.
    let int_div = "def f() -> int:\n\
                   \x20   x = 7\n\
                   \x20   x /= 2\n\
                   \x20   return x\n";
    let err = try_compile(int_div).expect_err("'/=' on an int local must be refused");
    assert!(err.contains("makes 'x' a float"), "got: {err}");
}

/// The same for fields. The field version also held the object pointer in a
/// scratch local while the value was emitted, and did nothing at all for a
/// field the class does not have.
#[test]
fn augmented_assignment_to_a_field() {
    let src = "from typing import List\n\
               \n\
               class C:\n\
               \x20   def __init__(self):\n\
               \x20       self.n = -7\n\
               \x20       self.t = 0.5\n\
               \x20       self.s = \"\"\n\
               \x20       self.xs = [1]\n\
               \n\
               def modulo() -> int:\n\
               \x20   c = C()\n\
               \x20   c.n %= 3\n\
               \x20   return c.n\n\
               \n\
               def float_plus() -> float:\n\
               \x20   c = C()\n\
               \x20   c.t += 1\n\
               \x20   return c.t\n\
               \n\
               def string_plus() -> int:\n\
               \x20   c = C()\n\
               \x20   c.s += \"ab\"\n\
               \x20   c.s += \"c\"\n\
               \x20   return len(c.s)\n\
               \n\
               def list_in_place() -> int:\n\
               \x20   c = C()\n\
               \x20   ys = c.xs\n\
               \x20   c.xs += [2, 3]\n\
               \x20   return len(ys)\n\
               \n\
               def value_does_work() -> int:\n\
               \x20   c = C()\n\
               \x20   zs = [10, 20, 30]\n\
               \x20   c.n += zs[2]\n\
               \x20   return c.n\n";
    assert_eq!(call_i32(src, "modulo"), 2);
    assert_eq!(call_f64(src, "float_plus"), 1.5);
    assert_eq!(call_i32(src, "string_plus"), 3);
    assert_eq!(call_i32(src, "list_in_place"), 3);
    assert_eq!(call_i32(src, "value_does_work"), 23);

    let missing = "class C:\n\
                   \x20   def __init__(self):\n\
                   \x20       self.n = 0\n\
                   \n\
                   def f() -> int:\n\
                   \x20   c = C()\n\
                   \x20   c.nope += 1\n\
                   \x20   return 0\n";
    let err = try_compile(missing).expect_err("a missing field must be refused");
    assert!(err.contains("'C' has no attribute 'nope'"), "got: {err}");
}

/// A string field started from a literal (`self.s = ""`) was typed as nothing
/// in particular, so every read of it lost its length: `c.s = "abc";
/// len(c.s)` answered 6513249, the bytes "abc" read as an integer. Only fields
/// set from an annotated parameter came out right. A write to a field the
/// class never declares used to vanish; it is refused now.
#[test]
fn string_fields_keep_their_type_and_unknown_fields_are_refused() {
    let src = "class C:\n\
               \x20   def __init__(self):\n\
               \x20       self.s = \"\"\n\
               \x20   def add(self, t: str):\n\
               \x20       self.s = self.s + t\n\
               \n\
               def assigned() -> int:\n\
               \x20   c = C()\n\
               \x20   c.s = \"abc\"\n\
               \x20   return len(c.s)\n\
               \n\
               def grown_in_a_method() -> int:\n\
               \x20   c = C()\n\
               \x20   c.add(\"ab\")\n\
               \x20   c.add(\"c\")\n\
               \x20   return len(c.s)\n";
    assert_eq!(call_i32(src, "assigned"), 3);
    assert_eq!(call_i32(src, "grown_in_a_method"), 3);

    let undeclared = "class C:\n\
                      \x20   def __init__(self):\n\
                      \x20       self.n = 0\n\
                      \n\
                      def f() -> int:\n\
                      \x20   c = C()\n\
                      \x20   c.other = 1\n\
                      \x20   return c.n\n";
    let err = try_compile(undeclared).expect_err("a write to an undeclared field must be refused");
    assert!(err.contains("no attribute 'other' to assign"), "got: {err}");
}

/// Every evaluation of a literal is a new object. A literal built into one
/// compile-time region per site, so a function called twice returned the same
/// list both times, the second call reset the first result, a literal whose
/// element recursed into its own function had its earlier elements
/// overwritten, and a recursive loop over `range()` shared one range object
/// between activations.
#[test]
fn every_evaluation_of_a_literal_is_a_new_object() {
    let src = "from typing import List\n\
               \n\
               def make(n: int) -> List[int]:\n\
               \x20   xs: List[int] = []\n\
               \x20   xs.append(n)\n\
               \x20   return xs\n\
               \n\
               def pair() -> List[int]:\n\
               \x20   return [7, 8]\n\
               \n\
               def depth(n: int) -> int:\n\
               \x20   if n == 0:\n\
               \x20       return 0\n\
               \x20   return len(nest(n - 1))\n\
               \n\
               def nest(n: int) -> List[int]:\n\
               \x20   return [n, depth(n), n + 5]\n\
               \n\
               def walk(n: int) -> int:\n\
               \x20   total = 0\n\
               \x20   for i in range(n):\n\
               \x20       total += 1 + walk(i)\n\
               \x20   return total\n\
               \n\
               def two_calls() -> int:\n\
               \x20   a = make(1)\n\
               \x20   b = make(2)\n\
               \x20   b.append(3)\n\
               \x20   return len(a) * 10 + a[0]\n\
               \n\
               def two_literal_results() -> int:\n\
               \x20   p = pair()\n\
               \x20   q = pair()\n\
               \x20   q.append(9)\n\
               \x20   return len(p)\n\
               \n\
               def recursion_mid_build() -> int:\n\
               \x20   r = nest(2)\n\
               \x20   return r[0] * 100 + r[1] * 10 + r[2]\n\
               \n\
               def recursion_over_range() -> int:\n\
               \x20   return walk(4)\n";
    assert_eq!(call_i32(src, "two_calls"), 11);
    assert_eq!(call_i32(src, "two_literal_results"), 2);
    assert_eq!(call_i32(src, "recursion_mid_build"), 237);
    assert_eq!(call_i32(src, "recursion_over_range"), 15);

    // A dict literal's first pair used to be evaluated twice, once to learn its
    // types, so `{c.bump(): c.bump()}` bumped four times.
    let side_effects = "class C:\n\
                        \x20   def __init__(self):\n\
                        \x20       self.n = 0\n\
                        \x20   def bump(self) -> int:\n\
                        \x20       self.n += 1\n\
                        \x20       return self.n\n\
                        \n\
                        def f() -> int:\n\
                        \x20   c = C()\n\
                        \x20   d = {c.bump(): c.bump()}\n\
                        \x20   return c.n\n";
    assert_eq!(call_i32(side_effects, "f"), 2);
}

/// `for row in board` over a list of lists binds each row as a list, so
/// `sum(row)` walks it. The loop variable used to be typed only for string
/// elements, and `sum()` of an untyped value answered the value itself, which
/// here was the row's pointer. Anything `sum()` cannot walk is refused now.
#[test]
fn a_list_of_lists_binds_typed_rows_and_sum_refuses_what_it_cannot_walk() {
    let src = "from typing import List\n\
               \n\
               def population(board: List[List[int]]) -> int:\n\
               \x20   total = 0\n\
               \x20   for row in board:\n\
               \x20       total += sum(row)\n\
               \x20   return total\n\
               \n\
               def f() -> int:\n\
               \x20   return population([[1, 0, 1], [1, 1, 0]])\n";
    assert_eq!(call_i32(src, "f"), 4);

    let set_sum = "from typing import Set\n\
                   \n\
                   def f() -> int:\n\
                   \x20   s: Set[int] = {1, 2}\n\
                   \x20   return sum(s)\n";
    let err = try_compile(set_sum).expect_err("sum() of a set must be refused");
    assert!(err.contains("sum() of a value of type"), "got: {err}");
}

/// `a or b or c` is one operation over three operands in Python's AST, and only
/// two used to be accepted. It folds left, which keeps both the value and the
/// short-circuit order.
#[test]
fn boolean_chains_of_any_length() {
    let src = "def f(a: int, b: int, c: int) -> int:\n\
               \x20   if a or b or c:\n\
               \x20       x = 1\n\
               \x20   else:\n\
               \x20       x = 0\n\
               \x20   if a and b and c:\n\
               \x20       y = 1\n\
               \x20   else:\n\
               \x20       y = 0\n\
               \x20   return x * 10 + y\n";
    let three = |a, b, c| {
        harness::call_untyped_i32(
            src,
            "f",
            &[
                wasmi::Value::I32(a),
                wasmi::Value::I32(b),
                wasmi::Value::I32(c),
            ],
        )
    };
    assert_eq!(three(0, 0, 1), 10);
    assert_eq!(three(0, 0, 0), 0);
    assert_eq!(three(1, 1, 1), 11);
    assert_eq!(three(1, 1, 0), 10);
}

/// Operators that are TypeErrors in CPython pushed a 0 over their operands and
/// reported success.
#[test]
fn operators_cpython_rejects_are_refused() {
    let float_bits = "def f() -> float:\n\
                      \x20   a = 1.5\n\
                      \x20   return a | 2.0\n";
    let err = try_compile(float_bits).expect_err("'|' on a float must be refused");
    assert!(
        err.contains("unsupported operand type(s) for |"),
        "got: {err}"
    );

    let matmul = "def f() -> int:\n\
                  \x20   a = 2\n\
                  \x20   return a @ 3\n";
    let err = try_compile(matmul).expect_err("'@' on ints must be refused");
    assert!(err.contains("for @"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Values compare by what they hold (collections, strings, dict keys)
// ---------------------------------------------------------------------------

/// Tuples and lists compare by value with `==`, `!=`, and the orderings, as
/// CPython's do: element by element, stopping at the first difference, with a
/// shorter sequence ordering first when one is a prefix of the other. Every
/// operator compared the two pointers, so `(a, 2) == (1, 2)` was False and
/// `(1, 2) < (1, 3)` answered by allocation order.
#[test]
fn tuples_and_lists_compare_by_value() {
    // (expression, CPython's answer)
    let cases: [(&str, i32); 16] = [
        ("(a, 2) == (1, 2)", 1),
        ("(a, 2) != (1, 3)", 1),
        ("(s, 1) == (\"x\", 1)", 1),
        ("(1.5, 2.0) == (1.5, 2.0)", 1),
        ("((1, 2), (3, 4)) == ((1, 2), (3, 4))", 1),
        ("[1, 2] == [1, 2]", 1),
        ("[1, 2] == [1, 2, 3]", 0),
        ("[s, \"r\"] == [\"x\", \"r\"]", 1),
        ("[1, 2] == (1, 2)", 0),
        ("(1, 2) < (1, 3)", 1),
        ("(2, 0) > (1, 9)", 1),
        ("(1, 2) <= (1, 2)", 1),
        ("(\"a\", 2) < (\"b\", 1)", 1),
        ("[1, 2] < [1, 2, 0]", 1),
        ("[3] > [2, 9]", 1),
        ("[1, 2] >= [1, 3]", 0),
    ];
    for (expr, expected) in cases {
        let src = format!(
            "def f() -> int:\n\
             \x20   a = 1\n\
             \x20   s = \"x\"\n\
             \x20   if {expr}:\n\
             \x20       return 1\n\
             \x20   return 0\n"
        );
        assert_eq!(call_i32(&src, "f"), expected, "{expr}");
    }
}

/// Strings order byte-wise, which for UTF-8 is code-point order, CPython's
/// order for `str`. Every ordering, and `is`, answered False whatever the
/// strings were.
#[test]
fn strings_order_and_compare_identity() {
    let cases: [(&str, i32); 6] = [
        ("a < b", 1),
        ("a > b", 0),
        ("a <= a", 1),
        ("b >= a", 1),
        ("\"ab\" < \"abc\"", 1),
        ("a is a", 1),
    ];
    for (expr, expected) in cases {
        let src = format!(
            "def f() -> int:\n\
             \x20   a = \"apple\"\n\
             \x20   b = \"banana\"\n\
             \x20   if {expr}:\n\
             \x20       return 1\n\
             \x20   return 0\n"
        );
        assert_eq!(call_i32(&src, "f"), expected, "{expr}");
    }
}

/// Everything built on `==` follows it: `in` over a list, `index`, `count`,
/// set membership and de-duplication, and dict keys. A set hashes a string by
/// its bytes and a tuple by its members, so equal values land in the same
/// bucket; a string used to hash by its offset, so one built at runtime missed
/// an equal member already in the set.
#[test]
fn containers_find_equal_values() {
    let src = "from typing import Dict, List, Set, Tuple\n\
               \n\
               def tuple_in_set() -> int:\n\
               \x20   s: Set[Tuple[int, int]] = {(1, 2)}\n\
               \x20   a = 1\n\
               \x20   return int((a, 2) in s)\n\
               \n\
               def tuple_set_dedup() -> int:\n\
               \x20   s: Set[Tuple[int, int]] = set()\n\
               \x20   s.add((1, 2))\n\
               \x20   s.add((1, 2))\n\
               \x20   s.add((2, 1))\n\
               \x20   return len(s)\n\
               \n\
               def tuple_in_list() -> int:\n\
               \x20   xs: List[Tuple[int, int]] = [(1, 2), (3, 4)]\n\
               \x20   return int((3, 4) in xs) * 100 + xs.index((3, 4)) * 10 + xs.count((1, 2))\n\
               \n\
               def tuple_dict_key() -> int:\n\
               \x20   d: Dict[Tuple[int, int], int] = {}\n\
               \x20   d[(1, 2)] = 5\n\
               \x20   d[(1, 2)] = 6\n\
               \x20   a = 1\n\
               \x20   return len(d) * 100 + d[(1, 2)] * 10 + d.get((a, 2), 0) // 6\n\
               \n\
               def runtime_string_in_set() -> int:\n\
               \x20   s: Set[str] = {\"ab\", \"cd\"}\n\
               \x20   x = \"a\" + \"b\"\n\
               \x20   t: Set[str] = set()\n\
               \x20   t.add(\"a\" + \"b\")\n\
               \x20   t.add(\"ab\")\n\
               \x20   return int(x in s) * 10 + len(t)\n";
    assert_eq!(call_i32(src, "tuple_in_set"), 1);
    assert_eq!(call_i32(src, "tuple_set_dedup"), 2);
    assert_eq!(call_i32(src, "tuple_in_list"), 111);
    assert_eq!(call_i32(src, "tuple_dict_key"), 161);
    assert_eq!(call_i32(src, "runtime_string_in_set"), 11);

    // A list cannot be a set member or a dict key: unhashable in CPython.
    let unhashable = "from typing import Dict, List\n\
                      \n\
                      def f() -> int:\n\
                      \x20   d: Dict[List[int], int] = {}\n\
                      \x20   d[[1]] = 2\n\
                      \x20   return len(d)\n";
    let err = try_compile(unhashable).expect_err("a list dict key must be refused");
    assert!(err.contains("unhashable type: 'list'"), "got: {err}");
}

/// A tuple subscript is an index whose value is a tuple. It was taken for a
/// slice, so `d[(1, 2)]` sliced the dict and answered its pointer, and
/// `xs[(1, 2)]` on a list returned `xs[1:2]`.
#[test]
fn a_tuple_subscript_is_an_index_not_a_slice() {
    let src = "from typing import Dict, Tuple\n\
               \n\
               def f() -> int:\n\
               \x20   d: Dict[Tuple[int, int], int] = {(1, 2): 5}\n\
               \x20   return d[(1, 2)] + d[1, 2]\n";
    assert_eq!(call_i32(src, "f"), 10);
}

/// `"b" in d.keys()` held the searched value in a scratch local while the
/// container was emitted, and the method call overwrote it.
#[test]
fn an_in_test_survives_a_container_that_does_work() {
    let src = "from typing import Dict, List\n\
               \n\
               def f() -> int:\n\
               \x20   d: Dict[str, int] = {\"a\": 1, \"b\": 2}\n\
               \x20   rows: List[List[int]] = [[1, 2], [3, 4]]\n\
               \x20   x = 3\n\
               \x20   return int(\"b\" in d.keys()) * 10 + int(x in rows[1])\n";
    assert_eq!(call_i32(src, "f"), 11);
}

// ---------------------------------------------------------------------------
// Tuple unpacking
// ---------------------------------------------------------------------------

/// Unpacked targets take their members' types. They were untyped, so a float
/// member bound as its low 32 bits, and a target could not be compared or
/// used as what it was. A value of the wrong length raises ValueError, which
/// it silently ignored before.
#[test]
fn unpacking_types_its_targets_and_checks_the_length() {
    let src = "from typing import List, Tuple\n\
               \n\
               def floats() -> float:\n\
               \x20   a, b = (1.5, 2.5)\n\
               \x20   return a + b\n\
               \n\
               def from_a_queue() -> int:\n\
               \x20   q: List[Tuple[int, int]] = [(1, 2)]\n\
               \x20   r, c = q.pop(0)\n\
               \x20   return int((r, c) == (1, 2))\n\
               \n\
               def too_many() -> int:\n\
               \x20   xs: List[int] = [1, 2, 3]\n\
               \x20   try:\n\
               \x20       x, y = xs\n\
               \x20       return 0\n\
               \x20   except ValueError:\n\
               \x20       return 1\n\
               \n\
               def too_few_around_a_star() -> int:\n\
               \x20   xs: List[int] = [1]\n\
               \x20   try:\n\
               \x20       a, *b, c = xs\n\
               \x20       return 0\n\
               \x20   except ValueError:\n\
               \x20       return 1\n";
    assert_eq!(call_f64(src, "floats"), 4.0);
    assert_eq!(call_i32(src, "from_a_queue"), 1);
    assert_eq!(call_i32(src, "too_many"), 1);
    assert_eq!(call_i32(src, "too_few_around_a_star"), 1);
}

// ---------------------------------------------------------------------------
// Module and class definitions are one object each
// ---------------------------------------------------------------------------

/// A module-level definition that is not a plain constant is evaluated once,
/// at instantiation, and every function shares it. Every read used to inline
/// the initializer, so each read was a new object: a mutation was lost the
/// moment it was made, an instance was new wherever it was named, and an
/// initializer ran once per mention.
#[test]
fn module_definitions_are_evaluated_once_and_shared() {
    let src = "from typing import Dict, List\n\
               \n\
               class Counter:\n\
               \x20   def __init__(self):\n\
               \x20       self.n = 0\n\
               \x20   def tick(self) -> int:\n\
               \x20       self.n += 1\n\
               \x20       return self.n\n\
               \n\
               G = [1]\n\
               D: Dict[int, int] = {}\n\
               C = Counter()\n\
               TRACK = Counter()\n\
               \n\
               def make() -> List[int]:\n\
               \x20   TRACK.tick()\n\
               \x20   return [1, 2, 3]\n\
               \n\
               DATA = make()\n\
               B = len(G)\n\
               \n\
               def grow(xs: List[int]):\n\
               \x20   xs.append(5)\n\
               \n\
               def mutated() -> int:\n\
               \x20   G.append(2)\n\
               \x20   grow(G)\n\
               \x20   G[0] = 9\n\
               \x20   return len(G) * 100 + G[0] * 10 + G[2]\n\
               \n\
               def dict_shared() -> int:\n\
               \x20   D[1] = 10\n\
               \x20   D[2] = 20\n\
               \x20   return len(D) * 100 + D[2]\n\
               \n\
               def instance_shared() -> int:\n\
               \x20   C.tick()\n\
               \x20   C.tick()\n\
               \x20   return C.tick()\n\
               \n\
               def initializer_ran_once() -> int:\n\
               \x20   return len(DATA) + len(DATA) + TRACK.n\n\
               \n\
               def evaluated_in_order() -> int:\n\
               \x20   return B\n";
    assert_eq!(call_i32(src, "mutated"), 395);
    assert_eq!(call_i32(src, "dict_shared"), 220);
    assert_eq!(call_i32(src, "instance_shared"), 3);
    assert_eq!(call_i32(src, "initializer_ran_once"), 7);
    // B = len(G) is taken at import, before `mutated()` could append.
    assert_eq!(call_i32(src, "evaluated_in_order"), 1);

    let forward = "B = len(A)\n\
                   A = [1, 2]\n\
                   \n\
                   def f() -> int:\n\
                   \x20   return B\n";
    let err = try_compile(forward).expect_err("a forward reference must be refused");
    assert!(
        err.contains("'A' is used before it is defined"),
        "got: {err}"
    );
}

/// A class-level variable is shared by the class the same way.
#[test]
fn class_variables_are_one_object() {
    let src = "class C:\n\
               \x20   items = []\n\
               \x20   LIMIT = 10\n\
               \x20   def add(self, v: int):\n\
               \x20       C.items.append(v)\n\
               \n\
               def f() -> int:\n\
               \x20   a = C()\n\
               \x20   b = C()\n\
               \x20   a.add(1)\n\
               \x20   b.add(2)\n\
               \x20   return len(C.items) * 100 + C.LIMIT\n";
    assert_eq!(call_i32(src, "f"), 210);
}

// ---------------------------------------------------------------------------
// Program six: annotated fields, augmented assignment through anything,
// round() and abs()
// ---------------------------------------------------------------------------

/// `self.items: Dict[str, Item] = {}` declares the field's type, which wins
/// over anything inferred from its values. It was refused ("Only variable
/// assignment supported"), which is how typed Python annotates a collection
/// field.
#[test]
fn annotated_fields_take_their_annotation() {
    let src = "from typing import Dict, List\n\
               \n\
               class Item:\n\
               \x20   def __init__(self, qty: int):\n\
               \x20       self.qty = qty\n\
               \n\
               class Store:\n\
               \x20   def __init__(self):\n\
               \x20       self.items: Dict[str, Item] = {}\n\
               \x20       self.log: List[str] = []\n\
               \x20   def put(self, sku: str, qty: int):\n\
               \x20       self.items[sku] = Item(qty)\n\
               \x20       self.log.append(sku)\n\
               \n\
               def f() -> int:\n\
               \x20   s = Store()\n\
               \x20   s.put(\"a\", 3)\n\
               \x20   s.put(\"b\", 4)\n\
               \x20   return s.items[\"b\"].qty * 100 + len(s.log) * 10 + len(s.log[1])\n";
    assert_eq!(call_i32(src, "f"), 421);
}

/// Augmented assignment through a subscript (`counts[w] += 1`) was refused
/// outright, and through a computed object (`self.items[k].qty += n`) it
/// would have evaluated the object twice. Each target is evaluated once now,
/// as in Python.
#[test]
fn augmented_assignment_through_subscripts_and_computed_objects() {
    let src = "from typing import Dict, List\n\
               \n\
               class It:\n\
               \x20   def __init__(self):\n\
               \x20       self.qty = 3\n\
               \n\
               def f() -> int:\n\
               \x20   c: Dict[str, int] = {\"a\": 1}\n\
               \x20   c[\"a\"] += 1\n\
               \x20   c[\"b\"] = 5\n\
               \x20   c[\"b\"] -= 2\n\
               \x20   xs: List[int] = [1, 2, 3]\n\
               \x20   i = 0\n\
               \x20   xs[i + 1] *= 10\n\
               \x20   d: Dict[str, It] = {\"k\": It()}\n\
               \x20   d[\"k\"].qty += 4\n\
               \x20   return c[\"a\"] * 1000 + c[\"b\"] * 100 + xs[1] + d[\"k\"].qty\n";
    assert_eq!(call_i32(src, "f"), 2327);

    // The object is evaluated once: a call in the target runs one time.
    let once = "class Box:\n\
                \x20   def __init__(self):\n\
                \x20       self.v = 0\n\
                \n\
                class Maker:\n\
                \x20   def __init__(self):\n\
                \x20       self.calls = 0\n\
                \x20       self.box = Box()\n\
                \x20   def get(self) -> Box:\n\
                \x20       self.calls += 1\n\
                \x20       return self.box\n\
                \n\
                def f() -> int:\n\
                \x20   m = Maker()\n\
                \x20   m.get().v += 5\n\
                \x20   return m.calls * 10 + m.box.v\n";
    assert_eq!(call_i32(once, "f"), 15);
}

/// `round()` rounds the exact binary value of a float, halves to even, as
/// CPython does. The shortcut `floor(x * 10**n + 0.5) / 10**n` disagrees
/// wherever the product is inexact, which is why 2.675 and 1.005 are here.
#[test]
fn round_matches_cpython_exactly() {
    // (x, ndigits, CPython's round(x, ndigits))
    let cases: [(&str, i32, f64); 14] = [
        ("2.675", 2, 2.67),
        ("1.005", 2, 1.0),
        ("0.125", 2, 0.12),
        ("0.375", 2, 0.38),
        ("2.5", 0, 2.0),
        ("3.5", 0, 4.0),
        ("-2.5", 0, -2.0),
        ("-0.4", 0, -0.0),
        ("99.99 * 0.95", 2, 94.99),
        ("1234.5678", 3, 1234.568),
        ("0.1 + 0.2", 1, 0.3),
        ("1e20", 2, 1e20),
        ("-7.25", 1, -7.2),
        ("123456789.123456789", 5, 123456789.12346),
    ];
    for (x, n, expected) in cases {
        let src = format!(
            "def f() -> float:\n\
             \x20   x = {x}\n\
             \x20   return round(x, {n})\n"
        );
        let got = call_f64(&src, "f");
        assert_eq!(
            got.to_bits(),
            expected.to_bits(),
            "round({x}, {n}) gave {got}"
        );
    }
    for (x, expected) in [
        ("2.5", 2),
        ("3.5", 4),
        ("-2.5", -2),
        ("2.6", 3),
        ("-0.4", 0),
    ] {
        let src = format!(
            "def f() -> int:\n\
             \x20   x = {x}\n\
             \x20   return round(x)\n"
        );
        assert_eq!(call_i32(&src, "f"), expected, "round({x})");
    }
    let abs_src = "def f() -> float:\n\
                   \x20   a = -7\n\
                   \x20   b = -2.5\n\
                   \x20   return abs(a) + abs(b)\n";
    assert_eq!(call_f64(abs_src, "f"), 9.5);
}
