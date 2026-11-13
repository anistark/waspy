//! Memory reclamation: the allocator, the root stack, and the collector.
//!
//! # The model
//!
//! Memory is reclaimed by a conservative mark-and-sweep collector. Nothing is
//! counted and no store carries a barrier: an object is kept for as long as a
//! word that points into it can be reached from a root, and freed by the first
//! collection after that stops being true. A word is only ever treated as a
//! possible pointer, never followed blindly, so a number that happens to look
//! like an address keeps a block alive but cannot corrupt one.
//!
//! # The heap
//!
//! Every block starts with an 8-byte header: its size (a multiple of 8, the
//! header included) with flag bits in the low three bits, and a second word
//! that is the next free block while the block is free and the collector's
//! own while it is allocated. Blocks tile the heap from `HEAP_BASE` to
//! `HEAP_TOP` with no gaps, so the heap can be walked header to header.
//!
//! `__alloc` takes a block from the free list of its exact size (the pools:
//! one list per size up to [`BIN_MAX`]), else splits the first large free
//! block that fits, else extends the heap. Every block is zeroed as it is
//! handed out, which is what code that builds an object in place relies on.
//!
//! # Roots
//!
//! A reference held only in a WASM local or on the operand stack is invisible
//! to anything that reads memory, so each one is mirrored on the root stack,
//! a growable array of words:
//!
//! - A function's named locals that can hold a reference each have a slot in
//!   its frame, written on every store (`compiler::emit` does this for every
//!   instruction that stores to such a local).
//! - Every block `__alloc` hands out, and every reference an expression reads
//!   out of the heap, is pushed above the frame's slots. They stay until the
//!   statement that made them ends, which is when no expression of this
//!   function can still be holding one.
//! - A function returning a reference pushes it for its caller as it leaves.
//!
//! Module globals, the pending exception, and the few words of static state
//! that hold objects are the other roots.
//!
//! # When it runs
//!
//! Only at a safepoint: the end of a statement that called into the runtime,
//! and the end of a comprehension's iteration. Between safepoints code is free
//! to keep raw pointers in locals, which is what every runtime sequence does;
//! a function further up the stack is always paused at a call, with what it
//! holds on the root stack.
//!
//! # Finalizers
//!
//! An instance of a class with `__del__` is finalized when it becomes
//! unreachable. CPython finalizes an object when its reference count reaches
//! zero and leaves cycles to its collector, so the two are told apart here by
//! counting references among the unreachable blocks: one nothing else dead
//! refers to is finalized at once, in the order CPython's cascade would run,
//! and one held by a dead cycle waits for a full collection.

use crate::compiler::asm::{call, Asm, Ty, Var, E, X};
use crate::compiler::context::{CompilationContext, CALL_DEPTH_GLOBAL, EXC_TYPE_GLOBAL};
use crate::compiler::emit::Function;
use crate::compiler::exceptions::EXC_VALUE_GLOBAL;
use crate::compiler::runtime::Helper;
use crate::ir::IRType;
use wasm_encoder::{Instruction as I, ValType};

/// The end of the heap: every block lies below it.
pub const HEAP_TOP_GLOBAL: u32 = 0;
/// The StopIteration flag.
const STOP_GLOBAL: u32 = 1;
/// Where the heap starts.
pub const HEAP_BASE_GLOBAL: u32 = 5;
/// The root stack: where its words are, how many bytes of it are in use, and
/// how many it has room for. Everything but the base is an offset, so the
/// stack can move when it grows.
pub const RS_BASE_GLOBAL: u32 = 6;
pub const RS_TOP_GLOBAL: u32 = 7;
const RS_CAP_GLOBAL: u32 = 8;
/// Set when enough has been allocated for a collection to be worth it; the
/// next safepoint runs one.
pub const GC_PENDING_GLOBAL: u32 = 9;
/// Bytes allocated since the last collection, and the count that asks for
/// the next one.
const GC_ALLOCATED_GLOBAL: u32 = 10;
const GC_THRESHOLD_GLOBAL: u32 = 11;
/// The free blocks larger than [`BIN_MAX`], lowest address first after a
/// sweep.
const GC_LARGE_GLOBAL: u32 = 12;
/// Where the per-size free lists are.
const GC_BINS_GLOBAL: u32 = 13;
/// How many blocks are allocated.
const GC_BLOCKS_GLOBAL: u32 = 14;
/// Set while a collection or a finalizer runs, when another must not start.
const GC_ACTIVE_GLOBAL: u32 = 15;
/// How many finalizable objects have not been finalized yet.
pub const FIN_LIVE_GLOBAL: u32 = 16;
/// During a collection: the sorted addresses of the allocated blocks, how
/// many there are, the top of the mark stack, and the end of the queue of
/// objects to finalize.
const GC_INDEX_GLOBAL: u32 = 17;
const GC_COUNT_GLOBAL: u32 = 18;
const GC_STACK_GLOBAL: u32 = 19;
const GC_QUEUE_GLOBAL: u32 = 20;
/// Where on the root stack the arguments of the call being made start, or
/// [`NO_ARGS`]. A caller sets it just before a call and the function called
/// takes it as it starts: the references pushed while its arguments were
/// worked out are the callee's from then on, and go when it returns, which
/// is when CPython lets go of a call's arguments. Only a module with
/// finalizers uses it, since only the time a finalizer runs depends on it.
pub const ARGS_BASE_GLOBAL: u32 = 21;
pub const NO_ARGS: i32 = -1;
/// How many globals the runtime has, which is the index of the first one
/// holding a module definition.
pub const RUNTIME_GLOBALS: u32 = 22;

/// Bytes of header ahead of every block's contents.
const HDR: i32 = 8;
/// The smallest block: a header and one slot.
const MIN_BLOCK: i32 = 16;
/// The largest block size with a free list of its own.
const BIN_MAX: i32 = 256;
/// Bytes the free lists' heads take, one word per multiple of 8 up to
/// [`BIN_MAX`].
pub const BINS_BYTES: u32 = (BIN_MAX as u32 / 8 + 1) * 4;

/// Header flags, in the low bits of the size word.
const ALLOCATED: i32 = 1;
const MARKED: i32 = 2;
/// Reached by the finalizer analysis, so not visited again.
const PEELED: i32 = 4;
/// Flags in the second header word of an allocated block: it has a
/// finalizer, and the finalizer has run. The bits below them count the
/// references to the block from other unreachable blocks.
const FINALIZABLE: i32 = 1 << 31;
const FINALIZED: i32 = 1 << 30;
/// The block lets go of what it holds first word to last, as CPython's
/// instances, dicts, and sets do. One without the flag goes last to first,
/// as its lists and tuples do. Only the order of finalizers depends on it.
pub const FORWARD: i32 = 1 << 29;
/// The block holds no references (a string's or bytes' text), so marking and
/// the finalizer analysis never read its words as pointers: a collection
/// spends no time on it, and text that happens to spell an object's address
/// keeps nothing alive.
pub const NOSCAN: i32 = 1 << 28;
/// The block is a JavaScript value's proxy (`compiler::js`): its one word
/// is a handle in the JavaScript side's table, given back when the block is
/// freed.
pub const JSREF: i32 = 1 << 27;
const AUX_FLAGS: i32 = FINALIZABLE | FINALIZED | FORWARD | NOSCAN | JSREF;

/// What one JavaScript value a proxy holds counts for, towards the next
/// collection: memory on the JavaScript side, which only a collection here
/// gives back, so a program making many proxies collects often enough.
pub const JS_VALUE_CHARGE: i32 = 1024;

/// Count `bytes` more towards the next collection, as if allocated.
pub(crate) fn charge(a: &mut Asm, bytes: impl Into<E>) {
    a.set_global(GC_ALLOCATED_GLOBAL, g(GC_ALLOCATED_GLOBAL) + bytes.into());
    a.if_(g(GC_ALLOCATED_GLOBAL).ge_u(g(GC_THRESHOLD_GLOBAL)), |a| {
        a.set_global(GC_PENDING_GLOBAL, 1)
    });
}

/// Bytes allocated before the first collection, and the least between two.
/// A program that allocates less than this in all never collects.
const MIN_THRESHOLD: i32 = 256 * 1024;

/// A collection that finalizes only what reference counting would have
/// freed, run where a reference was just dropped.
pub const COLLECT_DROPPED: i32 = 0;
/// A collection that also finalizes unreachable cycles, as CPython's own
/// collector does when it runs.
pub const COLLECT_FULL: i32 = 1;

/// Whether the module is built to collect at every safepoint and to
/// overwrite what it frees, so that a reference the collector cannot see
/// reads garbage instead of surviving by luck. Set by the environment
/// variable `WASPY_GC_STRESS` when compiling, to run the test suite that way.
pub fn stress() -> bool {
    std::env::var_os("WASPY_GC_STRESS").is_some()
}

/// What a runtime global starts as: zero, but for where the heap begins,
/// where its free lists are, and how much to allocate before collecting.
pub fn initial_global(index: u32, heap_base: u32, bins_base: u32) -> i32 {
    match index {
        HEAP_BASE_GLOBAL => heap_base as i32,
        GC_BINS_GLOBAL => bins_base as i32,
        GC_THRESHOLD_GLOBAL if !stress() => MIN_THRESHOLD,
        ARGS_BASE_GLOBAL => NO_ARGS,
        _ => 0,
    }
}

/// The collector's runtime functions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GcFn {
    /// `(size) -> block`: a zeroed block, not rooted.
    AllocRaw,
    /// `(block)`: give a block back.
    Free,
    /// `(bytes)`: make the root stack hold at least this many.
    GrowRoots,
    /// `(word) -> word`: push a word on the root stack.
    Root,
    /// `(bytes) -> frame`: a zeroed frame of that size on the root stack.
    Enter,
    /// `(word) -> header`: the allocated block the word points into, or 0.
    Find,
    /// `(word)`: mark the block the word points into, if any.
    MarkWord,
    /// `(word) -> header`: the unmarked block the word points into, or 0.
    FindDead,
    /// `(from, to)`: mark what the words in a range point at.
    Scan,
    /// `(base)`: mark everything reachable from the mark stack above `base`.
    Drain,
    /// `(header, base)`: queue the finalizers reference counting would run
    /// on freeing this block.
    Peel,
    /// `(dropped_from, dropped_to, mode, keep)`: collect.
    Collect,
    /// `()`: a full collection.
    Safepoint,
    /// `(object)`: run an object's `__del__`.
    Finalize,
    /// `()`: finalize what is alive as the program ends.
    Exit,
}

pub fn signature(function: &GcFn) -> (Vec<ValType>, Vec<ValType>) {
    use ValType::I32;
    match function {
        GcFn::AllocRaw | GcFn::Root | GcFn::Enter | GcFn::Find | GcFn::FindDead => {
            (vec![I32], vec![I32])
        }
        GcFn::Free | GcFn::GrowRoots | GcFn::MarkWord | GcFn::Drain | GcFn::Finalize => {
            (vec![I32], vec![])
        }
        GcFn::Scan | GcFn::Peel => (vec![I32, I32], vec![]),
        GcFn::Collect => (vec![I32; 4], vec![]),
        GcFn::Safepoint | GcFn::Exit => (vec![], vec![]),
    }
}

pub fn build(function: &GcFn, ctx: &CompilationContext) -> Function {
    match function {
        GcFn::AllocRaw => build_alloc_raw(),
        GcFn::Free => build_free(),
        GcFn::GrowRoots => build_grow_roots(ctx),
        GcFn::Root => build_root(ctx),
        GcFn::Enter => build_enter(ctx),
        GcFn::Find => build_find(),
        GcFn::MarkWord => build_mark_word(ctx),
        GcFn::FindDead => build_find_dead(ctx),
        GcFn::Scan => build_scan(ctx),
        GcFn::Drain => build_drain(ctx),
        GcFn::Peel => build_peel(ctx),
        GcFn::Collect => build_collect(ctx),
        GcFn::Safepoint => build_safepoint(ctx),
        GcFn::Finalize => build_finalize(ctx),
        GcFn::Exit => build_exit(ctx),
    }
}

fn helper(ctx: &CompilationContext, function: GcFn) -> u32 {
    ctx.helper(Helper::Gc(function))
}

fn g(global: u32) -> E {
    E::Global(global)
}

fn word(addr: impl Into<E>, offset: u64) -> E {
    addr.into().load(Ty::I32, offset)
}

/// The size in a header word.
fn size_of(header: impl Into<E>) -> E {
    header.into() & -8
}

/// The address of the free list for blocks of `size` bytes: one word per
/// multiple of 8, so the size halved is the offset.
fn bin(size: Var) -> E {
    g(GC_BINS_GLOBAL) + (size >> 1)
}

/// Put the free block at `block`, of `size` bytes, on the list for its size.
fn push_bin(a: &mut Asm, block: Var, size: Var) {
    a.store(Ty::I32, block, 4, word(bin(size), 0));
    a.store(Ty::I32, bin(size), 0, block);
}

/// `__alloc(size) -> block`: a zeroed block the collector can see. It is on
/// the root stack until the statement that asked for it ends, which covers
/// the time it takes to store it somewhere that keeps it.
pub fn build_alloc(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let size = a.param(0);
    let raw = call(helper(ctx, GcFn::AllocRaw), vec![size.e()], Some(Ty::I32));
    a.push(call(helper(ctx, GcFn::Root), vec![raw], Some(Ty::I32)));
    a.finish()
}

/// A function standing in for one the module never calls.
pub fn build_unused(params: usize) -> Function {
    let mut a = Asm::new(&vec![Ty::I32; params]);
    a.raw(I::Unreachable);
    a.finish()
}

/// `__alloc_obj(size, class_id) -> instance`: an instance with its class id
/// in its first word, registered for finalization when its class has a
/// `__del__`.
pub fn build_alloc_obj(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32, Ty::I32]);
    let (size, class_id) = (a.param(0), a.param(1));
    let object = a.let_(call(ctx.alloc_func_index, vec![size.e()], Some(Ty::I32)));
    a.store(Ty::I32, object, 0, class_id);
    let finalizable = ctx.finalizable_class_ids();
    if !finalizable.is_empty() {
        // An instance lets go of its attributes in the order they were set.
        a.store(Ty::I32, object - 4, 0, FORWARD);
        let mut is_finalizable = E::I32(0);
        for id in finalizable {
            is_finalizable = is_finalizable | class_id.eq(id);
        }
        a.if_(is_finalizable, |a| {
            a.store(Ty::I32, object - 4, 0, FINALIZABLE | FORWARD);
            a.set_global(FIN_LIVE_GLOBAL, g(FIN_LIVE_GLOBAL) + 1);
        });
    }
    a.push(object);
    a.finish()
}

fn build_alloc_raw() -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let size = a.param(0);
    // More than any memory holds: CPython raises MemoryError, and nothing
    // here could go on.
    a.if_(size.gt_u(0x7fff_0000), |a| a.raw(I::Unreachable));
    let need = a.let_((size + (HDR + 7)) & -8);
    a.if_(need.lt_u(MIN_BLOCK), |a| a.set(need, MIN_BLOCK));
    let block = a.let_(0);

    // A block of exactly this size, kept from an earlier sweep.
    a.if_(need.le_u(BIN_MAX), |a| {
        a.set(block, word(bin(need), 0));
        a.if_(block, |a| {
            a.store(Ty::I32, bin(need), 0, word(block, 4));
        });
    });

    // The first large free block it fits in, split when what is left over is
    // a block itself.
    a.if_(block.eqz(), |a| {
        let previous = a.let_(0);
        let current = a.let_(g(GC_LARGE_GLOBAL));
        let size = a.local(Ty::I32);
        let rest = a.local(Ty::I32);
        let next = a.local(Ty::I32);
        let tail = a.local(Ty::I32);
        let relink = |a: &mut Asm, to: Var| {
            a.if_else(
                previous,
                |a| a.store(Ty::I32, previous, 4, to),
                |a| a.set_global(GC_LARGE_GLOBAL, to),
            );
        };
        a.loop_(|a, search| {
            a.brk_if(search, current.eqz());
            a.set(size, word(current, 0));
            a.if_(size.ge_u(need), |a| {
                a.set(next, word(current, 4));
                a.set(rest, size - need);
                a.if_else(
                    rest.ge_u(MIN_BLOCK),
                    |a| {
                        a.set(tail, current + need);
                        a.store(Ty::I32, tail, 0, rest);
                        a.if_else(
                            rest.gt_u(BIN_MAX),
                            |a| {
                                a.store(Ty::I32, tail, 4, next);
                                relink(a, tail);
                            },
                            |a| {
                                relink(a, next);
                                push_bin(a, tail, rest);
                            },
                        );
                    },
                    |a| {
                        a.set(need, size);
                        relink(a, next);
                    },
                );
                a.set(block, current);
                a.brk_if(search, 1);
            });
            a.set(previous, current);
            a.set(current, word(current, 4));
            a.cont(search);
        });
    });

    // Otherwise the end of the heap, growing memory to reach it.
    a.if_(block.eqz(), |a| {
        a.set(block, g(HEAP_TOP_GLOBAL));
        let top = a.let_(block + need);
        a.if_(top.lt_u(block), |a| a.raw(I::Unreachable));
        a.if_(top.gt_u(E::MemBytes), |a| {
            a.push(((top - E::MemBytes) + 65535).shr_u(16));
            a.raw(I::MemoryGrow(0));
            a.raw(I::I32Const(-1));
            a.raw(I::I32Eq);
            a.raw(I::If(wasm_encoder::BlockType::Empty));
            a.raw(I::Unreachable);
            a.raw(I::End);
        });
        a.set_global(HEAP_TOP_GLOBAL, top);
    });

    a.store(Ty::I32, block, 0, need | ALLOCATED);
    a.store(Ty::I32, block, 4, 0);
    // Zeroed as it is handed out. Most blocks are a few words, which a store
    // or two clears faster than a `memory.fill` does.
    a.if_else(
        need.le_u(HDR + 32),
        |a| {
            a.store(Ty::I64, block, HDR as u64, 0i64);
            a.if_(need.gt_u(HDR + 8), |a| {
                a.store(Ty::I64, block, HDR as u64 + 8, 0i64);
                a.if_(need.gt_u(HDR + 16), |a| {
                    a.store(Ty::I64, block, HDR as u64 + 16, 0i64);
                    a.if_(need.gt_u(HDR + 24), |a| {
                        a.store(Ty::I64, block, HDR as u64 + 24, 0i64);
                    });
                });
            });
        },
        |a| a.fill(block + HDR, 0, need - HDR),
    );
    a.set_global(GC_BLOCKS_GLOBAL, g(GC_BLOCKS_GLOBAL) + 1);
    a.set_global(GC_ALLOCATED_GLOBAL, g(GC_ALLOCATED_GLOBAL) + need);
    a.if_(g(GC_ALLOCATED_GLOBAL).ge_u(g(GC_THRESHOLD_GLOBAL)), |a| {
        a.set_global(GC_PENDING_GLOBAL, 1)
    });
    a.push(block + HDR);
    a.finish()
}

fn build_free() -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let block = a.let_(a.param(0) - HDR);
    let size = a.let_(size_of(word(block, 0)));
    a.store(Ty::I32, block, 0, size);
    a.set_global(GC_BLOCKS_GLOBAL, g(GC_BLOCKS_GLOBAL) - 1);
    a.if_else(
        size.le_u(BIN_MAX),
        |a| push_bin(a, block, size),
        |a| {
            a.store(Ty::I32, block, 4, g(GC_LARGE_GLOBAL));
            a.set_global(GC_LARGE_GLOBAL, block);
        },
    );
    a.finish()
}

fn build_grow_roots(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let needed = a.param(0);
    let capacity = a.let_(g(RS_CAP_GLOBAL) << 1);
    a.if_(capacity.lt_u(needed + 1024), |a| {
        a.set(capacity, needed + 1024)
    });
    let stack = a.let_(call(
        helper(ctx, GcFn::AllocRaw),
        vec![capacity.e()],
        Some(Ty::I32),
    ));
    a.if_(g(RS_BASE_GLOBAL), |a| {
        a.copy(stack, g(RS_BASE_GLOBAL), g(RS_TOP_GLOBAL));
        a.run(call(helper(ctx, GcFn::Free), vec![g(RS_BASE_GLOBAL)], None));
    });
    a.set_global(RS_BASE_GLOBAL, stack);
    a.set_global(RS_CAP_GLOBAL, capacity);
    a.finish()
}

fn build_root(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let value = a.param(0);
    // A module that never allocates has no objects to keep.
    if !ctx.alloc_used.get() {
        a.push(value);
        return a.finish();
    }
    // Nothing below the heap is an object: a number, None, a literal.
    a.if_(value.lt_u(g(HEAP_BASE_GLOBAL)), |a| a.ret(value));
    let top = a.let_(g(RS_TOP_GLOBAL));
    a.if_((top + 4).gt_u(g(RS_CAP_GLOBAL)), |a| {
        a.run(call(helper(ctx, GcFn::GrowRoots), vec![top + 4], None));
    });
    a.store(Ty::I32, g(RS_BASE_GLOBAL) + top, 0, value);
    a.set_global(RS_TOP_GLOBAL, top + 4);
    a.push(value);
    a.finish()
}

fn build_enter(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let bytes = a.param(0);
    let frame = a.let_(g(RS_TOP_GLOBAL));
    let top = a.let_(frame + bytes);
    a.if_(top.gt_u(g(RS_CAP_GLOBAL)), |a| {
        a.run(call(helper(ctx, GcFn::GrowRoots), vec![top.e()], None));
    });
    a.fill(g(RS_BASE_GLOBAL) + frame, 0, bytes);
    a.set_global(RS_TOP_GLOBAL, top);
    a.push(frame);
    a.finish()
}

/// The allocated block `word` points into, by binary search of the index a
/// collection builds. A word pointing at a header, into a free block, or
/// outside the heap finds nothing.
fn build_find() -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let target = a.param(0);
    let index = g(GC_INDEX_GLOBAL);
    a.if_(g(GC_COUNT_GLOBAL).eqz(), |a| a.ret(0));
    a.if_(word(index.clone(), 0).gt_u(target), |a| a.ret(0));
    let low = a.let_(0);
    let high = a.let_(g(GC_COUNT_GLOBAL));
    let middle = a.local(Ty::I32);
    a.loop_(|a, search| {
        a.brk_if(search, (high - low).le_u(1));
        a.set(middle, (low + high).shr_u(1));
        a.if_else(
            word(index.clone() + (middle << 2), 0).le_u(target),
            |a| a.set(low, middle),
            |a| a.set(high, middle),
        );
        a.cont(search);
    });
    let block = a.let_(word(index + (low << 2), 0));
    a.if_(target.lt_u(block + HDR), |a| a.ret(0));
    a.if_(target.ge_u(block + size_of(word(block, 0))), |a| a.ret(0));
    a.push(block);
    a.finish()
}

/// Whether `value` can be a reference at all: aligned as every reference is,
/// and inside the heap.
fn outside_heap(value: Var) -> E {
    (value & 3) | value.lt_u(g(HEAP_BASE_GLOBAL)) | value.ge_u(g(HEAP_TOP_GLOBAL))
}

fn build_mark_word(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let value = a.param(0);
    a.if_(outside_heap(value), |a| a.ret_void());
    let block = a.let_(call(
        helper(ctx, GcFn::Find),
        vec![value.e()],
        Some(Ty::I32),
    ));
    a.if_(block.eqz(), |a| a.ret_void());
    let header = a.let_(word(block, 0));
    a.if_(header & MARKED, |a| a.ret_void());
    a.store(Ty::I32, block, 0, header | MARKED);
    a.store(Ty::I32, g(GC_STACK_GLOBAL), 0, block);
    a.set_global(GC_STACK_GLOBAL, g(GC_STACK_GLOBAL) + 4);
    a.finish()
}

fn build_find_dead(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let value = a.param(0);
    a.if_(outside_heap(value), |a| a.ret(0));
    let block = a.let_(call(
        helper(ctx, GcFn::Find),
        vec![value.e()],
        Some(Ty::I32),
    ));
    a.if_(block.eqz(), |a| a.ret(0));
    a.if_(word(block, 0) & MARKED, |a| a.ret(0));
    a.push(block);
    a.finish()
}

fn build_scan(ctx: &CompilationContext) -> Function {
    let mark = helper(ctx, GcFn::MarkWord);
    let mut a = Asm::new(&[Ty::I32, Ty::I32]);
    let (at, end) = (a.param(0), a.param(1));
    a.while_(at.lt_u(end), |a, _| {
        a.run(call(mark, vec![word(at, 0)], None));
        a.set(at, at + 4);
    });
    a.finish()
}

fn build_drain(ctx: &CompilationContext) -> Function {
    let scan = helper(ctx, GcFn::Scan);
    let mut a = Asm::new(&[Ty::I32]);
    let base = a.param(0);
    let block = a.local(Ty::I32);
    a.while_(g(GC_STACK_GLOBAL).gt_u(base), |a, _| {
        a.set_global(GC_STACK_GLOBAL, g(GC_STACK_GLOBAL) - 4);
        a.set(block, word(g(GC_STACK_GLOBAL), 0));
        a.if_((word(block, 4) & NOSCAN).eqz(), |a| {
            a.run(call(
                scan,
                vec![block + HDR, block + size_of(word(block, 0))],
                None,
            ));
        });
    });
    a.finish()
}

/// Whether the block's finalizer is still to run, from its second header
/// word.
fn awaits_finalizer(aux: impl Into<E>) -> E {
    (aux.into() & (FINALIZABLE | FINALIZED)).eq(FINALIZABLE)
}

/// Free `block` as reference counting would: queue its finalizer, then drop
/// the references it holds, in order, freeing in turn whatever that leaves
/// with none. The walk is depth first, because CPython's is: an object's
/// finalizer runs before those of what only it held.
///
/// `block` must be unmarked. One that something else dead still refers to
/// (its count is not zero) is left alone, as is one already visited.
fn build_peel(ctx: &CompilationContext) -> Function {
    let find_dead = helper(ctx, GcFn::FindDead);
    let mut a = Asm::new(&[Ty::I32, Ty::I32]);
    let (first, base) = (a.param(0), a.param(1));
    // The stack holds pairs: a block, and the word its scan reads next. A
    // block marked `FORWARD` is read first word to last, any other last to
    // first.
    let visit = |a: &mut Asm, block: Var, top: Var| {
        a.store(Ty::I32, block, 0, word(block, 0) | PEELED);
        a.if_(awaits_finalizer(word(block, 4)), |a| {
            a.store(Ty::I32, g(GC_QUEUE_GLOBAL), 0, block);
            a.set_global(GC_QUEUE_GLOBAL, g(GC_QUEUE_GLOBAL) + 4);
        });
        a.store(Ty::I32, top, 0, block);
        // A block of text starts past its end, so nothing in it is read.
        a.store(
            Ty::I32,
            top,
            4,
            (word(block, 4) & NOSCAN).pick(
                block + size_of(word(block, 0)),
                (word(block, 4) & FORWARD).pick(block + HDR, block + size_of(word(block, 0)) - 4),
            ),
        );
        a.set(top, top + 8);
    };
    a.if_(
        (word(first, 4) & !AUX_FLAGS) | (word(first, 0) & PEELED),
        |a| a.ret_void(),
    );
    let top = a.let_(base);
    visit(&mut a, first, top);
    let block = a.local(Ty::I32);
    let cursor = a.local(Ty::I32);
    let child = a.local(Ty::I32);
    let count = a.local(Ty::I32);
    a.while_(top.gt_u(base), |a, walk| {
        a.set(block, word(top - 8, 0));
        a.set(cursor, word(top - 4, 0));
        // Past either end of the block's contents: it is done.
        a.if_(
            cursor.lt_u(block + HDR) | cursor.ge_u(block + size_of(word(block, 0))),
            |a| {
                a.set(top, top - 8);
                a.cont(walk);
            },
        );
        a.store(
            Ty::I32,
            top - 4,
            0,
            (word(block, 4) & FORWARD).pick(cursor + 4, cursor - 4),
        );
        a.set(child, call(find_dead, vec![word(cursor, 0)], Some(Ty::I32)));
        // Nothing, or the block's own word for where its elements are,
        // which was not counted.
        a.if_(child.eqz() | child.eq(block), |a| a.cont(walk));
        a.set(count, word(child, 4) - 1);
        a.store(Ty::I32, child, 4, count);
        a.if_((count & !AUX_FLAGS) | (word(child, 0) & PEELED), |a| {
            a.cont(walk)
        });
        visit(a, child, top);
    });
    a.finish()
}

/// A full collection, or nothing in a module that never allocates (whose
/// statements still test for one being due).
fn build_safepoint(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[]);
    if ctx.alloc_used.get() {
        a.run(call(
            helper(ctx, GcFn::Collect),
            vec![E::I32(0), E::I32(0), E::I32(COLLECT_FULL), E::I32(0)],
            None,
        ));
    }
    a.finish()
}

/// `collect(dropped_from, dropped_to, mode, keep)`.
///
/// `dropped_from..dropped_to` is a range of the root stack (as offsets) that
/// has just been popped: the references the caller dropped, in the order it
/// dropped them, which is the order their finalizers run in. `keep` is one
/// more word to treat as a root, for a value the caller holds on the operand
/// stack.
fn build_collect(ctx: &CompilationContext) -> Function {
    let alloc_raw = helper(ctx, GcFn::AllocRaw);
    let free = helper(ctx, GcFn::Free);
    let mark = helper(ctx, GcFn::MarkWord);
    let scan = helper(ctx, GcFn::Scan);
    let drain = helper(ctx, GcFn::Drain);
    let finalizers = !ctx.finalizable_class_ids().is_empty();

    let mut a = Asm::new(&[Ty::I32; 4]);
    let (dropped_from, dropped_to, mode, keep) = (a.param(0), a.param(1), a.param(2), a.param(3));
    a.if_(g(GC_ACTIVE_GLOBAL), |a| a.ret_void());
    a.set_global(GC_ACTIVE_GLOBAL, 1);

    let live = a.local(Ty::I32);
    let work = a.local(Ty::I32);
    let slots = a.local(Ty::I32);
    let count = a.local(Ty::I32);
    let stack = a.local(Ty::I32);
    let queue = a.local(Ty::I32);
    let at = a.local(Ty::I32);
    let i = a.local(Ty::I32);
    let block = a.local(Ty::I32);
    let header = a.local(Ty::I32);
    let size = a.local(Ty::I32);
    let run = a.local(Ty::I32);
    let tail = a.local(Ty::I32);
    let blocks = a.local(Ty::I32);
    let ran = a.local(Ty::I32);
    let any = a.local(Ty::I32);
    let aux = a.local(Ty::I32);

    a.loop_(|a, again| {
        // Working memory: the index of allocated blocks, a stack of as many
        // words for marking (twice as many in a module with finalizers, whose
        // walk pushes pairs), and a queue with room for every object still
        // to finalize. The block holding it is one more than are allocated
        // now.
        let stack_shift = if finalizers { 3 } else { 2 };
        a.set(slots, g(GC_BLOCKS_GLOBAL) + 2);
        a.set(
            work,
            call(
                alloc_raw,
                vec![(slots << 2) + (slots << stack_shift) + (g(FIN_LIVE_GLOBAL) << 2) + 16],
                Some(Ty::I32),
            ),
        );
        a.set(stack, work + (slots << 2));
        a.set(queue, stack + (slots << stack_shift));
        a.set_global(GC_INDEX_GLOBAL, work);
        a.set_global(GC_STACK_GLOBAL, stack);
        a.set_global(GC_QUEUE_GLOBAL, queue);

        // The index: every allocated block, in address order.
        a.set(at, g(HEAP_BASE_GLOBAL));
        a.set(count, 0);
        a.while_(at.lt_u(g(HEAP_TOP_GLOBAL)), |a, _| {
            a.set(header, word(at, 0));
            a.if_(header & ALLOCATED, |a| {
                a.store(Ty::I32, work + (count << 2), 0, at);
                a.set(count, count + 1);
            });
            a.set(at, at + size_of(header));
        });
        a.set_global(GC_COUNT_GLOBAL, count);

        // The collector's own blocks are kept, and not read as objects.
        let keep_block = |a: &mut Asm, contents: E| {
            a.set(block, contents - HDR);
            a.store(Ty::I32, block, 0, word(block, 0) | MARKED);
        };
        keep_block(a, work.e());
        a.if_(g(RS_BASE_GLOBAL), |a| {
            keep_block(a, g(RS_BASE_GLOBAL));
            a.run(call(
                scan,
                vec![g(RS_BASE_GLOBAL), g(RS_BASE_GLOBAL) + g(RS_TOP_GLOBAL)],
                None,
            ));
        });
        a.run(call(mark, vec![keep.e()], None));
        a.run(call(mark, vec![g(EXC_VALUE_GLOBAL)], None));
        for global in ctx.reference_globals() {
            a.run(call(mark, vec![g(global)], None));
        }
        for (from, to) in static_roots() {
            a.run(call(scan, vec![E::I32(from), E::I32(to)], None));
        }
        a.run(call(drain, vec![stack.e()], None));

        if finalizers {
            let peel = helper(ctx, GcFn::Peel);
            let find_dead = helper(ctx, GcFn::FindDead);
            // Each unmarked block, with its header word in `header`.
            let each_dead = |a: &mut Asm, body: &mut dyn FnMut(&mut Asm)| {
                a.for_(i, 0, count, false, |a, next| {
                    a.set(block, word(work + (i << 2), 0));
                    a.set(header, word(block, 0));
                    a.if_(header & MARKED, |a| {
                        a.set(i, i + 1);
                        a.cont(next);
                    });
                    body(a);
                });
            };
            a.if_(g(FIN_LIVE_GLOBAL), |a| {
                // Is anything with a finalizer unreachable? Start every
                // unreachable block's count of references at zero.
                a.set(any, 0);
                each_dead(a, &mut |a| {
                    a.set(aux, word(block, 4) & AUX_FLAGS);
                    a.store(Ty::I32, block, 4, aux);
                    a.if_(awaits_finalizer(aux), |a| a.set(any, 1));
                });
                a.if_(any, |a| {
                    // Count the references each unreachable block has from
                    // the others. One it has to itself counts too (such an
                    // object is a cycle, which reference counting never
                    // frees), but only one to its own start: a collection
                    // built with its elements after its header points into
                    // itself to say where they are, which is no reference.
                    let target = a.local(Ty::I32);
                    each_dead(a, &mut |a| {
                        a.set(at, block + HDR);
                        a.set(size, block + size_of(header));
                        // Text refers to nothing.
                        a.if_(word(block, 4) & NOSCAN, |a| a.set(at, size));
                        a.while_(at.lt_u(size), |a, _| {
                            a.set(target, call(find_dead, vec![word(at, 0)], Some(Ty::I32)));
                            a.if_(
                                target.e().ne(0) & (target.ne(block) | word(at, 0).eq(block + HDR)),
                                |a| {
                                    a.store(Ty::I32, target, 4, word(target, 4) + 1);
                                },
                            );
                            a.set(at, at + 4);
                        });
                    });
                    // What the caller just dropped goes first, in the order
                    // it dropped it; then everything else nothing refers to.
                    a.set(at, g(RS_BASE_GLOBAL) + dropped_from);
                    a.set(size, g(RS_BASE_GLOBAL) + dropped_to);
                    a.while_(at.lt_u(size), |a, _| {
                        a.set(target, call(find_dead, vec![word(at, 0)], Some(Ty::I32)));
                        a.if_(target, |a| {
                            a.run(call(peel, vec![target.e(), stack.e()], None));
                        });
                        a.set(at, at + 4);
                    });
                    each_dead(a, &mut |a| {
                        a.run(call(peel, vec![block.e(), stack.e()], None));
                    });
                    // What is left is held by a cycle. CPython finalizes it
                    // when its collector runs, which a full collection is.
                    a.if_(mode.eq(COLLECT_FULL), |a| {
                        each_dead(a, &mut |a| {
                            a.if_(
                                awaits_finalizer(word(block, 4)) & (header & PEELED).eqz(),
                                |a| {
                                    a.store(Ty::I32, g(GC_QUEUE_GLOBAL), 0, block);
                                    a.set_global(GC_QUEUE_GLOBAL, g(GC_QUEUE_GLOBAL) + 4);
                                },
                            );
                        });
                    });
                    // An object waiting for its finalizer is kept, with all
                    // it reaches: the finalizer may read any of it.
                    a.for_(i, 0, count, false, |a, _| {
                        a.set(block, word(work + (i << 2), 0));
                        a.if_(
                            (word(block, 0) & MARKED).eqz() & awaits_finalizer(word(block, 4)),
                            |a| {
                                a.run(call(mark, vec![block + HDR], None));
                            },
                        );
                    });
                    a.run(call(drain, vec![stack.e()], None));
                });
            });
        }

        // Sweep: free what was not marked, joining neighbours, and rebuild
        // the free lists from what is free now.
        a.fill(g(GC_BINS_GLOBAL), 0, BINS_BYTES as i32);
        a.set_global(GC_LARGE_GLOBAL, 0);
        // A cached fact about a string is keyed by its address, which a new
        // string may take from here on.
        let (cache, cache_bytes) = crate::compiler::unicode::STR_CACHE_REGION;
        a.fill(cache, 0, cache_bytes);
        let release = |a: &mut Asm, to: E| {
            a.set(size, to - run);
            a.store(Ty::I32, run, 0, size);
            a.if_else(
                size.le_u(BIN_MAX),
                |a| push_bin(a, run, size),
                |a| {
                    a.store(Ty::I32, run, 4, 0);
                    a.if_else(
                        tail,
                        |a| a.store(Ty::I32, tail, 4, run),
                        |a| a.set_global(GC_LARGE_GLOBAL, run),
                    );
                    a.set(tail, run);
                },
            );
            a.set(run, 0);
        };
        a.set(at, g(HEAP_BASE_GLOBAL));
        a.set(run, 0);
        a.set(tail, 0);
        a.set(live, 0);
        a.set(blocks, 0);
        a.while_(at.lt_u(g(HEAP_TOP_GLOBAL)), |a, _| {
            a.set(header, word(at, 0));
            a.if_else(
                (header & (ALLOCATED | MARKED)).eq(ALLOCATED | MARKED),
                |a| {
                    a.store(Ty::I32, at, 0, header & !(MARKED | PEELED));
                    a.set(live, live + size_of(header));
                    a.set(blocks, blocks + 1);
                    a.if_(run, |a| release(a, at.e()));
                },
                |a| {
                    // A JavaScript value's proxy gives its handle back.
                    if let Some(release) = ctx.js.get(&crate::compiler::js::JsImport::Release) {
                        a.if_(
                            (header & ALLOCATED).ne(0) & (word(at, 4) & JSREF).ne(0),
                            |a| a.run(call(*release, vec![word(at, HDR as u64)], None)),
                        );
                    }
                    if stress() {
                        a.if_(header & ALLOCATED, |a| {
                            a.fill(at + HDR, 0xDD, size_of(header) - HDR);
                        });
                    }
                    a.if_(run.eqz(), |a| a.set(run, at));
                },
            );
            a.set(at, at + size_of(header));
        });
        // Free blocks at the very end are the heap's to extend into again.
        a.if_(run, |a| a.set_global(HEAP_TOP_GLOBAL, run));
        a.set_global(GC_BLOCKS_GLOBAL, blocks);
        // What is alive is what the program holds: not this working memory,
        // which would otherwise count towards when to collect next.
        a.set(live, live - size_of(word(work - HDR, 0)));

        a.set(ran, 0);
        if finalizers {
            let finalize = helper(ctx, GcFn::Finalize);
            a.set(at, queue);
            a.while_(at.lt_u(g(GC_QUEUE_GLOBAL)), |a, _| {
                a.set(block, word(at, 0));
                a.store(Ty::I32, block, 4, word(block, 4) | FINALIZED);
                a.set_global(FIN_LIVE_GLOBAL, g(FIN_LIVE_GLOBAL) - 1);
                a.run(call(finalize, vec![block + HDR], None));
                a.set(ran, 1);
                a.set(at, at + 4);
            });
        }
        a.run(call(free, vec![work.e()], None));
        // A finalizer ran: what it finalized can be freed now, unless it was
        // stored somewhere, and what it dropped can be finalized in turn.
        a.set(dropped_to, dropped_from);
        a.cont_if(again, ran);
    });

    a.set_global(GC_ALLOCATED_GLOBAL, 0);
    a.set_global(GC_PENDING_GLOBAL, 0);
    if !stress() {
        a.set_global(
            GC_THRESHOLD_GLOBAL,
            live.gt_u(MIN_THRESHOLD).select(live, MIN_THRESHOLD),
        );
    }
    a.set_global(GC_ACTIVE_GLOBAL, 0);
    a.finish()
}

/// The words of static state that hold objects: standard input's buffer,
/// `sys.argv` and `sys.path` once built, and `os.environ`.
fn static_roots() -> [(i32, i32); 2] {
    [
        crate::compiler::host::STATE,
        crate::compiler::stdlib::OS_STATE,
    ]
}

/// `finalize(object)`: call the `__del__` its class resolves to. An
/// exception it raises is discarded, as CPython discards one (after
/// reporting it on stderr), and whatever was pending around it is left as it
/// was.
fn build_finalize(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let object = a.param(0);
    let pending_type = a.let_(g(EXC_TYPE_GLOBAL));
    let pending_value = a.let_(g(EXC_VALUE_GLOBAL));
    let stopped = a.let_(g(STOP_GLOBAL));
    a.set_global(EXC_TYPE_GLOBAL, 0);
    a.set_global(EXC_VALUE_GLOBAL, 0);
    a.set_global(STOP_GLOBAL, 0);
    a.set_global(CALL_DEPTH_GLOBAL, g(CALL_DEPTH_GLOBAL) + 1);
    let class_id = a.let_(word(object, 0));
    for (id, function, result) in ctx.finalizers() {
        a.if_(class_id.eq(id), |a| {
            let result = if matches!(result, IRType::Float) {
                Ty::F64
            } else {
                Ty::I32
            };
            a.run(call(function, vec![object.e()], Some(result)));
        });
    }
    a.set_global(CALL_DEPTH_GLOBAL, g(CALL_DEPTH_GLOBAL) - 1);
    a.set_global(EXC_TYPE_GLOBAL, pending_type);
    a.set_global(EXC_VALUE_GLOBAL, pending_value);
    a.set_global(STOP_GLOBAL, stopped);
    a.finish()
}

/// `exit()`: what CPython does to a module as the interpreter shuts down.
/// Each module-level name is cleared in the order it was defined, which
/// finalizes what only it held, and then a last full collection finalizes
/// whatever is left.
fn build_exit(ctx: &CompilationContext) -> Function {
    let collect = helper(ctx, GcFn::Collect);
    let globals = ctx.reference_globals();
    let bytes = (globals.len() * 4) as i32;
    let mut a = Asm::new(&[]);
    let top = a.let_(g(RS_TOP_GLOBAL));
    a.if_((top + bytes).gt_u(g(RS_CAP_GLOBAL)), |a| {
        a.run(call(helper(ctx, GcFn::GrowRoots), vec![top + bytes], None));
    });
    for (i, global) in globals.iter().enumerate() {
        a.store(Ty::I32, g(RS_BASE_GLOBAL) + top, (i * 4) as u64, g(*global));
        a.set_global(*global, 0);
    }
    a.run(call(
        collect,
        vec![top.e(), top + bytes, E::I32(COLLECT_FULL), E::I32(0)],
        None,
    ));
    a.finish()
}
