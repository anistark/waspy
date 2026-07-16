//! Coverage audit for v0.20.0: every bundled `examples/*.py` (and the
//! multi-file `examples/user_modules_app/`) has at least one test that
//! compiles it, instantiates the WASM, and asserts concrete runtime results.
//!
//! The broad compile+instantiate sweep lives in `examples.rs`; this file adds
//! the *asserted results* tier for the examples that sweep alone covered.
//! Examples already asserted in `examples.rs` (loop_control, oop_*,
//! comprehensions, closures, generators, …) are not repeated here.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{
    call_f64, call_host_fs_i32, call_i32, call_i32_1, call_i32_2, call_instance_i32,
    call_untyped_f64, call_untyped_i32, examples_dir, instantiate_file, instantiate_with_host_fs,
    read_example,
};
use wasmi::Value;

// ---------------------------------------------------------------------------
// examples/basic_operations.py
// ---------------------------------------------------------------------------

/// The four arithmetic helpers compute real results through exported,
/// argument-taking functions.
#[test]
fn basic_operations_arithmetic() {
    let src = read_example("basic_operations.py");
    assert_eq!(call_i32_2(&src, "add", 2, 3), 5);
    assert_eq!(call_i32_2(&src, "subtract", 10, 4), 6);
    assert_eq!(call_i32_2(&src, "multiply", 6, 7), 42);
    assert_eq!(call_i32_2(&src, "divide", 20, 3), 6);
    assert_eq!(call_i32_2(&src, "modulo", 20, 3), 2);
}

/// Division and modulo guard against a zero divisor and return 0 instead of
/// trapping.
#[test]
fn basic_operations_zero_divisor_guard() {
    let src = read_example("basic_operations.py");
    assert_eq!(call_i32_2(&src, "divide", 5, 0), 0);
    assert_eq!(call_i32_2(&src, "modulo", 5, 0), 0);
}

/// Compiled functions call each other: add(multiply(a, b), subtract(a, b))
/// with (5, 3) is 15 + 2 = 17.
#[test]
fn basic_operations_composed_calls() {
    let src = read_example("basic_operations.py");
    assert_eq!(call_i32_2(&src, "combined_operation", 5, 3), 17);
}

// ---------------------------------------------------------------------------
// examples/control_flow.py
// ---------------------------------------------------------------------------

/// while-loop accumulation: factorial(5) = 120, count_up_to(10) = 55.
#[test]
fn control_flow_while_loops() {
    let src = read_example("control_flow.py");
    assert_eq!(call_i32_1(&src, "factorial", 5), 120);
    assert_eq!(call_i32_1(&src, "factorial", 0), 1);
    assert_eq!(call_i32_1(&src, "count_up_to", 10), 55);
}

/// if/else with an early return (base case) and loop-carried state.
#[test]
fn control_flow_fibonacci() {
    let src = read_example("control_flow.py");
    assert_eq!(call_i32_1(&src, "fibonacci", 0), 0);
    assert_eq!(call_i32_1(&src, "fibonacci", 1), 1);
    assert_eq!(call_i32_1(&src, "fibonacci", 10), 55);
}

/// Branch selection and a bool-returning comparison.
#[test]
fn control_flow_branches_and_bool() {
    let src = read_example("control_flow.py");
    assert_eq!(call_i32_2(&src, "max_num", 3, 9), 9);
    assert_eq!(call_i32_2(&src, "max_num", 9, 3), 9);
    assert_eq!(call_i32_1(&src, "is_even", 4), 1);
    assert_eq!(call_i32_1(&src, "is_even", 7), 0);
}

// ---------------------------------------------------------------------------
// examples/builtins.py
// ---------------------------------------------------------------------------

/// len() over a string and a list, min()/max() reduction, sum() over a list,
/// and the int()/float()/bool() conversions.
#[test]
fn builtins_compute_results() {
    let src = read_example("builtins.py");
    assert_eq!(call_i32(&src, "test_len"), 5);
    assert_eq!(call_i32(&src, "test_len_list"), 5);
    assert_eq!(call_i32(&src, "test_min_max"), 1);
    assert_eq!(call_i32(&src, "test_sum"), 15);
    // int(3.7) + int(float(2)) + (1 if bool(5)) = 3 + 2 + 1.
    assert_eq!(call_i32(&src, "test_conversions"), 6);
}

// ---------------------------------------------------------------------------
// examples/typed_demo.py
// ---------------------------------------------------------------------------

/// Typed integer and comparison functions.
#[test]
fn typed_demo_int_functions() {
    let src = read_example("typed_demo.py");
    assert_eq!(call_i32_2(&src, "add_integers", 2, 3), 5);
    assert_eq!(call_i32_2(&src, "comparisons", 5, 3), 1);
    assert_eq!(call_i32_2(&src, "comparisons", 3, 5), 0);
}

/// Float parameters and results through the untyped call path.
#[test]
fn typed_demo_float_functions() {
    let src = read_example("typed_demo.py");
    let sum = call_untyped_f64(
        &src,
        "add_floats",
        &[Value::F64(1.5.into()), Value::F64(2.25.into())],
    );
    assert_eq!(sum, 3.75);
    // The int argument widens to f64 before the addition.
    let mixed = call_untyped_f64(
        &src,
        "mixed_types",
        &[Value::I32(2), Value::F64(1.5.into())],
    );
    assert_eq!(mixed, 3.5);
}

/// Explicit conversions truncate (float -> int) and widen (int -> float).
#[test]
fn typed_demo_conversions() {
    let src = read_example("typed_demo.py");
    assert_eq!(
        call_untyped_i32(&src, "float_to_int", &[Value::F64(3.7.into())]),
        3
    );
    assert_eq!(
        call_untyped_f64(&src, "int_to_float", &[Value::I32(2)]),
        2.0
    );
}

/// bool logic ((a and b) or (not a)) over i32-encoded booleans.
#[test]
fn typed_demo_bool_operations() {
    let src = read_example("typed_demo.py");
    let f =
        |a: i32, b: i32| call_untyped_i32(&src, "bool_operations", &[Value::I32(a), Value::I32(b)]);
    assert_eq!(f(1, 1), 1); // a and b
    assert_eq!(f(0, 1), 1); // not a
    assert_eq!(f(1, 0), 0); // neither
}

/// A while-loop float power function: 2.0 ** 10 = 1024.0.
#[test]
fn typed_demo_power_calculation() {
    let src = read_example("typed_demo.py");
    let result = call_untyped_f64(
        &src,
        "power_calculation",
        &[Value::F64(2.0.into()), Value::I32(10)],
    );
    assert_eq!(result, 1024.0);
}

/// A module-level float constant feeds the circle-area computation. (The
/// example intentionally defines PI as the literal 3.14159, so the expected
/// value mirrors it — not a stand-in for f64::consts::PI.)
#[test]
#[allow(clippy::approx_constant)]
fn typed_demo_module_constant_in_math() {
    let src = read_example("typed_demo.py");
    let area = call_untyped_f64(&src, "calculate_circle_area", &[Value::F64(2.0.into())]);
    assert_eq!(area, 3.14159_f64 * 2.0 * 2.0);
}

// ---------------------------------------------------------------------------
// examples/module_level_demo.py
// ---------------------------------------------------------------------------

/// Module-level constants read back through functions: PI as an f64, DEBUG as
/// a bool. (The example defines PI = 3.14159 literally.)
#[test]
#[allow(clippy::approx_constant)]
fn module_level_constants() {
    let src = read_example("module_level_demo.py");
    assert_eq!(call_f64(&src, "get_pi"), 3.14159);
    assert_eq!(call_i32(&src, "is_debug_mode"), 0);
}

/// A module-level constant participates in float arithmetic with a parameter.
/// (The example defines PI = 3.14159 literally.)
#[test]
#[allow(clippy::approx_constant)]
fn module_level_constant_arithmetic() {
    let src = read_example("module_level_demo.py");
    let area = call_untyped_f64(&src, "calculate_circle_area", &[Value::F64(1.5.into())]);
    assert_eq!(area, 3.14159_f64 * 1.5 * 1.5);
}

// ---------------------------------------------------------------------------
// examples/list_float.py
// ---------------------------------------------------------------------------

/// Float collection slots hold full-width f64 values (60.875 and 2.75 are
/// exactly representable, so equality is exact).
#[test]
fn list_float_roundtrips() {
    let src = read_example("list_float.py");
    assert_eq!(call_f64(&src, "midpoint"), 2.5);
    assert_eq!(call_f64(&src, "total"), 60.875);
    assert_eq!(call_f64(&src, "pair_second"), 2.75);
    assert_eq!(call_i32(&src, "int_list_index"), 20);
}

// ---------------------------------------------------------------------------
// examples/exceptions.py
// ---------------------------------------------------------------------------

/// The try body's float division returns through except/finally scaffolding.
#[test]
fn exceptions_divide_returns_through_try() {
    let src = read_example("exceptions.py");
    let result = call_untyped_f64(
        &src,
        "divide",
        &[Value::F64(10.0.into()), Value::F64(4.0.into())],
    );
    assert_eq!(result, 2.5);
}

// ---------------------------------------------------------------------------
// examples/range_example.py
// ---------------------------------------------------------------------------

/// All three range() forms drive a for loop: one-arg, three-arg with step,
/// and descending.
#[test]
fn range_example_sums() {
    let src = read_example("range_example.py");
    assert_eq!(call_i32(&src, "sum_range"), 10);
    assert_eq!(call_i32(&src, "sum_with_step"), 20);
    assert_eq!(call_i32(&src, "sum_descending"), 55);
}

// ---------------------------------------------------------------------------
// examples/set_example.py
// ---------------------------------------------------------------------------

/// Sets dedup at construction and answer membership through the hash table.
#[test]
fn set_example_dedup_and_membership() {
    let src = read_example("set_example.py");
    assert_eq!(call_i32(&src, "dedup_size"), 3);
    assert_eq!(call_i32(&src, "membership"), 1);
}

// ---------------------------------------------------------------------------
// examples/tuple_example.py
// ---------------------------------------------------------------------------

/// Tuple literals index positionally, including the single-element form.
#[test]
fn tuple_example_indexing() {
    let src = read_example("tuple_example.py");
    assert_eq!(call_i32(&src, "tuple_sum"), 6);
    assert_eq!(call_i32(&src, "single_element"), 99);
}

// ---------------------------------------------------------------------------
// examples/bytes_example.py
// ---------------------------------------------------------------------------

/// Bytes indexing, slicing, and concatenation produce real values.
#[test]
fn bytes_example_operations() {
    let src = read_example("bytes_example.py");
    assert_eq!(call_i32(&src, "first_byte"), 104); // ord('h')
    assert_eq!(call_i32(&src, "slice_length"), 3);
    assert_eq!(call_i32(&src, "concat_length"), 10);
}

// ---------------------------------------------------------------------------
// stdlib examples (compile-time shims)
// ---------------------------------------------------------------------------

/// math.pi flows out of the shim as a full-precision f64.
#[test]
fn stdlib_math_pi() {
    let src = read_example("test_math.py");
    assert_eq!(call_f64(&src, "test_math"), std::f64::consts::PI);
}

/// sys.maxsize is the compile target's i32::MAX.
#[test]
fn stdlib_sys_maxsize() {
    let src = read_example("test_sys.py");
    assert_eq!(call_i32(&src, "test_sys"), i32::MAX);
}

/// re flag constants carry Python's values (IGNORECASE = 2).
#[test]
fn stdlib_re_flags() {
    let src = read_example("test_re.py");
    assert_eq!(call_i32(&src, "test_re"), 2);
}

/// datetime.MAXYEAR matches Python's 9999.
#[test]
fn stdlib_datetime_constants() {
    let src = read_example("test_datetime.py");
    assert_eq!(call_i32(&src, "test_datetime_constants"), 9999);
}

/// The json, logging, and os example suites run their exported entry points
/// to completion (each returns 0 on success).
#[test]
fn stdlib_json_logging_os_run() {
    let json = read_example("test_json.py");
    assert_eq!(call_i32(&json, "test_json_dumps"), 0);
    assert_eq!(call_i32(&json, "test_json_loads"), 0);
    let logging = read_example("test_logging.py");
    assert_eq!(call_i32(&logging, "main"), 0);
    let os = read_example("test_os.py");
    assert_eq!(call_i32(&os, "test_all_os"), 0);
}

/// The all-modules import smoke tests still compute a real value.
#[test]
fn stdlib_all_modules_import() {
    let all = read_example("stdlib_all_modules.py");
    assert_eq!(call_i32(&all, "test_sys_module"), i32::MAX);
    let imports = read_example("test_all_stdlib_imports.py");
    assert_eq!(call_i32(&imports, "test_imports"), i32::MAX);
}

// ---------------------------------------------------------------------------
// examples/algorithms.py
// ---------------------------------------------------------------------------

/// Integer algorithms: Euclid's gcd, trial-division primality, digit
/// manipulation, and Collatz step counting.
#[test]
fn algorithms_integer_math() {
    let src = read_example("algorithms.py");
    assert_eq!(call_i32_2(&src, "gcd", 48, 36), 12);
    assert_eq!(call_i32_2(&src, "gcd", 17, 5), 1);
    assert_eq!(call_i32_1(&src, "is_prime", 97), 1);
    assert_eq!(call_i32_1(&src, "is_prime", 91), 0); // 7 * 13
    assert_eq!(call_i32_1(&src, "digit_sum", 4921), 16);
    assert_eq!(call_i32_1(&src, "reverse_number", 1234), 4321);
    assert_eq!(call_i32_1(&src, "collatz_steps", 6), 8);
}

/// Newton's method converges to sqrt(2) well past f32 precision.
#[test]
fn algorithms_newton_sqrt() {
    let src = read_example("algorithms.py");
    let root = call_untyped_f64(&src, "approximate_sqrt", &[Value::F64(2.0.into())]);
    assert!((root - std::f64::consts::SQRT_2).abs() < 1e-12);
}

// ---------------------------------------------------------------------------
// examples/calculator_project/ (project-directory compilation)
// ---------------------------------------------------------------------------

/// The bundled project directory compiles config-aware (setup.py is read for
/// metadata, not compiled) and cross-file calls work: net_price(200, 21) =
/// 200 + 42, average(3, 4, 8) = 5.
#[test]
fn calculator_project_compiles_and_runs() {
    let dir = examples_dir().join("calculator_project");
    let wasm = waspy::compile_python_project(&dir, false).expect("project compilation");
    let (instance, mut store) = harness::instantiate_wasm(&wasm);
    let net = instance
        .get_typed_func::<(i32, i32), i32>(&store, "net_price")
        .expect("exported net_price");
    assert_eq!(net.call(&mut store, (200, 21)).expect("call"), 242);
    let avg = instance
        .get_typed_func::<(i32, i32, i32), i32>(&store, "average")
        .expect("exported average");
    assert_eq!(avg.call(&mut store, (3, 4, 8)).expect("call"), 5);
}

// ---------------------------------------------------------------------------
// examples/user_modules_app/ (multi-file, compiled from disk)
// ---------------------------------------------------------------------------

/// The bundled multi-file app compiles from its entry file with imports
/// resolved from disk, and every cross-module path computes 42: namespace
/// calls, aliased from-imports, module constants, and a class imported by a
/// module that itself imports another module.
#[test]
fn user_modules_app_compiles_from_disk() {
    let entry = examples_dir().join("user_modules_app").join("main.py");
    let (instance, mut store) = instantiate_file(&entry);
    assert_eq!(call_instance_i32(&instance, &mut store, "combined"), 42);
    assert_eq!(call_instance_i32(&instance, &mut store, "aliased"), 42);
    assert_eq!(call_instance_i32(&instance, &mut store, "scaled"), 42);
    assert_eq!(call_instance_i32(&instance, &mut store, "rect_area"), 42);
}

// ---------------------------------------------------------------------------
// examples/file_io.py (driven against the in-memory reference host)
// ---------------------------------------------------------------------------

/// The file I/O example runs end to end against the reference host: the
/// write is visible to the read, `with open(...)` copies through a second
/// file, and append mode extends the original ("hello from waspy" is 16
/// bytes, "\nsecond line" adds 12).
#[test]
fn file_io_example_round_trips() {
    let src = read_example("file_io.py");
    let (instance, mut store) = instantiate_with_host_fs(&src);
    assert_eq!(
        call_host_fs_i32(&instance, &mut store, "write_greeting"),
        16
    );
    assert_eq!(call_host_fs_i32(&instance, &mut store, "read_greeting"), 16);
    assert_eq!(
        call_host_fs_i32(&instance, &mut store, "copy_with_context_managers"),
        16
    );
    assert_eq!(call_host_fs_i32(&instance, &mut store, "append_line"), 28);
    assert_eq!(
        store.data().files.get("greeting.txt").map(|c| c.as_slice()),
        Some("hello from waspy\nsecond line".as_bytes())
    );
}
