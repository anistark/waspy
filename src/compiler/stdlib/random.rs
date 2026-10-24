//! `random`, bit for bit as CPython 3.13 draws it.
//!
//! The generator is CPython's MT19937 (`_randommodule.c`): the same state
//! initialization (`init_by_array`), the same tempering, and the same 53-bit
//! `random()`. On top of it the Python-level functions follow `random.py`:
//! `_randbelow` by rejection sampling over `getrandbits`, `randrange`'s
//! checks and arithmetic, `shuffle`'s and `sample`'s order of draws, and the
//! Gaussian methods with their constants. A seed gives CPython's sequence,
//! and an unseeded generator seeds itself from host entropy (WASI
//! `random_get`) on its first draw, as CPython seeds from `os.urandom`.
//!
//! The state lives in memory below the heap, past the libm region: one
//! generator per module, as CPython's `random` module has one shared
//! instance.

use crate::compiler::asm::{call, Asm, Ty, Var, E, X};
use crate::compiler::context::{CompilationContext, COLLECTION_DATA, COLLECTION_HEADER};
use crate::compiler::exceptions::ExcArg;
use crate::compiler::expression::{emit_as_sequence, emit_expr, recover_str_pair};
use crate::compiler::function::{emit_post_call_check, emit_raise};
use crate::compiler::libm::Libm;
use crate::compiler::runtime::Helper;
use crate::ir::{IRConstant, IRExpr, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

/// The generator's state: 624 words, the next index, whether it has been
/// seeded, and `gauss`'s cached second value.
const MT: i32 = crate::compiler::libm::LIBM + 2048;
const N: i32 = 624;
const M: i32 = 397;
const MT_INDEX: i32 = MT + N * 4;
const MT_SEEDED: i32 = MT_INDEX + 4;
const GAUSS_HAS: i32 = MT_INDEX + 8;
const GAUSS_NEXT: i32 = MT_INDEX + 16;
/// A seed's key words (624 of them for host entropy).
const MT_KEY: i32 = MT_INDEX + 32;
const _: () = assert!(MT_KEY + N * 4 <= crate::compiler::context::COLLECTION_HEAP_BASE as i32);
const _: () = assert!(crate::compiler::libm::LIBM_END <= MT);

const TWOPI: f64 = 2.0 * std::f64::consts::PI;
/// `4 * exp(-0.5) / sqrt(2.0)`, as `random.py` computes it at import.
const NV_MAGICCONST: f64 = 1.7155277699214135;

/// The runtime routines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RandomFn {
    /// `() -> u32`: the next tempered word, seeding first if need be.
    Genrand,
    /// `(key, len) -> ()`: CPython's `init_by_array`.
    InitByArray,
    /// `() -> ()`: seed from 624 words of host entropy.
    SeedEntropy,
    /// `(lo, hi) -> ()`: seed from a 64-bit unsigned number.
    SeedWords,
    /// `() -> f64`
    Random,
    /// `(k) -> int`: the low 32 bits of `getrandbits(k)`; ValueError (left
    /// pending) for a negative k.
    Getrandbits,
    /// `(n) -> int`: `_randbelow(n)` for `n > 0`.
    Randbelow,
    /// `(mu, sigma) -> f64`
    Gauss,
    Normalvariate,
    /// `(lambd) -> f64`; ZeroDivisionError for 0.
    Expovariate,
    /// `(list) -> ()`
    Shuffle,
    /// `(sequence, k) -> list`; ValueError for a k out of range.
    Sample,
}

impl RandomFn {
    pub fn signature(self) -> (Vec<Ty>, Option<Ty>) {
        use Ty::{F64, I32};
        match self {
            RandomFn::Genrand => (vec![], Some(I32)),
            RandomFn::InitByArray | RandomFn::SeedWords => (vec![I32, I32], None),
            RandomFn::SeedEntropy => (vec![], None),
            RandomFn::Random => (vec![], Some(F64)),
            RandomFn::Getrandbits | RandomFn::Randbelow => (vec![I32], Some(I32)),
            RandomFn::Gauss | RandomFn::Normalvariate => (vec![F64, F64], Some(F64)),
            RandomFn::Expovariate => (vec![F64], Some(F64)),
            RandomFn::Shuffle => (vec![I32], None),
            RandomFn::Sample => (vec![I32, I32], Some(I32)),
        }
    }
}

fn rf(ctx: &CompilationContext, routine: RandomFn, args: Vec<E>) -> E {
    let (_, result) = routine.signature();
    call(ctx.helper(Helper::Random(routine)), args, result)
}

fn lm(ctx: &CompilationContext, routine: Libm, args: Vec<E>) -> E {
    let (_, result) = routine.signature();
    call(ctx.helper(Helper::Libm(routine)), args, result)
}

fn mt_at(i: impl Into<E>) -> E {
    (i.into() * 4 + MT).load(Ty::I32, 0)
}

fn set_mt(a: &mut Asm, i: impl Into<E>, value: impl Into<E>) {
    a.store(Ty::I32, i.into() * 4 + MT, 0, value);
}

/// Leave `class(message)` pending, from inside a helper.
fn raise(a: &mut Asm, ctx: &CompilationContext, class: &str, message: &str) {
    let text = call(
        ctx.helper(Helper::ExcText(message.to_string())),
        vec![],
        Some(Ty::I32),
    );
    a.run(call(
        ctx.helper(Helper::RaiseText(class.to_string())),
        vec![text],
        None,
    ));
}

pub(crate) fn build(ctx: &CompilationContext, routine: RandomFn) -> Function {
    let (params, _) = routine.signature();
    let mut a = Asm::new(&params);
    match routine {
        RandomFn::Genrand => genrand(&mut a, ctx),
        RandomFn::InitByArray => init_by_array(&mut a),
        RandomFn::SeedEntropy => {
            let random_get =
                crate::compiler::host::wasi(ctx, crate::compiler::host::Wasi::RandomGet);
            a.run(call(
                random_get,
                vec![MT_KEY.into(), (N * 4).into()],
                Some(Ty::I32),
            ));
            a.run(rf(
                ctx,
                RandomFn::InitByArray,
                vec![MT_KEY.into(), N.into()],
            ));
        }
        RandomFn::SeedWords => {
            let (lo, hi) = (a.param(0), a.param(1));
            a.store(Ty::I32, MT_KEY, 0, lo);
            a.store(Ty::I32, MT_KEY, 4, hi);
            a.run(rf(
                ctx,
                RandomFn::InitByArray,
                vec![MT_KEY.into(), hi.eqz().select(1, 2)],
            ));
        }
        RandomFn::Random => {
            // a*2**26 + b over 53 bits, a drawn first.
            let x = a.let_(rf(ctx, RandomFn::Genrand, vec![]).shr_u(5));
            let y = a.let_(rf(ctx, RandomFn::Genrand, vec![]).shr_u(6));
            a.ret((x.to_f64() * 67108864.0 + y.to_f64()) * (1.0 / 9007199254740992.0));
        }
        RandomFn::Getrandbits => {
            let k = a.param(0);
            a.if_(k.lt(0), |a| {
                raise(a, ctx, "ValueError", "number of bits must be non-negative");
                a.ret(0);
            });
            a.if_(k.eqz(), |a| a.ret(0));
            a.if_(k.le(32), |a| {
                a.ret(rf(ctx, RandomFn::Genrand, vec![]).shr_u(E::from(32) - k));
            });
            // The least significant word comes first; the rest are drawn
            // (and wrap away, as every int past 32 bits does).
            let first = a.let_(rf(ctx, RandomFn::Genrand, vec![]));
            a.set(k, k - 32);
            a.while_(k.gt(0), |a, _| {
                a.run(rf(ctx, RandomFn::Genrand, vec![]));
                a.set(k, k - 32);
            });
            a.ret(first);
        }
        RandomFn::Randbelow => {
            let n = a.param(0);
            // k = n.bit_length(); r = getrandbits(k) until r < n
            let shift = a.let_(n.clz());
            let r = a.local(Ty::I32);
            a.loop_(|a, l| {
                a.set(r, rf(ctx, RandomFn::Genrand, vec![]).shr_u(shift));
                a.cont_if(l, r.ge_u(n));
            });
            a.ret(r);
        }
        RandomFn::Gauss => {
            let (mu, sigma) = (a.param(0), a.param(1));
            let z = a.local(Ty::F64);
            a.if_else(
                E::from(GAUSS_HAS).load(Ty::I32, 0),
                |a| {
                    a.set(z, E::from(GAUSS_NEXT).load(Ty::F64, 0));
                    a.store(Ty::I32, GAUSS_HAS, 0, 0);
                },
                |a| {
                    let x2pi = a.let_(rf(ctx, RandomFn::Random, vec![]) * TWOPI);
                    let u = a.let_(1.0 - rf(ctx, RandomFn::Random, vec![]));
                    let g2rad = a.let_((lm(ctx, Libm::Log, vec![u.e()]) * -2.0).sqrt());
                    a.set(z, lm(ctx, Libm::Cos, vec![x2pi.e()]) * g2rad);
                    a.store(
                        Ty::F64,
                        GAUSS_NEXT,
                        0,
                        lm(ctx, Libm::Sin, vec![x2pi.e()]) * g2rad,
                    );
                    a.store(Ty::I32, GAUSS_HAS, 0, 1);
                },
            );
            a.ret(mu + z * sigma);
        }
        RandomFn::Normalvariate => {
            let (mu, sigma) = (a.param(0), a.param(1));
            let z = a.local(Ty::F64);
            a.loop_(|a, l| {
                let u1 = a.let_(rf(ctx, RandomFn::Random, vec![]));
                let u2 = a.let_(1.0 - rf(ctx, RandomFn::Random, vec![]));
                a.set(z, (u1 - 0.5) * NV_MAGICCONST / u2);
                let zz = a.let_(z * z / 4.0);
                a.brk_if(l, zz.le(-lm(ctx, Libm::Log, vec![u2.e()])));
                a.cont(l);
            });
            a.ret(mu + z * sigma);
        }
        RandomFn::Expovariate => {
            let lambd = a.param(0);
            let x = a.let_(-lm(
                ctx,
                Libm::Log,
                vec![1.0 - rf(ctx, RandomFn::Random, vec![])],
            ));
            a.if_(lambd.eq(0.0), |a| {
                raise(a, ctx, "ZeroDivisionError", "float division by zero");
                a.ret(0.0);
            });
            a.ret(x / lambd);
        }
        RandomFn::Shuffle => {
            let list = a.param(0);
            let data = a.let_(list.load(Ty::I32, COLLECTION_DATA as u64));
            let i = a.local(Ty::I32);
            let j = a.local(Ty::I32);
            let t = a.local(Ty::I64);
            a.for_(i, list.load(Ty::I32, 0) - 1, 1, true, |a, _| {
                a.set(j, rf(ctx, RandomFn::Randbelow, vec![i + 1]));
                let at = |k: Var| k * 8 + data;
                a.set(t, at(i).load(Ty::I64, 0));
                a.store(Ty::I64, at(i), 0, at(j).load(Ty::I64, 0));
                a.store(Ty::I64, at(j), 0, t);
            });
        }
        RandomFn::Sample => sample(&mut a, ctx),
    }
    a.finish()
}

fn genrand(a: &mut Asm, ctx: &CompilationContext) {
    const MATRIX_A: u32 = 0x9908b0df;
    const UPPER: u32 = 0x80000000;
    const LOWER: u32 = 0x7fffffff;
    a.if_(E::from(MT_SEEDED).load(Ty::I32, 0).eqz(), |a| {
        a.run(rf(ctx, RandomFn::SeedEntropy, vec![]));
    });
    let index = a.let_(E::from(MT_INDEX).load(Ty::I32, 0));
    let y = a.local(Ty::I32);
    let kk = a.local(Ty::I32);
    // mt[k] = mt[k + offset] ^ (y >> 1) ^ mag01[y & 1]
    let twist = |a: &mut Asm, k: E, next: E, from: E| {
        a.set(y, (mt_at(k.clone()) & UPPER) | (mt_at(next) & LOWER));
        a.store(
            Ty::I32,
            k * 4 + MT,
            0,
            mt_at(from) ^ y.shr_u(1) ^ ((E::from(0) - (y & 1)) & MATRIX_A),
        );
    };
    a.if_(index.ge(N), |a| {
        a.for_(kk, 0, N - M, false, |a, _| twist(a, kk.e(), kk + 1, kk + M));
        a.for_(kk, N - M, N - 1, false, |a, _| {
            twist(a, kk.e(), kk + 1, kk + (M - N))
        });
        twist(a, (N - 1).into(), 0.into(), (M - 1).into());
        a.set(index, 0);
    });
    a.set(y, mt_at(index));
    a.store(Ty::I32, MT_INDEX, 0, index + 1);
    a.set(y, y ^ y.shr_u(11));
    a.set(y, y ^ ((y << 7) & 0x9d2c5680u32));
    a.set(y, y ^ ((y << 15) & 0xefc60000u32));
    a.ret(y ^ y.shr_u(18));
}

fn init_by_array(a: &mut Asm) {
    let (key, len) = (a.param(0), a.param(1));
    // init_genrand(19650218)
    set_mt(a, 0, 19650218);
    let i = a.local(Ty::I32);
    a.for_(i, 1, N, false, |a, _| {
        let prev = mt_at(i - 1);
        set_mt(a, i, (prev.clone() ^ prev.shr_u(30)) * 1812433253 + i);
    });
    a.set(i, 1);
    let j = a.let_(0);
    let k = a.let_(len.gt_u(N).select(len, N));
    let wrap = |a: &mut Asm| {
        a.set(i, i + 1);
        a.if_(i.ge(N), |a| {
            set_mt(a, 0, mt_at(N - 1));
            a.set(i, 1);
        });
    };
    a.while_(k.ne(0), |a, _| {
        let prev = mt_at(i - 1);
        set_mt(
            a,
            i,
            (mt_at(i) ^ ((prev.clone() ^ prev.shr_u(30)) * 1664525))
                + (j * 4 + key).load(Ty::I32, 0)
                + j,
        );
        wrap(a);
        a.set(j, j + 1);
        a.if_(j.ge_u(len), |a| a.set(j, 0));
        a.set(k, k - 1);
    });
    a.set(k, N - 1);
    a.while_(k.ne(0), |a, _| {
        let prev = mt_at(i - 1);
        set_mt(
            a,
            i,
            (mt_at(i) ^ ((prev.clone() ^ prev.shr_u(30)) * 1566083941)) - i,
        );
        wrap(a);
        a.set(k, k - 1);
    });
    set_mt(a, 0, 0x80000000u32);
    a.store(Ty::I32, MT_INDEX, 0, N);
    a.store(Ty::I32, MT_SEEDED, 0, 1);
    // A seed forgets gauss's cached value.
    a.store(Ty::I32, GAUSS_HAS, 0, 0);
}

/// `random.sample(population, k)`, as `random.py` draws it: from a copied
/// pool when the population is small next to k's set, otherwise by
/// reselecting indices already taken.
fn sample(a: &mut Asm, ctx: &CompilationContext) {
    let (seq, k) = (a.param(0), a.param(1));
    let n = a.let_(seq.load(Ty::I32, 0));
    a.if_(k.lt(0) | k.gt(n), |a| {
        raise(
            a,
            ctx,
            "ValueError",
            "Sample larger than population or is negative",
        );
        a.ret(0);
    });
    let alloc = |size: E| call(ctx.alloc_func_index, vec![size], Some(Ty::I32));
    let result = a.let_(alloc(k * 8 + COLLECTION_HEADER as i32));
    a.store(Ty::I32, result, 0, k);
    a.store(
        Ty::I32,
        result,
        crate::compiler::context::COLLECTION_CAP as u64,
        k,
    );
    a.store(
        Ty::I32,
        result,
        COLLECTION_DATA as u64,
        result + COLLECTION_HEADER as i32,
    );
    let out = |i: Var| i * 8 + (result + COLLECTION_HEADER as i32);
    let src = a.let_(seq.load(Ty::I32, COLLECTION_DATA as u64));
    // setsize = 21, plus 4 ** ceil(log(3k, 4)) for k > 5: the smallest power
    // of four above 3k, since 3k is never one.
    let setsize = a.let_(21i64);
    a.if_(k.gt(5), |a| {
        let p = a.let_(1i64);
        let three_k = a.let_(k.extend_s() * 3);
        a.while_(p.le(three_k), |a, _| a.set(p, p * 4));
        a.set(setsize, setsize + p);
    });
    let i = a.local(Ty::I32);
    let j = a.local(Ty::I32);
    a.if_else(
        n.extend_s().le(setsize),
        |a| {
            // pool = list(population); result[i] = pool[j]; pool[j] = pool[n-i-1]
            let pool = a.let_(alloc(n * 8 + 8));
            a.raw(I::LocalGet(pool.0));
            a.raw(I::LocalGet(src.0));
            a.push(n * 8);
            a.raw(I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            a.for_(i, 0, k, false, |a, _| {
                a.set(j, rf(ctx, RandomFn::Randbelow, vec![n - i]));
                a.store(Ty::I64, out(i), 0, (j * 8 + pool).load(Ty::I64, 0));
                a.store(
                    Ty::I64,
                    j * 8 + pool,
                    0,
                    ((n - i - 1) * 8 + pool).load(Ty::I64, 0),
                );
            });
        },
        |a| {
            // selected: one bit per index
            let bits = a.let_(alloc((n + 7).shr_u(3) + 8));
            a.raw(I::LocalGet(bits.0));
            a.push(0);
            a.push((n + 7).shr_u(3));
            a.raw(I::MemoryFill(0));
            let taken = |j: Var| ((j.shr_u(3) + bits).load8(0) >> (j & 7)) & 1;
            a.for_(i, 0, k, false, |a, _| {
                a.set(j, rf(ctx, RandomFn::Randbelow, vec![n.e()]));
                a.while_(taken(j), |a, _| {
                    a.set(j, rf(ctx, RandomFn::Randbelow, vec![n.e()]))
                });
                a.store8(
                    j.shr_u(3) + bits,
                    0,
                    (j.shr_u(3) + bits).load8(0) | (E::from(1) << (j & 7)),
                );
                a.store(Ty::I64, out(i), 0, (j * 8 + src).load(Ty::I64, 0));
            });
        },
    );
    a.ret(result);
}

// ---- the Python-level functions --------------------------------------------

/// Emit `random.<name>(arguments)`, or `None` when it is not implemented.
pub(crate) fn emit_call(
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Option<IRType> {
    let helper = |routine: RandomFn| I::Call(ctx.helper(Helper::Random(routine)));
    match name {
        "random" => {
            if !no_keywords(name, arguments, func, ctx, 0, 0, Placeholder::F64) {
                return Some(IRType::Float);
            }
            emit!(func, helper(RandomFn::Random));
            Some(IRType::Float)
        }
        "seed" => {
            // seed(a=None, version=2)
            let args = bind(name, arguments, &["a", "version"], ctx)?;
            if let Some(version) = args.get(1).copied().flatten() {
                if !matches!(version, IRExpr::Const(IRConstant::Int(2))) {
                    ctx.report("random.seed() with a version other than 2 is not supported");
                    return Some(IRType::None);
                }
            }
            match args[0] {
                None | Some(IRExpr::Const(IRConstant::None)) => {
                    emit!(func, helper(RandomFn::SeedEntropy));
                }
                Some(arg) => {
                    let ty = emit_expr(arg, func, ctx, memory_layout, None);
                    match ty {
                        IRType::Int | IRType::Bool => {
                            // abs(a): one word, the most negative int's
                            // magnitude included.
                            let v = ctx.temp_local;
                            emit!(
                                func,
                                I::LocalTee(v),
                                I::LocalGet(v),
                                I::I32Const(31),
                                I::I32ShrS,
                                I::I32Xor,
                                I::LocalGet(v),
                                I::I32Const(31),
                                I::I32ShrS,
                                I::I32Sub,
                                I::I32Const(0),
                                helper(RandomFn::SeedWords)
                            );
                        }
                        IRType::Float => {
                            // Its hash, as a size_t.
                            let Some(v) = ctx.hold_f64() else {
                                ctx.report("random.seed() nests too deeply");
                                return Some(IRType::None);
                            };
                            emit!(func, I::LocalSet(v));
                            crate::compiler::sets::emit_hash(
                                func,
                                ctx,
                                &IRType::Float,
                                crate::compiler::sets::Held::F64(v),
                            );
                            ctx.release_held_f64();
                            let h = ctx.hold_i64();
                            emit!(
                                func,
                                I::LocalTee(h),
                                I::I32WrapI64,
                                I::LocalGet(h),
                                I::I64Const(32),
                                I::I64ShrU,
                                I::I32WrapI64,
                                helper(RandomFn::SeedWords)
                            );
                            ctx.release_held_i64();
                        }
                        other => {
                            emit_drop(func, &other);
                            ctx.report(format!(
                                "random.seed() of a {} is not supported: CPython seeds a str or \
                                 bytes from its SHA-512 digest, which is not implemented. Hint: \
                                 seed with an int",
                                crate::type_to_string(&other)
                            ));
                        }
                    }
                }
            }
            Some(IRType::None)
        }
        "getrandbits" => {
            if !no_keywords(name, arguments, func, ctx, 1, 1, Placeholder::I32)
                || !emit_int(name, &arguments[0], func, ctx, memory_layout)
            {
                return Some(IRType::Int);
            }
            emit!(func, helper(RandomFn::Getrandbits));
            emit_post_call_check(func, ctx);
            Some(IRType::Int)
        }
        "randrange" | "randint" => {
            let (min, max) = if name == "randint" { (2, 2) } else { (1, 3) };
            if !no_keywords(name, arguments, func, ctx, min, max, Placeholder::I32) {
                return Some(IRType::Int);
            }
            emit_randrange(name, arguments, func, ctx, memory_layout);
            Some(IRType::Int)
        }
        "uniform" => {
            if !no_keywords(name, arguments, func, ctx, 2, 2, Placeholder::F64) {
                return Some(IRType::Float);
            }
            // a + (b - a) * random()
            let Some(a) = ctx.hold_f64() else {
                return refuse_deep(func, ctx, Placeholder::F64);
            };
            let ta = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            if !to_float(name, &ta, func, ctx) {
                ctx.release_held_f64();
                return Some(IRType::Float);
            }
            emit!(func, I::LocalSet(a));
            let tb = emit_expr(&arguments[1], func, ctx, memory_layout, None);
            if !to_float(name, &tb, func, ctx) {
                ctx.release_held_f64();
                return Some(IRType::Float);
            }
            emit!(
                func,
                I::LocalGet(a),
                I::F64Sub,
                helper(RandomFn::Random),
                I::F64Mul,
                I::LocalGet(a),
                I::F64Add
            );
            ctx.release_held_f64();
            Some(IRType::Float)
        }
        "gauss" | "normalvariate" => {
            let args = bind(name, arguments, &["mu", "sigma"], ctx)?;
            let Some(mu) = ctx.hold_f64() else {
                return refuse_deep(func, ctx, Placeholder::F64);
            };
            let mut ok = true;
            for (k, default) in [(0, 0.0), (1, 1.0)] {
                match args[k] {
                    Some(arg) => {
                        let t = emit_expr(arg, func, ctx, memory_layout, None);
                        ok &= to_float(name, &t, func, ctx);
                    }
                    None => emit!(func, I::F64Const(default.into())),
                }
                if k == 0 {
                    emit!(func, I::LocalSet(mu));
                }
            }
            let sigma = ctx.temp_local_f64;
            emit!(
                func,
                I::LocalSet(sigma),
                I::LocalGet(mu),
                I::LocalGet(sigma)
            );
            ctx.release_held_f64();
            if ok {
                emit!(
                    func,
                    helper(if name == "gauss" {
                        RandomFn::Gauss
                    } else {
                        RandomFn::Normalvariate
                    })
                );
            } else {
                emit!(func, I::Drop);
            }
            Some(IRType::Float)
        }
        "expovariate" => {
            let args = bind(name, arguments, &["lambd"], ctx)?;
            match args[0] {
                Some(arg) => {
                    let t = emit_expr(arg, func, ctx, memory_layout, None);
                    if !to_float(name, &t, func, ctx) {
                        return Some(IRType::Float);
                    }
                }
                None => emit!(func, I::F64Const(1.0.into())),
            }
            emit!(func, helper(RandomFn::Expovariate));
            emit_post_call_check(func, ctx);
            Some(IRType::Float)
        }
        "choice" => {
            if !no_keywords(name, arguments, func, ctx, 1, 1, Placeholder::I32) {
                return Some(IRType::Int);
            }
            let Some(elem) = emit_sequence(name, &arguments[0], func, ctx, memory_layout) else {
                emit!(func, I::I32Const(0));
                return Some(IRType::Int);
            };
            // seq[_randbelow(len(seq))], IndexError when it is empty
            let seq = ctx.temp_local;
            emit!(
                func,
                I::LocalTee(seq),
                I::I32Load(w32(0)),
                I::I32Eqz,
                I::If(BlockType::Empty)
            );
            emit_raise(
                func,
                ctx,
                "IndexError",
                ExcArg::Text("Cannot choose from an empty sequence"),
                1,
            );
            emit!(
                func,
                I::End,
                I::LocalGet(seq),
                I::I32Load(w32(COLLECTION_DATA as u64)),
                I::LocalGet(seq),
                I::I32Load(w32(0)),
                helper(RandomFn::Randbelow),
                I::I32Const(8),
                I::I32Mul,
                I::I32Add
            );
            match elem {
                IRType::Float => emit!(
                    func,
                    I::F64Load(MemArg {
                        offset: 0,
                        align: 3,
                        memory_index: 0
                    })
                ),
                IRType::String | IRType::Bytes => {
                    emit!(func, I::I32Load(w32(0)));
                    recover_str_pair(func, ctx);
                }
                _ => emit!(func, I::I32Load(w32(0))),
            }
            Some(elem)
        }
        "shuffle" => {
            if !no_keywords(name, arguments, func, ctx, 1, 1, Placeholder::None) {
                return Some(IRType::None);
            }
            let ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            if !matches!(ty, IRType::List(_)) {
                emit_drop(func, &ty);
                ctx.report(format!(
                    "random.shuffle() takes a list, got {} (CPython needs a mutable sequence)",
                    crate::type_to_string(&ty)
                ));
                return Some(IRType::None);
            }
            emit!(func, helper(RandomFn::Shuffle));
            Some(IRType::None)
        }
        "sample" => {
            let args = bind(name, arguments, &["population", "k", "counts"], ctx)?;
            if args[2].is_some() {
                ctx.report("random.sample() with counts= is not supported");
            }
            let (Some(population), Some(k)) = (args[0], args[1]) else {
                ctx.report("random.sample() takes a population and k");
                emit!(func, I::I32Const(0));
                return Some(IRType::List(Box::new(IRType::Unknown)));
            };
            let Some(seq) = ctx.hold() else {
                return refuse_deep(func, ctx, Placeholder::I32);
            };
            let elem = emit_sequence(name, population, func, ctx, memory_layout);
            if elem.is_none() {
                emit!(func, I::I32Const(0));
            }
            emit!(func, I::LocalSet(seq));
            let ok = emit_int(name, k, func, ctx, memory_layout);
            let k_local = ctx.temp_local;
            emit!(
                func,
                I::LocalSet(k_local),
                I::LocalGet(seq),
                I::LocalGet(k_local)
            );
            ctx.release_held();
            let Some(elem) = elem.filter(|_| ok) else {
                emit!(func, I::Drop, I::Drop, I::I32Const(0));
                return Some(IRType::List(Box::new(IRType::Unknown)));
            };
            emit!(func, helper(RandomFn::Sample));
            emit_post_call_check(func, ctx);
            Some(IRType::List(Box::new(elem)))
        }
        _ => None,
    }
}

fn w32(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

#[derive(Clone, Copy)]
enum Placeholder {
    I32,
    F64,
    None,
}

fn push_placeholder(func: &mut Function, p: Placeholder) {
    match p {
        Placeholder::I32 => emit!(func, I::I32Const(0)),
        Placeholder::F64 => emit!(func, I::F64Const(0.0.into())),
        Placeholder::None => {}
    }
}

fn refuse_deep(func: &mut Function, ctx: &CompilationContext, p: Placeholder) -> Option<IRType> {
    ctx.report("a random call nests more deeply than the compiler holds values for");
    push_placeholder(func, p);
    Some(match p {
        Placeholder::I32 => IRType::Int,
        Placeholder::F64 => IRType::Float,
        Placeholder::None => IRType::None,
    })
}

fn emit_drop(func: &mut Function, ty: &IRType) {
    if matches!(ty, IRType::None) {
        return;
    }
    emit!(func, I::Drop);
    if matches!(ty, IRType::String | IRType::Bytes) {
        emit!(func, I::Drop);
    }
}

/// Check the count of positional arguments, with no keywords; on a mismatch
/// report and push the placeholder.
fn no_keywords(
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    min: usize,
    max: usize,
    placeholder: Placeholder,
) -> bool {
    if arguments
        .iter()
        .any(|a| matches!(a, IRExpr::Keyword { .. }))
    {
        ctx.report(format!("random.{name}() takes no keyword arguments here"));
    } else if arguments.len() < min || arguments.len() > max {
        ctx.report(format!(
            "random.{name}() takes {} argument{}, got {}",
            if min == max {
                min.to_string()
            } else {
                format!("{min} to {max}")
            },
            if max == 1 { "" } else { "s" },
            arguments.len()
        ));
    } else {
        return true;
    }
    push_placeholder(func, placeholder);
    false
}

/// Bind positional and keyword arguments to the parameter names, reporting
/// (and answering `None`, which the caller refuses) when they do not fit.
fn bind<'a>(
    name: &str,
    arguments: &'a [IRExpr],
    params: &[&str],
    ctx: &CompilationContext,
) -> Option<Vec<Option<&'a IRExpr>>> {
    let mut bound: Vec<Option<&IRExpr>> = vec![None; params.len()];
    let mut positional = 0;
    for arg in arguments {
        match arg {
            IRExpr::Keyword { name: key, value } => {
                let Some(k) = params.iter().position(|p| p == key) else {
                    ctx.report(format!(
                        "random.{name}() got an unexpected keyword argument '{key}'"
                    ));
                    return None;
                };
                if bound[k].is_some() {
                    ctx.report(format!("random.{name}() got multiple values for '{key}'"));
                    return None;
                }
                bound[k] = Some(value);
            }
            value => {
                if positional >= params.len() {
                    ctx.report(format!(
                        "random.{name}() takes at most {} arguments ({} given)",
                        params.len(),
                        arguments.len()
                    ));
                    return None;
                }
                bound[positional] = Some(value);
                positional += 1;
            }
        }
    }
    Some(bound)
}

/// Convert a number on the stack to a float; refuse anything else.
fn to_float(name: &str, ty: &IRType, func: &mut Function, ctx: &CompilationContext) -> bool {
    match ty {
        IRType::Float => true,
        IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any => {
            emit!(func, I::F64ConvertI32S);
            true
        }
        other => {
            emit_drop(func, other);
            emit!(func, I::F64Const(0.0.into()));
            ctx.report(format!(
                "random.{name}() takes numbers, got {}",
                crate::type_to_string(other)
            ));
            false
        }
    }
}

/// Emit an argument that must be an int (CPython's `_index`).
fn emit_int(
    name: &str,
    arg: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> bool {
    let ty = emit_expr(arg, func, ctx, memory_layout, Some(&IRType::Int));
    match ty {
        IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any => true,
        other => {
            emit_drop(func, &other);
            emit!(func, I::I32Const(0));
            ctx.report(format!(
                "random.{name}() takes an int, got {} ('{}' object cannot be interpreted as an \
                 integer)",
                crate::type_to_string(&other),
                crate::compiler::operators::python_type_name(&other)
            ));
            false
        }
    }
}

/// Emit a population as a list-layout sequence (a list, a tuple of one
/// type, the characters of a str), giving its element type.
fn emit_sequence(
    name: &str,
    arg: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Option<IRType> {
    let ty = emit_expr(arg, func, ctx, memory_layout, None);
    if matches!(ty, IRType::Set(_) | IRType::Dict(_, _)) {
        emit_drop(func, &ty);
        ctx.report(format!(
            "random.{name}() takes a sequence; a {} is not one (CPython's TypeError). Hint: \
             pass sorted(...) of it",
            crate::type_to_string(&ty)
        ));
        return None;
    }
    match emit_as_sequence(func, ctx, ty) {
        IRType::List(elem) | IRType::VarTuple(elem) => Some(*elem),
        other => {
            emit_drop(func, &other);
            ctx.report(format!(
                "random.{name}() takes a list, tuple, or str here, got {}",
                crate::type_to_string(&other)
            ));
            None
        }
    }
}

/// `randrange(start[, stop[, step]])` and `randint(a, b)` (which is
/// `randrange(a, b + 1)`), with CPython's checks and messages.
fn emit_randrange(
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) {
    // Every argument is held: the message of an empty range names them.
    let mut held = Vec::new();
    let mut ok = true;
    for arg in arguments {
        let Some(slot) = ctx.hold() else {
            ctx.report("random.randrange() nests too deeply");
            ok = false;
            break;
        };
        ok &= emit_int(name, arg, func, ctx, memory_layout);
        emit!(func, I::LocalSet(slot));
        held.push(slot);
    }
    if !ok {
        for _ in &held {
            ctx.release_held();
        }
        emit!(func, I::I32Const(0));
        return;
    }
    let randbelow = I::Call(ctx.helper(Helper::Random(RandomFn::Randbelow)));
    let (width, step, n) = (ctx.temp_local, ctx.temp_local + 1, ctx.temp_local + 2);
    if name == "randint" {
        // randrange(a, b + 1)
        emit!(
            func,
            I::LocalGet(held[1]),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(held[1])
        );
    }
    let message = |func: &mut Function, args: &[u32]| {
        // f"empty range in randrange({start}, {stop}[, {step}])"
        let concat = ctx.helper(Helper::Concat);
        let to_str = ctx.helper(Helper::Str(IRType::Int));
        emit!(
            func,
            I::Call(ctx.helper(Helper::ExcText("empty range in randrange(".to_string())))
        );
        for (k, slot) in args.iter().enumerate() {
            if k > 0 {
                emit!(
                    func,
                    I::Call(ctx.helper(Helper::ExcText(", ".to_string()))),
                    I::Call(concat)
                );
            }
            emit!(func, I::LocalGet(*slot), I::Call(to_str), I::Call(concat));
        }
        emit!(
            func,
            I::Call(ctx.helper(Helper::ExcText(")".to_string()))),
            I::Call(concat),
            I::Call(ctx.helper(Helper::RaiseText("ValueError".to_string())))
        );
    };
    if held.len() == 1 {
        // randrange(stop)
        emit!(
            func,
            I::LocalGet(held[0]),
            I::I32Const(0),
            I::I32LeS,
            I::If(BlockType::Empty)
        );
        emit_raise(
            func,
            ctx,
            "ValueError",
            ExcArg::Text("empty range for randrange()"),
            1,
        );
        emit!(func, I::End, I::LocalGet(held[0]), randbelow);
    } else {
        // width = stop - start
        emit!(
            func,
            I::LocalGet(held[1]),
            I::LocalGet(held[0]),
            I::I32Sub,
            I::LocalSet(width)
        );
        if held.len() == 2 {
            emit!(
                func,
                I::LocalGet(width),
                I::I32Const(0),
                I::I32LeS,
                I::If(BlockType::Empty)
            );
            message(func, &held);
            crate::compiler::function::emit_exception_transfer(func, ctx, 1);
            emit!(
                func,
                I::End,
                I::LocalGet(held[0]),
                I::LocalGet(width),
                randbelow,
                I::I32Add
            );
        } else {
            emit!(
                func,
                I::LocalGet(held[2]),
                I::LocalTee(step),
                I::I32Eqz,
                I::If(BlockType::Empty)
            );
            emit_raise(
                func,
                ctx,
                "ValueError",
                ExcArg::Text("zero step for randrange()"),
                1,
            );
            // n = (width + step - 1) // step for a positive step, and
            // (width + step + 1) // step for a negative one.
            emit!(
                func,
                I::End,
                I::LocalGet(width),
                I::LocalGet(step),
                I::I32Add,
                I::I32Const(-1),
                I::I32Const(1),
                I::LocalGet(step),
                I::I32Const(0),
                I::I32GtS,
                I::Select,
                I::I32Add,
                I::LocalGet(step)
            );
            crate::compiler::expression::emit_floor_div_i32(func, ctx);
            emit!(
                func,
                I::LocalTee(n),
                I::I32Const(0),
                I::I32LeS,
                I::If(BlockType::Empty)
            );
            message(func, &held);
            crate::compiler::function::emit_exception_transfer(func, ctx, 1);
            emit!(
                func,
                I::End,
                I::LocalGet(held[0]),
                I::LocalGet(held[2]),
                I::LocalGet(n),
                randbelow,
                I::I32Mul,
                I::I32Add
            );
        }
    }
    for _ in &held {
        ctx.release_held();
    }
}

/// What `random.<name>()` answers.
pub(crate) fn result_type(
    name: &str,
    arguments: &[IRExpr],
    ctx: &CompilationContext,
) -> Option<IRType> {
    let element = || {
        let arg = arguments.first()?;
        match crate::compiler::function::infer_value_type(arg, ctx) {
            IRType::List(e) | IRType::VarTuple(e) => Some(*e),
            IRType::String => Some(IRType::String),
            IRType::Tuple(members) => members
                .first()
                .filter(|first| members.iter().all(|m| m == *first))
                .cloned(),
            _ => None,
        }
    };
    Some(match name {
        "random" | "uniform" | "gauss" | "normalvariate" | "expovariate" => IRType::Float,
        "getrandbits" | "randrange" | "randint" => IRType::Int,
        "seed" | "shuffle" => IRType::None,
        "choice" => element()?,
        "sample" => IRType::List(Box::new(element()?)),
        _ => return None,
    })
}
