//! A structured builder for runtime helpers written as code.
//!
//! The libm port and the other numeric helpers are long straight-line
//! computations over `f64`, `i32`, and `i64`, which read badly as instruction
//! lists. Here an expression is a tree (`E`) whose operators pick the
//! instruction from the operand types, and statements (`set`, `if_`, `ret`,
//! loops) are methods on `Asm`, which owns the locals and the code. A
//! constant next to an operand of another width takes that operand's type, so
//! `x * 0.5`, `hx & 0x7fffffff`, and `bits(x) >> 32` read as they do in C.
//!
//! Nothing here allocates or calls back into user code: a helper built this
//! way is a pure function of its parameters and the memory it is pointed at.

use wasm_encoder::{BlockType, Function, Instruction as I, MemArg, ValType};

/// The value types an expression can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    I32,
    I64,
    F64,
}

impl Ty {
    pub fn val(self) -> ValType {
        match self {
            Ty::I32 => ValType::I32,
            Ty::I64 => ValType::I64,
            Ty::F64 => ValType::F64,
        }
    }
}

/// A binary operation. Integer division, remainder, shifts, and comparisons
/// are signed unless they say otherwise; on floats the signedness is moot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    DivU,
    Rem,
    RemU,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    ShrU,
    Eq,
    Ne,
    Lt,
    LtU,
    Le,
    LeU,
    Gt,
    GtU,
    Ge,
    GeU,
    Min,
    Max,
    Copysign,
}

/// A unary operation or conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Un {
    Neg,
    Abs,
    Sqrt,
    Floor,
    Ceil,
    Trunc,
    Nearest,
    Eqz,
    /// Leading zero bits.
    Clz,
    /// f64 -> i64 bits, i64 bits -> f64.
    Bits,
    FromBits,
    /// i64 -> i32 (low word).
    Wrap,
    /// i32 -> i64.
    ExtendS,
    ExtendU,
    /// int -> f64, signed or unsigned.
    ToF64,
    ToF64U,
    /// f64 -> i32 / i64, truncating; traps outside the range, so only for
    /// values known to fit.
    ToI32,
    ToI64,
}

/// An expression.
#[derive(Clone, Debug)]
pub enum E {
    I32(i32),
    I64(i64),
    F64(f64),
    Get(u32, Ty),
    Bin(Op, Box<E>, Box<E>),
    Un(Un, Box<E>),
    /// `call f(args)`, giving a value of the type (or none).
    Call(u32, Vec<E>, Option<Ty>),
    /// `cond ? a : b`, both evaluated.
    Select(Box<E>, Box<E>, Box<E>),
    /// `cond ? a : b`, only the taken arm evaluated.
    If(Box<E>, Box<E>, Box<E>),
    /// A load of the type from `addr + offset`.
    Load(Ty, Box<E>, u64),
    /// A byte load, zero-extended to i32.
    Load8(Box<E>, u64),
    /// Store the value in the local, and give it.
    Tee(u32, Ty, Box<E>),
}

/// A local (or parameter) of a helper being built.
#[derive(Clone, Copy, Debug)]
pub struct Var(pub u32, pub Ty);

impl From<Var> for E {
    fn from(v: Var) -> E {
        E::Get(v.0, v.1)
    }
}
impl From<i32> for E {
    fn from(v: i32) -> E {
        E::I32(v)
    }
}
impl From<u32> for E {
    fn from(v: u32) -> E {
        E::I32(v as i32)
    }
}
impl From<i64> for E {
    fn from(v: i64) -> E {
        E::I64(v)
    }
}
impl From<u64> for E {
    fn from(v: u64) -> E {
        E::I64(v as i64)
    }
}
impl From<f64> for E {
    fn from(v: f64) -> E {
        E::F64(v)
    }
}
impl From<bool> for E {
    fn from(v: bool) -> E {
        E::I32(v as i32)
    }
}

/// An f64 constant from its bits.
pub fn fbits(bits: u64) -> E {
    E::F64(f64::from_bits(bits))
}

impl E {
    pub fn ty(&self) -> Ty {
        match self {
            E::I32(_) | E::Load8(..) => Ty::I32,
            E::I64(_) => Ty::I64,
            E::F64(_) => Ty::F64,
            E::Get(_, t) | E::Tee(_, t, _) | E::Load(t, _, _) => *t,
            E::Bin(op, a, _) => match op {
                Op::Eq
                | Op::Ne
                | Op::Lt
                | Op::LtU
                | Op::Le
                | Op::LeU
                | Op::Gt
                | Op::GtU
                | Op::Ge
                | Op::GeU => Ty::I32,
                _ => a.ty(),
            },
            E::Un(op, a) => match op {
                Un::Eqz | Un::Wrap | Un::ToI32 => Ty::I32,
                Un::Bits | Un::ExtendS | Un::ExtendU | Un::ToI64 => Ty::I64,
                Un::FromBits | Un::ToF64 | Un::ToF64U => Ty::F64,
                _ => a.ty(),
            },
            E::Call(_, _, t) => t.expect("a call used as a value returns one"),
            E::Select(_, a, _) | E::If(_, a, _) => a.ty(),
        }
    }

    /// A constant retyped to `ty`, where a literal stands next to an operand
    /// of another width.
    fn coerce(self, ty: Ty) -> E {
        match (self, ty) {
            (E::I32(v), Ty::I64) => E::I64(v as i64),
            (E::I32(v), Ty::F64) => E::F64(v as f64),
            (E::I64(v), Ty::F64) => E::F64(v as f64),
            (e, _) => e,
        }
    }

    pub fn bin(op: Op, a: impl Into<E>, b: impl Into<E>) -> E {
        let (a, b) = (a.into(), b.into());
        let shift = matches!(op, Op::Shl | Op::Shr | Op::ShrU);
        let (a, b) = match (&a, &b) {
            (_, E::I32(_) | E::I64(_) | E::F64(_)) => {
                let t = a.ty();
                (a, b.coerce(t))
            }
            (E::I32(_) | E::I64(_) | E::F64(_), _) if !shift => {
                let t = b.ty();
                (a.coerce(t), b)
            }
            _ => (a, b),
        };
        assert_eq!(a.ty(), b.ty(), "operands of {op:?} disagree: {a:?} {b:?}");
        E::Bin(op, Box::new(a), Box::new(b))
    }

    fn un(op: Un, a: impl Into<E>) -> E {
        E::Un(op, Box::new(a.into()))
    }

    fn emit(&self, f: &mut Vec<I<'static>>) {
        match self {
            E::I32(v) => f.push(I::I32Const(*v)),
            E::I64(v) => f.push(I::I64Const(*v)),
            E::F64(v) => f.push(I::F64Const((*v).into())),
            E::Get(l, _) => f.push(I::LocalGet(*l)),
            E::Tee(l, _, v) => {
                v.emit(f);
                f.push(I::LocalTee(*l));
            }
            E::Load(t, addr, offset) => {
                addr.emit(f);
                let m = MemArg {
                    offset: *offset,
                    align: match t {
                        Ty::I32 => 2,
                        _ => 3,
                    },
                    memory_index: 0,
                };
                f.push(match t {
                    Ty::I32 => I::I32Load(m),
                    Ty::I64 => I::I64Load(m),
                    Ty::F64 => I::F64Load(m),
                });
            }
            E::Load8(addr, offset) => {
                addr.emit(f);
                f.push(I::I32Load8U(MemArg {
                    offset: *offset,
                    align: 0,
                    memory_index: 0,
                }));
            }
            E::Call(index, args, _) => {
                for a in args {
                    a.emit(f);
                }
                f.push(I::Call(*index));
            }
            E::Select(c, a, b) => {
                a.emit(f);
                b.emit(f);
                c.emit(f);
                f.push(I::Select);
            }
            E::If(c, a, b) => {
                c.emit(f);
                f.push(I::If(BlockType::Result(a.ty().val())));
                a.emit(f);
                f.push(I::Else);
                b.emit(f);
                f.push(I::End);
            }
            E::Un(op, a) => {
                let t = a.ty();
                if *op == Un::Neg && t != Ty::F64 {
                    // 0 - a
                    E::bin(Op::Sub, E::I32(0).coerce(t), (**a).clone()).emit(f);
                    return;
                }
                a.emit(f);
                f.push(match (op, t) {
                    (Un::Neg, _) => I::F64Neg,
                    (Un::Abs, Ty::F64) => I::F64Abs,
                    (Un::Sqrt, Ty::F64) => I::F64Sqrt,
                    (Un::Floor, Ty::F64) => I::F64Floor,
                    (Un::Ceil, Ty::F64) => I::F64Ceil,
                    (Un::Trunc, Ty::F64) => I::F64Trunc,
                    (Un::Nearest, Ty::F64) => I::F64Nearest,
                    (Un::Eqz, Ty::I32) => I::I32Eqz,
                    (Un::Clz, Ty::I32) => I::I32Clz,
                    (Un::Clz, Ty::I64) => I::I64Clz,
                    (Un::Eqz, Ty::I64) => I::I64Eqz,
                    (Un::Bits, Ty::F64) => I::I64ReinterpretF64,
                    (Un::FromBits, Ty::I64) => I::F64ReinterpretI64,
                    (Un::Wrap, Ty::I64) => I::I32WrapI64,
                    (Un::ExtendS, Ty::I32) => I::I64ExtendI32S,
                    (Un::ExtendU, Ty::I32) => I::I64ExtendI32U,
                    (Un::ToF64, Ty::I32) => I::F64ConvertI32S,
                    (Un::ToF64, Ty::I64) => I::F64ConvertI64S,
                    (Un::ToF64U, Ty::I32) => I::F64ConvertI32U,
                    (Un::ToF64U, Ty::I64) => I::F64ConvertI64U,
                    (Un::ToI32, Ty::F64) => I::I32TruncF64S,
                    (Un::ToI64, Ty::F64) => I::I64TruncF64S,
                    (op, t) => panic!("no {op:?} on {t:?}"),
                });
            }
            E::Bin(op, a, b) => {
                let t = a.ty();
                a.emit(f);
                b.emit(f);
                f.push(bin_instruction(*op, t));
            }
        }
    }
}

fn bin_instruction(op: Op, t: Ty) -> I<'static> {
    match t {
        Ty::I32 => match op {
            Op::Add => I::I32Add,
            Op::Sub => I::I32Sub,
            Op::Mul => I::I32Mul,
            Op::Div => I::I32DivS,
            Op::DivU => I::I32DivU,
            Op::Rem => I::I32RemS,
            Op::RemU => I::I32RemU,
            Op::And => I::I32And,
            Op::Or => I::I32Or,
            Op::Xor => I::I32Xor,
            Op::Shl => I::I32Shl,
            Op::Shr => I::I32ShrS,
            Op::ShrU => I::I32ShrU,
            Op::Eq => I::I32Eq,
            Op::Ne => I::I32Ne,
            Op::Lt => I::I32LtS,
            Op::LtU => I::I32LtU,
            Op::Le => I::I32LeS,
            Op::LeU => I::I32LeU,
            Op::Gt => I::I32GtS,
            Op::GtU => I::I32GtU,
            Op::Ge => I::I32GeS,
            Op::GeU => I::I32GeU,
            _ => panic!("no {op:?} on i32"),
        },
        Ty::I64 => match op {
            Op::Add => I::I64Add,
            Op::Sub => I::I64Sub,
            Op::Mul => I::I64Mul,
            Op::Div => I::I64DivS,
            Op::DivU => I::I64DivU,
            Op::Rem => I::I64RemS,
            Op::RemU => I::I64RemU,
            Op::And => I::I64And,
            Op::Or => I::I64Or,
            Op::Xor => I::I64Xor,
            Op::Shl => I::I64Shl,
            Op::Shr => I::I64ShrS,
            Op::ShrU => I::I64ShrU,
            Op::Eq => I::I64Eq,
            Op::Ne => I::I64Ne,
            Op::Lt => I::I64LtS,
            Op::LtU => I::I64LtU,
            Op::Le => I::I64LeS,
            Op::LeU => I::I64LeU,
            Op::Gt => I::I64GtS,
            Op::GtU => I::I64GtU,
            Op::Ge => I::I64GeS,
            Op::GeU => I::I64GeU,
            _ => panic!("no {op:?} on i64"),
        },
        Ty::F64 => match op {
            Op::Add => I::F64Add,
            Op::Sub => I::F64Sub,
            Op::Mul => I::F64Mul,
            Op::Div => I::F64Div,
            Op::Eq => I::F64Eq,
            Op::Ne => I::F64Ne,
            Op::Lt => I::F64Lt,
            Op::Le => I::F64Le,
            Op::Gt => I::F64Gt,
            Op::Ge => I::F64Ge,
            Op::Min => I::F64Min,
            Op::Max => I::F64Max,
            Op::Copysign => I::F64Copysign,
            _ => panic!("no {op:?} on f64"),
        },
    }
}

macro_rules! arith {
    ($trait:ident, $method:ident, $op:expr) => {
        impl<R: Into<E>> std::ops::$trait<R> for E {
            type Output = E;
            fn $method(self, r: R) -> E {
                E::bin($op, self, r)
            }
        }
        impl<R: Into<E>> std::ops::$trait<R> for Var {
            type Output = E;
            fn $method(self, r: R) -> E {
                E::bin($op, self, r)
            }
        }
    };
}
/// A literal on the left: `1.0 - x`, `0x3ff - k`.
macro_rules! arith_lit {
    ($trait:ident, $method:ident, $op:expr, $($lit:ty),*) => {
        $(
            impl std::ops::$trait<E> for $lit {
                type Output = E;
                fn $method(self, r: E) -> E {
                    E::bin($op, self, r)
                }
            }
            impl std::ops::$trait<Var> for $lit {
                type Output = E;
                fn $method(self, r: Var) -> E {
                    E::bin($op, self, r)
                }
            }
        )*
    };
}
arith!(Add, add, Op::Add);
arith!(Sub, sub, Op::Sub);
arith!(Mul, mul, Op::Mul);
arith!(Div, div, Op::Div);
arith_lit!(Add, add, Op::Add, f64, i32, i64);
arith_lit!(Sub, sub, Op::Sub, f64, i32, i64);
arith_lit!(Mul, mul, Op::Mul, f64, i32, i64);
arith_lit!(Div, div, Op::Div, f64, i32, i64);
arith!(Rem, rem, Op::Rem);
arith!(BitAnd, bitand, Op::And);
arith!(BitOr, bitor, Op::Or);
arith!(BitXor, bitxor, Op::Xor);
arith!(Shl, shl, Op::Shl);
// `>>` is arithmetic; `shr_u` is the logical shift.
arith!(Shr, shr, Op::Shr);

impl std::ops::Neg for E {
    type Output = E;
    fn neg(self) -> E {
        E::un(Un::Neg, self)
    }
}
impl std::ops::Neg for Var {
    type Output = E;
    fn neg(self) -> E {
        E::un(Un::Neg, self)
    }
}

/// The methods every expression-like value has.
pub trait X: Into<E> + Sized {
    fn e(self) -> E {
        self.into()
    }
    fn eq(self, r: impl Into<E>) -> E {
        E::bin(Op::Eq, self, r)
    }
    fn ne(self, r: impl Into<E>) -> E {
        E::bin(Op::Ne, self, r)
    }
    fn lt(self, r: impl Into<E>) -> E {
        E::bin(Op::Lt, self, r)
    }
    fn le(self, r: impl Into<E>) -> E {
        E::bin(Op::Le, self, r)
    }
    fn gt(self, r: impl Into<E>) -> E {
        E::bin(Op::Gt, self, r)
    }
    fn ge(self, r: impl Into<E>) -> E {
        E::bin(Op::Ge, self, r)
    }
    fn lt_u(self, r: impl Into<E>) -> E {
        E::bin(Op::LtU, self, r)
    }
    fn le_u(self, r: impl Into<E>) -> E {
        E::bin(Op::LeU, self, r)
    }
    fn gt_u(self, r: impl Into<E>) -> E {
        E::bin(Op::GtU, self, r)
    }
    fn ge_u(self, r: impl Into<E>) -> E {
        E::bin(Op::GeU, self, r)
    }
    fn shr_u(self, r: impl Into<E>) -> E {
        E::bin(Op::ShrU, self, r)
    }
    fn copysign(self, r: impl Into<E>) -> E {
        E::bin(Op::Copysign, self, r)
    }
    fn abs(self) -> E {
        E::un(Un::Abs, self)
    }
    fn sqrt(self) -> E {
        E::un(Un::Sqrt, self)
    }
    fn floor(self) -> E {
        E::un(Un::Floor, self)
    }
    fn eqz(self) -> E {
        E::un(Un::Eqz, self)
    }
    fn clz(self) -> E {
        E::un(Un::Clz, self)
    }
    /// The bits of an f64, as an i64.
    fn bits(self) -> E {
        E::un(Un::Bits, self)
    }
    /// The f64 with these i64 bits.
    fn bits_to_f64(self) -> E {
        E::un(Un::FromBits, self)
    }
    fn wrap(self) -> E {
        E::un(Un::Wrap, self)
    }
    fn extend_s(self) -> E {
        E::un(Un::ExtendS, self)
    }
    fn extend_u(self) -> E {
        E::un(Un::ExtendU, self)
    }
    fn to_f64(self) -> E {
        E::un(Un::ToF64, self)
    }
    fn to_i32(self) -> E {
        E::un(Un::ToI32, self)
    }
    /// The high word of an f64, as C's `GET_HIGH_WORD` gives it.
    fn hi(self) -> E {
        self.bits().shr_u(32i64).wrap()
    }
    /// The low word of an f64.
    fn lo(self) -> E {
        self.bits().wrap()
    }
    /// This f64 with its high word replaced.
    fn with_hi(self, hi: impl Into<E>) -> E {
        ((hi.into().extend_u() << 32i64) | (self.bits() & 0xffff_ffffi64)).bits_to_f64()
    }
    /// This f64 with its low word replaced.
    fn with_lo(self, lo: impl Into<E>) -> E {
        ((self.bits() & (0xffff_ffff_0000_0000u64 as i64)) | lo.into().extend_u()).bits_to_f64()
    }
    /// A select of `a` or `b` on this condition, both evaluated.
    fn select(self, a: impl Into<E>, b: impl Into<E>) -> E {
        let (a, b) = (a.into(), b.into());
        let t = a.ty();
        E::Select(Box::new(self.into()), Box::new(a), Box::new(b.coerce(t)))
    }
    /// `a` or `b` on this condition, only the taken one evaluated.
    fn pick(self, a: impl Into<E>, b: impl Into<E>) -> E {
        let (a, b) = (a.into(), b.into());
        let t = a.ty();
        E::If(Box::new(self.into()), Box::new(a), Box::new(b.coerce(t)))
    }
    /// A load of the type from this address plus `offset`.
    fn load(self, ty: Ty, offset: u64) -> E {
        E::Load(ty, Box::new(self.into()), offset)
    }
    fn load8(self, offset: u64) -> E {
        E::Load8(Box::new(self.into()), offset)
    }
}
impl X for E {}
impl X for Var {}

/// The f64 made of two words, as C's `INSERT_WORDS` builds it.
pub fn words(hi: impl Into<E>, lo: impl Into<E>) -> E {
    ((hi.into().extend_u() << 32i64) | lo.into().extend_u()).bits_to_f64()
}

/// A call to the function at `index`.
pub fn call(index: u32, args: Vec<E>, result: Option<Ty>) -> E {
    E::Call(index, args, result)
}

/// A helper's body under construction.
pub struct Asm {
    params: usize,
    locals: Vec<Ty>,
    code: Vec<I<'static>>,
    /// The kind of each open block, innermost last; `true` for a loop.
    blocks: Vec<bool>,
}

/// A label to branch to: `brk` leaves the block, `cont` (on a loop) starts
/// the next iteration.
#[derive(Clone, Copy)]
pub struct Label(usize);

impl Asm {
    pub fn new(params: &[Ty]) -> Asm {
        Asm {
            params: params.len(),
            locals: params.to_vec(),
            code: Vec::new(),
            blocks: Vec::new(),
        }
    }

    pub fn param(&self, i: u32) -> Var {
        assert!((i as usize) < self.params);
        Var(i, self.locals[i as usize])
    }

    pub fn local(&mut self, ty: Ty) -> Var {
        self.locals.push(ty);
        Var(self.locals.len() as u32 - 1, ty)
    }

    /// A new local holding `value`.
    pub fn let_(&mut self, value: impl Into<E>) -> Var {
        let value = value.into();
        let v = self.local(value.ty());
        self.set(v, value);
        v
    }

    pub fn set(&mut self, v: Var, value: impl Into<E>) {
        let value = value.into().coerce(v.1);
        assert_eq!(value.ty(), v.1, "set of a {:?} local to {value:?}", v.1);
        value.emit(&mut self.code);
        self.code.push(I::LocalSet(v.0));
    }

    /// Evaluate for effect: a call with no result, or a value to drop.
    pub fn run(&mut self, e: impl Into<E>) {
        let e = e.into();
        let has_value = !matches!(e, E::Call(_, _, None));
        e.emit(&mut self.code);
        if has_value {
            self.code.push(I::Drop);
        }
    }

    pub fn store(&mut self, ty: Ty, addr: impl Into<E>, offset: u64, value: impl Into<E>) {
        addr.into().emit(&mut self.code);
        let value = value.into().coerce(ty);
        value.emit(&mut self.code);
        let m = MemArg {
            offset,
            align: match ty {
                Ty::I32 => 2,
                _ => 3,
            },
            memory_index: 0,
        };
        self.code.push(match ty {
            Ty::I32 => I::I32Store(m),
            Ty::I64 => I::I64Store(m),
            Ty::F64 => I::F64Store(m),
        });
    }

    pub fn store8(&mut self, addr: impl Into<E>, offset: u64, value: impl Into<E>) {
        addr.into().emit(&mut self.code);
        value.into().emit(&mut self.code);
        self.code.push(I::I32Store8(MemArg {
            offset,
            align: 0,
            memory_index: 0,
        }));
    }

    pub fn ret(&mut self, value: impl Into<E>) {
        value.into().emit(&mut self.code);
        self.code.push(I::Return);
    }

    /// Push a raw instruction (for what the builder does not model).
    pub fn raw(&mut self, ins: I<'static>) {
        self.code.push(ins);
    }

    /// Push the value of `e` without consuming it, for a raw sequence.
    pub fn push(&mut self, e: impl Into<E>) {
        e.into().emit(&mut self.code);
    }

    pub fn if_(&mut self, cond: impl Into<E>, then: impl FnOnce(&mut Asm)) {
        cond.into().emit(&mut self.code);
        self.code.push(I::If(BlockType::Empty));
        self.blocks.push(false);
        then(self);
        self.blocks.pop();
        self.code.push(I::End);
    }

    pub fn if_else(
        &mut self,
        cond: impl Into<E>,
        then: impl FnOnce(&mut Asm),
        otherwise: impl FnOnce(&mut Asm),
    ) {
        cond.into().emit(&mut self.code);
        self.code.push(I::If(BlockType::Empty));
        self.blocks.push(false);
        then(self);
        self.code.push(I::Else);
        otherwise(self);
        self.blocks.pop();
        self.code.push(I::End);
    }

    /// `loop { body }`: the body runs until it branches out with `brk` (its
    /// label leaves the loop) or falls off the end, which also leaves; `cont`
    /// starts the next iteration.
    pub fn loop_(&mut self, body: impl FnOnce(&mut Asm, Label)) {
        self.code.push(I::Block(BlockType::Empty));
        self.blocks.push(false);
        self.code.push(I::Loop(BlockType::Empty));
        self.blocks.push(true);
        let label = Label(self.blocks.len());
        body(self, label);
        self.blocks.pop();
        self.code.push(I::End);
        self.blocks.pop();
        self.code.push(I::End);
    }

    /// `while cond { body }`.
    pub fn while_(&mut self, cond: impl Into<E>, body: impl FnOnce(&mut Asm, Label)) {
        let cond = cond.into();
        self.loop_(|a, l| {
            a.brk_unless(l, cond);
            body(a, l);
            a.cont(l);
        });
    }

    /// `for v in from..to` (or down from `from` to `to`, inclusive, with
    /// `down`), counting in the i32 local `v`.
    pub fn for_(
        &mut self,
        v: Var,
        from: impl Into<E>,
        to: impl Into<E>,
        down: bool,
        body: impl FnOnce(&mut Asm, Label),
    ) {
        self.set(v, from);
        let to = to.into();
        let cond = if down { v.ge(to) } else { v.lt(to) };
        self.loop_(|a, l| {
            a.brk_unless(l, cond);
            body(a, l);
            a.set(v, v + if down { -1 } else { 1 });
            a.cont(l);
        });
    }

    fn depth_to(&self, label: Label, cont: bool) -> u32 {
        // A loop label's block is the one around the loop.
        let target = if cont { label.0 } else { label.0 - 1 };
        if cont {
            assert!(
                self.blocks[label.0 - 1],
                "cont on a block that is not a loop"
            );
        }
        (self.blocks.len() - target) as u32
    }

    pub fn brk_if(&mut self, label: Label, cond: impl Into<E>) {
        cond.into().emit(&mut self.code);
        let depth = if self.blocks[label.0 - 1] {
            self.depth_to(label, false)
        } else {
            (self.blocks.len() - label.0) as u32
        };
        self.code.push(I::BrIf(depth));
    }

    pub fn brk_unless(&mut self, label: Label, cond: impl Into<E>) {
        self.brk_if(label, cond.into().eqz());
    }

    /// Start the loop's next iteration.
    pub fn cont(&mut self, label: Label) {
        let depth = self.depth_to(label, true);
        self.code.push(I::Br(depth));
    }

    pub fn cont_if(&mut self, label: Label, cond: impl Into<E>) {
        cond.into().emit(&mut self.code);
        let depth = self.depth_to(label, true);
        self.code.push(I::BrIf(depth));
    }

    /// The finished function. A body that can reach its end without a
    /// `ret` must leave its result on the stack (or have none).
    pub fn finish(mut self) -> Function {
        assert!(self.blocks.is_empty());
        self.code.push(I::End);
        let locals: Vec<ValType> = self.locals[self.params..].iter().map(|t| t.val()).collect();
        let mut f = crate::compiler::runtime::function(&locals);
        for ins in &self.code {
            f.instruction(ins);
        }
        f
    }
}
