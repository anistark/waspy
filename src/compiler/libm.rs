//! The C math library, as runtime helpers.
//!
//! CPython's `math` module calls the platform libm, so a result can differ in
//! its last bit from one platform to the next. The reference here is musl's
//! libm (what CPython on WASI links), through its Rust port in the `libm`
//! crate: each routine below is that code, line for line, over the builder
//! in `asm`. The `libm` crate is also what the tests check these against,
//! bit for bit. `sqrt`, `floor`, `ceil`, `trunc`, `fabs`, and `copysign` are
//! single WASM instructions and need no helper.
//!
//! Two routines are CPython's own rather than libm's, because CPython does
//! not call libm for them: `hypot` (its `vector_norm`) and `remainder`
//! (`m_remainder`).
//!
//! The routines keep their working arrays and the two-part results of the
//! argument reduction in `LIBM`, a region of the unused space below the heap.
//! None of them calls back into user code, so one region serves every call.

// The constants are fdlibm's, digit for digit as musl and the `libm` crate
// spell them (some of them roundings of named constants), and `x - x` and
// `0.0 / 0.0` are how the C builds a NaN: the port keeps both as written.
#![allow(clippy::excessive_precision, clippy::approx_constant, clippy::eq_op)]

use crate::compiler::asm::{call, fbits, words, Asm, Ty, Var, E, X};
use crate::compiler::context::CompilationContext;
use crate::compiler::runtime::Helper;
use wasm_encoder::Function;

/// The routines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Libm {
    Sin,
    Cos,
    Tan,
    /// `(x, y, iy) -> f64`: sin on [-pi/4, pi/4], `x + y` the argument.
    KSin,
    /// `(x, y) -> f64`.
    KCos,
    /// `(x, y, odd) -> f64`: tan, or -1/tan when `odd`.
    KTan,
    /// `(x) -> n`: `x - n*pi/2` in `Y0 + Y1`.
    RemPio2,
    /// `(e0, nx) -> n`: Payne-Hanek reduction of the `nx` 24-bit chunks at
    /// `TX`, into `Y0 + Y1`.
    RemPio2Large,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sinh,
    Cosh,
    Tanh,
    Exp,
    Expm1,
    /// `exp(x)/2` for large `x`.
    Expo2,
    Log,
    Log10,
    Log2,
    Pow,
    Fmod,
    /// `(x, n: i32) -> f64`: `x * 2**n`.
    Scalbn,
    /// `(n) -> f64`: CPython's `vector_norm` of the `n` absolute values at
    /// `VEC`, the hypotenuse.
    VectorNorm,
    /// CPython's `m_remainder`.
    Remainder,
    /// CPython's `float_pow`, the `**` operator on floats: it leaves
    /// `ZeroDivisionError` or `OverflowError` pending (answering 0) where
    /// CPython raises, and traps where CPython's answer is a complex number.
    FloatPow,
}

/// The routines' memory, below the heap and past the host helpers' state.
pub(crate) const LIBM: i32 = 110592;
/// The two parts of a reduced argument.
const Y0: i32 = LIBM;
const Y1: i32 = LIBM + 8;
/// The 24-bit chunks of a large argument, three f64s.
const TX: i32 = LIBM + 16;
/// `rem_pio2_large`'s arrays: twenty f64s each, and twenty i32s.
const F: i32 = LIBM + 64;
const FQ: i32 = LIBM + 224;
const Q: i32 = LIBM + 384;
const IQ: i32 = LIBM + 544;
/// The tables, laid out by `data_for`.
const IPIO2_AT: i32 = LIBM + 640;
const PIO2_AT: i32 = LIBM + 912;
/// `hypot`'s coordinates.
pub(crate) const VEC: i32 = LIBM + 1024;
/// How many coordinates `VEC` holds.
pub(crate) const VEC_MAX: usize = 64;
pub(crate) const LIBM_END: i32 = VEC + VEC_MAX as i32 * 8;
const _: () = assert!(IPIO2_AT + 66 * 4 <= PIO2_AT);
const _: () = assert!(LIBM_END <= LIBM + 2048);

impl Libm {
    pub fn signature(self) -> (Vec<Ty>, Option<Ty>) {
        use Ty::{F64, I32};
        match self {
            Libm::KSin | Libm::KTan => (vec![F64, F64, I32], Some(F64)),
            Libm::KCos
            | Libm::Atan2
            | Libm::Pow
            | Libm::Fmod
            | Libm::Remainder
            | Libm::FloatPow => (vec![F64, F64], Some(F64)),
            Libm::RemPio2 => (vec![F64], Some(I32)),
            Libm::RemPio2Large => (vec![I32, I32], Some(I32)),
            Libm::Scalbn => (vec![F64, I32], Some(F64)),
            Libm::VectorNorm => (vec![I32], Some(F64)),
            _ => (vec![F64], Some(F64)),
        }
    }
}

/// A call to another routine.
fn lm(ctx: &CompilationContext, routine: Libm, args: Vec<E>) -> E {
    let (_, result) = routine.signature();
    call(ctx.helper(Helper::Libm(routine)), args, result)
}

/// The data segments the routines a module uses need.
pub(crate) fn data_for(helpers: &[Helper]) -> Vec<(u32, Vec<u8>)> {
    if !helpers.contains(&Helper::Libm(Libm::RemPio2Large)) {
        return Vec::new();
    }
    let ipio2: Vec<u8> = IPIO2.iter().flat_map(|w| w.to_le_bytes()).collect();
    let pio2: Vec<u8> = PIO2.iter().flat_map(|w| w.to_le_bytes()).collect();
    vec![(IPIO2_AT as u32, ipio2), (PIO2_AT as u32, pio2)]
}

pub(crate) fn build(ctx: &CompilationContext, routine: Libm) -> Function {
    let (params, _) = routine.signature();
    let mut a = Asm::new(&params);
    match routine {
        Libm::Sin => sin(&mut a, ctx),
        Libm::Cos => cos(&mut a, ctx),
        Libm::Tan => tan(&mut a, ctx),
        Libm::KSin => k_sin(&mut a),
        Libm::KCos => k_cos(&mut a),
        Libm::KTan => k_tan(&mut a),
        Libm::RemPio2 => rem_pio2(&mut a, ctx),
        Libm::RemPio2Large => rem_pio2_large(&mut a, ctx),
        Libm::Asin => asin(&mut a),
        Libm::Acos => acos(&mut a),
        Libm::Atan => atan(&mut a),
        Libm::Atan2 => atan2(&mut a, ctx),
        Libm::Sinh => sinh(&mut a, ctx),
        Libm::Cosh => cosh(&mut a, ctx),
        Libm::Tanh => tanh(&mut a, ctx),
        Libm::Exp => exp(&mut a, ctx),
        Libm::Expm1 => expm1(&mut a),
        Libm::Expo2 => expo2(&mut a, ctx),
        Libm::Log => log(&mut a, LogBase::E),
        Libm::Log10 => log(&mut a, LogBase::Ten),
        Libm::Log2 => log(&mut a, LogBase::Two),
        Libm::Pow => pow(&mut a, ctx),
        Libm::Fmod => fmod(&mut a),
        Libm::Scalbn => scalbn(&mut a),
        Libm::VectorNorm => vector_norm(&mut a),
        Libm::Remainder => remainder(&mut a, ctx),
        Libm::FloatPow => float_pow(&mut a, ctx),
    }
    a.finish()
}

fn f64_at(base: i32, index: impl Into<E>) -> E {
    (index.into() * 8 + base).load(Ty::F64, 0)
}

fn i32_at(base: i32, index: impl Into<E>) -> E {
    (index.into() * 4 + base).load(Ty::I32, 0)
}

fn set_f64_at(a: &mut Asm, base: i32, index: impl Into<E>, value: impl Into<E>) {
    a.store(Ty::F64, index.into() * 8 + base, 0, value);
}

fn set_i32_at(a: &mut Asm, base: i32, index: impl Into<E>, value: impl Into<E>) {
    a.store(Ty::I32, index.into() * 4 + base, 0, value);
}

// ---- sin, cos, tan --------------------------------------------------------

fn sin(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    let ix = a.let_(x.hi() & 0x7fffffff);
    a.if_(ix.le_u(0x3fe921fb), |a| {
        a.if_(ix.lt_u(0x3e500000), |a| a.ret(x));
        a.ret(lm(ctx, Libm::KSin, vec![x.e(), 0.0.into(), 0.into()]));
    });
    a.if_(ix.ge_u(0x7ff00000), |a| a.ret(x - x));
    let n = a.let_(lm(ctx, Libm::RemPio2, vec![x.e()]) & 3);
    let (y0, y1) = (f64_at(Y0, 0), f64_at(Y1, 0));
    let ks = lm(ctx, Libm::KSin, vec![y0.clone(), y1.clone(), 1.into()]);
    let kc = lm(ctx, Libm::KCos, vec![y0, y1]);
    a.if_(n.eq(0), |a| a.ret(ks.clone()));
    a.if_(n.eq(1), |a| a.ret(kc.clone()));
    a.if_(n.eq(2), |a| a.ret(-ks));
    a.ret(-kc);
}

fn cos(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    let ix = a.let_(x.hi() & 0x7fffffff);
    a.if_(ix.le_u(0x3fe921fb), |a| {
        // |x| < 2**-27 * sqrt(2): `x as i32 == 0`, so 1.
        a.if_(ix.lt_u(0x3e46a09e), |a| a.ret(1.0));
        a.ret(lm(ctx, Libm::KCos, vec![x.e(), 0.0.into()]));
    });
    a.if_(ix.ge_u(0x7ff00000), |a| a.ret(x - x));
    let n = a.let_(lm(ctx, Libm::RemPio2, vec![x.e()]) & 3);
    let (y0, y1) = (f64_at(Y0, 0), f64_at(Y1, 0));
    let ks = lm(ctx, Libm::KSin, vec![y0.clone(), y1.clone(), 1.into()]);
    let kc = lm(ctx, Libm::KCos, vec![y0, y1]);
    a.if_(n.eq(0), |a| a.ret(kc.clone()));
    a.if_(n.eq(1), |a| a.ret(-ks.clone()));
    a.if_(n.eq(2), |a| a.ret(-kc));
    a.ret(ks);
}

fn tan(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    let ix = a.let_(x.hi() & 0x7fffffff);
    a.if_(ix.le_u(0x3fe921fb), |a| {
        a.if_(ix.lt_u(0x3e400000), |a| a.ret(x));
        a.ret(lm(ctx, Libm::KTan, vec![x.e(), 0.0.into(), 0.into()]));
    });
    a.if_(ix.ge_u(0x7ff00000), |a| a.ret(x - x));
    let n = a.let_(lm(ctx, Libm::RemPio2, vec![x.e()]));
    a.ret(lm(
        ctx,
        Libm::KTan,
        vec![f64_at(Y0, 0), f64_at(Y1, 0), n & 1],
    ));
}

fn k_sin(a: &mut Asm) {
    const S1: f64 = -1.66666666666666324348e-01;
    const S2: f64 = 8.33333333332248946124e-03;
    const S3: f64 = -1.98412698298579493134e-04;
    const S4: f64 = 2.75573137070700676789e-06;
    const S5: f64 = -2.50507602534068634195e-08;
    const S6: f64 = 1.58969099521155010221e-10;
    let (x, y, iy) = (a.param(0), a.param(1), a.param(2));
    let z = a.let_(x * x);
    let w = a.let_(z * z);
    let r = a.let_((z * ((z * S4) + S3) + S2) + z * w * ((z * S6) + S5));
    let v = a.let_(z * x);
    a.if_(iy.eqz(), |a| a.ret(x + v * ((z * r) + S1)));
    a.ret(x - ((z * (y * 0.5 - v * r) - y) - v * S1));
}

fn k_cos(a: &mut Asm) {
    const C1: f64 = 4.16666666666666019037e-02;
    const C2: f64 = -1.38888888888741095749e-03;
    const C3: f64 = 2.48015872894767294178e-05;
    const C4: f64 = -2.75573143513906633035e-07;
    const C5: f64 = 2.08757232129817482790e-09;
    const C6: f64 = -1.13596475577881948265e-11;
    let (x, y) = (a.param(0), a.param(1));
    let z = a.let_(x * x);
    let w = a.let_(z * z);
    let r = a.let_(z * (z * (z * C3 + C2) + C1) + w * w * (z * (z * C6 + C5) + C4));
    let hz = a.let_(z * 0.5);
    let w = a.let_(1.0 - hz);
    a.ret(w + (((1.0 - w) - hz) + (z * r - x * y)));
}

fn k_tan(a: &mut Asm) {
    const T: [f64; 13] = [
        3.33333333333334091986e-01,
        1.33333333333201242699e-01,
        5.39682539762260521377e-02,
        2.18694882948595424599e-02,
        8.86323982359930005737e-03,
        3.59207910759131235356e-03,
        1.45620945432529025516e-03,
        5.88041240820264096874e-04,
        2.46463134818469906812e-04,
        7.81794442939557092300e-05,
        7.14072491382608190305e-05,
        -1.85586374855275456654e-05,
        2.59073051863633712884e-05,
    ];
    const PIO4: f64 = 7.85398163397448278999e-01;
    const PIO4_LO: f64 = 3.06161699786838301793e-17;
    let (x, y, odd) = (a.param(0), a.param(1), a.param(2));
    let hx = a.let_(x.hi());
    let big = a.let_((hx & 0x7fffffff).ge_u(0x3FE59428));
    a.if_(big, |a| {
        a.if_(hx.shr_u(31), |a| {
            a.set(x, -x);
            a.set(y, -y);
        });
        a.set(x, (PIO4 - x) + (PIO4_LO - y));
        a.set(y, 0.0);
    });
    let z = a.let_(x * x);
    let w = a.let_(z * z);
    let r = a.let_(w * (w * (w * (w * (w * T[11] + T[9]) + T[7]) + T[5]) + T[3]) + T[1]);
    let v = a.let_(z * (w * (w * (w * (w * (w * T[12] + T[10]) + T[8]) + T[6]) + T[4]) + T[2]));
    let s = a.let_(z * x);
    let r = a.let_(y + z * (s * (r + v) + y) + s * T[0]);
    let w = a.let_(x + r);
    a.if_(big, |a| {
        let s = a.let_(1.0 - odd.to_f64() * 2.0);
        let v = a.let_(s - (x + (r - w * w / (w + s))) * 2.0);
        a.ret(hx.shr_u(31).select(-v, v));
    });
    a.if_(odd.eqz(), |a| a.ret(w));
    // -1/(x+r) has up to 2ulp error, so compute it accurately.
    let w0 = a.let_(w.with_lo(0));
    let v = a.let_(r - (w0 - x));
    let t = a.let_(-1.0 / w);
    let a0 = a.let_(t.with_lo(0));
    a.ret(a0 + t * (a0 * w0 + 1.0 + a0 * v));
}

fn rem_pio2(a: &mut Asm, ctx: &CompilationContext) {
    const TO_INT: f64 = 1.5 / 2.2204460492503131e-16;
    const INV_PIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;
    let x = a.param(0);
    let sign = a.let_(x.bits().shr_u(63i64).wrap());
    let ix = a.let_(x.hi() & 0x7fffffff);
    let done = |a: &mut Asm, n: E, y0: E, y1: E| {
        a.store(Ty::F64, Y0, 0, y0);
        a.store(Ty::F64, Y1, 0, y1);
        a.ret(n);
    };
    let medium = |a: &mut Asm| {
        let f_n = a.let_((x * INV_PIO2 + TO_INT) - TO_INT);
        let n = a.let_(f_n.to_i32());
        let r = a.let_(x - f_n * PIO2_1);
        let w = a.let_(f_n * PIO2_1T);
        let y0 = a.let_(r - w);
        let ex = a.let_(ix.shr_u(20));
        let ey = |y0: Var| y0.bits().shr_u(52i64).wrap() & 0x7ff;
        a.if_((ex - ey(y0)).gt(16), |a| {
            let t = a.let_(r);
            a.set(w, f_n * PIO2_2);
            a.set(r, t - w);
            a.set(w, f_n * PIO2_2T - ((t - r) - w));
            a.set(y0, r - w);
            a.if_((ex - ey(y0)).gt(49), |a| {
                let t = a.let_(r);
                a.set(w, f_n * PIO2_3);
                a.set(r, t - w);
                a.set(w, f_n * PIO2_3T - ((t - r) - w));
                a.set(y0, r - w);
            });
        });
        done(a, n.e(), y0.e(), (r - y0) - w);
    };
    // A multiple of pi/2 near x, one round good to 85 bits.
    let near = |a: &mut Asm, k: i32| {
        let kf = k as f64;
        a.if_else(
            sign.eqz(),
            |a| {
                let z = a.let_(x - kf * PIO2_1);
                let y0 = a.let_(z - kf * PIO2_1T);
                done(a, k.into(), y0.e(), (z - y0) - kf * PIO2_1T);
            },
            |a| {
                let z = a.let_(x + kf * PIO2_1);
                let y0 = a.let_(z + kf * PIO2_1T);
                done(a, (-k).into(), y0.e(), (z - y0) + kf * PIO2_1T);
            },
        );
    };
    a.if_(ix.le_u(0x400f6a7a), |a| {
        // |x| ~<= 5pi/4
        a.if_((ix & 0xfffff).eq(0x921fb), medium);
        a.if_else(ix.le_u(0x4002d97c), |a| near(a, 1), |a| near(a, 2));
    });
    a.if_(ix.le_u(0x401c463b), |a| {
        // |x| ~<= 9pi/4
        a.if_else(
            ix.le_u(0x4015fdbc),
            |a| {
                a.if_(ix.eq(0x4012d97c), medium);
                near(a, 3);
            },
            |a| {
                a.if_(ix.eq(0x401921fb), medium);
                near(a, 4);
            },
        );
    });
    a.if_(ix.lt_u(0x413921fb), medium);
    a.if_(ix.ge_u(0x7ff00000), |a| {
        let y = a.let_(x - x);
        done(a, 0.into(), y.e(), y.e());
    });
    // z = scalbn(|x|, -ilogb(x) + 23), cut into 24-bit chunks.
    let z = a.let_(((x.bits() & 0x000f_ffff_ffff_ffffi64) | ((0x3ffi64 + 23) << 52)).bits_to_f64());
    for i in 0..2 {
        a.store(Ty::F64, TX + i * 8, 0, z.to_i32().to_f64());
        a.set(z, (z - f64_at(TX + i * 8, 0)) * fbits(0x4170000000000000));
    }
    a.store(Ty::F64, TX + 16, 0, z);
    let i = a.let_(2);
    a.while_(i.ne(0) & f64_at(TX, i).eq(0.0), |a, _| a.set(i, i - 1));
    let n = a.let_(lm(
        ctx,
        Libm::RemPio2Large,
        vec![ix.shr_u(20) - (0x3ff + 23), i + 1],
    ));
    a.if_(sign, |a| {
        a.store(Ty::F64, Y0, 0, -f64_at(Y0, 0));
        a.store(Ty::F64, Y1, 0, -f64_at(Y1, 0));
        a.ret(-n);
    });
    a.ret(n);
}

/// `x * 2**n` through the `Scalbn` helper.
fn scal(ctx: &CompilationContext, x: impl Into<E>, n: impl Into<E>) -> E {
    lm(ctx, Libm::Scalbn, vec![x.into(), n.into()])
}

fn rem_pio2_large(a: &mut Asm, ctx: &CompilationContext) {
    // prec = 1: jk = INIT_JK[1] = 4, jp = jk.
    const JK: i32 = 4;
    const JP: i32 = 4;
    let x1p24 = fbits(0x4170000000000000);
    let x1p_24 = fbits(0x3e70000000000000);
    let (e0, nx) = (a.param(0), a.param(1));
    let jx = a.let_(nx - 1);
    let jv = a.let_((e0 - 3) / 24);
    a.if_(jv.lt(0), |a| a.set(jv, 0));
    let q0 = a.let_(e0 - (jv + 1) * 24);
    let j = a.let_(jv - jx);
    let m = a.let_(jx + JK);
    let i = a.local(Ty::I32);
    let k = a.local(Ty::I32);
    let fw = a.local(Ty::F64);
    let z = a.local(Ty::F64);
    let n = a.local(Ty::I32);
    let ih = a.local(Ty::I32);
    let jj = a.local(Ty::I32);
    a.for_(i, 0, m + 1, false, |a, _| {
        set_f64_at(a, F, i, j.lt(0).pick(0.0, i32_at(IPIO2_AT, j).to_f64()));
        a.set(j, j + 1);
    });
    // q[i] = sum over j of x[j] * f[jx + i - j]
    let fill_q = |a: &mut Asm, i: Var| {
        a.set(fw, 0.0);
        a.for_(jj, 0, jx + 1, false, |a, _| {
            a.set(fw, fw + f64_at(TX, jj) * f64_at(F, jx + i - jj));
        });
        set_f64_at(a, Q, i, fw);
    };
    a.for_(i, 0, JK + 1, false, |a, _| fill_q(a, i));
    let jz = a.let_(JK);
    a.loop_(|a, recompute| {
        // Distill q[] into iq[] reversingly.
        a.set(i, 0);
        a.set(z, f64_at(Q, jz));
        a.for_(jj, jz, 1, true, |a, _| {
            a.set(fw, (x1p_24.clone() * z).to_i32().to_f64());
            set_i32_at(a, IQ, i, (z - x1p24.clone() * fw).to_i32());
            a.set(z, f64_at(Q, jj - 1) + fw);
            a.set(i, i + 1);
        });
        // Compute n.
        a.set(z, scal(ctx, z, q0));
        a.set(z, z - (z * 0.125).floor() * 8.0);
        a.set(n, z.to_i32());
        a.set(z, z - n.to_f64());
        a.set(ih, 0);
        a.if_else(
            q0.gt(0),
            |a| {
                // iq[jz-1] determines n.
                a.set(i, i32_at(IQ, jz - 1) >> (24 - q0));
                a.set(n, n + i);
                set_i32_at(a, IQ, jz - 1, i32_at(IQ, jz - 1) - (i << (24 - q0)));
                a.set(ih, i32_at(IQ, jz - 1) >> (23 - q0));
            },
            |a| {
                a.if_else(
                    q0.eq(0),
                    |a| a.set(ih, i32_at(IQ, jz - 1) >> 23),
                    |a| a.if_(z.ge(0.5), |a| a.set(ih, 2)),
                );
            },
        );
        a.if_(ih.gt(0), |a| {
            // q > 0.5
            a.set(n, n + 1);
            let carry = a.let_(0);
            a.for_(i, 0, jz, false, |a, _| {
                a.set(jj, i32_at(IQ, i));
                a.if_else(
                    carry.eqz(),
                    |a| {
                        a.if_(jj.ne(0), |a| {
                            a.set(carry, 1);
                            set_i32_at(a, IQ, i, 0x1000000 - jj);
                        });
                    },
                    |a| set_i32_at(a, IQ, i, 0xffffff - jj),
                );
            });
            a.if_(q0.eq(1), |a| {
                set_i32_at(a, IQ, jz - 1, i32_at(IQ, jz - 1) & 0x7fffff);
            });
            a.if_(q0.eq(2), |a| {
                set_i32_at(a, IQ, jz - 1, i32_at(IQ, jz - 1) & 0x3fffff);
            });
            a.if_(ih.eq(2), |a| {
                a.set(z, 1.0 - z);
                a.if_(carry, |a| a.set(z, z - scal(ctx, 1.0, q0)));
            });
        });
        // Check whether recomputation is needed.
        a.if_(z.eq(0.0), |a| {
            a.set(jj, 0);
            a.for_(i, jz - 1, JK, true, |a, _| a.set(jj, jj | i32_at(IQ, i)));
            a.if_(jj.eqz(), |a| {
                a.set(k, 1);
                a.while_(i32_at(IQ, k * -1 + JK).eqz(), |a, _| a.set(k, k + 1));
                a.for_(i, jz + 1, jz + k + 1, false, |a, _| {
                    set_f64_at(a, F, jx + i, i32_at(IPIO2_AT, jv + i).to_f64());
                    fill_q(a, i);
                });
                a.set(jz, jz + k);
                a.cont(recompute);
            });
        });
    });
    // Chop off zero terms.
    a.if_else(
        z.eq(0.0),
        |a| {
            a.set(jz, jz - 1);
            a.set(q0, q0 - 24);
            a.while_(i32_at(IQ, jz).eqz(), |a, _| {
                a.set(jz, jz - 1);
                a.set(q0, q0 - 24);
            });
        },
        |a| {
            // Break z into 24-bit chunks if necessary.
            a.set(z, scal(ctx, z, -q0));
            a.if_else(
                z.ge(x1p24.clone()),
                |a| {
                    a.set(fw, (x1p_24.clone() * z).to_i32().to_f64());
                    set_i32_at(a, IQ, jz, (z - x1p24.clone() * fw).to_i32());
                    a.set(jz, jz + 1);
                    a.set(q0, q0 + 24);
                    set_i32_at(a, IQ, jz, fw.to_i32());
                },
                |a| set_i32_at(a, IQ, jz, z.to_i32()),
            );
        },
    );
    // Convert the integer chunks to floating-point values.
    a.set(fw, scal(ctx, 1.0, q0));
    a.for_(i, jz, 0, true, |a, _| {
        set_f64_at(a, Q, i, fw * i32_at(IQ, i).to_f64());
        a.set(fw, fw * x1p_24.clone());
    });
    // fq = PIo2[0..jp] * q[jz..0]
    a.for_(i, jz, 0, true, |a, _| {
        a.set(fw, 0.0);
        a.set(k, 0);
        a.while_(k.le(JP) & k.le(jz - i), |a, _| {
            a.set(fw, fw + f64_at(PIO2_AT, k) * f64_at(Q, i + k));
            a.set(k, k + 1);
        });
        set_f64_at(a, FQ, jz - i, fw);
    });
    // Compress fq[] into y[].
    a.set(fw, 0.0);
    a.for_(i, jz, 0, true, |a, _| a.set(fw, fw + f64_at(FQ, i)));
    a.store(Ty::F64, Y0, 0, ih.eqz().select(fw, -fw));
    a.set(fw, f64_at(FQ, 0) - fw);
    a.for_(i, 1, jz + 1, false, |a, _| a.set(fw, fw + f64_at(FQ, i)));
    a.store(Ty::F64, Y1, 0, ih.eqz().select(fw, -fw));
    a.ret(n & 7);
}

const IPIO2: [i32; 66] = [
    0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
    0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
    0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
    0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
    0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
    0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
    0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
    0x73A8C9, 0x60E27B, 0xC08C6B,
];

const PIO2: [f64; 8] = [
    1.57079625129699707031e+00,
    7.54978941586159635335e-08,
    5.39030252995776476554e-15,
    3.28200341580791294123e-22,
    1.27065575308067607349e-29,
    1.22933308981111328932e-36,
    2.73370053816464559624e-44,
    2.16741683877804819444e-51,
];

// ---- asin, acos, atan, atan2 ---------------------------------------------

const PIO2_HI: f64 = 1.57079632679489655800e+00;
const PIO2_LO: f64 = 6.12323399573676603587e-17;

/// asin's and acos's rational approximation.
fn asin_r(z: Var) -> E {
    const PS0: f64 = 1.66666666666666657415e-01;
    const PS1: f64 = -3.25565818622400915405e-01;
    const PS2: f64 = 2.01212532134862925881e-01;
    const PS3: f64 = -4.00555345006794114027e-02;
    const PS4: f64 = 7.91534994289814532176e-04;
    const PS5: f64 = 3.47933107596021167570e-05;
    const QS1: f64 = -2.40339491173441421878e+00;
    const QS2: f64 = 2.02094576023350569471e+00;
    const QS3: f64 = -6.88283971605453293030e-01;
    const QS4: f64 = 7.70381505559019352791e-02;
    let p = z * (z * (z * (z * (z * (z * PS5 + PS4) + PS3) + PS2) + PS1) + PS0);
    let q = z * (z * (z * (z * QS4 + QS3) + QS2) + QS1) + 1.0;
    p / q
}

fn asin(a: &mut Asm) {
    let x = a.param(0);
    let hx = a.let_(x.hi());
    let ix = a.let_(hx & 0x7fffffff);
    a.if_(ix.ge_u(0x3ff00000), |a| {
        // |x| >= 1 or nan
        a.if_(((ix - 0x3ff00000) | x.lo()).eqz(), |a| {
            a.ret(x * PIO2_HI + fbits(0x3870000000000000));
        });
        a.ret(0.0 / (x - x));
    });
    a.if_(ix.lt_u(0x3fe00000), |a| {
        // |x| < 0.5
        a.if_(ix.ge_u(0x00100000) & ix.lt_u(0x3e500000), |a| a.ret(x));
        let xx = a.let_(x * x);
        a.ret(x + x * asin_r(xx));
    });
    // 1 > |x| >= 0.5
    let z = a.let_((1.0 - x.abs()) * 0.5);
    let s = a.let_(z.sqrt());
    let r = a.let_(asin_r(z));
    let y = a.local(Ty::F64);
    a.if_else(
        ix.ge_u(0x3fef3333),
        |a| a.set(y, PIO2_HI - ((s + s * r) * 2.0 - PIO2_LO)),
        |a| {
            let f = a.let_(s.with_lo(0));
            let c = a.let_((z - f * f) / (s + f));
            a.set(
                y,
                0.5 * PIO2_HI - (s * 2.0 * r - (PIO2_LO - c * 2.0) - (0.5 * PIO2_HI - f * 2.0)),
            );
        },
    );
    a.ret(hx.shr_u(31).select(-y, y));
}

fn acos(a: &mut Asm) {
    let x = a.param(0);
    let x1p_120f = fbits(0x3870000000000000);
    let hx = a.let_(x.hi());
    let ix = a.let_(hx & 0x7fffffff);
    a.if_(ix.ge_u(0x3ff00000), |a| {
        a.if_(((ix - 0x3ff00000) | x.lo()).eqz(), |a| {
            a.if_(hx.shr_u(31), |a| a.ret(2.0 * PIO2_HI + x1p_120f.clone()));
            a.ret(0.0);
        });
        a.ret(0.0 / (x - x));
    });
    a.if_(ix.lt_u(0x3fe00000), |a| {
        a.if_(ix.le_u(0x3c600000), |a| a.ret(PIO2_HI + x1p_120f.clone()));
        let xx = a.let_(x * x);
        a.ret(PIO2_HI - (x - (PIO2_LO - x * asin_r(xx))));
    });
    a.if_(hx.shr_u(31), |a| {
        // x < -0.5
        let z = a.let_((x + 1.0) * 0.5);
        let s = a.let_(z.sqrt());
        let w = a.let_(asin_r(z) * s - PIO2_LO);
        a.ret((PIO2_HI - (s + w)) * 2.0);
    });
    // x > 0.5
    let z = a.let_((1.0 - x) * 0.5);
    let s = a.let_(z.sqrt());
    let df = a.let_(s.with_lo(0));
    let c = a.let_((z - df * df) / (s + df));
    let w = a.let_(asin_r(z) * s + c);
    a.ret((df + w) * 2.0);
}

fn atan(a: &mut Asm) {
    const ATANHI: [f64; 4] = [
        4.63647609000806093515e-01,
        7.85398163397448278999e-01,
        9.82793723247329054082e-01,
        1.57079632679489655800e+00,
    ];
    const ATANLO: [f64; 4] = [
        2.26987774529616870924e-17,
        3.06161699786838301793e-17,
        1.39033110312309984516e-17,
        6.12323399573676603587e-17,
    ];
    const AT: [f64; 11] = [
        3.33333333333329318027e-01,
        -1.99999999998764832476e-01,
        1.42857142725034663711e-01,
        -1.11111104054623557880e-01,
        9.09088713343650656196e-02,
        -7.69187620504482999495e-02,
        6.66107313738753120669e-02,
        -5.83357013379057348645e-02,
        4.97687799461593236017e-02,
        -3.65315727442169155270e-02,
        1.62858201153657823623e-02,
    ];
    let x = a.param(0);
    let ix = a.let_(x.hi());
    let sign = a.let_(ix.shr_u(31));
    a.set(ix, ix & 0x7fff_ffff);
    a.if_(ix.ge_u(0x4410_0000), |a| {
        a.if_(x.ne(x), |a| a.ret(x));
        let z = a.let_(fbits(0x0380_0000) + ATANHI[3]);
        a.ret(sign.select(-z, z));
    });
    let id = a.local(Ty::I32);
    a.if_else(
        ix.lt_u(0x3fdc_0000),
        |a| {
            // |x| < 0.4375
            a.if_(ix.lt_u(0x3e40_0000), |a| a.ret(x));
            a.set(id, -1);
        },
        |a| {
            a.set(x, x.abs());
            a.if_else(
                ix.lt_u(0x3ff30000),
                |a| {
                    a.if_else(
                        ix.lt_u(0x3fe60000),
                        |a| {
                            a.set(x, (x * 2.0 - 1.0) / (x + 2.0));
                            a.set(id, 0);
                        },
                        |a| {
                            a.set(x, (x - 1.0) / (x + 1.0));
                            a.set(id, 1);
                        },
                    );
                },
                |a| {
                    a.if_else(
                        ix.lt_u(0x40038000),
                        |a| {
                            a.set(x, (x - 1.5) / (x * 1.5 + 1.0));
                            a.set(id, 2);
                        },
                        |a| {
                            a.set(x, -1.0 / x);
                            a.set(id, 3);
                        },
                    );
                },
            );
        },
    );
    let z = a.let_(x * x);
    let w = a.let_(z * z);
    let s1 =
        a.let_(z * (w * (w * (w * (w * (w * AT[10] + AT[8]) + AT[6]) + AT[4]) + AT[2]) + AT[0]));
    let s2 = a.let_(w * (w * (w * (w * (w * AT[9] + AT[7]) + AT[5]) + AT[3]) + AT[1]));
    a.if_(id.lt(0), |a| a.ret(x - x * (s1 + s2)));
    let table = |t: &[f64; 4]| {
        id.eq(0)
            .select(t[0], id.eq(1).select(t[1], id.eq(2).select(t[2], t[3])))
    };
    let z = a.let_(table(&ATANHI) - (x * (s1 + s2) - table(&ATANLO) - x));
    a.ret(sign.select(-z, z));
}

fn atan2(a: &mut Asm, ctx: &CompilationContext) {
    const PI: f64 = 3.1415926535897931160E+00;
    const PI_LO: f64 = 1.2246467991473531772E-16;
    let (y, x) = (a.param(0), a.param(1));
    a.if_(x.ne(x) | y.ne(y), |a| a.ret(x + y));
    let ix = a.let_(x.hi());
    let lx = a.let_(x.lo());
    let iy = a.let_(y.hi());
    let ly = a.let_(y.lo());
    a.if_(((ix - 0x3ff00000) | lx).eqz(), |a| {
        a.ret(lm(ctx, Libm::Atan, vec![y.e()]));
    });
    let m = a.let_((iy.shr_u(31) & 1) | (ix.shr_u(30) & 2));
    a.set(ix, ix & 0x7fffffff);
    a.set(iy, iy & 0x7fffffff);
    let by_m = |m: Var, c: [f64; 4]| {
        m.eq(0)
            .select(c[0], m.eq(1).select(c[1], m.eq(2).select(c[2], c[3])))
    };
    a.if_((iy | ly).eqz(), |a| {
        a.if_(m.lt_u(2), |a| a.ret(y));
        a.ret(m.eq(2).select(PI, -PI));
    });
    a.if_((ix | lx).eqz(), |a| {
        a.ret((m & 1).select(-PI / 2.0, PI / 2.0))
    });
    a.if_(ix.eq(0x7ff00000), |a| {
        a.if_(iy.eq(0x7ff00000), |a| {
            a.ret(by_m(
                m,
                [PI / 4.0, -PI / 4.0, 3.0 * PI / 4.0, -3.0 * PI / 4.0],
            ));
        });
        a.ret(by_m(m, [0.0, -0.0, PI, -PI]));
    });
    a.if_((ix + (64 << 20)).lt_u(iy) | iy.eq(0x7ff00000), |a| {
        a.ret((m & 1).select(-PI / 2.0, PI / 2.0));
    });
    let z = a.local(Ty::F64);
    a.if_else(
        (m & 2).ne(0) & (iy + (64 << 20)).lt_u(ix),
        |a| a.set(z, 0.0),
        |a| a.set(z, lm(ctx, Libm::Atan, vec![(y / x).abs()])),
    );
    a.if_(m.eq(0), |a| a.ret(z));
    a.if_(m.eq(1), |a| a.ret(-z));
    a.if_(m.eq(2), |a| a.ret(PI - (z - PI_LO)));
    a.ret((z - PI_LO) - PI);
}

// ---- sinh, cosh, tanh, exp, expm1 ----------------------------------------

fn sinh(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    let h = a.let_(x.bits().lt(0i64).select(-0.5, 0.5));
    let absx = a.let_(x.abs());
    let w = a.let_(absx.hi());
    a.if_(w.lt_u(0x40862e42), |a| {
        let t = a.let_(lm(ctx, Libm::Expm1, vec![absx.e()]));
        a.if_(w.lt_u(0x3ff00000), |a| {
            a.if_(w.lt_u(0x3ff00000 - (26 << 20)), |a| a.ret(x));
            a.ret(h * (t * 2.0 - t * t / (t + 1.0)));
        });
        a.ret(h * (t + t / (t + 1.0)));
    });
    a.ret(h * 2.0 * lm(ctx, Libm::Expo2, vec![absx.e()]));
}

fn cosh(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    a.set(x, x.abs());
    let w = a.let_(x.hi());
    a.if_(w.lt_u(0x3fe62e42), |a| {
        a.if_(w.lt_u(0x3ff00000 - (26 << 20)), |a| a.ret(1.0));
        let t = a.let_(lm(ctx, Libm::Expm1, vec![x.e()]));
        a.ret(t * t / ((t + 1.0) * 2.0) + 1.0);
    });
    a.if_(w.lt_u(0x40862e42), |a| {
        let t = a.let_(lm(ctx, Libm::Exp, vec![x.e()]));
        a.ret((t + 1.0 / t) * 0.5);
    });
    a.ret(lm(ctx, Libm::Expo2, vec![x.e()]));
}

fn tanh(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    let sign = a.let_(x.bits().lt(0i64));
    a.set(x, x.abs());
    let w = a.let_(x.hi());
    let t = a.local(Ty::F64);
    let expm1 = |v: E| lm(ctx, Libm::Expm1, vec![v]);
    a.if_else(
        w.gt_u(0x3fe193ea),
        |a| {
            a.if_else(
                w.gt_u(0x40340000),
                |a| a.set(t, 1.0 - 0.0 / x),
                |a| {
                    a.set(t, expm1(x * 2.0));
                    a.set(t, 1.0 - 2.0 / (t + 2.0));
                },
            );
        },
        |a| {
            a.if_else(
                w.gt_u(0x3fd058ae),
                |a| {
                    a.set(t, expm1(x * 2.0));
                    a.set(t, t / (t + 2.0));
                },
                |a| {
                    a.if_else(
                        w.ge_u(0x00100000),
                        |a| {
                            a.set(t, expm1(x * -2.0));
                            a.set(t, -t / (t + 2.0));
                        },
                        |a| a.set(t, x),
                    );
                },
            );
        },
    );
    a.ret(sign.select(-t, t));
}

fn expo2(a: &mut Asm, ctx: &CompilationContext) {
    let x = a.param(0);
    let kln2 = fbits(0x40962066151add8b);
    let scale = words(((0x3ff + 2043 / 2) as u32) << 20, 0);
    a.ret(lm(ctx, Libm::Exp, vec![x - kln2]) * scale.clone() * scale);
}

fn exp(a: &mut Asm, ctx: &CompilationContext) {
    const LN2HI: f64 = 6.93147180369123816490e-01;
    const LN2LO: f64 = 1.90821492927058770002e-10;
    const INVLN2: f64 = 1.44269504088896338700e+00;
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    let x = a.param(0);
    let hx = a.let_(x.hi());
    let sign = a.let_(hx.shr_u(31));
    a.set(hx, hx & 0x7fffffff);
    a.if_(hx.ge_u(0x4086232b), |a| {
        a.if_(x.ne(x), |a| a.ret(x));
        a.if_(x.gt(709.782712893383973096), |a| {
            a.ret(x * fbits(0x7fe0000000000000));
        });
        a.if_(x.lt(-745.13321910194110842), |a| a.ret(0.0));
    });
    let k = a.local(Ty::I32);
    let hi = a.local(Ty::F64);
    let lo = a.local(Ty::F64);
    a.if_else(
        hx.gt_u(0x3fd62e42),
        |a| {
            a.if_else(
                hx.ge_u(0x3ff0a2b2),
                |a| a.set(k, (x * INVLN2 + sign.select(-0.5, 0.5)).to_i32()),
                |a| a.set(k, 1 - sign - sign),
            );
            a.set(hi, x - k.to_f64() * LN2HI);
            a.set(lo, k.to_f64() * LN2LO);
            a.set(x, hi - lo);
        },
        |a| {
            a.if_else(
                hx.gt_u(0x3e300000),
                |a| {
                    a.set(k, 0);
                    a.set(hi, x);
                    a.set(lo, 0.0);
                },
                |a| a.ret(x + 1.0),
            );
        },
    );
    let xx = a.let_(x * x);
    let c = a.let_(x - xx * (xx * (xx * (xx * (xx * P5 + P4) + P3) + P2) + P1));
    let y = a.let_((x * c / (2.0 - c) - lo + hi) + 1.0);
    a.if_(k.eqz(), |a| a.ret(y));
    a.ret(scal(ctx, y, k));
}

fn expm1(a: &mut Asm) {
    const O_THRESHOLD: f64 = 7.09782712893383973096e+02;
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const INVLN2: f64 = 1.44269504088896338700e+00;
    const Q1: f64 = -3.33333333333331316428e-02;
    const Q2: f64 = 1.58730158725481460165e-03;
    const Q3: f64 = -7.93650757867487942473e-05;
    const Q4: f64 = 4.00821782732936239552e-06;
    const Q5: f64 = -2.01099218183624371326e-07;
    let x = a.param(0);
    let hx = a.let_(x.hi() & 0x7fffffff);
    let sign = a.let_(x.bits().shr_u(63i64).wrap());
    a.if_(hx.ge_u(0x4043687A), |a| {
        a.if_(x.ne(x), |a| a.ret(x));
        a.if_(sign, |a| a.ret(-1.0));
        a.if_(x.gt(O_THRESHOLD), |a| a.ret(x * fbits(0x7fe0000000000000)));
    });
    let hi = a.local(Ty::F64);
    let lo = a.local(Ty::F64);
    let k = a.local(Ty::I32);
    let c = a.local(Ty::F64);
    let t = a.local(Ty::F64);
    a.if_else(
        hx.gt_u(0x3fd62e42),
        |a| {
            a.if_else(
                hx.lt_u(0x3FF0A2B2),
                |a| {
                    a.if_else(
                        sign.eqz(),
                        |a| {
                            a.set(hi, x - LN2_HI);
                            a.set(lo, LN2_LO);
                            a.set(k, 1);
                        },
                        |a| {
                            a.set(hi, x + LN2_HI);
                            a.set(lo, -LN2_LO);
                            a.set(k, -1);
                        },
                    );
                },
                |a| {
                    a.set(k, (x * INVLN2 + sign.select(-0.5, 0.5)).to_i32());
                    a.set(t, k.to_f64());
                    a.set(hi, x - t * LN2_HI);
                    a.set(lo, t * LN2_LO);
                },
            );
            a.set(x, hi - lo);
            a.set(c, (hi - x) - lo);
        },
        |a| {
            a.if_else(
                hx.lt_u(0x3c900000),
                |a| a.ret(x),
                |a| {
                    a.set(c, 0.0);
                    a.set(k, 0);
                },
            );
        },
    );
    let hfx = a.let_(x * 0.5);
    let hxs = a.let_(x * hfx);
    let r1 = a.let_(hxs * (hxs * (hxs * (hxs * (hxs * Q5 + Q4) + Q3) + Q2) + Q1) + 1.0);
    a.set(t, 3.0 - r1 * hfx);
    let e = a.let_(hxs * ((r1 - t) / (6.0 - x * t)));
    a.if_(k.eqz(), |a| a.ret(x - (x * e - hxs)));
    a.set(e, x * (e - c) - c);
    a.set(e, e - hxs);
    a.if_(k.eq(-1), |a| a.ret((x - e) * 0.5 - 0.5));
    a.if_(k.eq(1), |a| {
        a.if_(x.lt(-0.25), |a| a.ret((e - (x + 0.5)) * -2.0));
        a.ret((x - e) * 2.0 + 1.0);
    });
    let twopk = a.let_((((k + 0x3ff).extend_u()) << 52).bits_to_f64());
    let y = a.local(Ty::F64);
    a.if_(k.lt(0) | k.gt(56), |a| {
        a.set(y, x - e + 1.0);
        a.if_else(
            k.eq(1024),
            |a| a.set(y, y * 2.0 * fbits(0x7fe0000000000000)),
            |a| a.set(y, y * twopk),
        );
        a.ret(y - 1.0);
    });
    let uf = a.let_(((E::from(0x3ff) - k).extend_u() << 52).bits_to_f64());
    a.if_else(
        k.lt(20),
        |a| a.set(y, (x - e + (1.0 - uf)) * twopk),
        |a| a.set(y, (x - (e + uf) + 1.0) * twopk),
    );
    a.ret(y);
}

// ---- log, log10, log2 ------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum LogBase {
    E,
    Ten,
    Two,
}

fn log(a: &mut Asm, base: LogBase) {
    const LN2_HI: f64 = 6.93147180369123816490e-01;
    const LN2_LO: f64 = 1.90821492927058770002e-10;
    const LG1: f64 = 6.666666666666735130e-01;
    const LG2: f64 = 3.999999999940941908e-01;
    const LG3: f64 = 2.857142874366239149e-01;
    const LG4: f64 = 2.222219843214978396e-01;
    const LG5: f64 = 1.818357216161805012e-01;
    const LG6: f64 = 1.531383769920937332e-01;
    const LG7: f64 = 1.479819860511658591e-01;
    let x = a.param(0);
    let ui = a.let_(x.bits());
    let hx = a.let_(ui.shr_u(32i64).wrap());
    let k = a.let_(0);
    a.if_else(
        hx.lt_u(0x00100000) | hx.shr_u(31),
        |a| {
            a.if_((ui << 1).eqz(), |a| a.ret(-1.0 / (x * x)));
            a.if_(hx.shr_u(31), |a| a.ret((x - x) / 0.0));
            // Subnormal: scale x up.
            a.set(k, k - 54);
            a.set(x, x * fbits(0x4350000000000000));
            a.set(ui, x.bits());
            a.set(hx, ui.shr_u(32i64).wrap());
        },
        |a| {
            a.if_(hx.ge_u(0x7ff00000), |a| a.ret(x));
            a.if_(hx.eq(0x3ff00000) & (ui << 32).eqz(), |a| a.ret(0.0));
        },
    );
    // Reduce x into [sqrt(2)/2, sqrt(2)].
    a.set(hx, hx + (0x3ff00000 - 0x3fe6a09e));
    a.set(k, k + (hx.shr_u(20) - 0x3ff));
    a.set(hx, (hx & 0x000fffff) + 0x3fe6a09e);
    a.set(ui, (hx.extend_u() << 32) | (ui & 0xffffffffi64));
    a.set(x, ui.bits_to_f64());
    let f = a.let_(x - 1.0);
    let hfsq = a.let_(f * 0.5 * f);
    let s = a.let_(f / (f + 2.0));
    let z = a.let_(s * s);
    let w = a.let_(z * z);
    let t1 = a.let_(w * (w * (w * LG6 + LG4) + LG2));
    let t2 = a.let_(z * (w * (w * (w * LG7 + LG5) + LG3) + LG1));
    let r = a.let_(t2 + t1);
    let dk = a.let_(k.to_f64());
    if base == LogBase::E {
        a.ret(s * (hfsq + r) + dk * LN2_LO - hfsq + f + dk * LN2_HI);
        return;
    }
    // hi + lo = f - hfsq + s*(hfsq + R) ~ log(1 + f)
    let hi = a.let_((f - hfsq).with_lo(0));
    let lo = a.let_(f - hi - hfsq + s * (hfsq + r));
    let val_hi = a.local(Ty::F64);
    let val_lo = a.local(Ty::F64);
    let y = a.local(Ty::F64);
    if base == LogBase::Ten {
        const IVLN10HI: f64 = 4.34294481878168880939e-01;
        const IVLN10LO: f64 = 2.50829467116452752298e-11;
        const LOG10_2HI: f64 = 3.01029995663611771306e-01;
        const LOG10_2LO: f64 = 3.69423907715893078616e-13;
        a.set(val_hi, hi * IVLN10HI);
        a.set(y, dk * LOG10_2HI);
        a.set(
            val_lo,
            dk * LOG10_2LO + (lo + hi) * IVLN10LO + lo * IVLN10HI,
        );
    } else {
        const IVLN2HI: f64 = 1.44269504072144627571e+00;
        const IVLN2LO: f64 = 1.67517131648865118353e-10;
        a.set(val_hi, hi * IVLN2HI);
        a.set(val_lo, (lo + hi) * IVLN2LO + lo * IVLN2HI);
        a.set(y, dk);
    }
    let w = a.let_(y + val_hi);
    a.set(val_lo, val_lo + ((y - w) + val_hi));
    a.ret(val_lo + w);
}

// ---- pow -------------------------------------------------------------------

fn pow(a: &mut Asm, ctx: &CompilationContext) {
    const BP: [f64; 2] = [1.0, 1.5];
    const DP_H: [f64; 2] = [0.0, 5.84962487220764160156e-01];
    const DP_L: [f64; 2] = [0.0, 1.35003920212974897128e-08];
    const TWO53: f64 = 9007199254740992.0;
    const HUGE: f64 = 1.0e300;
    const TINY: f64 = 1.0e-300;
    const L1: f64 = 5.99999999999994648725e-01;
    const L2: f64 = 4.28571428578550184252e-01;
    const L3: f64 = 3.33333329818377432918e-01;
    const L4: f64 = 2.72728123808534006489e-01;
    const L5: f64 = 2.30660745775561754067e-01;
    const L6: f64 = 2.06975017800338417784e-01;
    const P1: f64 = 1.66666666666666019037e-01;
    const P2: f64 = -2.77777777770155933842e-03;
    const P3: f64 = 6.61375632143793436117e-05;
    const P4: f64 = -1.65339022054652515390e-06;
    const P5: f64 = 4.13813679705723846039e-08;
    const LG2: f64 = 6.93147180559945286227e-01;
    const LG2_H: f64 = 6.93147182464599609375e-01;
    const LG2_L: f64 = -1.90465429995776804525e-09;
    const OVT: f64 = 8.0085662595372944372e-017;
    const CP: f64 = 9.61796693925975554329e-01;
    const CP_H: f64 = 9.61796700954437255859e-01;
    const CP_L: f64 = -7.02846165095275826516e-09;
    const IVLN2: f64 = 1.44269504088896338700e+00;
    const IVLN2_H: f64 = 1.44269502162933349609e+00;
    const IVLN2_L: f64 = 1.92596299112661746887e-08;
    let (x, y) = (a.param(0), a.param(1));
    let hx = a.let_(x.hi());
    let lx = a.let_(x.lo());
    let hy = a.let_(y.hi());
    let ly = a.let_(y.lo());
    let ix = a.let_(hx & 0x7fffffff);
    let iy = a.let_(hy & 0x7fffffff);
    // x**0 = 1, even if x is NaN; 1**y = 1, even if y is NaN.
    a.if_((iy | ly).eqz(), |a| a.ret(1.0));
    a.if_(hx.eq(0x3ff00000) & lx.eqz(), |a| a.ret(1.0));
    a.if_(
        ix.gt(0x7ff00000)
            | (ix.eq(0x7ff00000) & lx.ne(0))
            | iy.gt(0x7ff00000)
            | (iy.eq(0x7ff00000) & ly.ne(0)),
        |a| a.ret(x + y),
    );
    // yisint: 0 not an integer, 1 an odd one, 2 an even one (for x < 0).
    let yisint = a.let_(0);
    let k = a.local(Ty::I32);
    let j = a.local(Ty::I32);
    a.if_(hx.lt(0), |a| {
        a.if_else(
            iy.ge(0x43400000),
            |a| a.set(yisint, 2),
            |a| {
                a.if_(iy.ge(0x3ff00000), |a| {
                    a.set(k, (iy >> 20) - 0x3ff);
                    a.if_else(
                        k.gt(20),
                        |a| {
                            a.set(j, ly.shr_u(52 - k));
                            a.if_((j << (52 - k)).eq(ly), |a| a.set(yisint, 2 - (j & 1)));
                        },
                        |a| {
                            a.if_(ly.eqz(), |a| {
                                a.set(j, iy >> (20 - k));
                                a.if_((j << (20 - k)).eq(iy), |a| {
                                    a.set(yisint, 2 - (j & 1));
                                });
                            });
                        },
                    );
                });
            },
        );
    });
    a.if_(ly.eqz(), |a| {
        a.if_(iy.eq(0x7ff00000), |a| {
            // y is +-inf
            a.if_(((ix - 0x3ff00000) | lx).eqz(), |a| a.ret(1.0));
            a.if_(ix.ge(0x3ff00000), |a| a.ret(hy.ge(0).select(y, 0.0)));
            a.ret(hy.ge(0).select(0.0, -y));
        });
        a.if_(iy.eq(0x3ff00000), |a| a.ret(hy.ge(0).pick(x, 1.0 / x)));
        a.if_(hy.eq(0x40000000), |a| a.ret(x * x));
        a.if_(hy.eq(0x3fe00000) & hx.ge(0), |a| a.ret(x.sqrt()));
    });
    let ax = a.let_(x.abs());
    a.if_(lx.eqz(), |a| {
        a.if_(ix.eq(0x7ff00000) | ix.eqz() | ix.eq(0x3ff00000), |a| {
            // x is +-0, +-inf, +-1
            let z = a.let_(ax);
            a.if_(hy.lt(0), |a| a.set(z, 1.0 / z));
            a.if_(hx.lt(0), |a| {
                a.if_else(
                    ((ix - 0x3ff00000) | yisint).eqz(),
                    |a| a.set(z, (z - z) / (z - z)),
                    |a| a.if_(yisint.eq(1), |a| a.set(z, -z)),
                );
            });
            a.ret(z);
        });
    });
    let s = a.let_(1.0);
    a.if_(hx.lt(0), |a| {
        a.if_(yisint.eqz(), |a| a.ret((x - x) / (x - x)));
        a.if_(yisint.eq(1), |a| a.set(s, -1.0));
    });
    let t1 = a.local(Ty::F64);
    let t2 = a.local(Ty::F64);
    a.if_else(
        iy.gt(0x41e00000),
        |a| {
            // |y| > 2**31
            a.if_(iy.gt(0x43f00000), |a| {
                a.if_(ix.le(0x3fefffff), |a| {
                    a.ret(hy.lt(0).select(E::from(HUGE) * HUGE, E::from(TINY) * TINY));
                });
                a.if_(ix.ge(0x3ff00000), |a| {
                    a.ret(hy.gt(0).select(E::from(HUGE) * HUGE, E::from(TINY) * TINY));
                });
            });
            a.if_(ix.lt(0x3fefffff), |a| {
                a.ret(hy.lt(0).select(s * HUGE * HUGE, s * TINY * TINY));
            });
            a.if_(ix.gt(0x3ff00000), |a| {
                a.ret(hy.gt(0).select(s * HUGE * HUGE, s * TINY * TINY));
            });
            // |1 - x| <= 2**-20: log(x) by x - x^2/2 + x^3/3 - x^4/4.
            let t = a.let_(ax - 1.0);
            let w = a.let_((t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25)));
            let u = a.let_(t * IVLN2_H);
            let v = a.let_(t * IVLN2_L - w * IVLN2);
            a.set(t1, (u + v).with_lo(0));
            a.set(t2, v - (t1 - u));
        },
        |a| {
            let n = a.let_(0);
            a.if_(ix.lt(0x00100000), |a| {
                a.set(ax, ax * TWO53);
                a.set(n, n - 53);
                a.set(ix, ax.hi());
            });
            a.set(n, n + ((ix >> 20) - 0x3ff));
            a.set(j, ix & 0x000fffff);
            a.set(ix, j | 0x3ff00000);
            a.if_else(
                j.le(0x3988E),
                |a| a.set(k, 0),
                |a| {
                    a.if_else(
                        j.lt(0xBB67A),
                        |a| a.set(k, 1),
                        |a| {
                            a.set(k, 0);
                            a.set(n, n + 1);
                            a.set(ix, ix - 0x00100000);
                        },
                    );
                },
            );
            a.set(ax, ax.with_hi(ix));
            let at = |t: [f64; 2]| k.eqz().select(t[0], t[1]);
            // ss = s_h + s_l = (x - 1)/(x + 1) or (x - 1.5)/(x + 1.5)
            let u = a.let_(ax - at(BP));
            let v = a.let_(1.0 / (ax + at(BP)));
            let ss = a.let_(u * v);
            let s_h = a.let_(ss.with_lo(0));
            let t_h = a.let_(words(
                ((ix.shr_u(1)) | 0x20000000) + 0x00080000 + (k << 18),
                0,
            ));
            let t_l = a.let_(ax - (t_h - at(BP)));
            let s_l = a.let_(v * ((u - s_h * t_h) - s_h * t_l));
            // log(ax)
            let s2 = a.let_(ss * ss);
            let r =
                a.let_(s2 * s2 * (s2 * (s2 * (s2 * (s2 * (s2 * L6 + L5) + L4) + L3) + L2) + L1));
            a.set(r, r + s_l * (s_h + ss));
            a.set(s2, s_h * s_h);
            a.set(t_h, (s2 + 3.0 + r).with_lo(0));
            a.set(t_l, r - ((t_h - 3.0) - s2));
            // u + v = ss*(1 + ...)
            a.set(u, s_h * t_h);
            a.set(v, s_l * t_h + t_l * ss);
            // 2/(3log2)*(ss + ...)
            let p_h = a.let_((u + v).with_lo(0));
            let p_l = a.let_(v - (p_h - u));
            let z_h = a.let_(p_h * CP_H);
            let z_l = a.let_(p_h * CP_L + p_l * CP + at(DP_L));
            // log2(ax) = (ss + ..)*2/(3*log2) = n + dp_h + z_h + z_l
            let t = a.let_(n.to_f64());
            a.set(t1, (((z_h + z_l) + at(DP_H)) + t).with_lo(0));
            a.set(t2, z_l - (((t1 - t) - at(DP_H)) - z_h));
        },
    );
    // Split y into y1 + y2 and compute (y1 + y2)*(t1 + t2).
    let y1 = a.let_(y.with_lo(0));
    let p_l = a.let_((y - y1) * t1 + y * t2);
    let p_h = a.let_(y1 * t1);
    let z = a.let_(p_l + p_h);
    a.set(j, z.hi());
    let i = a.let_(z.lo());
    a.if_else(
        j.ge(0x40900000),
        |a| {
            // z >= 1024
            a.if_(((j - 0x40900000) | i).ne(0), |a| a.ret(s * HUGE * HUGE));
            a.if_((p_l + OVT).gt(z - p_h), |a| a.ret(s * HUGE * HUGE));
        },
        |a| {
            a.if_((j & 0x7fffffff).ge(0x4090cc00), |a| {
                // z <= -1075
                a.if_(((j - 0xc090cc00u32) | i).ne(0), |a| a.ret(s * TINY * TINY));
                a.if_(p_l.le(z - p_h), |a| a.ret(s * TINY * TINY));
            });
        },
    );
    // 2**(p_h + p_l)
    a.set(i, j & 0x7fffffff);
    a.set(k, (i >> 20) - 0x3ff);
    let n = a.let_(0);
    a.if_(i.gt(0x3fe00000), |a| {
        // |z| > 0.5: n = [z + 0.5]
        a.set(n, j + (E::from(0x00100000) >> (k + 1)));
        a.set(k, ((n & 0x7fffffff) >> 20) - 0x3ff);
        let t = a.let_(words(n & (E::from(0x000fffff) >> k ^ -1), 0));
        a.set(n, ((n & 0x000fffff) | 0x00100000) >> (20 - k));
        a.if_(j.lt(0), |a| a.set(n, -n));
        a.set(p_h, p_h - t);
    });
    let t = a.let_((p_l + p_h).with_lo(0));
    let u = a.let_(t * LG2_H);
    let v = a.let_((p_l - (t - p_h)) * LG2 + t * LG2_L);
    a.set(z, u + v);
    let w = a.let_(v - (z - u));
    let t = a.let_(z * z);
    let t1 = a.let_(z - t * (t * (t * (t * (t * P5 + P4) + P3) + P2) + P1));
    let r = a.let_((z * t1) / (t1 - 2.0) - (w + z * w));
    a.set(z, 1.0 - (r - z));
    a.set(j, z.hi());
    a.set(j, j + (n << 20));
    a.if_else(
        (j >> 20).le(0),
        |a| a.set(z, scal(ctx, z, n)),
        |a| a.set(z, z.with_hi(j)),
    );
    a.ret(s * z);
}

// ---- fmod, scalbn ------------------------------------------------------------

/// musl's `fmod`: exact, so any correct one agrees.
fn fmod(a: &mut Asm) {
    let (x, y) = (a.param(0), a.param(1));
    let uxi = a.let_(x.bits());
    let uyi = a.let_(y.bits());
    let ex = a.let_(uxi.shr_u(52i64).wrap() & 0x7ff);
    let ey = a.let_(uyi.shr_u(52i64).wrap() & 0x7ff);
    let sx = a.let_(uxi.shr_u(63i64));
    let i = a.local(Ty::I64);
    a.if_((uyi << 1).eqz() | y.ne(y) | ex.eq(0x7ff), |a| {
        a.ret((x * y) / (x * y))
    });
    a.if_((uxi << 1).le_u(uyi << 1), |a| {
        a.if_((uxi << 1).eq(uyi << 1), |a| a.ret(x * 0.0));
        a.ret(x);
    });
    // Normalize x and y.
    a.if_else(
        ex.eqz(),
        |a| {
            a.set(i, uxi << 12);
            a.while_(i.shr_u(63i64).eqz(), |a, _| {
                a.set(ex, ex - 1);
                a.set(i, i << 1);
            });
            a.set(uxi, uxi << (E::from(1) - ex).extend_u());
        },
        |a| {
            a.set(uxi, uxi & (-1i64 as u64 >> 12) as i64);
            a.set(uxi, uxi | (1i64 << 52));
        },
    );
    a.if_else(
        ey.eqz(),
        |a| {
            a.set(i, uyi << 12);
            a.while_(i.shr_u(63i64).eqz(), |a, _| {
                a.set(ey, ey - 1);
                a.set(i, i << 1);
            });
            a.set(uyi, uyi << (E::from(1) - ey).extend_u());
        },
        |a| {
            a.set(uyi, uyi & (-1i64 as u64 >> 12) as i64);
            a.set(uyi, uyi | (1i64 << 52));
        },
    );
    // x mod y
    a.while_(ex.gt(ey), |a, _| {
        a.set(i, uxi - uyi);
        a.if_(i.shr_u(63i64).eqz(), |a| {
            a.if_(i.eqz(), |a| a.ret(x * 0.0));
            a.set(uxi, i);
        });
        a.set(uxi, uxi << 1);
        a.set(ex, ex - 1);
    });
    a.set(i, uxi - uyi);
    a.if_(i.shr_u(63i64).eqz(), |a| {
        a.if_(i.eqz(), |a| a.ret(x * 0.0));
        a.set(uxi, i);
    });
    a.while_(uxi.shr_u(52i64).eqz(), |a, _| {
        a.set(uxi, uxi << 1);
        a.set(ex, ex - 1);
    });
    // Scale the result.
    a.if_else(
        ex.gt(0),
        |a| {
            a.set(uxi, uxi - (1i64 << 52));
            a.set(uxi, uxi | (ex.extend_u() << 52));
        },
        |a| a.set(uxi, uxi.shr_u((E::from(1) - ex).extend_u())),
    );
    a.ret((uxi | (sx << 63)).bits_to_f64());
}

/// musl's `scalbn`.
fn scalbn(a: &mut Asm) {
    let (x, n) = (a.param(0), a.param(1));
    let y = a.let_(x);
    a.if_else(
        n.gt(1023),
        |a| {
            a.set(y, y * fbits(0x7fe0000000000000));
            a.set(n, n - 1023);
            a.if_(n.gt(1023), |a| {
                a.set(y, y * fbits(0x7fe0000000000000));
                a.set(n, n - 1023);
                a.if_(n.gt(1023), |a| a.set(n, 1023));
            });
        },
        |a| {
            a.if_(n.lt(-1022), |a| {
                // 0x1p-1022 * 0x1p53, keeping n < -53 to avoid double
                // rounding in the subnormal range.
                let down = fbits(0x0010000000000000) * fbits(0x4340000000000000);
                a.set(y, y * down.clone());
                a.set(n, n + (1022 - 53));
                a.if_(n.lt(-1022), |a| {
                    a.set(y, y * down);
                    a.set(n, n + (1022 - 53));
                    a.if_(n.lt(-1022), |a| a.set(n, -1022));
                });
            });
        },
    );
    a.ret(y * (((n + 0x3ff).extend_u()) << 52).bits_to_f64());
}

// ---- CPython's own: hypot and remainder ---------------------------------------

/// Dekker's exact product, `x*y == hi + lo`, with `hi` the rounded product.
fn two_product(a: &mut Asm, x: Var, y: Var) -> (Var, Var) {
    let split = |a: &mut Asm, v: Var| {
        let t = a.let_(v * 134217729.0);
        let hi = a.let_(t - (t - v));
        let lo = a.let_(v - hi);
        (hi, lo)
    };
    let (xh, xl) = split(a, x);
    let (yh, yl) = split(a, y);
    let p = a.let_(xh * yh);
    let q = a.let_(xh * yl + xl * yh);
    let z = a.let_(p + q);
    let zz = a.let_(p - z + q + xl * yl);
    (z, zz)
}

/// CPython's `vector_norm` over the `n` absolute values at `VEC`.
fn vector_norm(a: &mut Asm) {
    let n = a.param(0);
    let i = a.local(Ty::I32);
    let x = a.local(Ty::F64);
    let max = a.let_(0.0);
    let found_nan = a.let_(0);
    a.for_(i, 0, n, false, |a, _| {
        a.set(x, f64_at(VEC, i));
        a.set(found_nan, found_nan | x.ne(x));
        a.if_(x.gt(max), |a| a.set(max, x));
    });
    a.if_(max.eq(f64::INFINITY), |a| a.ret(max));
    a.if_(found_nan, |a| a.ret(f64::NAN));
    a.if_(max.eq(0.0) | n.le(1), |a| a.ret(max));
    // frexp(max): max = m * 2**max_e with 0.5 <= m < 1.
    let biased = a.let_(max.bits().shr_u(52i64).wrap());
    let pre = a.let_(1.0);
    a.if_(biased.eqz(), |a| {
        // Below 2**-1022 (CPython's max_e < -1023 case and the binade
        // above it): divide everything by DBL_MIN first, then scale back.
        let dbl_min = fbits(0x0010000000000000);
        a.for_(i, 0, n, false, |a, _| {
            set_f64_at(a, VEC, i, f64_at(VEC, i) / dbl_min.clone());
        });
        a.set(max, max / dbl_min.clone());
        a.set(pre, dbl_min);
        a.set(biased, max.bits().shr_u(52i64).wrap());
    });
    // scale = 2**-max_e, max_e = biased - 1022; below 2**-1022 (for the top
    // two binades) it is subnormal.
    let scale = a.let_(biased.lt(2045).pick(
        ((E::from(2045) - biased).extend_u() << 52).bits_to_f64(),
        (E::from(1i64) << (E::from(2096) - biased).extend_u()).bits_to_f64(),
    ));
    let csum = a.let_(1.0);
    let frac1 = a.let_(0.0);
    let frac2 = a.let_(0.0);
    a.for_(i, 0, n, false, |a, _| {
        a.set(x, f64_at(VEC, i) * scale);
        let (hi, lo) = two_product(a, x, x);
        // dl_fast_sum(csum, hi)
        let sum = a.let_(csum + hi);
        let z = a.let_(sum - csum);
        a.set(frac2, frac2 + (hi - z));
        a.set(csum, sum);
        a.set(frac1, frac1 + lo);
    });
    let h = a.let_((csum - 1.0 + (frac1 + frac2)).sqrt());
    let neg_h = a.let_(-h);
    let (hi, lo) = two_product(a, neg_h, h);
    let sum = a.let_(csum + hi);
    let z = a.let_(sum - csum);
    a.set(frac2, frac2 + (hi - z));
    a.set(csum, sum);
    a.set(frac1, frac1 + lo);
    a.set(x, csum - 1.0 + (frac1 + frac2));
    a.set(h, h + x / (h * 2.0));
    a.ret(pre * (h / scale));
}

/// CPython's `m_remainder`.
fn remainder(a: &mut Asm, ctx: &CompilationContext) {
    let (x, y) = (a.param(0), a.param(1));
    let finite = |v: Var| (v - v).eq(0.0);
    a.if_(finite(x) & finite(y), |a| {
        a.if_(y.eq(0.0), |a| a.ret(f64::NAN));
        let absx = a.let_(x.abs());
        let absy = a.let_(y.abs());
        let m = a.let_(lm(ctx, Libm::Fmod, vec![absx.e(), absy.e()]));
        let c = a.let_(absy - m);
        let r = a.local(Ty::F64);
        a.if_else(
            m.lt(c),
            |a| a.set(r, m),
            |a| {
                a.if_else(
                    m.gt(c),
                    |a| a.set(r, -c),
                    |a| {
                        // absx is halfway between two multiples of absy:
                        // take the even one.
                        a.set(
                            r,
                            m - lm(ctx, Libm::Fmod, vec![(absx - m) * 0.5, absy.e()]) * 2.0,
                        );
                    },
                );
            },
        );
        a.ret(E::from(1.0).copysign(x) * r);
    });
    a.if_(x.ne(x), |a| a.ret(x));
    a.if_(y.ne(y), |a| a.ret(y));
    a.if_(finite(x).eqz(), |a| a.ret(f64::NAN));
    a.ret(x);
}

/// CPython's `float_pow`.
fn float_pow(a: &mut Asm, ctx: &CompilationContext) {
    let (iv, iw) = (a.param(0), a.param(1));
    // DOUBLE_IS_ODD_INTEGER: fmod(fabs(v), 2.0) == 1.0
    let odd = |v: Var| lm(ctx, Libm::Fmod, vec![v.abs(), 2.0.into()]).eq(1.0);
    a.if_(iw.eq(0.0), |a| a.ret(1.0));
    a.if_(iv.ne(iv), |a| a.ret(iv));
    a.if_(iw.ne(iw), |a| a.ret(iv.eq(1.0).select(1.0, iw)));
    a.if_(iw.abs().eq(f64::INFINITY), |a| {
        let v = a.let_(iv.abs());
        a.if_(v.eq(1.0), |a| a.ret(1.0));
        a.if_(iw.gt(0.0).eq(v.gt(1.0)), |a| a.ret(iw.abs()));
        a.ret(0.0);
    });
    a.if_(iv.abs().eq(f64::INFINITY), |a| {
        let is_odd = a.let_(odd(iw));
        a.if_(iw.gt(0.0), |a| a.ret(is_odd.select(iv, iv.abs())));
        a.ret(is_odd.select(E::from(0.0).copysign(iv), 0.0));
    });
    a.if_(iv.eq(0.0), |a| {
        let is_odd = a.let_(odd(iw));
        a.if_(iw.lt(0.0), |a| {
            let text = call(
                ctx.helper(Helper::ExcText(
                    "0.0 cannot be raised to a negative power".to_string(),
                )),
                vec![],
                Some(Ty::I32),
            );
            a.run(call(
                ctx.helper(Helper::RaiseText("ZeroDivisionError".to_string())),
                vec![text],
                None,
            ));
            a.ret(0.0);
        });
        a.ret(is_odd.select(iv, 0.0));
    });
    let negate = a.let_(0);
    a.if_(iv.lt(0.0), |a| {
        // A negative number to a fractional power is complex, which no
        // value here can be.
        a.if_(iw.ne(iw.floor()), |a| {
            a.raw(wasm_encoder::Instruction::Unreachable)
        });
        a.set(iv, -iv);
        a.set(negate, odd(iw));
    });
    a.if_(iv.eq(1.0), |a| a.ret(negate.select(-1.0, 1.0)));
    let ix = a.let_(lm(ctx, Libm::Pow, vec![iv.e(), iw.e()]));
    // _Py_ADJUST_ERANGE1: an infinity overflowed; an underflow to 0 is fine.
    a.if_(ix.abs().eq(f64::INFINITY), |a| {
        a.run(call(
            ctx.helper(Helper::RaiseErrno {
                class: "OverflowError".to_string(),
                errno: 34,
                message: "Result too large".to_string(),
            }),
            vec![],
            None,
        ));
        a.ret(0.0);
    });
    a.ret(negate.select(-ix, ix));
}
