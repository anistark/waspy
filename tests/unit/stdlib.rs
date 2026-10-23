//! The standard library's functions on runtime values.
//!
//! Expected values are CPython 3.13's for the same source. The `math`
//! routines are also checked against the `libm` crate (musl's libm, the
//! reference platform), bit for bit, over inputs chosen to reach every branch
//! of each routine.

#[path = "../utils/harness.rs"]
mod harness;

#[allow(unused_imports)]
use harness::*;
use wasmi::Value;

/// Run a program's `__main__` block and compare its standard output.
fn prints(source: &str, want: &str) {
    let run = run_main(&dedent(source), "", &[]);
    assert_eq!(run.stderr, "", "stderr");
    assert_eq!(run.stdout, want);
}

/// Run a program's `__main__` block and compare both of its output streams.
fn streams(source: &str, stdout: &str, stderr: &str) {
    let run = run_main(&dedent(source), "", &[]);
    assert_eq!(run.stdout, stdout, "stdout");
    assert_eq!(run.stderr, stderr, "stderr");
}

/// A deterministic stream of f64s covering every binade, the values near
/// multiples of pi/2, the specials, and the subnormals.
fn inputs() -> Vec<f64> {
    let mut out = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        -0.5,
        2.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::MIN_POSITIVE,
        f64::MAX,
        -f64::MAX,
        5e-324,
        -5e-324,
        1e-310,
        std::f64::consts::PI,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::FRAC_PI_4,
        1e22,
        1e300,
        709.78,
        710.0,
        -745.0,
        -746.0,
        0.9999999999999999,
        1.0000000000000002,
    ];
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..4000 {
        // Any bit pattern.
        out.push(f64::from_bits(next()));
    }
    for _ in 0..4000 {
        // Ordinary magnitudes, either sign.
        let e = (next() % 80) as i32 - 40;
        let m = (next() >> 11) as f64 / (1u64 << 53) as f64;
        let sign = if next() & 1 == 0 { 1.0 } else { -1.0 };
        out.push(sign * m * 2f64.powi(e));
    }
    for k in 1..400 {
        // Near k * pi/2, and up to the huge arguments.
        let base = k as f64 * std::f64::consts::FRAC_PI_2;
        out.push(base);
        out.push(f64::from_bits(base.to_bits() + 1));
        out.push(f64::from_bits(base.to_bits() - 1));
        out.push(base * 2f64.powi((k % 60) * 17));
    }
    for _ in 0..500 {
        // Near 1, where log and pow are delicate.
        out.push(1.0 + ((next() % 2000) as f64 - 1000.0) * f64::EPSILON);
    }
    out
}

/// Call the export `name` with the float arguments, `None` if it traps (an
/// exception nothing catches).
fn call(
    instance: &wasmi::Instance,
    store: &mut wasmi::Store<Host>,
    name: &str,
    args: &[f64],
) -> Option<f64> {
    let f = instance.get_func(&*store, name).expect("export");
    let args: Vec<Value> = args.iter().map(|v| Value::F64((*v).into())).collect();
    let mut results = [Value::F64(0.0.into())];
    f.call(&mut *store, &args, &mut results).ok()?;
    match results[0] {
        Value::F64(v) => Some(f64::from(v)),
        ref other => panic!("expected f64, got {other:?}"),
    }
}

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

#[test]
fn math_routines_match_libm_bit_for_bit() {
    let source = dedent(
        r#"
        import math

        def sin(x: float) -> float:
            return math.sin(x)

        def cos(x: float) -> float:
            return math.cos(x)

        def tan(x: float) -> float:
            return math.tan(x)

        def asin(x: float) -> float:
            return math.asin(x)

        def acos(x: float) -> float:
            return math.acos(x)

        def atan(x: float) -> float:
            return math.atan(x)

        def sinh(x: float) -> float:
            return math.sinh(x)

        def cosh(x: float) -> float:
            return math.cosh(x)

        def tanh(x: float) -> float:
            return math.tanh(x)

        def exp(x: float) -> float:
            return math.exp(x)

        def log(x: float) -> float:
            return math.log(x)

        def log10(x: float) -> float:
            return math.log10(x)

        def log2(x: float) -> float:
            return math.log2(x)

        def sqrt(x: float) -> float:
            return math.sqrt(x)

        def atan2(y: float, x: float) -> float:
            return math.atan2(y, x)

        def pow(x: float, y: float) -> float:
            return math.pow(x, y)

        def fmod(x: float, y: float) -> float:
            return math.fmod(x, y)
    "#,
    );
    let (instance, mut store) = instantiate(&source);
    let xs = inputs();
    type F1 = fn(f64) -> f64;
    let unary: &[(&str, F1)] = &[
        ("sin", libm::sin),
        ("cos", libm::cos),
        ("tan", libm::tan),
        ("asin", libm::asin),
        ("acos", libm::acos),
        ("atan", libm::atan),
        ("sinh", libm::sinh),
        ("cosh", libm::cosh),
        ("tanh", libm::tanh),
        ("exp", libm::exp),
        ("log", libm::log),
        ("log10", libm::log10),
        ("log2", libm::log2),
        ("sqrt", libm::sqrt),
    ];
    for (name, reference) in unary {
        for &x in &xs {
            let want = reference(x);
            let got = call(&instance, &mut store, name, &[x]);
            // CPython raises where libm answers NaN from a number, or an
            // infinity from a finite one; and where log meets zero.
            let raises = (want.is_nan() && !x.is_nan())
                || (want.is_infinite() && x.is_finite())
                || (name.starts_with("log") && x <= 0.0);
            match got {
                None => assert!(raises, "{name}({x:e}) trapped; libm: {want:e}"),
                Some(got) => {
                    assert!(!raises, "{name}({x:e}) = {got:e} should raise");
                    assert!(
                        same(got, want),
                        "{name}({x:e} = {:#x}): {got:e} ({:#x}), libm {want:e} ({:#x})",
                        x.to_bits(),
                        got.to_bits(),
                        want.to_bits()
                    );
                }
            }
        }
    }
    type F2 = fn(f64, f64) -> f64;
    let binary: &[(&str, F2)] = &[
        ("atan2", libm::atan2),
        ("pow", libm::pow),
        ("fmod", libm::fmod),
    ];
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for (i, &x) in xs.iter().enumerate().step_by(7) {
        for &y in xs.iter().skip(i % 13).step_by(97) {
            pairs.push((x, y));
        }
    }
    // pow's own special cases: integral, half-integral, and huge exponents.
    for &x in &[
        -8.0, -2.0, -1.0, -0.5, 0.0, -0.0, 0.5, 2.0, 10.0, 1.0000001, 0.9999999,
    ] {
        for &y in &[
            -3.0, -2.0, -1.0, -0.5, 0.5, 1.0, 2.0, 3.0, 1e10, -1e10, 1e300, 0.3, -0.3,
        ] {
            pairs.push((x, y));
        }
    }
    for (name, reference) in binary {
        for &(x, y) in &pairs {
            let want = reference(x, y);
            let got = call(&instance, &mut store, name, &[x, y]);
            let finite_args = x.is_finite() && y.is_finite();
            let raises = match *name {
                "pow" => finite_args && !want.is_finite(),
                "fmod" => want.is_nan() && !x.is_nan() && !y.is_nan(),
                _ => false,
            };
            // CPython's fmod(x, ±inf) is x for finite x.
            let want = if *name == "fmod" && y.is_infinite() && x.is_finite() {
                x
            } else {
                want
            };
            match got {
                None => assert!(raises, "{name}({x:e}, {y:e}) trapped; libm: {want:e}"),
                Some(got) => {
                    assert!(!raises, "{name}({x:e}, {y:e}) = {got:e} should raise");
                    assert!(
                        same(got, want),
                        "{name}({x:e}, {y:e}): {got:e} ({:#x}), libm {want:e} ({:#x})",
                        got.to_bits(),
                        want.to_bits()
                    );
                }
            }
        }
    }
}

#[test]
fn math_functions_answer_as_cpython() {
    prints(
        r#"
        import math
        from math import sqrt, hypot as h


        def domain(x: float) -> str:
            try:
                return str(math.sqrt(x))
            except ValueError as e:
                return "ValueError: " + str(e)


        def overflow(x: float) -> str:
            try:
                return str(math.exp(x))
            except OverflowError as e:
                return "OverflowError: " + str(e)


        def to_int(x: float) -> str:
            try:
                return str(math.floor(x))
            except ValueError as e:
                return "ValueError: " + str(e)
            except OverflowError as e:
                return "OverflowError: " + str(e)


        def logs(x: float, b: float) -> str:
            try:
                return str(math.log(x, b))
            except ValueError as e:
                return "ValueError: " + str(e)
            except ZeroDivisionError as e:
                return "ZeroDivisionError: " + str(e)


        def main() -> None:
            print(math.sin(1.0), math.cos(2), math.tan(0.5), math.atan2(1, -1))
            print(math.asin(0.4), math.acos(-0.3), math.atan(7))
            print(math.sinh(1.5), math.cosh(-2.0), math.tanh(0.4))
            print(math.exp(2), math.log(10), math.log10(1000), math.log2(1024), math.log(8, 2))
            print(math.pow(2, 0.5), math.pow(-2, 3), sqrt(2), h(3, 4), math.hypot(1, 2, 3))
            print(math.floor(-2.5), math.ceil(2.1), math.trunc(-7.9), math.floor(7))
            print(math.fabs(-3), math.copysign(2, -0.0), math.fmod(-7, 3), math.remainder(5, 3))
            print(math.degrees(math.pi), math.radians(180))
            print(math.factorial(10), math.factorial(0), math.gcd(12, 18, -8), math.gcd())
            print(math.isnan(math.nan), math.isinf(-math.inf), math.isfinite(1e308), math.isfinite(math.inf))
            print(math.sin(1e22), math.cos(1e300), math.tan(100.0))
            print(domain(-1.0), domain(16.0), overflow(1000.0), overflow(2.0))
            print(to_int(math.nan), to_int(math.inf), to_int(-1.5))
            print(logs(8.0, 1.0), logs(0.0, 2.0), logs(100.0, 10.0))
            total = 0.0
            for i in range(1, 6):
                total += math.sqrt(i) * math.cos(i)
            print(total)


        if __name__ == "__main__":
            main()
    "#,
        "0.8414709848078965 -0.4161468365471424 0.5463024898437905 2.356194490192345\n0.41151684606748806 1.8754889808102941 1.4288992721907328\n2.1292794550948173 3.7621956910836314 0.3799489622552249\n7.38905609893065 2.302585092994046 3.0 10.0 3.0\n1.4142135623730951 -8.0 1.4142135623730951 5.0 3.7416573867739413\n-3 3 -7 7\n3.0 -2.0 -1.0 -1.0\n180.0 3.141592653589793\n3628800 1 2 0\nTrue True True False\n-0.8522008497671888 -0.5753861119575491 -0.5872139151569291\nValueError: math domain error 4.0 OverflowError: math range error 7.38905609893065\nValueError: cannot convert float NaN to integer OverflowError: cannot convert float infinity to integer -2\nZeroDivisionError: float division by zero ValueError: math domain error 2.0\n-2.4359348099247162\n",
    );
}

/// math's two-argument functions over CPython's special values, the int
/// functions, and every error a math function raises, caught.
#[test]
fn math_errors_and_special_values() {
    prints(
        r#"
        import math


        def attempt(k: int, x: float, y: float) -> str:
            try:
                if k == 0:
                    return repr(math.pow(x, y))
                if k == 1:
                    return repr(math.fmod(x, y))
                if k == 2:
                    return repr(math.remainder(x, y))
                if k == 3:
                    return repr(math.atan2(x, y))
                if k == 4:
                    return repr(math.hypot(x, y))
                if k == 5:
                    return repr(math.copysign(x, y))
                return repr(math.log(x, y))
            except ValueError as e:
                return "ValueError(" + str(e) + ")"
            except OverflowError as e:
                return "OverflowError(" + str(e) + ")"
            except ZeroDivisionError as e:
                return "ZeroDivisionError(" + str(e) + ")"


        def ints(n: int) -> str:
            try:
                return str(math.factorial(n)) + " " + str(math.gcd(n, 12)) + " " + str(math.gcd(n, -18, 27))
            except ValueError as e:
                return "ValueError(" + str(e) + ")"


        def one(k: int, x: float) -> str:
            try:
                if k == 0:
                    return repr(math.acos(x))
                if k == 1:
                    return repr(math.cosh(x))
                if k == 2:
                    return repr(math.log10(x))
                if k == 3:
                    return repr(math.sin(x))
                if k == 4:
                    return repr(math.ceil(x))
                return repr(math.sqrt(x))
            except ValueError as e:
                return "ValueError(" + str(e) + ")"
            except OverflowError as e:
                return "OverflowError(" + str(e) + ")"


        def main() -> None:
            inf = math.inf
            nan = math.nan
            pairs = [(0.0, -1.0), (10.0, 400.0), (-2.0, 0.5), (-2.0, 3.0), (0.0, 0.0), (inf, -1.0),
                     (-inf, 3.0), (1.0, nan), (nan, 0.0), (0.0, -inf), (-0.0, -1.0), (1e300, -2.0),
                     (-7.0, 3.0), (7.0, inf), (inf, inf), (nan, 0.0), (100.0, 10.0), (2.5, 1.0),
                     (3.5, 1.0), (-7.5, 5.0), (inf, 1.0), (1.0, inf), (1.0, 0.0), (8.0, 2.0), (8.0, 1.0),
                     (8.0, 0.0), (0.5, 2.0), (1e308, 1e308), (3e-320, 4e-320), (-0.0, -1.0)]
            for k in range(7):
                line = ""
                for p in pairs:
                    line += attempt(k, p[0], p[1]) + " "
                print(line)
            for n in [0, 1, 5, 12, -1, -12]:
                print(ints(n))
            for k in range(6):
                line = ""
                for x in [2.0, -2.0, inf, -inf, nan, 0.0, -0.0, 1000.0, 0.6, 1e-310]:
                    line += one(k, x) + " "
                print(line)
            print(math.gcd(-2147483647, 0), math.factorial(12), math.floor(True), math.trunc(-3))
            print(math.hypot(), math.hypot(-3.0), math.hypot(1.0, 1.0, 1.0, 1.0), math.hypot(math.hypot(3, 4), 12))
            print(math.isnan(3), math.isinf(3), math.isfinite(True), math.fabs(-0.0))
            print(math.degrees(1.0), math.radians(-90))


        if __name__ == "__main__":
            main()
    "#,
        "ValueError(math domain error) OverflowError(math range error) ValueError(math domain error) -8.0 1.0 0.0 -inf 1.0 1.0 inf ValueError(math domain error) 0.0 -343.0 inf inf 1.0 1e+20 2.5 3.5 -23730.46875 inf 1.0 1.0 64.0 8.0 1.0 0.25 OverflowError(math range error) 1.0 ValueError(math domain error) \n0.0 10.0 -0.0 -2.0 ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) nan nan 0.0 -0.0 0.0 -1.0 7.0 ValueError(math domain error) nan 0.0 0.5 0.5 -2.5 ValueError(math domain error) 1.0 ValueError(math domain error) 0.0 0.0 ValueError(math domain error) 0.5 0.0 3e-320 -0.0 \n0.0 10.0 -0.0 1.0 ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) nan nan 0.0 -0.0 0.0 -1.0 7.0 ValueError(math domain error) nan 0.0 0.5 -0.5 2.5 ValueError(math domain error) 1.0 ValueError(math domain error) 0.0 0.0 ValueError(math domain error) 0.5 0.0 -1e-320 -0.0 \n3.141592653589793 0.02499479361892016 -1.3258176636680326 -0.5880026035475675 0.0 1.5707963267948966 -1.5707963267948966 nan nan 3.141592653589793 -3.141592653589793 1.5707963267948966 -1.1659045405098132 0.0 0.7853981633974483 nan 1.4711276743037347 1.1902899496825317 1.2924966677897853 -0.982793723247329 1.5707963267948966 0.0 1.5707963267948966 1.3258176636680326 1.446441332248135 1.5707963267948966 0.24497866312686414 0.7853981633974483 0.6435011087932844 -3.141592653589793 \n1.0 400.12498047485116 2.0615528128088303 3.605551275463989 0.0 inf inf nan nan inf 1.0 1e+300 7.615773105863909 inf inf nan 100.4987562112089 2.692582403567252 3.640054944640259 9.013878188659973 inf inf 1.0 8.246211251235321 8.06225774829855 8.0 2.0615528128088303 1.4142135623730951e+308 5e-320 1.0 \n-0.0 10.0 2.0 2.0 0.0 -inf inf 1.0 nan -0.0 -0.0 -1e+300 7.0 7.0 inf nan 100.0 2.5 3.5 7.5 inf 1.0 1.0 8.0 8.0 8.0 0.5 1e+308 3e-320 -0.0 \nValueError(math domain error) 0.3843108934201205 ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) nan ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) 0.0 nan ValueError(math domain error) 2.0 ZeroDivisionError(float division by zero) ZeroDivisionError(float division by zero) ValueError(math domain error) ZeroDivisionError(float division by zero) 0.0 ValueError(math domain error) 3.0 ZeroDivisionError(float division by zero) ValueError(math domain error) -1.0 1.0 1.0003911695069592 ValueError(math domain error) \n1 12 9\n1 1 1\n120 1 1\n479001600 12 3\nValueError(factorial() not defined for negative values)\nValueError(factorial() not defined for negative values)\nValueError(math domain error) ValueError(math domain error) ValueError(math domain error) ValueError(math domain error) nan 1.5707963267948966 1.5707963267948966 ValueError(math domain error) 0.9272952180016123 1.5707963267948966 \n3.7621956910836314 3.7621956910836314 inf inf nan 1.0 1.0 OverflowError(math range error) 1.1854652182422676 1.0 \n0.3010299956639812 ValueError(math domain error) inf ValueError(math domain error) nan ValueError(math domain error) ValueError(math domain error) 3.0 -0.2218487496163564 -310.0 \n0.9092974268256817 -0.9092974268256817 ValueError(math domain error) ValueError(math domain error) nan 0.0 -0.0 0.8268795405320025 0.5646424733950354 1e-310 \n2 -2 OverflowError(cannot convert float infinity to integer) OverflowError(cannot convert float infinity to integer) ValueError(cannot convert float NaN to integer) 0 0 1000 1 1 \n1.4142135623730951 ValueError(math domain error) inf ValueError(math domain error) nan 0.0 -0.0 31.622776601683793 0.7745966692414834 9.999999999999986e-156 \n2147483647 479001600 1 -3\n0.0 3.0 2.0 13.0\nFalse False True 0.0\n57.29577951308232 -1.5707963267948966\n",
    );
}

/// `**` on floats is CPython's `float_pow` over libm's `pow`: a fractional
/// exponent, the special values, zero to a negative power, and overflow. It
/// used to multiply repeatedly, trapping on a fractional exponent.
#[test]
fn float_power_is_cpythons_float_pow() {
    prints(
        r#"
        def power(x: float, y: float) -> str:
            try:
                return repr(x ** y)
            except ZeroDivisionError as e:
                return "ZeroDivisionError(" + str(e) + ")"
            except OverflowError as e:
                return "OverflowError(" + str(e) + ")"


        def main() -> None:
            inf = float("inf")
            nan = float("nan")
            line = ""
            for p in [(2.0, 0.5), (2.0, 10.0), (1.1, 10.0), (10.0, -2.0), (0.0, -1.0), (0.0, 0.0),
                      (-0.0, 3.0), (-2.0, 3.0), (-2.0, 2.0), (-1.0, 1e300), (1.0, nan), (nan, 0.0),
                      (inf, -2.0), (-inf, 3.0), (-inf, -3.0), (0.5, inf), (2.0, -inf), (10.0, 400.0),
                      (2.0, 1024.0), (2.0, -1080.0), (1e-300, 2.0), (9.0, 0.5), (4.0, 1.5), (3.0, 3.0)]:
                line += power(p[0], p[1]) + " "
            print(line)
            n = 3
            print(n ** 2.0, 2.5 ** n, 2 ** 0.5, 16 ** 0.25)
            x = 2.0
            x **= 3
            print(x)
            try:
                y = 10.0 ** 400.0
                print(y)
            except OverflowError as e:
                print(repr(e), str(e))


        if __name__ == "__main__":
            main()
    "#,
        "1.4142135623730951 1024.0 2.5937424601000023 0.01 ZeroDivisionError(0.0 cannot be raised to a negative power) 1.0 -0.0 -8.0 4.0 1.0 1.0 1.0 0.0 -inf -0.0 0.0 0.0 OverflowError((34, 'Result too large')) OverflowError((34, 'Result too large')) 0.0 0.0 3.0 8.0 27.0 \n9.0 15.625 1.4142135623730951 2.0\n8.0\nOverflowError(34, 'Result too large') (34, 'Result too large')\n",
    );
}

/// random's functions draw what CPython's draw for a seed: int, bool, float, and
/// negative seeds, sequences of each kind, and sample's set path.
#[test]
fn random_functions_answer_as_cpython() {
    prints(
        r#"
        import random


        def main() -> None:
            random.seed(42)
            print(random.random(), random.random(), random.random())
            print(random.getrandbits(5), random.getrandbits(31), random.getrandbits(0))
            print(random.randrange(10), random.randrange(3, 30), random.randrange(0, 100, 7), random.randrange(50, 0, -3))
            print(random.randint(1, 6), random.randint(-5, 5), random.uniform(1.5, 2.5), random.uniform(3, 7))
            xs = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
            random.shuffle(xs)
            print(xs)
            print(random.choice(xs), random.choice("hello"), random.choice((2.5, 3.5, 4.5)))
            print(random.sample(xs, 3), random.sample("abcdefghij", 4), random.sample(xs, 10))
            print(random.gauss(), random.gauss(), random.gauss(10.0, 2.0))
            print(random.normalvariate(), random.normalvariate(5, 0.5), random.expovariate(), random.expovariate(3))
            random.seed(0)
            a = random.random()
            random.seed(-1)
            print(a, random.random())
            random.seed(2147483647)
            print(random.random())
            random.seed(True)
            print(random.random())
            random.seed(3.5)
            print(random.random())
            random.seed(-2.75)
            print(random.random())
            big = []
            for i in range(60):
                big.append(i)
            random.seed(7)
            print(random.sample(big, 25))
            total = 0.0
            h = 0
            for i in range(1000):
                total += random.random()
                h = (h * 31 + random.randrange(1000000)) % 65521
            print(total, h)


        if __name__ == "__main__":
            main()
    "#,
        "0.6394267984578837 0.025010755222666936 0.27502931836911926\n7 299655412 0\n1 24 77 44\n5 1 1.5317826794817835 3.37478095944637\n[3, 6, 8, 10, 7, 2, 5, 1, 9, 4]\n4 l 2.5\n[8, 5, 2] ['e', 'c', 'd', 'g'] [2, 6, 9, 10, 3, 8, 7, 5, 1, 4]\n-0.1574413757701871 -1.2296084991800966 11.922978145060576\n0.5233270166651023 5.735119564613094 0.8612159413488458 0.4064431907492771\n0.8444218515250481 0.13436424411240122\n0.3177580158172969\n0.13436424411240122\n0.3039190124834461\n0.1501979193521198\n[20, 9, 25, 41, 3, 4, 52, 34, 6, 23, 37, 55, 32, 13, 2, 5, 27, 26, 54, 15, 44, 35, 43, 48, 7]\n479.31607044469274 6420\n",
    );
}

/// A thousand draws of each function, for seven seeds, folded into checksums
/// CPython's draws give.
#[test]
fn random_draws_a_thousand_per_function_per_seed() {
    prints(
        r#"
        import random


        def mix(h: int, r: int) -> int:
            return (h * 31 + r % 65521) % 65521


        def draws(seed: int) -> str:
            out = ""
            random.seed(seed)
            total = 0.0
            for i in range(1000):
                total += random.random()
            out += repr(total) + " "
            random.seed(seed)
            h = 0
            for i in range(1000):
                h = mix(h, random.getrandbits(1 + i % 31))
            out += str(h) + " "
            random.seed(seed)
            h = 0
            for i in range(1000):
                h = mix(h, random.randrange(1 + i * 997))
                h = mix(h, random.randrange(-50, 50 + i, 1 + i % 7))
                h = mix(h, random.randint(-i, i))
            out += str(h) + " "
            random.seed(seed)
            total = 0.0
            for i in range(1000):
                total += random.uniform(-2.5, 7.5) + random.gauss(1.0, 3.0)
            out += repr(total) + " "
            random.seed(seed)
            total = 0.0
            for i in range(1000):
                total += random.normalvariate(0.5, 2.0) + random.expovariate(1.5)
            out += repr(total) + " "
            random.seed(seed)
            xs = [i for i in range(50)]
            h = 0
            for i in range(200):
                random.shuffle(xs)
                h = mix(h, xs[0] * 50 + xs[49])
                h = mix(h, random.choice(xs))
                picked = random.sample(xs, i % 40)
                for p in picked:
                    h = mix(h, p)
            out += str(h)
            return out


        def main() -> None:
            for seed in [0, 1, 42, 12345, -7, 2147483647, 99991]:
                print(seed, draws(seed))


        if __name__ == "__main__":
            main()
    "#,
        "0 496.37098080025083 14456 16609 3613.017916598317 1275.7319374716753 5536\n1 514.1370278623858 49051 38184 3255.3439622016404 1149.5824616665516 32271\n42 512.5619702436156 57850 36188 3559.9689246686366 1263.0571314130548 28822\n12345 490.2182367020749 62734 46713 3321.8485249143223 1227.0749961608228 55502\n-7 481.7410797949598 50496 56183 3295.1890941691186 1151.2978570069201 33486\n2147483647 493.74743913627134 26254 59802 3333.4988252868793 1104.2416784265802 9064\n99991 500.73850501045285 61607 65375 3404.7087570315684 1180.481263943397 35249\n",
    );
}

/// Every error random raises, caught, with the generator left where CPython
/// leaves it, and the keyword forms.
#[test]
fn random_errors_and_keywords() {
    prints(
        r#"
        import random
        from random import randint, choice


        def attempt(k: int) -> str:
            empty: list[int] = []
            try:
                if k == 0:
                    return str(random.getrandbits(-1))
                if k == 1:
                    return str(random.randrange(0))
                if k == 2:
                    return str(random.randrange(5, 5))
                if k == 3:
                    return str(random.randrange(1, 10, 0))
                if k == 4:
                    return str(random.randrange(10, 1, 2))
                if k == 5:
                    return str(randint(3, 2))
                if k == 6:
                    return str(choice(empty))
                if k == 7:
                    return str(random.sample([1, 2, 3], 4))
                if k == 8:
                    return str(random.sample([1, 2, 3], -1))
                if k == 9:
                    return str(random.expovariate(0))
                return str(random.randrange(-3))
            except ValueError as e:
                return "ValueError: " + str(e)
            except IndexError as e:
                return "IndexError: " + str(e)
            except ZeroDivisionError as e:
                return "ZeroDivisionError: " + str(e)


        def main() -> None:
            random.seed(5)
            for k in range(11):
                print(attempt(k))
            # The draws an exception interrupted leave the state where CPython's are.
            print(random.random(), random.gauss(mu=2.0), random.gauss(sigma=0.5), random.expovariate(lambd=2.0))
            random.seed(a=11)
            print(random.random(), randint(1, 100), choice(["x", "y", "z"]), random.getrandbits(32) & 65535)


        if __name__ == "__main__":
            main()
    "#,
        "ValueError: number of bits must be non-negative\nValueError: empty range for randrange()\nValueError: empty range in randrange(5, 5)\nValueError: zero step for randrange()\nValueError: empty range in randrange(10, 1, 2)\nValueError: empty range in randrange(3, 3)\nIndexError: Cannot choose from an empty sequence\nValueError: Sample larger than population or is negative\nValueError: Sample larger than population or is negative\nZeroDivisionError: float division by zero\nValueError: empty range for randrange()\n0.7417869892607294 2.66946891438597 -1.1469550473159555 0.6733418134403047\n0.4523795535098186 72 y 18550\n",
    );
}

/// os.path's string functions are posixpath's, over separators, dots, and
/// non-ASCII names; abspath joins onto the WASI working directory, /, as
/// CPython does when run from /.
#[test]
fn os_path_functions_are_posixpaths() {
    prints(
        r#"
        import os
        import os.path
        from os.path import join, splitext


        def main() -> None:
            paths = ["", "/", "//", "///", "a", "a/", "/a", "/a/", "a/b", "a/b/", "/a/b/c.txt", "a//b",
                     "./a", "../a", "a/./b", "a/../b", "../../a", "/../a", "/a/../../b", "a/b/../..",
                     ".", "..", "...", ".bashrc", "a/.bashrc", "a.b.c", "a/b.c/d", "file.", "..tar.gz",
                     "//a//b//", "///a/./b/", "a/b/./../c/", "é/ü.txt", "/x/y/../../../z", "a..b/c"]
            for p in paths:
                root, ext = os.path.splitext(p)
                head, tail = os.path.split(p)
                print(repr(p), repr(os.path.basename(p)), repr(os.path.dirname(p)), repr(head), repr(tail),
                      repr(root), repr(ext), repr(os.path.normpath(p)), repr(os.path.abspath(p)), os.path.isabs(p))
            print(join("a", "b"), join("a/", "b"), join("a", "/b"), join("", "b"), join("a", ""), join("a", "b", "c"),
                  join("/", "x", "/y", "z/"), os.path.join("a"), splitext("x.py")[1])
            print(os.getcwd(), len(os.urandom(16)), os.urandom(0), os.name, os.sep)


        if __name__ == "__main__":
            main()
    "#,
        "'' '' '' '' '' '' '' '.' '/' False\n'/' '' '/' '/' '' '/' '' '/' '/' True\n'//' '' '//' '//' '' '//' '' '//' '//' True\n'///' '' '///' '///' '' '///' '' '/' '/' True\n'a' 'a' '' '' 'a' 'a' '' 'a' '/a' False\n'a/' '' 'a' 'a' '' 'a/' '' 'a' '/a' False\n'/a' 'a' '/' '/' 'a' '/a' '' '/a' '/a' True\n'/a/' '' '/a' '/a' '' '/a/' '' '/a' '/a' True\n'a/b' 'b' 'a' 'a' 'b' 'a/b' '' 'a/b' '/a/b' False\n'a/b/' '' 'a/b' 'a/b' '' 'a/b/' '' 'a/b' '/a/b' False\n'/a/b/c.txt' 'c.txt' '/a/b' '/a/b' 'c.txt' '/a/b/c' '.txt' '/a/b/c.txt' '/a/b/c.txt' True\n'a//b' 'b' 'a' 'a' 'b' 'a//b' '' 'a/b' '/a/b' False\n'./a' 'a' '.' '.' 'a' './a' '' 'a' '/a' False\n'../a' 'a' '..' '..' 'a' '../a' '' '../a' '/a' False\n'a/./b' 'b' 'a/.' 'a/.' 'b' 'a/./b' '' 'a/b' '/a/b' False\n'a/../b' 'b' 'a/..' 'a/..' 'b' 'a/../b' '' 'b' '/b' False\n'../../a' 'a' '../..' '../..' 'a' '../../a' '' '../../a' '/a' False\n'/../a' 'a' '/..' '/..' 'a' '/../a' '' '/a' '/a' True\n'/a/../../b' 'b' '/a/../..' '/a/../..' 'b' '/a/../../b' '' '/b' '/b' True\n'a/b/../..' '..' 'a/b/..' 'a/b/..' '..' 'a/b/../..' '' '.' '/' False\n'.' '.' '' '' '.' '.' '' '.' '/' False\n'..' '..' '' '' '..' '..' '' '..' '/' False\n'...' '...' '' '' '...' '...' '' '...' '/...' False\n'.bashrc' '.bashrc' '' '' '.bashrc' '.bashrc' '' '.bashrc' '/.bashrc' False\n'a/.bashrc' '.bashrc' 'a' 'a' '.bashrc' 'a/.bashrc' '' 'a/.bashrc' '/a/.bashrc' False\n'a.b.c' 'a.b.c' '' '' 'a.b.c' 'a.b' '.c' 'a.b.c' '/a.b.c' False\n'a/b.c/d' 'd' 'a/b.c' 'a/b.c' 'd' 'a/b.c/d' '' 'a/b.c/d' '/a/b.c/d' False\n'file.' 'file.' '' '' 'file.' 'file' '.' 'file.' '/file.' False\n'..tar.gz' '..tar.gz' '' '' '..tar.gz' '..tar' '.gz' '..tar.gz' '/..tar.gz' False\n'//a//b//' '' '//a//b' '//a//b' '' '//a//b//' '' '//a/b' '//a/b' True\n'///a/./b/' '' '///a/./b' '///a/./b' '' '///a/./b/' '' '/a/b' '/a/b' True\n'a/b/./../c/' '' 'a/b/./../c' 'a/b/./../c' '' 'a/b/./../c/' '' 'a/c' '/a/c' False\n'é/ü.txt' 'ü.txt' 'é' 'é' 'ü.txt' 'é/ü' '.txt' 'é/ü.txt' '/é/ü.txt' False\n'/x/y/../../../z' 'z' '/x/y/../../..' '/x/y/../../..' 'z' '/x/y/../../../z' '' '/z' '/z' True\n'a..b/c' 'c' 'a..b' 'a..b' 'c' 'a..b/c' '' 'a..b/c' '/a..b/c' False\na/b a/b /b b a/ a/b/c /y/z/ a .py\n/ 16 b'' posix /\n",
    );
}

/// Run a program's `__main__` block on a host with this environment and these
/// files, giving its standard output.
fn run_on(source: &str, env: &[&str], files: &[(&str, &str)]) -> String {
    let wasm = compile(&dedent(source));
    let mut host = Host::with_input("", &[]);
    host.env = env.iter().map(|e| e.to_string()).collect();
    for (name, text) in files {
        host.files
            .insert(name.to_string(), text.as_bytes().to_vec());
    }
    let (instance, mut store) = instantiate_wasm_with(&wasm, host);
    instance
        .get_typed_func::<(), ()>(&store, "_start")
        .expect("_start")
        .call(&mut store, ())
        .expect("runs");
    String::from_utf8(store.data().stdout.clone()).expect("utf-8")
}

/// `os.environ` and `os.getenv` read the host's environment (the first of a
/// repeated name kept, as CPython's `setdefault` keeps it), and
/// `os.path.exists`/`isfile`/`isdir` ask the host what a path names,
/// resolved against the preopened directory as `open()` resolves it.
#[test]
fn os_reads_the_host() {
    let out = run_on(
        r#"
        import os
        from os.path import exists, isfile, isdir

        def main() -> None:
            print(os.environ["HOME"], os.environ.get("NOPE", "-"), len(os.environ))
            print(os.getenv("LANG"), os.getenv("NOPE"), os.getenv("NOPE") is None, os.getenv("NOPE", "dflt"))
            print("HOME" in os.environ, sorted(os.environ.keys()))
            for p in ["notes.txt", "./notes.txt", "data", "data/a.csv", "missing", ".", "data/"]:
                print(p, exists(p), isfile(p), isdir(p))
            print(os.getpid(), os.getcwd())

        if __name__ == "__main__":
            main()
        "#,
        &[
            "HOME=/home/me",
            "LANG=en_US.UTF-8",
            "EMPTY=",
            "HOME=/shadowed",
        ],
        &[("notes.txt", "hi"), ("data/a.csv", "1,2")],
    );
    assert_eq!(
        out,
        "/home/me - 3\n\
         en_US.UTF-8 None True dflt\n\
         True ['EMPTY', 'HOME', 'LANG']\n\
         notes.txt True True False\n\
         ./notes.txt True True False\n\
         data True False True\n\
         data/a.csv True True False\n\
         missing False False False\n\
         . True False True\n\
         data/ True False True\n\
         42 /\n"
    );
}

/// functools.reduce over ints, floats, and strs, with and without an initial
/// value, through a def or a lambda typed from the sequence, and its TypeError.
#[test]
fn functools_reduce() {
    prints(
        r#"
        import functools
        from functools import reduce
        from typing import List


        def add(a: int, b: int) -> int:
            return a + b


        def longer(a: str, b: str) -> str:
            return a if len(a) >= len(b) else b


        def fold_empty(xs: List[int]) -> str:
            try:
                return str(reduce(add, xs))
            except TypeError as e:
                return "TypeError: " + str(e)


        def main() -> None:
            xs = [1, 2, 3, 4, 5]
            print(functools.reduce(add, xs), reduce(add, xs, 100), reduce(lambda a, b: a * b, xs))
            print(reduce(lambda a, b: a + b, [0.5, 1.5, 2.0]), reduce(lambda acc, x: acc + x * 0.5, xs, 0.0))
            print(reduce(longer, ["a", "abc", "ab", "abcd", "x"]), reduce(lambda a, b: a + b, "hello", ""))
            print(reduce(lambda a, b: a if a > b else b, (3, 9, 2)), reduce(add, [7]), reduce(add, [], 9))
            print(fold_empty([]), fold_empty([4, 4]))
            print(reduce(lambda acc, s: acc + len(s), ["ab", "cde"], 0))


        if __name__ == "__main__":
            main()
    "#,
        "15 115 120\n4.0 7.5\nabcd hello\n9 7 9\nTypeError: reduce() of empty iterable with no initial value 8\n5\n",
    );
}

/// functools.partial binds its arguments once, where it is called: partials
/// made in a loop keep their own, and rebinding the name later changes nothing.
#[test]
fn functools_partial() {
    prints(
        r#"
        import functools
        from functools import partial


        def power(base: float, exp: int) -> float:
            return base ** exp


        def greet(greeting: str, name: str, punct: str = "!") -> str:
            return greeting + ", " + name + punct


        def scale(x: int, factor: int = 2) -> int:
            return x * factor


        def main() -> None:
            square = partial(power, exp=2)
            cube = functools.partial(power, exp=3)
            hello = partial(greet, "Hello")
            print(square(3.0), cube(2.0), hello("Ann", "?"), hello("Bob", "."))
            triple = partial(scale, factor=3)
            fs = []
            for i in range(1, 4):
                fs.append(partial(scale, i))
            first = fs[0]
            last = fs[2]
            ones = []
            for f in fs:
                ones.append(f(1))
            print(triple(5), first(10), last(10), ones)
            n = 7
            seven = partial(scale, n)
            n = 9
            print(seven(1))


        if __name__ == "__main__":
            main()
    "#,
        "9.0 8.0 Hello, Ann? Hello, Bob.\n15 10 30 [1, 2, 3]\n7\n",
    );
}

/// cached_property computes once per instance, and assigning the attribute
/// replaces the kept value.
#[test]
fn functools_cached_property() {
    prints(
        r#"
        from functools import cached_property
        import functools


        class Circle:
            def __init__(self, r: float) -> None:
                self.r = r
                self.computed = 0

            @cached_property
            def area(self) -> float:
                self.computed += 1
                return 3.0 * self.r * self.r

            @functools.cached_property
            def label(self) -> str:
                self.computed += 10
                return "circle of " + str(self.r)


        def main() -> None:
            c = Circle(2.0)
            print(c.computed, c.area, c.area, c.computed)
            print(c.label, c.label, c.computed)
            c.area = 1.5
            print(c.area, c.computed)
            d = Circle(1.0)
            print(d.area, d.computed, c.area)


        if __name__ == "__main__":
            main()
    "#,
        "0 12.0 12.0 1\ncircle of 2.0 circle of 2.0 11\n1.5 11\n3.0 1 1.5\n",
    );
}

/// cmp_to_key sorts by a comparison function, reverse= included.
#[test]
fn functools_cmp_to_key() {
    prints(
        r#"
        import functools
        from functools import cmp_to_key


        def by_length_then_text(a: str, b: str) -> int:
            if len(a) != len(b):
                return len(a) - len(b)
            if a < b:
                return -1
            if a > b:
                return 1
            return 0


        def descending(a: int, b: int) -> int:
            return b - a


        def main() -> None:
            words = ["pear", "fig", "banana", "kiwi", "apple", "fig"]
            print(sorted(words, key=cmp_to_key(by_length_then_text)))
            print(sorted([3, 1, 4, 1, 5, 9, 2, 6], key=functools.cmp_to_key(descending)))
            print(sorted([3, 1, 2], key=cmp_to_key(descending), reverse=True))


        if __name__ == "__main__":
            main()
    "#,
        "['fig', 'fig', 'kiwi', 'pear', 'apple', 'banana']\n[9, 6, 5, 4, 3, 2, 1, 1]\n[1, 2, 3]\n",
    );
}

/// An exception a sort key, a comparison, or an instance's __lt__ raises stops
/// sorted() and reaches the try around it.
#[test]
fn sorting_raises_from_a_key_or_comparison() {
    prints(
        r#"
        from functools import cmp_to_key, lru_cache


        def bad_key(x: int) -> int:
            if x == 3:
                raise ValueError("three")
            return -x


        def bad_cmp(a: int, b: int) -> int:
            if a == 7 or b == 7:
                raise KeyError(7)
            return a - b


        class P:
            def __init__(self, v: int) -> None:
                self.v = v

            def __lt__(self, other: "P") -> bool:
                if self.v == 0 or other.v == 0:
                    raise ZeroDivisionError("zero")
                return self.v < other.v


        @lru_cache(maxsize=None)
        def fib(n: int) -> int:
            return n if n < 2 else fib(n - 1) + fib(n - 2)


        def main() -> None:
            try:
                print(sorted([1, 2, 4], key=bad_key))
                print(sorted([1, 3, 2], key=bad_key))
            except ValueError as e:
                print("ValueError", e)
            try:
                print(sorted([5, 1, 3], key=cmp_to_key(bad_cmp)))
                print(sorted([5, 7, 3], key=cmp_to_key(bad_cmp)))
            except KeyError as e:
                print("KeyError", e)
            ps = [P(3), P(1), P(2)]
            qs: list[P] = sorted(ps)
            print([p.v for p in qs])
            try:
                rs: list[P] = sorted([P(3), P(0), P(2)])
                print([p.v for p in rs])
            except ZeroDivisionError as e:
                print("ZeroDivisionError", e)
            print(fib(20))


        if __name__ == "__main__":
            main()
    "#,
        "[4, 2, 1]\nValueError three\n[1, 3, 5]\nKeyError 7\n[1, 2, 3]\nZeroDivisionError zero\n6765\n",
    );
}

/// singledispatchmethod picks the method registered for the argument's type
/// (a bool takes the int arm), by annotation or by register(type).
#[test]
fn functools_singledispatchmethod() {
    prints(
        r#"
        from functools import singledispatchmethod, partialmethod
        import functools


        class Formatter:
            def __init__(self, prefix: str) -> None:
                self.prefix = prefix

            @singledispatchmethod
            def show(self, value: object) -> str:
                return self.prefix + "something"

            @show.register
            def _(self, value: int) -> str:
                return self.prefix + "int " + str(value * 2)

            @show.register
            def _(self, value: str) -> str:
                return self.prefix + "str " + value.upper()

            @show.register(float)
            def _(self, value) -> str:
                return self.prefix + "float " + str(value / 2)


        def main() -> None:
            f = Formatter("> ")
            print(f.show(21))
            print(f.show("hi"))
            print(f.show(3.0))
            print(f.show(True))


        if __name__ == "__main__":
            main()
    "#,
        "> int 42\n> str HI\n> float 1.5\n> int 2\n",
    );
}

/// partialmethod binds a method's leading arguments, and a trailing one by
/// keyword.
#[test]
fn functools_partialmethod() {
    prints(
        r#"
        from functools import partialmethod
        import functools


        class Cell:
            def __init__(self) -> None:
                self.alive = False
                self.note = ""

            def set_state(self, state: bool, note: str = "") -> str:
                self.alive = state
                self.note = note
                return str(state) + ":" + note

            set_alive = partialmethod(set_state, True)
            set_dead = functools.partialmethod(set_state, False, note="rip")


        def main() -> None:
            c = Cell()
            print(c.set_alive("born"), c.alive, c.note)
            print(c.set_dead(), c.alive, c.note)


        if __name__ == "__main__":
            main()
    "#,
        "True:born True born\nFalse:rip False rip\n",
    );
}

/// Every itertools function, against CPython's output for the same program.
#[test]
fn itertools_functions() {
    prints(
        r#"
        import itertools
        from itertools import count, cycle, repeat, chain, islice, accumulate


        def main() -> None:
            c = count(10, 5)
            print(next(c), next(c), next(c))
            print(list(islice(count(), 5)), list(islice(count(2.5, 0.5), 3)))
            print(list(islice(cycle([1, 2, 3]), 7)), list(islice(cycle("ab"), 5)))
            print(list(repeat("x", 3)), list(islice(repeat(7), 2)), list(repeat(1, 0)))
            print(list(chain([1, 2], [3], [4, 5])), list(chain("ab", "cd")), list(chain.from_iterable([[1], [2, 3]])))
            print(list(accumulate([1, 2, 3, 4])), list(accumulate([1, 2, 3], lambda a, b: a * b)), list(accumulate([1, 2], initial=100)))
            print(list(itertools.batched([1, 2, 3, 4, 5], 2)), list(itertools.batched("abcdefg", 3)))
            print(list(itertools.compress("ABCDEF", [1, 0, 1, 0, 1, 1])))
            print(list(itertools.dropwhile(lambda x: x < 5, [1, 4, 6, 4, 1])), list(itertools.takewhile(lambda x: x < 5, [1, 4, 6, 4, 1])))
            print(list(itertools.filterfalse(lambda x: x % 2, range(10))), list(itertools.filterfalse(None, [0, 1, 0, 2])))
            print(list(itertools.pairwise([1, 2, 3, 4])), list(itertools.pairwise("abc")))
            print(list(itertools.islice("ABCDEFG", 2)), list(itertools.islice("ABCDEFG", 2, 4)), list(itertools.islice("ABCDEFG", 2, None)), list(itertools.islice("ABCDEFG", 0, None, 2)))
            a, b = itertools.tee([1, 2, 3])
            print(list(a), list(b))
            print(list(itertools.zip_longest("ABCD", "xy", fillvalue="-")))
            for k, g in itertools.groupby("AAAABBBCCDAABBB"):
                print(k, list(g))
            print([(k, len(list(g))) for k, g in itertools.groupby([1, 1, 2, 3, 3, 3])])
            print(list(itertools.product("ab", [1, 2])), list(itertools.product([0, 1], repeat=2)))
            print(list(itertools.permutations([1, 2, 3])), list(itertools.permutations("abc", 2)))
            print(list(itertools.combinations([1, 2, 3, 4], 2)), list(itertools.combinations("abcd", 3)))
            print(list(itertools.combinations_with_replacement([1, 2, 3], 2)))
            print(sum(islice(count(1), 10)), max(itertools.chain([3, 9], [4])), sorted(itertools.chain("ba", "dc")))


        if __name__ == "__main__":
            main()
    "#,
        "10 15 20\n[0, 1, 2, 3, 4] [2.5, 3.0, 3.5]\n[1, 2, 3, 1, 2, 3, 1] ['a', 'b', 'a', 'b', 'a']\n['x', 'x', 'x'] [7, 7] []\n[1, 2, 3, 4, 5] ['a', 'b', 'c', 'd'] [1, 2, 3]\n[1, 3, 6, 10] [1, 2, 6] [100, 101, 103]\n[(1, 2), (3, 4), (5,)] [('a', 'b', 'c'), ('d', 'e', 'f'), ('g',)]\n['A', 'C', 'E', 'F']\n[6, 4, 1] [1, 4]\n[0, 2, 4, 6, 8] [0, 0]\n[(1, 2), (2, 3), (3, 4)] [('a', 'b'), ('b', 'c')]\n['A', 'B'] ['C', 'D'] ['C', 'D', 'E', 'F', 'G'] ['A', 'C', 'E', 'G']\n[1, 2, 3] [1, 2, 3]\n[('A', 'x'), ('B', 'y'), ('C', '-'), ('D', '-')]\nA ['A', 'A', 'A', 'A']\nB ['B', 'B', 'B']\nC ['C', 'C']\nD ['D']\nA ['A', 'A']\nB ['B', 'B', 'B']\n[(1, 2), (2, 1), (3, 3)]\n[('a', 1), ('a', 2), ('b', 1), ('b', 2)] [(0, 0), (0, 1), (1, 0), (1, 1)]\n[(1, 2, 3), (1, 3, 2), (2, 1, 3), (2, 3, 1), (3, 1, 2), (3, 2, 1)] [('a', 'b'), ('a', 'c'), ('b', 'a'), ('b', 'c'), ('c', 'a'), ('c', 'b')]\n[(1, 2), (1, 3), (1, 4), (2, 3), (2, 4), (3, 4)] [('a', 'b', 'c'), ('a', 'b', 'd'), ('a', 'c', 'd'), ('b', 'c', 'd')]\n[(1, 1), (1, 2), (1, 3), (2, 2), (2, 3), (3, 3)]\n55 9 ['a', 'b', 'c', 'd']\n",
    );
}

/// starmap over a module function and a lambda, and islice's argument check.
#[test]
fn itertools_starmap_and_errors() {
    prints(
        r#"
        import itertools


        def power(a: int, b: int) -> int:
            return a ** b


        def main() -> None:
            print(list(itertools.starmap(power, [(2, 5), (3, 2), (10, 3)])))
            print(list(itertools.starmap(lambda a, b: a + b, [(1, 2), (3, 4)])))
            it = iter_check()
            print(it)


        def iter_check() -> str:
            try:
                list(itertools.islice([1, 2], -1))
            except ValueError as e:
                return str(e)
            return "no error"


        if __name__ == "__main__":
            main()
    "#,
        "[32, 9, 1000]\n[3, 7]\nStop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.\n",
    );
}

/// list, tuple, sum, sorted, min, max, any, all, and set over generators and sequences.
#[test]
fn iterator_consumers() {
    prints(
        r#"
        def gen(n: int):
            i = 0
            while i < n:
                yield i * i
                i += 1


        def words():
            yield "b"
            yield "a"
            yield "c"


        def main() -> None:
            d = {"x": 1, "y": 2}
            print(list(gen(4)), list(range(3)), list("hey"), list(d), list((1.5, 2.5)))
            print(tuple(gen(3)), tuple([4, 5]), tuple("ab"), len(tuple(range(5))))
            print(sum(gen(4)), sorted(gen(4), reverse=True), min(gen(4)), max(gen(4)))
            print(sorted(words()), max(words()), any(gen(1)), all(gen(3)), set(gen(3)))
            xs = [3, 1, 2]
            ys = list(xs)
            ys.append(9)
            print(xs, ys, list(x + 1 for x in xs))
            t = tuple(xs)
            print(t, t[0], t.count(1), any([0, 0, 3]), all([1, 2, 0]), any([]), all(""), any(x > 2 for x in xs))


        if __name__ == "__main__":
            main()
    "#,
        "[0, 1, 4, 9] [0, 1, 2] ['h', 'e', 'y'] ['x', 'y'] [1.5, 2.5]\n(0, 1, 4) (4, 5) ('a', 'b') 5\n14 [9, 4, 1, 0] 0 9\n['a', 'b', 'c'] c False False {0, 1, 4}\n[3, 1, 2] [3, 1, 2, 9] [4, 2, 3]\n(3, 1, 2) 3 1 True False False True True\n",
    );
}

/// A comprehension over an iterator pulls one value at a time, so the iterator's side effects interleave with the body's as in CPython, and groupby's groups are read before the next key invalidates them.
#[test]
fn comprehensions_pull_lazily() {
    prints(
        r#"
        import itertools


        def g():
            for i in range(3):
                print("yield", i)
                yield i


        def show(x: int) -> int:
            print("got", x)
            return x * 10


        def main() -> None:
            print([show(x) for x in g()])
            print({show(x) for x in g() if x != 1})
            print([x + y for x in g() for y in [100, 200]])
            print([(k, list(grp)) for k, grp in itertools.groupby("aab")])


        if __name__ == "__main__":
            main()
    "#,
        "yield 0\ngot 0\nyield 1\ngot 1\nyield 2\ngot 2\n[0, 10, 20]\nyield 0\ngot 0\nyield 1\nyield 2\ngot 2\n{0, 20}\nyield 0\nyield 1\nyield 2\n[100, 200, 101, 201, 102, 202]\n[('a', ['a', 'a']), ('b', ['b'])]\n",
    );
}

/// namedtuple() and typing.NamedTuple: construction with keywords and defaults, repr, indexing and slicing, unpacking, iteration, comparison with tuples, hashing, _replace, _asdict, _fields, _make, and methods of a NamedTuple class.
#[test]
fn named_tuples() {
    prints(
        r#"
        from collections import namedtuple
        from typing import NamedTuple

        Point = namedtuple("Point", "x y")
        Pair = namedtuple("Pair", ["name", "score"], defaults=[0])


        class Item(NamedTuple):
            name: str
            price: float
            qty: int = 1

            def total(self) -> float:
                return self.price * self.qty


        def main() -> None:
            p = Point(1, 2)
            q = Point(x=3, y=4)
            print(p, q, p.x + q.y, p[0], p[-1], len(p), p == Point(1, 2), p == (1, 2), p < q)
            x, y = q
            print(x, y, list(p), tuple(q), sum(p), max(q), 2 in p, p.index(2), p.count(1))
            print(p._replace(y=9), p._asdict(), Point._fields, Point._make([5, 6]))
            print(Pair("a"), Pair("b", 3), Pair("c", score=4).score)
            it = Item("pen", 1.5, 4)
            print(it, it.total(), it.name, it[1], Item("cup", 2.0))
            print(sorted([Point(2, 1), Point(1, 5), Point(1, 2)]))
            s = {Point(1, 2), Point(1, 2), Point(2, 1)}
            print(len(s), Point(1, 2) in s, {Point(0, 0): "origin"}[Point(0, 0)])
            print(p + (7,), p[1:], [v * 10 for v in p], isinstance(p, tuple))
            for v in q:
                print(v)


        if __name__ == "__main__":
            main()
    "#,
        "Point(x=1, y=2) Point(x=3, y=4) 5 1 2 2 True True True\n3 4 [1, 2] (3, 4) 3 4 True 1 1\nPoint(x=1, y=9) {'x': 1, 'y': 2} ('x', 'y') Point(x=5, y=6)\nPair(name='a', score=0) Pair(name='b', score=3) 4\nItem(name='pen', price=1.5, qty=4) 6.0 pen 1.5 Item(name='cup', price=2.0, qty=1)\n[Point(x=1, y=2), Point(x=1, y=5), Point(x=2, y=1)]\n2 True origin\n(1, 2, 7) (2,) [10, 20] True\n3\n4\n",
    );
}

/// A namedtuple()'s fields take their types from the calls of the class: strings and floats, unpacked in loops and comprehensions, and passed to key functions.
#[test]
fn named_tuple_field_types() {
    prints(
        r#"
        import collections

        Person = collections.namedtuple("Person", "name, age")
        Vec = collections.namedtuple("Vec", ("dx", "dy"))


        def older(p: Person) -> Person:
            return p._replace(age=p.age + 1)


        def make(n: str, a: int) -> Person:
            return Person(n, a)


        def main() -> None:
            people = [Person("ann", 31), Person("bob", 25), make("cy", 40)]
            for name, age in people:
                print(name, age)
            print([p.name for p in people], [n.upper() for n, _ in people])
            print(max(people, key=lambda p: p.age), older(people[0]))
            v = Vec(0.5, -1.0)
            print(v, v.dx * 2, abs(v.dy), repr(v), str(v))
            print(sorted(people, key=lambda p: p.age)[0].name)
            t = people[1]
            try:
                print(t[5])
            except IndexError as e:
                print("IndexError:", e)
            try:
                Vec._make([1.0])
            except TypeError as e:
                print("TypeError:", e)
            print(Person._make(("dee", 50)), Vec._make([2.0, 3.0]))


        if __name__ == "__main__":
            main()
    "#,
        "ann 31\nbob 25\ncy 40\n['ann', 'bob', 'cy'] ['ANN', 'BOB', 'CY']\nPerson(name='cy', age=40) Person(name='ann', age=32)\nVec(dx=0.5, dy=-1.0) 1.0 1.0 Vec(dx=0.5, dy=-1.0) Vec(dx=0.5, dy=-1.0)\nbob\nIndexError: tuple index out of range\nTypeError: Expected 2 arguments, got 1\nPerson(name='dee', age=50) Vec(dx=2.0, dy=3.0)\n",
    );
}

/// max() and sorted() with key=lambda over a list of instances held in a variable.
#[test]
fn key_lambdas_over_instances() {
    prints(
        r#"
        class Person:
            def __init__(self, name: str, age: int) -> None:
                self.name = name
                self.age = age


        def main() -> None:
            people = [Person("ann", 31), Person("bob", 25)]
            print(max(people, key=lambda p: p.age).name)
            print(sorted(people, key=lambda p: p.age)[0].name)


        if __name__ == "__main__":
            main()
    "#,
        "ann\nbob\n",
    );
}

/// What CPython raises for a named tuple at definition or assignment, and the
/// shapes that are refused rather than answered wrong.
#[test]
fn named_tuple_refusals() {
    let refused = |source: &str, needle: &str| {
        let err = try_compile(&dedent(source)).expect_err("must be refused");
        assert!(err.contains(needle), "expected `{needle}` in: {err}");
    };
    refused(
        r#"
        from collections import namedtuple
        P = namedtuple("P", "x y")

        def f() -> int:
            p = P(1, 2)
            p.x = 3
            return p.x
        "#,
        "can't set attribute 'x'",
    );
    refused(
        r#"
        from collections import namedtuple
        P = namedtuple("P", "x _y")
        "#,
        "field names cannot start with an underscore",
    );
    refused(
        r#"
        from collections import namedtuple
        P = namedtuple("P", "x x")
        "#,
        "duplicate field name",
    );
    refused(
        r#"
        from collections import namedtuple

        def f() -> int:
            P = namedtuple("P", "x y")
            return P(1, 2).x
        "#,
        "module-level assignment",
    );
    // A field given an int in one call and a string in another has no one
    // type, so it is untyped, which holds only ints.
    refused(
        r#"
        from collections import namedtuple
        P = namedtuple("P", "x")

        def f() -> str:
            return str(P(1)) + str(P("a"))
        "#,
        "a place with no type",
    );
    refused(
        r#"
        class A:
            def __init__(self) -> None:
                self.x = 1

        def f() -> int:
            return A.nothing()
        "#,
        "type object 'A' has no attribute 'nothing'",
    );
}

/// groupby's groups, stored past the groupby, are empty, as CPython's are once the groupby has moved on (the last one included).
#[test]
fn stored_groups_are_empty() {
    prints(
        r#"
        import itertools


        def main() -> None:
            groups = list(itertools.groupby([1, 1, 2]))
            print([(k, list(grp)) for k, grp in groups])


        if __name__ == "__main__":
            main()
    "#,
        "[(1, []), (2, [])]\n",
    );
}

/// Counter: construction from an iterable, a mapping, and keywords; missing counts of 0; most_common's order; the multiset operators and comparisons; update, subtract, elements, total; and its repr.
#[test]
fn counter_api() {
    prints(
        r#"
        from collections import Counter


        def main() -> None:
            a = Counter("abracadabra")
            b = Counter(a=1, b=5, z=2)
            print(a, b)
            print(a + b, a - b, a | b, a & b)
            print(+Counter(x=2, y=-1), -Counter(x=2, y=-1))
            print(a.most_common(), a.most_common(0), a.most_common(-2), a.most_common(10))
            print(a == Counter("aaaaabbrrcd"), a != b, Counter(a=1) == Counter(a=1, b=0))
            print(Counter(a=1) <= Counter(a=2), Counter(a=3) < Counter(a=2), a >= Counter("ab"))
            c = Counter()
            for w in ["x", "y", "x"]:
                c[w] += 1
            c.subtract(["x", "z"])
            print(c, c["z"], len(c), c.total())
            del c["nope"]
            del c["y"]
            print(c, list(c.keys()), list(c.values()), list(c.items()))
            d = Counter({"k": 3})
            d += Counter(k=-5, m=2)
            print(d)
            d.update({"m": 1, "n": 0})
            print(d, d.get("m"), d.get("q"), "n" in d)
            e = Counter([1, 2, 2, 3, 3, 3])
            print(e, e[3], sorted(e.elements()), e.copy() == e, dict(e))
            print(Counter(), Counter().most_common(1), bool(Counter()), bool(e))


        if __name__ == "__main__":
            main()
    "#,
        "Counter({'a': 5, 'b': 2, 'r': 2, 'c': 1, 'd': 1}) Counter({'b': 5, 'z': 2, 'a': 1})\nCounter({'b': 7, 'a': 6, 'r': 2, 'z': 2, 'c': 1, 'd': 1}) Counter({'a': 4, 'r': 2, 'c': 1, 'd': 1}) Counter({'a': 5, 'b': 5, 'r': 2, 'z': 2, 'c': 1, 'd': 1}) Counter({'b': 2, 'a': 1})\nCounter({'x': 2}) Counter({'y': 1})\n[('a', 5), ('b', 2), ('r', 2), ('c', 1), ('d', 1)] [] [] [('a', 5), ('b', 2), ('r', 2), ('c', 1), ('d', 1)]\nTrue True True\nTrue False True\nCounter({'x': 1, 'y': 1, 'z': -1}) -1 3 1\nCounter({'x': 1, 'z': -1}) ['x', 'z'] [1, -1] [('x', 1), ('z', -1)]\nCounter({'m': 2})\nCounter({'m': 3, 'n': 0}) 3 None True\nCounter({3: 3, 2: 2, 1: 1}) 3 [1, 2, 2, 3, 3, 3] True {1: 1, 2: 2, 3: 3}\nCounter() [] False True\n",
    );
}

/// defaultdict with each kind of factory (a type, a class, a function, a lambda, a nested defaultdict, None), and OrderedDict's move_to_end, popitem(last=False), and order-sensitive equality.
#[test]
fn defaultdict_and_ordered_dict() {
    prints(
        r#"
        from collections import defaultdict, OrderedDict, Counter


        class Acc:
            def __init__(self) -> None:
                self.n = 0

            def __repr__(self) -> str:
                return f"Acc({self.n})"


        def zero() -> float:
            return 0.5


        def main() -> None:
            a = defaultdict(int)
            for ch in "mississippi":
                a[ch] += 1
            print(a, a["z"], len(a), sorted(a.items()))
            s = defaultdict(set)
            s["x"].add(1)
            s["x"].add(1)
            s["y"].add(2)
            print(s, dict(s))
            n = defaultdict(lambda: -1)
            n["k"] += 5
            print(dict(n), n["q"])
            f = defaultdict(zero)
            f["a"] += 1.0
            print(dict(f))
            objs = defaultdict(Acc)
            objs["p"].n += 3
            print(objs)
            nested = defaultdict(dict)
            nested["a"]["b"] = 1
            print(nested, nested["a"]["b"])
            cnt = defaultdict(Counter)
            cnt["fruit"]["apple"] += 2
            print(cnt)
            plain = defaultdict(None)
            plain["x"] = 1
            try:
                plain["y"]
            except KeyError as e:
                print("KeyError", e)
            print(plain, "x" in plain, plain.get("y", 7), plain.pop("x"), plain)
            od = OrderedDict([("a", 1), ("b", 2), ("c", 3)])
            od.move_to_end("a")
            od.move_to_end("c", last=False)
            print(od, od.popitem(), od.popitem(last=False), od)
            o2 = OrderedDict(x=1)
            print(o2 == OrderedDict(x=1), o2 == {"x": 1}, OrderedDict(a=1, b=2) == OrderedDict(b=2, a=1))
            print(OrderedDict(), list(OrderedDict(z=1, y=2)), OrderedDict({"q": 9}))


        if __name__ == "__main__":
            main()
    "#,
        "defaultdict(<class 'int'>, {'m': 1, 'i': 4, 's': 4, 'p': 2, 'z': 0}) 0 5 [('i', 4), ('m', 1), ('p', 2), ('s', 4), ('z', 0)]\ndefaultdict(<class 'set'>, {'x': {1}, 'y': {2}}) {'x': {1}, 'y': {2}}\n{'k': 4} -1\n{'a': 1.5}\ndefaultdict(<class '__main__.Acc'>, {'p': Acc(3)})\ndefaultdict(<class 'dict'>, {'a': {'b': 1}}) 1\ndefaultdict(<class 'collections.Counter'>, {'fruit': Counter({'apple': 2})})\nKeyError 'y'\ndefaultdict(None, {}) True 7 1 defaultdict(None, {})\nOrderedDict({'b': 2}) ('a', 1) ('c', 3) OrderedDict({'b': 2})\nTrue True False\nOrderedDict() ['z', 'y'] OrderedDict({'q': 9})\n",
    );
}

/// deque as a queue (a breadth-first search), its two ends, maxlen, rotate, indexing, and its errors; ChainMap's lookups through its maps, writes to the first, new_child, and parents.
#[test]
fn deque_and_chain_map() {
    prints(
        r#"
        from collections import deque, ChainMap


        def bfs(start: int, edges: dict[int, list[int]]) -> list[int]:
            seen = {start}
            order = []
            q = deque([start])
            while q:
                node = q.popleft()
                order.append(node)
                for nxt in edges.get(node, []):
                    if nxt not in seen:
                        seen.add(nxt)
                        q.append(nxt)
            return order


        def main() -> None:
            print(bfs(1, {1: [2, 3], 2: [4], 3: [4, 5], 4: [6]}))
            q = deque("abc")
            q.extend("de")
            q.extendleft(["y", "z"])
            print(q, len(q), q.count("a"), q.index("c"), "e" in q)
            q.rotate(-2)
            print(q)
            q.reverse()
            print(q, q.pop(), q.popleft())
            q.remove("b")
            q[0] = "Q"
            del q[1]
            print(q, list(reversed(q)), q == deque(["Q", "c", "a", "z"]))
            last = deque(range(10), maxlen=3)
            print(last, last.maxlen, deque(maxlen=0), deque())
            try:
                deque().pop()
            except IndexError as e:
                print("IndexError", e)
            try:
                q.remove("nope")
            except ValueError as e:
                print("ValueError", e)
            d2 = deque([1, 2]) + deque([3])
            d2 += [4]
            print(d2, d2 * 2, d2.copy(), deque([1]) < deque([2]))
            a = {"x": 1}
            b = {"x": 9, "y": 2}
            cm = ChainMap(a, b)
            print(cm["x"], cm["y"], len(cm), sorted(cm), "y" in cm, cm.get("z", 0))
            cm["z"] = 3
            print(a, cm.maps, list(cm.items()), cm)
            child = cm.new_child()
            child["x"] = 100
            print(child["x"], cm["x"], child.parents, dict(child))
            try:
                del cm["y"]
            except KeyError as e:
                print("KeyError", e)


        if __name__ == "__main__":
            main()
    "#,
        "[1, 2, 3, 4, 5, 6]\ndeque(['z', 'y', 'a', 'b', 'c', 'd', 'e']) 7 1 4 True\ndeque(['a', 'b', 'c', 'd', 'e', 'z', 'y'])\ndeque(['z', 'e', 'd', 'c', 'b']) a y\ndeque(['Q', 'd', 'c']) ['c', 'd', 'Q'] False\ndeque([7, 8, 9], maxlen=3) 3 deque([], maxlen=0) deque([])\nIndexError pop from an empty deque\nValueError 'nope' is not in deque\ndeque([1, 2, 3, 4]) deque([1, 2, 3, 4, 1, 2, 3, 4]) deque([1, 2, 3, 4]) True\n1 2 2 ['x', 'y'] True 0\n{'x': 1, 'z': 3} [{'x': 1, 'z': 3}, {'x': 9, 'y': 2}] [('x', 1), ('y', 2), ('z', 3)] ChainMap({'x': 1, 'z': 3}, {'x': 9, 'y': 2})\n100 1 ChainMap({'x': 1, 'z': 3}, {'x': 9, 'y': 2}) {'x': 100, 'y': 2, 'z': 3}\nKeyError \"Key not found in the first mapping: 'y'\"\n",
    );
}

/// Counter, defaultdict, deque, and OrderedDict together, as a program uses them.
#[test]
fn collections_first_probe() {
    prints(
        r#"
        from collections import Counter, defaultdict, deque, OrderedDict


        def main() -> None:
            words = "the cat and the hat and the bat".split()
            c = Counter(words)
            print(c)
            print(c["the"], c["dog"], len(c), "cat" in c, c.most_common(2))
            c["dog"] += 2
            c.update(["cat", "cat"])
            print(c, c.total(), sorted(c), list(c.elements())[:4])
            d = defaultdict(list)
            for i, w in enumerate(words):
                d[w].append(i)
            print(d)
            print(dict(d), d["zzz"], len(d))
            q = deque([1, 2, 3], maxlen=4)
            q.append(4)
            q.append(5)
            q.appendleft(0)
            print(q, q.popleft(), q.pop(), list(q), len(q), q[0], q[-1])
            q.rotate(1)
            print(q, q.maxlen, 3 in q)
            od = OrderedDict()
            od["b"] = 1
            od["a"] = 2
            od.move_to_end("b")
            print(od, list(od), od.popitem(last=False), od)


        if __name__ == "__main__":
            main()
    "#,
        "Counter({'the': 3, 'and': 2, 'cat': 1, 'hat': 1, 'bat': 1})\n3 0 5 True [('the', 3), ('and', 2)]\nCounter({'the': 3, 'cat': 3, 'and': 2, 'dog': 2, 'hat': 1, 'bat': 1}) 12 ['and', 'bat', 'cat', 'dog', 'hat', 'the'] ['the', 'the', 'the', 'cat']\ndefaultdict(<class 'list'>, {'the': [0, 3, 6], 'cat': [1], 'and': [2, 5], 'hat': [4], 'bat': [7]})\n{'the': [0, 3, 6], 'cat': [1], 'and': [2, 5], 'hat': [4], 'bat': [7]} [] 6\ndeque([2, 3], maxlen=4) 0 4 [2, 3] 2 2 3\ndeque([3, 2], maxlen=4) 4 True\nOrderedDict({'b': 1}) ['a', 'b'] ('a', 2) OrderedDict({'b': 1})\n",
    );
}

/// The `collections` shapes that are refused rather than answered wrong.
#[test]
fn collections_refusals() {
    let refused = |source: &str, needle: &str| {
        let err = try_compile(&dedent(source)).expect_err("must be refused");
        assert!(err.contains(needle), "expected `{needle}` in: {err}");
    };
    // Nothing says what the Counter holds.
    refused(
        r#"
        from collections import Counter

        def f(c: Counter) -> int:
            return c["a"]
        "#,
        "a bare 'Counter' annotation",
    );
    refused(
        r#"
        from collections import defaultdict

        def f() -> int:
            d = defaultdict(list)
            return len(d)
        "#,
        "needs its key type",
    );
    // A lambda factory that reads the scope it is written in.
    refused(
        r#"
        from collections import defaultdict

        def f(start: int) -> int:
            d = defaultdict(lambda: start)
            d["a"] += 1
            return d["a"]
        "#,
        "reads 'start'",
    );
    // CPython's repr shows the factory function's address.
    refused(
        r#"
        from collections import defaultdict

        def zero() -> int:
            return 0

        def f() -> str:
            d = defaultdict(zero)
            d["a"] += 1
            return str(d)
        "#,
        "defines no __repr__",
    );
    // An instance of a class that is not a dict, where a dict is expected
    // (a Counter is one, and passes as the dict it holds).
    refused(
        r#"
        class A:
            def __init__(self) -> None:
                self.x = 1

        def total(d: dict[str, int]) -> int:
            return len(d)

        def f() -> int:
            return total(A())
        "#,
        "an instance of A is used where dict is expected",
    );
    refused(
        r#"
        from collections import deque

        def f() -> int:
            q = deque([1, 2])
            q.sort()
            return q[0]
        "#,
        "'deque' object has no attribute 'sort'",
    );
}

/// type() of a collections class is CPython's class, and isinstance() answers for the class and for dict.
#[test]
fn collections_class_identity() {
    prints(
        r#"
        from collections import Counter, defaultdict, deque


        def zero() -> int:
            return 0


        def main() -> None:
            c = Counter("ab")
            d = defaultdict(zero)
            d["a"] += 1
            q = deque([1])
            print(type(c), type(c).__name__, type(d), type(q).__name__, type(c) is type(Counter("x")))
            print(isinstance(c, Counter), isinstance(c, dict), isinstance(q, deque), isinstance(q, Counter), isinstance(d, dict))


        if __name__ == "__main__":
            main()
    "#,
        "<class 'collections.Counter'> Counter <class 'collections.defaultdict'> deque True\nTrue True True False True\n",
    );
}

/// json.dumps of every serializable type with each option (indent, separators, sort_keys, ensure_ascii), and json.loads into declared types, with CPython's JSONDecodeError messages and positions.
#[test]
fn json_dumps_and_loads() {
    prints(
        r#"
        import json


        def main() -> None:
            print(json.dumps((1, 2.5, True, None, "x")))
            print(json.dumps({"a": [1, 2], "b": [3]}), json.dumps({"b": {"c": 3}}))
            print(json.dumps({"k": "é\n\"q\"\t\\"}), json.dumps("😀"), json.dumps("é", ensure_ascii=False))
            print(json.dumps({"b": 1, "a": 2}, sort_keys=True), json.dumps([1, 2], separators=(",", ":")))
            print(json.dumps({"a": [[1], []], "c": [[2, 3]]}, indent=2))
            print(json.dumps([{"x": 1}, {}], indent="\t"), json.dumps([], indent=4), json.dumps({"k": (1, "v")}, indent=1))
            print(json.dumps([float("nan"), float("inf"), -float("inf"), 1e100, 0.1]))
            nums: list[int] = json.loads("[1, 2, -3, 0]")
            print(nums, sum(nums))
            d: dict[str, list[float]] = json.loads('{"x": [1.5, 2.5, -1e3], "y": []}')
            print(d)
            s: str = json.loads('"a\\u00e9\\ud83d\\ude00\\n"')
            print(repr(s), len(s))
            flags: list[bool] = json.loads(" [true, false] ")
            o: dict[str, int] = json.loads('{"a": 1, "a": 2, "b": 3}')
            print(flags, o)
            for bad in ["", "[1,]", "[1 2]", '{"a" 1}', '{"a": 1,}', "[1", '"abc', "nul", "1 2", '{1: 2}', '"\\x"']:
                try:
                    v: list[int] = json.loads(bad)
                    print(v)
                except json.JSONDecodeError as e:
                    print("JSONDecodeError:", e)


        if __name__ == "__main__":
            main()
    "#,
        "[1, 2.5, true, null, \"x\"]\n{\"a\": [1, 2], \"b\": [3]} {\"b\": {\"c\": 3}}\n{\"k\": \"\\u00e9\\n\\\"q\\\"\\t\\\\\"} \"\\ud83d\\ude00\" \"é\"\n{\"a\": 2, \"b\": 1} [1,2]\n{\n  \"a\": [\n    [\n      1\n    ],\n    []\n  ],\n  \"c\": [\n    [\n      2,\n      3\n    ]\n  ]\n}\n[\n	{\n		\"x\": 1\n	},\n	{}\n] [] {\n \"k\": [\n  1,\n  \"v\"\n ]\n}\n[NaN, Infinity, -Infinity, 1e+100, 0.1]\n[1, 2, -3, 0] 0\n{'x': [1.5, 2.5, -1000.0], 'y': []}\n'aé😀\\n' 4\n[True, False] {'a': 2, 'b': 3}\nJSONDecodeError: Expecting value: line 1 column 1 (char 0)\nJSONDecodeError: Illegal trailing comma before end of array: line 1 column 3 (char 2)\nJSONDecodeError: Expecting ',' delimiter: line 1 column 4 (char 3)\nJSONDecodeError: Expecting ':' delimiter: line 1 column 6 (char 5)\nJSONDecodeError: Illegal trailing comma before end of object: line 1 column 8 (char 7)\nJSONDecodeError: Expecting ',' delimiter: line 1 column 3 (char 2)\nJSONDecodeError: Unterminated string starting at: line 1 column 1 (char 0)\nJSONDecodeError: Expecting value: line 1 column 1 (char 0)\nJSONDecodeError: Extra data: line 1 column 3 (char 2)\nJSONDecodeError: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)\nJSONDecodeError: Invalid \\escape: line 1 column 2 (char 1)\n",
    );
}

/// JSONDecodeError through `json.JSONDecodeError` and a from-import: a
/// ValueError, shown as CPython's `json.decoder.JSONDecodeError`, and the
/// last line of the report of one nothing catches.
#[test]
fn json_errors_by_name() {
    let run = run_main(
        &dedent(
            r#"
        import json
        from json import dumps, loads, JSONDecodeError


        def main() -> None:
            try:
                x: int = loads("{")
                print(x)
            except JSONDecodeError as e:
                print(type(e), type(e).__name__, isinstance(e, ValueError), repr(e))
            print(dumps({"a": 1}), json.dumps([[1.5]]))
            y: int = json.loads("[")
            print(y)


        if __name__ == "__main__":
            main()
    "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stdout, "<class 'json.decoder.JSONDecodeError'> JSONDecodeError True JSONDecodeError('Expecting property name enclosed in double quotes: line 1 column 2 (char 1)')\n{\"a\": 1} [[1.5]]\n");
    assert_eq!(
        run.stderr,
        "json.decoder.JSONDecodeError: Expecting value: line 1 column 2 (char 1)\n"
    );
    assert_eq!(run.exit, 1);
}

/// `json.dump` to a file and `json.load` back, through WASI.
#[test]
fn json_files() {
    let out = run_on(
        r#"
        import json


        def main() -> None:
            scores = {"ada": [3, 5], "bob": []}
            with open("out.json", "w") as f:
                json.dump(scores, f, indent=1)
            with open("out.json") as f:
                back: dict[str, list[int]] = json.load(f)
            print(back == scores, back)
            with open("in.json") as f:
                cfg: dict[str, str] = json.load(f)
            print(cfg["mode"])


        if __name__ == "__main__":
            main()
    "#,
        &[],
        &[("in.json", "{\"mode\": \"fast\"}")],
    );
    assert_eq!(out, "True {'ada': [3, 5], 'bob': []}\nfast\n");
}

/// A document whose values the declared type cannot hold raises TypeError
/// (CPython would hand the other value back, which no declared type here
/// can hold), and a declaration json.loads cannot parse into is refused.
#[test]
fn json_loads_needs_its_type() {
    let run = run_main(
        &dedent(
            r#"
            import json

            def main() -> None:
                try:
                    w: list[int] = json.loads('[1, "a"]')
                    print(w)
                except TypeError as e:
                    print(e)

            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(
        run.stdout,
        "json.loads(): the value at char 4 is a string, where int is declared\n"
    );
    for (source, needle) in [
        (
            "import json\n\ndef f(s: str) -> int:\n    d = json.loads(s)\n    return len(d)\n",
            "parses into the type the value is declared to have",
        ),
        (
            "import json\n\ndef f(s: str) -> int:\n    d: dict[int, int] = json.loads(s)\n    return len(d)\n",
            "a JSON object's keys are strs",
        ),
        (
            "import json\n\ndef f() -> str:\n    return json.dumps({1, 2})\n",
            "Object of type set is not JSON serializable",
        ),
        (
            "import json\n\ndef f() -> str:\n    return json.dumps([1], default=str)\n",
            "'default' argument is not supported",
        ),
    ] {
        let err = try_compile(source).expect_err("must be refused");
        assert!(err.contains(needle), "expected `{needle}` in: {err}");
    }
}

/// datetime: `time`, `datetime`, and `timezone`: reprs, formats, arithmetic, conversions, ISO 8601 and strptime parsing.
#[test]
fn datetime_classes() {
    prints(
        r#"
        from datetime import date, time, datetime, timedelta, timezone

        def main() -> None:
            utc = timezone.utc
            ist = timezone(timedelta(hours=5, minutes=30), "IST")
            print(repr(utc), repr(ist), str(ist), ist.tzname(None))
            print(repr(timezone(timedelta(hours=-5))), timezone(timedelta(hours=-5)).tzname(None))
            print(repr(timezone.min), repr(timezone.max))
            t = time(13, 5, 9)
            print(repr(t), str(t), t.isoformat("milliseconds"))
            print(repr(time(1, 2, 3, 4, timezone(timedelta(hours=1)), fold=1)))
            print(time(13, 5, 9, tzinfo=utc).strftime("%Y %m %d %H %z %Z"))
            print(t < time(14), t == time(13, 5, 9), t != time(13, 5, 9))
            d = datetime(2024, 3, 15, 13, 5, 9, 7, tzinfo=ist)
            print(repr(d))
            print(str(d), d.isoformat(), d.isoformat(timespec="hours"))
            print(d.strftime("%z %Z %c %f %j %U %W %G %V %u %w %I %p %e %D %F %T %R %C %:z"))
            print(d.ctime(), d.timestamp())
            print(repr(d.date()), repr(d.time()), repr(d.timetz()))
            print(repr(d.utcoffset()), d.tzname(), d.dst())
            n = datetime(2024, 1, 1)
            print(repr(n), n.utcoffset(), n.tzname())
            print(repr(n + timedelta(days=1, hours=25)), repr(n - timedelta(microseconds=1)))
            print(repr(datetime(2024, 1, 1, tzinfo=utc) - datetime(2023, 12, 31, 20, tzinfo=ist)))
            print(repr(n.astimezone(utc)), repr(datetime(2024, 1, 1, tzinfo=timezone(timedelta(hours=2))).astimezone(utc)))
            print(repr(n.astimezone()))
            print(datetime(2024, 1, 1, tzinfo=utc) == datetime(2024, 1, 1, 5, 30, tzinfo=ist))
            print(datetime(2024, 1, 1, tzinfo=utc) < datetime(2024, 1, 1, 5, 31, tzinfo=ist))
            print(datetime(2024, 1, 1, tzinfo=utc) == datetime(2024, 1, 1))
            print(datetime(2024, 1, 1, 12, 30, 15, 500).timestamp())
            print(datetime(2024, 1, 1, 12, 30, 15, 500, tzinfo=timezone(timedelta(hours=-3))).timestamp())
            print(repr(datetime.fromtimestamp(1700000000.5, utc)), repr(datetime.utcfromtimestamp(-1.5)))
            print(repr(datetime.combine(date(2024, 1, 2), time(3, 4))))
            print(repr(datetime.combine(date(2024, 1, 2), time(3, 4), utc)))
            print(repr(n.replace(tzinfo=utc)), repr(n.replace(tzinfo=utc).replace(tzinfo=None)))
            print(repr(n.replace(day=5, hour=3)))
            print(repr(datetime.max), repr(datetime.min), repr(time.max))
            print(f"{n:%Y/%m}", f"{n}")
            print(repr(datetime.fromisoformat("2024-01-01 12:30:45.123+05:30")))
            print(repr(datetime.fromisoformat("2024-01-01T12:30Z")))
            print(repr(datetime.fromisoformat("20240101T123045")))
            print(repr(datetime.fromisoformat("2024-W01-2T10")))
            print(repr(datetime.fromisoformat("2024-01-01X10:00")))
            print(repr(time.fromisoformat("T12:30")), repr(time.fromisoformat("12:30:01.5-01:00")))
            print(repr(datetime.strptime("2024-03-15 1:05:09 pm", "%Y-%m-%d %I:%M:%S %p")))
            print(repr(datetime.strptime("15 march 2024", "%d %B %Y")))
            print(repr(datetime.strptime("2024-03-15T10:00:00+0530", "%Y-%m-%dT%H:%M:%S%z")))
            print(repr(datetime.strptime("2024-03-15 10:00 Z", "%Y-%m-%d %H:%M %z")))
            print(repr(datetime.strptime("Fri Mar 15 13:05:09 2024", "%c")))
            print(repr(datetime.strptime("2024 100", "%Y %j")))
            print(repr(datetime.strptime("2024 10 1", "%Y %W %w")), repr(datetime.strptime("2024 10 1", "%Y %U %w")))
            print(repr(datetime.strptime("2024-03-15 UTC", "%Y-%m-%d %Z")))
            print(repr(datetime.strptime("1.5", "%S.%f")))
            print(repr(datetime.strptime("202412", "%Y%m%d")))
            print(repr(datetime.strptime("01:02:03 PM", "%r")))
            print(repr(datetime.strptime("2024-03-15 10:00 +05:30 UTC", "%Y-%m-%d %H:%M %z %Z")))
            print(isinstance(n, date), isinstance(date(2024, 1, 1), datetime), isinstance(n, (int, date)))
            print(n.weekday(), n.isoweekday(), n.isocalendar(), n.toordinal())
            print(repr(datetime.fromordinal(738000)), repr(datetime.fromisocalendar(2024, 1, 1)))

        if __name__ == "__main__":
            main()
        "#,
        r#"datetime.timezone.utc datetime.timezone(datetime.timedelta(seconds=19800), 'IST') IST IST
datetime.timezone(datetime.timedelta(days=-1, seconds=68400)) UTC-05:00
datetime.timezone(datetime.timedelta(days=-1, seconds=60)) datetime.timezone(datetime.timedelta(seconds=86340))
datetime.time(13, 5, 9) 13:05:09 13:05:09.000
datetime.time(1, 2, 3, 4, tzinfo=datetime.timezone(datetime.timedelta(seconds=3600)), fold=1)
1900 01 01 13 +0000 UTC
True True False
datetime.datetime(2024, 3, 15, 13, 5, 9, 7, tzinfo=datetime.timezone(datetime.timedelta(seconds=19800), 'IST'))
2024-03-15 13:05:09.000007+05:30 2024-03-15T13:05:09.000007+05:30 2024-03-15T13+05:30
+0530 IST Fri Mar 15 13:05:09 2024 000007 075 10 11 2024 11 5 5 01 PM 15 03/15/24 2024-03-15 13:05:09 13:05 20 +05:30
Fri Mar 15 13:05:09 2024 1710488109.000007
datetime.date(2024, 3, 15) datetime.time(13, 5, 9, 7) datetime.time(13, 5, 9, 7, tzinfo=datetime.timezone(datetime.timedelta(seconds=19800), 'IST'))
datetime.timedelta(seconds=19800) IST None
datetime.datetime(2024, 1, 1, 0, 0) None None
datetime.datetime(2024, 1, 3, 1, 0) datetime.datetime(2023, 12, 31, 23, 59, 59, 999999)
datetime.timedelta(seconds=34200)
datetime.datetime(2024, 1, 1, 0, 0, tzinfo=datetime.timezone.utc) datetime.datetime(2023, 12, 31, 22, 0, tzinfo=datetime.timezone.utc)
datetime.datetime(2024, 1, 1, 0, 0, tzinfo=datetime.timezone(datetime.timedelta(0), 'UTC'))
True
True
False
1704112215.0005
1704123015.0005
datetime.datetime(2023, 11, 14, 22, 13, 20, 500000, tzinfo=datetime.timezone.utc) datetime.datetime(1969, 12, 31, 23, 59, 58, 500000)
datetime.datetime(2024, 1, 2, 3, 4)
datetime.datetime(2024, 1, 2, 3, 4, tzinfo=datetime.timezone.utc)
datetime.datetime(2024, 1, 1, 0, 0, tzinfo=datetime.timezone.utc) datetime.datetime(2024, 1, 1, 0, 0)
datetime.datetime(2024, 1, 5, 3, 0)
datetime.datetime(9999, 12, 31, 23, 59, 59, 999999) datetime.datetime(1, 1, 1, 0, 0) datetime.time(23, 59, 59, 999999)
2024/01 2024-01-01 00:00:00
datetime.datetime(2024, 1, 1, 12, 30, 45, 123000, tzinfo=datetime.timezone(datetime.timedelta(seconds=19800)))
datetime.datetime(2024, 1, 1, 12, 30, tzinfo=datetime.timezone.utc)
datetime.datetime(2024, 1, 1, 12, 30, 45)
datetime.datetime(2024, 1, 2, 10, 0)
datetime.datetime(2024, 1, 1, 10, 0)
datetime.time(12, 30) datetime.time(12, 30, 1, 500000, tzinfo=datetime.timezone(datetime.timedelta(days=-1, seconds=82800)))
datetime.datetime(2024, 3, 15, 13, 5, 9)
datetime.datetime(2024, 3, 15, 0, 0)
datetime.datetime(2024, 3, 15, 10, 0, tzinfo=datetime.timezone(datetime.timedelta(seconds=19800)))
datetime.datetime(2024, 3, 15, 10, 0, tzinfo=datetime.timezone.utc)
datetime.datetime(2024, 3, 15, 13, 5, 9)
datetime.datetime(2024, 4, 9, 0, 0)
datetime.datetime(2024, 3, 4, 0, 0) datetime.datetime(2024, 3, 11, 0, 0)
datetime.datetime(2024, 3, 15, 0, 0)
datetime.datetime(1900, 1, 1, 0, 0, 1, 500000)
datetime.datetime(2024, 1, 2, 0, 0)
datetime.datetime(1900, 1, 1, 13, 2, 3)
datetime.datetime(2024, 3, 15, 10, 0, tzinfo=datetime.timezone(datetime.timedelta(seconds=19800), 'UTC'))
True False True
0 1 datetime.IsoCalendarDate(year=2024, week=1, weekday=1) 738886
datetime.datetime(2021, 7, 29, 0, 0) datetime.datetime(2024, 1, 1, 0, 0)
"#,
    );
}

/// datetime: The errors the classes raise, with CPython's messages.
#[test]
fn datetime_errors() {
    prints(
        r#"
        from datetime import date, time, datetime, timedelta, timezone

        def t1() -> str:
            return repr(timezone(timedelta(days=1)))
        def t2() -> str:
            return repr(datetime(2024, 13, 1))
        def t3() -> str:
            return repr(datetime(2024, 1, 1, 24))
        def t4() -> str:
            return repr(datetime.fromisoformat("2024-13-01"))
        def t5() -> str:
            return repr(datetime.fromisoformat("2024-01-01T25:00"))
        def t6() -> str:
            return repr(datetime.fromisoformat("2024-01-01T"))
        def t7() -> str:
            return repr(datetime.strptime("2024", "%Y%"))
        def t8() -> str:
            return repr(datetime.strptime("2024", "%Q"))
        def t9() -> str:
            return repr(datetime.strptime("2024 x", "%Y %"))
        def t10() -> str:
            return repr(datetime.strptime("  2024", "%Y"))
        def t11() -> str:
            return repr(datetime.strptime("2024", " %Y"))
        def t12() -> str:
            return repr(datetime.strptime("2024-02-30", "%Y-%m-%d"))
        def t13() -> str:
            return repr(datetime.strptime("2024-01-01x", "%Y-%m-%d"))
        def t14() -> str:
            return repr(datetime(2024, 1, 1, tzinfo=timezone.utc) - datetime(2024, 1, 1))
        def t15() -> str:
            return str(datetime(2024, 1, 1, tzinfo=timezone.utc) < datetime(2024, 1, 1))
        def t16() -> str:
            return str(time(1, tzinfo=timezone.utc) < time(1))
        def t17() -> str:
            return repr(datetime(9999, 12, 31) + timedelta(days=1))
        def t18() -> str:
            return time(13, 5).isoformat("bogus")
        def t19() -> str:
            return repr(date.fromisoformat("2024-W54-1"))
        def t20() -> str:
            return repr(datetime.strptime("02-29", "%m-%d"))
        def t21() -> str:
            return repr(datetime.strptime("+05:3000", "%z"))
        def t22() -> str:
            return repr(time.fromisoformat("24:00"))
        def t23() -> str:
            return repr(datetime.min - timedelta(days=1))
        def t24() -> str:
            return repr(datetime.min.replace(tzinfo=timezone.utc).astimezone(timezone(timedelta(hours=-1))))
        def t25() -> str:
            return repr(datetime.fromisoformat("2024-1-1"))

        def main() -> None:
            for f in [t1, t2, t3, t4, t5, t6, t7, t8, t9, t10, t11, t12, t13, t14, t15, t16, t17, t18, t19, t20, t21, t22, t23, t24, t25]:
                try:
                    print(f())
                except (ValueError, TypeError, OverflowError) as e:
                    print(type(e).__name__, e)

        if __name__ == "__main__":
            main()
        "#,
        r#"ValueError offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24), not datetime.timedelta(days=1).
ValueError month must be in 1..12
ValueError hour must be in 0..23
ValueError month must be in 1..12
ValueError hour must be in 0..23
ValueError Invalid isoformat string: '2024-01-01T'
ValueError stray % in format '%Y%'
ValueError 'Q' is a bad directive in format '%Q'
ValueError stray % in format '%Y %'
ValueError time data '  2024' does not match format '%Y'
ValueError time data '2024' does not match format ' %Y'
ValueError day is out of range for month
ValueError unconverted data remains: x
TypeError can't subtract offset-naive and offset-aware datetimes
TypeError can't compare offset-naive and offset-aware datetimes
TypeError can't compare offset-naive and offset-aware times
OverflowError date value out of range
ValueError Unknown timespec value
ValueError Invalid isoformat string: '2024-W54-1'
ValueError day is out of range for month
ValueError Inconsistent use of : in +05:3000
ValueError hour must be in 0..23
OverflowError date value out of range
OverflowError date value out of range
ValueError Invalid isoformat string: '2024-1-1'
"#,
    );
}

/// datetime: `timedelta` normalization and arithmetic, and `date`.
#[test]
fn datetime_timedelta_and_date() {
    prints(
        r#"
        import datetime
        from datetime import date, timedelta

        def main() -> None:
            td = timedelta(days=1, hours=1.5, minutes=-3, seconds=0.25, microseconds=7)
            print(repr(td), str(td), td.total_seconds(), td.days, td.seconds, td.microseconds)
            print(repr(-td), str(-td), repr(abs(-td)), repr(td * 2), repr(2 * td), repr(td * 0.5))
            print(td / timedelta(hours=1), repr(td / 3), td // timedelta(hours=1), repr(td // 3), repr(td % timedelta(hours=1)))
            print(repr(timedelta(0)), bool(timedelta(0)), bool(td), td > timedelta(days=1), td == td)
            print(repr(timedelta.max), repr(timedelta.min), repr(timedelta.resolution))
            print(str(timedelta(days=-1, seconds=5)), str(timedelta(weeks=3)), str(timedelta(microseconds=-1)))
            d = date(2024, 2, 28)
            print(repr(d), str(d), d.isoformat(), d.weekday(), d.isoweekday(), d.isocalendar(), d.toordinal())
            print(repr(d + timedelta(days=2)), repr(d - timedelta(days=60)), repr(date(2024, 3, 1) - d))
            print(repr(date.fromordinal(1)), repr(date.fromisoformat("2024-03-15")), repr(date.fromisoformat("20240315")))
            print(repr(date.fromisoformat("2024-W11-5")), repr(date.fromisocalendar(2020, 53, 7)))
            print(d.strftime("%A %d %B %Y, %a %b %j %U %W"), d.ctime(), f"{d:%d/%m/%Y}", f"{d}")
            print(repr(d.replace(year=2023, day=1)), d < date(2024, 3, 1))
            days = [date(2024, 5, 1), date(2023, 1, 1), date(2024, 1, 15)]
            print([str(x) for x in days])
            print(repr(date.min), repr(date.max), repr(date.resolution), date(2024, 1, 1) == date(2024, 1, 1))
            print(repr(datetime.date(1999, 12, 31)), repr(datetime.timedelta(seconds=90)))
            print(datetime.MINYEAR, datetime.MAXYEAR)
            print(type(d).__name__, repr(type(td)))
            iso = d.isocalendar()
            print(iso.year, iso.week, iso.weekday, iso[1], repr(iso))

        if __name__ == "__main__":
            main()
        "#,
        r#"datetime.timedelta(days=1, seconds=5220, microseconds=250007) 1 day, 1:27:00.250007 91620.250007 1 5220 250007
datetime.timedelta(days=-2, seconds=81179, microseconds=749993) -2 days, 22:32:59.749993 datetime.timedelta(days=1, seconds=5220, microseconds=250007) datetime.timedelta(days=2, seconds=10440, microseconds=500014) datetime.timedelta(days=2, seconds=10440, microseconds=500014) datetime.timedelta(seconds=45810, microseconds=125004)
25.450069446388888 datetime.timedelta(seconds=30540, microseconds=83336) 25 datetime.timedelta(seconds=30540, microseconds=83335) datetime.timedelta(seconds=1620, microseconds=250007)
datetime.timedelta(0) False True True True
datetime.timedelta(days=999999999, seconds=86399, microseconds=999999) datetime.timedelta(days=-999999999) datetime.timedelta(microseconds=1)
-1 day, 0:00:05 21 days, 0:00:00 -1 day, 23:59:59.999999
datetime.date(2024, 2, 28) 2024-02-28 2024-02-28 2 3 datetime.IsoCalendarDate(year=2024, week=9, weekday=3) 738944
datetime.date(2024, 3, 1) datetime.date(2023, 12, 30) datetime.timedelta(days=2)
datetime.date(1, 1, 1) datetime.date(2024, 3, 15) datetime.date(2024, 3, 15)
datetime.date(2024, 3, 15) datetime.date(2021, 1, 3)
Wednesday 28 February 2024, Wed Feb 059 08 09 Wed Feb 28 00:00:00 2024 28/02/2024 2024-02-28
datetime.date(2023, 2, 1) True
['2024-05-01', '2023-01-01', '2024-01-15']
datetime.date(1, 1, 1) datetime.date(9999, 12, 31) datetime.timedelta(days=1) True
datetime.date(1999, 12, 31) datetime.timedelta(seconds=90)
1 9999
date <class 'datetime.timedelta'>
2024 9 3 9 datetime.IsoCalendarDate(year=2024, week=9, weekday=3)
"#,
    );
}

/// datetime: The classes as a program uses them: fields, parameters, loops, collections, and the clock.
#[test]
fn datetime_in_programs() {
    prints(
        r#"
        from datetime import date, datetime, timedelta, timezone
        from typing import Optional

        class Event:
            def __init__(self, name: str, when: datetime) -> None:
                self.name = name
                self.when = when

            def later(self, by: timedelta) -> "Event":
                return Event(self.name, self.when + by)

        def days_between(a: date, b: date) -> int:
            return (b - a).days

        def first(xs: list[date]) -> Optional[date]:
            if xs:
                return xs[0]
            return None

        def main() -> None:
            now = datetime.now()
            print(now.year >= 2024, date.today().year >= 2024, datetime.now(timezone.utc).tzinfo is timezone.utc)
            e = Event("launch", datetime(2024, 5, 1, 9, 30))
            print(e.later(timedelta(hours=36)).when, e.name)
            print(days_between(date(2024, 1, 1), date(2024, 12, 25)))
            xs = [date(2024, 1, 1), date(2024, 2, 1)]
            print(xs, repr(xs[1]), first(xs), first([]))
            spans = {"a": timedelta(1), "b": timedelta(hours=2)}
            print(spans["b"], len(spans))
            total = timedelta(0)
            for s in spans.values():
                total += s
            print(total)
            d = date(2024, 1, 31)
            n = 0
            while d < date(2024, 3, 1):
                d += timedelta(days=7)
                n += 1
            print(d, n)
            print([str(date(2024, m, 1)) for m in range(1, 4)])

        if __name__ == "__main__":
            main()
        "#,
        r#"True True True
2024-05-02 21:30:00 launch
359
[datetime.date(2024, 1, 1), datetime.date(2024, 2, 1)] datetime.date(2024, 2, 1) 2024-01-01 None
2:00:00 2
1 day, 2:00:00
2024-03-06 5
['2024-01-01', '2024-02-01', '2024-03-01']
"#,
    );
}

/// logging: The module's functions on the root logger (configured by the first), loggers by name in their hierarchy, and their reprs.
#[test]
fn logging_module_functions() {
    streams(
        r#"
        import logging

        log = logging.getLogger(__name__)

        def work(n: int) -> int:
            log.info("working on %d", n)
            log.warning("careful with %s and %d", "x", n)
            return n * 2

        def main() -> None:
            logging.warning("no config yet")
            logging.info("hidden")
            logging.error("error %d%%", 5)
            print(work(3))
            child = logging.getLogger("__main__.child")
            child.error("from child %r", "c")
            print(log, child, logging.getLogger())
            print(child.parent is log, log.parent is logging.root, logging.getLogger("") is logging.root)

        if __name__ == "__main__":
            main()
        "#,
        r#"6
<Logger __main__ (WARNING)> <Logger __main__.child (WARNING)> <RootLogger root (WARNING)>
True True True
"#,
        r#"WARNING:root:no config yet
ERROR:root:error 5%
WARNING:__main__:careful with x and 3
ERROR:__main__.child:from child 'c'
"#,
    );
}

/// logging: Handlers of the program's, the three format styles, filters, propagation, levels, `disable()`, and `lastResort`.
#[test]
fn logging_handlers_formatters_filters() {
    streams(
        r#"
        import sys
        import logging
        from logging import Formatter, Handler, LogRecord, getLogger


        class ListHandler(Handler):
            def __init__(self) -> None:
                super().__init__()
                self.lines: list[str] = []

            def emit(self, record: LogRecord) -> None:
                self.lines.append(self.format(record))


        class OnlyErrors(logging.Filter):
            def filter(self, record: LogRecord) -> bool:
                return record.levelno >= logging.ERROR


        def nested() -> None:
            log = getLogger("app.nested")

            def inner() -> None:
                log.warning("inside %s", "inner")

            inner()
            f = lambda: log.error("from a lambda")
            f()


        def main() -> None:
            getLogger("plain").warning("no handler anywhere: %d", 1)
            getLogger("plain").info("dropped by lastResort")
            app = getLogger("app")
            app.setLevel(logging.DEBUG)
            h = ListHandler()
            h.setFormatter(Formatter("%(levelname)-8s|%(name)s|%(funcName)s:%(lineno)d|%(message)s"))
            app.addHandler(h)
            app.debug("debug %s %r", "one", "two")
            app.info("mapping %(a)s-%(b)s", {"a": "x", "b": "y"})
            app.log(25, "custom level")
            nested()
            child = app.getChild("child")
            child.propagate = False
            child.error("not propagated")
            print(child.hasHandlers(), app.hasHandlers(), child.getEffectiveLevel(), child.name)
            h2 = ListHandler()
            h2.setFormatter(Formatter("{levelname}/{name}/{message!r}/{levelno:03d}", style="{"))
            h2.addFilter(OnlyErrors())
            app.addHandler(h2)
            app.warning("warn %d", 3)
            app.error("err")
            h3 = ListHandler()
            h3.setFormatter(Formatter("$levelname ${name}: $message $$", style="$"))
            h3.setLevel("WARNING")
            app.addHandler(h3)
            app.info("info for h only")
            app.critical("critical %.2f", 2.5)
            app.removeHandler(h2)
            flt = logging.Filter("app.sub")
            h.addFilter(flt)
            app.error("filtered out of h")
            getLogger("app.sub.x").error("passes the filter")
            h.removeFilter(flt)
            logging.disable(logging.ERROR)
            app.error("disabled")
            app.critical("still on")
            logging.disable(logging.NOTSET)
            app.disabled = True
            app.critical("logger disabled")
            app.disabled = False
            for line in h.lines:
                print(line)
            print(h2.lines)
            print(h3.lines)
            print(logging.getLevelName(10), logging.getLevelName(25), logging.getLevelName(logging.FATAL))
            print(repr(h3), repr(logging.StreamHandler(sys.stdout)), repr(logging.NullHandler()))
            print(app.isEnabledFor(logging.DEBUG), getLogger("other").isEnabledFor(logging.INFO))
            try:
                Formatter("no fields here")
            except ValueError as e:
                print("ValueError", e)
            try:
                Formatter("{levelname!x}", style="{")
            except ValueError as e:
                print("ValueError", e)
            try:
                app.setLevel("LOUD")
            except ValueError as e:
                print("ValueError", e)


        if __name__ == "__main__":
            main()
        "#,
        r#"False True 10 app.child
DEBUG   |app|main:39|debug one 'two'
INFO    |app|main:40|mapping x-y
Level 25|app|main:41|custom level
WARNING |app.nested|inner:24|inside inner
ERROR   |app.nested|<lambda>:27|from a lambda
WARNING |app|main:51|warn 3
ERROR   |app|main:52|err
INFO    |app|main:57|info for h only
CRITICAL|app|main:58|critical 2.50
ERROR   |app.sub.x|main:63|passes the filter
CRITICAL|app|main:67|still on
["ERROR/app/'err'/040", "CRITICAL/app/'critical 2.50'/050"]
['CRITICAL app: critical 2.50 $', 'ERROR app: filtered out of h $', 'ERROR app.sub.x: passes the filter $', 'CRITICAL app: still on $']
DEBUG Level 25 CRITICAL
<ListHandler (WARNING)> <StreamHandler <stdout> (NOTSET)> <NullHandler (NOTSET)>
True False
ValueError Invalid format 'no fields here' for '%' style
ValueError invalid format: invalid conversion: 'x'
ValueError Unknown level: 'LOUD'
"#,
        r#"no handler anywhere: 1
not propagated
"#,
    );
}

/// logging: `basicConfig()` taking effect once, `force=True`, a stream, a level name, and `FileHandler`'s modes and `delay`.
#[test]
fn logging_basic_config_and_files() {
    streams(
        r#"
        import sys
        import logging

        FMT = "[%(levelname)s] %(name)s: %(message)s"


        def main() -> None:
            logging.basicConfig(level=logging.INFO, format=FMT, datefmt="%Y")
            logging.debug("hidden")
            logging.info("shown %s", "info")
            logging.basicConfig(level=logging.DEBUG, format="ignored %(message)s")
            logging.debug("still hidden")
            logging.basicConfig(force=True, stream=sys.stdout, style="{", format="{levelname}:{message}", level="DEBUG")
            logging.debug("now on stdout")
            log = logging.getLogger("files")
            fh = logging.FileHandler("app.log", mode="w")
            fh.setFormatter(logging.Formatter("%(levelno)d %(message)s"))
            log.addHandler(fh)
            log.warning("one")
            log.error("two %d", 2)
            with open("app.log") as f:
                print(f.read())
            fa = logging.FileHandler("app.log")
            log.removeHandler(fh)
            log.addHandler(fa)
            log.critical("three")
            with open("app.log") as f:
                print(f.read())
            d = logging.FileHandler("later.log", delay=True)
            log.addHandler(d)
            log.info("four")
            log.error("five")
            with open("later.log") as f:
                print(repr(f.read()))
            print(logging.root.level, len(logging.root.handlers), logging.getLogger().getEffectiveLevel())
            logging.shutdown()


        if __name__ == "__main__":
            main()
        "#,
        r#"DEBUG:now on stdout
WARNING:one
ERROR:two 2
30 one
40 two 2

CRITICAL:three
30 one
40 two 2
three

INFO:four
ERROR:five
'four\nfive\n'
10 1 10
"#,
        r#"[INFO] root: shown info
"#,
    );
}

/// logging: `basicConfig(handlers=...)` and `filename=`, a `Formatter` subclass, and what a record holds.
#[test]
fn logging_handlers_and_records() {
    streams(
        r#"
        import sys
        import logging


        class Upper(logging.Formatter):
            def format(self, record: logging.LogRecord) -> str:
                return super().format(record).upper()


        class Keep(logging.Handler):
            def __init__(self) -> None:
                super().__init__()
                self.lines: list[str] = []

            def emit(self, record: logging.LogRecord) -> None:
                self.lines.append(record.getMessage() + "|" + record.msg + "|" + record.levelname
                                  + "|" + record.funcName + "|" + str(record.taskName) + "|" + record.threadName)


        def main() -> None:
            out = logging.StreamHandler(sys.stdout)
            out.setFormatter(Upper("%(name)s says %(message)s"))
            logging.basicConfig(handlers=[out, logging.FileHandler("run.log", "w")], level=logging.INFO,
                                format="%(levelname)s %(message)s")
            logging.info("hello %s", "there")
            logging.getLogger("svc").warning("careful")
            with open("run.log") as f:
                print(f.read(), end="")
            logging.basicConfig(filename="other.log", force=True, filemode="w")
            logging.warning("to the file")
            logging.error("again")
            with open("other.log") as f:
                print(f.read(), end="")
            print(type(logging.root.handlers[0]).__name__, logging.root.handlers[0].formatter is not None)
            k = Keep()
            logging.getLogger("svc").addHandler(k)
            logging.getLogger("svc").error("x=%d", 7)
            print(k.lines)


        if __name__ == "__main__":
            main()
        "#,
        r#"ROOT SAYS HELLO THERE
SVC SAYS CAREFUL
INFO hello there
WARNING careful
WARNING:root:to the file
ERROR:root:again
FileHandler True
['x=7|x=%d|ERROR|main|None|MainThread']
"#,
        r#""#,
    );
}

/// logging: The module under another name, and names imported from it.
#[test]
fn logging_names() {
    streams(
        r#"
        import logging as lg
        from logging import getLogger, INFO, StreamHandler, Formatter
        import sys

        def main() -> None:
            h = StreamHandler(sys.stdout)
            h.setFormatter(Formatter("%(levelname)s %(name)s %(message)s"))
            a = getLogger("x")
            a.addHandler(h)
            a.setLevel(INFO)
            a.info("via alias %d", lg.INFO)
            lg.getLogger("x").warning("again")

        if __name__ == "__main__":
            main()
        "#,
        r#"INFO x via alias 20
WARNING x again
"#,
        r#""#,
    );
}

/// logging: a record's time, from the host's clock: `asctime` in the default
/// format (`%Y-%m-%d %H:%M:%S,mmm`, local time being UTC) and in a `datefmt`,
/// with `msecs` and `created` agreeing with it.
#[test]
fn logging_record_times() {
    let run = run_main(
        &dedent(
            r#"
            import sys
            import logging

            def main() -> None:
                logging.basicConfig(stream=sys.stdout,
                                    format="%(asctime)s|%(msecs)03d|%(created).0f|%(relativeCreated)d|%(process)d|%(message)s")
                logging.warning("hi")
                h = logging.StreamHandler(sys.stdout)
                h.setFormatter(logging.Formatter("%(asctime)s %(message)s", datefmt="%Y-%m-%dT%H:%M:%S"))
                logging.getLogger("t").addHandler(h)
                logging.getLogger("t").error("there")

            if __name__ == "__main__":
                main()
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stderr, "");
    let lines: Vec<&str> = run.stdout.lines().collect();
    let first = regex::Regex::new(
        r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}),(\d{3})\|(\d{3})\|(\d+)\|(\d+)\|42\|hi$",
    )
    .unwrap();
    let caps = first.captures(lines[0]).expect(lines[0]);
    assert_eq!(&caps[2], &caps[3], "asctime's milliseconds are msecs");
    let second = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2} there$").unwrap();
    assert!(second.is_match(lines[1]), "{}", lines[1]);
    // The second record also reaches the root's handler.
    assert!(lines[2].ends_with("|there"), "{}", lines[2]);
}

/// logging: what CPython does that a compiled module cannot is refused, each
/// with why.
#[test]
fn logging_refusals() {
    let refused = |body: &str, why: &str| {
        let src = format!(
            "import logging\nlog = logging.getLogger('a')\n\ndef main() -> None:\n    {body}\n"
        );
        let err = try_compile(&src).expect_err(body);
        assert!(err.contains(why), "{body}: {err}");
        assert_eq!(err.matches(why).count(), 1, "reported once: {err}");
    };
    refused("logging.exception('x')", "logs the traceback");
    refused("log.warn('x')", "DeprecationWarning");
    refused("log.info('x', extra={'a': 1})", "extra=");
    refused("log.error('x', exc_info=True)", "exc_info");
    refused("logging.Formatter('%(pathname)s')", "names the source file");
    refused(
        "logging.Formatter('%(nothing)s')",
        "has no attribute 'nothing'",
    );
    refused(
        "logging.basicConfig(colour=1)",
        "Unrecognised argument(s): colour",
    );
    refused("logging.addLevelName(5, 'TRACE')", "logging.addLevelName");
    refused(
        "log.info('%d %d', 1)",
        "not enough arguments for format string",
    );
    refused("f = 'a%s'\n    log.info(f, 1)", "written as a literal");
    refused("logging.basicConfig(level='LOUD')", "Unknown level");
}

/// The module's functions, Pattern and Match objects, flags, lookarounds, and templates.
#[test]
fn re_functions_and_objects() {
    streams(
        r#"
        import re

        def main() -> None:
            m = re.search(r"(\d+)-(\d+)", "call 555-1234 now")
            if m:
                print(m.group(), m.group(1), m.group(2), m.span(), m.start(1), m.end(2), m.groups())
            print(re.match(r"a+", "aaab"), re.match(r"b", "aaab"), re.fullmatch(r"a+b", "aaab"))
            print(re.findall(r"\w+", "hello, wörld 42!"), re.findall(r"(\w)(\d)", "a1 b2 c"), re.findall(r"(a)", "aXa"))
            print(re.split(r"\s*,\s*", "a , b,c ,d"), re.split(r"(,)", "a,b"), re.split(r"x*", "axbc"))
            print(re.sub(r"(\w+)@(\w+)", r"\2 at \1", "me@home you@work"), re.subn(r"o", "0", "foo boo"))
            print(re.sub(r"x*", "-", "abxd"), re.sub(r"\d", lambda m: str(int(m.group()) * 2), "a1b2c3"))
            p = re.compile(r"(?P<word>[a-z]+)(?P<num>\d*)", re.IGNORECASE)
            m2 = p.match("Hello99 there")
            if m2:
                print(m2.group("word"), m2["num"], m2.groupdict(), m2.lastindex, m2.lastgroup, m2.re is p)
            print(p, re.compile("a"), repr(m2))
            print([x.group() for x in re.finditer(r"\d+", "1 22 333")])
            print(re.search(r"^b", "a\nb", re.M), re.search(r"a.b", "a\nb"), re.search(r"a.b", "a\nb", re.S))
            print(re.findall(r"(?i)straße", "STRASSE straße STRAßE"), re.findall(r"[a-c]+", "ABCabc", re.I))
            print(re.findall(r"\bfoo\b", "foo foobar barfoo foo."), re.findall(r"(?<=a)b", "ab cb ab"), re.findall(r"a(?!b)", "ab ac a"))
            print(re.match(r"(a|ab)(c|bcd)(d*)", "abcd").groups(), re.match(r"(a*)*", "aaa").span(), re.match(r"(a*?)+?b", "aab").groups())
            print(re.sub(r"(?P<x>a)", r"[\g<x>\g<0>]", "banana"), re.escape("a.b*c"), re.findall(r"(a)?b", "b ab"))

        if __name__ == "__main__":
            main()
        "#,
        r#"555-1234 555 1234 (5, 13) 5 13 ('555', '1234')
<re.Match object; span=(0, 3), match='aaa'> None <re.Match object; span=(0, 4), match='aaab'>
['hello', 'wörld', '42'] [('a', '1'), ('b', '2')] ['a', 'a']
['a', 'b', 'c', 'd'] ['a', ',', 'b'] ['', 'a', '', 'b', 'c', '']
home at me work at you ('f00 b00', 4)
-a-b--d- a2b4c6
Hello 99 {'word': 'Hello', 'num': '99'} 2 num True
re.compile('(?P<word>[a-z]+)(?P<num>\\d*)', re.IGNORECASE) re.compile('a') <re.Match object; span=(0, 7), match='Hello99'>
['1', '22', '333']
<re.Match object; span=(2, 3), match='b'> None <re.Match object; span=(0, 3), match='a\nb'>
['straße', 'STRAßE'] ['ABCabc']
['foo', 'foo'] ['b', 'b'] ['a', 'a']
('a', 'bcd', '') (0, 3) ('a',)
b[aa]n[aa]n[aa] a\.b\*c ['', 'a']
"#,
        r#""#,
    );
}

/// Backtracking as sre does it: lazy, possessive, and atomic repeats, conditionals, back references, empty matches, `lastindex`, `pos`/`endpos`, and case folding.
#[test]
fn re_matching_semantics() {
    streams(
        r#"
        import re
        from typing import Optional

        def show(m: Optional[re.Match]) -> str:
            if m is None:
                return "None"
            return str(m.span()) + str(m.groups())

        def main() -> None:
            print(re.findall(r"a.*?b", "aXbYb ab"), re.findall(r"<.+?>", "<a><bb>"), re.findall(r"a{2,3}", "aaaaaaa"))
            print(re.findall(r"a{,2}", "aaab"), re.findall(r"x{2}y{1,}", "xxyy xy xxy"), re.findall(r"a++b", "aaab aab"))
            print(re.match(r"(?>a+)ab", "aaab"), re.match(r"(?>a+)b", "aaab"), re.match(r"a*+a", "aaa"))
            print(re.findall(r"(a)?(?(1)b|c)", "ab c ac"), re.match(r"(?P<q>['\"]).*?(?P=q)", "'hi' there").group())
            print(re.findall(r"(\w)\1", "aabbcd ee"), re.findall(r"(?i)(a)\1", "aA Aa ab"), re.search(r"(?<!\$)\b\d+", "$12 34").group())
            print(re.findall(r"""(?x) \d+  # digits
                                 \s* [a-z]+  # word""", "12 ab 3cd"), re.findall(r"(?s:.)x", "\nx ax"))
            print(re.findall(r"\d", "١٢٣ 45"), re.findall(r"(?a)\d", "١٢٣ 45"), re.findall(r"\w+", "ŝtrange_ünicode", re.A))
            print(re.split(r"\b", "a bc"), re.split(r"", "abc"), re.findall(r"", "ab"), re.sub(r"", "-", "ab"))
            print(re.findall(r"(a|b)*c", "ababc"), re.match(r"(a|b)*c", "ababc").group(1), re.match(r"(?:(a)|b)*", "ab").groups())
            print(re.match(r"(a)(b)?", "a").lastindex, re.match(r"(a)|(b)", "b").lastindex, re.match(r"((a)b)", "ab").lastindex)
            p = re.compile(r"o")
            print(p.search("foo", 2), p.search("foo", 0, 1), p.findall("ooooo", 1, 3), p.match("xo", 1), p.fullmatch("xoo", 1))
            print(re.sub(r"a", r"\n", "bab") == "b\nb", re.sub(r"(a)", r"\1\1", "abc", count=1), re.subn(r"\s+", " ", "a  b   c"))
            try:
                re.sub(r"(a)", r"\2", "a")
            except re.error as e:
                print("error:", e)
            try:
                re.sub(r"(a)", r"\g<nope>", "a")
            except IndexError as e:
                print("IndexError:", e)
            print(re.findall(r"[^\W\d]+", "ab12 cd"), re.findall(r"[\d.-]+", "1.5-2 x"), re.findall(r"[]a]", "]a["), re.findall(r"[a-]+", "a-b"))
            print(re.match(r"^$", ""), re.search(r"$", "a\n").span(), re.findall(r"^\w", "a\nb\nc", re.M), re.search(r"\Z", "a\n").span())
            print(re.findall(r"(?=(\w\w))", "abcd"), re.match(r"(?=a)a", "a").span(), re.search(r"(?<=ab)c", "abc").span())
            print(re.match(r"(a*)+$", "aaaa").groups(), re.match(r"(a|)+b", "aab").groups(), re.match(r"(?:a?)*?y", "ay").span())
            print(re.match(r"(\d+)\.(\d+)", "3.14").expand(r"\2 then \1"), re.fullmatch(r"[A-Z]{2}\d{4}", "AB1234") is not None)
            print(re.findall(r"(?i)[k]", "kKK"), re.findall(r"(?i)s", "sSſ"), re.findall(r"(?i:ß)", "ßẞ"))

        if __name__ == "__main__":
            main()
        "#,
        r#"['aXb', 'ab'] ['<a>', '<bb>'] ['aaa', 'aaa']
['aa', 'a', '', ''] ['xxyy', 'xxy'] ['aaab', 'aab']
None <re.Match object; span=(0, 4), match='aaab'> None
['a', '', ''] 'hi'
['a', 'b', 'e'] ['a', 'A'] 34
['12 ab', '3cd'] ['\nx', 'ax']
['١', '٢', '٣', '4', '5'] ['4', '5'] ['trange_', 'nicode']
['', 'a', ' ', 'bc', ''] ['', 'a', 'b', 'c', ''] ['', '', ''] -a-b-
['b'] b ('a',)
1 2 1
<re.Match object; span=(2, 3), match='o'> None ['o', 'o'] <re.Match object; span=(1, 2), match='o'> None
True aabc ('a b c', 2)
error: invalid group reference 2 at position 1
IndexError: unknown group name 'nope'
['ab', 'cd'] ['1.5-2'] [']', 'a'] ['a-']
<re.Match object; span=(0, 0), match=''> (1, 1) ['a', 'b', 'c'] (2, 2)
['ab', 'bc', 'cd'] (0, 1) (2, 3)
('',) ('',) (0, 2)
14 then 3 True
['k', 'K', 'K'] ['s', 'S', 'ſ'] ['ß', 'ẞ']
"#,
        r#""#,
    );
}

/// Patterns as programs use them: a module constant, groups used as strings, unpacking, and splitting.
#[test]
fn re_in_programs() {
    streams(
        r#"
        import re

        LINE = re.compile(r"(?P<key>\w+)\s*=\s*(?P<val>\d+)")

        def parse(text: str) -> dict[str, int]:
            out: dict[str, int] = {}
            for line in text.split("\n"):
                m = LINE.match(line)
                if m:
                    out[m.group("key")] = int(m.group("val"))
            return out

        def main() -> None:
            print(parse("a = 1\nbb=22\nbad\nc= 3"))
            names: list[str] = []
            for m in re.finditer(r"([A-Z])\w*", "Alice and Bob met Carol"):
                names.append(m.group(1) + "!")
                print(len(m.group(0)), f"<{m.group(1)}>", m.group(0).lower(), m.group(0) == "Bob")
            print(names)
            m = re.match(r"(\d+)-(\d+)", "10-20")
            if m:
                a, b = m.groups()
                print(a, b, int(a) + int(b))
                lo, hi = m.span(1)
                print(lo, hi)
            words = re.split(r"[,;]\s*", "x, y;z")
            print(words, len(words), " ".join(words))
            parts = re.split(r"(-)", "1-2")
            print(parts)

        if __name__ == "__main__":
            main()
        "#,
        r#"{'a': 1, 'bb': 22, 'c': 3}
5 <A> alice False
3 <B> bob True
5 <C> carol False
['A!', 'B!', 'C!']
10 20 30
0 2
['x', 'y', 'z'] 3 x y z
['1', '-', '2']
"#,
        r#""#,
    );
}

/// `ord`, `chr`, and an optional string's truth.
#[test]
fn ord_chr_and_optional_text() {
    streams(
        r#"
        from typing import Optional

        def opt(x: Optional[str]) -> str:
            if x:
                return "yes"
            return "no"

        def main() -> None:
            s = "héllo€😀"
            codes = [ord(c) for c in s]
            chars = list(s)
            print(codes, chars, chr(233), chr(104).isalnum(), chr(0x1F600), chr(8364), chr(127), chr(0x7ff))
            print("".join([chr(c) for c in codes]) == s)
            xs = [1, 2, 3, 4, 5]
            del xs[2:]
            print(xs, tuple(chars), opt(""), opt(None), opt("a"))
            n: Optional[str] = None
            print(f"{n}", str(n))
            try:
                ord("ab")
            except TypeError as e:
                print(e)
            try:
                chr(-1)
            except ValueError as e:
                print(e)

        if __name__ == "__main__":
            main()
        "#,
        r#"[104, 233, 108, 108, 111, 8364, 128512] ['h', 'é', 'l', 'l', 'o', '€', '😀'] é True 😀 €  ߿
True
[1, 2] ('h', 'é', 'l', 'l', 'o', '€', '😀') no no yes
None None
ord() expected a character, but string of length 2 found
chr() arg not in range(0x110000)
"#,
        r#""#,
    );
}

/// re: a pattern CPython rejects is reported with `re.error`'s message, and
/// what cannot be compiled with the program is refused.
#[test]
fn re_refusals() {
    let refused = |body: &str, why: &str| {
        let src = format!("import re\n\ndef main() -> None:\n    {body}\n");
        let err = try_compile(&src).expect_err(body);
        assert!(err.contains(why), "{body}: {err}");
    };
    refused(
        r"re.compile('(')",
        "re.error: missing ), unterminated subpattern at position 0",
    );
    refused(
        r"re.compile('a**')",
        "re.error: multiple repeat at position 2",
    );
    refused(
        r"re.compile('(?<=a+)b')",
        "re.error: look-behind requires fixed-width pattern",
    );
    refused(
        r"re.compile('[z-a]')",
        "re.error: bad character range z-a at position 1",
    );
    refused(
        r"re.compile('a(?i)b')",
        "global flags not at the start of the expression at position 1",
    );
    refused(
        "p = input()\n    re.search(p, 'x')",
        "written as a string literal or a module constant",
    );
    refused("re.sub('a', 'b', 'aa', 1)", "DeprecationWarning");
    refused("re.split(',', 'a,b', 1)", "DeprecationWarning");
    refused("re.compile('[[a]')", "FutureWarning");
}
