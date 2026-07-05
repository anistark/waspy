//! Integration coverage for the bundled `examples/*.py`.
//!
//! Tiers:
//!   1. Every standalone example must compile to a valid, instantiable WASM
//!      module. This is the "invalid / unrunnable module" class of regression
//!      that the 0.10.0 correctness pass fixed; the sweep keeps it fixed and any
//!      new example is covered automatically.
//!   2. Multi-file compilation produces a valid module and cross-file calls run.
//!   3. Two codegen defects this harness surfaced — a `str`-typed function
//!      parameter compared with `==`, and `raise ExceptionType(arg)` — have
//!      dedicated regression tests.
//!   4. The 0.11.0 headline feature — `break` / `continue` — asserts concrete
//!      runtime results by calling the exported functions of
//!      `examples/loop_control.py`.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{
    call_f64, call_i32, call_i32_2, example_python_files, instantiate_wasm, read_example,
    try_compile, try_compile_multi, try_instantiate, MULTI_FILE_ONLY,
};

/// Every standalone-compilable example compiles, validates, and instantiates.
/// Multi-file examples are excluded (and covered by their own test below).
/// Failures are collected so the report lists every broken example, not just
/// the first.
#[test]
fn all_examples_compile_and_instantiate() {
    let mut failures = Vec::new();
    for path in example_python_files() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if MULTI_FILE_ONLY.contains(&name.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read example file");
        let result = try_compile(&source).and_then(|wasm| try_instantiate(&wasm));
        if let Err(err) = result {
            failures.push(format!("{name}: {err}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} example(s) failed to compile + instantiate:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// The multi-file demo compiles `basic_operations.py` + `calculator.py` into a
/// single module (calculator depends on the other file) and runs functions that
/// cross the file boundary. `complex_calculation(x, y)` computes
/// `(x + y) * (x - y)`, so `(5, 3) == 16`; `calculate_factorial(5) == 120`
/// exercises cross-file recursion.
#[test]
fn calculator_multi_file_compiles_and_runs() {
    let basic = read_example("basic_operations.py");
    let calculator = read_example("calculator.py");
    let wasm = try_compile_multi(&[
        ("basic_operations.py", &basic),
        ("calculator.py", &calculator),
    ])
    .expect("multi-file compilation");
    let (instance, mut store) = instantiate_wasm(&wasm);
    let complex = instance
        .get_typed_func::<(i32, i32), i32>(&store, "complex_calculation")
        .expect("exported complex_calculation");
    assert_eq!(complex.call(&mut store, (5, 3)).expect("call"), 16);
    let factorial = instance
        .get_typed_func::<i32, i32>(&store, "calculate_factorial")
        .expect("exported calculate_factorial");
    assert_eq!(factorial.call(&mut store, 5).expect("call"), 120);
}

/// Regression test for the `str`-parameter bug `calculator.py` surfaced: a
/// function with a `str` parameter compared via `==`, called with a string
/// literal. Both layers must work — the call narrows the string argument to its
/// offset word, and the callee recovers the length from the blob prefix to run
/// the byte-for-byte comparison. `classify` returns 1 for "add", 2 for "sub",
/// 0 otherwise; the no-arg entry points make it callable without host-side
/// string marshalling.
#[test]
fn str_parameter_equality_runs() {
    let src = "def classify(op: str) -> int:\n    if op == \"add\":\n        return 1\n    if op == \"sub\":\n        return 2\n    return 0\n\ndef check_add() -> int:\n    return classify(\"add\")\n\ndef check_sub() -> int:\n    return classify(\"sub\")\n\ndef check_other() -> int:\n    return classify(\"xyz\")\n";
    assert_eq!(call_i32(src, "check_add"), 1);
    assert_eq!(call_i32(src, "check_sub"), 2);
    assert_eq!(call_i32(src, "check_other"), 0);
}

/// Regression test for the `raise ExceptionType(arg)` bug `exceptions.py`
/// surfaced: raising a built-in exception constructed with an argument
/// (`raise ValueError("never")`) must not leave the argument on the stack. The
/// exception is resolved to its type code by name, so the module is valid and
/// instantiates. The `try` returns 7 before the (never-taken) handler.
#[test]
fn raise_with_argument_is_valid() {
    let src = "def guard() -> int:\n    try:\n        return 7\n    except ValueError:\n        raise ValueError(\"never\")\n    finally:\n        done = 1\n    return 0\n";
    assert_eq!(call_i32(src, "guard"), 7);
}

/// `break` exits the loop early: summing `range(100)` but breaking at `i == 5`
/// yields 0 + 1 + 2 + 3 + 4 = 10.
#[test]
fn break_exits_loop_early() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32(&src, "sum_until_five"), 10);
}

/// `continue` skips the rest of the body: summing the odd numbers below ten
/// yields 1 + 3 + 5 + 7 + 9 = 25.
#[test]
fn continue_skips_iteration() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32(&src, "sum_odds_below_ten"), 25);
}

/// `break` / `continue` inside a `while True` loop: the first multiple of 3
/// strictly greater than 10 is 12.
#[test]
fn break_continue_in_while_loop() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32_2(&src, "first_multiple_over", 10, 3), 12);
}

/// `break` exits only the innermost loop: the inner loop breaks at `j == 1`
/// after one increment, across three outer iterations, so the count is 3.
#[test]
fn break_exits_innermost_loop_only() {
    let src = read_example("loop_control.py");
    assert_eq!(call_i32(&src, "count_inner_breaks"), 3);
}

/// Statically nested list-of-lists: each inner literal occupies its own region,
/// so `grid[0][1] + grid[1][0]` reads 2 + 3 = 5 (Issue #14).
#[test]
fn nested_list_indexing() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "nested_grid"), 5);
}

/// A list literal built inside a loop that escapes must get a fresh region per
/// iteration, allocated from the runtime heap rather than the one compile-time
/// region every iteration would otherwise share. `grid[0][0]` stays 0 and
/// `grid[2][0]` is 2, so the result is 0*100 + 2 = 2; aliasing would give 202
/// (every row pointing at the last iteration's data). Issue #14.
#[test]
fn per_iteration_collection_does_not_alias() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "loop_escape"), 2);
}

/// Float dict values round-trip through their 8-byte slot: `d[1] + d[2]` =
/// 3.5 + 7.5 = 11.0, truncated to 11.
#[test]
fn float_dict_values_round_trip() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "float_dict_sum"), 11);
}

/// Float set members de-duplicate by value: `{1.5, 1.5, 2.5}` has two distinct
/// members. Members are hashed and compared at full f64 width.
#[test]
fn float_set_members_dedup() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "float_set_size"), 2);
}

/// Set hash table (v0.12.0 P3): dedup on insert, `in`/`not in` membership, the
/// linear-probe collision chain, float members, and stale-state clearing when a
/// set literal is rebuilt each loop iteration.
#[test]
fn set_hash_table() {
    let src = read_example("nested_collections.py");
    // {1, 2, 2, 3, 1} dedups to 3 members.
    assert_eq!(call_i32(&src, "int_set_dedup"), 3);
    // `5 in s` and `4 not in s` both hold -> 2.
    assert_eq!(call_i32(&src, "set_membership"), 2);
    // 0, 8, 16 collide in bucket 0: probing keeps them distinct and findable.
    assert_eq!(call_i32(&src, "set_collision_probe"), 32);
    // `2.5 in {1.5, 2.5, 3.5}` via the f64-hashed probe.
    assert_eq!(call_i32(&src, "float_set_membership"), 1);
    // Each loop iteration rebuilds a 2-member set from a cleared region: 2*3.
    assert_eq!(call_i32(&src, "set_loop_fresh"), 6);
}

/// f64 values round-trip through collection slots without precision loss (the
/// v0.12.0 P2 layout). Each value below needs more than f32's ~7 significant
/// digits, so an exact compare fails if the slot were a lossy 4-byte f32.
#[test]
fn float_collections_are_lossless() {
    let src = read_example("nested_collections.py");
    // The Python literal 3.141592653589793 is exactly the f64 value of PI; an
    // f32 slot would round it to ~3.1415927 and fail this exact compare.
    let pi = std::f64::consts::PI;
    // Pi to full f64 precision out of a list slot.
    assert_eq!(call_f64(&src, "float_list_roundtrip"), pi);
    // The classic 0.1 + 0.2 low bits only survive with full-width storage.
    assert_eq!(call_f64(&src, "float_list_sum"), 0.1_f64 + 0.2_f64);
    // Dict value lookup keeps full precision.
    assert_eq!(call_f64(&src, "float_dict_precise"), pi);
    // Float tuple member.
    assert_eq!(call_f64(&src, "float_tuple_roundtrip"), pi);
}

/// Float dict *keys* (v0.12.0 follow-up) match at full f64 width, both on
/// lookup and on in-place assignment. 1.5 and 2.5 share their low 32 bits, so a
/// lossy i32-word key compare could not tell them apart.
#[test]
fn float_dict_keys_are_width_aware() {
    let src = read_example("nested_collections.py");
    // d[1.5]==10 and d[2.5]==20 resolve distinctly: 10 + 20*100.
    assert_eq!(call_i32(&src, "float_dict_key_lookup"), 2010);
    // Float key, int value: d[2.5]==9.
    assert_eq!(call_i32(&src, "float_dict_key_int_value"), 9);
    // Assigning through a float key updates in place: 99 + 10.
    assert_eq!(call_i32(&src, "float_dict_key_assign"), 109);
}

/// `in` over a float list matches by value at full width.
#[test]
fn float_list_membership() {
    let src = read_example("nested_collections.py");
    assert_eq!(call_i32(&src, "float_membership"), 1);
}

/// Iterating a float list literal binds each element as an f64 loop variable, so
/// the running sum is exact: 0.1 + 0.2 + 0.3 == 0.6000000000000001 in f64.
#[test]
fn float_list_iteration_binds_f64() {
    let src = read_example("nested_collections.py");
    assert_eq!(
        call_f64(&src, "float_loop_sum"),
        0.1_f64 + 0.2_f64 + 0.3_f64
    );
}

/// Heap-allocated instances (v0.13.0 P0): two live `Counter` instances mutate
/// independently. Under the old fixed-address model both names aliased one
/// instance, so the second constructor call clobbered the first's state.
#[test]
fn two_instances_have_independent_state() {
    let src = read_example("oop_objects.py");
    // a: 10 -> 12 via two increments; b: 100 -> 103 via add(3).
    assert_eq!(call_i32(&src, "two_counters_independent"), 1303);
}

/// Every `ClassName(...)` allocates a fresh zeroed block, so a second instance
/// starts from its own `__init__` state rather than the first's leftovers.
#[test]
fn each_instantiation_gets_a_fresh_instance() {
    let src = read_example("oop_objects.py");
    assert_eq!(call_i32(&src, "fresh_instance_per_call"), 2);
}

/// An instance returned from a factory function stays live and mutable in the
/// caller (the pointer is a first-class value).
#[test]
fn instance_returned_from_factory() {
    let src = read_example("oop_objects.py");
    assert_eq!(call_i32(&src, "counter_from_factory"), 42);
}

/// An instance passed as a function argument is mutated through the shared
/// pointer, so the caller observes the callee's writes: 5 + 4 increments = 9.
#[test]
fn instance_passed_as_argument_shares_state() {
    let src = read_example("oop_objects.py");
    assert_eq!(call_i32(&src, "counter_as_argument"), 9);
}

/// Instances stored in a list read back as live pointers: mutating the
/// read-back instance (via `+=` on a field) is visible through the original
/// name. (7+5) + 30 = 42.
#[test]
fn instances_stored_in_list_read_back() {
    let src = read_example("oop_objects.py");
    assert_eq!(call_i32(&src, "instances_in_a_list"), 42);
}

/// Instances stored in a tuple and a dict read back as live pointers, the same
/// slot convention as lists. Both sum to 42.
#[test]
fn instances_stored_in_tuple_and_dict() {
    let src = read_example("oop_objects.py");
    assert_eq!(call_i32(&src, "instances_in_a_tuple"), 42);
    assert_eq!(call_i32(&src, "instances_in_a_dict"), 42);
}

/// Float (f64) fields remain per-instance: scaling one `Point` leaves the
/// other untouched. (1.5+2.5)*2 + 10.0 + 20.0 = 38.0.
#[test]
fn float_fields_are_per_instance() {
    let src = read_example("oop_objects.py");
    assert_eq!(call_f64(&src, "float_fields_two_instances"), 38.0);
}

/// A subclass override replaces the base implementation at call sites typed as
/// the subclass, while the base keeps its own: Animal.speak()=1, Dog.speak()=2.
#[test]
fn subclass_overrides_method() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "override_wins"), 12);
}

/// A method defined only on the base is callable on a subclass instance and
/// reads the inherited field laid out in the base prefix: leg_count() = 4.
#[test]
fn subclass_inherits_unoverridden_method() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "inherited_method_on_subclass"), 4);
}

/// `super().__init__(...)` chains construction: base fields (legs, energy) are
/// set by the base constructor and the subclass's own field appends after the
/// base prefix. 4 + 10 + 3 = 17.
#[test]
fn super_init_chains_construction() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "super_init_chains"), 17);
}

/// `super().method(...)` dispatches to the base implementation even when the
/// subclass overrides it: Dog's speak_like_parent() gets Animal's 1.
#[test]
fn super_method_call_dispatches_to_base() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "super_method_call"), 1);
}

/// A two-level hierarchy chains construction through both bases:
/// Puppy -> Dog -> Animal. 4 + 10 = 14.
#[test]
fn two_level_inheritance_chain() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "two_level_chain"), 14);
}

/// `isinstance` is true for the instance's own class and every ancestor, and
/// false for an unrelated subclass: 1 + 10 + 100 = 111.
#[test]
fn isinstance_across_two_level_hierarchy() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "isinstance_across_hierarchy"), 111);
}

/// `issubclass` folds to a compile-time constant over the declared hierarchy:
/// transitive (Puppy <= Animal) and reflexive (Dog <= Dog) are true, the
/// reverse direction is false. 1 + 10 = 11.
#[test]
fn issubclass_folds_at_compile_time() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "issubclass_checks"), 11);
}

/// `isinstance` consults the runtime class-id tag, not the static type: a
/// factory annotated `-> Animal` that actually returns a Dog still answers
/// `isinstance(a, Dog)` with True.
#[test]
fn isinstance_uses_runtime_tag_not_static_type() {
    let src = read_example("oop_inheritance.py");
    assert_eq!(call_i32(&src, "runtime_type_check"), 1);
}

/// Multiple inheritance is rejected with a compile error rather than silently
/// compiling a class with a broken field layout.
#[test]
fn multiple_inheritance_is_rejected() {
    let src = "class A:\n    def ping(self) -> int:\n        return 1\n\nclass B:\n    def pong(self) -> int:\n        return 2\n\nclass C(A, B):\n    def both(self) -> int:\n        return 3\n";
    let err = try_compile(src).expect_err("multiple inheritance must not compile");
    assert!(
        err.contains("single inheritance"),
        "unexpected error message: {err}"
    );
}
