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

/// Dispatch is static, so a subclass override is invisible to a base method
/// that calls it through `self`: `Media.describe()` reaches `Media.kind()` even
/// on a `Video`. Calling the override directly is correct. Pinned as-is; it
/// changes to "video:b" when virtual dispatch lands.
#[test]
fn an_inherited_method_still_uses_static_dispatch() {
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
    assert_eq!(
        call_str(src, "inherited"),
        "media:b",
        "an inherited base method is expected to still reach the base override"
    );
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
