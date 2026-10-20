//! CPython's character classes and case mappings at run time: the tables of
//! `core::unicode_tables` laid out in memory, and the runtime helpers (see
//! `compiler::runtime`) that read them.
//!
//! The tables sit at the start of the heap, each at a fixed address, and the
//! allocator starts past the last one a program uses; a program that calls
//! none of these helpers carries none of the tables. The helpers are ports of
//! the same CPython routines `core::unicode` ports, so a method folded on a
//! literal and the same method at run time give one answer.

use crate::compiler::context::{CompilationContext, COLLECTION_HEAP_BASE};
use crate::compiler::runtime::{alloc_string, byte, function, w32, Helper};
use crate::core::unicode::{CaseMap, Class, Predicate};
use std::sync::OnceLock;
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

/// A table in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Table {
    /// `(start, end)` pairs, 8 bytes each.
    Class(Class),
    /// `(start, end, step, delta)`, 16 bytes each.
    Runs(CaseMap),
    /// `(code point, count, c0, c1, c2)`, 20 bytes each.
    Multi(CaseMap),
}

impl Table {
    fn bytes(self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut word = |w: u32| out.extend_from_slice(&w.to_le_bytes());
        match self {
            Table::Class(class) => {
                for &(start, end) in class.ranges() {
                    word(start);
                    word(end);
                }
            }
            Table::Runs(map) => {
                for &(start, end, step, delta) in map.runs() {
                    word(start);
                    word(end);
                    word(step);
                    word(delta as u32);
                }
            }
            Table::Multi(map) => {
                for &(cp, to, n) in map.multi() {
                    word(cp);
                    word(n);
                    for c in to {
                        word(c);
                    }
                }
            }
        }
        out
    }

    fn count(self) -> i32 {
        (match self {
            Table::Class(class) => class.ranges().len(),
            Table::Runs(map) => map.runs().len(),
            Table::Multi(map) => map.multi().len(),
        }) as i32
    }

    fn all() -> Vec<Table> {
        let mut all: Vec<Table> = Class::ALL.iter().map(|&c| Table::Class(c)).collect();
        for map in CaseMap::ALL {
            all.push(Table::Runs(map));
            all.push(Table::Multi(map));
        }
        all
    }

    /// The table's address and byte length.
    fn place(self) -> (u32, u32) {
        static LAYOUT: OnceLock<Vec<(Table, u32, u32)>> = OnceLock::new();
        let layout = LAYOUT.get_or_init(|| {
            let mut at = COLLECTION_HEAP_BASE;
            Table::all()
                .into_iter()
                .map(|table| {
                    let len = table.bytes().len() as u32;
                    let placed = (table, at, len);
                    at += (len + 7) & !7;
                    placed
                })
                .collect()
        });
        let &(_, at, len) = layout
            .iter()
            .find(|(table, _, _)| *table == self)
            .expect("every table is laid out");
        (at, len)
    }

    fn address(self) -> i32 {
        self.place().0 as i32
    }
}

/// The tables `helper` reads.
fn tables_of(helper: &Helper) -> Vec<Table> {
    match helper {
        Helper::UniHas(class) => vec![Table::Class(*class)],
        Helper::UniMap(map) => vec![Table::Runs(*map), Table::Multi(*map)],
        _ => vec![],
    }
}

/// The data segments the helpers a module uses need, as `(address, bytes)`,
/// and where the heap starts past them.
pub(crate) fn data_for(helpers: &[Helper]) -> (Vec<(u32, Vec<u8>)>, u32) {
    let mut used: Vec<Table> = helpers.iter().flat_map(tables_of).collect();
    used.sort_by_key(|t| t.place().0);
    used.dedup();
    let mut heap = COLLECTION_HEAP_BASE;
    let segments = used
        .into_iter()
        .map(|table| {
            let (at, len) = table.place();
            heap = heap.max((at + len + 7) & !7);
            (at, table.bytes())
        })
        .collect();
    (segments, heap)
}

/// A case transform of a whole string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Case {
    Upper,
    Lower,
    Capitalize,
    Title,
}

fn incr(f: &mut Function, local: u32, by: i32) {
    emit!(
        f,
        I::LocalGet(local),
        I::I32Const(by),
        I::I32Add,
        I::LocalSet(local)
    );
}

/// Decode the UTF-8 character at the address in `addr` into `cp`, its byte
/// width into `w`.
fn decode(f: &mut Function, addr: u32, cp: u32, w: u32) {
    let cont = |f: &mut Function, k: u64| {
        emit!(
            f,
            I::LocalGet(addr),
            I::I32Load8U(crate::compiler::runtime::mem(k, 0)),
            I::I32Const(0x3f),
            I::I32And
        );
    };
    emit!(
        f,
        I::LocalGet(addr),
        I::I32Load8U(byte()),
        I::LocalTee(cp),
        I::I32Const(0x80),
        I::I32LtU
    );
    emit!(
        f,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::LocalSet(w),
        I::Else
    );
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0xe0),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x1f),
        I::I32And,
        I::I32Const(6),
        I::I32Shl
    );
    cont(f, 1);
    emit!(
        f,
        I::I32Or,
        I::LocalSet(cp),
        I::I32Const(2),
        I::LocalSet(w),
        I::Else
    );
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0xf0),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x0f),
        I::I32And,
        I::I32Const(12),
        I::I32Shl
    );
    cont(f, 1);
    emit!(f, I::I32Const(6), I::I32Shl, I::I32Or);
    cont(f, 2);
    emit!(
        f,
        I::I32Or,
        I::LocalSet(cp),
        I::I32Const(3),
        I::LocalSet(w),
        I::Else
    );
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x07),
        I::I32And,
        I::I32Const(18),
        I::I32Shl
    );
    cont(f, 1);
    emit!(f, I::I32Const(12), I::I32Shl, I::I32Or);
    cont(f, 2);
    emit!(f, I::I32Const(6), I::I32Shl, I::I32Or);
    cont(f, 3);
    emit!(f, I::I32Or, I::LocalSet(cp), I::I32Const(4), I::LocalSet(w));
    emit!(f, I::End, I::End, I::End);
}

/// Push the UTF-8 width of `cp`, writing its bytes at the address in `out`
/// unless that is 0.
fn encode(f: &mut Function, out: u32, cp: u32) {
    let piece = |f: &mut Function, k: u64, shift: i32, mask: i32, lead: i32| {
        emit!(
            f,
            I::LocalGet(out),
            I::LocalGet(cp),
            I::I32Const(shift),
            I::I32ShrU,
            I::I32Const(mask),
            I::I32And,
            I::I32Const(lead),
            I::I32Or,
            I::I32Store8(crate::compiler::runtime::mem(k, 0))
        );
    };
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x80),
        I::I32LtU,
        I::If(BlockType::Result(ValType::I32))
    );
    emit!(f, I::LocalGet(out), I::If(BlockType::Empty));
    piece(f, 0, 0, 0x7f, 0);
    emit!(f, I::End, I::I32Const(1), I::Else);
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x800),
        I::I32LtU,
        I::If(BlockType::Result(ValType::I32))
    );
    emit!(f, I::LocalGet(out), I::If(BlockType::Empty));
    piece(f, 0, 6, 0x1f, 0xc0);
    piece(f, 1, 0, 0x3f, 0x80);
    emit!(f, I::End, I::I32Const(2), I::Else);
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x10000),
        I::I32LtU,
        I::If(BlockType::Result(ValType::I32))
    );
    emit!(f, I::LocalGet(out), I::If(BlockType::Empty));
    piece(f, 0, 12, 0x0f, 0xe0);
    piece(f, 1, 6, 0x3f, 0x80);
    piece(f, 2, 0, 0x3f, 0x80);
    emit!(f, I::End, I::I32Const(3), I::Else);
    emit!(f, I::LocalGet(out), I::If(BlockType::Empty));
    piece(f, 0, 18, 0x07, 0xf0);
    piece(f, 1, 12, 0x3f, 0x80);
    piece(f, 2, 6, 0x3f, 0x80);
    piece(f, 3, 0, 0x3f, 0x80);
    emit!(f, I::End, I::I32Const(4), I::End, I::End, I::End);
}

/// Binary search for the first entry (of `stride` bytes at `base`, `count` of
/// them) whose word at `key` is not below the local `cp`, into `lo`.
#[allow(clippy::too_many_arguments)]
fn lower_bound(
    f: &mut Function,
    base: i32,
    count: i32,
    stride: i32,
    key: u64,
    cp: u32,
    lo: u32,
    hi: u32,
    mid: u32,
) {
    emit!(
        f,
        I::I32Const(0),
        I::LocalSet(lo),
        I::I32Const(count),
        I::LocalSet(hi)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(lo),
        I::LocalGet(hi),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(lo),
        I::LocalGet(hi),
        I::I32Add,
        I::I32Const(1),
        I::I32ShrU,
        I::LocalTee(mid),
        I::I32Const(stride),
        I::I32Mul,
        I::I32Load(w32(base as u64 + key)),
        I::LocalGet(cp),
        I::I32LtU,
        I::If(BlockType::Empty),
        I::LocalGet(mid),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(lo),
        I::Else,
        I::LocalGet(mid),
        I::LocalSet(hi),
        I::End,
        I::Br(0),
        I::End,
        I::End
    );
}

/// `(cp) -> bool`: whether the code point is in the class.
pub(crate) fn build_has(class: Class) -> Function {
    let table = Table::Class(class);
    let mut f = function(&[ValType::I32; 3]);
    let (cp, lo, hi, mid) = (0, 1, 2, 3);
    lower_bound(
        &mut f,
        table.address(),
        table.count(),
        8,
        4,
        cp,
        lo,
        hi,
        mid,
    );
    emit!(
        f,
        I::LocalGet(lo),
        I::I32Const(table.count()),
        I::I32LtU,
        I::If(BlockType::Result(ValType::I32)),
        I::LocalGet(lo),
        I::I32Const(8),
        I::I32Mul,
        I::I32Load(w32(table.address() as u64)),
        I::LocalGet(cp),
        I::I32LeU,
        I::Else,
        I::I32Const(0),
        I::End,
        I::End
    );
    f
}

/// `(cp, out) -> width`: the case mapping of a code point, its UTF-8 written
/// at `out` (unless 0), pushing the bytes it takes.
pub(crate) fn build_map(map: CaseMap) -> Function {
    let runs = Table::Runs(map);
    let multi = Table::Multi(map);
    let mut f = function(&[ValType::I32; 6]);
    let (cp, out, lo, hi, mid, n, k, total) = (0, 1, 2, 3, 4, 5, 6, 7);
    // ASCII: the letters of the other case, everything else itself.
    let (from, to) = match map {
        CaseMap::Lower => (b'A', b'Z'),
        _ => (b'a', b'z'),
    };
    let delta = if map == CaseMap::Lower { 32 } else { -32 };
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x80),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(from as i32),
        I::I32Sub,
        I::I32Const((to - from) as i32),
        I::I32LeU,
        I::If(BlockType::Empty),
        I::LocalGet(cp),
        I::I32Const(delta),
        I::I32Add,
        I::LocalSet(cp),
        I::End,
        I::LocalGet(out),
        I::If(BlockType::Empty),
        I::LocalGet(out),
        I::LocalGet(cp),
        I::I32Store8(byte()),
        I::End,
        I::I32Const(1),
        I::Return,
        I::End
    );
    // A mapping to several characters.
    if multi.count() > 0 {
        lower_bound(
            &mut f,
            multi.address(),
            multi.count(),
            20,
            0,
            cp,
            lo,
            hi,
            mid,
        );
        emit!(
            f,
            I::LocalGet(lo),
            I::I32Const(multi.count()),
            I::I32LtU,
            I::If(BlockType::Empty),
            I::LocalGet(lo),
            I::I32Const(20),
            I::I32Mul,
            I::I32Const(multi.address()),
            I::I32Add,
            I::LocalTee(mid),
            I::I32Load(w32(0)),
            I::LocalGet(cp),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::LocalGet(mid),
            I::I32Load(w32(4)),
            I::LocalSet(n),
            I::I32Const(0),
            I::LocalSet(total),
            I::I32Const(0),
            I::LocalSet(k)
        );
        emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
        emit!(
            f,
            I::LocalGet(k),
            I::LocalGet(n),
            I::I32GeU,
            I::BrIf(1),
            I::LocalGet(mid),
            I::LocalGet(k),
            I::I32Const(4),
            I::I32Mul,
            I::I32Add,
            I::I32Load(w32(8)),
            I::LocalSet(cp),
            // out = out ? out + total : 0, kept in `hi`
            I::LocalGet(out),
            I::LocalGet(total),
            I::I32Add,
            I::I32Const(0),
            I::LocalGet(out),
            I::Select,
            I::LocalSet(hi)
        );
        encode(&mut f, hi, cp);
        emit!(f, I::LocalGet(total), I::I32Add, I::LocalSet(total));
        incr(&mut f, k, 1);
        emit!(
            f,
            I::Br(0),
            I::End,
            I::End,
            I::LocalGet(total),
            I::Return,
            I::End,
            I::End
        );
    }
    // A run of single characters with one delta.
    lower_bound(&mut f, runs.address(), runs.count(), 16, 4, cp, lo, hi, mid);
    emit!(
        f,
        I::LocalGet(lo),
        I::I32Const(runs.count()),
        I::I32LtU,
        I::If(BlockType::Empty),
        I::LocalGet(lo),
        I::I32Const(16),
        I::I32Mul,
        I::I32Const(runs.address()),
        I::I32Add,
        I::LocalTee(mid),
        I::I32Load(w32(0)),
        I::LocalGet(cp),
        I::I32LeU,
        I::LocalGet(cp),
        I::LocalGet(mid),
        I::I32Load(w32(0)),
        I::I32Sub,
        I::LocalGet(mid),
        I::I32Load(w32(8)),
        I::I32RemU,
        I::I32Eqz,
        I::I32And,
        I::If(BlockType::Empty),
        I::LocalGet(cp),
        I::LocalGet(mid),
        I::I32Load(w32(12)),
        I::I32Add,
        I::LocalSet(cp),
        I::End,
        I::End
    );
    encode(&mut f, out, cp);
    emit!(f, I::End);
    f
}

/// `(s, n, i, w) -> cp`: CPython's `handle_capital_sigma` for the capital
/// sigma of width `w` at byte `i` of the `n` bytes at `s`.
pub(crate) fn build_final_sigma(ctx: &CompilationContext) -> Function {
    let ignorable = ctx.helper(Helper::UniHas(Class::CaseIgnorable));
    let cased = ctx.helper(Helper::UniHas(Class::Cased));
    let mut f = function(&[ValType::I32; 5]);
    let (s, n, i, w) = (0, 1, 2, 3);
    let (j, c, cw, is_final, addr) = (4, 5, 6, 7, 8);
    // Back past case-ignorable characters to a cased one.
    emit!(f, I::LocalGet(i), I::LocalSet(j));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(j), I::I32Eqz, I::BrIf(1));
    incr(&mut f, j, -1);
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(j),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(s),
        I::LocalGet(j),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(0xc0),
        I::I32And,
        I::I32Const(0x80),
        I::I32Ne,
        I::BrIf(1)
    );
    incr(&mut f, j, -1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(j),
        I::I32Add,
        I::LocalSet(addr)
    );
    decode(&mut f, addr, c, cw);
    emit!(
        f,
        I::LocalGet(c),
        I::Call(ignorable),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(c),
        I::Call(cased),
        I::LocalSet(is_final),
        I::Br(2),
        I::End,
        I::Br(0),
        I::End,
        I::End
    );
    // Then forward past case-ignorable ones: no cased character may follow.
    emit!(f, I::LocalGet(is_final), I::If(BlockType::Empty));
    emit!(f, I::LocalGet(i), I::LocalGet(w), I::I32Add, I::LocalSet(j));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(j), I::LocalGet(n), I::I32GeU, I::BrIf(1));
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(j),
        I::I32Add,
        I::LocalSet(addr)
    );
    decode(&mut f, addr, c, cw);
    emit!(
        f,
        I::LocalGet(c),
        I::Call(ignorable),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(c),
        I::Call(cased),
        I::I32Eqz,
        I::LocalSet(is_final),
        I::Br(2),
        I::End,
        I::LocalGet(j),
        I::LocalGet(cw),
        I::I32Add,
        I::LocalSet(j),
        I::Br(0),
        I::End,
        I::End,
        I::End
    );
    emit!(
        f,
        I::I32Const(0x3c2),
        I::I32Const(0x3c3),
        I::LocalGet(is_final),
        I::Select,
        I::End
    );
    f
}

/// `(s, n, out) -> bytes`: the transform of the `n` bytes at `s`, written at
/// `out` unless 0, pushing the bytes it takes.
pub(crate) fn build_case_run(ctx: &CompilationContext, case: Case) -> Function {
    // Only the helpers (and so the tables) this transform reads.
    let map = |m| ctx.helper(Helper::UniMap(m));
    let mut f = function(&[ValType::I32; 7]);
    let (s, n, out) = (0, 1, 2);
    let (i, total, cp, w, dst, prev_cased, addr) = (3, 4, 5, 6, 7, 8, 9);
    // The lowercase of the character, a capital sigma by its context.
    let lower_here = |f: &mut Function| {
        let sigma = ctx.helper(Helper::FinalSigma);
        let lower = map(CaseMap::Lower);
        emit!(
            f,
            I::LocalGet(cp),
            I::I32Const(0x3a3),
            I::I32Eq,
            I::If(BlockType::Result(ValType::I32)),
            I::LocalGet(s),
            I::LocalGet(n),
            I::LocalGet(i),
            I::LocalGet(w),
            I::Call(sigma),
            I::LocalSet(addr)
        );
        encode(f, dst, addr);
        emit!(
            f,
            I::Else,
            I::LocalGet(cp),
            I::LocalGet(dst),
            I::Call(lower),
            I::End
        );
    };
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(n),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::LocalSet(addr)
    );
    decode(&mut f, addr, cp, w);
    emit!(
        f,
        I::LocalGet(out),
        I::LocalGet(total),
        I::I32Add,
        I::I32Const(0),
        I::LocalGet(out),
        I::Select,
        I::LocalSet(dst)
    );
    match case {
        Case::Upper => emit!(
            f,
            I::LocalGet(cp),
            I::LocalGet(dst),
            I::Call(map(CaseMap::Upper))
        ),
        Case::Lower => lower_here(&mut f),
        Case::Capitalize => {
            emit!(
                f,
                I::LocalGet(i),
                I::I32Eqz,
                I::If(BlockType::Result(ValType::I32)),
                I::LocalGet(cp),
                I::LocalGet(dst),
                I::Call(map(CaseMap::Title)),
                I::Else
            );
            lower_here(&mut f);
            emit!(f, I::End);
        }
        Case::Title => {
            emit!(
                f,
                I::LocalGet(prev_cased),
                I::If(BlockType::Result(ValType::I32))
            );
            lower_here(&mut f);
            let (title, cased) = (
                map(CaseMap::Title),
                ctx.helper(Helper::UniHas(Class::Cased)),
            );
            emit!(
                f,
                I::Else,
                I::LocalGet(cp),
                I::LocalGet(dst),
                I::Call(title),
                I::End
            );
            emit!(f, I::LocalGet(cp), I::Call(cased), I::LocalSet(prev_cased));
        }
    }
    emit!(f, I::LocalGet(total), I::I32Add, I::LocalSet(total));
    emit!(f, I::LocalGet(i), I::LocalGet(w), I::I32Add, I::LocalSet(i));
    emit!(f, I::Br(0), I::End, I::End, I::LocalGet(total), I::End);
    f
}

/// `(s) -> str`: the transform of a string, measured, then written into a
/// string of exactly that length.
pub(crate) fn build_case(ctx: &CompilationContext, case: Case) -> Function {
    let run = ctx.helper(Helper::CaseRun(case));
    let mut f = function(&[ValType::I32; 3]);
    let (s, n, len, out) = (0, 1, 2, 3);
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalSet(n),
        I::LocalGet(s),
        I::LocalGet(n),
        I::I32Const(0),
        I::Call(run),
        I::LocalSet(len)
    );
    alloc_string(&mut f, ctx, len, out);
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(n),
        I::LocalGet(out),
        I::Call(run),
        I::Drop,
        I::LocalGet(out),
        I::End
    );
    f
}

/// `(s) -> bool`: a string predicate, as CPython evaluates it.
pub(crate) fn build_str_is(ctx: &CompilationContext, predicate: Predicate) -> Function {
    let has = |class| ctx.helper(Helper::UniHas(class));
    let mut f = function(&[ValType::I32; 6]);
    let s = 0;
    let (n, i, cp, w, cased, addr) = (1, 2, 3, 4, 5, 6);
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalTee(n),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::Return,
        I::End
    );
    let step = |f: &mut Function| {
        emit!(
            f,
            I::LocalGet(s),
            I::LocalGet(i),
            I::I32Add,
            I::LocalSet(addr)
        );
        decode(f, addr, cp, w);
    };
    let open_loop = |f: &mut Function| {
        emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
        emit!(f, I::LocalGet(i), I::LocalGet(n), I::I32GeU, I::BrIf(1));
    };
    let close_loop = |f: &mut Function| {
        emit!(f, I::LocalGet(i), I::LocalGet(w), I::I32Add, I::LocalSet(i));
        emit!(f, I::Br(0), I::End, I::End);
    };
    if let Predicate::Upper | Predicate::Lower = predicate {
        let (this, other) = if predicate == Predicate::Upper {
            (has(Class::Upper), has(Class::Lower))
        } else {
            (has(Class::Lower), has(Class::Upper))
        };
        let title = has(Class::Title);
        // One character is its class alone.
        step(&mut f);
        emit!(
            f,
            I::LocalGet(w),
            I::LocalGet(n),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::LocalGet(cp),
            I::Call(this),
            I::Return,
            I::End
        );
        // No character of the other case or titlecase, and one of this case.
        open_loop(&mut f);
        step(&mut f);
        emit!(
            f,
            I::LocalGet(cp),
            I::Call(other),
            I::LocalGet(cp),
            I::Call(title),
            I::I32Or,
            I::If(BlockType::Empty),
            I::I32Const(0),
            I::Return,
            I::End,
            I::LocalGet(cp),
            I::Call(this),
            I::LocalGet(cased),
            I::I32Or,
            I::LocalSet(cased)
        );
        close_loop(&mut f);
        emit!(f, I::LocalGet(cased), I::End);
        return f;
    }
    // Every character in the class.
    open_loop(&mut f);
    step(&mut f);
    match predicate {
        Predicate::Alnum => {
            emit!(f, I::LocalGet(cp), I::Call(has(Class::Alpha)));
            for class in [Class::Decimal, Class::Digit, Class::Numeric] {
                emit!(f, I::LocalGet(cp), I::Call(has(class)), I::I32Or);
            }
        }
        other => {
            let class = match other {
                Predicate::Alpha => Class::Alpha,
                Predicate::Decimal => Class::Decimal,
                Predicate::Digit => Class::Digit,
                Predicate::Numeric => Class::Numeric,
                _ => Class::Space,
            };
            emit!(f, I::LocalGet(cp), I::Call(has(class)));
        }
    }
    emit!(
        f,
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::Return,
        I::End
    );
    close_loop(&mut f);
    emit!(f, I::I32Const(1), I::End);
    f
}

/// `(s, i, set, set_n) -> width`: the width of the character at byte `i` of
/// `s` (or, `before`, the one ending there) when it is whitespace (no `set`)
/// or one of the characters of the `set_n` bytes at `set`, else 0. A
/// continuation byte starts no character.
pub(crate) fn build_trim_at(ctx: &CompilationContext, with_set: bool, before: bool) -> Function {
    let space = ctx.helper(Helper::UniHas(Class::Space));
    let mut f = function(&[ValType::I32; 7]);
    let (s, i, set, set_n) = (0, 1, 2, 3);
    let (cp, w, k, c, cw, addr, j) = (4, 5, 6, 7, 8, 9, 10);
    if before {
        // Back over continuation bytes to the character's start.
        emit!(f, I::LocalGet(i), I::LocalSet(k));
        emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
        incr(&mut f, k, -1);
        emit!(
            f,
            I::LocalGet(k),
            I::I32Eqz,
            I::BrIf(1),
            I::LocalGet(s),
            I::LocalGet(k),
            I::I32Add,
            I::I32Load8U(byte()),
            I::I32Const(0xc0),
            I::I32And,
            I::I32Const(0x80),
            I::I32Ne,
            I::BrIf(1),
            I::Br(0),
            I::End,
            I::End,
            I::LocalGet(s),
            I::LocalGet(k),
            I::I32Add,
            I::LocalSet(addr)
        );
    } else {
        emit!(
            f,
            I::LocalGet(s),
            I::LocalGet(i),
            I::I32Add,
            I::LocalTee(addr),
            I::I32Load8U(byte()),
            I::I32Const(0xc0),
            I::I32And,
            I::I32Const(0x80),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::I32Const(0),
            I::Return,
            I::End
        );
    }
    decode(&mut f, addr, cp, w);
    let width = |f: &mut Function| {
        if before {
            emit!(f, I::LocalGet(i), I::LocalGet(k), I::I32Sub);
        } else {
            emit!(f, I::LocalGet(w));
        }
    };
    if !with_set {
        emit!(
            f,
            I::LocalGet(cp),
            I::Call(space),
            I::If(BlockType::Result(ValType::I32))
        );
        width(&mut f);
        emit!(f, I::Else, I::I32Const(0), I::End, I::End);
        return f;
    }
    // The set, a character at a time.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(j),
        I::LocalGet(set_n),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(set),
        I::LocalGet(j),
        I::I32Add,
        I::LocalSet(addr)
    );
    decode(&mut f, addr, c, cw);
    emit!(
        f,
        I::LocalGet(c),
        I::LocalGet(cp),
        I::I32Eq,
        I::If(BlockType::Empty)
    );
    width(&mut f);
    emit!(f, I::Return, I::End);
    emit!(
        f,
        I::LocalGet(j),
        I::LocalGet(cw),
        I::I32Add,
        I::LocalSet(j)
    );
    emit!(f, I::Br(0), I::End, I::End, I::I32Const(0), I::End);
    f
}

/// Where the code point cache sits: after the formatters' text buffer, below
/// the heap. Each of its entries is `[string][code points][char][byte]`: a
/// string's length in code points, and the last code point index looked up
/// in it with its byte offset. A string never changes and its memory is never
/// reused, so a pointer names one string for good, and an entry stays true.
const STR_CACHE: i32 = crate::compiler::runtime::TEXT + crate::compiler::runtime::TEXT_BYTES;
const STR_CACHE_ENTRIES: i32 = 256;
const _: () = assert!(STR_CACHE + STR_CACHE_ENTRIES * 16 <= COLLECTION_HEAP_BASE as i32);

/// Push the address of the cache entry for the string in `s`.
fn cache_entry(f: &mut Function, s: u32) {
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(-1640531535), // 2654435761, Knuth's multiplier
        I::I32Mul,
        I::I32Const(24),
        I::I32ShrU,
        I::I32Const(STR_CACHE_ENTRIES - 1),
        I::I32And,
        I::I32Const(16),
        I::I32Mul,
        I::I32Const(STR_CACHE),
        I::I32Add
    );
}

/// Push the byte width of the UTF-8 character whose lead byte is at
/// `base + at` (locals).
pub(crate) fn emit_width_at(f: &mut Function, base: u32, at: u32, lead: u32) {
    emit!(
        f,
        I::LocalGet(base),
        I::LocalGet(at),
        I::I32Add,
        I::I32Load8U(byte()),
        I::LocalSet(lead),
        I::I32Const(1),
        I::I32Const(2),
        I::I32Const(3),
        I::I32Const(4),
        I::LocalGet(lead),
        I::I32Const(0xf0),
        I::I32LtU,
        I::Select,
        I::LocalGet(lead),
        I::I32Const(0xe0),
        I::I32LtU,
        I::Select,
        I::LocalGet(lead),
        I::I32Const(0x80),
        I::I32LtU,
        I::Select
    );
}

/// `(s) -> n`: the string's length in code points.
pub(crate) fn build_char_len() -> Function {
    let mut f = function(&[ValType::I32; 4]);
    let s = 0;
    let (e, n, i, count) = (1, 2, 3, 4);
    cache_entry(&mut f, s);
    emit!(
        f,
        I::LocalTee(e),
        I::I32Load(w32(0)),
        I::LocalGet(s),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::LocalGet(e),
        I::I32Load(w32(4)),
        I::Return,
        I::End,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalSet(n)
    );
    // Every byte but a continuation byte starts a code point.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(n),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(count),
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(0xc0),
        I::I32And,
        I::I32Const(0x80),
        I::I32Ne,
        I::I32Add,
        I::LocalSet(count)
    );
    incr(&mut f, i, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::LocalGet(e),
        I::LocalGet(s),
        I::I32Store(w32(0)),
        I::LocalGet(e),
        I::LocalGet(count),
        I::I32Store(w32(4)),
        I::LocalGet(e),
        I::I32Const(0),
        I::I32Store(w32(8)),
        I::LocalGet(e),
        I::I32Const(0),
        I::I32Store(w32(12)),
        I::LocalGet(count),
        I::End
    );
    f
}

/// `(s, i) -> byte`: the byte offset of code point `i` (at most the length)
/// of the string. Constant time for ASCII, and walking from the last index
/// looked up otherwise, so a loop over a string's indices stays linear.
pub(crate) fn build_char_offset(ctx: &CompilationContext) -> Function {
    let char_len = ctx.helper(Helper::CharLen);
    let mut f = function(&[ValType::I32; 4]);
    let (s, i) = (0, 1);
    let (e, k, b, lead) = (2, 3, 4, 5);
    // ASCII: the index is the offset.
    emit!(
        f,
        I::LocalGet(s),
        I::Call(char_len),
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::LocalGet(i),
        I::Return,
        I::End
    );
    cache_entry(&mut f, s);
    emit!(
        f,
        I::LocalTee(e),
        I::I32Load(w32(8)),
        I::LocalSet(k),
        I::LocalGet(e),
        I::I32Load(w32(12)),
        I::LocalSet(b),
        // Nearer the start than the last index: from the start.
        I::LocalGet(i),
        I::LocalGet(k),
        I::LocalGet(i),
        I::I32Sub,
        I::I32LtS,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalSet(k),
        I::I32Const(0),
        I::LocalSet(b),
        I::End
    );
    // Forward a code point at a time.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(k), I::LocalGet(i), I::I32GeS, I::BrIf(1));
    emit!(f, I::LocalGet(b));
    emit_width_at(&mut f, s, b, lead);
    emit!(f, I::I32Add, I::LocalSet(b));
    incr(&mut f, k, 1);
    emit!(f, I::Br(0), I::End, I::End);
    // Or back.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(k), I::LocalGet(i), I::I32LeS, I::BrIf(1));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    incr(&mut f, b, -1);
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(b),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(0xc0),
        I::I32And,
        I::I32Const(0x80),
        I::I32Eq,
        I::BrIf(0),
        I::End,
        I::End
    );
    incr(&mut f, k, -1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::LocalGet(e),
        I::LocalGet(i),
        I::I32Store(w32(8)),
        I::LocalGet(e),
        I::LocalGet(b),
        I::I32Store(w32(12)),
        I::LocalGet(b),
        I::End
    );
    f
}

/// `(s, byte) -> i`: the code point index of a byte offset that starts one
/// (or is the length).
pub(crate) fn build_char_index(ctx: &CompilationContext) -> Function {
    let char_len = ctx.helper(Helper::CharLen);
    let mut f = function(&[ValType::I32; 3]);
    let (s, at) = (0, 1);
    let (i, count, n) = (2, 3, 4);
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalTee(n),
        I::LocalGet(s),
        I::Call(char_len),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::LocalGet(at),
        I::Return,
        I::End
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(at),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(count),
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(0xc0),
        I::I32And,
        I::I32Const(0x80),
        I::I32Ne,
        I::I32Add,
        I::LocalSet(count)
    );
    incr(&mut f, i, 1);
    emit!(f, I::Br(0), I::End, I::End, I::LocalGet(count), I::End);
    f
}

/// `(s) -> list`: the list of the string's characters, each a new
/// one-character string, which is what a walk over a str visits.
pub(crate) fn build_str_chars(ctx: &CompilationContext) -> Function {
    use crate::compiler::context::{
        COLLECTION_CAP, COLLECTION_DATA, COLLECTION_HEADER, COLLECTION_SLOT,
    };
    let char_len = ctx.helper(Helper::CharLen);
    let mut f = function(&[ValType::I32; 7]);
    let s = 0;
    let (count, list, i, b, w, lead, piece) = (1, 2, 3, 4, 5, 6, 7);
    emit!(
        f,
        I::LocalGet(s),
        I::Call(char_len),
        I::LocalTee(count),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::I32Const(COLLECTION_HEADER as i32),
        I::I32Add,
        I::Call(ctx.alloc_func_index),
        I::LocalTee(list),
        I::LocalGet(count),
        I::I32Store(w32(0)),
        I::LocalGet(list),
        I::LocalGet(count),
        I::I32Store(w32(COLLECTION_CAP as u64)),
        I::LocalGet(list),
        I::LocalGet(list),
        I::I32Const(COLLECTION_HEADER as i32),
        I::I32Add,
        I::I32Store(w32(COLLECTION_DATA as u64))
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(i), I::LocalGet(count), I::I32GeU, I::BrIf(1));
    emit_width_at(&mut f, s, b, lead);
    emit!(f, I::LocalSet(w));
    alloc_string(&mut f, ctx, w, piece);
    emit!(
        f,
        I::LocalGet(piece),
        I::LocalGet(s),
        I::LocalGet(b),
        I::I32Add,
        I::LocalGet(w),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(list),
        I::LocalGet(i),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::I32Add,
        I::LocalGet(piece),
        I::I32Store(w32(COLLECTION_HEADER as u64)),
        I::LocalGet(b),
        I::LocalGet(w),
        I::I32Add,
        I::LocalSet(b)
    );
    incr(&mut f, i, 1);
    emit!(f, I::Br(0), I::End, I::End, I::LocalGet(list), I::End);
    f
}
