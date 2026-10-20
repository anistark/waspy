use crate::compiler::context::{
    comp_gen_local_name, comp_local_name, strlen_local_name, CompilationContext, COLLECTION_CAP,
    COLLECTION_DATA, COLLECTION_HEADER, COLLECTION_SLOT, DICT_ENTRY,
};
use crate::compiler::function::{
    emit_call_depth_step, emit_post_call_check, emit_raise, load_field_instr, lookup_field,
};
use crate::ir::{
    IRBoolOp, IRCompareOp, IRComprehensionKind, IRConstant, IRExpr, IRGenerator, IROp, IRType,
    IRUnaryOp, MemoryLayout, MethodKind, STRING_LEN_PREFIX,
};
use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

/// Refuse a float value headed for a place this compiler holds as an int: a
/// variable or field typed int, an int parameter, an index. Python would keep
/// the float (or raise, for an index); converting it would truncate silently.
pub(crate) fn report_float_where_int(ctx: &CompilationContext) {
    ctx.report(
        "a float value is stored where this program holds an int, which would truncate it. \
         Python keeps the float: hold it in a float variable or field (start it as 0.0, or \
         annotate it ': float'), or convert explicitly with int() or use '//'",
    );
}

/// `a is b` / `a is not b` with both one-word values on the stack. Identity
/// is the word: an instance's pointer, None's 0. These dropped both operands
/// and answered False, so `x is None` was never true and a `while node is
/// not None` walk never ran.
///
/// None is the word 0, which an untyped value can also be, so `x is None` on
/// one cannot be told apart from `x == 0`. An optional number is boxed, so its
/// None is the null pointer and compares like any other optional. A value
/// whose type can never be None answers a constant; one that can be both is
/// refused.
fn emit_identity(
    func: &mut Function,
    ctx: &CompilationContext,
    op: &IRCompareOp,
    left: &IRType,
    right: &IRType,
) {
    let is = matches!(op, IRCompareOp::Is);
    let numeric = |t: &IRType| matches!(t, IRType::Int | IRType::Bool | IRType::Float);
    // An optional number is boxed, so None (the null pointer) is told apart
    // from 0 like any other optional.
    let zero_or_none = |t: &IRType| match t {
        IRType::Optional(inner) => matches!(**inner, IRType::Unknown | IRType::Any),
        IRType::Unknown | IRType::Any => true,
        _ => false,
    };
    let other = match (left, right) {
        (IRType::None, other) | (other, IRType::None) => Some(other),
        _ => None,
    };
    let constant = |func: &mut Function, answer: bool| {
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(answer as i32));
    };
    match other {
        // `None is None`.
        Some(IRType::None) => constant(func, is),
        // An int or a bool is never None.
        Some(t) if numeric(t) => constant(func, !is),
        Some(t) if zero_or_none(t) => {
            ctx.report(format!(
                "'is None' on a value of type '{}' cannot tell None from 0 here, since both \
                 are the same word. Hint: use a class or a str for the optional value, or a \
                 separate flag",
                crate::type_to_string(t)
            ));
            constant(func, false);
        }
        Some(_) => {
            func.instruction(&if is {
                Instruction::I32Eq
            } else {
                Instruction::I32Ne
            });
        }
        None if numeric(left) || numeric(right) => {
            ctx.report("'is' between two numbers is not supported: use '=='");
            constant(func, false);
        }
        None => {
            func.instruction(&if is {
                Instruction::I32Eq
            } else {
                Instruction::I32Ne
            });
        }
    }
}

/// The answer of `isinstance(v, target)` for a builtin type `target`, from
/// `v`'s static type, or `None` when `target` is not a builtin type.
fn builtin_isinstance(ty: &IRType, target: &str) -> Option<bool> {
    let answer = match target {
        // bool is a subclass of int.
        "int" => matches!(ty, IRType::Int | IRType::Bool),
        "bool" => matches!(ty, IRType::Bool),
        "float" => matches!(ty, IRType::Float),
        "str" => matches!(ty, IRType::String),
        "bytes" => matches!(ty, IRType::Bytes),
        "list" => matches!(ty, IRType::List(_)),
        "tuple" => matches!(ty, IRType::Tuple(_) | IRType::VarTuple(_)),
        "dict" => matches!(ty, IRType::Dict(_, _)),
        "set" => matches!(ty, IRType::Set(_)),
        "range" => matches!(ty, IRType::Range),
        "object" => true,
        _ => return None,
    };
    Some(answer)
}

/// `isinstance` with the object's value on the stack, for each name in
/// `targets` (a user class or a builtin type), or'd together.
fn emit_isinstance(
    func: &mut Function,
    ctx: &CompilationContext,
    obj_type: &IRType,
    targets: &[&str],
) -> IRType {
    let drop_obj = |func: &mut Function| {
        func.instruction(&Instruction::Drop);
        if matches!(obj_type, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
    };
    if matches!(
        obj_type,
        IRType::Unknown | IRType::Any | IRType::Union(_) | IRType::Optional(_)
    ) {
        ctx.report(format!(
            "isinstance() of a value of type '{}' cannot be answered here, since its type is \
             only known at run time. Hint: annotate the value it comes from",
            crate::type_to_string(obj_type)
        ));
        drop_obj(func);
        func.instruction(&Instruction::I32Const(0));
        return IRType::Bool;
    }
    let mut constant = false;
    let mut class_ids = Vec::new();
    for target in targets {
        if ctx.get_class_info(target).is_some() {
            if matches!(obj_type, IRType::Class(_)) {
                class_ids.extend(ctx.assignable_class_ids(target));
            }
        } else if let Some(answer) = builtin_isinstance(obj_type, target) {
            constant |= answer;
        } else {
            ctx.report(format!(
                "isinstance() against '{target}' is not supported: it is not a class this \
                 program defines or a builtin type"
            ));
        }
    }
    if constant || class_ids.is_empty() {
        drop_obj(func);
        func.instruction(&Instruction::I32Const(constant as i32));
        return IRType::Bool;
    }
    // tag = *(obj + 0); fold `tag == id` over the assignable ids with `or`.
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(ctx.temp_local));
    func.instruction(&Instruction::I32Const(0));
    class_ids.sort_unstable();
    class_ids.dedup();
    for id in class_ids {
        func.instruction(&Instruction::LocalGet(ctx.temp_local));
        func.instruction(&Instruction::I32Const(id));
        func.instruction(&Instruction::I32Eq);
        func.instruction(&Instruction::I32Or);
    }
    IRType::Bool
}

/// `text[start:end:step]` for a str or bytes on the stack as `(offset, len)`
/// and a step other than 1, as CPython's `slice.indices` computes it: a
/// negative step starts from the end and stops before the start. The bytes
/// picked are copied into a fresh blob. A zero step traps, as CPython raises
/// ValueError.
fn emit_stepped_text_slice(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    start: Option<&IRExpr>,
    end: Option<&IRExpr>,
    step: &IRExpr,
    is_string: bool,
) {
    let held: Vec<u32> = (0..5)
        .map(|_| crate::compiler::equality::hold(ctx))
        .collect();
    let (off, len, lo, hi, st) = (held[0], held[1], held[2], held[3], held[4]);
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));
    // A str's bounds count code points.
    if is_string {
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::Call(
            ctx.helper(crate::compiler::runtime::Helper::CharLen),
        ));
        func.instruction(&Instruction::LocalSet(len));
    }
    emit_expr(step, func, ctx, memory_layout, Some(&IRType::Int));
    func.instruction(&Instruction::LocalTee(st));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);

    // Each bound: given, normalized (a negative one counts from the end) and
    // clamped to [0, len] for a positive step or [-1, len - 1] for a negative
    // one; absent, the end the step starts from or runs to.
    for (bound, local, is_start) in [(start, lo, true), (end, hi, false)] {
        match bound {
            Some(expr) => {
                emit_expr(expr, func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::LocalSet(local));
                // if v < 0: v += len
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::LocalGet(len));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32LtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(local));
                // lower = step < 0 ? -1 : 0; upper = step < 0 ? len - 1 : len
                let push_bounds = |func: &mut Function| {
                    func.instruction(&Instruction::LocalGet(st));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                };
                // v = max(v, lower), lower = -(step < 0)
                func.instruction(&Instruction::I32Const(0));
                push_bounds(func);
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::LocalTee(ctx.temp_local + 9));
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 9));
                func.instruction(&Instruction::I32LtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(local));
                // v = min(v, upper)
                func.instruction(&Instruction::LocalGet(len));
                push_bounds(func);
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::LocalTee(ctx.temp_local + 9));
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 9));
                func.instruction(&Instruction::I32GtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(local));
            }
            None => {
                // start: 0 forwards, len - 1 backwards; end: len forwards, -1
                // backwards.
                if is_start {
                    func.instruction(&Instruction::LocalGet(len));
                    func.instruction(&Instruction::I32Const(-1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Const(0));
                } else {
                    func.instruction(&Instruction::I32Const(-1));
                    func.instruction(&Instruction::LocalGet(len));
                }
                func.instruction(&Instruction::LocalGet(st));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32LtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(local));
            }
        }
    }

    // count = max(0, ceil((hi - lo) / step)), computed over magnitudes.
    let count = ctx.temp_local + 10;
    let mag = ctx.temp_local + 11;
    func.instruction(&Instruction::LocalGet(hi));
    func.instruction(&Instruction::LocalGet(lo));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::LocalGet(st));
    func.instruction(&Instruction::LocalSet(mag));
    func.instruction(&Instruction::LocalGet(st));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    for local in [count, mag] {
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalGet(local));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(local));
    }
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::LocalGet(mag));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(mag));
    func.instruction(&Instruction::I32DivS);
    func.instruction(&Instruction::LocalTee(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(count));

    // A fresh blob: the length prefix, the picked bytes, and a NUL for a str.
    let data = ctx.temp_local + 12;
    let i = ctx.temp_local + 13;
    if is_string {
        emit_stepped_str_copy(func, ctx, off, lo, st, count);
        crate::compiler::equality::release(ctx, 5);
        return;
    }
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(
        STRING_LEN_PREFIX as i32 + i32::from(is_string),
    ));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(data));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(data));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    // data[i] = text[lo + i * step]
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(lo));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(st));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    if is_string {
        func.instruction(&Instruction::LocalGet(data));
        func.instruction(&Instruction::LocalGet(count));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Store8(byte_arg()));
    }
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(count));
    crate::compiler::equality::release(ctx, 5);
}

/// The `count` code points of the string at `off` from code point `lo`, `st`
/// apart, as a new string: their bytes measured, then each character copied
/// whole. Leaves the `(offset, length)` pair.
fn emit_stepped_str_copy(
    func: &mut Function,
    ctx: &CompilationContext,
    off: u32,
    lo: u32,
    st: u32,
    count: u32,
) {
    let char_offset = ctx.helper(crate::compiler::runtime::Helper::CharOffset);
    let (data, i, total, b, w, lead) = (
        ctx.temp_local + 12,
        ctx.temp_local + 13,
        ctx.temp_local + 14,
        ctx.temp_local + 15,
        ctx.temp_local + 16,
        ctx.temp_local + 17,
    );
    // For each picked character, its byte offset into `b` and width into `w`,
    // then `body`.
    let walk = |func: &mut Function, body: &dyn Fn(&mut Function)| {
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(count));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(lo));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(st));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::Call(char_offset));
        func.instruction(&Instruction::LocalSet(b));
        crate::compiler::unicode::emit_width_at(func, off, b, lead);
        func.instruction(&Instruction::LocalSet(w));
        body(func);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    };
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(total));
    walk(func, &|func| {
        func.instruction(&Instruction::LocalGet(total));
        func.instruction(&Instruction::LocalGet(w));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(total));
    });
    emit_alloc_string(func, ctx, total, data);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(total));
    walk(func, &|func| {
        func.instruction(&Instruction::LocalGet(data));
        func.instruction(&Instruction::LocalGet(total));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(b));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(w));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        func.instruction(&Instruction::LocalGet(total));
        func.instruction(&Instruction::LocalGet(w));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(total));
    });
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(total));
}

/// A local (or the generator field it was lifted into) that the loop lowering
/// in `ir::converter` parks a collection in: a `for ... in d.items()` dict or
/// a later `zip()` argument.
fn is_compiler_name(expr: &IRExpr) -> bool {
    let made =
        |name: &str| name.starts_with("__dict_") || name.starts_with(crate::ir::ZIP_SEQ_PREFIX);
    match expr {
        IRExpr::Variable(name) | IRExpr::Param(name) => made(name),
        IRExpr::Attribute { attribute, .. } => made(attribute),
        _ => false,
    }
}

/// `len(r)` for the range block on top of the stack, as CPython computes it:
/// `max(0, ceil((stop - start) / step))`, rounding away from zero for a
/// negative step.
fn emit_range_len(func: &mut Function, ctx: &CompilationContext) {
    let r = ctx.temp_local;
    let span = ctx.temp_local + 1;
    let step = ctx.temp_local + 2;
    func.instruction(&Instruction::LocalTee(r));
    func.instruction(&Instruction::I32Load(mem_off(8)));
    func.instruction(&Instruction::LocalSet(step));
    // span = stop - start, and the step's magnitude, both made positive for a
    // negative step: (start - stop) over -step.
    func.instruction(&Instruction::LocalGet(r));
    func.instruction(&Instruction::I32Load(mem_off(4)));
    func.instruction(&Instruction::LocalGet(r));
    func.instruction(&Instruction::I32Load(mem_off(0)));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(span));
    func.instruction(&Instruction::LocalGet(step));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    for local in [span, step] {
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalGet(local));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(local));
    }
    func.instruction(&Instruction::End);
    // max(0, (span + step - 1) / step); a zero step traps, as CPython's
    // range() raises for one.
    func.instruction(&Instruction::LocalGet(span));
    func.instruction(&Instruction::LocalGet(step));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(step));
    func.instruction(&Instruction::I32DivS);
    func.instruction(&Instruction::LocalTee(span));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(span));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
}

/// Most `min()`/`max()` arguments the fold parks in scratch locals.
const MIN_MAX_MAX_ARGS: usize = 12;

/// `min(a, b, ...)` / `max(a, b, ...)` with every argument already on the
/// stack. CPython keeps the first argument and replaces it only with one that
/// is strictly smaller (larger), which is what decides ties and NaN, and is
/// the order this folds in. Arguments must all be ints, all floats, or all
/// strings: the result of a mix would carry whichever type won at run time.
/// A held word, or a held f64 for a float, for a value of `ty`.
fn hold_value(ctx: &CompilationContext, ty: &IRType) -> u32 {
    if matches!(ty, IRType::Float) {
        match ctx.hold_f64() {
            Some(slot) => slot,
            None => {
                ctx.report("min() or max() nests more deeply than the compiler reserves room for");
                ctx.held_f64_depth.set(ctx.held_f64_depth.get() + 1);
                ctx.temp_local_f64
            }
        }
    } else {
        crate::compiler::equality::hold(ctx)
    }
}

fn release_value(ctx: &CompilationContext, ty: &IRType) {
    if matches!(ty, IRType::Float) {
        ctx.release_held_f64();
    } else {
        ctx.release_held();
    }
}

/// `min(iterable, *, key=..., default=...)` and `max(...)` (#118), and the
/// several-argument form when it takes a `key`, which is the same fold over
/// the arguments as a tuple.
///
/// The first element is the running best, and a later one replaces it only
/// when strictly smaller (larger for `max`), which is how CPython decides ties
/// and a NaN. With `key`, the key of each element is what is compared, each
/// computed once. An empty iterable answers `default`, or raises `ValueError`.
fn emit_min_max_iterable(
    is_min: bool,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let name = if is_min { "min" } else { "max" };
    let mut positional: Vec<IRExpr> = Vec::new();
    let mut key = None;
    let mut default = None;
    for arg in arguments {
        match arg {
            IRExpr::Keyword { name: k, value } if k == "key" => key = Some(value.as_ref()),
            IRExpr::Keyword { name: k, value } if k == "default" => default = Some(value.as_ref()),
            IRExpr::Keyword { name: k, .. } => {
                ctx.report(format!("{name}() got an unexpected keyword argument '{k}'"));
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }
            other => positional.push(other.clone()),
        }
    }
    let iterable = match positional.len() {
        0 => {
            ctx.report(format!("{name} expected at least 1 argument, got 0"));
            func.instruction(&Instruction::I32Const(0));
            return IRType::Unknown;
        }
        1 => positional.remove(0),
        _ => {
            if default.is_some() {
                ctx.report(format!(
                    "Cannot specify a default for {name}() with multiple positional arguments"
                ));
            }
            IRExpr::TupleLiteral(positional)
        }
    };

    let iter_ty = emit_expr(&iterable, func, ctx, memory_layout, None);
    let iter_ty = emit_as_sequence(func, ctx, iter_ty);
    let elem = match &iter_ty {
        IRType::List(e) | IRType::VarTuple(e) => Some((e.as_ref().clone(), false)),
        IRType::Tuple(members) => match members.first() {
            Some(first) if members.iter().all(|m| m == first) => Some((first.clone(), false)),
            None => Some((IRType::Unknown, false)),
            _ => None,
        },
        IRType::Range => Some((IRType::Int, true)),
        _ => None,
    };
    let Some((elem, is_range)) = elem else {
        if matches!(iter_ty, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
        func.instruction(&Instruction::Drop);
        ctx.report(match &iter_ty {
            IRType::Tuple(_) => {
                format!("{name}() of a tuple whose members are of different types is not supported")
            }
            IRType::Set(_) | IRType::Dict(..) | IRType::String | IRType::Bytes => format!(
                "{name}() over a {} is not supported yet. Hint: pass a list or a tuple",
                crate::compiler::operators::python_type_name(&iter_ty)
            ),
            other => format!(
                "'{}' object is not iterable",
                crate::compiler::operators::python_type_name(other)
            ),
        });
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    };
    // An element of no known type is an untyped word, read as an int.
    let elem = match elem {
        IRType::Unknown | IRType::Any => IRType::Int,
        other => other,
    };
    let seq = crate::compiler::equality::hold(ctx);
    func.instruction(&Instruction::LocalSet(seq));

    // The key closure, called at its own signature.
    let key_call = match key {
        Some(k) => {
            let kt = emit_value(k, func, ctx, memory_layout, None);
            let env = crate::compiler::equality::hold(ctx);
            func.instruction(&Instruction::LocalSet(env));
            let (param, result) = match &kt {
                IRType::Callable {
                    params,
                    return_type,
                } if params.len() == 1 => (params[0].clone(), return_type.as_ref().clone()),
                IRType::Callable { params, .. } if params.is_empty() => {
                    (IRType::Unknown, IRType::Unknown)
                }
                other => {
                    ctx.report(format!(
                        "{name}()'s 'key' must be a function of one argument, not a '{}'",
                        crate::compiler::operators::python_type_name(other)
                    ));
                    (IRType::Unknown, IRType::Unknown)
                }
            };
            let word = |t: &IRType| matches!(t, IRType::Int | IRType::Bool | IRType::None);
            if matches!(param, IRType::Unknown) && !word(&elem) {
                ctx.report(format!(
                    "{name}()'s 'key' takes an untyped word, and the elements are '{}'. Hint: \
                     use a key whose parameter is typed (a lambda written in the call is typed \
                     from the elements)",
                    crate::type_to_string(&elem)
                ));
            }
            if matches!(elem, IRType::Float) && !matches!(param, IRType::Float) {
                ctx.report(format!(
                    "{name}()'s 'key' takes a '{}' where the elements are floats",
                    crate::type_to_string(&param)
                ));
            }
            let result = match result {
                IRType::Unknown => IRType::Int,
                other => other,
            };
            let type_index = {
                let wanted = (
                    vec![wasm_value_type(&param), ValType::I32],
                    wasm_value_type(&result),
                );
                match ctx.signature_types.get(&wanted) {
                    Some(index) => *index,
                    None => {
                        ctx.report(format!(
                            "{name}()'s 'key' has no signature this module defines; this is a \
                             code generation bug"
                        ));
                        0
                    }
                }
            };
            Some((env, param, result, type_index))
        }
        None => None,
    };
    let cmp_ty = match &key_call {
        Some((_, _, result, _)) => result.clone(),
        None => elem.clone(),
    };
    if !matches!(cmp_ty, IRType::Float) {
        if let Some(why) = crate::compiler::equality::order_unsupported(ctx, &cmp_ty) {
            ctx.report(format!("{name}() cannot order {why}"));
        }
    }

    // `default`, evaluated with the other arguments as CPython does.
    let default_slot = match default {
        Some(d) => {
            let dt = emit_value(d, func, ctx, memory_layout, Some(&elem));
            match (&dt, &elem) {
                (a, b) if a == b => {}
                (IRType::Bool, IRType::Int) => {}
                _ => ctx.report(format!(
                    "the default of {name}() is a '{}' where the elements are '{}', so its \
                     answer would have two types. Hint: give a default of the elements' type",
                    crate::type_to_string(&dt),
                    crate::type_to_string(&elem)
                )),
            }
            if matches!(dt, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
            if matches!(elem, IRType::Float) && !matches!(dt, IRType::Float) {
                func.instruction(&Instruction::F64ConvertI32S);
            }
            let slot = hold_value(ctx, &elem);
            func.instruction(&Instruction::LocalSet(slot));
            Some(slot)
        }
        None => None,
    };

    let n = crate::compiler::equality::hold(ctx);
    let i = crate::compiler::equality::hold(ctx);
    let best = hold_value(ctx, &elem);
    let cand = hold_value(ctx, &elem);
    let keys = key_call
        .as_ref()
        .map(|(_, _, result, _)| (hold_value(ctx, result), hold_value(ctx, result)));

    // Element `i` (a local) of the sequence, at the element's width; a string
    // is its offset, the length coming back from its prefix at the end.
    let load_element = |func: &mut Function, index: Option<u32>| {
        if is_range {
            func.instruction(&Instruction::LocalGet(seq));
            func.instruction(&Instruction::I32Load(mem_off(0)));
            if let Some(index) = index {
                func.instruction(&Instruction::LocalGet(index));
                func.instruction(&Instruction::LocalGet(seq));
                func.instruction(&Instruction::I32Load(mem_off(8)));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
            }
            return;
        }
        func.instruction(&Instruction::LocalGet(seq));
        emit_data_base(func);
        if let Some(index) = index {
            func.instruction(&Instruction::LocalGet(index));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
        }
        if matches!(elem, IRType::Float) {
            func.instruction(&Instruction::F64Load(slot_arg()));
        } else {
            func.instruction(&Instruction::I32Load(slot_arg()));
        }
    };
    // key(element in `from`), into `into`.
    let call_key = |func: &mut Function, from: u32, into: u32| {
        let Some((env, param, _, type_index)) = &key_call else {
            return;
        };
        func.instruction(&Instruction::LocalGet(from));
        if matches!(param, IRType::Float) && !matches!(elem, IRType::Float) {
            func.instruction(&Instruction::F64ConvertI32S);
        }
        func.instruction(&Instruction::LocalGet(*env));
        func.instruction(&Instruction::LocalGet(*env));
        func.instruction(&Instruction::I32Load(slot_arg()));
        let can_raise = !ctx.can_raise.is_empty();
        if can_raise {
            emit_call_depth_step(func, 1);
        }
        func.instruction(&Instruction::CallIndirect {
            type_index: *type_index,
            table_index: 0,
        });
        if can_raise {
            emit_call_depth_step(func, -1);
            emit_post_call_check(func, ctx);
        }
        func.instruction(&Instruction::LocalSet(into));
    };

    emit_comp_iterable_len(func, ctx, is_range, seq);
    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    ctx.enter_expr_blocks(1);
    match default_slot {
        Some(slot) => {
            func.instruction(&Instruction::LocalGet(slot));
            func.instruction(&Instruction::LocalSet(best));
        }
        None => {
            let message = format!("{name}() iterable argument is empty");
            emit_raise(
                func,
                ctx,
                "ValueError",
                crate::compiler::exceptions::ExcArg::Text(&message),
                0,
            )
        }
    }
    func.instruction(&Instruction::Else);
    load_element(func, None);
    func.instruction(&Instruction::LocalSet(best));
    if let Some((best_key, _)) = keys {
        call_key(func, best, best_key);
    }
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    ctx.enter_expr_blocks(2);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    load_element(func, Some(i));
    func.instruction(&Instruction::LocalSet(cand));
    let (a, b) = match keys {
        Some((best_key, cand_key)) => {
            call_key(func, cand, cand_key);
            (cand_key, best_key)
        }
        None => (cand, best),
    };
    let op = if is_min {
        IRCompareOp::Lt
    } else {
        IRCompareOp::Gt
    };
    match &cmp_ty {
        IRType::Float => {
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&if is_min {
                Instruction::F64Lt
            } else {
                Instruction::F64Gt
            });
        }
        other => crate::compiler::equality::emit_values_order(func, ctx, other, a, b, &op),
    }
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(cand));
    func.instruction(&Instruction::LocalSet(best));
    if let Some((best_key, cand_key)) = keys {
        func.instruction(&Instruction::LocalGet(cand_key));
        func.instruction(&Instruction::LocalSet(best_key));
    }
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    ctx.exit_expr_blocks(2);
    func.instruction(&Instruction::End);
    ctx.exit_expr_blocks(1);

    func.instruction(&Instruction::LocalGet(best));
    if matches!(elem, IRType::String | IRType::Bytes) {
        recover_str_pair(func, ctx);
    }

    if let Some((_, _, result, _)) = &key_call {
        release_value(ctx, result);
        release_value(ctx, result);
    }
    release_value(ctx, &elem);
    release_value(ctx, &elem);
    ctx.release_held(); // i
    ctx.release_held(); // n
    if default_slot.is_some() {
        release_value(ctx, &elem);
    }
    if key_call.is_some() {
        ctx.release_held(); // env
    }
    ctx.release_held(); // seq
    elem
}

fn emit_min_max(
    func: &mut Function,
    ctx: &CompilationContext,
    is_min: bool,
    arg_types: &[IRType],
) -> IRType {
    let name = if is_min { "min" } else { "max" };
    let int_like = |t: &IRType| matches!(t, IRType::Int | IRType::Unknown);
    let kind = match arg_types.first() {
        None => Err(format!("{name}() expects at least 1 argument, got 0")),
        _ if arg_types.len() > MIN_MAX_MAX_ARGS => Err(format!(
            "{name}() with more than {MIN_MAX_MAX_ARGS} arguments is not supported"
        )),
        Some(t) if int_like(t) && arg_types.iter().all(int_like) => Ok(IRType::Int),
        Some(IRType::Bool) if arg_types.iter().all(|t| *t == IRType::Bool) => Ok(IRType::Bool),
        Some(IRType::Float) if arg_types.iter().all(|t| *t == IRType::Float) => Ok(IRType::Float),
        Some(IRType::String) if arg_types.iter().all(|t| *t == IRType::String) => {
            Ok(IRType::String)
        }
        _ => Err(format!(
            "{name}() of {} values is not supported: the arguments must all be ints, all \
             floats, or all strings",
            arg_types
                .iter()
                .map(crate::compiler::operators::python_type_name)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    };
    let result_type = match kind {
        Ok(t) => t,
        Err(message) => {
            ctx.report(message);
            for t in arg_types.iter().rev() {
                func.instruction(&Instruction::Drop);
                if matches!(t, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
            }
            func.instruction(&Instruction::I32Const(0));
            return IRType::Unknown;
        }
    };

    let is_float = result_type == IRType::Float;
    let slot = |i: usize| ctx.temp_local + 20 + i as u32;
    // Park the arguments, last (top of the stack) first; a string keeps only
    // its offset, since its length is in the blob's prefix.
    for i in (0..arg_types.len()).rev() {
        if result_type == IRType::String {
            func.instruction(&Instruction::Drop);
        }
        if is_float {
            func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
            func.instruction(&Instruction::I64ReinterpretF64);
            func.instruction(&Instruction::I32WrapI64);
            func.instruction(&Instruction::LocalSet(slot(2 * i)));
            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
            func.instruction(&Instruction::I64ReinterpretF64);
            func.instruction(&Instruction::I64Const(32));
            func.instruction(&Instruction::I64ShrU);
            func.instruction(&Instruction::I32WrapI64);
            func.instruction(&Instruction::LocalSet(slot(2 * i + 1)));
        } else {
            func.instruction(&Instruction::LocalSet(slot(i)));
        }
    }
    let load_f64 = |func: &mut Function, i: usize| {
        func.instruction(&Instruction::LocalGet(slot(2 * i)));
        func.instruction(&Instruction::I64ExtendI32U);
        func.instruction(&Instruction::LocalGet(slot(2 * i + 1)));
        func.instruction(&Instruction::I64ExtendI32U);
        func.instruction(&Instruction::I64Const(32));
        func.instruction(&Instruction::I64Shl);
        func.instruction(&Instruction::I64Or);
        func.instruction(&Instruction::F64ReinterpretI64);
    };

    // best = arg0; for each later x: if x < best (x > best for max): best = x
    let best = ctx.temp_local + 19;
    if is_float {
        load_f64(func, 0);
        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
    } else {
        func.instruction(&Instruction::LocalGet(slot(0)));
        func.instruction(&Instruction::LocalSet(best));
    }
    for i in 1..arg_types.len() {
        if is_float {
            load_f64(func, i);
            func.instruction(&Instruction::LocalTee(ctx.temp_local_f64));
            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
            func.instruction(&if is_min {
                Instruction::F64Lt
            } else {
                Instruction::F64Gt
            });
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
            func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
            func.instruction(&Instruction::End);
            continue;
        }
        if result_type == IRType::String {
            crate::compiler::equality::emit_str_cmp(func, ctx, slot(i), best);
            func.instruction(&Instruction::I32Const(0));
        } else {
            func.instruction(&Instruction::LocalGet(slot(i)));
            func.instruction(&Instruction::LocalGet(best));
        }
        func.instruction(&if is_min {
            Instruction::I32LtS
        } else {
            Instruction::I32GtS
        });
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(slot(i)));
        func.instruction(&Instruction::LocalSet(best));
        func.instruction(&Instruction::End);
    }
    if is_float {
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
    } else {
        func.instruction(&Instruction::LocalGet(best));
        if result_type == IRType::String {
            recover_str_pair(func, ctx);
        }
    }
    result_type
}

/// The dotted name of a standard-library module or submodule a method is
/// called on (`json`, `os.path`, `datetime.datetime`), unless a local of that
/// name shadows the module.
fn stdlib_call_name(ctx: &CompilationContext, object: &IRExpr) -> Option<String> {
    match object {
        IRExpr::Variable(module)
            if crate::stdlib::is_stdlib_module(module) && ctx.get_local_info(module).is_none() =>
        {
            Some(module.clone())
        }
        IRExpr::Attribute { object, attribute } => {
            stdlib_call_name(ctx, object).map(|parent| format!("{parent}.{attribute}"))
        }
        _ => None,
    }
}

/// Resolve a bare name used as a call/attribute receiver to a class, when it
/// statically denotes one: either a class's own name (`Counter.create()`), or
/// `cls` inside a classmethod, whose parameter is typed as the defining class
/// during IR conversion. Dispatch through the result is static, consistent
/// with the rest of the object model (no vtables).
/// An attribute of a class object (its id on the stack): its name, or a
/// class variable of the class it is, chosen by id when a subclass of the
/// static class defines its own.
fn emit_class_object_attribute(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    class_name: &str,
    attribute: &str,
) -> IRType {
    match attribute {
        "__name__" | "__qualname__" => {
            func.instruction(&Instruction::Call(
                ctx.helper(crate::compiler::runtime::Helper::ClassNameOf { repr: false }),
            ));
            recover_str_pair(func, ctx);
            IRType::String
        }
        _ if ctx
            .get_class_info(class_name)
            .is_some_and(|ci| ci.class_var_owner.contains_key(attribute)) =>
        {
            emit_class_var_by_tag(func, ctx, memory_layout, class_name, attribute, false)
        }
        _ => {
            func.instruction(&Instruction::Drop);
            ctx.report(format!(
                "reading '{attribute}' of the class object of '{class_name}' is not supported: \
                 only its name and its class variables are"
            ));
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}

/// `obj.var` where `var` is a class variable and not an attribute of the
/// instance: the variable of the instance's class, the instance pointer on
/// the stack. When some subclass of the static class defines its own, the
/// class tag picks which.
fn emit_class_var_through_instance(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    class_name: &str,
    var: &str,
) -> IRType {
    emit_class_var_by_tag(func, ctx, memory_layout, class_name, var, true)
}

/// A class variable of the class on the stack: an instance (`instance`, its
/// tag names the class) or a class object (the id itself).
fn emit_class_var_by_tag(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    class_name: &str,
    var: &str,
    instance: bool,
) -> IRType {
    let read = |owner: &str| IRExpr::Attribute {
        object: Box::new(IRExpr::Variable(owner.to_string())),
        attribute: var.to_string(),
    };
    // Each class the instance may be, by the class it reads `var` from.
    let mut owners: Vec<(String, Vec<i32>)> = Vec::new();
    let mut classes: Vec<&crate::compiler::context::ClassInfo> = ctx
        .class_map
        .values()
        .filter(|c| ctx.is_class_or_subclass(&c.name, class_name))
        .collect();
    classes.sort_by_key(|c| c.class_id);
    for class in classes {
        let Some(owner) = class.class_var_owner.get(var) else {
            continue;
        };
        match owners.iter_mut().find(|(o, _)| o == owner) {
            Some((_, ids)) => ids.push(class.class_id),
            None => owners.push((owner.clone(), vec![class.class_id])),
        }
    }
    if owners.len() <= 1 {
        func.instruction(&Instruction::Drop);
        let owner = owners
            .first()
            .map(|(o, _)| o.clone())
            .unwrap_or_else(|| class_name.to_string());
        return emit_expr(&read(&owner), func, ctx, memory_layout, None);
    }
    let Some(object) = ctx.hold() else {
        report_literal_too_deep(func, ctx, "a class variable read");
        return IRType::Unknown;
    };
    func.instruction(&Instruction::LocalSet(object));
    // The type every owner's value must share, from a dry run of the first.
    let (ty, _) = probe_type(&read(&owners[0].0), ctx, memory_layout, None);
    let word = match ty {
        IRType::Float => ValType::F64,
        _ => ValType::I32,
    };
    let (last, rest) = owners.split_last().expect("two or more owners");
    for (owner, ids) in rest {
        for (k, id) in ids.iter().enumerate() {
            func.instruction(&Instruction::LocalGet(object));
            if instance {
                func.instruction(&Instruction::I32Load(slot_arg()));
            }
            func.instruction(&Instruction::I32Const(*id));
            func.instruction(&Instruction::I32Eq);
            if k > 0 {
                func.instruction(&Instruction::I32Or);
            }
        }
        func.instruction(&Instruction::If(BlockType::Result(word)));
        ctx.enter_expr_blocks(1);
        emit_class_var_word(func, ctx, memory_layout, &read(owner), &ty, var);
        func.instruction(&Instruction::Else);
    }
    emit_class_var_word(func, ctx, memory_layout, &read(&last.0), &ty, var);
    for _ in rest {
        func.instruction(&Instruction::End);
    }
    ctx.exit_expr_blocks(rest.len() as u32);
    ctx.release_held();
    if matches!(ty, IRType::String | IRType::Bytes) {
        recover_str_pair(func, ctx);
    }
    ty
}

/// One owner's class variable as a single word (a string's offset), checked
/// to have the type the other owners' have.
fn emit_class_var_word(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    read: &IRExpr,
    ty: &IRType,
    var: &str,
) {
    let got = emit_expr(read, func, ctx, memory_layout, Some(ty));
    if matches!(got, IRType::String | IRType::Bytes) {
        func.instruction(&Instruction::Drop);
    }
    if got != *ty {
        ctx.report(format!(
            "the class variable '{var}' is a '{}' in one class and a '{}' in another, which \
             one read through an instance cannot give",
            crate::type_to_string(ty),
            crate::type_to_string(&got)
        ));
    }
}

/// The global holding class variable `var` as `class` sees it: the one of
/// the first class along its MRO that has one.
pub(crate) fn class_var_global(ctx: &CompilationContext, class: &str, var: &str) -> Option<String> {
    let mro = ctx.get_class_info(class)?.mro.clone();
    mro.iter()
        .map(|c| format!("{c}.{var}"))
        .find(|key| ctx.module_global_index.contains_key(key))
}

pub(crate) fn static_class_target(ctx: &CompilationContext, name: &str) -> Option<String> {
    if ctx.get_class_info(name).is_some() {
        return Some(name.to_string());
    }
    if name == "cls" {
        if let Some(IRType::Class(class_name)) = ctx.get_local_info("cls").map(|i| &i.var_type) {
            return Some(class_name.clone());
        }
    }
    None
}

/// Emit a method call addressed through a class rather than an instance:
/// `ClassName.method(...)`, or `cls.method(...)` inside a classmethod. A
/// `@staticmethod` takes only the explicit arguments; a `@classmethod` gets the
/// class id pushed as its implicit `cls`; a plain method called this way is
/// Python's unbound form, where the caller passes the instance explicitly.
fn emit_class_level_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    class_name: &str,
    method_name: &str,
    arguments: &[IRExpr],
) -> IRType {
    let Some(class_info) = ctx.get_class_info(class_name) else {
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    };
    let Some(method_idx) = class_info.methods.get(method_name).copied() else {
        // Unknown method on a known class: evaluate nothing, yield 0.
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    };
    let kind = class_info
        .method_kinds
        .get(method_name)
        .copied()
        .unwrap_or(MethodKind::Instance);
    // The id of the class the call is addressed through (not the defining
    // base), so a classmethod inherited by a subclass sees the subclass's id.
    let class_id = class_info.class_id;
    let owner = class_info
        .method_owner
        .get(method_name)
        .cloned()
        .unwrap_or_else(|| class_name.to_string());
    let (param_types, ret) = ctx
        .get_function_info(&format!("{owner}::{method_name}"))
        .map(|f| (f.param_types.clone(), f.return_type.clone()))
        .unwrap_or((Vec::new(), IRType::Unknown));

    // Explicit arguments map onto the parameter list after any implicit one.
    let arg_base = match kind {
        MethodKind::Class => {
            func.instruction(&Instruction::I32Const(class_id));
            1
        }
        _ => 0,
    };
    for (i, arg) in arguments.iter().enumerate() {
        let t = emit_expr(arg, func, ctx, memory_layout, param_types.get(i + arg_base));
        check_class_arg(
            ctx,
            &t,
            param_types.get(i + arg_base),
            i,
            &format!("{class_name}.{method_name}"),
        );
        // Narrow a string/bytes argument to its offset word, matching the
        // calling convention used at instantiation sites.
        if matches!(t, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
    }
    emit_user_call(func, ctx, method_idx);
    // A call result is a single word; rebuild the string/bytes pair.
    finish_user_call(func, ctx, &ret);
    ret
}

// Helper to convert f64 to Ieee64
#[inline]
fn f64_const(value: f64) -> wasm_encoder::Ieee64 {
    value.into()
}

/// Which per-character transform a string method applies.
#[derive(Clone, Copy, PartialEq)]
enum CaseMode {
    Upper,
    Lower,
    Capitalize,
    Title,
}

/// Which ends a strip-family method trims.
#[derive(Clone, Copy, PartialEq)]
enum TrimMode {
    Both,
    Left,
    Right,
}

/// `str.upper()` / `lower()` / `capitalize()` / `title()` at runtime.
///
/// Entry stack: `(offset, length)`. Exit stack: `(offset, length)` of a new
/// string; the receiver is never mutated, as strings are immutable. The whole
/// of Unicode is mapped as CPython maps it (`compiler::unicode`), final sigma
/// and titlecase included. Only ASCII used to be, and a byte at or above
/// 0x80 trapped.
fn emit_string_case(func: &mut Function, ctx: &CompilationContext, mode: CaseMode) {
    use crate::compiler::unicode::Case;
    let case = match mode {
        CaseMode::Upper => Case::Upper,
        CaseMode::Lower => Case::Lower,
        CaseMode::Capitalize => Case::Capitalize,
        CaseMode::Title => Case::Title,
    };
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::Call(
        ctx.helper(crate::compiler::runtime::Helper::Case(case)),
    ));
    recover_str_pair(func, ctx);
}

/// `str.strip()` / `lstrip()` / `rstrip()`, with or without a cut set.
///
/// Entry stack: `(offset, length)` and, when `has_chars`, the cut set's
/// `(offset, length)` above it. Exit stack: `(offset, length)` of a fresh block
/// holding the trimmed bytes.
fn emit_string_trim(
    func: &mut Function,
    ctx: &CompilationContext,
    mode: TrimMode,
    has_chars: bool,
) {
    let off = ctx.temp_local;
    let len = ctx.temp_local + 1;
    let start = ctx.temp_local + 2;
    let blk = ctx.temp_local + 3;
    let b = ctx.temp_local + 4;
    let end = ctx.temp_local + 5;
    let newlen = ctx.temp_local + 6;
    let chars_off = ctx.temp_local + 7;
    let chars_len = ctx.temp_local + 8;

    if has_chars {
        func.instruction(&Instruction::LocalSet(chars_len));
        func.instruction(&Instruction::LocalSet(chars_off));
    }
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));
    // The cut set's bytes, or none: whitespace as CPython's isspace has it.
    let cut_set = |func: &mut Function| {
        if has_chars {
            func.instruction(&Instruction::LocalGet(chars_off));
            func.instruction(&Instruction::LocalGet(chars_len));
        } else {
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32Const(0));
        }
    };

    // start: advance while the character is trimmed (left-trimming modes only).
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));
    if mode != TrimMode::Right {
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::LocalGet(len));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        // The width of the character at start if it is trimmed, or 0.
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(start));
        cut_set(func);
        func.instruction(&Instruction::Call(ctx.helper(
            crate::compiler::runtime::Helper::TrimAt {
                set: has_chars,
                before: false,
            },
        )));
        func.instruction(&Instruction::LocalTee(b));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::LocalGet(b));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(start));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    // end: retreat while the byte before it is trimmable (right-trimming only).
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalSet(end));
    if mode != TrimMode::Left {
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32LeU);
        func.instruction(&Instruction::BrIf(1));
        // The width of the character ending at end if it is trimmed, or 0.
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(end));
        cut_set(func);
        func.instruction(&Instruction::Call(ctx.helper(
            crate::compiler::runtime::Helper::TrimAt {
                set: has_chars,
                before: true,
            },
        )));
        func.instruction(&Instruction::LocalTee(b));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::LocalGet(b));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(end));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }

    // newlen = end - start
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(newlen));

    // block = __alloc(4 + newlen + 1), copy the kept range in.
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::LocalGet(newlen));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(blk));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(newlen));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(blk));

    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(start));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(newlen));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(newlen));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(newlen));
}

/// MemArg for a collection slot access. Slots sit at 4-byte alignment (the count
/// header is 4 bytes and slots are 8), so `align: 2` is the honest hint for both
/// i32 and f64 accesses; WASM treats alignment as advisory only, so an f64 here
/// is valid despite not being 8-byte aligned.
pub(crate) fn slot_arg() -> MemArg {
    MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }
}

/// Point a runtime-built region's data pointer at the block right after its
/// header, the layout every collection starts life in.
pub(crate) fn store_runtime_data_ptr(func: &mut Function, ptr_local: u32) {
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_DATA as u64)));
}

/// Replace a collection pointer on top of the stack with the address of its
/// first element.
pub(crate) fn emit_data_base(func: &mut Function) {
    func.instruction(&Instruction::I32Load(mem_off(COLLECTION_DATA as u64)));
}

/// Normalize the position in `index_local` against the length in `len_local`
/// and clamp it into `0..=len`, the way slicing and `list.insert` treat a
/// position: a negative one counts from the end, and one still out of range is
/// pinned to the nearer end rather than raising (item access raises instead).
///
/// Branchless, through `select`, and stack-neutral. It reads and writes only
/// the two locals it is given and claims no scratch locals of its own, so the
/// caller decides which offsets those are.
fn emit_normalize_and_clamp(func: &mut Function, index_local: u32, len_local: u32) {
    // `select` pops (v1, v2, cond) and yields v1 when cond != 0.
    // index = index < 0 ? index + len : index
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::LocalGet(len_local));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(index_local));
    // index = max(index, 0)
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(index_local));
    // index = min(index, len)
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::LocalGet(len_local));
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::LocalGet(len_local));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(index_local));
}

/// Write the capacity word of a region whose pointer is in `ptr_local` and
/// whose capacity is in `cap_local`, for regions built at runtime (`__alloc`
/// blocks: comprehension results, grown lists, unpacked slices).
fn store_runtime_cap(func: &mut Function, ptr_local: u32, cap_local: u32) {
    store_runtime_data_ptr(func, ptr_local);
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::LocalGet(cap_local));
    func.instruction(&Instruction::I32Store(MemArg {
        offset: COLLECTION_CAP as u64,
        align: 2,
        memory_index: 0,
    }));
}

/// Collections reserve one [`COLLECTION_SLOT`]-byte slot per element, but strings
/// and bytes leave two values on the stack (offset, length). After emitting such
/// an element, drop the length so only the offset remains; identical string
/// literals are interned to the same offset, so offset comparison preserves
/// value equality.
pub(crate) fn narrow_element_to_word(func: &mut Function, elem_type: &IRType) {
    if matches!(elem_type, IRType::String | IRType::Bytes) {
        func.instruction(&Instruction::Drop);
    }
}

/// Store the value on top of the stack into a collection slot (the destination
/// address must be pushed first). Floats are stored as a full `f64` so they
/// round-trip without loss; everything else is an i32 word in the slot's low 4
/// bytes (ints, bools, interned string/bytes offsets, collection pointers). The
/// element must already be narrowed to a single word (see
/// [`narrow_element_to_word`]) for string/bytes.
fn store_collection_word(func: &mut Function, elem_type: &IRType) {
    if matches!(elem_type, IRType::Float) {
        func.instruction(&Instruction::F64Store(slot_arg()));
    } else {
        func.instruction(&Instruction::I32Store(slot_arg()));
    }
}

/// [`store_collection_word`] at a constant byte offset from the address on the
/// stack, for a literal being filled through its block pointer.
fn store_collection_word_at(func: &mut Function, elem_type: &IRType, offset: u32) {
    if matches!(elem_type, IRType::Float) {
        func.instruction(&Instruction::F64Store(mem_off(offset as u64)));
    } else {
        func.instruction(&Instruction::I32Store(mem_off(offset as u64)));
    }
}

/// Allocate a fresh, zeroed block of `size` bytes for a collection literal and
/// leave its pointer in `blk`. Every evaluation of a literal gets its own block,
/// as every evaluation of a Python literal is a new object.
fn emit_literal_block(func: &mut Function, ctx: &CompilationContext, blk: u32, size: u32) {
    func.instruction(&Instruction::I32Const(((size + 7) & !7) as i32));
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(blk));
}

/// Write a list-layout header (`[len][cap][data -> blk + HEADER]`) into the
/// block in `blk`.
fn emit_literal_header(func: &mut Function, blk: u32, len: u32) {
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(len as i32));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(len as i32));
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));
    store_runtime_data_ptr(func, blk);
}

/// Report a literal nested deeper than the held slots allow, leaving a
/// placeholder pointer so the module stays valid while the error is collected.
fn report_literal_too_deep(func: &mut Function, ctx: &CompilationContext, kind: &str) {
    ctx.report(format!(
        "a {kind} literal is nested inside more collection literals and method calls \
         than the compiler reserves room for. Hint: build the inner parts into variables first"
    ));
    func.instruction(&Instruction::I32Const(0));
}

/// Load a collection slot (address on top of the stack) as a runtime value.
///
/// Floats are loaded as the full `f64` stored by [`store_collection_word`].
/// String/bytes slots hold only the value's offset, so the companion length is
/// recovered from the blob's length prefix (`load(offset - STRING_LEN_PREFIX)`)
/// and the `(offset, length)` pair the rest of the compiler expects is rebuilt;
/// `scratch` is an i32 scratch local used to hold the offset while doing so.
/// Everything else is a plain i32 from the slot's low word.
fn load_collection_word(func: &mut Function, elem_type: &IRType, scratch: u32) {
    match elem_type {
        IRType::Float => {
            func.instruction(&Instruction::F64Load(slot_arg()));
        }
        IRType::String | IRType::Bytes => {
            // Slot holds the offset; rebuild (offset, length).
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalTee(scratch)); // keep offset, save it
            func.instruction(&Instruction::LocalGet(scratch));
            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Load(slot_arg()));
        }
        _ => {
            func.instruction(&Instruction::I32Load(slot_arg()));
        }
    }
}

/// Emit `expr` where its value is used. A call that returns None pushes
/// nothing (a statement drops nothing after it), so where the value is wanted
/// None is pushed as its word, 0.
pub(crate) fn emit_value(
    expr: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected_type: Option<&IRType>,
) -> IRType {
    let ty = emit_expr(expr, func, ctx, memory_layout, expected_type);
    if ty == IRType::None
        && matches!(
            expr,
            IRExpr::FunctionCall { .. } | IRExpr::MethodCall { .. } | IRExpr::Conditional { .. }
        )
    {
        func.instruction(&Instruction::I32Const(0));
    }
    ty
}

/// The type `arm` emits, found by emitting it into a scratch function and
/// discarding the code and anything it reported. The flag says whether it
/// reported anything.
fn probe_type(
    arm: &IRExpr,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected_type: Option<&IRType>,
) -> (IRType, bool) {
    let reported = ctx.errors.borrow().len();
    let ty = emit_value(
        arm,
        &mut Function::new([]),
        ctx,
        memory_layout,
        expected_type,
    );
    let failed = ctx.errors.borrow().len() > reported;
    ctx.errors.borrow_mut().truncate(reported);
    (ty, failed)
}

/// The one type both arms of a conditional expression are read as, if there
/// is one. CPython keeps each arm's own type, but here a single value holds
/// whichever arm ran, so the arms must agree on how it reads: two classes share
/// their nearest common base, a class and None make an Optional, an empty
/// collection takes the other arm's element type, and an int widens only where
/// a float is expected (as it does when stored into a float place).
fn conditional_type(
    ctx: &CompilationContext,
    body: &IRType,
    orelse: &IRType,
    expected: Option<&IRType>,
) -> Option<IRType> {
    fn element(a: &IRType, b: &IRType) -> Option<IRType> {
        match (a, b) {
            (IRType::Unknown, t) | (t, IRType::Unknown) => Some(t.clone()),
            (a, b) if a == b => Some(a.clone()),
            _ => None,
        }
    }
    let class_of = |t: &IRType| match t {
        IRType::Class(c) => Some(c.clone()),
        IRType::Optional(inner) => match inner.as_ref() {
            IRType::Class(c) => Some(c.clone()),
            _ => None,
        },
        _ => None,
    };
    if body == orelse {
        return Some(body.clone());
    }
    match (body, orelse) {
        (IRType::Int, IRType::Float) | (IRType::Float, IRType::Int)
            if expected == Some(&IRType::Float) =>
        {
            Some(IRType::Float)
        }
        (IRType::Unknown, IRType::Int | IRType::Bool | IRType::None)
        | (IRType::Int | IRType::Bool | IRType::None, IRType::Unknown) => Some(IRType::Unknown),
        // None beside a number, or an optional one, is an optional number.
        (IRType::None, other) | (other, IRType::None)
            if matches!(other, IRType::Int | IRType::Bool | IRType::Float)
                || boxed_scalar(other).is_some() =>
        {
            Some(match boxed_scalar(other) {
                Some(_) => other.clone(),
                None => IRType::Optional(Box::new(other.clone())),
            })
        }
        (IRType::Optional(a), b) | (b, IRType::Optional(a))
            if boxed_scalar(&IRType::Optional(a.clone())).is_some() && **a == *b =>
        {
            Some(IRType::Optional(a.clone()))
        }
        (IRType::None, other) | (other, IRType::None) => {
            class_of(other).map(|c| IRType::Optional(Box::new(IRType::Class(c))))
        }
        (IRType::Class(_) | IRType::Optional(_), IRType::Class(_) | IRType::Optional(_)) => {
            let base = IRType::Class(ctx.common_base(&class_of(body)?, &class_of(orelse)?)?);
            if matches!(body, IRType::Optional(_)) || matches!(orelse, IRType::Optional(_)) {
                Some(IRType::Optional(Box::new(base)))
            } else {
                Some(base)
            }
        }
        (IRType::ClassRef(a), IRType::ClassRef(b)) => ctx.common_base(a, b).map(IRType::ClassRef),
        (IRType::List(a), IRType::List(b)) => element(a, b).map(|e| IRType::List(Box::new(e))),
        (IRType::VarTuple(a), IRType::VarTuple(b)) => {
            element(a, b).map(|e| IRType::VarTuple(Box::new(e)))
        }
        (IRType::Set(a), IRType::Set(b)) => element(a, b).map(|e| IRType::Set(Box::new(e))),
        (IRType::Dict(ka, va), IRType::Dict(kb, vb)) => Some(IRType::Dict(
            Box::new(element(ka, kb)?),
            Box::new(element(va, vb)?),
        )),
        _ => None,
    }
}

/// `body if condition else orelse`: a WASM `if` whose arms each leave the
/// shared type's value. A string's `(offset, length)` pair is two words, which
/// a single-result block cannot carry, so it travels through held slots.
fn emit_conditional(
    condition: &IRExpr,
    body: &IRExpr,
    orelse: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected_type: Option<&IRType>,
) -> IRType {
    let (body_ty, body_failed) = probe_type(body, ctx, memory_layout, expected_type);
    let (orelse_ty, orelse_failed) = probe_type(orelse, ctx, memory_layout, expected_type);
    if body_failed || orelse_failed {
        // Report what the arms report, once; the module is not built.
        let mut scratch = Function::new([]);
        emit_value(body, &mut scratch, ctx, memory_layout, expected_type);
        emit_value(orelse, &mut scratch, ctx, memory_layout, expected_type);
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    }
    let Some(ty) = conditional_type(ctx, &body_ty, &orelse_ty, expected_type) else {
        ctx.report(format!(
            "the arms of this conditional expression have different types ({} and {}), \
             and the value it produces can only be read as one of them. Hint: make both \
             arms the same type, or use an if/else statement",
            crate::type_to_string(&body_ty),
            crate::type_to_string(&orelse_ty)
        ));
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    };

    let pair = matches!(ty, IRType::String | IRType::Bytes);
    let slots = if pair {
        let Some(offset) = ctx.hold() else {
            ctx.report("a conditional expression is nested deeper than the compiler supports");
            func.instruction(&Instruction::I32Const(0));
            return IRType::Unknown;
        };
        let Some(length) = ctx.hold() else {
            ctx.release_held();
            ctx.report("a conditional expression is nested deeper than the compiler supports");
            func.instruction(&Instruction::I32Const(0));
            return IRType::Unknown;
        };
        Some((offset, length))
    } else {
        None
    };

    let cond_ty = emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
    emit_truthiness(func, ctx, &cond_ty);
    func.instruction(&Instruction::If(match ty {
        IRType::Float => BlockType::Result(ValType::F64),
        IRType::None | IRType::String | IRType::Bytes => BlockType::Empty,
        _ => BlockType::Result(ValType::I32),
    }));
    ctx.enter_expr_blocks(1);
    for (i, arm) in [body, orelse].into_iter().enumerate() {
        if i == 1 {
            func.instruction(&Instruction::Else);
        }
        // An optional number's arms are boxed where they are numbers.
        let expected = if ty == IRType::Float || boxed_scalar(&ty).is_some() {
            Some(&ty)
        } else {
            expected_type
        };
        let arm_ty = emit_value(arm, func, ctx, memory_layout, expected);
        match (&ty, &arm_ty) {
            (IRType::None, _) => {
                func.instruction(&Instruction::Drop);
            }
            (IRType::Float, IRType::Int | IRType::Bool) => {
                func.instruction(&Instruction::F64ConvertI32S);
            }
            _ => {}
        }
        if let Some((offset, length)) = slots {
            func.instruction(&Instruction::LocalSet(length));
            func.instruction(&Instruction::LocalSet(offset));
        }
    }
    ctx.exit_expr_blocks(1);
    func.instruction(&Instruction::End);
    if let Some((offset, length)) = slots {
        func.instruction(&Instruction::LocalGet(offset));
        func.instruction(&Instruction::LocalGet(length));
        ctx.release_held();
        ctx.release_held();
    }
    ty
}

/// Shape the word a user function or method returns into the value its
/// declared type reads as: a string's `(offset, length)` pair, or nothing for
/// a function declared `-> None`, whose call is a statement like any other
/// None-valued call. Its word used to be left behind, so calling one inside a
/// loop failed validation.
fn finish_user_call(func: &mut Function, ctx: &CompilationContext, ret: &IRType) {
    match ret {
        IRType::String | IRType::Bytes => recover_str_pair(func, ctx),
        IRType::None => {
            func.instruction(&Instruction::Drop);
        }
        _ => {}
    }
}

/// The value a call by `name` goes through when `name` is no function of the
/// module but a variable holding a closure: a local, the cell of a local a
/// closure captures, or a module definition. Returns it with its static type.
fn closure_callee(name: &str, ctx: &CompilationContext) -> Option<(IRExpr, IRType)> {
    if let Some(info) = ctx.get_local_info(name) {
        return Some((IRExpr::Variable(name.to_string()), info.var_type.clone()));
    }
    let cell = crate::ir::cell_local(name);
    if ctx.get_local_index(&cell).is_some() {
        let ty = ctx
            .cell_types
            .borrow()
            .get(&cell)
            .cloned()
            .unwrap_or(IRType::Unknown);
        return Some((IRExpr::CellLoad { cell }, ty));
    }
    ctx.get_module_var(name)?;
    let ty = ctx
        .module_global_types
        .get(name)
        .cloned()
        .unwrap_or(IRType::Unknown);
    Some((IRExpr::Variable(name.to_string()), ty))
}

/// Call the closure `callee` evaluates to (#43).
///
/// Dispatch goes through the funcref table: push the arguments, then the
/// environment pointer (the lifted function's trailing `__env` parameter),
/// then the table slot stored in the environment's first word. When the
/// callee's static type names its signature, the call is made at exactly that
/// signature, so a float argument or result keeps its width and a string
/// result its length. Otherwise every parameter and the result are taken as
/// single words, which is all a value that passed through an untyped place
/// can be (`check_untyped_store` refuses any other).
#[allow(clippy::too_many_arguments)]
fn emit_closure_call(
    callee: &IRExpr,
    callee_type: &IRType,
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let signature = match callee_type {
        IRType::Callable {
            params,
            return_type,
        } if !(params.is_empty() && matches!(**return_type, IRType::Unknown)) => {
            Some((params.clone(), return_type.as_ref().clone()))
        }
        _ => None,
    };

    let (type_index, return_type) = match &signature {
        Some((params, return_type)) => {
            if params.len() != arguments.len() {
                ctx.report(format!(
                    "{name}() takes {} argument(s) but {} were given",
                    params.len(),
                    arguments.len()
                ));
            }
            for (i, arg) in arguments.iter().enumerate() {
                let param = params.get(i).cloned().unwrap_or(IRType::Unknown);
                let t = emit_value(arg, func, ctx, memory_layout, Some(&param));
                match (&param, &t) {
                    (IRType::Float, IRType::Int | IRType::Bool) => {
                        func.instruction(&Instruction::F64ConvertI32S);
                    }
                    (IRType::Float, IRType::Float) => {}
                    (_, IRType::Float) => {
                        ctx.report(format!(
                            "argument {} to '{name}' is a float where the function takes a '{}'",
                            i + 1,
                            crate::type_to_string(&param)
                        ));
                        func.instruction(&Instruction::I32TruncF64S);
                    }
                    (_, IRType::String | IRType::Bytes) => {
                        func.instruction(&Instruction::Drop);
                    }
                    _ => {}
                }
                ctx.check_class_store(&t, &param, &format!("argument {} to '{name}'", i + 1));
                ctx.check_untyped_store(&t, &param, &format!("argument {} to '{name}'", i + 1));
            }
            let mut wasm_params: Vec<ValType> = params.iter().map(wasm_value_type).collect();
            wasm_params.push(ValType::I32);
            let key = (wasm_params, wasm_value_type(return_type));
            match ctx.signature_types.get(&key) {
                Some(index) => (*index, return_type.clone()),
                None => {
                    ctx.report(format!(
                        "no function of this module has the signature '{name}' is called with; \
                         this is a code generation bug"
                    ));
                    (0, return_type.clone())
                }
            }
        }
        None => {
            let Some(type_base) = ctx.closure_type_base else {
                ctx.report(format!("'{name}' is not a function"));
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            };
            if arguments.len() as u32 > ctx.closure_max_arity {
                ctx.report(format!(
                    "'{name}' is called with {} arguments, more than any function it could hold \
                     takes",
                    arguments.len()
                ));
            }
            for arg in arguments {
                let t = emit_value(arg, func, ctx, memory_layout, None);
                // An untyped closure's parameters are single i32 words, which
                // hold only what any untyped place does.
                if !matches!(t, IRType::Float) {
                    ctx.check_untyped_store(
                        &t,
                        &IRType::Unknown,
                        &format!("an argument to '{name}'"),
                    );
                }
                match t {
                    IRType::String | IRType::Bytes => {
                        func.instruction(&Instruction::Drop);
                    }
                    IRType::Float => {
                        ctx.report(format!(
                            "passing a float to '{name}', a lambda or closure whose parameters \
                             are not typed, is not supported: they are held as ints. Hint: use a \
                             'def' with annotated parameters"
                        ));
                        func.instruction(&Instruction::I32TruncF64S);
                    }
                    _ => {}
                }
            }
            (type_base + arguments.len() as u32, IRType::Unknown)
        }
    };

    // The environment pointer: the last argument, then reused to load the
    // table slot from its first word.
    emit_expr(callee, func, ctx, memory_layout, None);
    func.instruction(&Instruction::LocalTee(ctx.temp_local));
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
    func.instruction(&Instruction::I32Load(slot_arg()));
    // Any closure could be the one called, so the call is checked for a
    // pending exception whenever the module raises at all.
    let can_raise = !ctx.can_raise.is_empty();
    if can_raise {
        emit_call_depth_step(func, 1);
    }
    func.instruction(&Instruction::CallIndirect {
        type_index,
        table_index: 0,
    });
    if can_raise {
        emit_call_depth_step(func, -1);
        emit_post_call_check(func, ctx);
    }
    finish_user_call(func, ctx, &return_type);
    return_type
}

/// `format(value[, spec])` through the value type's format helper, the spec
/// checked when it is a constant; `ascii(value)`, its repr with every
/// non-ASCII character escaped.
fn emit_format_call(
    function_name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    use crate::compiler::runtime::Helper;
    let takes = if function_name == "format" {
        1..=2
    } else {
        1..=1
    };
    if !takes.contains(&arguments.len()) {
        ctx.report(format!(
            "{function_name}() takes {} argument(s), not {}",
            if function_name == "format" {
                "1 or 2"
            } else {
                "1"
            },
            arguments.len()
        ));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Const(0));
        return IRType::String;
    }
    let ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
    if matches!(ty, IRType::String | IRType::Bytes) {
        func.instruction(&Instruction::Drop);
    }
    let ty = match ty {
        IRType::Unknown | IRType::Any => IRType::Int,
        other => other,
    };
    if function_name == "ascii" {
        if let Some(why) = crate::compiler::runtime::str_unsupported(ctx, &ty, true) {
            ctx.report(format!("ascii() of {why} is not supported"));
        }
        func.instruction(&Instruction::Call(ctx.helper(Helper::Repr(ty))));
        func.instruction(&Instruction::Call(ctx.helper(Helper::AsciiEscape)));
        recover_str_pair(func, ctx);
        return IRType::String;
    }
    match arguments.get(1) {
        Some(spec) => {
            if let IRExpr::Const(IRConstant::String(text)) = spec {
                if let Some(why) = crate::compiler::format::spec_error(text, &ty) {
                    ctx.report(format!("format spec '{text}' raises ValueError: {why}"));
                }
            }
            let st = emit_expr(spec, func, ctx, memory_layout, None);
            if !matches!(st, IRType::String) {
                ctx.report(format!(
                    "format()'s spec must be a str, not a '{}'",
                    crate::type_to_string(&st)
                ));
                func.instruction(&Instruction::I32Const(0));
            } else {
                func.instruction(&Instruction::Drop);
            }
        }
        None => {
            match memory_layout.string_offsets.get("") {
                Some(offset) => {
                    func.instruction(&Instruction::I32Const(*offset as i32));
                }
                None => {
                    ctx.report("the empty string was never placed in memory; this is a code generation bug");
                    func.instruction(&Instruction::I32Const(0));
                }
            }
        }
    }
    if !matches!(
        ty,
        IRType::Int | IRType::Bool | IRType::Float | IRType::String
    ) {
        if let Some(why) = crate::compiler::runtime::str_unsupported(ctx, &ty, false) {
            ctx.report(format!("format() of {why} is not supported"));
        }
    }
    func.instruction(&Instruction::Call(ctx.helper(Helper::Format(ty))));
    recover_str_pair(func, ctx);
    IRType::String
}

/// `float(text)` as CPython parses it, or why it cannot be read: surrounding
/// whitespace, a sign, `inf`/`infinity`/`nan` in any case, and digits with
/// single underscores between them, a point, and an exponent.
fn parse_python_float(text: &str) -> Result<f64, String> {
    let invalid = || format!("raises ValueError: could not convert string to float: {text:?}");
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    if !trimmed.is_ascii() {
        return Err(
            "is not supported: a string with non-ASCII digits or spaces. Hint: use ASCII digits"
                .to_string(),
        );
    }
    let (negative, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let lower = body.to_ascii_lowercase();
    let magnitude = match lower.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ => {
            let bytes = body.as_bytes();
            let mut digits = 0;
            let mut seen_dot = false;
            let mut i = 0;
            let underscore_ok = |i: usize| {
                i > 0
                    && bytes[i - 1].is_ascii_digit()
                    && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
            };
            while i < bytes.len() {
                match bytes[i] {
                    b'0'..=b'9' => digits += 1,
                    b'_' if underscore_ok(i) => {}
                    b'.' if !seen_dot => seen_dot = true,
                    b'e' | b'E' if digits > 0 => break,
                    _ => return Err(invalid()),
                }
                i += 1;
            }
            if digits == 0 {
                return Err(invalid());
            }
            if i < bytes.len() {
                i += 1;
                if matches!(bytes.get(i), Some(b'+' | b'-')) {
                    i += 1;
                }
                let start = i;
                while i < bytes.len() {
                    match bytes[i] {
                        b'0'..=b'9' => {}
                        b'_' if i > start && underscore_ok(i) => {}
                        _ => return Err(invalid()),
                    }
                    i += 1;
                }
                if i == start {
                    return Err(invalid());
                }
            }
            body.replace('_', "")
                .parse::<f64>()
                .map_err(|_| invalid())?
        }
    };
    Ok(if negative { -magnitude } else { magnitude })
}

/// Evaluate an exception's constructor arguments into held slots, in order,
/// and fill the argument fields of the exception in local `obj` from them.
/// Returns the slots, which the caller releases.
fn emit_exception_args(
    class: &str,
    arguments: &[IRExpr],
    obj: u32,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) {
    let held = emit_exception_held_args(class, arguments, None, func, ctx, memory_layout);
    crate::compiler::exceptions::emit_fill_args(
        func,
        ctx,
        obj,
        class,
        &held,
        ctx.temp_local,
        ctx.temp_local + 1,
    );
    release_exception_args(ctx, &held);
}

fn emit_exception_held_args(
    class: &str,
    arguments: &[IRExpr],
    param_types: Option<&[IRType]>,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Vec<(u32, IRType)> {
    let mut held = Vec::new();
    for (i, arg) in arguments.iter().enumerate() {
        if let IRExpr::Keyword { name, value } = arg {
            ctx.report(format!(
                "{class}() raises TypeError: {class}() takes no keyword arguments ('{name}')"
            ));
            let t = emit_expr(value, func, ctx, memory_layout, None);
            emit_drop_value(func, &t);
            continue;
        }
        let expected = param_types.and_then(|p| p.get(i + 1));
        let ty = emit_value(arg, func, ctx, memory_layout, expected);
        if matches!(ty, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
        let ty = match ty {
            IRType::Unknown | IRType::Any => IRType::Int,
            other => other,
        };
        if let Some(why) = crate::compiler::runtime::str_unsupported(ctx, &ty, true) {
            ctx.report(format!("an exception argument of {why} is not supported"));
        }
        let slot = if matches!(ty, IRType::Float) {
            ctx.hold_f64()
        } else {
            ctx.hold()
        };
        let Some(slot) = slot else {
            report_literal_too_deep(func, ctx, "an exception's arguments");
            return held;
        };
        func.instruction(&Instruction::LocalSet(slot));
        held.push((slot, ty));
    }
    held
}

fn release_exception_args(ctx: &CompilationContext, held: &[(u32, IRType)]) {
    for (_, ty) in held.iter().rev() {
        if matches!(ty, IRType::Float) {
            ctx.release_held_f64();
        } else {
            ctx.release_held();
        }
    }
}

/// `E(args)` for an exception class: the arguments evaluated in order, the
/// object allocated and its arguments recorded (as `BaseException.__new__`
/// records them), then the class's own `__init__`, if it has one, run with
/// the same values.
fn emit_exception_construct(
    class: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let info = ctx.get_class_info(class).expect("an exception class");
    let (size, id) = (info.instance_size, info.class_id);
    let init = info.methods.get("__init__").copied();
    let owner = info.method_owner.get("__init__").cloned();
    let param_types = owner.and_then(|o| {
        ctx.get_function_info(&format!("{o}::__init__"))
            .map(|f| f.param_types.clone())
    });
    let held = emit_exception_held_args(
        class,
        arguments,
        param_types.as_deref(),
        func,
        ctx,
        memory_layout,
    );
    let Some(obj) = ctx.hold() else {
        report_literal_too_deep(func, ctx, "an exception");
        return IRType::Class(class.to_string());
    };
    func.instruction(&Instruction::I32Const(size as i32));
    func.instruction(&Instruction::I32Const(id));
    func.instruction(&Instruction::Call(ctx.alloc_obj_func_index));
    func.instruction(&Instruction::LocalSet(obj));
    crate::compiler::exceptions::emit_fill_args(
        func,
        ctx,
        obj,
        class,
        &held,
        ctx.temp_local,
        ctx.temp_local + 1,
    );
    func.instruction(&Instruction::LocalGet(obj));
    if let Some(init) = init {
        for (k, (slot, ty)) in held.iter().enumerate() {
            func.instruction(&Instruction::LocalGet(*slot));
            check_class_arg(
                ctx,
                ty,
                param_types.as_ref().and_then(|p| p.get(k + 1)),
                k,
                class,
            );
        }
        emit_user_call(func, ctx, init);
    }
    ctx.release_held();
    release_exception_args(ctx, &held);
    IRType::Class(class.to_string())
}

/// For each class whose MRO holds `current` (`current` itself among them),
/// what `super().method` in `current`'s methods reaches on an instance of
/// it: the first class after `current` in its MRO defining the method (its
/// function index and name), or none.
fn super_targets(
    ctx: &CompilationContext,
    current: &str,
    method: &str,
) -> Vec<(i32, Option<(u32, String)>)> {
    let mut classes: Vec<&crate::compiler::context::ClassInfo> = ctx
        .class_map
        .values()
        .filter(|c| c.mro.iter().any(|m| m == current))
        .collect();
    classes.sort_by_key(|c| c.class_id);
    classes
        .into_iter()
        .map(|class| {
            let after = class.mro.iter().skip_while(|m| *m != current).skip(1);
            let target = after.into_iter().find_map(|ancestor| {
                let info = ctx.get_class_info(ancestor)?;
                (info.method_owner.get(method) == Some(ancestor))
                    .then(|| (info.methods[method], ancestor.clone()))
            });
            (class.class_id, target)
        })
        .collect()
}

/// `super().method(args)` whose target depends on the instance's class
/// (cooperative multiple inheritance): the arguments evaluated once, then
/// the call chosen by the instance's class tag. A class reaching no target
/// raises AttributeError in CPython, which traps here.
#[allow(clippy::too_many_arguments)]
fn emit_cooperative_super_call(
    method: &str,
    arguments: &[IRExpr],
    targets: &[(i32, Option<(u32, String)>)],
    param_types: &[IRType],
    ret: &IRType,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    // Every target is called through the same arguments, so they must take
    // and give the same words.
    for (_, target) in targets {
        let Some((_, owner)) = target else { continue };
        let theirs = ctx
            .get_function_info(&format!("{owner}::{method}"))
            .map(|f| (f.param_types.clone(), f.return_type.clone()));
        let same = theirs.as_ref().is_some_and(|(p, r)| {
            p.len() == param_types.len()
                && p.iter()
                    .zip(param_types)
                    .all(|(a, b)| wasm_value_type(a) == wasm_value_type(b))
                && wasm_value_type(r) == wasm_value_type(ret)
        });
        if !same {
            ctx.report(format!(
                "super().{method}() reaches implementations with different signatures \
                 depending on the instance's class, which one call cannot make"
            ));
        }
    }
    let mut held = Vec::new();
    for (i, arg) in arguments.iter().enumerate() {
        let t = emit_expr(arg, func, ctx, memory_layout, param_types.get(i + 1));
        check_class_arg(ctx, &t, param_types.get(i + 1), i, method);
        if matches!(t, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
        let slot = if matches!(t, IRType::Float) {
            ctx.hold_f64()
        } else {
            ctx.hold()
        };
        let Some(slot) = slot else {
            report_literal_too_deep(func, ctx, "a super() call");
            return ret.clone();
        };
        func.instruction(&Instruction::LocalSet(slot));
        held.push((slot, t));
    }
    let result = BlockType::Result(wasm_value_type(ret));
    let mut opened = 0;
    for (id, target) in targets {
        let Some((index, _)) = target else { continue };
        func.instruction(&Instruction::LocalGet(0));
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::I32Const(*id));
        func.instruction(&Instruction::I32Eq);
        func.instruction(&Instruction::If(result));
        ctx.enter_expr_blocks(1);
        func.instruction(&Instruction::LocalGet(0));
        for (slot, _) in &held {
            func.instruction(&Instruction::LocalGet(*slot));
        }
        emit_user_call(func, ctx, *index);
        func.instruction(&Instruction::Else);
        opened += 1;
    }
    func.instruction(&Instruction::Unreachable);
    for _ in 0..opened {
        func.instruction(&Instruction::End);
    }
    ctx.exit_expr_blocks(opened);
    for (_, t) in held.iter().rev() {
        if matches!(t, IRType::Float) {
            ctx.release_held_f64();
        } else {
            ctx.release_held();
        }
    }
    finish_user_call(func, ctx, ret);
    ret.clone()
}

/// The offset of a string the converter interned.
fn interned(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    text: &str,
) {
    match memory_layout.string_offsets.get(text) {
        Some(offset) => {
            func.instruction(&Instruction::I32Const(*offset as i32));
        }
        None => {
            ctx.report(format!(
                "the string {text:?} was never placed in memory; this is a code generation bug"
            ));
            func.instruction(&Instruction::I32Const(0));
        }
    }
}

/// `print(*values, sep=" ", end="\n", file=sys.stdout, flush=False)`: each
/// value through `str()`, joined by `sep`, followed by `end`, written to
/// standard output (or standard error) with WASI's `fd_write`. Every argument
/// is evaluated before any is converted to text, as CPython evaluates a call
/// before running it, so `print(xs, xs.append(1))` shows the appended list.
fn emit_print(
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    use crate::compiler::runtime::{str_unsupported, Helper};
    let mut held: Vec<(u32, IRType)> = Vec::new();
    let mut f64_held = 0;
    let mut i32_held = 0;
    let mut sep: Option<u32> = None;
    let mut end: Option<u32> = None;
    let mut fd = 1;
    let release = |ctx: &CompilationContext, i32_held: usize, f64_held: usize| {
        for _ in 0..i32_held {
            ctx.release_held();
        }
        for _ in 0..f64_held {
            ctx.release_held_f64();
        }
    };
    for arg in arguments {
        match arg {
            IRExpr::Keyword { name, value } => match name.as_str() {
                "sep" | "end" => {
                    if matches!(value.as_ref(), IRExpr::Const(IRConstant::None)) {
                        continue;
                    }
                    let ty = emit_expr(value, func, ctx, memory_layout, None);
                    if !matches!(ty, IRType::String) {
                        ctx.report(format!(
                            "print() raises TypeError: {name} must be None or a string, not {}",
                            crate::compiler::operators::python_type_name(&ty)
                        ));
                        func.instruction(&Instruction::I32Const(0));
                    } else {
                        func.instruction(&Instruction::Drop);
                    }
                    let Some(slot) = ctx.hold() else {
                        report_literal_too_deep(func, ctx, "print()");
                        return IRType::None;
                    };
                    i32_held += 1;
                    func.instruction(&Instruction::LocalSet(slot));
                    if name == "sep" {
                        sep = Some(slot);
                    } else {
                        end = Some(slot);
                    }
                }
                "file" => match crate::compiler::host::std_stream(ctx, value) {
                    Some(1) | Some(2) => {
                        fd = crate::compiler::host::std_stream(ctx, value).unwrap()
                    }
                    _ if matches!(value.as_ref(), IRExpr::Const(IRConstant::None)) => {}
                    Some(_) => ctx.report(
                        "print(file=sys.stdin) raises io.UnsupportedOperation: not writable",
                    ),
                    None => ctx.report(
                        "print(file=...) is supported for sys.stdout and sys.stderr. Hint: \
                         write to a file with f.write(...)",
                    ),
                },
                "flush" => {
                    // Writes are not buffered, so there is nothing to flush.
                    let ty = emit_expr(value, func, ctx, memory_layout, None);
                    emit_drop_value(func, &ty);
                }
                other => ctx.report(format!(
                    "print() raises TypeError: '{other}' is an invalid keyword argument for print()"
                )),
            },
            positional => {
                let ty = emit_value(positional, func, ctx, memory_layout, None);
                if matches!(ty, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
                let ty = match ty {
                    IRType::Unknown | IRType::Any => IRType::Int,
                    other => other,
                };
                if let Some(why) = str_unsupported(ctx, &ty, false) {
                    ctx.report(format!("print() of {why} is not supported"));
                }
                let slot = if matches!(ty, IRType::Float) {
                    f64_held += 1;
                    ctx.hold_f64()
                } else {
                    i32_held += 1;
                    ctx.hold()
                };
                let Some(slot) = slot else {
                    report_literal_too_deep(func, ctx, "print()");
                    return IRType::None;
                };
                func.instruction(&Instruction::LocalSet(slot));
                held.push((slot, ty));
            }
        }
    }
    // The line: each value as text, the separator between, the end after.
    let concat = ctx.helper(Helper::Concat);
    let Some(line) = ctx.hold() else {
        report_literal_too_deep(func, ctx, "print()");
        return IRType::None;
    };
    i32_held += 1;
    let push_sep = |func: &mut Function| match sep {
        Some(slot) => {
            func.instruction(&Instruction::LocalGet(slot));
        }
        None => interned(func, ctx, memory_layout, " "),
    };
    for (k, (slot, ty)) in held.iter().enumerate() {
        if k > 0 {
            func.instruction(&Instruction::LocalGet(line));
            push_sep(func);
            func.instruction(&Instruction::Call(concat));
            func.instruction(&Instruction::LocalSet(line));
        }
        func.instruction(&Instruction::LocalGet(*slot));
        func.instruction(&Instruction::Call(ctx.helper(Helper::Str(ty.clone()))));
        // An instance's `__str__` may raise.
        if matches!(ty, IRType::Class(_)) {
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            crate::compiler::function::emit_post_call_check(func, ctx);
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
        }
        if k > 0 {
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            func.instruction(&Instruction::LocalGet(line));
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::Call(concat));
        }
        func.instruction(&Instruction::LocalSet(line));
    }
    if held.is_empty() {
        interned(func, ctx, memory_layout, "");
        func.instruction(&Instruction::LocalSet(line));
    }
    func.instruction(&Instruction::LocalGet(line));
    match end {
        Some(slot) => {
            func.instruction(&Instruction::LocalGet(slot));
        }
        None => interned(func, ctx, memory_layout, "\n"),
    }
    func.instruction(&Instruction::Call(concat));
    func.instruction(&Instruction::LocalSet(line));
    func.instruction(&Instruction::I32Const(fd));
    func.instruction(&Instruction::LocalGet(line));
    func.instruction(&Instruction::Call(ctx.helper(Helper::WriteFd)));
    release(ctx, i32_held, f64_held);
    IRType::None
}

/// `input([prompt])`: the prompt, through `str()`, written to standard
/// output; then a line of standard input without its newline. `EOFError` at
/// the end of input.
fn emit_input(
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    use crate::compiler::runtime::{str_unsupported, Helper};
    if arguments.len() > 1 || matches!(arguments.first(), Some(IRExpr::Keyword { .. })) {
        ctx.report(format!(
            "input() raises TypeError: input expected at most 1 positional argument, got {}",
            arguments.len()
        ));
    }
    if let Some(prompt) = arguments.first() {
        let ty = emit_expr(prompt, func, ctx, memory_layout, None);
        if matches!(ty, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
        let ty = match ty {
            IRType::Unknown | IRType::Any => IRType::Int,
            other => other,
        };
        if let Some(why) = str_unsupported(ctx, &ty, false) {
            ctx.report(format!("input() of {why} is not supported"));
        }
        func.instruction(&Instruction::Call(ctx.helper(Helper::Str(ty))));
        func.instruction(&Instruction::LocalSet(ctx.temp_local));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::LocalGet(ctx.temp_local));
        func.instruction(&Instruction::Call(ctx.helper(Helper::WriteFd)));
    }
    func.instruction(&Instruction::Call(ctx.helper(Helper::ReadLine {
        keep_newline: false,
        whole: false,
    })));
    func.instruction(&Instruction::LocalSet(ctx.temp_local));
    crate::compiler::function::emit_post_call_check(func, ctx);
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    crate::compiler::function::emit_raise(
        func,
        ctx,
        "EOFError",
        crate::compiler::exceptions::ExcArg::Text("EOF when reading a line"),
        1,
    );
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
    recover_str_pair(func, ctx);
    IRType::String
}

/// A method of `sys.stdout`, `sys.stderr` (`write`, `flush`), or `sys.stdin`
/// (`readline`, `read`, `readlines`).
fn emit_std_stream_method(
    fd: i32,
    method_name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    use crate::compiler::runtime::Helper;
    let stream = ["sys.stdin", "sys.stdout", "sys.stderr"][fd as usize];
    let arity = |ctx: &CompilationContext, n: usize| {
        if arguments.len() != n {
            ctx.report(format!(
                "{stream}.{method_name}() takes {n} argument(s), not {}",
                arguments.len()
            ));
        }
    };
    match (fd, method_name) {
        (1 | 2, "write") => {
            arity(ctx, 1);
            let Some(arg) = arguments.first() else {
                func.instruction(&Instruction::I32Const(0));
                return IRType::Int;
            };
            let ty = emit_expr(arg, func, ctx, memory_layout, None);
            if !matches!(ty, IRType::String) {
                ctx.report(format!(
                    "{stream}.write() raises TypeError: write() argument must be str, not {}",
                    crate::compiler::operators::python_type_name(&ty)
                ));
                func.instruction(&Instruction::I32Const(0));
            } else {
                func.instruction(&Instruction::Drop);
            }
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            func.instruction(&Instruction::I32Const(fd));
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::Call(ctx.helper(Helper::WriteFd)));
            // The number of characters written.
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::Call(ctx.helper(Helper::CharLen)));
            IRType::Int
        }
        (1 | 2, "flush") => {
            arity(ctx, 0);
            IRType::None
        }
        (0, "readline" | "read") => {
            arity(ctx, 0);
            func.instruction(&Instruction::Call(ctx.helper(Helper::ReadLine {
                keep_newline: true,
                whole: method_name == "read",
            })));
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            crate::compiler::function::emit_post_call_check(func, ctx);
            // At the end of input, the empty string.
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            interned(func, ctx, memory_layout, "");
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::End);
            recover_str_pair(func, ctx);
            IRType::String
        }
        (0, "readlines") => {
            arity(ctx, 0);
            func.instruction(&Instruction::Call(ctx.helper(Helper::ReadLines)));
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            crate::compiler::function::emit_post_call_check(func, ctx);
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            IRType::List(Box::new(IRType::String))
        }
        _ => {
            ctx.report(format!("{stream}.{method_name}() is not supported"));
            for arg in arguments {
                let ty = emit_expr(arg, func, ctx, memory_layout, None);
                emit_drop_value(func, &ty);
            }
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}

/// The value one `%` conversion formats: item `i` of a tuple operand of
/// `n` items, or a lone operand itself. `n` of 0 is a format with no
/// conversions, which only a mapping may go with; it pushes the empty string.
fn emit_percent_item(
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let [value, IRExpr::Const(IRConstant::Int(i)), IRExpr::Const(IRConstant::Int(n))] = arguments
    else {
        ctx.report("a malformed '%' item reached code generation; this is a compiler bug");
        func.instruction(&Instruction::I32Const(0));
        return IRType::Int;
    };
    let (i, n) = (*i as usize, *n as usize);
    let ty = crate::compiler::function::infer_value_type(value, ctx);
    let empty = |func: &mut Function| {
        let offset = memory_layout.string_offsets.get("").copied().unwrap_or(0);
        func.instruction(&Instruction::I32Const(offset as i32));
        func.instruction(&Instruction::I32Const(0));
    };
    match &ty {
        IRType::Tuple(items) => {
            if items.len() != n {
                ctx.report(if items.len() > n {
                    "'%' formatting raises TypeError: not all arguments converted during string formatting"
                } else {
                    "'%' formatting raises TypeError: not enough arguments for format string"
                });
            }
            if n == 0 {
                empty(func);
                return IRType::String;
            }
            let item = IRExpr::Indexing {
                container: Box::new(value.clone()),
                index: Box::new(IRExpr::Const(IRConstant::Int(i as i32))),
            };
            emit_expr(&item, func, ctx, memory_layout, None)
        }
        IRType::VarTuple(_) => {
            ctx.report(
                "'%' formatting with a variable-length tuple operand is not supported. \
                 Hint: unpack it into a tuple literal, for example \"%s %s\" % (t[0], t[1])",
            );
            func.instruction(&Instruction::I32Const(0));
            IRType::Int
        }
        _ if n == 0 => {
            let ty = emit_expr(value, func, ctx, memory_layout, None);
            if !matches!(ty, IRType::Dict(..)) {
                ctx.report(
                    "'%' formatting raises TypeError: not all arguments converted during string formatting",
                );
            }
            emit_drop_value(func, &ty);
            empty(func);
            IRType::String
        }
        _ if n == 1 => emit_expr(value, func, ctx, memory_layout, None),
        _ => {
            ctx.report("'%' formatting raises TypeError: not enough arguments for format string");
            let ty = emit_expr(value, func, ctx, memory_layout, None);
            emit_drop_value(func, &ty);
            func.instruction(&Instruction::I32Const(0));
            IRType::Int
        }
    }
}

/// Drop a value of `ty` from the stack.
fn emit_drop_value(func: &mut Function, ty: &IRType) {
    func.instruction(&Instruction::Drop);
    if matches!(ty, IRType::String | IRType::Bytes) {
        func.instruction(&Instruction::Drop);
    }
}

/// One `%` conversion: `%s %r %a` render `str`/`repr`/`ascii` of the value
/// under the text spec; `%d %i %u` an int (a float truncated, as CPython
/// does through `int()`), `%x %X %o %c` an int, and `%e %f %g` a float (an
/// int widened), under the number spec.
fn emit_percent_field(
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    use crate::compiler::runtime::{str_unsupported, Helper};
    let [value, IRExpr::Const(IRConstant::String(num)), IRExpr::Const(IRConstant::String(text)), IRExpr::Const(IRConstant::Int(conv))] =
        arguments
    else {
        ctx.report("a malformed '%' conversion reached code generation; this is a compiler bug");
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Const(0));
        return IRType::String;
    };
    let conv = char::from(*conv as u8);
    let ty = emit_expr(value, func, ctx, memory_layout, None);
    if matches!(ty, IRType::String | IRType::Bytes) {
        func.instruction(&Instruction::Drop);
    }
    let ty = match ty {
        IRType::Unknown | IRType::Any => IRType::Int,
        other => other,
    };
    let spec = |func: &mut Function, s: &str| match memory_layout.string_offsets.get(s) {
        Some(offset) => {
            func.instruction(&Instruction::I32Const(*offset as i32));
        }
        None => {
            ctx.report("a '%' spec was never placed in memory; this is a compiler bug");
            func.instruction(&Instruction::I32Const(0));
        }
    };
    let name = crate::compiler::operators::python_type_name(&ty);
    // A value the conversion cannot take: reported, and replaced by an int.
    let refuse = |func: &mut Function, message: String| {
        ctx.report(message);
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(0));
    };
    match conv {
        's' | 'r' | 'a' => {
            if let Some(why) = str_unsupported(ctx, &ty, conv != 's') {
                ctx.report(format!("'%{conv}' of {why} is not supported"));
            }
            if conv == 's' {
                func.instruction(&Instruction::Call(ctx.helper(Helper::Str(ty.clone()))));
            } else {
                func.instruction(&Instruction::Call(ctx.helper(Helper::Repr(ty.clone()))));
            }
            if conv == 'a' {
                func.instruction(&Instruction::Call(ctx.helper(Helper::AsciiEscape)));
            }
            spec(func, text);
            func.instruction(&Instruction::Call(
                ctx.helper(Helper::Format(IRType::String)),
            ));
        }
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
            match ty {
                IRType::Float => {}
                IRType::Int | IRType::Bool => {
                    func.instruction(&Instruction::F64ConvertI32S);
                }
                _ => {
                    refuse(
                        func,
                        format!("'%{conv}' formatting raises TypeError: must be real number, not {name}"),
                    );
                    func.instruction(&Instruction::F64ConvertI32S);
                }
            }
            spec(func, num);
            func.instruction(&Instruction::Call(
                ctx.helper(Helper::Format(IRType::Float)),
            ));
        }
        _ => {
            match (&ty, conv) {
                (IRType::Int | IRType::Bool, _) => {}
                (IRType::Float, 'd' | 'i' | 'u') => {
                    func.instruction(&Instruction::I32TruncF64S);
                }
                (IRType::Float, _) => {
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    ctx.report(format!(
                        "'%{conv}' formatting raises TypeError: %{conv} format: an integer is required, not float"
                    ));
                }
                (IRType::String, 'c') => refuse(
                    func,
                    "'%c' of a str is not supported. Hint: use '%s' for a one-character string"
                        .to_string(),
                ),
                _ => refuse(
                    func,
                    format!(
                        "'%{conv}' formatting raises TypeError: %{conv} format: a real number is required, not {name}"
                    ),
                ),
            }
            spec(func, num);
            func.instruction(&Instruction::Call(ctx.helper(Helper::Format(IRType::Int))));
        }
    }
    recover_str_pair(func, ctx);
    IRType::String
}

/// A set on the stack becomes the list of its members in iteration order,
/// so a construct that walks a sequence walks it; any other value is left as
/// it is. Returns the type now on the stack.
pub(crate) fn emit_as_sequence(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: IRType,
) -> IRType {
    match ty {
        IRType::Set(member) => {
            crate::compiler::sets::emit_members_as_list(func, ctx, &member);
            IRType::List(member)
        }
        // A walk over a dict visits its keys. A comprehension, `sum`,
        // `sorted`, `min`, and `max` walked its entries' slots as a list's,
        // keys and values interleaved, so `[k for k in {5: 3, 7: 2}]` was
        // `[5, 3]`. (The `for` statement walks the dict itself.)
        IRType::Dict(key, _) => {
            func.instruction(&Instruction::Call(
                ctx.helper(crate::compiler::runtime::Helper::DictKeys),
            ));
            IRType::List(key)
        }
        // A walk over a str visits its characters (the `for` statement
        // walks the string itself); a comprehension read the text as a list
        // header and trapped.
        IRType::String => {
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::Call(
                ctx.helper(crate::compiler::runtime::Helper::StrChars),
            ));
            IRType::List(Box::new(IRType::String))
        }
        // A tuple is laid out as a list is, so a walk reads it as one when
        // its members share a type. One that mixes them is refused: a walk
        // reads every member at one type, so `sorted((1, "a"))` sorted the
        // string's address and `for x in (1, "a")` read the wrong width.
        IRType::Tuple(members) => {
            let Some(first) = members.first() else {
                return IRType::VarTuple(Box::new(IRType::Unknown));
            };
            let mut member = first.clone();
            for m in &members[1..] {
                match unify_element(&member, m) {
                    Some(both) => member = both,
                    None => {
                        ctx.report(format!(
                            "iterating a tuple whose members have different types ({}) is not \
                             supported: the walk reads every member as one type. Hint: index it \
                             at constant positions, or make the members one type",
                            members
                                .iter()
                                .map(crate::type_to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        break;
                    }
                }
            }
            IRType::VarTuple(Box::new(member))
        }
        // An instance is iterated through its `__next__`, which the generator
        // pass wires up wherever it can see the iterator's class. One that
        // reaches a walk here was read as a list header: a generator returned
        // through an unannotated method walked its own fields.
        IRType::Class(name) => {
            let what = match name.strip_prefix("__Gen_") {
                Some(function) => format!("the generator '{function}()'"),
                None => format!("an instance of '{name}'"),
            };
            ctx.report(format!(
                "iterating {what} here is not supported: its class is not known where it is \
                 created. Hint: assign it to a variable from a direct call first, or annotate \
                 what returns it"
            ));
            IRType::List(Box::new(IRType::Unknown))
        }
        other => other,
    }
}

/// Why a value of `ty` cannot be indexed or sliced (`how`), in CPython's
/// words where CPython refuses it too.
fn not_subscriptable(ty: &IRType, how: &str) -> String {
    match ty {
        IRType::Unknown | IRType::Any => format!(
            "a value whose type is not known cannot be {how}: it would be read as an untyped \
             word. Hint: annotate it, for example the parameter it comes from"
        ),
        IRType::Range | IRType::Class(_) | IRType::Generator(_) => format!(
            "a value of type '{}' cannot be {how} yet",
            crate::compiler::operators::python_type_name(ty)
        ),
        other => format!(
            "'{}' object is not subscriptable",
            crate::compiler::operators::python_type_name(other)
        ),
    }
}

/// The WASM type a value of `ty` is passed and returned as.
pub(crate) fn wasm_value_type(ty: &IRType) -> ValType {
    match ty {
        IRType::Float => ValType::F64,
        _ => ValType::I32,
    }
}

/// Rebuild the `(offset, length)` pair from a bare string/bytes offset on top
/// of the stack, loading the length from the blob's prefix
/// (`load(offset - STRING_LEN_PREFIX)`). Used wherever a single offset word
/// crosses back into pair-land: a call result (functions return one word), an
/// instance field read, or a `__i32_to_str` result.
pub(crate) fn recover_str_pair(func: &mut Function, ctx: &CompilationContext) {
    func.instruction(&Instruction::LocalTee(ctx.temp_local));
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
}

/// The width a collection's slots are read back at: a list's or set's element
/// type, a dict's value type. `None` means the container does not have one
/// (or has not been typed yet), and every write into it is a single word.
pub(crate) fn collection_element_type(container: &IRType) -> Option<IRType> {
    match container {
        IRType::List(t) | IRType::Set(t) => Some(t.as_ref().clone()),
        IRType::Dict(_, v) => Some(v.as_ref().clone()),
        _ => None,
    }
}

/// Emit a value on its way into a collection slot, at the width the collection
/// is read back at.
///
/// Every write path used to emit the value with no type hint and store whatever
/// came out (#123), so `xs.append(3)` on a `List[float]` wrote a 4-byte integer
/// into an 8-byte float slot and the element read back as 1.5e-323. The hint
/// alone is not enough, because it only reaches constants and arithmetic: a
/// variable holding an int came out an int whatever was asked for. So the
/// emitted type is converted here as well.
///
/// A conversion that would lose the value, or a float written where the
/// collection has no float width to read it back at, is a compile error rather
/// than a silent store: the same stand the mixed-width literal check takes.
/// `what` names the write for the message ("list.append", "dict value", ...).
pub(crate) fn emit_collection_element(
    value: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    element_type: Option<&IRType>,
    what: &str,
) -> IRType {
    let declared = element_type.filter(|t| !matches!(t, IRType::Unknown | IRType::Any));
    // The hint is passed only where the conversion it performs is the one
    // wanted: widening an int to the float width the slot is read at. Passing
    // an `int` hint would make `emit_expr` truncate a float *constant* before
    // it ever reached the check below, which is the silent 2.5 -> 2 this is
    // meant to refuse.
    let hint = match declared {
        Some(IRType::Float) => declared,
        _ => None,
    };
    let actual = emit_expr(value, func, ctx, memory_layout, hint);
    if let Some(slot) = declared {
        ctx.check_class_store(&actual, slot, what);
    }
    if let Some(slot @ IRType::Any) = element_type {
        ctx.check_untyped_store(&actual, slot, what);
    }

    match (declared, &actual) {
        // An int or bool into a float slot widens, which is what the slot is
        // read back at and what Python's own arithmetic on the element does.
        (Some(IRType::Float), IRType::Int | IRType::Bool) => {
            func.instruction(&Instruction::F64ConvertI32S);
            IRType::Float
        }
        // A float into an int slot would have to truncate, losing the value
        // with nothing said.
        (Some(IRType::Int | IRType::Bool), IRType::Float) => {
            ctx.report(format!(
                "{what} stores a float into a collection whose elements are read as \
                 integers, which would truncate it. Hint: make the collection's \
                 elements floats, or convert with int()"
            ));
            func.instruction(&Instruction::I32TruncF64S);
            IRType::Int
        }
        // A value of no known type is one word wide by this compiler's
        // convention, so storing it in a float slot fills half of one. Its
        // type has to come from somewhere before it can be widened.
        (Some(IRType::Float), IRType::Unknown | IRType::Any) => {
            ctx.report(format!(
                "{what} stores a value of unknown type into a collection of floats. \
                 Hint: annotate the value it comes from, for example the parameter, \
                 the local, or the called function's return type"
            ));
            IRType::Unknown
        }
        // No element width to read an f64 back at: the slot's low word is what
        // every read takes, so this stored garbage (and failed validation where
        // a float was expected).
        (None, IRType::Float) => {
            ctx.report(format!(
                "{what} stores a float into a collection with no known element type, so \
                 the value cannot be read back. Hint: annotate it, for example \
                 'xs: List[float] = []'"
            ));
            IRType::Float
        }
        // A scalar of another kind would be read back as the element type: a
        // string's offset as an int, an int as a string's offset.
        (Some(slot), value) if scalar_mismatch(value, slot) => {
            ctx.report(format!(
                "{what} stores a '{}' into a collection whose elements are '{}', and it \
                 would be read back as one. Hint: give the collection the element type of \
                 what it holds",
                crate::type_to_string(value),
                crate::type_to_string(slot)
            ));
            actual
        }
        _ => actual,
    }
}

/// Whether `value` and `slot` are different kinds of scalar, which no
/// conversion on the way into a slot reconciles (an int into a float slot is
/// converted, and a bool is an int).
fn scalar_mismatch(value: &IRType, slot: &IRType) -> bool {
    let scalar = |t: &IRType| {
        matches!(
            t,
            IRType::Int | IRType::Bool | IRType::Float | IRType::String | IRType::Bytes
        )
    };
    let compatible = value == slot
        || matches!(
            (value, slot),
            (IRType::Bool, IRType::Int) | (IRType::Int | IRType::Bool, IRType::Float)
        );
    let slot_scalar = match slot {
        IRType::Optional(inner) => inner.as_ref(),
        other => other,
    };
    scalar(value) && scalar(slot_scalar) && !compatible && value != slot_scalar
}

/// Stash a freshly emitted search needle (its value is on top of the stack, of
/// `elem_type`) into a scratch local so a search loop can compare it against
/// each slot with [`emit_slot_eq_needle`]. Float needles go into the dedicated
/// f64 scratch (`ctx.temp_local_f64`); string/bytes collapse to their interned
/// offset; everything else is the i32 value itself, all stored in `needle_i32`.
fn stash_search_needle(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_type: &IRType,
    needle_i32: u32,
) {
    match elem_type {
        IRType::Float => func.instruction(&Instruction::LocalSet(ctx.temp_local_f64)),
        IRType::String | IRType::Bytes => {
            func.instruction(&Instruction::Drop); // length
            func.instruction(&Instruction::LocalSet(needle_i32))
        }
        _ => func.instruction(&Instruction::LocalSet(needle_i32)),
    };
}

/// With a slot address on top of the stack, store the needle stashed by
/// [`stash_search_needle`] into it (float as `f64`, otherwise the i32 word).
fn store_stashed_needle(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_type: &IRType,
    needle_i32: u32,
) {
    if matches!(elem_type, IRType::Float) {
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
        func.instruction(&Instruction::F64Store(slot_arg()));
    } else {
        func.instruction(&Instruction::LocalGet(needle_i32));
        func.instruction(&Instruction::I32Store(slot_arg()));
    }
}

/// Compare two strings by content, given their data offsets in `a_off` and
/// `b_off`, leaving an i32 bool on the stack.
///
/// A string slot in a collection or a dict entry holds only the offset, so
/// comparing slots as words compares *identity*: two equal strings built at
/// different times sit at different offsets and never matched, so
/// `counts[word]` with a word from `split()` never found its own entry. Each
/// blob carries its length in the four bytes before its data, so the length is
/// recovered here and the bytes are compared.
///
/// Uses the top of the scratch run so it can be called from inside the search
/// loops, which own the lower scratch locals.
pub(crate) fn emit_str_content_eq(
    func: &mut Function,
    ctx: &CompilationContext,
    a_off: u32,
    b_off: u32,
) {
    let len_a = ctx.temp_local + 15;
    let i = ctx.temp_local + 16;
    let out = ctx.temp_local + 17;

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(out));

    // Lengths first: different lengths cannot be equal, and this is the common
    // rejection.
    func.instruction(&Instruction::LocalGet(a_off));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(len_a));

    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::LocalGet(b_off));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len_a));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(a_off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalGet(b_off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(out));
}

/// With a slot address on top of the stack, load the slot per `elem_type` and
/// push an i32 `1` if it equals the needle stashed by [`stash_search_needle`],
/// else `0`. Floats compare as `f64` (so members dedup and `in` work by value,
/// not by a lossy bit pattern); everything else compares the i32 low word.
pub(crate) fn emit_slot_eq_needle(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_type: &IRType,
    needle_i32: u32,
) {
    if matches!(elem_type, IRType::Float) {
        func.instruction(&Instruction::F64Load(slot_arg()));
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
        func.instruction(&Instruction::F64Eq);
    } else if matches!(elem_type, IRType::String | IRType::Bytes) {
        // Strings compare by content, not by where they happen to live: two
        // equal strings built separately have different offsets.
        let slot_off = ctx.temp_local + 14;
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalSet(slot_off));
        emit_str_content_eq(func, ctx, slot_off, needle_i32);
    } else if matches!(
        elem_type,
        IRType::Tuple(_)
            | IRType::VarTuple(_)
            | IRType::List(_)
            | IRType::Dict(_, _)
            | IRType::Set(_)
            | IRType::Class(_)
    ) {
        // A collection, or an instance with `__eq__`, compares by value. This
        // compared the two pointers, so `(1, 2) in [(1, 2)]` was False and a
        // set of tuples never found or de-duplicated its members.
        if let Some(why) = crate::compiler::equality::eq_unsupported(ctx, elem_type) {
            ctx.report(format!("comparing {why} for equality is not supported"));
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::I32Const(0));
            return;
        }
        let slot = crate::compiler::equality::hold(ctx);
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalSet(slot));
        crate::compiler::equality::emit_values_eq(func, ctx, elem_type, slot, needle_i32);
        crate::compiler::equality::release(ctx, 1);
    } else {
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalGet(needle_i32));
        func.instruction(&Instruction::I32Eq);
    }
}

// --- Comprehensions (#44) -------------------------------------------------
//
// A comprehension builds its result at runtime because the element count isn't
// known at compile time (it depends on iterable lengths and filters). Codegen
// runs in (up to) two passes over the generators:
//
//   1. capacity: for a single generator, the length of its iterable; for
//      multiple generators, a counting pre-pass runs the outer loops (without
//      filters) and sums the innermost iterable's length. Filters only ever
//      *over*-allocate, which the bump allocator tolerates.
//   2. fill: the real nested loops bind the target variables, evaluate the
//      filters, and append each produced element; the final element count is
//      written to the result header afterwards.
//
// The result block comes from `__alloc`, so a comprehension built inside a
// loop is automatically a fresh region per
// iteration. Helper locals are reserved by the scan pass keyed on
// comprehension nesting depth (see `scan_expr_locals`); with multiple
// generators the inner iterable expressions are evaluated once per outer
// iteration in *both* passes, so side effects in them run twice (documented
// limitation, matching the "no observable side effects in iterables" reality
// of the current language subset).

/// Set `out` to 1 when the needle occurs in the haystack starting at `pos`.
/// An empty needle matches anywhere in range, which is what makes
/// `"ab".find("")` answer 0 the way Python's does.
#[allow(clippy::too_many_arguments)]
fn emit_match_at(
    func: &mut Function,
    h_off: u32,
    h_len: u32,
    n_off: u32,
    n_len: u32,
    pos: u32,
    out: u32,
    k: u32,
) {
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(out));
    // A needle running past the end cannot match.
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(h_off));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalGet(n_off));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
}

/// Which of the substring-search methods is being emitted.
#[derive(Clone, Copy, PartialEq)]
enum SearchMode {
    Find,
    Index,
    Count,
    StartsWith,
    EndsWith,
}

/// `find` / `index` / `count` / `startswith` / `endswith` on runtime strings.
///
/// Entry stack: the receiver's `(offset, length)` with the needle's
/// `(offset, length)` above it. Exit stack: one i32 (a position, a count, or a
/// bool). `index` raises `ValueError` when the needle is absent, which is
/// Python's behaviour and is catchable; `find` answers -1.
/// The position a search left on the stack, a byte offset into the haystack
/// (still in `temp_local`) or -1, as a code point index, which is what CPython
/// answers.
fn emit_byte_to_char_index(func: &mut Function, ctx: &CompilationContext) {
    let at = ctx.temp_local + 8;
    func.instruction(&Instruction::LocalTee(at));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
    func.instruction(&Instruction::LocalGet(at));
    func.instruction(&Instruction::Call(
        ctx.helper(crate::compiler::runtime::Helper::CharIndex),
    ));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(at));
    func.instruction(&Instruction::End);
}

fn emit_string_search(func: &mut Function, ctx: &CompilationContext, mode: SearchMode) {
    let h_off = ctx.temp_local;
    let h_len = ctx.temp_local + 1;
    let n_off = ctx.temp_local + 2;
    let n_len = ctx.temp_local + 3;
    let pos = ctx.temp_local + 4;
    let out = ctx.temp_local + 5;
    let k = ctx.temp_local + 6;
    let acc = ctx.temp_local + 7;

    func.instruction(&Instruction::LocalSet(n_len));
    func.instruction(&Instruction::LocalSet(n_off));
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalSet(h_off));

    match mode {
        SearchMode::StartsWith | SearchMode::EndsWith => {
            if mode == SearchMode::StartsWith {
                func.instruction(&Instruction::I32Const(0));
            } else {
                // A needle longer than the haystack would underflow the
                // subtraction, so answer False before computing the position.
                func.instruction(&Instruction::LocalGet(n_len));
                func.instruction(&Instruction::LocalGet(h_len));
                func.instruction(&Instruction::I32GtU);
                func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::Else);
                func.instruction(&Instruction::LocalGet(h_len));
                func.instruction(&Instruction::LocalGet(n_len));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::End);
            }
            func.instruction(&Instruction::LocalSet(pos));
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::LocalGet(h_len));
            func.instruction(&Instruction::I32GtU);
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::Else);
            emit_match_at(func, h_off, h_len, n_off, n_len, pos, out, k);
            func.instruction(&Instruction::LocalGet(out));
            func.instruction(&Instruction::End);
        }
        SearchMode::Find | SearchMode::Index => {
            func.instruction(&Instruction::I32Const(-1));
            func.instruction(&Instruction::LocalSet(acc));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(pos));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::LocalGet(h_len));
            func.instruction(&Instruction::I32GtU);
            func.instruction(&Instruction::BrIf(1));
            emit_match_at(func, h_off, h_len, n_off, n_len, pos, out, k);
            func.instruction(&Instruction::LocalGet(out));
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::LocalSet(acc));
            func.instruction(&Instruction::Br(2));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(pos));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            if mode == SearchMode::Index {
                func.instruction(&Instruction::LocalGet(acc));
                func.instruction(&Instruction::I32Const(-1));
                func.instruction(&Instruction::I32Eq);
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_raise(
                    func,
                    ctx,
                    "ValueError",
                    crate::compiler::exceptions::ExcArg::Text("substring not found"),
                    1,
                );
                func.instruction(&Instruction::End);
            }
            func.instruction(&Instruction::LocalGet(acc));
        }
        SearchMode::Count => {
            // Non-overlapping, like Python: a match advances past itself.
            // An empty needle matches between every character, so it answers
            // len + 1 and must advance by one to terminate.
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(acc));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(pos));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::LocalGet(h_len));
            func.instruction(&Instruction::I32GtU);
            func.instruction(&Instruction::BrIf(1));
            emit_match_at(func, h_off, h_len, n_off, n_len, pos, out, k);
            func.instruction(&Instruction::LocalGet(out));
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(acc));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(acc));
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::LocalGet(n_len));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(pos));
            func.instruction(&Instruction::End);
            // Always advance at least one, which covers the empty needle.
            func.instruction(&Instruction::LocalGet(n_len));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::LocalGet(out));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::I32Or);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(pos));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(pos));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(acc));
        }
    }
}

/// `a % b` on two i32s with Python's sign rule: the result takes the divisor's
/// sign. Entry stack `(a, b)`, `b` nonzero; exit stack the remainder.
pub(crate) fn emit_floor_mod_i32(func: &mut Function, ctx: &CompilationContext) {
    let b = ctx.temp_local + 41;
    let r = ctx.temp_local + 42;
    func.instruction(&Instruction::LocalTee(b));
    func.instruction(&Instruction::I32RemS);
    func.instruction(&Instruction::LocalTee(r));
    // r != 0 and r, b of opposite signs: step up by one divisor.
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::LocalGet(r));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Xor);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(r));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(r));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(r));
}

/// `a // b` on two i32s, rounding toward negative infinity as Python does.
/// Entry stack `(a, b)`, `b` nonzero; exit stack the quotient.
pub(crate) fn emit_floor_div_i32(func: &mut Function, ctx: &CompilationContext) {
    let a = ctx.temp_local + 40;
    let b = ctx.temp_local + 41;
    let q = ctx.temp_local + 42;
    func.instruction(&Instruction::LocalSet(b));
    func.instruction(&Instruction::LocalTee(a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32DivS);
    func.instruction(&Instruction::LocalSet(q));
    // Inexact, and a and b of opposite signs: truncation went up, so step down.
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32RemS);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Xor);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(q));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(q));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(q));
}

/// The Python spelling of a binary operator, for error messages.
pub(crate) fn operator_symbol(op: &IROp) -> &'static str {
    match op {
        IROp::Add => "+",
        IROp::Sub => "-",
        IROp::Mul => "*",
        IROp::Div => "/",
        IROp::FloorDiv => "//",
        IROp::Mod => "%",
        IROp::Pow => "**",
        IROp::MatMul => "@",
        IROp::LShift => "<<",
        IROp::RShift => ">>",
        IROp::BitOr => "|",
        IROp::BitXor => "^",
        IROp::BitAnd => "&",
    }
}

/// `round(x, n)` for a float `x`: entry stack `(x: f64, n: i32)`, exit the
/// rounded f64.
///
/// CPython rounds the exact binary value of `x` to `n` decimal places, halves
/// to even, and returns the double nearest that decimal. The textbook
/// `floor(x * 10**n + 0.5) / 10**n` disagrees wherever `x * 10**n` is inexact:
/// `round(2.675, 2)` is 2.67 because 2.675 is really 2.67499..., which the
/// shortcut rounds to 2.68. So the product is computed exactly, as a double
/// `hi` plus the rounding error `lo` (Dekker's two-product, which needs only
/// multiply and add), and rounded half-to-even with `f64.nearest`. The error
/// term can only matter when `hi` lands exactly on a half: then `lo`'s sign
/// says which side of the tie the true product is on. Dividing the integer
/// result by `10**n` (both exact) gives the correctly rounded quotient, which
/// is the double CPython returns.
///
/// `n` outside 0..=22 traps, since `10**n` is then inexact; a product of 2^52
/// or more is already an integer at that precision, so `x` is returned as is,
/// as CPython does.
fn emit_round_ndigits(func: &mut Function, ctx: &CompilationContext) {
    let n = ctx.temp_local;
    let i = ctx.temp_local + 1;
    let mut f = Vec::new();
    let mut taken = 0;
    for _ in 0..12 {
        match ctx.hold_f64() {
            Some(slot) => {
                f.push(slot);
                taken += 1;
            }
            None => {
                ctx.report("round() is nested too deeply here");
                f.push(ctx.temp_local_f64);
            }
        }
    }
    let [x, p, hi, lo, t, ahi, alo, bhi, blo, r, d, res] = [
        f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], f[8], f[9], f[10], f[11],
    ];
    let c = f64_const(134_217_729.0); // 2^27 + 1, the Veltkamp split constant
    use Instruction as I;

    func.instruction(&I::LocalSet(n));
    func.instruction(&I::LocalSet(x));

    // 0 <= n <= 22, unsigned-compare as one test.
    func.instruction(&I::LocalGet(n));
    func.instruction(&I::I32Const(22));
    func.instruction(&I::I32GtU);
    func.instruction(&I::If(BlockType::Empty));
    func.instruction(&I::Unreachable);
    func.instruction(&I::End);

    // p = 10 ** n, exact for these n.
    func.instruction(&I::F64Const(f64_const(1.0)));
    func.instruction(&I::LocalSet(p));
    func.instruction(&I::I32Const(0));
    func.instruction(&I::LocalSet(i));
    func.instruction(&I::Block(BlockType::Empty));
    func.instruction(&I::Loop(BlockType::Empty));
    func.instruction(&I::LocalGet(i));
    func.instruction(&I::LocalGet(n));
    func.instruction(&I::I32GeS);
    func.instruction(&I::BrIf(1));
    func.instruction(&I::LocalGet(p));
    func.instruction(&I::F64Const(f64_const(10.0)));
    func.instruction(&I::F64Mul);
    func.instruction(&I::LocalSet(p));
    func.instruction(&I::LocalGet(i));
    func.instruction(&I::I32Const(1));
    func.instruction(&I::I32Add);
    func.instruction(&I::LocalSet(i));
    func.instruction(&I::Br(0));
    func.instruction(&I::End);
    func.instruction(&I::End);

    func.instruction(&I::Block(BlockType::Empty));
    // hi = x * p
    func.instruction(&I::LocalGet(x));
    func.instruction(&I::LocalGet(p));
    func.instruction(&I::F64Mul);
    func.instruction(&I::LocalSet(hi));
    // Not below 2^52 (or NaN, or infinite): nothing to round.
    func.instruction(&I::LocalGet(x));
    func.instruction(&I::LocalSet(res));
    func.instruction(&I::LocalGet(hi));
    func.instruction(&I::F64Abs);
    func.instruction(&I::F64Const(f64_const(4_503_599_627_370_496.0)));
    func.instruction(&I::F64Lt);
    func.instruction(&I::I32Eqz);
    func.instruction(&I::BrIf(0));

    // Split x and p into halves that multiply exactly.
    for (v, vhi, vlo) in [(x, ahi, alo), (p, bhi, blo)] {
        func.instruction(&I::F64Const(c));
        func.instruction(&I::LocalGet(v));
        func.instruction(&I::F64Mul);
        func.instruction(&I::LocalTee(t));
        func.instruction(&I::LocalGet(t));
        func.instruction(&I::LocalGet(v));
        func.instruction(&I::F64Sub);
        func.instruction(&I::F64Sub);
        func.instruction(&I::LocalSet(vhi));
        func.instruction(&I::LocalGet(v));
        func.instruction(&I::LocalGet(vhi));
        func.instruction(&I::F64Sub);
        func.instruction(&I::LocalSet(vlo));
    }
    // lo = ((ahi*bhi - hi) + ahi*blo + alo*bhi) + alo*blo
    func.instruction(&I::LocalGet(ahi));
    func.instruction(&I::LocalGet(bhi));
    func.instruction(&I::F64Mul);
    func.instruction(&I::LocalGet(hi));
    func.instruction(&I::F64Sub);
    func.instruction(&I::LocalGet(ahi));
    func.instruction(&I::LocalGet(blo));
    func.instruction(&I::F64Mul);
    func.instruction(&I::F64Add);
    func.instruction(&I::LocalGet(alo));
    func.instruction(&I::LocalGet(bhi));
    func.instruction(&I::F64Mul);
    func.instruction(&I::F64Add);
    func.instruction(&I::LocalGet(alo));
    func.instruction(&I::LocalGet(blo));
    func.instruction(&I::F64Mul);
    func.instruction(&I::F64Add);
    func.instruction(&I::LocalSet(lo));

    // r = nearest(hi); d = hi - r, exact.
    func.instruction(&I::LocalGet(hi));
    func.instruction(&I::F64Nearest);
    func.instruction(&I::LocalSet(r));
    func.instruction(&I::LocalGet(hi));
    func.instruction(&I::LocalGet(r));
    func.instruction(&I::F64Sub);
    func.instruction(&I::LocalSet(d));
    // At a tie, `nearest` took the even side; the true product is off the tie
    // in the direction of `lo`.
    for (half, lo_test, step) in [(0.5, I::F64Gt, 1.0), (-0.5, I::F64Lt, -1.0)] {
        func.instruction(&I::LocalGet(d));
        func.instruction(&I::F64Const(f64_const(half)));
        func.instruction(&I::F64Eq);
        func.instruction(&I::LocalGet(lo));
        func.instruction(&I::F64Const(f64_const(0.0)));
        func.instruction(&lo_test);
        func.instruction(&I::I32And);
        func.instruction(&I::If(BlockType::Empty));
        func.instruction(&I::LocalGet(r));
        func.instruction(&I::F64Const(f64_const(step)));
        func.instruction(&I::F64Add);
        func.instruction(&I::LocalSet(r));
        func.instruction(&I::End);
    }
    func.instruction(&I::LocalGet(r));
    func.instruction(&I::LocalGet(p));
    func.instruction(&I::F64Div);
    func.instruction(&I::LocalSet(res));
    func.instruction(&I::End);

    func.instruction(&I::LocalGet(res));
    for _ in 0..taken {
        ctx.release_held_f64();
    }
}

/// Build the one-character string at byte `idx` of the string at `off` (the
/// whole UTF-8 character that starts there), and leave its `(offset, length)`
/// pair on the stack.
///
/// The result is a fresh blob with its own length prefix, never a pointer
/// into the source. Codegen narrows a string to its offset wherever it must
/// fit in a word (a function argument, a field, a collection element, a dict
/// key, a return value) and recovers the length from the four bytes before
/// it, so an interior pointer reads the preceding characters as its length.
/// Both `s[i]` and `for ch in s` go through here so they cannot drift apart.
/// It copied one byte, so a character outside ASCII came back cut in half.
///
/// Straight-line, no nested emission: `byte`, `one`, and `blk` may be ordinary
/// scratch locals.
pub(crate) fn emit_char_at(
    func: &mut Function,
    ctx: &CompilationContext,
    off: u32,
    idx: u32,
    byte: u32,
    one: u32,
    blk: u32,
) {
    crate::compiler::unicode::emit_width_at(func, off, idx, byte);
    func.instruction(&Instruction::LocalSet(one));
    emit_alloc_string(func, ctx, one, blk);
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(one));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(one));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(one));
}

/// `int(s)` for a string or bytes `s`, whose `(offset, length)` pair is on the
/// stack; leaves the parsed `i32`.
///
/// CPython's base-10 rules: surrounding whitespace is ignored, one optional
/// sign, then one or more digits with single underscores allowed between them
/// (`"1_000"`). Anything else raises `ValueError`, which is catchable. A byte at
/// or above 0x80 traps instead: CPython accepts every Unicode decimal digit
/// (`int("١٢")` is 12), and raising where it answers would be a divergence a
/// handler could silently act on. Values past 32 bits wrap, as all `int`
/// arithmetic here does.
fn emit_parse_int(func: &mut Function, ctx: &CompilationContext) {
    let off = ctx.temp_local;
    let end = ctx.temp_local + 1;
    let i = ctx.temp_local + 2;
    let c = ctx.temp_local + 3;
    let sign = ctx.temp_local + 4;
    let acc = ctx.temp_local + 5;
    let digits = ctx.temp_local + 6;
    let last_us = ctx.temp_local + 7;
    let bad = ctx.temp_local + 8;
    // Push `byte[at]`.
    let byte = |func: &mut Function, at: u32| {
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(at));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Load8U(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    };
    // ASCII whitespace: space, or \t \n \v \f \r (9..=13). Consumes the byte.
    let is_ws = |func: &mut Function| {
        func.instruction(&Instruction::LocalTee(c));
        func.instruction(&Instruction::I32Const(32));
        func.instruction(&Instruction::I32Eq);
        func.instruction(&Instruction::LocalGet(c));
        func.instruction(&Instruction::I32Const(9));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Const(5));
        func.instruction(&Instruction::I32LtU);
        func.instruction(&Instruction::I32Or);
    };

    func.instruction(&Instruction::LocalSet(end)); // length
    func.instruction(&Instruction::LocalSet(off));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    // Skip leading whitespace.
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    byte(func, i);
    is_ws(func);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Skip trailing whitespace: look at end - 1 while end > i.
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::BrIf(1));
    // byte[end - 1]
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load8U(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    is_ws(func);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(end));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Optional sign.
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(BlockType::Empty));
    byte(func, i);
    func.instruction(&Instruction::LocalTee(c));
    func.instruction(&Instruction::I32Const(b'-' as i32));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(sign));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(c));
    func.instruction(&Instruction::I32Const(b'-' as i32));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::LocalGet(c));
    func.instruction(&Instruction::I32Const(b'+' as i32));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Digits, with single underscores between them.
    for local in [acc, digits, last_us, bad] {
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(local));
    }
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(end));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    byte(func, i);
    func.instruction(&Instruction::LocalTee(c));
    func.instruction(&Instruction::I32Const(0x80));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);
    // c == '_'
    func.instruction(&Instruction::LocalGet(c));
    func.instruction(&Instruction::I32Const(b'_' as i32));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    //   bad |= digits == 0 || last_us
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::LocalGet(last_us));
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(bad));
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalSet(bad));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(last_us));
    func.instruction(&Instruction::Else);
    //   c - '0' < 10: a digit
    func.instruction(&Instruction::LocalGet(c));
    func.instruction(&Instruction::I32Const(b'0' as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(acc));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalGet(c));
    func.instruction(&Instruction::I32Const(b'0' as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(acc));
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(digits));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(last_us));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(bad));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // No digits, or a trailing underscore, is a ValueError too, naming the
    // string as CPython does.
    func.instruction(&Instruction::LocalGet(bad));
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(last_us));
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_raise(
        func,
        ctx,
        "ValueError",
        crate::compiler::exceptions::ExcArg::Prefix(
            "invalid literal for int() with base 10: ",
            off,
            IRType::String,
        ),
        1,
    );
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(acc));
    func.instruction(&Instruction::LocalGet(sign));
    func.instruction(&Instruction::I32Mul);
}

/// Allocate a string block of `len_local` bytes and leave its data pointer in
/// `blk_local`: `[len:i32][bytes...][NUL]`, with the returned pointer past the
/// length prefix so it is a plain string offset.
fn emit_alloc_string(
    func: &mut Function,
    ctx: &CompilationContext,
    len_local: u32,
    blk_local: u32,
) {
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::LocalGet(len_local));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(blk_local));
    func.instruction(&Instruction::LocalGet(blk_local));
    func.instruction(&Instruction::LocalGet(len_local));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(blk_local));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(blk_local));
}

/// `str.replace(old, new)` on runtime strings.
///
/// Entry stack: receiver, old, and new, each an `(offset, length)` pair.
/// Exit stack: the result's `(offset, length)`.
fn emit_string_replace(func: &mut Function, ctx: &CompilationContext) {
    let h_off = ctx.temp_local;
    let h_len = ctx.temp_local + 1;
    let o_off = ctx.temp_local + 2;
    let o_len = ctx.temp_local + 3;
    let n_off = ctx.temp_local + 4;
    let n_len = ctx.temp_local + 5;
    let i = ctx.temp_local + 6;
    let out = ctx.temp_local + 7;
    let k = ctx.temp_local + 8;
    let blk = ctx.temp_local + 9;
    let wpos = ctx.temp_local + 10;
    let cap = ctx.temp_local + 11;

    func.instruction(&Instruction::LocalSet(n_len));
    func.instruction(&Instruction::LocalSet(n_off));
    func.instruction(&Instruction::LocalSet(o_len));
    func.instruction(&Instruction::LocalSet(o_off));
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalSet(h_off));

    // An empty `old` inserts `new` between every character in Python. That is a
    // different loop shape, and answering something else would be a silent
    // wrong answer, so it traps instead.
    func.instruction(&Instruction::LocalGet(o_len));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);

    // Worst case every character is a match: len * new_len, plus the
    // unmatched characters. Sized once rather than counting in a first pass;
    // the allocator is a bump pointer, so the slack costs nothing but address
    // space.
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(cap));
    emit_alloc_string(func, ctx, cap, blk);

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(wpos));

    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    emit_match_at(func, h_off, h_len, o_off, o_len, i, out, k);
    func.instruction(&Instruction::LocalGet(out));
    func.instruction(&Instruction::If(BlockType::Empty));
    // memory.copy(blk + wpos, n_off, n_len); advance both cursors.
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(n_off));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::LocalGet(n_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(wpos));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(o_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(h_off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(wpos));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Fix the length prefix down to what was actually written, NUL-terminate.
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(wpos));
}

/// `str.split()` / `str.split(sep)` on runtime strings, returning a real list
/// of string offsets.
///
/// Entry stack: the receiver, plus the separator when `has_sep`. Exit stack: a
/// list pointer. With no separator Python splits on runs of whitespace and
/// drops leading and trailing empties; with one it splits on each occurrence
/// and keeps empty fields.
fn emit_string_split(func: &mut Function, ctx: &CompilationContext, has_sep: bool) {
    let h_off = ctx.temp_local;
    let h_len = ctx.temp_local + 1;
    let s_off = ctx.temp_local + 2;
    let s_len = ctx.temp_local + 3;
    let i = ctx.temp_local + 4;
    let out = ctx.temp_local + 5;
    let k = ctx.temp_local + 6;
    let list_ptr = ctx.temp_local + 7;
    let count = ctx.temp_local + 8;
    let start = ctx.temp_local + 9;
    let piece = ctx.temp_local + 10;
    let plen = ctx.temp_local + 11;
    let b = ctx.temp_local + 12;

    if has_sep {
        func.instruction(&Instruction::LocalSet(s_len));
        func.instruction(&Instruction::LocalSet(s_off));
    }
    func.instruction(&Instruction::LocalSet(h_len));
    func.instruction(&Instruction::LocalSet(h_off));

    if has_sep {
        // An empty separator is a ValueError in Python; trapping keeps it loud
        // rather than looping forever.
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::Unreachable);
        func.instruction(&Instruction::End);
    }

    // A string of length n splits into at most n + 1 pieces, so the list is
    // sized for the worst case up front and its length word written at the end.
    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(list_ptr));
    store_runtime_data_ptr(func, list_ptr);
    func.instruction(&Instruction::LocalGet(list_ptr));
    func.instruction(&Instruction::LocalGet(h_len));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));

    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));

    if has_sep {
        // Past the end: emit the final field (which may be empty) and stop.
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(h_len));
        func.instruction(&Instruction::I32GtU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(h_len));
        func.instruction(&Instruction::I32Eq);
        func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::Else);
        emit_match_at(func, h_off, h_len, s_off, s_len, i, out, k);
        func.instruction(&Instruction::LocalGet(out));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::If(BlockType::Empty));
        // piece = h[start..i]
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(plen));
        emit_alloc_string(func, ctx, plen, piece);
        func.instruction(&Instruction::LocalGet(piece));
        func.instruction(&Instruction::LocalGet(h_off));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(plen));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        func.instruction(&Instruction::LocalGet(piece));
        func.instruction(&Instruction::LocalGet(plen));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Store8(byte_arg()));
        // slot[count] = piece
        func.instruction(&Instruction::LocalGet(list_ptr));
        emit_data_base(func);
        func.instruction(&Instruction::LocalGet(count));
        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(piece));
        func.instruction(&Instruction::I32Store(slot_arg()));
        func.instruction(&Instruction::LocalGet(count));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(count));
        // Skip the separator and start the next field after it.
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(s_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalSet(start));
        func.instruction(&Instruction::Else);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::End);
    } else {
        // Whitespace mode: skip any run of blanks, then take everything up to
        // the next blank as a field. Leading and trailing runs produce no
        // fields, which is what Python's argument-less split() does.
        let space_at = ctx.helper(crate::compiler::runtime::Helper::TrimAt {
            set: false,
            before: false,
        });
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(h_len));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        // The width of the whitespace character at i (CPython's isspace),
        // or 0.
        func.instruction(&Instruction::LocalGet(h_off));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::Call(space_at));
        func.instruction(&Instruction::LocalTee(b));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(b));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(h_len));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalSet(start));

        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(h_len));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(h_off));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::Call(space_at));
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(plen));
        emit_alloc_string(func, ctx, plen, piece);
        func.instruction(&Instruction::LocalGet(piece));
        func.instruction(&Instruction::LocalGet(h_off));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(plen));
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        func.instruction(&Instruction::LocalGet(piece));
        func.instruction(&Instruction::LocalGet(plen));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Store8(byte_arg()));
        func.instruction(&Instruction::LocalGet(list_ptr));
        emit_data_base(func);
        func.instruction(&Instruction::LocalGet(count));
        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(piece));
        func.instruction(&Instruction::I32Store(slot_arg()));
        func.instruction(&Instruction::LocalGet(count));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(count));
    }

    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(list_ptr));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(list_ptr));
}

/// `sep.join(parts)` on runtime values.
///
/// Entry stack: the separator's `(offset, length)` with the list pointer above
/// it. Exit stack: the joined string's `(offset, length)`. A list element is a
/// string offset, so each part's length comes from its own prefix word.
fn emit_string_join(func: &mut Function, ctx: &CompilationContext) {
    let s_off = ctx.temp_local;
    let s_len = ctx.temp_local + 1;
    let list_ptr = ctx.temp_local + 2;
    let n = ctx.temp_local + 3;
    let i = ctx.temp_local + 4;
    let total = ctx.temp_local + 5;
    let elem = ctx.temp_local + 6;
    let elen = ctx.temp_local + 7;
    let blk = ctx.temp_local + 8;
    let wpos = ctx.temp_local + 9;
    let data = ctx.temp_local + 10;

    func.instruction(&Instruction::LocalSet(list_ptr));
    func.instruction(&Instruction::LocalSet(s_len));
    func.instruction(&Instruction::LocalSet(s_off));

    func.instruction(&Instruction::LocalGet(list_ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::LocalGet(list_ptr));
    emit_data_base(func);
    func.instruction(&Instruction::LocalSet(data));

    // Pass one: the exact result length, so the block is sized rather than
    // guessed. total = sum(len(part)) + sep_len * (n - 1).
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(total));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(elem));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::LocalGet(elem));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));
    func.instruction(&Instruction::End);

    emit_alloc_string(func, ctx, total, blk);

    // Pass two: copy each part, with the separator before all but the first.
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(wpos));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(s_off));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::LocalGet(s_len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(wpos));
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(elem));
    func.instruction(&Instruction::LocalGet(elem));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(elen));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(elem));
    func.instruction(&Instruction::LocalGet(elen));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(wpos));
    func.instruction(&Instruction::LocalGet(elen));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(wpos));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(total));
}

/// Which way a padding method distributes the fill.
#[derive(Clone, Copy, PartialEq)]
enum PadMode {
    Left,
    Right,
    Center,
}

/// `str.ljust(width)` / `rjust(width)` / `center(width)` on runtime strings.
///
/// Entry stack: the receiver's `(offset, length)` with the width above it.
/// A string already at least `width` long is returned as a copy, matching
/// Python, which never truncates here. The fill is always a space; a custom
/// fill character is rejected by the caller.
fn emit_string_pad(func: &mut Function, ctx: &CompilationContext, mode: PadMode, has_fill: bool) {
    let off = ctx.temp_local;
    let len = ctx.temp_local + 1;
    let width = ctx.temp_local + 2;
    let newlen = ctx.temp_local + 3;
    let blk = ctx.temp_local + 4;
    let left = ctx.temp_local + 5;
    let i = ctx.temp_local + 6;
    let fill = ctx.temp_local + 7;
    let fill_len = ctx.temp_local + 8;
    let marg = ctx.temp_local + 9;
    let pos = ctx.temp_local + 10;
    let char_len = ctx.helper(crate::compiler::runtime::Helper::CharLen);

    // The fill character: one character, as CPython requires (it raises
    // TypeError for any other length), else a space. Widths count code
    // points, and a fill character may take several bytes; both were bytes.
    if has_fill {
        func.instruction(&Instruction::LocalSet(fill_len));
        func.instruction(&Instruction::LocalTee(fill));
        func.instruction(&Instruction::Call(char_len));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Ne);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::Unreachable);
        func.instruction(&Instruction::End);
    } else {
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::LocalSet(fill_len));
    }
    func.instruction(&Instruction::LocalSet(width));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));

    // marg = max(width - code points, 0)
    func.instruction(&Instruction::LocalGet(width));
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::Call(char_len));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalTee(marg));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(marg));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(marg));

    match mode {
        PadMode::Left => {
            func.instruction(&Instruction::I32Const(0));
        }
        PadMode::Right => {
            func.instruction(&Instruction::LocalGet(marg));
        }
        PadMode::Center => {
            // CPython's left margin is marg // 2 + (marg & width & 1), so an
            // odd margin puts its extra space on the left when the width is
            // odd and on the right when it is even.
            func.instruction(&Instruction::LocalGet(marg));
            func.instruction(&Instruction::I32Const(2));
            func.instruction(&Instruction::I32DivU);
            func.instruction(&Instruction::LocalGet(marg));
            func.instruction(&Instruction::LocalGet(width));
            func.instruction(&Instruction::I32And);
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32And);
            func.instruction(&Instruction::I32Add);
        }
    }
    func.instruction(&Instruction::LocalSet(left));

    // newlen = len + marg * fill bytes
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalGet(marg));
    func.instruction(&Instruction::LocalGet(fill_len));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(newlen));
    emit_alloc_string(func, ctx, newlen, blk);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(pos));

    // `count` fill characters at pos.
    let fills = |func: &mut Function, count: &dyn Fn(&mut Function)| {
        count(func);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32LeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(blk));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        if has_fill {
            func.instruction(&Instruction::LocalGet(fill));
            func.instruction(&Instruction::LocalGet(fill_len));
            func.instruction(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
        } else {
            func.instruction(&Instruction::I32Const(b' ' as i32));
            func.instruction(&Instruction::I32Store8(byte_arg()));
        }
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::LocalGet(fill_len));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(i));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    };
    fills(func, &|func| {
        func.instruction(&Instruction::LocalGet(left));
    });
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(pos));
    fills(func, &|func| {
        func.instruction(&Instruction::LocalGet(marg));
        func.instruction(&Instruction::LocalGet(left));
        func.instruction(&Instruction::I32Sub);
    });

    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(newlen));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(newlen));
}

/// Reduce an already-emitted value to a single i32 that is 1 when Python would
/// call it truthy, and 0 otherwise.
///
/// Every caller that tests a value has to go through this. A `str` is an
/// `(offset, length)` pair, so testing it directly left the offset on the
/// stack and the module failed to validate; a collection is a pointer, which
/// is never null, so an empty list tested as True where Python says False.
/// Int, bool, and an instance pointer are already their own truth value.
pub(crate) fn emit_truthiness(func: &mut Function, ctx: &CompilationContext, ty: &IRType) {
    match ty {
        IRType::String | IRType::Bytes => {
            // (offset, length) -> length != 0
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32Ne);
        }
        IRType::List(_)
        | IRType::Dict(_, _)
        | IRType::Set(_)
        | IRType::Tuple(_)
        | IRType::VarTuple(_) => {
            // A collection is truthy when it holds something, so the count in
            // the header word decides, not the pointer.
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32Ne);
        }
        IRType::Float => {
            func.instruction(&Instruction::F64Const(f64_const(0.0)));
            func.instruction(&Instruction::F64Ne);
        }
        // None is false, and a number is its own truth.
        IRType::Optional(inner) if boxed_scalar(ty).is_some() => {
            let Some(cell) = ctx.hold() else {
                report_literal_too_deep(func, ctx, "an optional number");
                return;
            };
            func.instruction(&Instruction::LocalTee(cell));
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            func.instruction(&Instruction::LocalGet(cell));
            if matches!(**inner, IRType::Float) {
                func.instruction(&Instruction::F64Load(mem_off(0)));
                func.instruction(&Instruction::F64Const(f64_const(0.0)));
                func.instruction(&Instruction::F64Ne);
            } else {
                func.instruction(&Instruction::I32Load(mem_off(0)));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Ne);
            }
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::End);
            ctx.release_held();
        }
        _ => {
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32Ne);
        }
    }
}

/// Three-way compare two strings by content, given their data offsets, leaving
/// -1, 0, or 1 on the stack. Ordering is bytewise, which matches Python for
/// ASCII; the case transforms already trap on a byte at or above 0x80, so a
/// non-ASCII string cannot reach a sort through them.
///
/// Uses the top of the scratch run so it can be called from inside a sort loop,
/// which owns the lower locals.
fn emit_str_cmp(func: &mut Function, ctx: &CompilationContext, a_off: u32, b_off: u32) {
    let la = ctx.temp_local + 24;
    let lb = ctx.temp_local + 25;
    let m = ctx.temp_local + 26;
    let i = ctx.temp_local + 27;
    let out = ctx.temp_local + 28;
    let ca = ctx.temp_local + 29;
    let cb = ctx.temp_local + 30;

    func.instruction(&Instruction::LocalGet(a_off));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(la));
    func.instruction(&Instruction::LocalGet(b_off));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(lb));

    // m = min(la, lb): only the shared prefix can differ byte for byte.
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalSet(m));

    // Equal through the shared prefix: the shorter string sorts first.
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalSet(out));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(a_off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalSet(ca));
    func.instruction(&Instruction::LocalGet(b_off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalSet(cb));
    func.instruction(&Instruction::LocalGet(ca));
    func.instruction(&Instruction::LocalGet(cb));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(ca));
    func.instruction(&Instruction::LocalGet(cb));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(out));
}

/// Three-way compare two sort keys of type `ty`, given their slot addresses in
/// `a_addr` and `b_addr`, leaving -1, 0, or 1 on the stack.
///
/// A tuple key compares lexicographically, member by member, which is what
/// makes `key=lambda kv: (-kv[1], kv[0])` order by count and then break ties on
/// the word the way Python does.
fn emit_key_cmp(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    a_addr: u32,
    b_addr: u32,
) {
    match ty {
        IRType::Float => {
            let av = ctx.temp_local_f64_2;
            let bv = ctx.temp_local_f64_3;
            func.instruction(&Instruction::LocalGet(a_addr));
            func.instruction(&Instruction::F64Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(av));
            func.instruction(&Instruction::LocalGet(b_addr));
            func.instruction(&Instruction::F64Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(bv));
            func.instruction(&Instruction::LocalGet(av));
            func.instruction(&Instruction::LocalGet(bv));
            func.instruction(&Instruction::F64Lt);
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            func.instruction(&Instruction::I32Const(-1));
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::LocalGet(av));
            func.instruction(&Instruction::LocalGet(bv));
            func.instruction(&Instruction::F64Gt);
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
        }
        IRType::String | IRType::Bytes => {
            let ao = ctx.temp_local + 22;
            let bo = ctx.temp_local + 23;
            func.instruction(&Instruction::LocalGet(a_addr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(ao));
            func.instruction(&Instruction::LocalGet(b_addr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(bo));
            emit_str_cmp(func, ctx, ao, bo);
        }
        IRType::Tuple(members) => {
            // Each slot holds a pointer to the tuple's own region; walk the
            // members in order and stop at the first that differs.
            let ap = ctx.temp_local + 18;
            let bp = ctx.temp_local + 19;
            let acc = ctx.temp_local + 20;
            let am = ctx.temp_local + 21;
            let bm = ctx.temp_local + 31;
            func.instruction(&Instruction::LocalGet(a_addr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            emit_data_base(func);
            func.instruction(&Instruction::LocalSet(ap));
            func.instruction(&Instruction::LocalGet(b_addr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            emit_data_base(func);
            func.instruction(&Instruction::LocalSet(bp));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(acc));
            for (idx, member) in members.iter().enumerate() {
                func.instruction(&Instruction::LocalGet(acc));
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ap));
                func.instruction(&Instruction::I32Const(
                    (idx as u32 * COLLECTION_SLOT) as i32,
                ));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(am));
                func.instruction(&Instruction::LocalGet(bp));
                func.instruction(&Instruction::I32Const(
                    (idx as u32 * COLLECTION_SLOT) as i32,
                ));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(bm));
                emit_key_cmp(func, ctx, member, am, bm);
                func.instruction(&Instruction::LocalSet(acc));
                func.instruction(&Instruction::End);
            }
            func.instruction(&Instruction::LocalGet(acc));
        }
        _ => {
            let av = ctx.temp_local + 22;
            let bv = ctx.temp_local + 23;
            func.instruction(&Instruction::LocalGet(a_addr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(av));
            func.instruction(&Instruction::LocalGet(b_addr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(bv));
            func.instruction(&Instruction::LocalGet(av));
            func.instruction(&Instruction::LocalGet(bv));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            func.instruction(&Instruction::I32Const(-1));
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::LocalGet(av));
            func.instruction(&Instruction::LocalGet(bv));
            func.instruction(&Instruction::I32GtS);
            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
        }
    }
}

/// Best-effort result type of a sort key applied to an element of type
/// `elem_ty`.
///
/// A closure call reports `Unknown`, so a key's type has to come from the
/// lambda's own body. Only the shapes a sort key actually takes are covered:
/// the element itself, a constant, a member of it, a negation, and a tuple of
/// those, which is what `key=lambda kv: (-kv[1], kv[0])` needs in order to
/// compare by count and then break ties on the word.
fn infer_key_type(key: &IRExpr, elem_ty: &IRType, ctx: &CompilationContext) -> IRType {
    match key {
        // Before the finalize pass, or for a lambda written inline that has not
        // been lifted.
        IRExpr::Lambda { params, body, .. } => match params.first() {
            Some(param) => infer_key_body(body, &param.name, elem_ty),
            None => IRType::Unknown,
        },
        // After lifting, which is what code generation actually sees.
        IRExpr::ClosureMake { lambda_name, .. } => match ctx.lambda_bodies.get(lambda_name) {
            Some((param, body)) => infer_key_body(body, param, elem_ty),
            None => IRType::Unknown,
        },
        _ => IRType::Unknown,
    }
}

fn infer_key_body(expr: &IRExpr, param: &str, elem_ty: &IRType) -> IRType {
    match expr {
        IRExpr::Const(IRConstant::Int(_)) => IRType::Int,
        IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
        IRExpr::Const(IRConstant::Bool(_)) => IRType::Bool,
        IRExpr::Const(IRConstant::String(_)) => IRType::String,
        IRExpr::Variable(name) | IRExpr::Param(name) if name == param => elem_ty.clone(),
        IRExpr::UnaryOp { operand, .. } => infer_key_body(operand, param, elem_ty),
        IRExpr::Conditional { body, orelse, .. } => {
            match (
                infer_key_body(body, param, elem_ty),
                infer_key_body(orelse, param, elem_ty),
            ) {
                (a, b) if a == b => a,
                _ => IRType::Unknown,
            }
        }
        IRExpr::BinaryOp { left, right, .. } => {
            let lt = infer_key_body(left, param, elem_ty);
            let rt = infer_key_body(right, param, elem_ty);
            if matches!(lt, IRType::Float) || matches!(rt, IRType::Float) {
                IRType::Float
            } else if matches!(lt, IRType::String) {
                IRType::String
            } else {
                IRType::Int
            }
        }
        IRExpr::TupleLiteral(items) => IRType::Tuple(
            items
                .iter()
                .map(|item| infer_key_body(item, param, elem_ty))
                .collect(),
        ),
        // The builtins a key commonly ends in.
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => match function_name.as_str() {
            "len" | "int" | "ord" => IRType::Int,
            "float" => IRType::Float,
            "str" => IRType::String,
            "bool" => IRType::Bool,
            "abs" | "sum" | "min" | "max" => arguments
                .first()
                .map(|a| infer_key_body(a, param, elem_ty))
                .unwrap_or(IRType::Unknown),
            _ => IRType::Unknown,
        },
        // A string method that returns a string keeps the key a string.
        IRExpr::MethodCall {
            method_name,
            object,
            ..
        } => match method_name.as_str() {
            "lower" | "upper" | "strip" | "lstrip" | "rstrip" | "capitalize" | "title"
            | "replace" => infer_key_body(object, param, elem_ty),
            "find" | "index" | "count" => IRType::Int,
            "startswith" | "endswith" => IRType::Bool,
            _ => IRType::Unknown,
        },
        // `kv[0]` on a tuple element takes that member's type; on a list it
        // takes the element type.
        IRExpr::Indexing { container, index } => {
            let base = infer_key_body(container, param, elem_ty);
            match (&base, index.as_ref()) {
                (IRType::Tuple(members), IRExpr::Const(IRConstant::Int(i))) => {
                    members.get(*i as usize).cloned().unwrap_or(IRType::Unknown)
                }
                (IRType::List(inner) | IRType::VarTuple(inner), _) => (**inner).clone(),
                (IRType::String, _) => IRType::String,
                _ => IRType::Unknown,
            }
        }
        _ => IRType::Unknown,
    }
}

/// Whether a sort key's body needs its parameter's *type* to compile
/// correctly, which a lambda parameter does not carry.
///
/// A lifted lambda's parameter is untyped, so `len(kv)`, `kv[0]`, and
/// `kv.method()` inside one read the value as an untyped word: indexing a
/// string offset treats the first four characters as a length, and `len` on a
/// string reads its bytes rather than its prefix. The sort itself is fine, so
/// the key is refused rather than the whole construct, and the message says
/// what to do instead.
fn key_needs_param_type(expr: &IRExpr, param: &str) -> bool {
    fn is_param(expr: &IRExpr, param: &str) -> bool {
        matches!(expr, IRExpr::Variable(n) | IRExpr::Param(n) if n == param)
    }
    match expr {
        IRExpr::Indexing { container, index } => {
            is_param(container, param)
                || key_needs_param_type(container, param)
                || key_needs_param_type(index, param)
        }
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            is_param(object, param)
                || key_needs_param_type(object, param)
                || arguments.iter().any(|a| key_needs_param_type(a, param))
        }
        IRExpr::FunctionCall { arguments, .. } => arguments
            .iter()
            .any(|a| is_param(a, param) || key_needs_param_type(a, param)),
        IRExpr::UnaryOp { operand, .. } => key_needs_param_type(operand, param),
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => [condition, body, orelse]
            .iter()
            .any(|e| key_needs_param_type(e, param)),
        IRExpr::BinaryOp { left, right, .. } => {
            key_needs_param_type(left, param) || key_needs_param_type(right, param)
        }
        IRExpr::TupleLiteral(items) | IRExpr::ListLiteral(items) => {
            items.iter().any(|i| key_needs_param_type(i, param))
        }
        _ => false,
    }
}

/// MemArg for a single-byte access. A byte's natural alignment is 1, so the
/// alignment hint must be 0; anything larger is rejected by the validator.
pub(crate) fn byte_arg() -> MemArg {
    MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }
}

pub(crate) fn mem_off(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn mem_off_aligned(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

/// A value held as one integer-shaped word, which an untyped word stands in
/// for without losing anything.
fn is_word_type(ty: &IRType) -> bool {
    matches!(
        ty,
        IRType::Int | IRType::Bool | IRType::None | IRType::Unknown | IRType::Callable { .. }
    )
}

/// Whether a variable typed `held` can be rebound to a `value` without a
/// reader of it at `held` misreading it: the same kind of object, whose
/// elements agree wherever both are known. Classes are checked by the caller.
fn same_shape(held: &IRType, value: &IRType) -> bool {
    let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
    match (held, value) {
        (IRType::Class(_), IRType::Class(_)) => true,
        (IRType::List(a), IRType::List(b))
        | (IRType::Set(a), IRType::Set(b))
        | (IRType::VarTuple(a), IRType::VarTuple(b)) => a == b || open(a) || open(b),
        (IRType::Dict(ak, av), IRType::Dict(bk, bv)) => {
            (ak == bk || open(ak) || open(bk)) && (av == bv || open(av) || open(bv))
        }
        _ => false,
    }
}

/// Resolve a comprehension helper local reserved by the scan pass.
fn comp_local(ctx: &CompilationContext, name: String) -> u32 {
    ctx.get_local_index(&name)
        .unwrap_or_else(|| panic!("comprehension helper local {name} not reserved"))
}

/// Push the run-time element count of the iterable stashed in `ptr`. List-like
/// iterables (lists, tuples — same `[len][slots]` layout) load the header;
/// ranges compute their trip count from `[start][stop][step]`, clamped at 0
/// so an empty range (e.g. `range(5, 0)`) contributes nothing.
/// Push the number of values the range whose pointer is in `ptr` yields.
pub(crate) fn emit_range_count(func: &mut Function, ctx: &CompilationContext, ptr: u32) {
    emit_comp_iterable_len(func, ctx, true, ptr);
}

fn emit_comp_iterable_len(func: &mut Function, ctx: &CompilationContext, is_range: bool, ptr: u32) {
    if !is_range {
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(slot_arg()));
        return;
    }

    // step > 0 ? (stop - start + step - 1) / step
    //          : (start - stop - step - 1) / (-step)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(8)));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(4)));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(0)));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(8)));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(8)));
    func.instruction(&Instruction::I32DivS);
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(0)));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(4)));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(8)));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(8)));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32DivS);
    func.instruction(&Instruction::End);

    // Clamp a negative trip count (empty range) to zero.
    func.instruction(&Instruction::LocalTee(ctx.temp_local));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
}

/// Emit the nested generator loops of a comprehension, calling `innermost`
/// once per innermost iteration (with the targets of every generator bound).
///
/// Generator 0's iterable must already be evaluated and stashed in its `ptr`
/// local (its type passed as `gen0_ty`) — the capacity phase does this so the
/// fill phase doesn't re-evaluate it. Inner generators' iterables can
/// reference outer targets, so they are (re)evaluated per outer iteration.
/// `with_conditions: false` skips the filters (used by the counting pre-pass,
/// where they could only shrink the capacity estimate).
#[allow(clippy::too_many_arguments)]
fn emit_comp_loops(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    generators: &[IRGenerator],
    g: usize,
    depth: u32,
    gen0_ty: &IRType,
    with_conditions: bool,
    innermost: &mut dyn FnMut(&mut Function),
) {
    if g == generators.len() {
        innermost(func);
        return;
    }
    let generator = &generators[g];
    let ptr = comp_local(ctx, comp_gen_local_name("ptr", depth, g));
    let idx = comp_local(ctx, comp_gen_local_name("idx", depth, g));
    let len = comp_local(ctx, comp_gen_local_name("len", depth, g));

    let ty = if g == 0 {
        gen0_ty.clone()
    } else {
        let t = emit_expr(&generator.iterable, func, ctx, memory_layout, None);
        let t = emit_as_sequence(func, ctx, t);
        narrow_element_to_word(func, &t);
        func.instruction(&Instruction::LocalSet(ptr));
        t
    };

    let n_conds = if with_conditions {
        generator.conditions.len()
    } else {
        0
    };

    if matches!(ty, IRType::Range) {
        // Iterate the range by stepping the target itself (mirrors the `for`
        // statement's range path); `idx`/`len` are unused here.
        let target = comp_local(ctx, generator.targets[0].clone());

        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(0)));
        func.instruction(&Instruction::LocalSet(target));

        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        ctx.enter_expr_blocks(2);

        // Exit test depends on the runtime sign of step.
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(8)));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32GtS);
        func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        func.instruction(&Instruction::LocalGet(target));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(4)));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::Else);
        func.instruction(&Instruction::LocalGet(target));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(4)));
        func.instruction(&Instruction::I32LeS);
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::BrIf(1));

        // Filters guard everything inside this iteration.
        if with_conditions {
            for condition in &generator.conditions {
                emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
                func.instruction(&Instruction::If(BlockType::Empty));
                ctx.enter_expr_blocks(1);
            }
        }
        emit_comp_loops(
            func,
            ctx,
            memory_layout,
            generators,
            g + 1,
            depth,
            gen0_ty,
            with_conditions,
            innermost,
        );
        for _ in 0..n_conds {
            func.instruction(&Instruction::End);
        }
        ctx.exit_expr_blocks(n_conds as u32);

        // target += step
        func.instruction(&Instruction::LocalGet(target));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(8)));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(target));

        func.instruction(&Instruction::Br(0));

        ctx.exit_expr_blocks(2);
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    } else {
        // List-like layout: [len:i32][slot0][slot1]... (lists and tuples).
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalSet(len));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(idx));

        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        ctx.enter_expr_blocks(2);

        func.instruction(&Instruction::LocalGet(idx));
        func.instruction(&Instruction::LocalGet(len));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));

        // Bind the target(s) from slot `idx`.
        if let [target] = generator.targets.as_slice() {
            let target_idx = comp_local(ctx, target.clone());
            let target_is_float = matches!(
                ctx.get_local_info(target).map(|i| &i.var_type),
                Some(IRType::Float)
            );
            // An element's declared type decides how wide the slot is read and,
            // for a string, whether the companion length local has to be filled
            // in as well.
            let elem_ty = match &ty {
                IRType::List(inner) | IRType::VarTuple(inner) => (**inner).clone(),
                _ => IRType::Unknown,
            };
            // A variable the pre-pass could not type is read as an int, so
            // one binding a str would render its address.
            let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
            if ctx
                .get_local_info(target)
                .is_some_and(|i| open(&i.var_type))
                && matches!(elem_ty, IRType::String | IRType::Bytes)
            {
                ctx.report(format!(
                    "the comprehension variable '{target}' binds {} values, but its type could \
                     not be worked out where it is used. Hint: annotate the collection it walks",
                    crate::type_to_string(&elem_ty)
                ));
            }
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            if target_is_float {
                func.instruction(&Instruction::F64Load(mem_off(0)));
            } else {
                func.instruction(&Instruction::I32Load(mem_off(0)));
            }
            func.instruction(&Instruction::LocalSet(target_idx));
            // A string element is stored as its offset alone, so the loop
            // variable's length has to come from the blob's own prefix word.
            // Without this the companion local kept whatever the last string
            // assignment left in it: `len(w)` answered nonsense and an f-string
            // placeholder rendered the pointer as a number.
            if matches!(elem_ty, IRType::String | IRType::Bytes) {
                if let Some(len_idx) = ctx.get_local_index(&strlen_local_name(target)) {
                    func.instruction(&Instruction::LocalGet(target_idx));
                    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::LocalSet(len_idx));
                }
            }
        } else {
            // `for k, v in items`: the element is a tuple pointer; unpack its
            // slots positionally (i32 words — float members keep the existing
            // tuple-unpack limitation).
            let elem = comp_local(ctx, comp_local_name("elem", depth));
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::I32Load(mem_off(0)));
            func.instruction(&Instruction::LocalSet(elem));
            for (j, target) in generator.targets.iter().enumerate() {
                let target_idx = comp_local(ctx, target.clone());
                func.instruction(&Instruction::LocalGet(elem));
                emit_data_base(func);
                func.instruction(&Instruction::I32Load(mem_off(
                    (j as u32 * COLLECTION_SLOT) as u64,
                )));
                func.instruction(&Instruction::LocalSet(target_idx));
                // A string member's slot holds only its offset, so its length
                // has to be recovered from the blob's own prefix word, exactly
                // as the single-target path does.
                if matches!(
                    ctx.get_local_info(target).map(|i| &i.var_type),
                    Some(IRType::String) | Some(IRType::Bytes)
                ) {
                    if let Some(len_idx) = ctx.get_local_index(&strlen_local_name(target)) {
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(len_idx));
                    }
                }
            }
        }

        if with_conditions {
            for condition in &generator.conditions {
                emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
                func.instruction(&Instruction::If(BlockType::Empty));
                ctx.enter_expr_blocks(1);
            }
        }
        emit_comp_loops(
            func,
            ctx,
            memory_layout,
            generators,
            g + 1,
            depth,
            gen0_ty,
            with_conditions,
            innermost,
        );
        for _ in 0..n_conds {
            func.instruction(&Instruction::End);
        }
        ctx.exit_expr_blocks(n_conds as u32);

        func.instruction(&Instruction::LocalGet(idx));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(idx));

        func.instruction(&Instruction::Br(0));

        ctx.exit_expr_blocks(2);
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }
}

/// Call a user-written function, with the bookkeeping an exception needs to
/// travel back out of it.
///
/// A callee that cannot raise (the common case, and every function in a program
/// that never raises) is called exactly as before, so this costs nothing there.
/// One that can raise is bracketed by the call-depth counter and followed by a
/// check: if it came back with an exception pending, its result is meaningless
/// and this frame keeps unwinding instead of using it.
/// Emit a method call that dispatches on the receiver's runtime class.
///
/// The receiver pointer is already on the stack and stays there as the
/// implicit `self`; it is also stashed, because the table index is computed
/// from its class tag *after* the arguments have been emitted, and by then the
/// stack position is buried. The slot is picked by nesting depth, so a virtual
/// call inside another's argument list keeps its own.
///
/// The index is `vtable_base + (class_id - 1) * stride + column`: class ids
/// start at 1 and are stamped into every instance at offset 0 by
/// `__alloc_obj`, so the whole lookup is one load and three arithmetic ops.
#[allow(clippy::too_many_arguments)]
fn emit_virtual_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    param_types: &[IRType],
    ret: IRType,
    column: u32,
    type_index: u32,
) -> IRType {
    let Some(receiver) = ctx.hold() else {
        ctx.report(format!(
            "'{method_name}' is called through more nested virtual method calls than the \
             compiler reserves room for. Hint: assign an intermediate result to a variable"
        ));
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::Unreachable);
        func.instruction(&Instruction::I32Const(0));
        return ret;
    };

    // Keep the receiver on the stack as `self` and stash a copy for the index.
    func.instruction(&Instruction::LocalTee(receiver));

    for (i, arg) in arguments.iter().enumerate() {
        let t = emit_expr(arg, func, ctx, memory_layout, param_types.get(i + 1));
        check_class_arg(ctx, &t, param_types.get(i + 1), i, method_name);
        if matches!(t, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
    }
    ctx.release_held();

    func.instruction(&Instruction::LocalGet(receiver));
    func.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(ctx.vtable_stride as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const((ctx.vtable_base + column) as i32));
    func.instruction(&Instruction::I32Add);

    // The callee is not known here, so the exception bookkeeping `emit_user_call`
    // does for a direct call is applied when *any* implementation of this name
    // can raise: skipping it would strand a propagating exception at the
    // dispatch, which is a wrong answer rather than a slower one.
    let raises = ctx
        .virtual_implementations(column)
        .any(|f| ctx.can_raise.contains(&f));
    if raises {
        emit_call_depth_step(func, 1);
    }
    func.instruction(&Instruction::CallIndirect {
        type_index,
        table_index: 0,
    });
    if raises {
        emit_call_depth_step(func, -1);
        emit_post_call_check(func, ctx);
    }

    finish_user_call(func, ctx, &ret);
    ret
}

/// Dispatch `==` through the vtable when a subclass overrides `__eq__`.
///
/// Unlike a method call, both operands are already on the stack as
/// `(self, other)`, so the receiver is buried. The argument is lifted into a
/// held slot, a copy of the receiver is taken, and the pair is rebuilt. Held
/// slots rather than scratch locals: this is also reached from inside the
/// collection scans (a tuple dict key holding an instance), which keep their
/// own state in the low scratch locals.
pub(crate) fn emit_virtual_compare(
    func: &mut Function,
    ctx: &CompilationContext,
    column: u32,
    type_index: u32,
) {
    let other = crate::compiler::equality::hold(ctx);
    let receiver = crate::compiler::equality::hold(ctx);
    func.instruction(&Instruction::LocalSet(other));
    func.instruction(&Instruction::LocalTee(receiver));
    func.instruction(&Instruction::LocalGet(other));

    func.instruction(&Instruction::LocalGet(receiver));
    func.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(ctx.vtable_stride as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const((ctx.vtable_base + column) as i32));
    func.instruction(&Instruction::I32Add);

    let raises = ctx
        .virtual_implementations(column)
        .any(|f| ctx.can_raise.contains(&f));
    if raises {
        emit_call_depth_step(func, 1);
    }
    func.instruction(&Instruction::CallIndirect {
        type_index,
        table_index: 0,
    });
    if raises {
        emit_call_depth_step(func, -1);
        emit_post_call_check(func, ctx);
    }
    crate::compiler::equality::release(ctx, 2);
}

/// Pick the `@singledispatch` arm registered for `arg_type`: the exact type
/// first, then (as Python's `bool` is an `int`) an `int` arm for a `bool`
/// argument, then for an instance the nearest base class with an arm.
/// `None` means the base function handles it.
fn select_dispatch_arm<'a>(
    ctx: &CompilationContext,
    arms: &'a [(IRType, String)],
    arg_type: &IRType,
) -> Option<&'a str> {
    if let Some((_, name)) = arms.iter().find(|(t, _)| t == arg_type) {
        return Some(name);
    }
    if *arg_type == IRType::Bool {
        if let Some((_, name)) = arms.iter().find(|(t, _)| *t == IRType::Int) {
            return Some(name);
        }
    }
    if let IRType::Class(class_name) = arg_type {
        let mro = ctx
            .get_class_info(class_name)
            .map(|ci| ci.mro.clone())
            .unwrap_or_default();
        for base in mro.iter().skip(1) {
            if let Some((_, name)) = arms.iter().find(|(t, _)| *t == IRType::Class(base.clone())) {
                return Some(name);
            }
        }
    }
    None
}

pub(crate) fn emit_user_call(func: &mut Function, ctx: &CompilationContext, index: u32) {
    if !ctx.can_raise.contains(&index) {
        func.instruction(&Instruction::Call(index));
        return;
    }
    emit_call_depth_step(func, 1);
    func.instruction(&Instruction::Call(index));
    emit_call_depth_step(func, -1);
    emit_post_call_check(func, ctx);
}

/// Locals the sequence-index path uses: the container pointer (or a string's
/// offset) and its length. They sit above the scratch run the expression
/// helpers use, so an index expression nested inside another one does not
/// clobber them.
const INDEX_PTR: u32 = 8;
const INDEX_LEN: u32 = 9;

/// Normalize and bounds-check an index held in `idx` against the length of the
/// collection `ptr` points at, for callers that already have both in locals
/// (index assignment, whose container and index are stashed before the value is
/// evaluated).
pub(crate) fn emit_stored_index_check(
    func: &mut Function,
    ctx: &CompilationContext,
    ptr: u32,
    idx: u32,
    message: &str,
) {
    let len = ctx.temp_local + INDEX_LEN;
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(len));
    emit_index_check(func, ctx, idx, len, message);
}

/// Turn a (pointer, index) pair on the stack into the address of that element,
/// with the index normalized and bounds-checked first. Used by list and tuple
/// indexing, whose layouts are identical.
fn emit_sequence_address(func: &mut Function, ctx: &CompilationContext, message: &str) {
    let idx = ctx.temp_local;
    let ptr = ctx.temp_local + INDEX_PTR;
    let len = ctx.temp_local + INDEX_LEN;
    func.instruction(&Instruction::LocalSet(idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(len));
    emit_index_check(func, ctx, idx, len, message);

    func.instruction(&Instruction::LocalGet(ptr));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
}

/// True when a divisor cannot be zero: a nonzero numeric literal. Division by
/// one of those needs no runtime guard, which keeps `n // 2` and `x / 2.0`
/// exactly as they were.
pub(crate) fn divisor_is_never_zero(expr: &IRExpr) -> bool {
    match expr {
        IRExpr::Const(IRConstant::Int(n)) => *n != 0,
        IRExpr::Const(IRConstant::Float(f)) => *f != 0.0,
        IRExpr::Const(IRConstant::Bool(b)) => *b,
        _ => false,
    }
}

/// Guard a division whose divisor is on top of the stack, raising
/// `ZeroDivisionError` when it is zero and leaving the operands untouched
/// otherwise.
///
/// Integer division by zero used to trap, which is loud but uncatchable and
/// not what Python does, and float division by zero produced inf silently.
/// Both raise now, so `except ZeroDivisionError:` works.
fn emit_zero_division_guard(
    func: &mut Function,
    ctx: &CompilationContext,
    is_float: bool,
    message: &str,
) {
    if is_float {
        func.instruction(&Instruction::LocalTee(ctx.temp_local_f64_2));
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
        func.instruction(&Instruction::F64Const(f64_const(0.0)));
        func.instruction(&Instruction::F64Eq);
    } else {
        func.instruction(&Instruction::LocalTee(ctx.temp_local));
        func.instruction(&Instruction::LocalGet(ctx.temp_local));
        func.instruction(&Instruction::I32Eqz);
    }
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_raise(
        func,
        ctx,
        "ZeroDivisionError",
        crate::compiler::exceptions::ExcArg::Text(message),
        1,
    );
    func.instruction(&Instruction::End);
}

/// Normalize a possibly negative index and bounds-check it.
///
/// `xs[-1]` is Python for "the last element", and it used to compute an address
/// *before* the region: with the count word sitting at offset 0, `xs[-1]` read
/// the length back as though it were an element. An index past the end read
/// whatever followed the region. Both answered silently rather than failing, so
/// the index is folded against the length here and anything still out of range
/// raises `IndexError`, which is Python's answer and can be caught.
///
/// Entry: the index in `idx` and the length in `len`. Exit: `idx` holds a
/// normalized index that is known to be in range.
pub(crate) fn emit_index_check(
    func: &mut Function,
    ctx: &CompilationContext,
    idx: u32,
    len: u32,
    message: &str,
) {
    // if idx < 0 { idx += len }
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(idx));
    func.instruction(&Instruction::End);

    // if idx < 0 || idx >= len { raise IndexError }
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_raise(
        func,
        ctx,
        "IndexError",
        crate::compiler::exceptions::ExcArg::Text(message),
        1,
    );
    func.instruction(&Instruction::End);
}

/// Report a literal whose elements do not share one slot width.
///
/// Every element of a list, tuple, set, or dict entry occupies one 8-byte slot,
/// and the *collection* decides how that slot is read: a float collection loads
/// f64s, any other loads the low word. A literal mixing `2.5` with `1` has no
/// single answer, so one of the two element types used to read back as garbage.
/// It is a compile error now, reported through the codegen error sink because
/// element types are only known here.
fn check_uniform_slot_width(ctx: &CompilationContext, what: &str, types: &[IRType]) {
    let floats = types.iter().any(|t| matches!(t, IRType::Float));
    let words = types
        .iter()
        .any(|t| matches!(t, IRType::Int | IRType::Bool));
    if floats && words {
        ctx.report(format!(
            "a {what} mixing float and int elements is not supported: every element \
             occupies one slot and the collection reads them all at one width, so the \
             ints (or the floats) would come back as garbage. \
             Hint: make the elements one type, for example write 1.0 instead of 1"
        ));
    }
}

/// Refuse an argument that is an instance of a class unrelated to its
/// parameter's (see [`CompilationContext::check_class_store`]). `position` is
/// zero-based; `callee` names the function or method being called.
/// A dict literal, built into a fresh block. With `value_type`, each value is
/// converted to it on its way in (an int widens to a float), which is how the
/// dict `**kwargs` collects is built at the parameter's value type.
fn emit_dict_literal(
    pairs: &[(IRExpr, IRExpr)],
    value_type: Option<&IRType>,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    // Dict layout in memory: [num_entries:i32][key0][val0][key1][val1]...
    // Each key and value occupies one COLLECTION_SLOT, so an entry is
    // DICT_ENTRY bytes wide (float values round-trip losslessly). Built
    // into a fresh block per evaluation; see the list literal.
    //
    // The key and value types come from the stores below. They used to
    // come from emitting the first pair once more and dropping it, so
    // `{f(): g()}` called both functions twice.
    let dict_size = COLLECTION_HEADER + pairs.len() as u32 * DICT_ENTRY;
    let Some(blk) = ctx.hold() else {
        report_literal_too_deep(func, ctx, "dict");
        return IRType::Dict(Box::new(IRType::Unknown), Box::new(IRType::Unknown));
    };
    emit_literal_block(func, ctx, blk, dict_size);
    emit_literal_header(func, blk, pairs.len() as u32);

    let mut key_types = Vec::with_capacity(pairs.len());
    let mut value_types = Vec::with_capacity(pairs.len());
    for (i, (key_expr, val_expr)) in pairs.iter().enumerate() {
        let key_off = COLLECTION_HEADER + i as u32 * DICT_ENTRY;

        func.instruction(&Instruction::LocalGet(blk));
        let k_type = emit_expr(key_expr, func, ctx, memory_layout, None);
        narrow_element_to_word(func, &k_type);
        store_collection_word_at(func, &k_type, key_off);

        func.instruction(&Instruction::LocalGet(blk));
        let v_type = match value_type {
            Some(value_type) => emit_collection_element(
                val_expr,
                func,
                ctx,
                memory_layout,
                Some(value_type),
                "a keyword argument collected by **kwargs",
            ),
            None => emit_expr(val_expr, func, ctx, memory_layout, None),
        };
        narrow_element_to_word(func, &v_type);
        store_collection_word_at(func, &v_type, key_off + COLLECTION_SLOT);

        key_types.push(k_type);
        value_types.push(v_type);
    }
    ctx.release_held();
    if let Some(why) = key_types
        .first()
        .and_then(|k| crate::compiler::equality::hash_unsupported(ctx, k))
    {
        ctx.report(format!("{why} cannot be a dict key"));
    }
    check_uniform_slot_width(ctx, "dict literal's keys", &key_types);
    check_uniform_slot_width(ctx, "dict literal's values", &value_types);

    let key_type = literal_element_type(ctx, "dict literal's keys", &key_types);
    let value_type = match value_type {
        Some(declared) if !matches!(declared, IRType::Unknown | IRType::Any) => declared.clone(),
        _ => literal_element_type(ctx, "dict literal's values", &value_types),
    };
    func.instruction(&Instruction::LocalGet(blk));
    IRType::Dict(Box::new(key_type), Box::new(value_type))
}

/// Emit argument `position` of a call to the user function `callee`, whose
/// parameter there is `param`, as the single word (or f64) the parameter
/// takes: an int widens to a float parameter, a string or bytes value passes
/// its offset, and `*args` is packed at its member type.
#[allow(clippy::too_many_arguments)]
fn emit_user_argument(
    arg: &IRExpr,
    param: Option<&IRType>,
    position: usize,
    callee: &str,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) {
    let callee = &crate::compiler::function::display_function_name(callee);
    // The tuple a call's surplus positional arguments are bound into, built
    // at the member type so `total(1, 2.5)` for `*nums: float` widens the 1.
    if let (Some(IRType::VarTuple(member)), IRExpr::TupleLiteral(items)) = (param, arg) {
        let built = emit_sequence_of(items, member, func, ctx, memory_layout, callee);
        check_class_arg(ctx, &built, param, position, callee);
        return;
    }
    // The dict `**kwargs` collects, built at the parameter's value type.
    if let (Some(IRType::Dict(key, value)), IRExpr::DictLiteral(pairs)) = (param, arg) {
        if matches!(key.as_ref(), IRType::String) {
            let built = emit_dict_literal(pairs, Some(value), func, ctx, memory_layout);
            check_class_arg(ctx, &built, param, position, callee);
            return;
        }
    }
    let arg_type = emit_value(arg, func, ctx, memory_layout, param);
    match (param, &arg_type) {
        (Some(IRType::Float), IRType::Int | IRType::Bool) => {
            func.instruction(&Instruction::F64ConvertI32S);
        }
        (Some(IRType::Float), IRType::Float) => {}
        (Some(p), IRType::Float) if !matches!(p, IRType::Float) => {
            // An untyped parameter is refused by the untyped-store check.
            if !matches!(p, IRType::Unknown | IRType::Any) {
                ctx.report(format!(
                    "argument {} of {callee}() is a float, where the parameter is '{}'",
                    position + 1,
                    crate::type_to_string(p)
                ));
            }
            func.instruction(&Instruction::I32TruncF64S);
        }
        (Some(IRType::Float), other) => {
            ctx.report(format!(
                "argument {} of {callee}() is a '{}', where the parameter is a float",
                position + 1,
                crate::type_to_string(other)
            ));
            if matches!(other, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::F64Const(0.0_f64.into()));
        }
        (_, IRType::String | IRType::Bytes) => {
            func.instruction(&Instruction::Drop);
        }
        _ => {}
    }
    if let Some(IRType::VarTuple(member)) = param {
        check_var_tuple_arg(ctx, &arg_type, member, position, callee);
    }
    check_class_arg(ctx, &arg_type, param, position, callee);
}

/// A value passed where a `tuple[T, ...]` is taken must already be laid out as
/// one: a list, a tuple of any length, or a fixed tuple whose members are all
/// `T`. Its members cannot be converted in place, so a different member type
/// is refused.
fn check_var_tuple_arg(
    ctx: &CompilationContext,
    arg: &IRType,
    member: &IRType,
    position: usize,
    callee: &str,
) {
    let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
    let fits = match arg {
        IRType::VarTuple(m) | IRType::List(m) => {
            m.as_ref() == member || open(member) && !matches!(m.as_ref(), IRType::Float)
        }
        IRType::Tuple(members) => members
            .iter()
            .all(|m| m == member || open(member) && !matches!(m, IRType::Float)),
        _ => false,
    };
    if !fits {
        ctx.report(format!(
            "argument {} of {callee}() is a '{}', where the parameter is a 'Tuple[{}, ...]'",
            position + 1,
            crate::type_to_string(arg),
            crate::type_to_string(member)
        ));
    }
}

/// Build a fresh `tuple[member, ...]` from `items`, each converted to
/// `member` on its way in (an int widens to a float), in the list layout.
fn emit_sequence_of(
    items: &[IRExpr],
    member: &IRType,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    callee: &str,
) -> IRType {
    let size = COLLECTION_HEADER + items.len() as u32 * COLLECTION_SLOT;
    let Some(blk) = ctx.hold() else {
        report_literal_too_deep(func, ctx, "tuple");
        return IRType::VarTuple(Box::new(member.clone()));
    };
    emit_literal_block(func, ctx, blk, size);
    emit_literal_header(func, blk, items.len() as u32);
    let what = format!("the *args of {callee}()");
    let element = (!matches!(member, IRType::Unknown | IRType::Any)).then_some(member);
    let mut types = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        func.instruction(&Instruction::LocalGet(blk));
        let ty = emit_collection_element(item, func, ctx, memory_layout, element, &what);
        if element.is_none() {
            ctx.check_untyped_store(&ty, &IRType::Any, &what);
        }
        narrow_element_to_word(func, &ty);
        store_collection_word_at(func, &ty, COLLECTION_HEADER + i as u32 * COLLECTION_SLOT);
        types.push(ty);
    }
    ctx.release_held();
    func.instruction(&Instruction::LocalGet(blk));
    match element {
        Some(member) => IRType::VarTuple(Box::new(member.clone())),
        None => IRType::VarTuple(Box::new(types.first().cloned().unwrap_or(IRType::Unknown))),
    }
}

fn check_class_arg(
    ctx: &CompilationContext,
    arg: &IRType,
    param: Option<&IRType>,
    position: usize,
    callee: &str,
) {
    if let Some(param) = param {
        let what = format!("argument {} of {callee}()", position + 1);
        ctx.check_class_store(arg, param, &what);
        ctx.check_untyped_store(arg, param, &what);
    }
}

/// The element type of a collection literal, from the types of its elements.
///
/// Instances of different classes take their nearest common base class, so a
/// method called on an element dispatches on the element's own class. The
/// first element's class used to stand for all of them, which ran its methods
/// on the others (#128). Classes with no common base, or instances mixed with
/// other values, have no one type to read the elements at, so the element type
/// is unknown and a method call on one is refused. Any other literal keeps its
/// first element's type, as before.
fn literal_element_type(ctx: &CompilationContext, what: &str, types: &[IRType]) -> IRType {
    let Some(first) = types.first() else {
        return IRType::Unknown;
    };
    if types.iter().any(|t| matches!(t, IRType::Class(_))) {
        let mut base: Option<String> = None;
        for ty in types {
            let IRType::Class(name) = ty else {
                return refuse_mixed_literal(ctx, what, first, ty);
            };
            base = match base {
                None => Some(name.clone()),
                Some(so_far) => match ctx.common_base(&so_far, name) {
                    Some(common) => Some(common),
                    None => return IRType::Unknown,
                },
            };
        }
        return base.map_or(IRType::Unknown, IRType::Class);
    }
    // Class objects the same way: `[Square, Tri]` holds `type[Shape]`.
    if types.iter().any(|t| matches!(t, IRType::ClassRef(_))) {
        let mut base: Option<String> = None;
        for ty in types {
            let IRType::ClassRef(name) = ty else {
                return refuse_mixed_literal(ctx, what, first, ty);
            };
            base = match base {
                None => Some(name.clone()),
                Some(so_far) => match ctx.common_base(&so_far, name) {
                    Some(common) => Some(common),
                    None => return refuse_mixed_literal(ctx, what, first, ty),
                },
            };
        }
        return base.map_or(IRType::Unknown, IRType::ClassRef);
    }
    let mut unified = first.clone();
    for ty in &types[1..] {
        match unify_element(&unified, ty) {
            Some(both) => unified = both,
            // `check_uniform_slot_width` reports an int beside a float.
            None if matches!(
                (&unified, ty),
                (IRType::Float, IRType::Int | IRType::Bool)
                    | (IRType::Int | IRType::Bool, IRType::Float)
            ) =>
            {
                return first.clone()
            }
            None => return refuse_mixed_literal(ctx, what, &unified, ty),
        }
    }
    unified
}

/// The one element type two elements of a literal share: equal types, an
/// untyped word beside an int (which is how an untyped word reads), and
/// containers whose element types unify that way, such as `[[], [1]]`.
fn unify_element(a: &IRType, b: &IRType) -> Option<IRType> {
    let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
    match (a, b) {
        _ if a == b => Some(a.clone()),
        (x, IRType::Int) | (IRType::Int, x) if open(x) => Some(IRType::Int),
        (IRType::List(x), IRType::List(y)) => Some(IRType::List(Box::new(unify_open(x, y)?))),
        (IRType::Set(x), IRType::Set(y)) => Some(IRType::Set(Box::new(unify_open(x, y)?))),
        (IRType::VarTuple(x), IRType::VarTuple(y)) => {
            Some(IRType::VarTuple(Box::new(unify_open(x, y)?)))
        }
        (IRType::Dict(k1, v1), IRType::Dict(k2, v2)) => Some(IRType::Dict(
            Box::new(unify_open(k1, k2)?),
            Box::new(unify_open(v1, v2)?),
        )),
        (IRType::Tuple(xs), IRType::Tuple(ys)) if xs.len() == ys.len() => Some(IRType::Tuple(
            xs.iter()
                .zip(ys)
                .map(|(x, y)| unify_element(x, y))
                .collect::<Option<Vec<_>>>()?,
        )),
        _ => None,
    }
}

/// Like `unify_element`, but an element type nothing was stored at (the
/// `Unknown` of an empty literal) takes the other side's.
fn unify_open(a: &IRType, b: &IRType) -> Option<IRType> {
    match (a, b) {
        (IRType::Unknown, other) | (other, IRType::Unknown) => Some(other.clone()),
        _ => unify_element(a, b),
    }
}

/// A literal whose elements no one element type holds: every element of a
/// collection is read as one type here, so a str beside an int read back as
/// its address.
fn refuse_mixed_literal(ctx: &CompilationContext, what: &str, a: &IRType, b: &IRType) -> IRType {
    ctx.report(format!(
        "a {what} mixing {} and {} elements is not supported: a collection holds one \
         element type here, and the others would be read back as it. \
         Hint: keep each collection to one type, or use a class or a tuple for a record",
        crate::type_to_string(a),
        crate::type_to_string(b)
    ));
    a.clone()
}

/// Emit a list/set/dict comprehension. See the module comment above for the
/// two-pass (capacity, fill) strategy. Leaves the result pointer on the stack.
fn emit_comprehension(
    kind: IRComprehensionKind,
    element: &IRExpr,
    value: Option<&IRExpr>,
    generators: &[IRGenerator],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let depth = ctx.comp_depth.get();
    // Everything emitted from here on (iterables, filters, elements) sits one
    // comprehension level deeper: nested comprehensions use the next depth's
    // helper locals, and literals know they're (re)built per iteration.
    ctx.comp_depth.set(depth + 1);

    let res = comp_local(ctx, comp_local_name("res", depth));
    let widx = comp_local(ctx, comp_local_name("widx", depth));
    let cap = comp_local(ctx, comp_local_name("cap", depth));
    let ptr0 = comp_local(ctx, comp_gen_local_name("ptr", depth, 0));

    // --- Capacity phase -----------------------------------------------------
    // Evaluate the first iterable once and stash it; the fill phase reuses it.
    let gen0_ty = emit_expr(&generators[0].iterable, func, ctx, memory_layout, None);
    let gen0_ty = emit_as_sequence(func, ctx, gen0_ty);
    narrow_element_to_word(func, &gen0_ty);
    func.instruction(&Instruction::LocalSet(ptr0));

    if generators.len() == 1 {
        emit_comp_iterable_len(func, ctx, matches!(gen0_ty, IRType::Range), ptr0);
        func.instruction(&Instruction::LocalSet(cap));
    } else {
        // Counting pre-pass: run the outer loops (no filters) and sum the
        // innermost iterable's length.
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(cap));
        let last = generators.len() - 1;
        let ptr_last = comp_local(ctx, comp_gen_local_name("ptr", depth, last));
        let last_iterable = &generators[last].iterable;
        emit_comp_loops(
            func,
            ctx,
            memory_layout,
            &generators[..last],
            0,
            depth,
            &gen0_ty,
            false,
            &mut |func| {
                let t = emit_expr(last_iterable, func, ctx, memory_layout, None);
                let t = emit_as_sequence(func, ctx, t);
                narrow_element_to_word(func, &t);
                func.instruction(&Instruction::LocalSet(ptr_last));
                emit_comp_iterable_len(func, ctx, matches!(t, IRType::Range), ptr_last);
                func.instruction(&Instruction::LocalGet(cap));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(cap));
            },
        );
    }

    // --- Allocation ----------------------------------------------------------
    // `__alloc` blocks are always fresh (monotonic bump over zeroed memory),
    // so no zero-fill is needed — in particular the set table starts empty.
    match kind {
        IRComprehensionKind::List | IRComprehensionKind::Dict => {
            let entry = if matches!(kind, IRComprehensionKind::Dict) {
                DICT_ENTRY
            } else {
                COLLECTION_SLOT
            };
            func.instruction(&Instruction::LocalGet(cap));
            func.instruction(&Instruction::I32Const(entry as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(res));
            // The block is sized for `cap` elements even though filters may
            // produce fewer, so that is the capacity a later `append` can use
            // before it has to reallocate.
            store_runtime_cap(func, res, cap);
        }
        IRComprehensionKind::Set => {
            // A set comprehension adds to an empty set one element at a time.
            crate::compiler::sets::emit_new_set(func, ctx);
            func.instruction(&Instruction::LocalSet(res));
        }
    }

    // --- Fill phase ------------------------------------------------------------
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(widx));

    let mut elem_ty = IRType::Unknown;
    let mut val_ty = IRType::Unknown;
    // A set's members hash and compare by their type, which has to be known
    // before the first is added.
    let set_member = matches!(kind, IRComprehensionKind::Set).then(|| {
        let t = probe_type(element, ctx, memory_layout, None).0;
        if let Some(why) = crate::compiler::sets::member_unsupported(ctx, &t) {
            ctx.report(format!("{why} cannot be a set member"));
        }
        t
    });
    let set_value = set_member
        .as_ref()
        .map(|t| crate::compiler::sets::Held::of(ctx, t));
    {
        let elem_ty = &mut elem_ty;
        let val_ty = &mut val_ty;
        emit_comp_loops(
            func,
            ctx,
            memory_layout,
            generators,
            0,
            depth,
            &gen0_ty,
            true,
            &mut |func| match kind {
                IRComprehensionKind::List => {
                    // slot address = res + HEADER + widx*SLOT
                    func.instruction(&Instruction::LocalGet(res));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(widx));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    let t = emit_expr(element, func, ctx, memory_layout, None);
                    narrow_element_to_word(func, &t);
                    store_collection_word(func, &t);
                    *elem_ty = t;

                    func.instruction(&Instruction::LocalGet(widx));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(widx));
                }
                IRComprehensionKind::Dict => {
                    // key slot at res + HEADER + widx*ENTRY, value slot right after
                    func.instruction(&Instruction::LocalGet(res));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(widx));
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    let kt = emit_expr(element, func, ctx, memory_layout, None);
                    narrow_element_to_word(func, &kt);
                    store_collection_word(func, &kt);
                    *elem_ty = kt;

                    func.instruction(&Instruction::LocalGet(res));
                    func.instruction(&Instruction::I32Const(
                        (COLLECTION_HEADER + COLLECTION_SLOT) as i32,
                    ));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalGet(widx));
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    let vt = emit_expr(
                        value.expect("dict comprehension has a value"),
                        func,
                        ctx,
                        memory_layout,
                        None,
                    );
                    narrow_element_to_word(func, &vt);
                    store_collection_word(func, &vt);
                    *val_ty = vt;

                    func.instruction(&Instruction::LocalGet(widx));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(widx));
                }
                IRComprehensionKind::Set => {
                    let member = set_member.as_ref().expect("a set comprehension");
                    let v = set_value.expect("a set comprehension");
                    let t = emit_collection_element(
                        element,
                        func,
                        ctx,
                        memory_layout,
                        Some(member),
                        "a set comprehension",
                    );
                    narrow_element_to_word(func, &t);
                    v.set(func);
                    crate::compiler::sets::emit_add(func, ctx, member, res, v);
                    func.instruction(&Instruction::Drop);
                    *elem_ty = member.clone();
                }
            },
        );
    }

    if let Some(v) = set_value {
        v.release(ctx);
    }

    // --- Finalize -------------------------------------------------------------
    // Lists and dicts record how many elements the filters let through; the
    // set's count was maintained by the dedup insert.
    if !matches!(kind, IRComprehensionKind::Set) {
        func.instruction(&Instruction::LocalGet(res));
        func.instruction(&Instruction::LocalGet(widx));
        func.instruction(&Instruction::I32Store(slot_arg()));
    }
    func.instruction(&Instruction::LocalGet(res));

    ctx.comp_depth.set(depth);
    match kind {
        IRComprehensionKind::List => IRType::List(Box::new(elem_ty)),
        IRComprehensionKind::Set => IRType::Set(Box::new(elem_ty)),
        IRComprehensionKind::Dict => IRType::Dict(Box::new(elem_ty), Box::new(val_ty)),
    }
}

/// Emit WebAssembly instructions for an IR expression
pub fn emit_expr(
    expr: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected_type: Option<&IRType>,
) -> IRType {
    // An optional number in arithmetic, a comparison, or a conversion is
    // unboxed first, raising CPython's TypeError for None.
    if let Some(rewritten) = unbox_operands(expr, ctx) {
        return emit_expr(&rewritten, func, ctx, memory_layout, expected_type);
    }
    let ty = emit_expr_inner(expr, func, ctx, memory_layout, expected_type);
    // A number stored where an optional one is held is boxed.
    match (expected_type.and_then(boxed_scalar), &ty) {
        (Some(slot), t) if t == slot => {
            emit_box(func, ctx, slot);
            IRType::Optional(Box::new(slot.clone()))
        }
        (Some(IRType::Float), IRType::Int | IRType::Bool) => {
            func.instruction(&Instruction::F64ConvertI32S);
            emit_box(func, ctx, &IRType::Float);
            IRType::Optional(Box::new(IRType::Float))
        }
        // An untyped word is read as an int.
        (Some(slot @ (IRType::Int | IRType::Bool)), IRType::Unknown | IRType::Any) => {
            emit_box(func, ctx, slot);
            IRType::Optional(Box::new(slot.clone()))
        }
        // An optional number stored where a plain one is held (`return x`
        // after `if x is None: ...`): its number. Were it None, CPython would
        // pass the None on, which the place cannot hold, so that traps
        // rather than raising an exception CPython would not.
        (None, IRType::Optional(inner))
            if boxed_scalar(&ty).is_some()
                && matches!(
                    (expected_type, inner.as_ref()),
                    (Some(IRType::Int), IRType::Int | IRType::Bool)
                        | (Some(IRType::Float), IRType::Float)
                ) =>
        {
            let inner = inner.as_ref().clone();
            let Some(cell) = ctx.hold() else {
                report_literal_too_deep(func, ctx, "an optional number");
                return ty;
            };
            func.instruction(&Instruction::LocalTee(cell));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::Unreachable);
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(cell));
            func.instruction(&if matches!(inner, IRType::Float) {
                Instruction::F64Load(mem_off(0))
            } else {
                Instruction::I32Load(mem_off(0))
            });
            ctx.release_held();
            inner
        }
        _ => ty,
    }
}

/// The number an `Optional[int]` / `Optional[bool]` / `Optional[float]`
/// holds. Such a value is a pointer to a cell holding the number, and None
/// the null pointer, so None is not 0.
pub(crate) fn boxed_scalar(ty: &IRType) -> Option<&IRType> {
    match ty {
        IRType::Optional(inner)
            if matches!(**inner, IRType::Int | IRType::Bool | IRType::Float) =>
        {
            Some(inner)
        }
        _ => None,
    }
}

/// Box the number of type `ty` on the stack: a fresh cell holding it.
pub(crate) fn emit_box(func: &mut Function, ctx: &CompilationContext, ty: &IRType) {
    let float = matches!(ty, IRType::Float);
    let value = if float { ctx.hold_f64() } else { ctx.hold() };
    let (Some(value), Some(cell)) = (value, ctx.hold()) else {
        report_literal_too_deep(func, ctx, "an optional number");
        return;
    };
    func.instruction(&Instruction::LocalSet(value));
    func.instruction(&Instruction::I32Const(8));
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(cell));
    func.instruction(&Instruction::LocalGet(value));
    func.instruction(&if float {
        Instruction::F64Store(mem_off(0))
    } else {
        Instruction::I32Store(mem_off(0))
    });
    func.instruction(&Instruction::LocalGet(cell));
    ctx.release_held();
    if float {
        ctx.release_held_f64();
    } else {
        ctx.release_held();
    }
}

/// The number in the optional on the stack, raising TypeError(`message`)
/// when it is None.
fn emit_unbox(func: &mut Function, ctx: &CompilationContext, ty: &IRType, message: &str) {
    let Some(cell) = ctx.hold() else {
        report_literal_too_deep(func, ctx, "an optional number");
        return;
    };
    func.instruction(&Instruction::LocalTee(cell));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_raise(
        func,
        ctx,
        "TypeError",
        crate::compiler::exceptions::ExcArg::Text(message),
        1,
    );
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(cell));
    func.instruction(&if matches!(ty, IRType::Float) {
        Instruction::F64Load(mem_off(0))
    } else {
        Instruction::I32Load(mem_off(0))
    });
    ctx.release_held();
}

/// The intrinsic `__unbox(value, message)`: the number in an optional, or
/// TypeError(message) for None.
const UNBOX_FN: &str = "__unbox_optional";

/// What `expr` is, when that is plain without emitting it: a variable, a
/// field, a call's declared result, a one-argument `dict.get`.
pub(crate) fn quick_type(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRType> {
    match expr {
        IRExpr::Variable(n) | IRExpr::Param(n) => ctx
            .get_local_info(n)
            .map(|i| i.var_type.clone())
            .or_else(|| ctx.module_global_types.get(n).cloned()),
        IRExpr::Attribute { object, attribute } => match quick_type(object, ctx)? {
            IRType::Class(class) => lookup_field(ctx, &class, attribute).map(|(_, t)| t),
            _ => None,
        },
        IRExpr::FunctionCall { function_name, .. } => ctx
            .get_function_info(function_name)
            .map(|f| f.return_type.clone()),
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } if method_name == "get" && arguments.len() == 1 => match quick_type(object, ctx)? {
            IRType::Dict(_, value) if boxed_scalar(&IRType::Optional(value.clone())).is_some() => {
                Some(IRType::Optional(value))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether evaluating `expr` twice, or skipping it, cannot be told apart.
fn is_plain_operand(expr: &IRExpr) -> bool {
    match expr {
        IRExpr::Const(_) | IRExpr::Variable(_) | IRExpr::Param(_) => true,
        IRExpr::Attribute { object, .. } => is_plain_operand(object),
        _ => false,
    }
}

/// `expr` with its optional-number operands unboxed, when it has any: an
/// arithmetic operator, an ordering, a negation, or `int()`/`float()` raises
/// TypeError on None as CPython does; `==` and `!=` with None are a test.
fn unbox_operands(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let optional = |e: &IRExpr| quick_type(e, ctx).filter(|t| boxed_scalar(t).is_some());
    let unbox = |e: &IRExpr, message: String| IRExpr::FunctionCall {
        function_name: UNBOX_FN.to_string(),
        arguments: vec![e.clone(), IRExpr::Const(IRConstant::String(message))],
    };
    let name_of = |e: &IRExpr| {
        let t = match e {
            IRExpr::Const(IRConstant::Int(_)) => IRType::Int,
            IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
            IRExpr::Const(IRConstant::Bool(_)) => IRType::Bool,
            IRExpr::Const(IRConstant::String(_)) => IRType::String,
            IRExpr::Const(IRConstant::None) => IRType::None,
            other => quick_type(other, ctx).unwrap_or(IRType::Unknown),
        };
        let t = boxed_scalar(&t).cloned().unwrap_or(t);
        crate::compiler::operators::python_type_name(&t)
    };
    let refuse_both = |what: &str| {
        ctx.report(format!(
            "{what} two optional numbers is not supported: which one CPython's TypeError names \
             depends on which is None. Hint: test them for None first"
        ));
        None
    };
    match expr {
        IRExpr::BinaryOp { left, right, op } => {
            let symbol = operator_symbol(op);
            match (optional(left), optional(right)) {
                (Some(_), Some(_)) => refuse_both(&format!("'{symbol}' between")),
                (Some(_), None) if is_plain_operand(right) => Some(IRExpr::BinaryOp {
                    left: Box::new(unbox(
                        left,
                        format!(
                            "unsupported operand type(s) for {symbol}: 'NoneType' and '{}'",
                            name_of(right)
                        ),
                    )),
                    right: right.clone(),
                    op: op.clone(),
                }),
                (None, Some(_)) => Some(IRExpr::BinaryOp {
                    left: left.clone(),
                    right: Box::new(unbox(
                        right,
                        format!(
                            "unsupported operand type(s) for {symbol}: '{}' and 'NoneType'",
                            name_of(left)
                        ),
                    )),
                    op: op.clone(),
                }),
                (Some(_), None) => {
                    ctx.report(
                        "an optional number on the left of an operator whose right operand has \
                         effects is not supported: CPython evaluates the right operand before \
                         raising for None. Hint: compute the right operand into a variable first",
                    );
                    None
                }
                (None, None) => None,
            }
        }
        IRExpr::CompareOp { left, right, op } => {
            let symbol = match op {
                IRCompareOp::Lt => "<",
                IRCompareOp::LtE => "<=",
                IRCompareOp::Gt => ">",
                IRCompareOp::GtE => ">=",
                IRCompareOp::Eq | IRCompareOp::NotEq => {
                    // `x == y`: None equals only None, so the number is
                    // compared only when there is one.
                    let (opt, other) = match (optional(left), optional(right)) {
                        (Some(_), Some(_)) => return refuse_both("comparing"),
                        (Some(_), None) => (left, right),
                        (None, Some(_)) => (right, left),
                        (None, None) => return None,
                    };
                    let is = matches!(op, IRCompareOp::Eq);
                    if matches!(**other, IRExpr::Const(IRConstant::None)) {
                        return Some(IRExpr::CompareOp {
                            left: opt.clone(),
                            right: other.clone(),
                            op: if is {
                                IRCompareOp::Is
                            } else {
                                IRCompareOp::IsNot
                            },
                        });
                    }
                    if !is_plain_operand(opt) || !is_plain_operand(other) {
                        ctx.report(
                            "'==' with an optional number is supported between plain names \
                             and constants. Hint: compute the operands into variables first",
                        );
                        return None;
                    }
                    let value = unbox(opt, String::new());
                    return Some(IRExpr::Conditional {
                        condition: Box::new(IRExpr::CompareOp {
                            left: opt.clone(),
                            right: Box::new(IRExpr::Const(IRConstant::None)),
                            op: IRCompareOp::IsNot,
                        }),
                        body: Box::new(IRExpr::CompareOp {
                            left: Box::new(value),
                            right: other.clone(),
                            op: op.clone(),
                        }),
                        orelse: Box::new(IRExpr::Const(IRConstant::Bool(!is))),
                    });
                }
                _ => return None,
            };
            let message = |a: &str, b: &str| {
                format!("'{symbol}' not supported between instances of '{a}' and '{b}'")
            };
            match (optional(left), optional(right)) {
                (Some(_), Some(_)) => refuse_both("ordering"),
                (Some(_), None) if is_plain_operand(right) => Some(IRExpr::CompareOp {
                    left: Box::new(unbox(left, message("NoneType", &name_of(right)))),
                    right: right.clone(),
                    op: op.clone(),
                }),
                (None, Some(_)) => Some(IRExpr::CompareOp {
                    left: left.clone(),
                    right: Box::new(unbox(right, message(&name_of(left), "NoneType"))),
                    op: op.clone(),
                }),
                (Some(_), None) => {
                    ctx.report(
                        "an optional number on the left of a comparison whose right operand has \
                         effects is not supported. Hint: compute the right operand into a \
                         variable first",
                    );
                    None
                }
                (None, None) => None,
            }
        }
        IRExpr::UnaryOp { operand, op } if !matches!(op, IRUnaryOp::Not) => {
            optional(operand)?;
            let symbol = match op {
                IRUnaryOp::Neg => "-",
                IRUnaryOp::UAdd => "+",
                _ => "~",
            };
            Some(IRExpr::UnaryOp {
                operand: Box::new(unbox(
                    operand,
                    format!("bad operand type for unary {symbol}: 'NoneType'"),
                )),
                op: op.clone(),
            })
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if matches!(function_name.as_str(), "int" | "float") && arguments.len() == 1 => {
            optional(&arguments[0])?;
            let message = if function_name == "int" {
                "int() argument must be a string, a bytes-like object or a real number, not \
                 'NoneType'"
            } else {
                "float() argument must be a string or a real number, not 'NoneType'"
            };
            Some(IRExpr::FunctionCall {
                function_name: function_name.clone(),
                arguments: vec![unbox(&arguments[0], message.to_string())],
            })
        }
        _ => None,
    }
}

fn emit_expr_inner(
    expr: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected_type: Option<&IRType>,
) -> IRType {
    match expr {
        IRExpr::Const(constant) => {
            match constant {
                IRConstant::Int(i) => {
                    // Widen to f64 when a float is expected (e.g. an int literal
                    // passed to a float parameter or stored in a float field).
                    if let Some(IRType::Float) = expected_type {
                        func.instruction(&Instruction::F64Const(f64_const(*i as f64)));
                        IRType::Float
                    } else {
                        func.instruction(&Instruction::I32Const(*i));
                        IRType::Int
                    }
                }
                IRConstant::Float(f) => {
                    func.instruction(&Instruction::F64Const(f64_const(*f)));

                    // An int is expected, but Python never truncates a float
                    // on its way into a variable, field, or argument.
                    if let Some(IRType::Int) = expected_type {
                        report_float_where_int(ctx);
                        func.instruction(&Instruction::I32TruncF64S);
                        IRType::Int
                    } else {
                        IRType::Float
                    }
                }
                IRConstant::Bool(b) => {
                    func.instruction(&Instruction::I32Const(if *b { 1 } else { 0 }));
                    IRType::Bool
                }
                IRConstant::String(s) => {
                    // Get the string's offset in memory. Every literal is
                    // interned while lowering; one that was not would read
                    // whatever sits at offset 0.
                    let offset = match memory_layout.string_offsets.get(s) {
                        Some(offset) => offset,
                        None => {
                            ctx.report(format!(
                                "the string {s:?} was never placed in memory; this is a code \
                                 generation bug"
                            ));
                            &0
                        }
                    };

                    // Push the string's memory offset and length onto the stack
                    func.instruction(&Instruction::I32Const(*offset as i32));
                    func.instruction(&Instruction::I32Const(s.len() as i32));

                    IRType::String
                }
                IRConstant::None => {
                    // None is represented as i32 constant 0
                    func.instruction(&Instruction::I32Const(0));
                    IRType::None
                }
                // Lowering builds collections as literals; a collection
                // constant reaching here has no layout to answer with.
                IRConstant::List(_)
                | IRConstant::Dict(_)
                | IRConstant::Tuple(_)
                | IRConstant::Set(_) => {
                    ctx.report("a constant collection value is not supported here");
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
                IRConstant::Bytes(b) => {
                    // Get the bytes' offset in memory
                    let offset = memory_layout.bytes_offsets.get(b).unwrap_or(&0);

                    // Push the bytes' memory offset and length onto the stack
                    func.instruction(&Instruction::I32Const(*offset as i32));
                    func.instruction(&Instruction::I32Const(b.len() as i32));

                    IRType::Bytes
                }
            }
        }
        IRExpr::Param(name) | IRExpr::Variable(name) => {
            if let Some(local_info) = ctx.get_local_info(name) {
                let index = local_info.index;
                let var_type = local_info.var_type.clone();
                func.instruction(&Instruction::LocalGet(index));
                // String/bytes values are an (offset, length) pair but the local
                // holds only the offset; push the length to rebuild the pair the
                // rest of the pipeline expects. A local assigned in the body has a
                // companion length local; a str/bytes *parameter* does not, so its
                // length is recovered from the blob prefix via
                // load(offset - STRING_LEN_PREFIX). Without this, referencing a
                // string parameter left one word on the stack instead of two,
                // underflowing later consumers (e.g. `==`) into invalid WASM.
                if matches!(var_type, IRType::String | IRType::Bytes) {
                    if let Some(len_idx) = ctx.get_local_index(&strlen_local_name(name)) {
                        func.instruction(&Instruction::LocalGet(len_idx));
                    } else {
                        func.instruction(&Instruction::LocalTee(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                    }
                }
                var_type
            } else if let Some(&global) = ctx.module_global_index.get(name.as_str()) {
                // A module definition evaluated once by `__module_init`. Every
                // reader shares the one object, so a mutation through any of
                // them is seen by all of them.
                match ctx.module_global_types.get(name.as_str()).cloned() {
                    Some(ty) => {
                        func.instruction(&Instruction::GlobalGet(global));
                        if matches!(ty, IRType::String | IRType::Bytes) {
                            recover_str_pair(func, ctx);
                        }
                        ty
                    }
                    // Only `__module_init` runs before a global is set, so this
                    // is a definition that reads one defined after it, which
                    // CPython refuses with a NameError at import.
                    None => {
                        ctx.report(format!(
                            "module-level '{name}' is used before it is defined"
                        ));
                        func.instruction(&Instruction::I32Const(0));
                        IRType::Unknown
                    }
                }
            } else if let Some((declared, value)) = ctx.get_module_var(name) {
                // Module-level variable: inline its initializer. Clone first so
                // the recursive emit does not alias the borrow of `ctx`. Emit at
                // the value's natural type (expected_type None) — passing the
                // caller's expectation through would, e.g., truncate a float
                // constant to i32 in `2 * PI`.
                let declared = declared.clone();
                let value = value.clone();
                let emitted = emit_expr(&value, func, ctx, memory_layout, None);
                declared.unwrap_or(emitted)
            } else if let Some((module, attribute)) = ctx.stdlib_imports.get(name).cloned() {
                // `from math import pi`: read through the module, which folds
                // a constant and refuses anything else.
                emit_expr(
                    &IRExpr::Attribute {
                        object: Box::new(IRExpr::Variable(module)),
                        attribute,
                    },
                    func,
                    ctx,
                    memory_layout,
                    expected_type,
                )
            } else if let Some(class) = ctx.get_class_info(name) {
                // A class named as a value is the class object, its id.
                func.instruction(&Instruction::I32Const(class.class_id));
                IRType::ClassRef(name.clone())
            } else {
                // Nothing binds the name: a NameError in Python. This pushed
                // -999 and reported success.
                ctx.report(format!("name '{name}' is not defined"));
                func.instruction(&Instruction::I32Const(0));
                IRType::Unknown
            }
        }
        IRExpr::BinaryOp { left, right, op } => {
            let left_type = emit_expr(left, func, ctx, memory_layout, None);
            // Emit the right operand at its natural type rather than forcing it to
            // the left's type: a float right operand under an int left (e.g.
            // `2 * (a_float + b_float)`) must not be truncated to int — the
            // int/float widening below promotes whichever side is the integer.
            let right_type = emit_expr(right, func, ctx, memory_layout, None);

            // Handle string and bytes operations
            if left_type == IRType::String || left_type == IRType::Bytes {
                match op {
                    IROp::Add => {
                        if (left_type == IRType::String && right_type == IRType::String)
                            || (left_type == IRType::Bytes && right_type == IRType::Bytes)
                        {
                            // String/Bytes concatenation. Stack on entry:
                            //   (left_offset, left_len, right_offset, right_len)
                            // A new `[len:i32][bytes][nul?]` blob is allocated at
                            // runtime via `__alloc`, both operands are copied in
                            // with `memory.copy`, and the result `(offset, len)`
                            // (offset past the length prefix) is left on the
                            // stack. Strings get a trailing NUL; bytes do not.
                            let is_string = left_type == IRType::String;
                            let prefix = STRING_LEN_PREFIX as i32;

                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // right_len
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // right_offset
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // left_len
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // left_offset

                            // total_len = left_len + right_len
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 5)); // total_len

                            // block = __alloc(prefix + total_len [+ 1 for NUL])
                            func.instruction(&Instruction::I32Const(prefix));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                            func.instruction(&Instruction::I32Add);
                            if is_string {
                                func.instruction(&Instruction::I32Const(1));
                                func.instruction(&Instruction::I32Add);
                            }
                            func.instruction(&Instruction::Call(ctx.alloc_func_index));
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 6)); // block

                            // Write the length prefix at the block start.
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                            func.instruction(&Instruction::I32Store(MemArg {
                                offset: 0,
                                align: 2,
                                memory_index: 0,
                            }));

                            // data_ptr = block + prefix (the value's offset)
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                            func.instruction(&Instruction::I32Const(prefix));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 6)); // data_ptr

                            // memory.copy(data_ptr, left_offset, left_len)
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                            func.instruction(&Instruction::MemoryCopy {
                                src_mem: 0,
                                dst_mem: 0,
                            });

                            // memory.copy(data_ptr + left_len, right_offset, right_len)
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::MemoryCopy {
                                src_mem: 0,
                                dst_mem: 0,
                            });

                            // Strings are NUL-terminated; write it past the data.
                            if is_string {
                                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                                func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                                func.instruction(&Instruction::I32Add);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Store8(MemArg {
                                    offset: 0,
                                    align: 0,
                                    memory_index: 0,
                                }));
                            }

                            // Result: (data_ptr, total_len)
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                            return left_type.clone();
                        }
                    }
                    // A literal format is lowered per conversion; one that
                    // reaches codegen is a format string known only at run
                    // time, with no conversions to read when compiling. This
                    // dropped the operand and answered the format string
                    // unchanged.
                    IROp::Mod if left_type == IRType::String => {
                        ctx.report(
                            "'%' formatting with a format string known only at run time is not \
                             supported. Hint: write the format string as a literal, or use an \
                             f-string, for example f\"{n}!\"",
                        );
                        for _ in
                            0..2 + matches!(right_type, IRType::String | IRType::Bytes) as usize
                        {
                            func.instruction(&Instruction::Drop);
                        }
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32Const(0));
                        return IRType::String;
                    }
                    _ => {}
                }
            }

            if let Some(result) = crate::compiler::operators::emit_non_numeric_binop(
                func,
                ctx,
                op,
                left,
                right,
                &left_type,
                &right_type,
            ) {
                return result;
            }

            // int/bool/unknown values are all i32-represented; widen the i32
            // side to f64 when the other operand is a float.
            let left_int_like = matches!(left_type, IRType::Int | IRType::Bool | IRType::Unknown);
            let right_int_like = matches!(right_type, IRType::Int | IRType::Bool | IRType::Unknown);
            if left_type == IRType::Float && right_int_like {
                // Right operand (top of stack) is i32; widen it to f64.
                func.instruction(&Instruction::F64ConvertI32S);
            } else if left_int_like && right_type == IRType::Float {
                // Left operand is the i32 buried under the f64 right operand.
                // Stash the f64 (needs an f64 local), widen the int, restore.
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                func.instruction(&Instruction::F64ConvertI32S);
                func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
            }

            let result_type = if left_type == IRType::Float || right_type == IRType::Float {
                match op {
                    IROp::Add => {
                        func.instruction(&Instruction::F64Add);
                    }
                    IROp::Sub => {
                        func.instruction(&Instruction::F64Sub);
                    }
                    IROp::Mul => {
                        func.instruction(&Instruction::F64Mul);
                    }
                    IROp::Div => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, true, "float division by zero");
                        }
                        func.instruction(&Instruction::F64Div);
                    }
                    IROp::Mod => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, true, "float modulo by zero");
                        }
                        emit_float_modulo_operation(func, ctx);
                    }
                    IROp::FloorDiv => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(
                                func,
                                ctx,
                                true,
                                "float floor division by zero",
                            );
                        }
                        func.instruction(&Instruction::F64Div);
                        func.instruction(&Instruction::F64Floor);
                    }
                    IROp::Pow => {
                        emit_float_power_operation(func, ctx);
                    }
                    // `@` and the bitwise operators are TypeErrors on a float in
                    // CPython. These pushed a 0.0 over the two operands and
                    // reported success.
                    IROp::MatMul
                    | IROp::LShift
                    | IROp::RShift
                    | IROp::BitOr
                    | IROp::BitXor
                    | IROp::BitAnd => {
                        ctx.report(format!(
                            "unsupported operand type(s) for {}: a float operand",
                            operator_symbol(op)
                        ));
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::F64Const(f64_const(0.0)));
                    }
                }
                IRType::Float
            } else {
                // Integer operations
                match op {
                    IROp::Add => {
                        func.instruction(&Instruction::I32Add);
                    }
                    IROp::Sub => {
                        func.instruction(&Instruction::I32Sub);
                    }
                    IROp::Mul => {
                        func.instruction(&Instruction::I32Mul);
                    }
                    IROp::Div => {
                        // Python 3's `/` is true division: `7 / 2` is 3.5, not
                        // 3. This emitted an integer divide, so the result was
                        // silently truncated wherever it was used as a number
                        // and failed validation wherever a float was expected.
                        // `//` is the floor division that keeps the integer
                        // result, and it is unchanged.
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, false, "division by zero");
                        }
                        let divisor = ctx.temp_local + 40;
                        func.instruction(&Instruction::LocalSet(divisor));
                        func.instruction(&Instruction::F64ConvertI32S);
                        func.instruction(&Instruction::LocalGet(divisor));
                        func.instruction(&Instruction::F64ConvertI32S);
                        func.instruction(&Instruction::F64Div);
                    }
                    // Python's `%` and `//` floor; WebAssembly's `rem_s` and
                    // `div_s` truncate toward zero. The two agree when the
                    // operands share a sign and differ by one step when they
                    // do not, so `-7 % 3` answered -1 (CPython: 2) and
                    // `-7 // 2` answered -3 (CPython: -4), silently, in any
                    // program with a negative operand.
                    IROp::Mod => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, false, "integer modulo by zero");
                        }
                        emit_floor_mod_i32(func, ctx);
                    }
                    IROp::FloorDiv => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(
                                func,
                                ctx,
                                false,
                                "integer division or modulo by zero",
                            );
                        }
                        emit_floor_div_i32(func, ctx);
                    }
                    IROp::Pow => {
                        emit_integer_power_operation(func, ctx);
                    }
                    IROp::MatMul => {
                        // `@` on two ints is a TypeError in CPython. This pushed
                        // a 0 over the two operands and reported success.
                        ctx.report("unsupported operand type(s) for @: 'int' and 'int'");
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                    }
                    IROp::LShift => {
                        func.instruction(&Instruction::I32Shl);
                    }
                    IROp::RShift => {
                        func.instruction(&Instruction::I32ShrS);
                    }
                    IROp::BitOr => {
                        func.instruction(&Instruction::I32Or);
                    }
                    IROp::BitXor => {
                        func.instruction(&Instruction::I32Xor);
                    }
                    IROp::BitAnd => {
                        func.instruction(&Instruction::I32And);
                    }
                }
                // True division is the one integer operator whose result is a
                // float.
                if matches!(op, IROp::Div) {
                    IRType::Float
                } else {
                    IRType::Int
                }
            };

            // Widen an int result where a float is expected. The reverse used
            // to truncate, so `self.v = a / b` into an int field answered 1
            // for 3 / 2 (#132); Python keeps the float.
            if let Some(expected) = expected_type {
                if *expected == IRType::Int && result_type == IRType::Float {
                    report_float_where_int(ctx);
                    func.instruction(&Instruction::I32TruncF64S);
                    return IRType::Int;
                } else if *expected == IRType::Float && result_type == IRType::Int {
                    func.instruction(&Instruction::F64ConvertI32S);
                    return IRType::Float;
                }
            }

            result_type
        }
        IRExpr::UnaryOp { operand, op } => {
            // `not f()` of a call answering None tests the None it answers.
            let operand_type = if matches!(op, IRUnaryOp::Not) {
                emit_value(operand, func, ctx, memory_layout, None)
            } else {
                emit_expr(operand, func, ctx, memory_layout, None)
            };

            match operand_type {
                IRType::Float => {
                    match op {
                        IRUnaryOp::Neg => {
                            // Negate float: -x
                            func.instruction(&Instruction::F64Const(f64_const(-1.0)));
                            func.instruction(&Instruction::F64Mul);
                        }
                        IRUnaryOp::Not => {
                            // `x == 0.0` is already `not x`. This used to
                            // invert it as well, so `not 0.0` was False.
                            func.instruction(&Instruction::F64Const(f64_const(0.0)));
                            func.instruction(&Instruction::F64Eq);
                        }
                        IRUnaryOp::Invert => {
                            ctx.report("bad operand type for unary ~: 'float'");
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::F64Const(f64_const(0.0)));
                        }
                        IRUnaryOp::UAdd => {
                            // No-op for floats
                        }
                    }
                    if matches!(op, IRUnaryOp::Not) {
                        IRType::Bool
                    } else {
                        IRType::Float
                    }
                }
                ref other
                    if !matches!(op, IRUnaryOp::Not)
                        && !matches!(
                            other,
                            IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any
                        ) =>
                {
                    let symbol = match op {
                        IRUnaryOp::Neg => "-",
                        IRUnaryOp::UAdd => "+",
                        _ => "~",
                    };
                    ctx.report(format!(
                        "bad operand type for unary {symbol}: '{}'",
                        crate::compiler::operators::python_type_name(other)
                    ));
                    func.instruction(&Instruction::Drop);
                    if matches!(other, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Int
                }
                _ => {
                    // Integer/Boolean operations
                    match op {
                        IRUnaryOp::Neg => {
                            // Negate: -x. The operand is already on the stack, so
                            // multiply by -1 (mirroring the float path). Emitting
                            // `i32.const 0; i32.sub` here would instead compute
                            // `operand - 0`, leaving the value unchanged.
                            func.instruction(&Instruction::I32Const(-1));
                            func.instruction(&Instruction::I32Mul);
                            IRType::Int
                        }
                        IRUnaryOp::Not => {
                            // Reduce to Python's truth value first: `not ""`
                            // and `not []` are True, and testing the raw
                            // (offset, length) pair or the region pointer got
                            // both wrong.
                            emit_truthiness(func, ctx, &operand_type);
                            // Then invert (1->0, 0->1)
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Xor);
                            IRType::Bool
                        }
                        IRUnaryOp::Invert => {
                            // Bitwise NOT: ~x
                            func.instruction(&Instruction::I32Const(-1));
                            func.instruction(&Instruction::I32Xor);
                            IRType::Int
                        }
                        IRUnaryOp::UAdd => {
                            // No operation needed for unary +
                            IRType::Int
                        }
                    }
                }
            }
        }
        IRExpr::CompareOp { left, right, op } => {
            // Membership tests (`in` / `not in`) search a container rather than
            // comparing two scalars, so they are handled before the numeric
            // comparison logic below.
            if matches!(op, IRCompareOp::In | IRCompareOp::NotIn) {
                let elem_type = emit_expr(left, func, ctx, memory_layout, None);
                // The searched value is held across the container's emission,
                // which is arbitrary codegen: it used to sit in the scratch local
                // the search reads it from, so `"b" in d.keys()` had it
                // overwritten by the method call and compared garbage. It moves
                // into that scratch local only once the container is on the stack.
                let is_float_needle = matches!(elem_type, IRType::Float);
                let held_f64 = if is_float_needle {
                    ctx.hold_f64()
                } else {
                    None
                };
                let held = if is_float_needle {
                    None
                } else {
                    Some(crate::compiler::equality::hold(ctx))
                };
                match (held_f64, held) {
                    (Some(slot), _) => {
                        func.instruction(&Instruction::LocalSet(slot));
                    }
                    (None, Some(slot)) => {
                        if matches!(elem_type, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop); // length
                        }
                        func.instruction(&Instruction::LocalSet(slot));
                    }
                    (None, None) => {
                        ctx.report(
                            "an 'in' test is nested inside more float 'in' tests than the \
                             compiler reserves room for",
                        );
                        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                    }
                }
                let container_type = emit_expr(right, func, ctx, memory_layout, None);
                match (held_f64, held) {
                    (Some(slot), _) => {
                        func.instruction(&Instruction::LocalGet(slot));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                        ctx.release_held_f64();
                    }
                    (None, Some(slot)) => {
                        func.instruction(&Instruction::LocalGet(slot));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
                        crate::compiler::equality::release(ctx, 1);
                    }
                    (None, None) => {}
                }

                // A set is a hash table (constant-time probe); a list is a linear
                // scan. Other containers fall back to a conservative constant.
                let is_set = matches!(container_type, IRType::Set(_));
                // A dict searches its keys, which sit every DICT_ENTRY bytes
                // rather than every slot. `k in d` used to fall through to the
                // constant below and answer False for every key the dict
                // actually held.
                let is_dict = matches!(container_type, IRType::Dict(_, _));
                let searchable = is_set
                    || is_dict
                    || matches!(
                        container_type,
                        IRType::List(_) | IRType::Tuple(_) | IRType::VarTuple(_)
                    );

                // `sub in text` is a substring test, not a container scan.
                if matches!(container_type, IRType::String) {
                    let h_off = ctx.temp_local + 11;
                    let h_len = ctx.temp_local + 12;
                    let n_off = ctx.temp_local + 1;
                    let n_len = ctx.temp_local + 13;
                    func.instruction(&Instruction::LocalSet(h_len));
                    func.instruction(&Instruction::LocalSet(h_off));
                    // The needle's length was dropped when it was stashed; it
                    // is recoverable from the blob's own prefix word.
                    func.instruction(&Instruction::LocalGet(n_off));
                    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::LocalSet(n_len));
                    func.instruction(&Instruction::LocalGet(h_off));
                    func.instruction(&Instruction::LocalGet(h_len));
                    func.instruction(&Instruction::LocalGet(n_off));
                    func.instruction(&Instruction::LocalGet(n_len));
                    emit_string_search(func, ctx, SearchMode::Find);
                    func.instruction(&Instruction::I32Const(-1));
                    if matches!(op, IRCompareOp::NotIn) {
                        func.instruction(&Instruction::I32Eq);
                    } else {
                        func.instruction(&Instruction::I32Ne);
                    }
                    return IRType::Bool;
                }

                if !searchable {
                    // Answering a constant here is a silent wrong answer for
                    // any container that does hold the value, so this is a
                    // compile error instead.
                    ctx.report(format!(
                        "'in' is not supported on a value of type {} yet. \
                         Hint: search a list, tuple, set, dict, or str",
                        crate::type_to_string(&container_type)
                    ));
                    func.instruction(&Instruction::Drop); // container pointer
                    func.instruction(&Instruction::Unreachable);
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Bool;
                }

                // A tuple is searched position by position, each member at
                // its own type.
                if let IRType::Tuple(members) = &container_type {
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    emit_tuple_search(func, ctx, TupleSearch::Contains, members, &elem_type);
                    if matches!(op, IRCompareOp::NotIn) {
                        func.instruction(&Instruction::I32Eqz);
                    }
                    return IRType::Bool;
                }

                // A list or dict scan reads each slot at the elements' type.
                // It read them at the needle's, so `"a" in [1]` loaded the
                // int as a string and `1 in [1.0]` the float's low word.
                let mut scan_type = elem_type.clone();
                let mut exact_flag = None;
                let container_elem = match &container_type {
                    IRType::List(e) | IRType::VarTuple(e) => Some(e.as_ref().clone()),
                    IRType::Dict(k, _) => Some(k.as_ref().clone()),
                    _ => None,
                };
                if let Some(e) = container_elem.filter(|_| !is_set) {
                    let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
                    let int_like = |t: &IRType| matches!(t, IRType::Int | IRType::Bool);
                    let same = e == elem_type
                        || open(&e)
                        || open(&elem_type)
                        || (int_like(&e) && int_like(&elem_type))
                        || unify_element(&e, &elem_type).is_some()
                        || same_tuple_kind(&e, &elem_type);
                    if !same {
                        match (&e, &elem_type) {
                            (IRType::Float, n) if int_like(n) => {
                                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                                func.instruction(&Instruction::F64ConvertI32S);
                                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                                scan_type = IRType::Float;
                            }
                            // A float among ints matches only at an int value.
                            (e, IRType::Float) if int_like(e) => {
                                let Some(flag) = ctx.hold() else {
                                    report_literal_too_deep(func, ctx, "'in'");
                                    return IRType::Bool;
                                };
                                let v = ctx.temp_local_f64;
                                func.instruction(&Instruction::LocalGet(v));
                                func.instruction(&Instruction::LocalGet(v));
                                func.instruction(&Instruction::F64Trunc);
                                func.instruction(&Instruction::F64Eq);
                                func.instruction(&Instruction::LocalGet(v));
                                func.instruction(&Instruction::F64Abs);
                                func.instruction(&Instruction::F64Const(2147483648.0.into()));
                                func.instruction(&Instruction::F64Lt);
                                func.instruction(&Instruction::I32And);
                                func.instruction(&Instruction::LocalTee(flag));
                                func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                                func.instruction(&Instruction::LocalGet(v));
                                func.instruction(&Instruction::I32TruncF64S);
                                func.instruction(&Instruction::Else);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::End);
                                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
                                scan_type = e.clone();
                                exact_flag = Some(flag);
                            }
                            _ if tuple_member_compared(&e, &elem_type) == Some(false) => {
                                // Values of two distinct types are never equal.
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(matches!(
                                    op,
                                    IRCompareOp::NotIn
                                )
                                    as i32));
                                return IRType::Bool;
                            }
                            _ => {
                                ctx.report(format!(
                                    "testing a {} for membership among {} elements is not supported",
                                    crate::type_to_string(&elem_type),
                                    crate::type_to_string(&e)
                                ));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                        }
                    }
                }

                // Stack: (container_ptr); the needle is already stashed.
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // container_ptr
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // found

                if is_set {
                    // The table's own lookup, so `in` probes exactly where an
                    // insert would have put the member.
                    let member = match &container_type {
                        IRType::Set(e) if !matches!(e.as_ref(), IRType::Unknown | IRType::Any) => {
                            e.as_ref().clone()
                        }
                        _ => elem_type.clone(),
                    };
                    let set = crate::compiler::equality::hold(ctx);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::LocalSet(set));
                    let v = crate::compiler::sets::Held::of(ctx, &member);
                    match (&member, &elem_type) {
                        (IRType::Float, IRType::Float) => {
                            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
                        }
                        (IRType::Float, _) => {
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::F64ConvertI32S);
                        }
                        (_, IRType::Float) => {
                            ctx.report(
                                "testing a float for membership in a set of another type is \
                                 not supported",
                            );
                            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
                            func.instruction(&Instruction::I32TruncF64S);
                        }
                        _ => {
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                        }
                    }
                    v.set(func);
                    crate::compiler::sets::emit_contains(func, ctx, &member, set, v);
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                    v.release(ctx);
                    ctx.release_held();
                } else {
                    // List membership: linear scan over [count][elem0][elem1]...
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // count
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // counter

                    func.instruction(&Instruction::Block(BlockType::Empty));
                    func.instruction(&Instruction::Loop(BlockType::Empty));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32GeS);
                    func.instruction(&Instruction::BrIf(1));
                    // slot address = data + counter*stride. A dict entry is a
                    // key slot followed by a value slot, and `in` searches the
                    // keys, so it strides two slots at a time.
                    let stride = if is_dict { DICT_ENTRY } else { COLLECTION_SLOT };
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(stride as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    emit_slot_eq_needle(func, ctx, &scan_type, ctx.temp_local + 1);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // found = 1
                    func.instruction(&Instruction::Br(2));
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));
                    func.instruction(&Instruction::Br(0));
                    func.instruction(&Instruction::End); // loop
                    func.instruction(&Instruction::End); // block
                }

                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // found
                if let Some(flag) = exact_flag {
                    func.instruction(&Instruction::LocalGet(flag));
                    func.instruction(&Instruction::I32And);
                    ctx.release_held();
                }
                if matches!(op, IRCompareOp::NotIn) {
                    func.instruction(&Instruction::I32Eqz);
                }
                return IRType::Bool;
            }

            let left_type = emit_value(left, func, ctx, memory_layout, None);
            let right_type = emit_value(right, func, ctx, memory_layout, Some(&left_type));

            if let (IRType::Set(a), IRType::Set(b)) = (&left_type, &right_type) {
                return crate::compiler::operators::emit_set_comparison(func, ctx, op, a, b);
            }

            // Equality between class instances dispatches to `__eq__` when the
            // left operand's class defines or inherits one (dataclasses always
            // do). The two instance pointers already on the stack are exactly
            // the (self, other) argument pair. Dispatch is static, consistent
            // with the object model: the *static* type of the left operand
            // picks the implementation. Classes without `__eq__` keep pointer
            // identity, and ordering comparisons stay pointer-based.
            if matches!(op, IRCompareOp::Eq | IRCompareOp::NotEq)
                && matches!(right_type, IRType::Class(_))
            {
                if let IRType::Class(class_name) = &left_type {
                    let eq_method = ctx
                        .get_class_info(class_name)
                        .and_then(|ci| ci.methods.get("__eq__").copied());
                    if let Some(eq_idx) = eq_method {
                        // `==` dispatches on the left operand, and a subclass
                        // that overrides `__eq__` must be the one that answers.
                        // Both operands are already on the stack, so the
                        // receiver is under the argument and the general
                        // helper (which emits its own arguments) does not fit;
                        // the index is computed from a copy of the receiver
                        // taken before the argument was pushed.
                        match ctx.virtual_call(class_name, "__eq__") {
                            Some((column, type_index)) => {
                                emit_virtual_compare(func, ctx, column, type_index);
                            }
                            None => emit_user_call(func, ctx, eq_idx),
                        }
                        if matches!(op, IRCompareOp::NotEq) {
                            func.instruction(&Instruction::I32Eqz);
                        }
                        return IRType::Bool;
                    }
                }
            }

            // Ordering between class instances dispatches to the rich
            // comparison method, trying the left operand's own (`a < b` is
            // `a.__lt__(b)`) and then the right operand's reflection (`b.__gt__(a)`),
            // as CPython does. A class with neither is a compile error: CPython
            // raises `TypeError`, and comparing the two instance pointers, which
            // is what this did before (#120), answered by allocation order.
            if matches!(
                op,
                IRCompareOp::Lt | IRCompareOp::LtE | IRCompareOp::Gt | IRCompareOp::GtE
            ) && (matches!(left_type, IRType::Class(_))
                || matches!(right_type, IRType::Class(_)))
            {
                let (own, reflected, symbol) = match op {
                    IRCompareOp::Lt => ("__lt__", "__gt__", "<"),
                    IRCompareOp::LtE => ("__le__", "__ge__", "<="),
                    IRCompareOp::Gt => ("__gt__", "__lt__", ">"),
                    _ => ("__ge__", "__le__", ">="),
                };
                let method_of = |ty: &IRType, name: &str| match ty {
                    IRType::Class(c) => ctx
                        .get_class_info(c)
                        .and_then(|ci| ci.methods.get(name).copied()),
                    _ => None,
                };
                if let (IRType::Class(_), IRType::Class(_)) = (&left_type, &right_type) {
                    if let Some(idx) = method_of(&left_type, own) {
                        emit_user_call(func, ctx, idx);
                        return IRType::Bool;
                    }
                    if let Some(idx) = method_of(&right_type, reflected) {
                        // Swap the two instance pointers so the right operand
                        // is `self`.
                        func.instruction(&Instruction::LocalSet(ctx.temp_local));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                        emit_user_call(func, ctx, idx);
                        return IRType::Bool;
                    }
                }
                ctx.report(format!(
                    "'{symbol}' not supported between instances of '{}' and '{}'",
                    crate::type_to_string(&left_type),
                    crate::type_to_string(&right_type)
                ));
                for t in [&right_type, &left_type] {
                    func.instruction(&Instruction::Drop);
                    if matches!(t, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                }
                func.instruction(&Instruction::I32Const(0));
                return IRType::Bool;
            }

            // Two collections compare by value. Every operator here compared the
            // two pointers, so `(a, 2) == (1, 2)` and `[1, 2] == [1, 2]` were
            // False and `(1, 2) < (1, 3)` answered by allocation order.
            let is_collection = |t: &IRType| {
                matches!(
                    t,
                    IRType::List(_)
                        | IRType::Tuple(_)
                        | IRType::VarTuple(_)
                        | IRType::Dict(_, _)
                        | IRType::Set(_)
                )
            };
            // A tuple of any length and a fixed tuple are both tuples, laid out
            // alike: a fixed one whose members all share the other's member
            // type compares as one of any length.
            let (left_type, right_type) = match (&left_type, &right_type) {
                (IRType::VarTuple(m), IRType::Tuple(ms)) if ms.iter().all(|t| t == m.as_ref()) => {
                    (left_type.clone(), left_type.clone())
                }
                (IRType::Tuple(ms), IRType::VarTuple(m)) if ms.iter().all(|t| t == m.as_ref()) => {
                    (right_type.clone(), right_type.clone())
                }
                (IRType::VarTuple(_), IRType::Tuple(_))
                | (IRType::Tuple(_), IRType::VarTuple(_)) => {
                    ctx.report(format!(
                        "comparing a {} with a {} is not supported; give both the same \
                         member types",
                        crate::type_to_string(&left_type),
                        crate::type_to_string(&right_type)
                    ));
                    (left_type.clone(), left_type.clone())
                }
                _ => (left_type, right_type),
            };
            if is_collection(&left_type) || is_collection(&right_type) {
                let drop_operand = |func: &mut Function, t: &IRType| {
                    func.instruction(&Instruction::Drop);
                    if matches!(t, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                };
                // Identity is a pointer comparison between two single-word values.
                if matches!(op, IRCompareOp::Is | IRCompareOp::IsNot)
                    && is_collection(&left_type)
                    && is_collection(&right_type)
                {
                    func.instruction(&if matches!(op, IRCompareOp::Is) {
                        Instruction::I32Eq
                    } else {
                        Instruction::I32Ne
                    });
                    return IRType::Bool;
                }
                let same_kind =
                    std::mem::discriminant(&left_type) == std::mem::discriminant(&right_type);
                if !same_kind {
                    // A list never equals a tuple, and a collection never equals
                    // a number or a string. Ordering them is a TypeError.
                    drop_operand(func, &right_type);
                    drop_operand(func, &left_type);
                    match op {
                        IRCompareOp::Eq => {
                            func.instruction(&Instruction::I32Const(0));
                        }
                        IRCompareOp::NotEq => {
                            func.instruction(&Instruction::I32Const(1));
                        }
                        _ => {
                            ctx.report(format!(
                                "comparing a {} with a {} is not supported",
                                crate::type_to_string(&left_type),
                                crate::type_to_string(&right_type)
                            ));
                            func.instruction(&Instruction::I32Const(0));
                        }
                    }
                    return IRType::Bool;
                }
                // Same kind, different element or member types: comparing them
                // needs cross-type equality (`(1, 2) == (1.0, 2)` is True), which
                // is refused rather than read at the wrong width.
                if left_type != right_type {
                    ctx.report(format!(
                        "comparing a {} with a {} is not supported; give both the same \
                         element types",
                        crate::type_to_string(&left_type),
                        crate::type_to_string(&right_type)
                    ));
                    drop_operand(func, &right_type);
                    drop_operand(func, &left_type);
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Bool;
                }
                let why = match op {
                    IRCompareOp::Eq | IRCompareOp::NotEq => {
                        crate::compiler::equality::eq_unsupported(ctx, &left_type)
                    }
                    _ => crate::compiler::equality::order_unsupported(ctx, &left_type),
                };
                if let Some(why) = why {
                    ctx.report(format!("comparing {why} is not supported"));
                    drop_operand(func, &right_type);
                    drop_operand(func, &left_type);
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Bool;
                }
                let a = crate::compiler::equality::hold(ctx);
                let b = crate::compiler::equality::hold(ctx);
                func.instruction(&Instruction::LocalSet(b));
                func.instruction(&Instruction::LocalSet(a));
                match op {
                    IRCompareOp::Eq | IRCompareOp::NotEq => {
                        crate::compiler::equality::emit_values_eq(func, ctx, &left_type, a, b);
                        if matches!(op, IRCompareOp::NotEq) {
                            func.instruction(&Instruction::I32Eqz);
                        }
                    }
                    _ => crate::compiler::equality::emit_values_order(
                        func, ctx, &left_type, a, b, op,
                    ),
                }
                crate::compiler::equality::release(ctx, 2);
                return IRType::Bool;
            }

            // String/bytes comparison: each operand is an (offset, length) pair,
            // so the stack holds (left_off, left_len, right_off, right_len). The
            // numeric paths below assume single-word scalars and would compare
            // only the top word (the right operand's length) while stranding the
            // left pair. Handle str/bytes here: Eq/NotEq compare contents
            // byte-for-byte — interned constants share an offset, but
            // runtime-built strings (concatenation, slices) do not, so an offset
            // compare is insufficient. Ordering/identity comparisons aren't
            // supported yet and yield a constant after balancing the stack. See #90.
            if matches!(left_type, IRType::String | IRType::Bytes)
                && matches!(right_type, IRType::String | IRType::Bytes)
            {
                match op {
                    IRCompareOp::Eq | IRCompareOp::NotEq => {
                        let left_off = ctx.temp_local;
                        let left_len = ctx.temp_local + 1;
                        let right_off = ctx.temp_local + 2;
                        let right_len = ctx.temp_local + 3;
                        let result = ctx.temp_local + 4;
                        let counter = ctx.temp_local + 5;

                        func.instruction(&Instruction::LocalSet(right_len));
                        func.instruction(&Instruction::LocalSet(right_off));
                        func.instruction(&Instruction::LocalSet(left_len));
                        func.instruction(&Instruction::LocalSet(left_off));

                        // result = 1 (equal until a mismatch is found)
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::LocalSet(result));

                        func.instruction(&Instruction::Block(BlockType::Empty));
                        // Different lengths => not equal.
                        func.instruction(&Instruction::LocalGet(left_len));
                        func.instruction(&Instruction::LocalGet(right_len));
                        func.instruction(&Instruction::I32Ne);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(result));
                        func.instruction(&Instruction::Br(1)); // exit outer block
                        func.instruction(&Instruction::End);

                        // Compare bytes until the end or a mismatch.
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(counter));
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        // counter >= len => every byte matched; result stays 1.
                        func.instruction(&Instruction::LocalGet(counter));
                        func.instruction(&Instruction::LocalGet(left_len));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1)); // exit outer block
                                                                 // left[counter]
                        func.instruction(&Instruction::LocalGet(left_off));
                        func.instruction(&Instruction::LocalGet(counter));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Load8U(MemArg {
                            offset: 0,
                            align: 0,
                            memory_index: 0,
                        }));
                        // right[counter]
                        func.instruction(&Instruction::LocalGet(right_off));
                        func.instruction(&Instruction::LocalGet(counter));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Load8U(MemArg {
                            offset: 0,
                            align: 0,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Ne);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(result));
                        func.instruction(&Instruction::Br(2)); // exit outer block
                        func.instruction(&Instruction::End);
                        // counter += 1
                        func.instruction(&Instruction::LocalGet(counter));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(counter));
                        func.instruction(&Instruction::Br(0)); // continue loop
                        func.instruction(&Instruction::End); // loop
                        func.instruction(&Instruction::End); // block

                        func.instruction(&Instruction::LocalGet(result));
                        if matches!(op, IRCompareOp::NotEq) {
                            func.instruction(&Instruction::I32Eqz);
                        }
                    }
                    _ => {
                        // Ordering and identity. These used to drop both pairs
                        // and answer False, whatever the strings were, so
                        // `"apple" < "banana"` and even `a is a` were False.
                        let a = crate::compiler::equality::hold(ctx);
                        let b = crate::compiler::equality::hold(ctx);
                        func.instruction(&Instruction::Drop); // right length
                        func.instruction(&Instruction::LocalSet(b));
                        func.instruction(&Instruction::Drop); // left length
                        func.instruction(&Instruction::LocalSet(a));
                        match op {
                            // Identity: the same blob. Strings are immutable, so
                            // this can only be True for one object reached twice.
                            IRCompareOp::Is | IRCompareOp::IsNot => {
                                func.instruction(&Instruction::LocalGet(a));
                                func.instruction(&Instruction::LocalGet(b));
                                func.instruction(&if matches!(op, IRCompareOp::Is) {
                                    Instruction::I32Eq
                                } else {
                                    Instruction::I32Ne
                                });
                            }
                            _ => crate::compiler::equality::emit_values_order(
                                func, ctx, &left_type, a, b, op,
                            ),
                        }
                        crate::compiler::equality::release(ctx, 2);
                    }
                }
                return IRType::Bool;
            }

            // Handle type coercion for comparison
            if left_type == IRType::Float && right_type == IRType::Int {
                func.instruction(&Instruction::F64ConvertI32S);
            } else if left_type == IRType::Int && right_type == IRType::Float {
                // Move stack: f64 under i32
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                func.instruction(&Instruction::F64ConvertI32S);
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
            }

            if left_type == IRType::Float || right_type == IRType::Float {
                // Float comparisons
                match op {
                    IRCompareOp::Eq => {
                        func.instruction(&Instruction::F64Eq);
                    }
                    IRCompareOp::NotEq => {
                        func.instruction(&Instruction::F64Eq);
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Xor); // Invert
                    }
                    IRCompareOp::Lt => {
                        func.instruction(&Instruction::F64Lt);
                    }
                    IRCompareOp::LtE => {
                        func.instruction(&Instruction::F64Le);
                    }
                    IRCompareOp::Gt => {
                        func.instruction(&Instruction::F64Gt);
                    }
                    IRCompareOp::GtE => {
                        func.instruction(&Instruction::F64Ge);
                    }
                    // `in` is handled above; identity between two numbers is
                    // not something Python defines.
                    IRCompareOp::In | IRCompareOp::NotIn | IRCompareOp::Is | IRCompareOp::IsNot => {
                        ctx.report("'is' between two numbers is not supported: use '=='");
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                    }
                }
            } else {
                // Integer comparisons
                match op {
                    IRCompareOp::Eq => {
                        func.instruction(&Instruction::I32Eq);
                    }
                    IRCompareOp::NotEq => {
                        func.instruction(&Instruction::I32Ne);
                    }
                    IRCompareOp::Lt => {
                        func.instruction(&Instruction::I32LtS);
                    }
                    IRCompareOp::LtE => {
                        func.instruction(&Instruction::I32LeS);
                    }
                    IRCompareOp::Gt => {
                        func.instruction(&Instruction::I32GtS);
                    }
                    IRCompareOp::GtE => {
                        func.instruction(&Instruction::I32GeS);
                    }
                    IRCompareOp::Is | IRCompareOp::IsNot => {
                        emit_identity(func, ctx, op, &left_type, &right_type);
                    }
                    // `in` is handled above.
                    IRCompareOp::In | IRCompareOp::NotIn => {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                    }
                }
            }

            IRType::Bool
        }
        IRExpr::BoolOp { left, right, op } => {
            match op {
                IRBoolOp::And => {
                    // Short-circuit AND operation
                    emit_expr(left, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));

                    // If-else pattern for short-circuit evaluation. Both arms
                    // leave the boolean result, so the if yields an i32.
                    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                    ctx.enter_expr_blocks(1);
                    emit_expr(right, func, ctx, memory_layout, Some(&IRType::Bool));
                    ctx.exit_expr_blocks(1);
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::I32Const(0)); // False
                    func.instruction(&Instruction::End);
                }
                IRBoolOp::Or => {
                    // Short-circuit OR operation
                    emit_expr(left, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));

                    // If-else pattern for short-circuit evaluation. Both arms
                    // leave the boolean result, so the if yields an i32.
                    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                    func.instruction(&Instruction::I32Const(1)); // True
                    func.instruction(&Instruction::Else);
                    ctx.enter_expr_blocks(1);
                    emit_expr(right, func, ctx, memory_layout, Some(&IRType::Bool));
                    ctx.exit_expr_blocks(1);
                    func.instruction(&Instruction::End);
                }
            }

            IRType::Bool
        }
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => emit_conditional(
            condition,
            body,
            orelse,
            func,
            ctx,
            memory_layout,
            expected_type,
        ),
        IRExpr::Keyword { name, .. } => {
            ctx.report(format!("keyword argument '{name}' is not supported here"));
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            // `from mod import f as g` on a user-written module (#41): calls
            // to `g` resolve to the merged `f` — the imported module's
            // definitions are statically linked into this single WASM module.
            // A real definition named `g` always wins over the alias.
            let resolved_alias = ctx.resolve_import_alias(function_name).to_string();
            let function_name = &resolved_alias;

            // `from json import dumps` then `dumps(x)`: a call into the module.
            if ctx.get_function_info(function_name).is_none() {
                if let Some((module, attribute)) = ctx.stdlib_imports.get(function_name).cloned() {
                    return emit_expr(
                        &IRExpr::MethodCall {
                            object: Box::new(IRExpr::Variable(module)),
                            method_name: attribute,
                            arguments: arguments.clone(),
                        },
                        func,
                        ctx,
                        memory_layout,
                        expected_type,
                    );
                }
            }

            // `"template" % args`, lowered per conversion.
            // The name of an instance's class, for an exception's repr.
            if function_name == UNBOX_FN {
                let ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                let message = match &arguments[1] {
                    IRExpr::Const(IRConstant::String(m)) => m.clone(),
                    _ => String::new(),
                };
                let Some(inner) = boxed_scalar(&ty).cloned() else {
                    return ty;
                };
                emit_unbox(func, ctx, &inner, &message);
                return inner;
            }
            if function_name == crate::compiler::exceptions::CLASS_NAME_FN {
                if let Some(arg) = arguments.first() {
                    emit_expr(arg, func, ctx, memory_layout, None);
                }
                func.instruction(&Instruction::Call(
                    ctx.helper(crate::compiler::runtime::Helper::ClassName),
                ));
                recover_str_pair(func, ctx);
                return IRType::String;
            }

            // `print(...)` and `input([prompt])`, through the host.
            if matches!(function_name.as_str(), "print" | "input")
                && ctx.get_function_info(function_name).is_none()
            {
                return if function_name == "print" {
                    emit_print(arguments, func, ctx, memory_layout)
                } else {
                    emit_input(arguments, func, ctx, memory_layout)
                };
            }

            if function_name == "__percent_field" {
                return emit_percent_field(arguments, func, ctx, memory_layout);
            }
            if function_name == "__percent_item" {
                return emit_percent_item(arguments, func, ctx, memory_layout);
            }

            // `format(value[, spec])` and `ascii(value)`.
            if matches!(function_name.as_str(), "format" | "ascii")
                && ctx.get_function_info(function_name).is_none()
            {
                return emit_format_call(function_name, arguments, func, ctx, memory_layout);
            }

            // `min(xs)`, `max(xs, key=f, default=d)`: a fold over one iterable.
            if matches!(function_name.as_str(), "min" | "max")
                && ctx.get_function_info(function_name).is_none()
                && (arguments.len() == 1
                    || arguments
                        .iter()
                        .any(|a| matches!(a, IRExpr::Keyword { .. })))
            {
                return emit_min_max_iterable(
                    function_name == "min",
                    arguments,
                    func,
                    ctx,
                    memory_layout,
                );
            }

            // The finalize pass places keywords for this module's own functions
            // and classes; any left are for a callee whose parameters it could
            // not see.
            if let Some(IRExpr::Keyword { name, .. }) = arguments
                .iter()
                .find(|a| matches!(a, IRExpr::Keyword { .. }))
            {
                ctx.report(format!(
                    "keyword argument '{name}' is not supported in a call to \
                     '{function_name}()'. Hint: pass it positionally"
                ));
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }

            // Iterator-protocol intrinsic (see `ir::generators`): leave the
            // current StopIteration flag (global 1) on the stack and clear it.
            if function_name == crate::ir::STOP_CHECK_FN {
                func.instruction(&Instruction::GlobalGet(1));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::GlobalSet(1));
                return IRType::Bool;
            }

            // Positional dict-entry access backing `for k, v in d.items()` and
            // the key and value loops (see `ir::converter`): entry i's key sits
            // at data + i*DICT_ENTRY, its value one slot later. Each loads at
            // the dict's own key or value type. Both loaded an int word, so a
            // string key bound as an int and a float value lost its width.
            if function_name == crate::ir::DICT_KEY_AT_FN
                || function_name == crate::ir::DICT_VAL_AT_FN
            {
                let [dict, index] = arguments.as_slice() else {
                    ctx.report(format!("{function_name} takes a dict and an index"));
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                };
                let dict_type = emit_expr(dict, func, ctx, memory_layout, None);
                let (slot, ty) = match (&dict_type, function_name == crate::ir::DICT_VAL_AT_FN) {
                    (IRType::Dict(k, _), false) => (0, (**k).clone()),
                    (IRType::Dict(_, v), true) => (COLLECTION_SLOT, (**v).clone()),
                    (_, is_value) => (if is_value { COLLECTION_SLOT } else { 0 }, IRType::Unknown),
                };
                emit_data_base(func);
                emit_expr(index, func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                if slot > 0 {
                    func.instruction(&Instruction::I32Const(slot as i32));
                    func.instruction(&Instruction::I32Add);
                }
                load_collection_word(func, &ty, ctx.temp_local + 1);
                return ty;
            }

            // Class instantiation: `ClassName(args)`. Handled before the generic
            // argument emission so the instance pointer (`self`) is the first
            // argument to `__init__` and the user arguments are coerced to their
            // declared parameter types (e.g. int literals widened to f64).
            //
            // Each instantiation calls the runtime instance allocator
            // `__alloc_obj(instance_size, class_id)`, so every `ClassName(...)`
            // yields a distinct heap pointer (tagged with its class id at
            // offset 0 for `isinstance`) and multiple instances coexist. The
            // sequence is stack-only (alloc result -> self arg -> `__init__`
            // returns `self` back), so nested instantiations in the argument
            // list compose without clobbering any scratch local. Fresh heap
            // memory is zero, so unassigned fields read as 0/0.0.
            // `cls(...)` inside a classmethod constructs the defining class,
            // resolved statically through the `cls` parameter's type.
            if let Some(class_target) = static_class_target(ctx, function_name) {
                // An exception keeps what it was constructed with.
                if crate::compiler::exceptions::is_exception_class(ctx, &class_target) {
                    return emit_exception_construct(
                        &class_target,
                        arguments,
                        func,
                        ctx,
                        memory_layout,
                    );
                }
                let (instance_size, class_id) = ctx
                    .get_class_info(&class_target)
                    .map(|c| (c.instance_size, c.class_id))
                    .unwrap_or((0, 0));
                let init_idx = ctx
                    .get_class_info(&class_target)
                    .and_then(|c| c.methods.get("__init__").copied());
                // The class that textually defines `__init__` — the subclass
                // itself, or the base it inherits the constructor from.
                let init_owner = ctx
                    .get_class_info(&class_target)
                    .and_then(|c| c.method_owner.get("__init__").cloned())
                    .unwrap_or_else(|| class_target.clone());

                if let Some(init_idx) = init_idx {
                    // __init__ parameter types, `self` first.
                    let param_types: Vec<IRType> = ctx
                        .get_function_info(&format!("{init_owner}::__init__"))
                        .map(|f| f.param_types.clone())
                        .unwrap_or_default();
                    // self = __alloc_obj(instance_size, class_id), left on the
                    // stack as the first argument to __init__.
                    func.instruction(&Instruction::I32Const(instance_size as i32));
                    func.instruction(&Instruction::I32Const(class_id));
                    func.instruction(&Instruction::Call(ctx.alloc_obj_func_index));
                    for (i, arg) in arguments.iter().enumerate() {
                        let t = emit_expr(arg, func, ctx, memory_layout, param_types.get(i + 1));
                        check_class_arg(ctx, &t, param_types.get(i + 1), i, &class_target);
                        // A string/bytes argument is an (offset, length) pair
                        // but each parameter is one i32 slot; narrow it to the
                        // offset word (the callee recovers the length from the
                        // blob prefix), matching the user-function convention.
                        if matches!(t, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                    }
                    // __init__ is compiled to return `self`, so the call's
                    // result is the freshly allocated instance pointer.
                    emit_user_call(func, ctx, init_idx);
                } else {
                    // No constructor: evaluate and discard any arguments, then
                    // allocate the (zeroed, tagged) instance.
                    for arg in arguments {
                        let t = emit_expr(arg, func, ctx, memory_layout, None);
                        func.instruction(&Instruction::Drop);
                        if matches!(t, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                    }
                    func.instruction(&Instruction::I32Const(instance_size as i32));
                    func.instruction(&Instruction::I32Const(class_id));
                    func.instruction(&Instruction::Call(ctx.alloc_obj_func_index));
                }
                return IRType::Class(class_target);
            }

            // `issubclass(Sub, Base)` — both arguments are bare class-name
            // tokens, so the answer folds to a compile-time constant and no
            // argument code is emitted at all. Anything else answered False.
            if function_name == "issubclass" {
                if let (Some(IRExpr::Variable(sub)), Some(IRExpr::Variable(base)), 2) =
                    (arguments.first(), arguments.get(1), arguments.len())
                {
                    if ctx.get_class_info(sub).is_some() && ctx.get_class_info(base).is_some() {
                        let result = ctx.is_class_or_subclass(sub, base);
                        func.instruction(&Instruction::I32Const(result as i32));
                        return IRType::Bool;
                    }
                }
                ctx.report("issubclass() is supported between two classes this program defines");
                func.instruction(&Instruction::I32Const(0));
                return IRType::Bool;
            }

            // `isinstance(obj, target)`: a class this program defines is
            // checked against the instance's class tag; a builtin type, which
            // this compiler's values carry statically, folds to a constant. A
            // tuple of targets is any of them. It answered False for every
            // target that was not a user class, `isinstance(3, int)` included.
            if function_name == "isinstance" {
                let targets: Option<Vec<&str>> = match arguments.get(1) {
                    Some(IRExpr::Variable(t)) => Some(vec![t.as_str()]),
                    Some(IRExpr::TupleLiteral(items)) => items
                        .iter()
                        .map(|i| match i {
                            IRExpr::Variable(t) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect(),
                    _ => None,
                };
                let (Some(obj), Some(targets), 2) = (arguments.first(), targets, arguments.len())
                else {
                    ctx.report("isinstance() takes a value and a class, a builtin type, or a tuple of them");
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Bool;
                };
                let obj_type = emit_expr(obj, func, ctx, memory_layout, None);
                return emit_isinstance(func, ctx, &obj_type, &targets);
            }

            // Calling a closure-valued name (#43): the callee is a local (or a
            // module variable) holding a closure environment pointer, not a
            // statically known function. Dispatch through the funcref table:
            // push the arguments, the environment pointer (the lambda's
            // trailing `__env` parameter), and the table slot stored in the
            // environment's first word, then `call_indirect` with the
            // signature for this arity. A known `def` of the same name wins
            // (checked first), preserving the existing static-call behavior.
            if ctx.get_function_info(function_name.as_str()).is_none() {
                if let Some((callee, callee_type)) = closure_callee(function_name, ctx) {
                    if ctx.closure_type_base.is_some() {
                        return emit_closure_call(
                            &callee,
                            &callee_type,
                            function_name,
                            arguments,
                            func,
                            ctx,
                            memory_layout,
                        );
                    }
                }
            }

            // `@functools.singledispatch` (#120): the implementation is chosen
            // by the static type of the first argument, so that argument is
            // emitted first to learn its type, then the arm is picked and the
            // rest of the call proceeds exactly like a call to that arm.
            if let Some(arms) = ctx.dispatch_tables.get(function_name.as_str()) {
                let Some(first) = arguments.first() else {
                    ctx.report(format!(
                        "'{function_name}' is a @singledispatch function and needs at least \
                         one argument to dispatch on"
                    ));
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                };
                let first_type = emit_expr(first, func, ctx, memory_layout, None);
                if matches!(first_type, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
                let arm = select_dispatch_arm(ctx, arms, &first_type);
                if arm.is_none() && matches!(first_type, IRType::Unknown | IRType::Any) {
                    ctx.report(format!(
                        "cannot dispatch '{function_name}': the type of its argument is not \
                         known. Hint: annotate the value it is called with"
                    ));
                }
                let target = arm.unwrap_or(function_name.as_str());
                let Some(info) = ctx.get_function_info(target) else {
                    ctx.report(format!(
                        "'{function_name}' dispatches to '{target}', which is not compiled"
                    ));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                };
                let (index, return_type, param_types) = (
                    info.index,
                    info.return_type.clone(),
                    info.param_types.clone(),
                );
                for (i, arg) in arguments.iter().enumerate().skip(1) {
                    let t = emit_expr(arg, func, ctx, memory_layout, param_types.get(i));
                    check_class_arg(ctx, &t, param_types.get(i), i, function_name);
                    if matches!(t, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                }
                emit_user_call(func, ctx, index);
                finish_user_call(func, ctx, &return_type);
                return return_type;
            }

            // Push arguments onto the stack in order. For a call to a known user
            // function, a string/bytes argument must be narrowed to its single
            // offset word: each parameter is one i32 slot, so the callee recovers
            // the length from the blob prefix (load(offset - STRING_LEN_PREFIX))
            // rather than receiving it as a second word. Without this drop the
            // length (top of the pair) is passed as the offset, so the callee
            // loads out of bounds. Built-in calls keep the full (offset, length)
            // pair their lowering expects.
            // Look up the function index if it exists in our context
            if let Some(func_info) = ctx.get_function_info(function_name.as_str()) {
                let (index, return_type, param_types) = (
                    func_info.index,
                    func_info.return_type.clone(),
                    func_info.param_types.clone(),
                );
                for (i, arg) in arguments.iter().enumerate() {
                    emit_user_argument(
                        arg,
                        param_types.get(i),
                        i,
                        function_name,
                        func,
                        ctx,
                        memory_layout,
                    );
                }
                emit_user_call(func, ctx, index);
                // A function returns a single word; a string/bytes result is
                // its offset, so rebuild the (offset, length) pair consumers
                // expect from the blob prefix.
                finish_user_call(func, ctx, &return_type);
                return_type
            } else {
                // The builtins that walk their first argument walk a set as
                // the list of its members, in iteration order.
                let walks = matches!(function_name.as_str(), "sum" | "sorted" | "__sorted_desc");
                let mut arg_types = Vec::new();
                for (i, arg) in arguments.iter().enumerate() {
                    let t = emit_value(arg, func, ctx, memory_layout, None);
                    arg_types.push(if walks && i == 0 {
                        emit_as_sequence(func, ctx, t)
                    } else {
                        t
                    });
                }
                // Built-in functions
                match function_name.as_str() {
                    // `type(x)` of an instance is its class, read from the
                    // instance's class tag. The builtin types are not values
                    // here, so `type()` of anything else is refused.
                    "type" if arg_types.len() == 1 => match &arg_types[0] {
                        IRType::Class(class) => {
                            func.instruction(&Instruction::I32Load(slot_arg()));
                            IRType::ClassRef(class.clone())
                        }
                        IRType::ClassRef(_) => {
                            func.instruction(&Instruction::Drop);
                            ctx.report(
                                "type() of a class is its metaclass, which is not a value \
                                 here",
                            );
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Unknown
                        }
                        other => {
                            let words = if matches!(other, IRType::String | IRType::Bytes) {
                                2
                            } else {
                                1
                            };
                            for _ in 0..words {
                                func.instruction(&Instruction::Drop);
                            }
                            ctx.report(format!(
                                "type() of a '{}' is not supported: only the class of an \
                                 instance of a class this program defines is a value here",
                                crate::compiler::operators::python_type_name(other)
                            ));
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Unknown
                        }
                    },
                    "len" => {
                        if arg_types.len() != 1 {
                            ctx.report(format!(
                                "len() takes exactly one argument ({} given)",
                                arg_types.len()
                            ));
                            for t in arg_types.iter().rev() {
                                func.instruction(&Instruction::Drop);
                                if matches!(t, IRType::String | IRType::Bytes) {
                                    func.instruction(&Instruction::Drop);
                                }
                            }
                            func.instruction(&Instruction::I32Const(0));
                            return IRType::Int;
                        }
                        match &arg_types[0] {
                            // A str's length is in code points; it answered
                            // its UTF-8 bytes, so `len("aß")` was 3.
                            IRType::String => {
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Call(
                                    ctx.helper(crate::compiler::runtime::Helper::CharLen),
                                ));
                            }
                            IRType::Bytes => {
                                // (offset, length) on stack with length on top.
                                // Keep the length, discard the offset below it.
                                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                            }
                            // Lists, dicts, sets and tuples store their
                            // element/entry count in their first word.
                            IRType::List(_)
                            | IRType::Tuple(_)
                            | IRType::VarTuple(_)
                            | IRType::Dict(_, _)
                            | IRType::Set(_) => {
                                func.instruction(&Instruction::I32Load(slot_arg()));
                            }
                            // A range block is [start][stop][step]; its length
                            // is what CPython computes. This read the start
                            // as a count, so len(range(5)) answered 0.
                            IRType::Range => emit_range_len(func, ctx),
                            // The compiler's own loop lowering measures the
                            // collections it parks in `__`-named locals and
                            // generator fields, which are always collections.
                            IRType::Unknown if is_compiler_name(&arguments[0]) => {
                                func.instruction(&Instruction::I32Load(slot_arg()));
                            }
                            // Anything else has no length in Python (a
                            // TypeError), and an untyped value is a word here:
                            // reading it as a count answered garbage.
                            other => {
                                let hint = if matches!(other, IRType::Unknown | IRType::Any) {
                                    ". Hint: annotate the value it comes from"
                                } else {
                                    ""
                                };
                                ctx.report(format!(
                                    "object of type '{}' has no len(){hint}",
                                    crate::compiler::operators::python_type_name(other)
                                ));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                            }
                        }
                        IRType::Int
                    }
                    "open" => {
                        // open(path[, mode]) -> file (#25). The path's
                        // (offset, length) pair feeds `waspy_host.open`
                        // directly; the mode must be a string literal and is
                        // folded to host flag bits at compile time (a fresh
                        // "r"/"w" literal need not be interned in memory).
                        if let Some(io) = ctx.file_io {
                            // The generic argument emission above already
                            // pushed every argument; discard everything after
                            // the path pair (the mode, if present).
                            for t in arg_types.iter().skip(1) {
                                match t {
                                    IRType::String | IRType::Bytes => {
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    _ => {
                                        func.instruction(&Instruction::Drop);
                                    }
                                }
                            }
                            let flags = arguments
                                .get(1)
                                .map(file_mode_flags)
                                .unwrap_or(FILE_FLAG_READ);
                            func.instruction(&Instruction::I32Const(flags));
                            func.instruction(&Instruction::Call(io.open));
                            // Through WASI a failure is CPython's exception.
                            crate::compiler::function::emit_post_call_check(func, ctx);
                        } else {
                            // Unreachable in practice: the module scan emits
                            // the host imports whenever `open` appears. Keep
                            // the stack balanced regardless.
                            for t in &arg_types {
                                match t {
                                    IRType::String | IRType::Bytes => {
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    _ => {
                                        func.instruction(&Instruction::Drop);
                                    }
                                }
                            }
                            func.instruction(&Instruction::I32Const(-1));
                        }
                        IRType::File
                    }
                    "print" => {
                        // Pop the arguments off the stack
                        for arg_type in &arg_types {
                            match arg_type {
                                IRType::String | IRType::Bytes => {
                                    // Strings/bytes are (offset, length), drop both
                                    func.instruction(&Instruction::Drop);
                                    func.instruction(&Instruction::Drop);
                                }
                                _ => {
                                    // All other types are single values
                                    func.instruction(&Instruction::Drop);
                                }
                            }
                        }
                        IRType::None
                    }
                    "min" | "max" => emit_min_max(func, ctx, function_name == "min", &arg_types),
                    // `set(iterable)`; `set()` is lowered to an empty display.
                    "set" if arg_types.len() == 1 => {
                        crate::compiler::sets::emit_set_from(func, ctx, &arg_types[0])
                    }
                    "int" => {
                        // int(x): truncate a float, parse a string, pass an int
                        // through. A string used to be treated as an int: the
                        // arm answered its length and left its offset on the
                        // stack, so `int("12")` was 2 and every argument after
                        // it in a call shifted by one.
                        match arg_types.first() {
                            Some(IRType::Float) => {
                                func.instruction(&Instruction::I32TruncF64S);
                            }
                            Some(IRType::String | IRType::Bytes) => {
                                emit_parse_int(func, ctx);
                            }
                            Some(
                                t @ (IRType::List(_)
                                | IRType::VarTuple(_)
                                | IRType::Dict(_, _)
                                | IRType::Set(_)
                                | IRType::Tuple(_)
                                | IRType::Class(_)
                                | IRType::None),
                            ) => {
                                ctx.report(format!(
                                    "int() of a value of type '{}' is not supported; CPython \
                                     raises TypeError for it",
                                    crate::type_to_string(t)
                                ));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                            }
                            _ => {}
                        }
                        IRType::Int
                    }
                    "abs" => {
                        match (arg_types.len(), arg_types.first()) {
                            (1, Some(IRType::Float)) => {
                                func.instruction(&Instruction::F64Abs);
                                IRType::Float
                            }
                            (1, Some(IRType::Int | IRType::Bool | IRType::Unknown)) => {
                                // |x| = (x ^ s) - s with s = x >> 31. Wraps for
                                // i32::MIN, as all int arithmetic here does.
                                let x = ctx.temp_local;
                                func.instruction(&Instruction::LocalTee(x));
                                func.instruction(&Instruction::LocalGet(x));
                                func.instruction(&Instruction::I32Const(31));
                                func.instruction(&Instruction::I32ShrS);
                                func.instruction(&Instruction::I32Xor);
                                func.instruction(&Instruction::LocalGet(x));
                                func.instruction(&Instruction::I32Const(31));
                                func.instruction(&Instruction::I32ShrS);
                                func.instruction(&Instruction::I32Sub);
                                IRType::Int
                            }
                            _ => {
                                ctx.report(format!(
                                    "abs() takes one number, got {}",
                                    arg_types
                                        .iter()
                                        .map(crate::type_to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ));
                                for t in arg_types.iter().rev() {
                                    func.instruction(&Instruction::Drop);
                                    if matches!(t, IRType::String | IRType::Bytes) {
                                        func.instruction(&Instruction::Drop);
                                    }
                                }
                                func.instruction(&Instruction::I32Const(0));
                                IRType::Int
                            }
                        }
                    }
                    "round" => {
                        // round(x) and round(x, ndigits), with CPython's
                        // semantics: halves go to the even neighbour, judged on
                        // the exact binary value of x.
                        let refuse = |func: &mut Function, what: String| {
                            ctx.report(what);
                            for t in arg_types.iter().rev() {
                                func.instruction(&Instruction::Drop);
                                if matches!(t, IRType::String | IRType::Bytes) {
                                    func.instruction(&Instruction::Drop);
                                }
                            }
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Int
                        };
                        let ndigits_const = match arguments.get(1) {
                            Some(IRExpr::Const(IRConstant::Int(n))) => Some(*n),
                            _ => None,
                        };
                        match (arg_types.as_slice(), ndigits_const) {
                            ([IRType::Float], _) => {
                                // f64.nearest is round-half-to-even on the exact
                                // value; the conversion traps outside an i32.
                                func.instruction(&Instruction::F64Nearest);
                                func.instruction(&Instruction::I32TruncF64S);
                                IRType::Int
                            }
                            ([IRType::Int | IRType::Bool], _) => IRType::Int,
                            ([IRType::Float, IRType::Int], _) => {
                                emit_round_ndigits(func, ctx);
                                IRType::Float
                            }
                            // An int rounded to zero or more digits is itself.
                            ([IRType::Int | IRType::Bool, IRType::Int], Some(n)) if n >= 0 => {
                                func.instruction(&Instruction::Drop);
                                IRType::Int
                            }
                            ([IRType::Int | IRType::Bool, IRType::Int], _) => refuse(
                                func,
                                "round() of an int to a negative or computed number of digits \
                                 is not supported"
                                    .to_string(),
                            ),
                            _ => refuse(
                                func,
                                format!(
                                    "round() takes a number and an optional int, got {}",
                                    arg_types
                                        .iter()
                                        .map(crate::type_to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            ),
                        }
                    }
                    "float" => {
                        // float(x): widen an int to f64; floats pass through.
                        match arg_types.first() {
                            Some(IRType::Float) => {}
                            Some(IRType::String | IRType::Bytes) => {
                                // A literal is parsed here, where its digits
                                // are known; anything else by the correctly
                                // rounded runtime parser, which raises
                                // ValueError where CPython does.
                                func.instruction(&Instruction::Drop);
                                let literal = match arguments.first() {
                                    Some(IRExpr::Const(IRConstant::String(text))) => Some(text),
                                    _ => None,
                                };
                                match literal.map(|text| (text, parse_python_float(text))) {
                                    Some((_, Ok(value))) => {
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::F64Const(value.into()));
                                    }
                                    Some((text, Err(why))) => {
                                        ctx.report(format!("float({text:?}) {why}"));
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::F64Const(0.0.into()));
                                    }
                                    None => {
                                        func.instruction(&Instruction::Call(
                                            ctx.helper(
                                                crate::compiler::runtime::Helper::ParseFloat,
                                            ),
                                        ));
                                        crate::compiler::function::emit_post_call_check(func, ctx);
                                    }
                                }
                            }
                            _ => {
                                func.instruction(&Instruction::F64ConvertI32S);
                            }
                        }
                        IRType::Float
                    }
                    "str" | "repr" if arg_types.len() == 1 => {
                        // str(x) and repr(x) of every type CPython renders the
                        // same way each time: each type's rendering is a
                        // runtime helper, a collection's calling its members'.
                        let repr = function_name == "repr";
                        let ty = arg_types[0].clone();
                        if !repr && matches!(ty, IRType::String) {
                            return IRType::String;
                        }
                        if let Some(why) = crate::compiler::runtime::str_unsupported(ctx, &ty, repr)
                        {
                            ctx.report(format!("{function_name}() of {why} is not supported"));
                        }
                        // A string or bytes value is passed as its offset.
                        if matches!(ty, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                        let ty = match ty {
                            IRType::Unknown | IRType::Any => IRType::Int,
                            other => other,
                        };
                        let helper = if repr {
                            crate::compiler::runtime::Helper::Repr(ty)
                        } else {
                            crate::compiler::runtime::Helper::Str(ty)
                        };
                        func.instruction(&Instruction::Call(ctx.helper(helper)));
                        recover_str_pair(func, ctx);
                        IRType::String
                    }
                    "sorted" | "__sorted_desc" => {
                        // sorted(seq[, key][, reverse]) returns a new list; the
                        // original is untouched. This used to fall through to
                        // the generic builtin path and answer a value that had
                        // nothing to do with sorting, while reporting success.
                        //
                        // The lowering folds `reverse` into the name and makes
                        // `key` the second argument, so both are known here.
                        let descending = function_name == "__sorted_desc";
                        if arg_types.is_empty() {
                            ctx.report("sorted() takes an iterable");
                            func.instruction(&Instruction::Unreachable);
                            func.instruction(&Instruction::I32Const(0));
                            return IRType::Unknown;
                        }
                        let elem_ty = match &arg_types[0] {
                            IRType::List(inner) | IRType::VarTuple(inner) => (**inner).clone(),
                            IRType::Tuple(members) => {
                                members.first().cloned().unwrap_or(IRType::Unknown)
                            }
                            other => {
                                ctx.report(format!(
                                    "sorted() needs a list or tuple, got {}",
                                    crate::type_to_string(other)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Unknown;
                            }
                        };
                        // With a key the comparison runs on what the key
                        // returns; without one it runs on the elements.
                        let key_expr = arguments.get(1);
                        let key_ty = match key_expr {
                            Some(k) => {
                                // A key that reaches into its parameter needs
                                // that parameter's type, which a lambda does
                                // not carry, so it would compile to a wrong
                                // answer rather than a failure.
                                let untyped_key = !matches!(
                                    arg_types.get(1),
                                    Some(IRType::Callable { params, .. })
                                        if params.first().is_some_and(|p| !matches!(p, IRType::Unknown | IRType::Any))
                                );
                                if let IRExpr::ClosureMake { lambda_name, .. } = k {
                                    if let Some((param, body)) = ctx.lambda_bodies.get(lambda_name)
                                    {
                                        if untyped_key && key_needs_param_type(body, param) {
                                            ctx.report(
                                                "sorted()'s 'key' cannot index its argument, \
                                                 call a method on it, or pass it to len(): a \
                                                 lambda parameter carries no type, so those \
                                                 read the value wrongly. Hint: sort by the \
                                                 element itself, or build the list you want \
                                                 sorted first",
                                            );
                                        }
                                    }
                                }
                                let mut inferred = infer_key_type(k, &elem_ty, ctx);
                                // A key function's declared or inferred result.
                                if let (
                                    IRType::Unknown,
                                    Some(IRType::Callable { return_type, .. }),
                                ) = (&inferred, arg_types.get(1))
                                {
                                    inferred = return_type.as_ref().clone();
                                }
                                if matches!(inferred, IRType::Unknown) {
                                    ctx.report(
                                        "sorted() cannot tell what its 'key' returns, so it \
                                         cannot know how to compare two of them. Hint: give \
                                         the sequence a known element type, and have the key \
                                         return the element, one of its members, or a tuple \
                                         of those",
                                    );
                                }
                                inferred
                            }
                            None => elem_ty.clone(),
                        };
                        // Comparing values whose type is unknown would compare
                        // the raw slot words: for a list of tuples that is
                        // comparing pointers, which leaves the list in
                        // allocation order while reporting success.
                        if matches!(key_ty, IRType::Unknown) {
                            ctx.report(
                                "sorted() cannot tell what type it is ordering, so it cannot \
                                 compare two of them. Hint: annotate the sequence (for example \
                                 `xs: List[Tuple[int, str]] = []`) so its element type is known",
                            );
                        }

                        // The sequence pointer is already on the stack; the key
                        // closure's environment is emitted once, before the
                        // loops, and reused for every call.
                        // A dedicated high range: everything below is claimed
                        // by the expression codegen this arm calls into (the
                        // key closure's own construction, and `emit_key_cmp`),
                        // and these have to survive across both.
                        let src = ctx.temp_local + 32;
                        let n = ctx.temp_local + 33;
                        let out = ctx.temp_local + 34;
                        let keys = ctx.temp_local + 35;
                        let env = ctx.temp_local + 36;
                        let i = ctx.temp_local + 37;
                        let j = ctx.temp_local + 38;
                        let addr_a = ctx.temp_local + 39;
                        let addr_b = ctx.temp_local + 40;
                        // The saved element and key are kept as i32 halves,
                        // since the scratch run is i32 and an f64 element must
                        // keep both of its words. These indices stay clear of
                        // the ones `emit_key_cmp` claims, because the saved
                        // values have to survive every comparison it emits.
                        let elem_lo = ctx.temp_local + 41;
                        let elem_hi = ctx.temp_local + 42;
                        let key_lo = ctx.temp_local + 43;
                        let key_hi = ctx.temp_local + 44;

                        // Every argument was already emitted before this
                        // match, so the key closure's environment sits on top
                        // of the sequence pointer and comes off first.
                        if key_expr.is_some() {
                            func.instruction(&Instruction::LocalSet(env));
                        }
                        func.instruction(&Instruction::LocalSet(src));
                        func.instruction(&Instruction::LocalGet(src));
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(n));

                        // out = a fresh list holding a copy of the elements.
                        func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                        func.instruction(&Instruction::LocalGet(n));
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::Call(ctx.alloc_func_index));
                        func.instruction(&Instruction::LocalSet(out));
                        store_runtime_data_ptr(func, out);
                        func.instruction(&Instruction::LocalGet(out));
                        func.instruction(&Instruction::LocalGet(n));
                        func.instruction(&Instruction::I32Store(slot_arg()));
                        func.instruction(&Instruction::LocalGet(out));
                        func.instruction(&Instruction::LocalGet(n));
                        func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));
                        func.instruction(&Instruction::LocalGet(out));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(src));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(n));
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });

                        if key_expr.is_some() {
                            // keys[i] = key(out[i]), computed once per element
                            // rather than on every comparison, which is what
                            // Python's own sort does.
                            func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                            func.instruction(&Instruction::LocalGet(n));
                            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                            func.instruction(&Instruction::I32Mul);
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::Call(ctx.alloc_func_index));
                            func.instruction(&Instruction::LocalSet(keys));
                            store_runtime_data_ptr(func, keys);
                            func.instruction(&Instruction::LocalGet(keys));
                            func.instruction(&Instruction::LocalGet(n));
                            func.instruction(&Instruction::I32Store(slot_arg()));

                            let Some(type_base) = ctx.closure_type_base else {
                                ctx.report("sorted()'s 'key' needs closure support in this module");
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::List(Box::new(elem_ty));
                            };
                            // The key is called at its own signature where its
                            // type says what that is, so a float element or a
                            // float key keeps its width.
                            let (key_param, key_result) = match arg_types.get(1) {
                                Some(IRType::Callable {
                                    params,
                                    return_type,
                                }) if params.len() == 1 => {
                                    (params[0].clone(), return_type.as_ref().clone())
                                }
                                _ => (IRType::Unknown, IRType::Unknown),
                            };
                            if matches!(elem_ty, IRType::Float)
                                && !matches!(key_param, IRType::Float)
                            {
                                ctx.report(
                                    "sorted()'s 'key' takes an int where the elements are \
                                     floats. Hint: annotate the key function's parameter as \
                                     a float",
                                );
                            }
                            let key_type_index = if matches!(key_param, IRType::Unknown)
                                && matches!(key_result, IRType::Unknown)
                            {
                                type_base + 1
                            } else {
                                let wanted = (
                                    vec![wasm_value_type(&key_param), ValType::I32],
                                    wasm_value_type(&key_result),
                                );
                                match ctx.signature_types.get(&wanted) {
                                    Some(index) => *index,
                                    None => {
                                        ctx.report(
                                            "sorted()'s 'key' has no signature this module \
                                             defines; this is a code generation bug",
                                        );
                                        type_base + 1
                                    }
                                }
                            };

                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::LocalSet(i));
                            func.instruction(&Instruction::Block(BlockType::Empty));
                            func.instruction(&Instruction::Loop(BlockType::Empty));
                            func.instruction(&Instruction::LocalGet(i));
                            func.instruction(&Instruction::LocalGet(n));
                            func.instruction(&Instruction::I32GeS);
                            func.instruction(&Instruction::BrIf(1));
                            // Destination slot, pushed before the value.
                            func.instruction(&Instruction::LocalGet(keys));
                            emit_data_base(func);
                            func.instruction(&Instruction::LocalGet(i));
                            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                            func.instruction(&Instruction::I32Mul);
                            func.instruction(&Instruction::I32Add);
                            // key(out[i]): the element word, then the closure
                            // environment, then the table slot it names.
                            func.instruction(&Instruction::LocalGet(out));
                            emit_data_base(func);
                            func.instruction(&Instruction::LocalGet(i));
                            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                            func.instruction(&Instruction::I32Mul);
                            func.instruction(&Instruction::I32Add);
                            match (&elem_ty, &key_param) {
                                (IRType::Float, IRType::Float) => {
                                    func.instruction(&Instruction::F64Load(slot_arg()));
                                }
                                (IRType::Int | IRType::Bool, IRType::Float) => {
                                    func.instruction(&Instruction::I32Load(slot_arg()));
                                    func.instruction(&Instruction::F64ConvertI32S);
                                }
                                _ => {
                                    func.instruction(&Instruction::I32Load(slot_arg()));
                                }
                            }
                            func.instruction(&Instruction::LocalGet(env));
                            func.instruction(&Instruction::LocalGet(env));
                            func.instruction(&Instruction::I32Load(slot_arg()));
                            func.instruction(&Instruction::CallIndirect {
                                type_index: key_type_index,
                                table_index: 0,
                            });
                            if matches!(key_result, IRType::Float) {
                                func.instruction(&Instruction::F64Store(slot_arg()));
                            } else {
                                func.instruction(&Instruction::I32Store(slot_arg()));
                            }
                            func.instruction(&Instruction::LocalGet(i));
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalSet(i));
                            func.instruction(&Instruction::Br(0));
                            func.instruction(&Instruction::End);
                            func.instruction(&Instruction::End);
                        }

                        // Insertion sort, which is stable: equal keys keep the
                        // order they came in, exactly as Python's sort does.
                        let cmp_base = if key_expr.is_some() { keys } else { out };
                        // The saved key needs an address of its own to compare
                        // against: shifting overwrites slot `i` on the first
                        // move, so comparing against the array position there
                        // would compare against the value just shifted in.
                        let saved = ctx.temp_local + 45;
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::Call(ctx.alloc_func_index));
                        func.instruction(&Instruction::LocalSet(saved));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::LocalSet(i));
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        func.instruction(&Instruction::LocalGet(i));
                        func.instruction(&Instruction::LocalGet(n));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1));

                        // Save element i and its key.
                        func.instruction(&Instruction::LocalGet(out));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(i));
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(addr_a));
                        func.instruction(&Instruction::LocalGet(addr_a));
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(elem_lo));
                        func.instruction(&Instruction::LocalGet(addr_a));
                        func.instruction(&Instruction::I32Load(mem_off(4)));
                        func.instruction(&Instruction::LocalSet(elem_hi));
                        func.instruction(&Instruction::LocalGet(cmp_base));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(i));
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(addr_b));
                        func.instruction(&Instruction::LocalGet(addr_b));
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(key_lo));
                        func.instruction(&Instruction::LocalGet(addr_b));
                        func.instruction(&Instruction::I32Load(mem_off(4)));
                        func.instruction(&Instruction::LocalSet(key_hi));
                        func.instruction(&Instruction::LocalGet(saved));
                        func.instruction(&Instruction::LocalGet(key_lo));
                        func.instruction(&Instruction::I32Store(slot_arg()));
                        func.instruction(&Instruction::LocalGet(saved));
                        func.instruction(&Instruction::LocalGet(key_hi));
                        func.instruction(&Instruction::I32Store(mem_off(4)));

                        func.instruction(&Instruction::LocalGet(i));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::LocalSet(j));

                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        func.instruction(&Instruction::LocalGet(j));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32LtS);
                        func.instruction(&Instruction::BrIf(1));
                        // Compare key[j] against the saved key, which is parked
                        // in a scratch slot so it has an address to compare.
                        func.instruction(&Instruction::LocalGet(cmp_base));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(j));
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(addr_a));
                        emit_key_cmp(func, ctx, &key_ty, addr_a, saved);
                        func.instruction(&Instruction::I32Const(0));
                        if descending {
                            func.instruction(&Instruction::I32LtS);
                        } else {
                            func.instruction(&Instruction::I32GtS);
                        }
                        func.instruction(&Instruction::I32Eqz);
                        func.instruction(&Instruction::BrIf(1));

                        // Shift element and key one place right.
                        for base in [out, cmp_base] {
                            func.instruction(&Instruction::LocalGet(base));
                            emit_data_base(func);
                            func.instruction(&Instruction::LocalGet(j));
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                            func.instruction(&Instruction::I32Mul);
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalGet(base));
                            emit_data_base(func);
                            func.instruction(&Instruction::LocalGet(j));
                            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                            func.instruction(&Instruction::I32Mul);
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::I64Load(slot_arg()));
                            func.instruction(&Instruction::I64Store(slot_arg()));
                            if base == out && key_expr.is_none() {
                                break;
                            }
                        }

                        func.instruction(&Instruction::LocalGet(j));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::LocalSet(j));
                        func.instruction(&Instruction::Br(0));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::End);

                        // Drop the saved element and key into the hole.
                        func.instruction(&Instruction::LocalGet(out));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(j));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(addr_a));
                        func.instruction(&Instruction::LocalGet(addr_a));
                        func.instruction(&Instruction::LocalGet(elem_lo));
                        func.instruction(&Instruction::I32Store(slot_arg()));
                        func.instruction(&Instruction::LocalGet(addr_a));
                        func.instruction(&Instruction::LocalGet(elem_hi));
                        func.instruction(&Instruction::I32Store(mem_off(4)));
                        if key_expr.is_some() {
                            func.instruction(&Instruction::LocalGet(keys));
                            emit_data_base(func);
                            func.instruction(&Instruction::LocalGet(j));
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                            func.instruction(&Instruction::I32Mul);
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalSet(addr_b));
                            func.instruction(&Instruction::LocalGet(addr_b));
                            func.instruction(&Instruction::LocalGet(key_lo));
                            func.instruction(&Instruction::I32Store(slot_arg()));
                            func.instruction(&Instruction::LocalGet(addr_b));
                            func.instruction(&Instruction::LocalGet(key_hi));
                            func.instruction(&Instruction::I32Store(mem_off(4)));
                        }

                        func.instruction(&Instruction::LocalGet(i));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(i));
                        func.instruction(&Instruction::Br(0));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::End);

                        func.instruction(&Instruction::LocalGet(out));
                        IRType::List(Box::new(elem_ty))
                    }
                    "sum" => {
                        if arg_types.is_empty() {
                            ctx.report("sum() takes at least one argument, got 0");
                            func.instruction(&Instruction::I32Const(0));
                            return IRType::Int;
                        }
                        // sum(iterable[, start]) over a list/tuple pointer:
                        // loop the [len][slot0][slot1]... layout, accumulating
                        // into a scratch local. Float elements accumulate at
                        // f64 width; everything else uses the slot's low i32
                        // word (bools sum as 0/1, like Python).
                        let elem_type = match &arg_types[0] {
                            IRType::List(elem) | IRType::VarTuple(elem) => (**elem).clone(),
                            // A tuple sums at f64 width only when every member
                            // is a float; sets use a hash-table layout this
                            // linear walk can't read, so they fall through.
                            IRType::Tuple(types) => {
                                if !types.is_empty()
                                    && types.iter().all(|t| matches!(t, IRType::Float))
                                {
                                    IRType::Float
                                } else {
                                    IRType::Int
                                }
                            }
                            // Anything else cannot be walked as `[len][slots]`.
                            // This used to answer the argument itself, so
                            // `sum()` of a set, or of a list whose element type
                            // had been lost, returned its pointer as the total.
                            other => {
                                ctx.report(format!(
                                    "sum() of a value of type '{}' is not supported. Hint: sum a \
                                     list or tuple whose element type is known, for example \
                                     one annotated 'List[int]'",
                                    crate::type_to_string(other)
                                ));
                                for t in arg_types.iter().rev() {
                                    func.instruction(&Instruction::Drop);
                                    if matches!(t, IRType::String | IRType::Bytes) {
                                        func.instruction(&Instruction::Drop);
                                    }
                                }
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                        };
                        let is_float = matches!(elem_type, IRType::Float);
                        let ptr = ctx.temp_local;
                        let len = ctx.temp_local + 1;
                        let idx = ctx.temp_local + 2;
                        let acc_i32 = ctx.temp_local + 3;
                        let acc_f64 = ctx.temp_local_f64;

                        // Seed the accumulator with `start` (top of stack when
                        // present) or zero, then pop the iterable pointer.
                        if arg_types.len() == 2 {
                            if is_float {
                                if !matches!(arg_types[1], IRType::Float) {
                                    func.instruction(&Instruction::F64ConvertI32S);
                                }
                                func.instruction(&Instruction::LocalSet(acc_f64));
                            } else {
                                func.instruction(&Instruction::LocalSet(acc_i32));
                            }
                        } else if is_float {
                            func.instruction(&Instruction::F64Const(0.0.into()));
                            func.instruction(&Instruction::LocalSet(acc_f64));
                        } else {
                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::LocalSet(acc_i32));
                        }
                        func.instruction(&Instruction::LocalSet(ptr));

                        // len = *ptr; idx = 0.
                        func.instruction(&Instruction::LocalGet(ptr));
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(len));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(idx));

                        // while idx < len: acc += slot[idx]; idx += 1.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        func.instruction(&Instruction::LocalGet(idx));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1));

                        if is_float {
                            func.instruction(&Instruction::LocalGet(acc_f64));
                        } else {
                            func.instruction(&Instruction::LocalGet(acc_i32));
                        }
                        func.instruction(&Instruction::LocalGet(ptr));
                        emit_data_base(func);
                        func.instruction(&Instruction::LocalGet(idx));
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        if is_float {
                            func.instruction(&Instruction::F64Load(mem_off(0)));
                            func.instruction(&Instruction::F64Add);
                            func.instruction(&Instruction::LocalSet(acc_f64));
                        } else {
                            func.instruction(&Instruction::I32Load(mem_off(0)));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalSet(acc_i32));
                        }

                        func.instruction(&Instruction::LocalGet(idx));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(idx));
                        func.instruction(&Instruction::Br(0));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::End);

                        if is_float {
                            func.instruction(&Instruction::LocalGet(acc_f64));
                            IRType::Float
                        } else {
                            func.instruction(&Instruction::LocalGet(acc_i32));
                            IRType::Int
                        }
                    }
                    _ => {
                        // A name that is neither a compiled function nor a
                        // builtin the compiler implements. This used to push
                        // a 0 and report success, so a call to `reduce`,
                        // `partial`, `abs`, or a misspelled name answered 0
                        // (#120). Balance the stack and report it.
                        for t in arg_types.iter().rev() {
                            func.instruction(&Instruction::Drop);
                            if matches!(t, IRType::String | IRType::Bytes) {
                                func.instruction(&Instruction::Drop);
                            }
                        }
                        ctx.report(format!(
                            "call to '{function_name}', which is not a function this \
                             program defines or a builtin the compiler implements"
                        ));
                        func.instruction(&Instruction::I32Const(0));
                        IRType::Unknown
                    }
                }
            }
        }
        IRExpr::ListLiteral(elements) => {
            // List layout in memory: [length:i32][elem0][elem1]... Each element
            // occupies one COLLECTION_SLOT (8 bytes), wide enough for a lossless
            // f64; narrower values use the slot's low word.

            // Each evaluation builds into its own fresh block, whose pointer
            // lives in a held slot while the elements are emitted. It used to
            // build into one compile-time region per literal: a function
            // called twice returned the same object both times, and a literal
            // whose element called back into its own function (`[n, f(n - 1)]`)
            // had its earlier elements overwritten by the inner call.
            let list_size = COLLECTION_HEADER + elements.len() as u32 * COLLECTION_SLOT;
            let Some(blk) = ctx.hold() else {
                report_literal_too_deep(func, ctx, "list");
                return IRType::List(Box::new(IRType::Unknown));
            };
            emit_literal_block(func, ctx, blk, list_size);
            emit_literal_header(func, blk, elements.len() as u32);

            let mut element_types = Vec::with_capacity(elements.len());
            for (i, elem) in elements.iter().enumerate() {
                // A store pops the value, then the address, so the block pointer
                // goes first and the slot offset rides in the store's offset.
                func.instruction(&Instruction::LocalGet(blk));
                let ty = emit_expr(elem, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &ty);
                store_collection_word_at(func, &ty, COLLECTION_HEADER + i as u32 * COLLECTION_SLOT);
                element_types.push(ty);
            }
            ctx.release_held();
            check_uniform_slot_width(ctx, "list literal", &element_types);
            func.instruction(&Instruction::LocalGet(blk));
            IRType::List(Box::new(literal_element_type(
                ctx,
                "list literal",
                &element_types,
            )))
        }
        IRExpr::SetLiteral(elements) => {
            // The members' types first, so the table knows how each hashes and
            // compares; then the set is built as CPython builds a display.
            let types: Vec<IRType> = elements
                .iter()
                .map(|e| probe_type(e, ctx, memory_layout, None).0)
                .collect();
            check_uniform_slot_width(ctx, "set literal", &types);
            let member = literal_element_type(ctx, "set literal", &types);
            if !elements.is_empty() {
                if let Some(why) = crate::compiler::sets::member_unsupported(ctx, &member) {
                    ctx.report(format!("{why} cannot be a set member"));
                }
            }
            crate::compiler::sets::emit_display(func, ctx, memory_layout, elements, &member);
            IRType::Set(Box::new(member))
        }
        IRExpr::TupleLiteral(elements) => {
            // Tuple layout in memory: [length:i32][elem0][elem1]... One
            // COLLECTION_SLOT per element, matching list storage.

            // Built into a fresh block per evaluation; see the list literal.
            let tuple_size = COLLECTION_HEADER + elements.len() as u32 * COLLECTION_SLOT;
            let Some(blk) = ctx.hold() else {
                report_literal_too_deep(func, ctx, "tuple");
                return IRType::Tuple(vec![]);
            };
            emit_literal_block(func, ctx, blk, tuple_size);
            emit_literal_header(func, blk, elements.len() as u32);

            let mut element_types = Vec::with_capacity(elements.len());
            for (i, elem) in elements.iter().enumerate() {
                func.instruction(&Instruction::LocalGet(blk));
                let elem_type = emit_expr(elem, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &elem_type);
                store_collection_word_at(
                    func,
                    &elem_type,
                    COLLECTION_HEADER + i as u32 * COLLECTION_SLOT,
                );
                element_types.push(elem_type);
            }
            ctx.release_held();
            // Typed per position, so an int beside a float is fine: a walk
            // that would read them at one width goes through
            // `emit_as_sequence`, which refuses the mix.
            func.instruction(&Instruction::LocalGet(blk));
            IRType::Tuple(element_types)
        }
        IRExpr::DictLiteral(pairs) => emit_dict_literal(pairs, None, func, ctx, memory_layout),
        IRExpr::CellNew => {
            // One slot, wide enough for any captured value.
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            IRType::Int
        }
        IRExpr::CellLoad { cell } => {
            let Some(index) = ctx.get_local_index(cell) else {
                ctx.report(format!(
                    "closure cell '{cell}' is not in scope; this is a code generation bug"
                ));
                func.instruction(&Instruction::Unreachable);
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            };
            let ty = ctx
                .cell_types
                .borrow()
                .get(cell)
                .cloned()
                .unwrap_or(IRType::Unknown);
            func.instruction(&Instruction::LocalGet(index));
            match ty {
                IRType::Float => {
                    func.instruction(&Instruction::F64Load(mem_off_aligned(0, 3)));
                }
                IRType::String | IRType::Bytes => {
                    func.instruction(&Instruction::I32Load(mem_off(0)));
                    recover_str_pair(func, ctx);
                }
                _ => {
                    func.instruction(&Instruction::I32Load(mem_off(0)));
                }
            }
            ty
        }
        IRExpr::CellStore { cell, value } => {
            let Some(index) = ctx.get_local_index(cell) else {
                ctx.report(format!(
                    "closure cell '{cell}' is not in scope; this is a code generation bug"
                ));
                func.instruction(&Instruction::Unreachable);
                return IRType::None;
            };
            let known = ctx.cell_types.borrow().get(cell).cloned();
            func.instruction(&Instruction::LocalGet(index));
            let value_type = emit_value(value, func, ctx, memory_layout, known.as_ref());
            // A cell holds one variable, which keeps one type: the first store
            // fixes it, and every closure reads it back at that type.
            let ty = match (&known, &value_type) {
                (None, t) => t.clone(),
                (Some(k), t) if k == t => k.clone(),
                (Some(IRType::Float), IRType::Int | IRType::Bool) => {
                    func.instruction(&Instruction::F64ConvertI32S);
                    IRType::Float
                }
                (Some(k), IRType::Unknown) if is_word_type(k) => k.clone(),
                (Some(IRType::Unknown), t) if is_word_type(t) => t.clone(),
                (Some(k), t) if is_word_type(k) && is_word_type(t) => k.clone(),
                (Some(IRType::Optional(inner)), t)
                    if matches!(t, IRType::None) || **inner == *t =>
                {
                    IRType::Optional(inner.clone())
                }
                (Some(k), t) if same_shape(k, t) => {
                    ctx.check_class_store(t, k, "a variable a closure captures");
                    k.clone()
                }
                (Some(k), t) => {
                    ctx.report(format!(
                        "a variable a closure captures holds a '{}' and is then assigned a \
                         '{}', and a closure reads it at one type. Hint: use a separate variable",
                        crate::type_to_string(k),
                        crate::type_to_string(t)
                    ));
                    k.clone()
                }
            };
            ctx.cell_types.borrow_mut().insert(cell.clone(), ty.clone());
            match &value_type {
                // A string or bytes value is an (offset, length) pair; the cell
                // keeps the offset, the same word a collection slot keeps, and
                // the length comes back from the blob's prefix on the way out.
                IRType::String | IRType::Bytes => {
                    func.instruction(&Instruction::Drop);
                }
                _ => {}
            }
            if matches!(ty, IRType::Float) {
                func.instruction(&Instruction::F64Store(mem_off_aligned(0, 3)));
            } else {
                func.instruction(&Instruction::I32Store(mem_off(0)));
            }
            IRType::None
        }
        IRExpr::EnvRead { env, slot } => {
            // A closure environment is [table_slot][captured0][captured1]...,
            // sharing the collection slot layout so a capture sits at
            // HEADER + slot*SLOT. The compiler generates both the block and
            // every read of it, so the slot is in range by construction and
            // there is nothing to check.
            let Some(index) = ctx.get_local_index(env) else {
                ctx.report(format!(
                    "closure environment '{env}' is not in scope; this is a code generation bug"
                ));
                func.instruction(&Instruction::Unreachable);
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            };
            func.instruction(&Instruction::LocalGet(index));
            func.instruction(&Instruction::I32Load(mem_off(
                (COLLECTION_HEADER + slot * COLLECTION_SLOT) as u64,
            )));
            IRType::Unknown
        }
        IRExpr::Indexing { container, index } => {
            let container_type = emit_expr(container, func, ctx, memory_layout, None);
            // Hint the index with the container's key type. List/tuple/string
            // indices are ints; a float-keyed dict wants the key kept as an f64 so
            // it isn't coerced to int and mis-compared (`{1.5: ...}[1.5]`).
            let index_hint = match &container_type {
                IRType::Dict(key_type, _) if matches!(key_type.as_ref(), IRType::Float) => {
                    IRType::Float
                }
                _ => IRType::Int,
            };
            let index_type = emit_expr(index, func, ctx, memory_layout, Some(&index_hint));
            // A string key arrives as an (offset, length) pair; a dict slot
            // holds one word, so the length is dropped and the offset is what
            // gets compared. The same narrowing happens on the assignment side.
            narrow_element_to_word(func, &index_type);

            match container_type {
                IRType::String => {
                    // String indexing returns a single-character string.
                    // Stack: (offset, length, index) -> result (blob, 1). It
                    // used to be `(offset + index, 1)`, a pointer into the
                    // source: `len(ch)` in a function handed `s[1]` answered
                    // 1627389952. See `emit_char_at`.
                    // The index counts code points, so it is checked
                    // against the length in code points and then turned
                    // into the character's byte offset.
                    let idx = ctx.temp_local;
                    let off = ctx.temp_local + INDEX_PTR;
                    let len = ctx.temp_local + INDEX_LEN;
                    func.instruction(&Instruction::LocalSet(idx));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::LocalTee(off));
                    func.instruction(&Instruction::Call(
                        ctx.helper(crate::compiler::runtime::Helper::CharLen),
                    ));
                    func.instruction(&Instruction::LocalSet(len));
                    emit_index_check(func, ctx, idx, len, "string index out of range");
                    func.instruction(&Instruction::LocalGet(off));
                    func.instruction(&Instruction::LocalGet(idx));
                    func.instruction(&Instruction::Call(
                        ctx.helper(crate::compiler::runtime::Helper::CharOffset),
                    ));
                    func.instruction(&Instruction::LocalSet(idx));
                    emit_char_at(
                        func,
                        ctx,
                        off,
                        idx,
                        ctx.temp_local + 1,
                        ctx.temp_local + 2,
                        ctx.temp_local + 3,
                    );
                    IRType::String
                }
                IRType::Bytes => {
                    // Bytes indexing returns an integer (byte value 0-255).
                    // Stack: (offset, length, index) -> result (byte)
                    let idx = ctx.temp_local;
                    let off = ctx.temp_local + INDEX_PTR;
                    let len = ctx.temp_local + INDEX_LEN;
                    func.instruction(&Instruction::LocalSet(idx));
                    func.instruction(&Instruction::LocalSet(len));
                    func.instruction(&Instruction::LocalSet(off));
                    emit_index_check(func, ctx, idx, len, "index out of range");

                    func.instruction(&Instruction::LocalGet(off));
                    func.instruction(&Instruction::LocalGet(idx));
                    func.instruction(&Instruction::I32Add); // offset + index
                                                            // Load unsigned byte (0-255)
                    func.instruction(&Instruction::I32Load8U(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));

                    IRType::Int
                }
                IRType::List(ref element_type) | IRType::VarTuple(ref element_type) => {
                    // List indexing: list is stored as [length:i32][elem0][elem1]...
                    // Stack: (list_ptr, index). Address = list_ptr + HEADER + index*SLOT.
                    let what = if matches!(container_type, IRType::List(_)) {
                        "list index out of range"
                    } else {
                        "tuple index out of range"
                    };
                    emit_sequence_address(func, ctx, what);
                    load_collection_word(func, element_type.as_ref(), ctx.temp_local + 1);

                    element_type.as_ref().clone()
                }
                IRType::Dict(key_type, value_type) => {
                    // Dictionary indexing using linear search.
                    // Dict layout: [num_entries:i32][key0][val0][key1][val1]...
                    // (each key/value is one COLLECTION_SLOT). Stack: (dict_ptr,
                    // search_key) with search_key on top. Keys and values are both
                    // compared at their natural width: a float key is an f64 (kept
                    // in the second f64 scratch so it can coexist with a float
                    // value in `temp_local_f64`); everything else is an i32 word.
                    let is_float_key = matches!(key_type.as_ref(), IRType::Float);
                    let is_float_value = matches!(value_type.as_ref(), IRType::Float);
                    // Stash the search key at its natural width.
                    if is_float_key {
                        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
                    } else {
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
                    }
                    func.instruction(&Instruction::LocalSet(ctx.temp_local)); // dict_ptr

                    // Load the number of entries
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // num_entries

                    // Initialize counter and result (result defaults to 0 / 0.0
                    // when the key is absent). Using a result local avoids
                    // leaving a stray value on the stack on the found path. Float
                    // values are captured into the dedicated f64 scratch.
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // counter
                    if is_float_value {
                        func.instruction(&Instruction::F64Const(f64_const(0.0)));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                    } else {
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                        // result
                    }

                    // Whether the key turned up. A dict read for a key the
                    // dict does not hold used to answer 0 (or 0.0), which is a
                    // real value and indistinguishable from a stored one;
                    // Python raises KeyError, and so does this now.
                    let found = ctx.temp_local + INDEX_LEN;
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalSet(found));

                    // Loop: while counter < num_entries
                    func.instruction(&Instruction::Block(BlockType::Empty));
                    func.instruction(&Instruction::Loop(BlockType::Empty));

                    // Check if counter >= num_entries
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32GeS);
                    func.instruction(&Instruction::BrIf(1)); // Break loop

                    // Load key at offset: data + counter*DICT_ENTRY
                    func.instruction(&Instruction::LocalGet(ctx.temp_local)); // dict_ptr
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // counter
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    // Load the slot's key and compare with search_key at the key's
                    // natural width (f64 for float keys, i32 word otherwise).
                    if is_float_key {
                        func.instruction(&Instruction::F64Load(slot_arg()));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
                        func.instruction(&Instruction::F64Eq);
                    } else if matches!(key_type.as_ref(), IRType::String | IRType::Bytes)
                        || matches!(index_type, IRType::String | IRType::Bytes)
                    {
                        // A string key is stored as its offset, so comparing the
                        // words compares identity: a key built at runtime never
                        // matched the equal key already in the dict.
                        let slot_off = ctx.temp_local + 14;
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(slot_off));
                        emit_str_content_eq(func, ctx, slot_off, ctx.temp_local + 1);
                    } else {
                        // Anything else goes through the shared comparison, which
                        // compares a tuple key by value (it compared pointers, so a
                        // tuple key never matched an equal one) and refuses a key
                        // that cannot be hashed.
                        let key_ty = if matches!(key_type.as_ref(), IRType::Unknown) {
                            index_type.clone()
                        } else {
                            key_type.as_ref().clone()
                        };
                        match crate::compiler::equality::hash_unsupported(ctx, &key_ty) {
                            Some(why) => {
                                ctx.report(format!("{why} cannot be a dict key"));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                            }
                            None => emit_slot_eq_needle(func, ctx, &key_ty, ctx.temp_local + 1),
                        }
                    }

                    // If equal, capture the value and break out of the loop.
                    // Value slot = data + counter*DICT_ENTRY + SLOT.
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Add);
                    if is_float_value {
                        func.instruction(&Instruction::F64Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                    } else {
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                        // result
                    }
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::LocalSet(found));
                    func.instruction(&Instruction::Br(2)); // Break out of the loop
                    func.instruction(&Instruction::End);

                    // Increment counter
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                    func.instruction(&Instruction::Br(0)); // Continue loop
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::End);

                    func.instruction(&Instruction::LocalGet(found));
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    let shown = if matches!(key_type.as_ref(), IRType::Unknown) {
                        index_type.clone()
                    } else {
                        key_type.as_ref().clone()
                    };
                    let key = if is_float_key {
                        crate::compiler::exceptions::ExcArg::Value(
                            ctx.temp_local_f64_2,
                            IRType::Float,
                        )
                    } else {
                        crate::compiler::exceptions::ExcArg::Value(ctx.temp_local + 1, shown)
                    };
                    emit_raise(func, ctx, "KeyError", key, 1);
                    func.instruction(&Instruction::End);

                    // Push the looked-up value.
                    match value_type.as_ref() {
                        // Float values keep full f64 precision in their slot.
                        IRType::Float => {
                            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
                        }
                        // For string/bytes values the word is the blob offset;
                        // rebuild the (offset, length) pair from the length prefix,
                        // matching list/tuple read-back in `load_collection_word`.
                        // See #91.
                        IRType::String | IRType::Bytes => {
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
                            func.instruction(&Instruction::I32Sub);
                            func.instruction(&Instruction::I32Load(slot_arg()));
                        }
                        _ => {
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        }
                    }

                    value_type.as_ref().clone()
                }
                IRType::Tuple(element_types) => {
                    // Tuple indexing: tuple is stored as [length:i32][elem0][elem1]...
                    // Stack: (tuple_ptr, index). Address = tuple_ptr + HEADER + index*SLOT.
                    emit_sequence_address(func, ctx, "tuple index out of range");

                    // A tuple carries one type per position, so the index
                    // decides which. This used to answer the *first* member's
                    // type whatever was indexed, so `pairs[0][1]` on a
                    // `(int, str)` came back typed `int`: concatenating two of
                    // them compiled as integer addition and the result was read
                    // back as a string offset, which is a silent wrong answer.
                    let elem_type = match index.as_ref() {
                        IRExpr::Const(IRConstant::Int(i)) => {
                            let n = element_types.len() as i64;
                            let norm = if (*i as i64) < 0 {
                                *i as i64 + n
                            } else {
                                *i as i64
                            };
                            usize::try_from(norm)
                                .ok()
                                .and_then(|k| element_types.get(k))
                                .cloned()
                                .unwrap_or(IRType::Unknown)
                        }
                        // A computed index can land on any member, so the type
                        // is only knowable when they all agree.
                        _ => {
                            let uniform = element_types.windows(2).all(|pair| pair[0] == pair[1]);
                            if uniform {
                                element_types.first().cloned().unwrap_or(IRType::Unknown)
                            } else {
                                ctx.report(
                                    "a tuple whose members have different types can only be \
                                     indexed with a literal position, since the type of the \
                                     result decides how it is read. Hint: index with a \
                                     constant, or use a list",
                                );
                                IRType::Unknown
                            }
                        }
                    };

                    load_collection_word(func, &elem_type, ctx.temp_local + 1);

                    elem_type
                }
                other => {
                    // Anything else would read memory at whatever its word
                    // is: `kv[1]` on an untyped parameter holding 5 answered
                    // 0, where CPython raises TypeError.
                    func.instruction(&Instruction::Drop); // the index
                    func.instruction(&Instruction::Drop); // the container
                    ctx.report(not_subscriptable(&other, "indexed"));
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            let container_type = emit_expr(container, func, ctx, memory_layout, None);

            let unit_step = match step.as_deref() {
                None => true,
                Some(IRExpr::Const(IRConstant::Int(1))) => true,
                Some(_) => false,
            };
            if !unit_step && matches!(container_type, IRType::String | IRType::Bytes) {
                // `s[::-1]` and every other step: the step was ignored, so
                // reversing a string answered the string unchanged.
                emit_stepped_text_slice(
                    func,
                    ctx,
                    memory_layout,
                    start.as_deref(),
                    end.as_deref(),
                    step.as_deref().expect("a non-unit step is present"),
                    container_type == IRType::String,
                );
                return container_type;
            }

            // A tuple whose members share a type slices as a tuple of any
            // length: the layout is the same, and so is the answer.
            let container_type = match container_type {
                IRType::Tuple(members)
                    if !members.is_empty() && members.windows(2).all(|w| w[0] == w[1]) =>
                {
                    IRType::VarTuple(Box::new(members[0].clone()))
                }
                IRType::Tuple(members) if members.is_empty() => {
                    IRType::VarTuple(Box::new(IRType::Unknown))
                }
                other => other,
            };
            let slices_a_tuple = matches!(container_type, IRType::VarTuple(_));
            match container_type {
                IRType::String | IRType::Bytes => {
                    // String/Bytes slicing: str[start:end] / bytes[start:end].
                    // Entry stack: (offset, length). Result: (new_offset,
                    // new_length) into the same backing memory.
                    //
                    // The clamping is fully branchless (i32 `select`), with all
                    // operands held in locals. An earlier version used
                    // `If(BlockType::Empty)` blocks that consumed values pushed
                    // before the block (a net-nonzero stack effect), which failed
                    // WASM validation.
                    let off = ctx.temp_local + 5;
                    let len = ctx.temp_local + 6;
                    let lo = ctx.temp_local + 2;
                    let hi = ctx.temp_local + 3;
                    let scratch = ctx.temp_local + 4;

                    // Stash (offset, length) into high locals first so any nested
                    // start/end expression (which may use the low scratch locals)
                    // cannot clobber them.
                    func.instruction(&Instruction::LocalSet(len));
                    func.instruction(&Instruction::LocalSet(off));
                    // A str's bounds count code points; they become byte
                    // offsets once clamped.
                    let is_str = matches!(container_type, IRType::String);
                    if is_str {
                        func.instruction(&Instruction::LocalGet(off));
                        func.instruction(&Instruction::Call(
                            ctx.helper(crate::compiler::runtime::Helper::CharLen),
                        ));
                        func.instruction(&Instruction::LocalSet(len));
                    }

                    // start, defaulting to 0
                    if let Some(s) = start {
                        emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
                    } else {
                        func.instruction(&Instruction::I32Const(0));
                    }
                    func.instruction(&Instruction::LocalSet(lo));

                    // end, defaulting to length
                    if let Some(e) = end {
                        emit_expr(e, func, ctx, memory_layout, Some(&IRType::Int));
                    } else {
                        func.instruction(&Instruction::LocalGet(len));
                    }
                    func.instruction(&Instruction::LocalSet(hi));

                    // Normalize negatives and clamp each bound to [0, length].
                    emit_normalize_and_clamp(func, lo, len);
                    emit_normalize_and_clamp(func, hi, len);

                    // new_length = max(hi - lo, 0)
                    func.instruction(&Instruction::LocalGet(hi));
                    func.instruction(&Instruction::LocalGet(lo));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::LocalTee(scratch));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalGet(scratch));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32GtS);
                    func.instruction(&Instruction::Select);
                    func.instruction(&Instruction::LocalSet(scratch));
                    if is_str {
                        let char_offset = ctx.helper(crate::compiler::runtime::Helper::CharOffset);
                        func.instruction(&Instruction::LocalGet(scratch));
                        func.instruction(&Instruction::If(BlockType::Empty));
                        for bound in [lo, hi] {
                            func.instruction(&Instruction::LocalGet(off));
                            func.instruction(&Instruction::LocalGet(bound));
                            func.instruction(&Instruction::Call(char_offset));
                            func.instruction(&Instruction::LocalSet(bound));
                        }
                        func.instruction(&Instruction::LocalGet(hi));
                        func.instruction(&Instruction::LocalGet(lo));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::LocalSet(scratch));
                        func.instruction(&Instruction::End);
                    }

                    // Step is not yet honoured (default step=1); evaluate and
                    // discard it so a provided step doesn't unbalance the stack.
                    if let Some(s) = step {
                        emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
                        func.instruction(&Instruction::Drop);
                    }

                    // A slice's bytes live inside the source blob, so its offset
                    // points partway into that blob rather than past a length
                    // prefix. Allocate a fresh `[len:i32][bytes][nul?]` blob and
                    // copy the slice into it (mirroring concatenation) so the
                    // value carries a recoverable length prefix — the layout the
                    // rest of the compiler assumes for collection read-back
                    // (`load(offset - 4)`). See #92. `scratch` holds new_length.
                    let is_string = matches!(container_type, IRType::String);
                    let prefix = STRING_LEN_PREFIX as i32;
                    let src = ctx.temp_local; // slice source offset
                    let blk = ctx.temp_local + 1; // allocated block / data ptr

                    // src = offset + lo
                    func.instruction(&Instruction::LocalGet(off));
                    func.instruction(&Instruction::LocalGet(lo));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(src));

                    // block = __alloc(prefix + new_length [+ 1 for NUL])
                    func.instruction(&Instruction::I32Const(prefix));
                    func.instruction(&Instruction::LocalGet(scratch));
                    func.instruction(&Instruction::I32Add);
                    if is_string {
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                    }
                    func.instruction(&Instruction::Call(ctx.alloc_func_index));
                    func.instruction(&Instruction::LocalSet(blk));

                    // Write the length prefix at the block start.
                    func.instruction(&Instruction::LocalGet(blk));
                    func.instruction(&Instruction::LocalGet(scratch));
                    func.instruction(&Instruction::I32Store(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));

                    // data_ptr = block + prefix (the value's offset)
                    func.instruction(&Instruction::LocalGet(blk));
                    func.instruction(&Instruction::I32Const(prefix));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(blk)); // data_ptr

                    // memory.copy(data_ptr, src, new_length)
                    func.instruction(&Instruction::LocalGet(blk));
                    func.instruction(&Instruction::LocalGet(src));
                    func.instruction(&Instruction::LocalGet(scratch));
                    func.instruction(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });

                    // Strings are NUL-terminated; write it past the data.
                    if is_string {
                        func.instruction(&Instruction::LocalGet(blk));
                        func.instruction(&Instruction::LocalGet(scratch));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32Store8(MemArg {
                            offset: 0,
                            align: 0,
                            memory_index: 0,
                        }));
                    }

                    // Result: (data_ptr, new_length)
                    func.instruction(&Instruction::LocalGet(blk));
                    func.instruction(&Instruction::LocalGet(scratch));

                    container_type.clone()
                }
                IRType::List(elem_type) | IRType::VarTuple(elem_type) => {
                    // List slicing: `xs[a:b]`, returning a new list (a tuple of
                    // any length slices to another one).
                    //
                    // This used to compute a length, drop it, and push a null
                    // pointer, and its clamp pushed a value from both arms of
                    // an `if` typed as returning nothing, so the module did not
                    // validate. Indices follow Python: a negative one counts
                    // from the end, and both ends clamp into range rather than
                    // raising, so `xs[:99]` is the whole list.
                    //
                    // The bounds are evaluated while the list pointer is still
                    // on the stack, because emitting them runs arbitrary
                    // expression codegen that claims the same scratch locals.
                    if let Some(st) = step {
                        // A step other than 1 would need a strided copy (and a
                        // negative one reverses); rejecting it is better than
                        // the silent `[::2] == [:]` the old code would give.
                        let literal_one = matches!(st.as_ref(), IRExpr::Const(IRConstant::Int(1)));
                        if !literal_one {
                            ctx.report(
                                "a list slice step other than 1 is not supported yet. \
                                 Hint: slice with [start:end] and step in a loop",
                            );
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Unreachable);
                            func.instruction(&Instruction::I32Const(0));
                            return IRType::List(elem_type.clone());
                        }
                    }

                    let ptr = ctx.temp_local;
                    let len = ctx.temp_local + 1;
                    let lo = ctx.temp_local + 2;
                    let hi = ctx.temp_local + 3;
                    let count = ctx.temp_local + 4;
                    let out = ctx.temp_local + 5;

                    match start {
                        Some(e) => {
                            emit_expr(e, func, ctx, memory_layout, Some(&IRType::Int));
                        }
                        None => {
                            func.instruction(&Instruction::I32Const(0));
                        }
                    };
                    // A missing end is the length, which is not known until the
                    // pointer is in a local; mark it with i32::MIN, a value no
                    // real index can take after normalization.
                    match end {
                        Some(e) => {
                            emit_expr(e, func, ctx, memory_layout, Some(&IRType::Int));
                        }
                        None => {
                            func.instruction(&Instruction::I32Const(i32::MIN));
                        }
                    };
                    func.instruction(&Instruction::LocalSet(hi));
                    func.instruction(&Instruction::LocalSet(lo));
                    func.instruction(&Instruction::LocalSet(ptr));
                    func.instruction(&Instruction::LocalGet(ptr));
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::LocalSet(len));

                    if end.is_none() {
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::LocalSet(hi));
                    }

                    // Normalize and clamp each bound: negative counts from the
                    // end, then everything is pinned into 0..=len.
                    emit_normalize_and_clamp(func, lo, len);
                    emit_normalize_and_clamp(func, hi, len);

                    // count = max(0, hi - lo)
                    func.instruction(&Instruction::LocalGet(hi));
                    func.instruction(&Instruction::LocalGet(lo));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::LocalSet(count));
                    func.instruction(&Instruction::LocalGet(count));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalSet(count));
                    func.instruction(&Instruction::End);

                    // The result is a fresh region, so growing or mutating it
                    // never touches the list it was sliced from.
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::LocalGet(count));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::Call(ctx.alloc_func_index));
                    func.instruction(&Instruction::LocalSet(out));
                    store_runtime_data_ptr(func, out);
                    func.instruction(&Instruction::LocalGet(out));
                    func.instruction(&Instruction::LocalGet(count));
                    func.instruction(&Instruction::I32Store(slot_arg()));
                    func.instruction(&Instruction::LocalGet(out));
                    func.instruction(&Instruction::LocalGet(count));
                    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));

                    // Slots are opaque here: copying them verbatim is right for
                    // every element type, an f64's two words and a string's
                    // offset included.
                    func.instruction(&Instruction::LocalGet(out));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(ptr));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(lo));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalGet(count));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });

                    func.instruction(&Instruction::LocalGet(out));
                    if slices_a_tuple {
                        IRType::VarTuple(elem_type.clone())
                    } else {
                        IRType::List(elem_type.clone())
                    }
                }
                // This answered the container itself.
                other => {
                    func.instruction(&Instruction::Drop);
                    ctx.report(not_subscriptable(&other, "sliced"));
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::Attribute { object, attribute } => {
            // A constant read through an imported user module's namespace
            // (#41): `mod.CONST` (or `m.CONST` via an alias). Module-level
            // variables from every merged file share one namespace, so inline
            // the merged variable's initializer, exactly like a plain
            // module-variable read. A local of the same name shadows the
            // module binding.
            if let IRExpr::Variable(name) = &**object {
                if ctx.get_local_info(name).is_none() && ctx.user_modules.contains_key(name) {
                    if ctx.get_module_var(attribute).is_some() {
                        return emit_expr(
                            &IRExpr::Variable(attribute.clone()),
                            func,
                            ctx,
                            memory_layout,
                            expected_type,
                        );
                    }
                    // A function or class read as a value, or a name the
                    // module does not define. This answered 0.
                    ctx.report(format!(
                        "cannot read '{name}.{attribute}' as a value: only a user module's \
                         variables can be read through it, and its functions and classes called"
                    ));
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                }
            }

            // Resolve a stdlib attribute, whether on a module (`os.sep`) or a
            // submodule (`os.path.sep`, where `object` is itself `os.path`).
            let stdlib_value = match &**object {
                IRExpr::Variable(module_name) if crate::stdlib::is_stdlib_module(module_name) => {
                    crate::stdlib::get_stdlib_attributes(module_name, attribute)
                }
                IRExpr::Attribute {
                    object: inner,
                    attribute: sub,
                } => match &**inner {
                    IRExpr::Variable(parent) if crate::stdlib::is_stdlib_submodule(parent, sub) => {
                        crate::stdlib::get_submodule_attribute(parent, sub, attribute)
                    }
                    _ => None,
                },
                _ => None,
            };

            if stdlib_value.is_none() {
                if let (IRExpr::Variable(module), "argv") = (object.as_ref(), attribute.as_str()) {
                    if module == "sys" && ctx.get_local_info(module).is_none() {
                        func.instruction(&Instruction::Call(
                            ctx.helper(crate::compiler::runtime::Helper::Argv),
                        ));
                        return IRType::List(Box::new(IRType::String));
                    }
                }
                if crate::compiler::host::std_stream(
                    ctx,
                    &IRExpr::Attribute {
                        object: object.clone(),
                        attribute: attribute.clone(),
                    },
                )
                .is_some()
                {
                    ctx.report(format!(
                        "sys.{attribute} as a value is not supported. Hint: call its methods \
                         directly, as in sys.{attribute}.write(...)"
                    ));
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                }
                if let Some(qualified) = stdlib_call_name(ctx, object) {
                    ctx.report(format!(
                        "'{qualified}.{attribute}' is not available yet: only the standard \
                         library's constants compile today, and the rest is planned for 0.20.0"
                    ));
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                }
            }
            if let Some(value) = stdlib_value {
                return match value {
                    crate::stdlib::StdlibValue::Int(i) => {
                        func.instruction(&Instruction::I32Const(i));
                        IRType::Int
                    }
                    crate::stdlib::StdlibValue::String(s) => {
                        let offset = memory_layout.string_offsets.get(&s).copied().unwrap_or(0);
                        func.instruction(&Instruction::I32Const(offset as i32));
                        func.instruction(&Instruction::I32Const(s.len() as i32));
                        IRType::String
                    }
                    crate::stdlib::StdlibValue::Float(f) => {
                        func.instruction(&Instruction::F64Const(f.into()));
                        IRType::Float
                    }
                    crate::stdlib::StdlibValue::Module(module_name) => {
                        // Module doesn't need to push anything to the stack
                        IRType::Module(module_name)
                    }
                };
            }

            // `ClassName.var` (or `cls.var` inside a classmethod) reads a
            // class-level variable: from its global when it is evaluated once
            // (anything but a plain constant), otherwise inlined.
            if let IRExpr::Variable(name) = &**object {
                if let Some(class_name) = static_class_target(ctx, name) {
                    let key = class_var_global(ctx, &class_name, attribute)
                        .unwrap_or_else(|| format!("{class_name}.{attribute}"));
                    if ctx.module_global_index.contains_key(&key) {
                        return emit_expr(
                            &IRExpr::Variable(key),
                            func,
                            ctx,
                            memory_layout,
                            expected_type,
                        );
                    }
                    if let Some(class_info) = ctx.get_class_info(&class_name) {
                        if let Some(value) = class_info.class_var_values.get(attribute) {
                            let value = value.clone();
                            return emit_expr(&value, func, ctx, memory_layout, expected_type);
                        }
                    }
                }
            }

            let obj_type = emit_expr(object, func, ctx, memory_layout, None);

            match &obj_type {
                // `x.__class__` is `type(x)`: the class the instance's tag
                // names.
                IRType::Class(class_name) if attribute == "__class__" => {
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    IRType::ClassRef(class_name.clone())
                }
                IRType::ClassRef(class_name) => {
                    emit_class_object_attribute(func, ctx, memory_layout, class_name, attribute)
                }
                IRType::Class(class_name) => {
                    // A `@property` read compiles to its getter: `obj.attr`
                    // becomes `Class::attr(self)`, with the instance pointer
                    // already on the stack as the only argument.
                    let class_info = ctx.get_class_info(class_name);
                    let getter =
                        class_info.and_then(|ci| match ci.method_kinds.get(attribute.as_str()) {
                            Some(MethodKind::PropertyGetter) => {
                                ci.methods.get(attribute.as_str()).copied().map(|idx| {
                                    let owner = ci
                                        .method_owner
                                        .get(attribute.as_str())
                                        .cloned()
                                        .unwrap_or_else(|| class_name.clone());
                                    (idx, owner)
                                })
                            }
                            _ => None,
                        });
                    if let Some((getter_idx, owner)) = getter {
                        let ret = ctx
                            .get_function_info(&format!("{owner}::{attribute}"))
                            .map(|f| f.return_type.clone())
                            .unwrap_or(IRType::Unknown);
                        // A getter a subclass overrides is reached through the
                        // vtable, so `self.p` inside a base method reads the
                        // subclass's property on a subclass instance.
                        if let Some((column, type_index)) = ctx.virtual_call(class_name, attribute)
                        {
                            return emit_virtual_call(
                                func,
                                ctx,
                                memory_layout,
                                attribute,
                                &[],
                                &[],
                                ret,
                                column,
                                type_index,
                            );
                        }
                        emit_user_call(func, ctx, getter_idx);
                        // A call result is a single word; rebuild the
                        // string/bytes pair.
                        if matches!(ret, IRType::String | IRType::Bytes) {
                            recover_str_pair(func, ctx);
                        }
                        return ret;
                    }

                    // Instance field read: load with the field's width and report
                    // its type so float fields participate in f64 arithmetic.
                    // A string/bytes field slot holds only the offset word, so
                    // rebuild the (offset, length) pair from the blob prefix.
                    if let Some((field_offset, field_ty)) = lookup_field(ctx, class_name, attribute)
                    {
                        func.instruction(&load_field_instr(&field_ty, field_offset));
                        if matches!(field_ty, IRType::String | IRType::Bytes) {
                            recover_str_pair(func, ctx);
                        }
                        field_ty
                    } else if ctx
                        .get_class_info(class_name)
                        .is_some_and(|ci| ci.class_var_owner.contains_key(attribute.as_str()))
                    {
                        emit_class_var_through_instance(
                            func,
                            ctx,
                            memory_layout,
                            class_name,
                            attribute,
                        )
                    } else if attribute == "args"
                        && crate::compiler::exceptions::is_exception_class(ctx, class_name)
                    {
                        crate::compiler::exceptions::emit_args(func, ctx)
                    } else {
                        // CPython raises AttributeError; answering 0 here
                        // reported success with a wrong value.
                        ctx.report(format!("'{class_name}' has no attribute '{attribute}'"));
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                        IRType::Unknown
                    }
                }
                other => {
                    // An attribute read through a value whose type is not
                    // known, most often an unannotated parameter, used to
                    // answer 0 and report success, so `other.v` inside
                    // `def __lt__(self, other)` compared against nothing
                    // (#120). The rich comparison methods type `other` as
                    // the class when it is unannotated; anything else must
                    // say what it is.
                    let hint = if matches!(other, IRType::Unknown | IRType::Any) {
                        ". Hint: annotate the parameter or variable it is read from"
                    } else {
                        ""
                    };
                    ctx.report(format!(
                        "cannot read attribute '{attribute}' of a value of type '{}'{hint}",
                        crate::type_to_string(other)
                    ));
                    func.instruction(&Instruction::Drop);
                    if matches!(other, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::Comprehension {
            kind,
            element,
            value,
            generators,
        } => emit_comprehension(
            *kind,
            element,
            value.as_deref(),
            generators,
            func,
            ctx,
            memory_layout,
        ),
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            // `dict.fromkeys(...)`, a method of the type rather than a value.
            if let IRExpr::Variable(name) = object.as_ref() {
                if name == "dict"
                    && method_name == "fromkeys"
                    && ctx.get_local_info(name).is_none()
                    && ctx.get_module_var(name).is_none()
                {
                    return emit_dict_fromkeys(func, ctx, memory_layout, arguments);
                }
            }
            // `super().method(...)` / `super().__init__(...)`: static dispatch
            // to the immediate base class of the class whose method is being
            // compiled. `super()` itself is never evaluated — `self` is always
            // local 0 in a method — so the sequence stays stack-only. Because
            // each ClassInfo's method table already contains its base's fully
            // resolved methods, this composes across deeper hierarchies.
            if let IRExpr::FunctionCall {
                function_name,
                arguments: super_args,
            } = &**object
            {
                if function_name == "super" && super_args.is_empty() {
                    // `super().__init__(...)` reaching BaseException's: the
                    // exception's arguments become these.
                    if method_name == "__init__" {
                        if let Some(current) = ctx.current_class.clone() {
                            let reaches_base_exception = super_targets(ctx, &current, "__init__")
                                .iter()
                                .all(|(_, t)| t.is_none());
                            if crate::compiler::exceptions::is_exception_class(ctx, &current)
                                && reaches_base_exception
                            {
                                emit_exception_args(
                                    &current,
                                    arguments,
                                    0,
                                    func,
                                    ctx,
                                    memory_layout,
                                );
                                return IRType::None;
                            }
                        }
                    }
                    // `super()` is the next class after this one in the MRO of
                    // the instance's own class, which with several bases
                    // depends on what the instance is: the targets, by class.
                    let targets = ctx
                        .current_class
                        .clone()
                        .map(|c| super_targets(ctx, &c, method_name))
                        .unwrap_or_default();
                    let distinct: Vec<(u32, String)> = {
                        let mut seen: Vec<(u32, String)> = Vec::new();
                        for (_, t) in &targets {
                            if let Some(t) = t {
                                if !seen.contains(t) {
                                    seen.push(t.clone());
                                }
                            }
                        }
                        seen
                    };
                    if let Some((_, first_owner)) = distinct.first() {
                        let (param_types, ret) = ctx
                            .get_function_info(&format!("{first_owner}::{method_name}"))
                            .map(|f| (f.param_types.clone(), f.return_type.clone()))
                            .unwrap_or((Vec::new(), IRType::Unknown));
                        let every_class_has_one = targets.iter().all(|(_, t)| t.is_some());
                        if distinct.len() == 1 && every_class_has_one {
                            func.instruction(&Instruction::LocalGet(0)); // self
                            for (i, arg) in arguments.iter().enumerate() {
                                let t = emit_expr(
                                    arg,
                                    func,
                                    ctx,
                                    memory_layout,
                                    param_types.get(i + 1),
                                );
                                check_class_arg(ctx, &t, param_types.get(i + 1), i, method_name);
                                // Narrow a string/bytes argument to its offset
                                // word, matching the calling convention used at
                                // instantiation sites.
                                if matches!(t, IRType::String | IRType::Bytes) {
                                    func.instruction(&Instruction::Drop);
                                }
                            }
                            emit_user_call(func, ctx, distinct[0].0);
                            // A call result is a single word; rebuild the
                            // string/bytes pair.
                            finish_user_call(func, ctx, &ret);
                            return ret;
                        }
                        return emit_cooperative_super_call(
                            method_name,
                            arguments,
                            &targets,
                            &param_types,
                            &ret,
                            func,
                            ctx,
                            memory_layout,
                        );
                    }
                    // No base class, or none that defines the method: an
                    // AttributeError in Python. This answered 0.
                    ctx.report(format!(
                        "'super' object has no attribute '{method_name}': no base class of \
                         this class defines it"
                    ));
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
                }
            }

            // A call through an imported user module's namespace (#41):
            // `mod.f(...)`, or `m.f(...)` with `import mod as m`. The
            // module's functions and classes are statically linked into this
            // single WASM module by the multi-file merge, so compile it as a
            // plain call (or class instantiation) of the merged name. A local
            // variable of the same name shadows the module binding.
            if let IRExpr::Variable(name) = &**object {
                if ctx.get_local_info(name).is_none() && ctx.user_modules.contains_key(name) {
                    return emit_expr(
                        &IRExpr::FunctionCall {
                            function_name: method_name.clone(),
                            arguments: arguments.clone(),
                        },
                        func,
                        ctx,
                        memory_layout,
                        expected_type,
                    );
                }
            }

            // A call into the standard library: `json.dumps(x)`,
            // `os.path.join(a, b)`, `datetime.datetime.now()`. Its calls are
            // not implemented yet. They used to compile to placeholders:
            // `json.dumps` answered "{}", `os.getcwd()` "/", `datetime.now()`
            // the moment the module was compiled, and a runtime `re.match` a
            // match whatever the input. Constant `re.sub` and `re.escape`
            // calls are folded during lowering and never reach this.
            // `sys.exit([status])` raises SystemExit.
            if let IRExpr::Variable(module) = object.as_ref() {
                if module == "sys"
                    && method_name == "exit"
                    && ctx.get_local_info(module).is_none()
                    && crate::compiler::exceptions::is_exception_class(ctx, "SystemExit")
                {
                    if arguments.len() > 1 {
                        ctx.report(format!(
                            "sys.exit() raises TypeError: exit expected at most 1 argument, got {}",
                            arguments.len()
                        ));
                    }
                    emit_exception_construct("SystemExit", arguments, func, ctx, memory_layout);
                    let obj = ctx.temp_local + 2;
                    func.instruction(&Instruction::LocalSet(obj));
                    crate::compiler::exceptions::emit_raise_object(func, ctx, obj, 0);
                    return IRType::None;
                }
            }
            if let Some(fd) = crate::compiler::host::std_stream(ctx, object) {
                return emit_std_stream_method(
                    fd,
                    method_name,
                    arguments,
                    func,
                    ctx,
                    memory_layout,
                );
            }
            if let Some(qualified) = stdlib_call_name(ctx, object) {
                let qualified = format!("{qualified}.{method_name}");
                if qualified == "re.purge" && arguments.is_empty() {
                    return IRType::None;
                }
                ctx.report(format!(
                    "'{qualified}()' is not implemented yet; the standard library's functions \
                     are planned for 0.20.0, and only its constants (and re.sub / re.escape \
                     over constant arguments) compile today"
                ));
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }

            // Class-level method call: `ClassName.method(...)` or, inside a
            // classmethod, `cls.method(...)`. There is no instance to emit;
            // dispatch is resolved statically by the method's kind.
            if let IRExpr::Variable(name) = &**object {
                if let Some(class_name) = static_class_target(ctx, name) {
                    return emit_class_level_method_call(
                        func,
                        ctx,
                        memory_layout,
                        &class_name,
                        method_name,
                        arguments,
                    );
                }
            }

            let object_type = emit_expr(object, func, ctx, memory_layout, None);

            let deletes = matches!(
                method_name.as_str(),
                crate::ir::DEL_ITEM_METHOD | crate::ir::DEL_SLICE_METHOD
            );
            if deletes && !matches!(object_type, IRType::List(_) | IRType::Dict(..)) {
                let kind = match &object_type {
                    IRType::Tuple(_) => "tuple".to_string(),
                    other => crate::type_to_string(other),
                };
                ctx.report(format!("'{kind}' object doesn't support item deletion"));
                if matches!(object_type, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
                func.instruction(&Instruction::Drop);
                return IRType::None;
            }
            if method_name == crate::ir::DEL_SLICE_METHOD && matches!(object_type, IRType::Dict(..))
            {
                ctx.report("a dict cannot be sliced, so 'del d[a:b]' is a TypeError");
                func.instruction(&Instruction::Drop);
                return IRType::None;
            }

            match &object_type {
                IRType::String => {
                    // String methods: upper(), lower(), split(sep), etc.
                    match method_name.as_str() {
                        "upper" => {
                            // upper(): ASCII-uppercase every character.
                            // The receiver's (offset, length) is on the stack;
                            // this builds a fresh block and leaves the
                            // transformed (offset, length) in its place.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_case(func, ctx, CaseMode::Upper);
                            IRType::String
                        }

                        "lower" => {
                            // lower(): ASCII-lowercase every character.
                            // The receiver's (offset, length) is on the stack;
                            // this builds a fresh block and leaves the
                            // transformed (offset, length) in its place.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_case(func, ctx, CaseMode::Lower);
                            IRType::String
                        }

                        "strip" => {
                            // strip([chars]): trim both ends.
                            // With no argument ASCII whitespace is trimmed; with
                            // one, any character in the given cut set is. The
                            // argument is emitted first so its own codegen is
                            // finished before the scratch locals are claimed.
                            let has_chars = match arguments.len() {
                                0 => false,
                                1 => {
                                    let arg_type =
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                    if !matches!(arg_type, IRType::String) {
                                        ctx.report(format!(
                                            "str.{}() takes a str, got {}",
                                            method_name,
                                            crate::type_to_string(&arg_type)
                                        ));
                                        func.instruction(&Instruction::Unreachable);
                                        func.instruction(&Instruction::I32Const(0));
                                        func.instruction(&Instruction::I32Const(0));
                                        return IRType::String;
                                    }
                                    true
                                }
                                _ => {
                                    ctx.report(format!(
                                        "str.{method_name}() takes at most one argument"
                                    ));
                                    func.instruction(&Instruction::Unreachable);
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    return IRType::String;
                                }
                            };
                            emit_string_trim(func, ctx, TrimMode::Both, has_chars);
                            IRType::String
                        }

                        "lstrip" => {
                            // lstrip([chars]): trim the left end.
                            // With no argument ASCII whitespace is trimmed; with
                            // one, any character in the given cut set is. The
                            // argument is emitted first so its own codegen is
                            // finished before the scratch locals are claimed.
                            let has_chars = match arguments.len() {
                                0 => false,
                                1 => {
                                    let arg_type =
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                    if !matches!(arg_type, IRType::String) {
                                        ctx.report(format!(
                                            "str.{}() takes a str, got {}",
                                            method_name,
                                            crate::type_to_string(&arg_type)
                                        ));
                                        func.instruction(&Instruction::Unreachable);
                                        func.instruction(&Instruction::I32Const(0));
                                        func.instruction(&Instruction::I32Const(0));
                                        return IRType::String;
                                    }
                                    true
                                }
                                _ => {
                                    ctx.report(format!(
                                        "str.{method_name}() takes at most one argument"
                                    ));
                                    func.instruction(&Instruction::Unreachable);
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    return IRType::String;
                                }
                            };
                            emit_string_trim(func, ctx, TrimMode::Left, has_chars);
                            IRType::String
                        }

                        "rstrip" => {
                            // rstrip([chars]): trim the right end.
                            // With no argument ASCII whitespace is trimmed; with
                            // one, any character in the given cut set is. The
                            // argument is emitted first so its own codegen is
                            // finished before the scratch locals are claimed.
                            let has_chars = match arguments.len() {
                                0 => false,
                                1 => {
                                    let arg_type =
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                    if !matches!(arg_type, IRType::String) {
                                        ctx.report(format!(
                                            "str.{}() takes a str, got {}",
                                            method_name,
                                            crate::type_to_string(&arg_type)
                                        ));
                                        func.instruction(&Instruction::Unreachable);
                                        func.instruction(&Instruction::I32Const(0));
                                        func.instruction(&Instruction::I32Const(0));
                                        return IRType::String;
                                    }
                                    true
                                }
                                _ => {
                                    ctx.report(format!(
                                        "str.{method_name}() takes at most one argument"
                                    ));
                                    func.instruction(&Instruction::Unreachable);
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    return IRType::String;
                                }
                            };
                            emit_string_trim(func, ctx, TrimMode::Right, has_chars);
                            IRType::String
                        }

                        "capitalize" => {
                            // capitalize(): first character upper, the rest lower.
                            // The receiver's (offset, length) is on the stack;
                            // this builds a fresh block and leaves the
                            // transformed (offset, length) in its place.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_case(func, ctx, CaseMode::Capitalize);
                            IRType::String
                        }

                        "title" => {
                            // title(): uppercase each letter that starts a word.
                            // The receiver's (offset, length) is on the stack;
                            // this builds a fresh block and leaves the
                            // transformed (offset, length) in its place.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_case(func, ctx, CaseMode::Title);
                            IRType::String
                        }

                        "split" => {
                            // split() splits on runs of whitespace and drops
                            // leading/trailing empties; split(sep) splits on
                            // each occurrence and keeps empty fields.
                            let has_sep = match arguments.len() {
                                0 => false,
                                1 => {
                                    let t =
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                    if !matches!(t, IRType::String) {
                                        ctx.report(format!(
                                            "str.split() takes a str, got {}",
                                            crate::type_to_string(&t)
                                        ));
                                        func.instruction(&Instruction::Unreachable);
                                        func.instruction(&Instruction::I32Const(0));
                                        return IRType::List(Box::new(IRType::String));
                                    }
                                    true
                                }
                                _ => {
                                    ctx.report(
                                        "str.split() takes at most one argument. \
                                         Hint: maxsplit is not supported",
                                    );
                                    func.instruction(&Instruction::Unreachable);
                                    func.instruction(&Instruction::I32Const(0));
                                    return IRType::List(Box::new(IRType::String));
                                }
                            };
                            emit_string_split(func, ctx, has_sep);
                            IRType::List(Box::new(IRType::String))
                        }

                        "find" => {
                            // find(sub): first index of sub, or -1.
                            // The needle is emitted on top of the receiver, so
                            // the stack is (h_off, h_len, n_off, n_len).
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                            let arg_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(arg_type, IRType::String) {
                                ctx.report(format!(
                                    "str.{}() takes a str, got {}",
                                    method_name,
                                    crate::type_to_string(&arg_type)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                            emit_string_search(func, ctx, SearchMode::Find);
                            emit_byte_to_char_index(func, ctx);
                            IRType::Int
                        }

                        "index" => {
                            // index(sub): like find, but raises ValueError when absent.
                            // The needle is emitted on top of the receiver, so
                            // the stack is (h_off, h_len, n_off, n_len).
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                            let arg_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(arg_type, IRType::String) {
                                ctx.report(format!(
                                    "str.{}() takes a str, got {}",
                                    method_name,
                                    crate::type_to_string(&arg_type)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                            emit_string_search(func, ctx, SearchMode::Index);
                            emit_byte_to_char_index(func, ctx);
                            IRType::Int
                        }

                        "count" => {
                            // count(sub): non-overlapping occurrences of sub.
                            // The needle is emitted on top of the receiver, so
                            // the stack is (h_off, h_len, n_off, n_len).
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                            let arg_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(arg_type, IRType::String) {
                                ctx.report(format!(
                                    "str.{}() takes a str, got {}",
                                    method_name,
                                    crate::type_to_string(&arg_type)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Int;
                            }
                            emit_string_search(func, ctx, SearchMode::Count);
                            IRType::Int
                        }

                        "startswith" => {
                            // startswith(prefix): does the string begin with prefix?
                            // The needle is emitted on top of the receiver, so
                            // the stack is (h_off, h_len, n_off, n_len).
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            let arg_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(arg_type, IRType::String) {
                                ctx.report(format!(
                                    "str.{}() takes a str, got {}",
                                    method_name,
                                    crate::type_to_string(&arg_type)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_search(func, ctx, SearchMode::StartsWith);
                            IRType::Bool
                        }

                        "endswith" => {
                            // endswith(suffix): does the string end with suffix?
                            // The needle is emitted on top of the receiver, so
                            // the stack is (h_off, h_len, n_off, n_len).
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            let arg_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(arg_type, IRType::String) {
                                ctx.report(format!(
                                    "str.{}() takes a str, got {}",
                                    method_name,
                                    crate::type_to_string(&arg_type)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_search(func, ctx, SearchMode::EndsWith);
                            IRType::Bool
                        }

                        "replace" => {
                            // replace(old, new): every non-overlapping
                            // occurrence, like Python. An empty `old` traps.
                            if arguments.len() != 2 {
                                ctx.report("str.replace() takes exactly two arguments");
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            let a = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            let b = emit_expr(&arguments[1], func, ctx, memory_layout, None);
                            if !matches!(a, IRType::String) || !matches!(b, IRType::String) {
                                ctx.report("str.replace() takes two str arguments");
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_replace(func, ctx);
                            IRType::String
                        }

                        "isdigit" | "isalpha" | "isalnum" | "isspace" | "isupper" | "islower"
                        | "isdecimal" | "isnumeric" => {
                            // The predicates over every character, as CPython
                            // classifies it (`core::unicode`). Constant
                            // receivers are folded during lowering. These
                            // tested ASCII bytes, so `"é".isalpha()` answered
                            // False.
                            use crate::core::unicode::Predicate;
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            let predicate = match method_name.as_str() {
                                "isdigit" => Predicate::Digit,
                                "isalpha" => Predicate::Alpha,
                                "isalnum" => Predicate::Alnum,
                                "isspace" => Predicate::Space,
                                "isupper" => Predicate::Upper,
                                "islower" => Predicate::Lower,
                                "isdecimal" => Predicate::Decimal,
                                _ => Predicate::Numeric,
                            };
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Call(
                                ctx.helper(crate::compiler::runtime::Helper::StrIs(predicate)),
                            ));
                            IRType::Bool
                        }

                        "join" => {
                            // sep.join(parts): the parts are a list of strings,
                            // each slot holding the piece's offset.
                            if arguments.len() != 1 {
                                ctx.report("str.join() takes exactly one argument");
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            let t = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            let joinable = match &t {
                                IRType::List(inner) | IRType::VarTuple(inner) => {
                                    matches!(**inner, IRType::String | IRType::Unknown)
                                }
                                // A tuple carries one type per position, so it
                                // joins when every element is a string.
                                IRType::Tuple(items) => items
                                    .iter()
                                    .all(|e| matches!(e, IRType::String | IRType::Unknown)),
                                _ => false,
                            };
                            if !joinable {
                                ctx.report(format!(
                                    "str.join() takes a list of str, got {}",
                                    crate::type_to_string(&t)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_join(func, ctx);
                            IRType::String
                        }

                        "format" => {
                            // A literal template is lowered to a concatenation
                            // chain in `lower_format_template`, so reaching
                            // codegen means the receiver was a runtime string.
                            // There is no template to parse then, and the old
                            // stub answered the empty string.
                            ctx.report(
                                "str.format() needs a literal format string. \
                                 Hint: write the template inline, or build the \
                                 string with '+'",
                            );
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                            }
                            func.instruction(&Instruction::Unreachable);
                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::I32Const(0));
                            IRType::String
                        }

                        "ljust" | "rjust" | "center" => {
                            // ljust / rjust / center(width[, fillchar]): pad to
                            // width, never truncating.
                            let refuse = |func: &mut Function, message: String| {
                                ctx.report(message);
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                IRType::String
                            };
                            if arguments.is_empty() || arguments.len() > 2 {
                                return refuse(
                                    func,
                                    format!("str.{method_name}() takes a width and an optional fill character"),
                                );
                            }
                            let t = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(t, IRType::Int | IRType::Bool) {
                                return refuse(
                                    func,
                                    format!(
                                        "str.{}() takes an int width, got {}",
                                        method_name,
                                        crate::type_to_string(&t)
                                    ),
                                );
                            }
                            let has_fill = arguments.len() == 2;
                            if has_fill {
                                let f = emit_expr(&arguments[1], func, ctx, memory_layout, None);
                                if f != IRType::String {
                                    return refuse(
                                        func,
                                        format!(
                                            "str.{method_name}()'s fill character must be a str, got {}",
                                            crate::type_to_string(&f)
                                        ),
                                    );
                                }
                            }
                            let mode = match method_name.as_str() {
                                "ljust" => PadMode::Left,
                                "rjust" => PadMode::Right,
                                _ => PadMode::Center,
                            };
                            emit_string_pad(func, ctx, mode, has_fill);
                            IRType::String
                        }

                        _ => {
                            // A string method the compiler does not implement
                            // used to drop the receiver's `(offset, length)`
                            // and every argument and push nothing, so the
                            // module failed WebAssembly validation and the
                            // error blamed code generation instead of naming
                            // the method. The list and dict receivers report
                            // theirs; this is the same case on a string.
                            ctx.report(format!(
                                "'{method_name}' is not supported on a str yet. \
                                 Hint: the supported string methods are capitalize, \
                                 center, count, endswith, find, format, index, \
                                 isalnum, isalpha, isdigit, islower, isspace, \
                                 isupper, join, ljust, lower, lstrip, replace, \
                                 rjust, rstrip, split, startswith, strip, title, \
                                 and upper"
                            ));
                            func.instruction(&Instruction::Drop); // length
                            func.instruction(&Instruction::Drop); // offset
                            func.instruction(&Instruction::Unreachable);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Unknown
                        }
                    }
                }
                IRType::File => {
                    emit_file_method_call(func, ctx, memory_layout, method_name, arguments)
                }
                IRType::List(_element_type) => emit_list_method_call(
                    func,
                    ctx,
                    memory_layout,
                    method_name,
                    arguments,
                    &object_type,
                ),
                IRType::Dict(_, _) => emit_dict_method_call(
                    func,
                    ctx,
                    memory_layout,
                    method_name,
                    arguments,
                    &object_type,
                ),
                IRType::Set(_element_type) => emit_set_method_call(
                    func,
                    ctx,
                    memory_layout,
                    method_name,
                    arguments,
                    &object_type,
                ),
                IRType::Tuple(members) => emit_tuple_method_call(
                    func,
                    ctx,
                    memory_layout,
                    method_name,
                    arguments,
                    members,
                ),
                // A tuple of any length has a list's layout, so its two methods
                // are the list's; nothing that would change it exists.
                IRType::VarTuple(member) if matches!(method_name.as_str(), "index" | "count") => {
                    emit_list_method_call(
                        func,
                        ctx,
                        memory_layout,
                        method_name,
                        arguments,
                        &IRType::List(member.clone()),
                    )
                }
                IRType::VarTuple(_) => {
                    ctx.report(format!("'tuple' object has no attribute '{method_name}'"));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
                IRType::Class(class_name) => {
                    // Custom class method call. The object pointer (`self`) is
                    // already on the stack; coerce the user arguments to the
                    // method's declared parameter types and report its real
                    // return type so float results flow correctly. The
                    // parameter/return lookup goes through `method_owner`: an
                    // inherited method is registered as `Base::method`, not
                    // `Sub::method`.
                    let method_idx = ctx
                        .get_class_info(class_name)
                        .and_then(|ci| ci.methods.get(method_name.as_str()).copied());
                    if let Some(method_idx) = method_idx {
                        let class_info = ctx.get_class_info(class_name);
                        let owner = class_info
                            .and_then(|ci| ci.method_owner.get(method_name.as_str()).cloned())
                            .unwrap_or_else(|| class_name.clone());
                        let kind = class_info
                            .and_then(|ci| ci.method_kinds.get(method_name.as_str()).copied())
                            .unwrap_or(MethodKind::Instance);
                        let (param_types, ret) = ctx
                            .get_function_info(&format!("{owner}::{method_name}"))
                            .map(|f| (f.param_types.clone(), f.return_type.clone()))
                            .unwrap_or((Vec::new(), IRType::Unknown));

                        // A method some subclass overrides dispatches through
                        // the vtable on the receiver's *runtime* class, not the
                        // class it is declared as. Everything else keeps its
                        // direct call.
                        if kind == MethodKind::Instance {
                            if let Some((column, type_index)) =
                                ctx.virtual_call(class_name, method_name)
                            {
                                return emit_virtual_call(
                                    func,
                                    ctx,
                                    memory_layout,
                                    method_name,
                                    arguments,
                                    &param_types,
                                    ret,
                                    column,
                                    type_index,
                                );
                            }
                        }

                        // A static or class method ignores the instance: drop
                        // the pointer, and for a classmethod push the static
                        // class's id as the implicit `cls` instead.
                        let arg_base = match kind {
                            MethodKind::Static => {
                                func.instruction(&Instruction::Drop);
                                0
                            }
                            MethodKind::Class => {
                                func.instruction(&Instruction::Drop);
                                let class_id = class_info.map(|ci| ci.class_id).unwrap_or_default();
                                func.instruction(&Instruction::I32Const(class_id));
                                1
                            }
                            _ => 1,
                        };
                        for (i, arg) in arguments.iter().enumerate() {
                            let t = emit_expr(
                                arg,
                                func,
                                ctx,
                                memory_layout,
                                param_types.get(i + arg_base),
                            );
                            check_class_arg(
                                ctx,
                                &t,
                                param_types.get(i + arg_base),
                                i,
                                &format!("{class_name}.{method_name}"),
                            );
                            // Narrow a string/bytes argument to its offset
                            // word, matching the calling convention used at
                            // instantiation sites.
                            if matches!(t, IRType::String | IRType::Bytes) {
                                func.instruction(&Instruction::Drop);
                            }
                        }
                        emit_user_call(func, ctx, method_idx);
                        // A call result is a single word; rebuild the
                        // string/bytes pair.
                        finish_user_call(func, ctx, &ret);
                        ret
                    } else {
                        // Method or class not found: drop the object and args.
                        func.instruction(&Instruction::Drop);
                        for arg in arguments {
                            emit_expr(arg, func, ctx, memory_layout, None);
                            func.instruction(&Instruction::Drop);
                        }
                        IRType::Unknown
                    }
                }
                other => {
                    // A method on a receiver kind with no method support
                    // yet, set mutation (`s.add(...)`) above all. Consume exactly
                    // what the receiver left on the stack: one word for a
                    // pointer-shaped value, two for a string/bytes pair. The
                    // previous unconditional pair of drops underflowed the
                    // stack for the one-word case and produced a module that
                    // failed validation while the compiler reported success.
                    func.instruction(&Instruction::Drop);
                    if matches!(other, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                    for arg in arguments {
                        let arg_type = emit_expr(arg, func, ctx, memory_layout, None);
                        func.instruction(&Instruction::Drop);
                        if matches!(arg_type, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                    }
                    // Anything that mutates the receiver would silently do
                    // nothing, so this is a compile error. The trap stays: the
                    // error sink lets the rest of the module compile (so one
                    // run reports every such call), and a module that is built
                    // and then discarded must still be valid.
                    ctx.report(format!(
                        "'{}' is not supported on a value of type {} yet. \
                         Hint: build the result with a supported operation, \
                         for example a list or dict, instead of mutating this value",
                        method_name,
                        crate::type_to_string(other)
                    ));
                    func.instruction(&Instruction::Unreachable);
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::RangeCall { start, stop, step } => {
            // Range object layout in memory: [start:i32][stop:i32][step:i32][current:i32]
            //
            // A fresh block per evaluation, like a collection literal. It was one
            // compile-time region per `range()` site, so a recursive function
            // looping over `range()` had every activation iterating the same
            // object: `walk(4)` summing `1 + walk(i)` over `range(n)` answered
            // 1 where CPython answers 15.
            let Some(blk) = ctx.hold() else {
                report_literal_too_deep(func, ctx, "range");
                return IRType::Range;
            };
            emit_literal_block(func, ctx, blk, 16);
            let store_field = |func: &mut Function, offset: u64| {
                func.instruction(&Instruction::I32Store(mem_off(offset)));
            };

            // start (default 0) at offset 0
            func.instruction(&Instruction::LocalGet(blk));
            if let Some(s) = start {
                emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
            } else {
                func.instruction(&Instruction::I32Const(0));
            }
            store_field(func, 0);

            // stop at offset 4
            func.instruction(&Instruction::LocalGet(blk));
            emit_expr(stop, func, ctx, memory_layout, Some(&IRType::Int));
            store_field(func, 4);

            // step (default 1) at offset 8
            func.instruction(&Instruction::LocalGet(blk));
            if let Some(s) = step {
                emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
            } else {
                func.instruction(&Instruction::I32Const(1));
            }
            store_field(func, 8);
            ctx.release_held();

            // current = start at offset 12
            func.instruction(&Instruction::LocalGet(blk));
            func.instruction(&Instruction::LocalGet(blk));
            func.instruction(&Instruction::I32Load(mem_off(0)));
            store_field(func, 12);

            func.instruction(&Instruction::LocalGet(blk));
            IRType::Range
        }
        IRExpr::DynamicImportExpr { module_name } => {
            // There is no module loader at runtime. This used to evaluate the
            // name, drop it, and answer 0 as if a module had been imported.
            ctx.report(
                "a dynamic import (importlib.import_module or __import__) is not supported; \
                 modules are linked at compile time. Hint: use an import statement",
            );
            let t = emit_expr(module_name, func, ctx, memory_layout, None);
            func.instruction(&Instruction::Drop);
            if matches!(t, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
        IRExpr::Lambda {
            params,
            body: _,
            captured_vars: _,
        } => {
            // The finalize pass lifts every lambda into `ClosureMake`; one that
            // was not lifted has no function to refer to.
            let param_types = params.iter().map(|p| p.param_type.clone()).collect();
            ctx.report("a lambda that was not lifted into a function cannot be compiled");
            func.instruction(&Instruction::I32Const(0));

            IRType::Callable {
                params: param_types,
                return_type: Box::new(IRType::Unknown),
            }
        }
        IRExpr::ClosureMake {
            lambda_name,
            captured,
        } => {
            // Closure creation (#43): allocate the environment
            // `[table_slot:i32][cap0:8B][cap1:8B]...` and copy each captured
            // enclosing local into it by value. The layout matches a list's
            // `[word][slot0]...`, so the lifted lambda rebinds captures with
            // plain `__env[k]` indexing. Captured floats would need an f64
            // store and a typed rebind; they read as 0 for now (documented
            // limitation), as does a name that never resolved to a local.
            let slot = ctx.lambda_slots.get(lambda_name).copied().unwrap_or(0);

            func.instruction(&Instruction::I32Const(
                (COLLECTION_HEADER + captured.len() as u32 * COLLECTION_SLOT) as i32,
            ));
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(ctx.temp_local));

            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Const(slot as i32));
            func.instruction(&Instruction::I32Store(slot_arg()));

            for (k, name) in captured.iter().enumerate() {
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                match ctx.get_local_info(name) {
                    Some(info) if !matches!(info.var_type, IRType::Float) => {
                        func.instruction(&Instruction::LocalGet(info.index));
                    }
                    // Captured floats are refused when the lambda is lifted;
                    // anything else that is not a local here is not a value
                    // the closure can be given. Both stored 0.
                    _ => {
                        ctx.report(format!(
                            "a closure captures '{name}', which is not a local variable of the \
                             function that creates it"
                        ));
                        func.instruction(&Instruction::I32Const(0));
                    }
                }
                func.instruction(&Instruction::I32Store(mem_off(
                    (COLLECTION_HEADER + k as u32 * COLLECTION_SLOT) as u64,
                )));
            }

            ctx.pending_captures
                .borrow_mut()
                .push((lambda_name.clone(), captured.clone()));

            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            // The value's type is the lifted function's signature, less the
            // environment, so a call through it is made at the real types.
            crate::compiler::function::closure_signature(lambda_name, ctx)
        }
    }
}

/// Scratch locals for the arithmetic helpers below, which need several values
/// live at once. They used to write to locals 0, 1, and 2 outright, which are
/// the function's first parameters: `a ** b` clobbered `a`, so reading it
/// afterwards gave the wrong value, and in a function whose first locals are
/// not the width the helper assumed, the module failed to validate.
const ARITH_A: u32 = 17;
const ARITH_B: u32 = 18;
const ARITH_C: u32 = 19;

/// `a ** b` over integers, by repeated multiplication.
///
/// Python answers a *float* for a negative exponent (`2 ** -1` is 0.5), which
/// an i32 result cannot hold; this used to answer 0 silently, and traps now.
pub fn emit_integer_power_operation(func: &mut Function, ctx: &CompilationContext) {
    let exp = ctx.temp_local + ARITH_A;
    let base = ctx.temp_local + ARITH_B;
    let result = ctx.temp_local + ARITH_C;

    // Stack: (base, exp).
    func.instruction(&Instruction::LocalSet(exp));
    func.instruction(&Instruction::LocalSet(base));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(result));

    // A negative exponent has no integer answer.
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);

    // while exp > 0 { result *= base; exp -= 1 }
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::LocalGet(base));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(exp));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(result));
}

/// `a ** b` over floats, by repeated multiplication over an integral exponent
/// (`x ** -2.0` is `1 / x ** 2`).
///
/// A fractional exponent needs exp/log, which this runtime does not have, so it
/// traps. It used to answer the base itself, silently, and the whole helper
/// used `return` for its special cases, which returned from the *enclosing
/// function* rather than from the expression.
pub fn emit_float_power_operation(func: &mut Function, ctx: &CompilationContext) {
    let exp = ctx.temp_local_f64;
    let base = ctx.temp_local_f64_2;
    let result = ctx.temp_local_f64_3;
    let count = ctx.temp_local + ARITH_A;
    let negative = ctx.temp_local + ARITH_B;

    // Stack: (base, exp).
    func.instruction(&Instruction::LocalSet(exp));
    func.instruction(&Instruction::LocalSet(base));

    // Only an exponent that is a whole number can be done this way.
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::F64Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);

    // count = |exp|, negative = exp < 0
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::F64Const(f64_const(0.0)));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(negative));
    func.instruction(&Instruction::LocalGet(exp));
    func.instruction(&Instruction::F64Abs);
    func.instruction(&Instruction::I32TruncF64S);
    func.instruction(&Instruction::LocalSet(count));

    // result = 1.0; while count > 0 { result *= base; count -= 1 }
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::LocalGet(base));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // A negative exponent is the reciprocal, and 0 ** -n has none.
    func.instruction(&Instruction::LocalGet(negative));
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::F64Const(f64_const(0.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_raise(
        func,
        ctx,
        "ZeroDivisionError",
        crate::compiler::exceptions::ExcArg::Text("0.0 cannot be raised to a negative power"),
        2,
    );
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(result));
}

/// `a % b` over floats, as `a - b * floor(a / b)`, which is Python's
/// convention (the result takes the divisor's sign) rather than C's `fmod`.
///
/// The subtraction used to run the other way round, so `3.5 % 2.0` answered
/// -1.5 instead of 1.5 wherever the module happened to validate at all.
pub fn emit_float_modulo_operation(func: &mut Function, ctx: &CompilationContext) {
    let divisor = ctx.temp_local_f64;
    let dividend = ctx.temp_local_f64_2;

    // Stack: (a, b).
    func.instruction(&Instruction::LocalSet(divisor));
    func.instruction(&Instruction::LocalSet(dividend));

    func.instruction(&Instruction::LocalGet(dividend));
    func.instruction(&Instruction::LocalGet(divisor));
    func.instruction(&Instruction::LocalGet(dividend));
    func.instruction(&Instruction::LocalGet(divisor));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::F64Floor);
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::F64Sub);
}

/// Emit WebAssembly instructions for list method calls
/// Host file-open flag bits — the `flags` argument of `waspy_host.open`,
/// folded at compile time from the Python mode string ("r", "w", "a", "rb",
/// "w+", ...). Part of the documented host interface (#25).
pub const FILE_FLAG_READ: i32 = 1;
pub const FILE_FLAG_WRITE: i32 = 2;
pub const FILE_FLAG_APPEND: i32 = 4;
pub const FILE_FLAG_BINARY: i32 = 8;
pub const FILE_FLAG_UPDATE: i32 = 16;

/// Default byte cap for a size-less `f.read()`: one WASM page.
const FILE_READ_DEFAULT_CAP: i32 = 65536;

/// Fold a compile-time `open()` mode string into host flag bits. A
/// non-literal (or unrecognized) mode falls back to read-only.
fn file_mode_flags(mode: &IRExpr) -> i32 {
    let IRExpr::Const(IRConstant::String(mode)) = mode else {
        return FILE_FLAG_READ;
    };
    let mut flags = 0;
    for ch in mode.chars() {
        flags |= match ch {
            'r' => FILE_FLAG_READ,
            'w' => FILE_FLAG_WRITE,
            'a' => FILE_FLAG_APPEND,
            'b' => FILE_FLAG_BINARY,
            '+' => FILE_FLAG_UPDATE,
            _ => 0,
        };
    }
    if flags & (FILE_FLAG_READ | FILE_FLAG_WRITE | FILE_FLAG_APPEND) == 0 {
        flags |= FILE_FLAG_READ;
    }
    flags
}

/// Emit a method call on a file object (#25). The file descriptor (the file
/// value's single i32 word) is already on the stack. Supported: `read([n])`,
/// `write(s)`, `close()`, `flush()`. Each lowers to the `waspy_host` imports;
/// scratch locals are used only across straight-line sequences with no nested
/// `emit_expr`, per the scratch-local discipline.
fn emit_file_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
) -> IRType {
    let mem = |offset: u64| MemArg {
        offset,
        align: 2,
        memory_index: 0,
    };

    let Some(io) = ctx.file_io else {
        // A File-typed value in a module whose IR never calls `open()` (so no
        // host imports were emitted). Keep the stack balanced and yield the
        // method's usual result shape.
        func.instruction(&Instruction::Drop); // fd
        for arg in arguments {
            let t = emit_expr(arg, func, ctx, memory_layout, None);
            func.instruction(&Instruction::Drop);
            if matches!(t, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
        }
        return match method_name {
            "read" => {
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Const(0));
                IRType::String
            }
            "write" => {
                func.instruction(&Instruction::I32Const(0));
                IRType::Int
            }
            "close" | "flush" => IRType::None,
            _ => {
                func.instruction(&Instruction::I32Const(0));
                IRType::Unknown
            }
        };
    };

    match method_name {
        "read" => {
            // read([n]) -> str: allocate a fresh [len][bytes][nul] blob and
            // fill it with `waspy_host.read` calls until the cap is reached
            // or the host reports EOF (n <= 0). Python's size-less read()
            // means "read everything"; here it reads up to one page — the
            // documented v0.19 cap. Scratch: t+0 fd, t+1 cap, t+2 blob base,
            // t+3 total read, t+4 last chunk size.
            let (t0, t1, t2, t3, t4) = (
                ctx.temp_local,
                ctx.temp_local + 1,
                ctx.temp_local + 2,
                ctx.temp_local + 3,
                ctx.temp_local + 4,
            );
            // Cap: explicit size argument, or the default page. Emitted while
            // the fd is still on the stack (before any scratch store), so a
            // nested expression can't clobber our locals.
            if let Some(size_arg) = arguments.first() {
                let t = emit_expr(size_arg, func, ctx, memory_layout, None);
                if t == IRType::Float {
                    ctx.report("read() takes an int size, not a float");
                    func.instruction(&Instruction::I32TruncF64S);
                }
            } else {
                func.instruction(&Instruction::I32Const(FILE_READ_DEFAULT_CAP));
            }
            func.instruction(&Instruction::LocalSet(t1)); // cap
            func.instruction(&Instruction::LocalSet(t0)); // fd

            // Python's read(-1) (and any negative size) means "read all":
            // widen to the default cap.
            func.instruction(&Instruction::LocalGet(t1));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::I32Const(FILE_READ_DEFAULT_CAP));
            func.instruction(&Instruction::LocalSet(t1));
            func.instruction(&Instruction::End);

            // base = __alloc(prefix + cap + 1 for the NUL)
            func.instruction(&Instruction::LocalGet(t1));
            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32 + 1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(t2));

            // total = 0
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(t3));

            // while total < cap: n = read(fd, base + prefix + total,
            // cap - total); if n <= 0 break; total += n
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(t3));
            func.instruction(&Instruction::LocalGet(t1));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(t0));
            func.instruction(&Instruction::LocalGet(t2));
            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(t3));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(t1));
            func.instruction(&Instruction::LocalGet(t3));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::Call(io.read));
            func.instruction(&Instruction::LocalTee(t4));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LeS);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(t3));
            func.instruction(&Instruction::LocalGet(t4));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(t3));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End); // loop
            func.instruction(&Instruction::End); // block

            // Stamp the length prefix so collection read-back and len() work
            // like every other runtime-built string.
            func.instruction(&Instruction::LocalGet(t2));
            func.instruction(&Instruction::LocalGet(t3));
            func.instruction(&Instruction::I32Store(mem(0)));

            // Result pair: (base + prefix, total)
            func.instruction(&Instruction::LocalGet(t2));
            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(t3));
            IRType::String
        }
        "write" => {
            // write(s) -> int (bytes written). Scratch: t+0 fd, t+1 offset,
            // t+2 length — stored only after every nested emit is done.
            let (t0, t1, t2) = (ctx.temp_local, ctx.temp_local + 1, ctx.temp_local + 2);
            match arguments.first() {
                Some(arg) => {
                    let t = emit_expr(arg, func, ctx, memory_layout, None);
                    match t {
                        IRType::String | IRType::Bytes => {
                            // Stack: fd, offset, length.
                            func.instruction(&Instruction::LocalSet(t2));
                            func.instruction(&Instruction::LocalSet(t1));
                            func.instruction(&Instruction::LocalSet(t0));
                            func.instruction(&Instruction::LocalGet(t0));
                            func.instruction(&Instruction::LocalGet(t1));
                            func.instruction(&Instruction::LocalGet(t2));
                            func.instruction(&Instruction::Call(io.write));
                        }
                        IRType::Float => {
                            // Not writable; consume and report 0 bytes.
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Drop); // fd
                            func.instruction(&Instruction::I32Const(0));
                        }
                        _ => {
                            // A single-word value (e.g. a string read back out
                            // of a collection slot) is a blob offset; recover
                            // its length from the prefix, the standard
                            // convention.
                            func.instruction(&Instruction::LocalSet(t1));
                            func.instruction(&Instruction::LocalSet(t0));
                            func.instruction(&Instruction::LocalGet(t0));
                            func.instruction(&Instruction::LocalGet(t1));
                            func.instruction(&Instruction::LocalGet(t1));
                            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
                            func.instruction(&Instruction::I32Sub);
                            func.instruction(&Instruction::I32Load(mem(0)));
                            func.instruction(&Instruction::Call(io.write));
                        }
                    }
                }
                None => {
                    func.instruction(&Instruction::Drop); // fd
                    func.instruction(&Instruction::I32Const(0));
                }
            }
            IRType::Int
        }
        "close" => {
            // close() -> None. The host result is dropped; None pushes
            // nothing (the Expression-statement convention).
            func.instruction(&Instruction::Call(io.close));
            func.instruction(&Instruction::Drop);
            IRType::None
        }
        "flush" => {
            // The host interface is unbuffered; flush is a no-op.
            func.instruction(&Instruction::Drop); // fd
            IRType::None
        }
        _ => {
            // Unknown file method: consume everything, yield 0.
            func.instruction(&Instruction::Drop); // fd
            for arg in arguments {
                let t = emit_expr(arg, func, ctx, memory_layout, None);
                func.instruction(&Instruction::Drop);
                if matches!(t, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
            }
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}

/// How many elements a list must have room for beyond its current length.
#[derive(Clone, Copy)]
pub(crate) enum Reserve {
    /// A fixed count known at compile time (`append`, `insert`, a new dict key).
    Count(i32),
    /// A count computed at runtime and held in a local (`extend`).
    Local(u32),
}

impl Reserve {
    fn push(self, func: &mut Function) {
        match self {
            Reserve::Count(n) => func.instruction(&Instruction::I32Const(n)),
            Reserve::Local(idx) => func.instruction(&Instruction::LocalGet(idx)),
        };
    }
}

/// Reallocate a list before a write would run past the end of its region.
///
/// On entry `ctx.temp_local` holds the list pointer; on exit it holds the
/// pointer the caller must write through, which is a fresh, larger `__alloc`
/// block whenever the region cannot hold `extra` more elements. Capacity
/// doubles (with a floor of [`LIST_GROWTH_FLOOR`], and never less than what was
/// asked for), which keeps a loop of appends linear overall.
///
/// Only the elements move: the region, and therefore every name for this
/// collection, stays put. A list grown inside a function it was passed to, or
/// reached by indexing another collection, is visible to the caller afterwards,
/// which is what the data-block indirection buys.
///
/// Scratch usage: `temp_local + 3..=temp_local + 5`. Callers must not hold live
/// values there across the call.
pub(crate) fn emit_collection_reserve(
    func: &mut Function,
    ctx: &CompilationContext,
    extra: Reserve,
    stride: u32,
) {
    let ptr = ctx.temp_local;
    let len = ctx.temp_local + 3;
    let cap = ctx.temp_local + 4;
    let new_ptr = ctx.temp_local + 5;

    // len = load(ptr); cap = load(ptr + COLLECTION_CAP)
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(MemArg {
        offset: COLLECTION_CAP as u64,
        align: 2,
        memory_index: 0,
    }));
    func.instruction(&Instruction::LocalSet(cap));

    // Room for `extra` more elements? A region built without a capacity word
    // reads cap 0 and takes the reallocating path, which is correct but
    // slower, never the other way round.
    func.instruction(&Instruction::LocalGet(len));
    extra.push(func);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32LeS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Else);

    // cap = max(cap*2, len + extra, LIST_GROWTH_FLOOR)
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(cap));
    for lower_bound in [None, Some(extra)] {
        // `None` is the constant floor; `Some` the requested length.
        func.instruction(&Instruction::LocalGet(cap));
        match lower_bound {
            None => func.instruction(&Instruction::I32Const(LIST_GROWTH_FLOOR)),
            Some(extra) => {
                func.instruction(&Instruction::LocalGet(len));
                extra.push(func);
                func.instruction(&Instruction::I32Add)
            }
        };
        // Both candidates are on the stack; keep the larger one.
        func.instruction(&Instruction::LocalGet(cap));
        match lower_bound {
            None => func.instruction(&Instruction::I32Const(LIST_GROWTH_FLOOR)),
            Some(extra) => {
                func.instruction(&Instruction::LocalGet(len));
                extra.push(func);
                func.instruction(&Instruction::I32Add)
            }
        };
        func.instruction(&Instruction::I32GtS);
        func.instruction(&Instruction::Select);
        func.instruction(&Instruction::LocalSet(cap));
    }

    // Only the elements move: new_data = __alloc(cap*stride), copy the live
    // ones across, and point the header at the new block. The region itself
    // stays where it is, so every name for this collection, the caller's
    // variable, an element of another collection, a field, a temporary, keeps
    // seeing the grown contents. That is the whole reason the elements sit
    // behind a pointer.
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Const(stride as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(new_ptr));

    // memory.copy(new_data, data, len*stride)
    func.instruction(&Instruction::LocalGet(new_ptr));
    func.instruction(&Instruction::LocalGet(ptr));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(stride as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(new_ptr));
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_DATA as u64)));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));

    func.instruction(&Instruction::End);
}

/// Smallest capacity a grown list is given, so `xs = []` followed by appends
/// does not reallocate on every element.
const LIST_GROWTH_FLOOR: i32 = 4;

/// `s.add(v)`, `s.remove(v)`, and `s.discard(v)`. Entry stack: (set_ptr).
///
/// `add` rehashes into a fresh, larger `__alloc` block when the table fills
/// (load factor above 1/2, counting tombstones), and writes the new pointer
/// back through the variable or field the set was reached through, exactly as
/// list and dict growth do. `remove`/`discard` leave a tombstone rather than an
/// empty bucket, so members whose probe ran past the removed one stay findable.
///
/// Python raises `KeyError` when `remove` is given a value the set does not
/// hold; there is no exception value to raise here, so that case traps, while
/// `discard` returns quietly like Python's.
pub fn emit_set_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    set_type: &IRType,
) -> IRType {
    crate::compiler::sets::emit_method(func, ctx, memory_layout, method_name, arguments, set_type)
}

/// How many positional arguments each supported list method takes, as
/// `(minimum, maximum)`. `sort`'s optional slot is its `reverse` keyword, which
/// lowering rewrites into a positional argument.
fn list_method_arity(method_name: &str) -> Option<(usize, usize)> {
    Some(match method_name {
        "append" | "count" | "extend" | "index" | "remove" | "__delitem__" => (1, 1),
        "__delslice__" => (2, 2),
        "clear" | "reverse" => (0, 0),
        "insert" => (2, 2),
        "pop" | "sort" => (0, 1),
        _ => return None,
    })
}

pub fn emit_list_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    list_type: &IRType,
) -> IRType {
    // Every arm below reads the arguments it needs and ignored the rest, so
    // `xs.append(3, 4)` compiled successfully and appended only the 3, and
    // `xs.clear(9)` dropped its argument on the floor. Python raises a
    // TypeError for both. Checking the count once here covers every method
    // rather than repeating it per arm.
    if let Some((min, max)) = list_method_arity(method_name) {
        if arguments.len() < min || arguments.len() > max {
            let takes = if min == max {
                format!("exactly {min}")
            } else {
                format!("{min} to {max}")
            };
            ctx.report(format!(
                "list.{method_name}() takes {takes} argument(s), got {}",
                arguments.len()
            ));
            func.instruction(&Instruction::Drop); // list_ptr
            func.instruction(&Instruction::Unreachable);
            func.instruction(&Instruction::I32Const(0));
            return IRType::Unknown;
        }
    }

    match method_name {
        "append" => {
            // list.append(value). Entry stack: (list_ptr). Each element occupies
            // one COLLECTION_SLOT; the value is stored at its natural width so a
            // float keeps full f64 precision. When the region is full the list
            // is reallocated first (see `emit_list_grow`), so an append never
            // writes into the collection that happens to sit next in memory.
            if !arguments.is_empty() {
                // Emit the value while list_ptr stays safely on the stack below
                // it, then stash it into a type-appropriate scratch local. It
                // is emitted at the list's element width, not its own, or an
                // int appended to a float list writes 4 bytes into an 8-byte
                // slot (#123).
                let value_type = emit_collection_element(
                    &arguments[0],
                    func,
                    ctx,
                    memory_layout,
                    collection_element_type(list_type).as_ref(),
                    "list.append()",
                );
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                emit_collection_reserve(func, ctx, Reserve::Count(1), COLLECTION_SLOT);

                // length = load(list_ptr)
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // address = list_ptr + HEADER + length*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                store_stashed_needle(func, ctx, &value_type, ctx.temp_local + 1);

                // length += 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Store(slot_arg()));
            }
            // append() returns None
            IRType::None
        }
        "__delitem__" => {
            // `del xs[i]` is `xs.pop(i)` with the element dropped.
            let popped =
                emit_list_method_call(func, ctx, memory_layout, "pop", arguments, list_type);
            if matches!(popped, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
            func.instruction(&Instruction::Drop);
            IRType::None
        }
        "__delslice__" => {
            // `del xs[a:b]`: clamp both bounds as slicing does and close the
            // gap. Both bounds are emitted while list_ptr is on the stack.
            emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
            emit_expr(&arguments[1], func, ctx, memory_layout, Some(&IRType::Int));
            let ptr = ctx.temp_local;
            let len = ctx.temp_local + 1;
            let start = ctx.temp_local + 2;
            let end = ctx.temp_local + 3;
            func.instruction(&Instruction::LocalSet(end));
            func.instruction(&Instruction::LocalSet(start));
            func.instruction(&Instruction::LocalSet(ptr));
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(len));
            emit_normalize_and_clamp(func, start, len);
            emit_normalize_and_clamp(func, end, len);

            func.instruction(&Instruction::LocalGet(end));
            func.instruction(&Instruction::LocalGet(start));
            func.instruction(&Instruction::I32GtS);
            func.instruction(&Instruction::If(BlockType::Empty));
            // memmove(data + start, data + end, (len - end) slots)
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(start));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(end));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(len));
            func.instruction(&Instruction::LocalGet(end));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            // length -= end - start
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::LocalGet(len));
            func.instruction(&Instruction::LocalGet(end));
            func.instruction(&Instruction::LocalGet(start));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::End);
            IRType::None
        }
        "pop" => {
            // list.pop([index]) — remove the element at `index` (the last one
            // by default) and answer it. Entry stack: (list_ptr).
            //
            // The index used to be evaluated, stored, and then never used for
            // anything but the load: the length was decremented and the tail
            // left where it was, so `[9, 3, 2].pop(0)` answered 9 (right) and
            // left `[9, 3]` behind (wrong, CPython leaves `[3, 2]`). Every
            // element above the popped one has to move down a slot, the same
            // shift `remove` does. This is the mirror of the `insert` position
            // bug: the argument was read and discarded.
            let elem_type = match list_type {
                IRType::List(t) => t.as_ref().clone(),
                _ => IRType::Unknown,
            };

            // The index is emitted while list_ptr is still on the stack: its
            // emission can use the scratch locals, and `xs.pop(ys.pop())`
            // popped from `ys` twice.
            if let Some(index) = arguments.first() {
                emit_expr(index, func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index
            }
            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

            // length = load(list_ptr)
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // length

            if !arguments.is_empty() {
                // A negative index counts from the end, as everywhere else.
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32LtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2));
            } else {
                // Last element: index = length - 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index
            }

            // A position the list does not have, an empty list included,
            // raises IndexError.
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::I32Or);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::If(BlockType::Empty));
            emit_raise(
                func,
                ctx,
                "IndexError",
                crate::compiler::exceptions::ExcArg::Text("pop from empty list"),
                2,
            );
            func.instruction(&Instruction::End);
            emit_raise(
                func,
                ctx,
                "IndexError",
                crate::compiler::exceptions::ExcArg::Text("pop index out of range"),
                1,
            );
            func.instruction(&Instruction::End);

            // Read the element out before the shift overwrites its slot. A
            // float keeps its own scratch so the f64 is not truncated; every
            // other element is one word, a string's being its blob offset.
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            if matches!(elem_type, IRType::Float) {
                func.instruction(&Instruction::F64Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
            } else {
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));
            }

            // Shift (index, length) one slot to the left, closing the gap.
            // memory.copy has memmove semantics, so the overlap is safe.
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });

            // length -= 1
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Store(slot_arg()));

            // Answer the element that was removed.
            if matches!(elem_type, IRType::Float) {
                func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
            } else {
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                if matches!(elem_type, IRType::String | IRType::Bytes) {
                    recover_str_pair(func, ctx);
                }
            }
            elem_type
        }
        "clear" => {
            // list.clear()
            // Stack: list_ptr
            // Set length to 0
            func.instruction(&Instruction::I32Const(0)); // length = 0
            func.instruction(&Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            IRType::None
        }
        "extend" => {
            // list.extend(iterable)
            // Appends all items from iterable to the list
            // Stack: list_ptr, iterable

            if !arguments.is_empty() {
                // Emit the iterable while list_ptr is still on the stack: its
                // emission can use the scratch locals.
                let iterable_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);

                // A tuple shares the list layout; one whose members share a type
                // copies slot for slot like a list.
                let source = match &iterable_type {
                    IRType::List(_) => Some(iterable_type.clone()),
                    IRType::VarTuple(member) => Some(IRType::List(member.clone())),
                    IRType::Tuple(members) if members.windows(2).all(|w| w[0] == w[1]) => {
                        Some(IRType::List(Box::new(
                            members.first().cloned().unwrap_or(IRType::Unknown),
                        )))
                    }
                    _ => None,
                };
                let Some(iterable_type) = source else {
                    // It dropped the iterable and extended by nothing, so
                    // `xs.extend(range(3))` left `xs` as it was.
                    ctx.report(format!(
                        "list.extend() takes a list or a tuple of one element type here, \
                         not '{}'. Hint: append the elements in a loop",
                        crate::type_to_string(&iterable_type)
                    ));
                    if matches!(iterable_type, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::Drop);
                    return IRType::None;
                };
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // iterable_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                // Elements are copied slot for slot, so the two lists have to
                // read their slots at the same width. Extending a float list
                // with an int one used to copy the raw words across and leave
                // every copied element reading as garbage (#123). Converting
                // per element would need a typed copy loop; the honest answer
                // until then is to name the mismatch.
                let dest_elem = collection_element_type(list_type);
                let src_elem = collection_element_type(&iterable_type);
                let width = |t: &Option<IRType>| matches!(t, Some(IRType::Float));
                if width(&dest_elem) != width(&src_elem) {
                    ctx.report(format!(
                        "list.extend() copies elements between lists that are read at \
                         different widths ('{}' into '{}'). Hint: make both element types \
                         the same, or append the elements in a loop",
                        src_elem
                            .as_ref()
                            .map(crate::type_to_string)
                            .unwrap_or_else(|| "unknown".to_string()),
                        dest_elem
                            .as_ref()
                            .map(crate::type_to_string)
                            .unwrap_or_else(|| "unknown".to_string()),
                    ));
                }

                // Load iterable length
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // iterable_len

                // Make room for every element about to be copied in;
                // this may replace list_ptr with a larger region.
                emit_collection_reserve(
                    func,
                    ctx,
                    Reserve::Local(ctx.temp_local + 2),
                    COLLECTION_SLOT,
                );

                // Load list length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // list_len

                // Initialize loop counter
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // i = 0

                // Loop: for i in range(iterable_len)
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Check if i >= iterable_len
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // i
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // iterable_len
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1)); // Exit loop if done

                // Copy the whole 8-byte slot from iterable[i] to
                // list[list_len] with a type-agnostic memory.copy, so any
                // element width (i32 word or full f64) moves intact.
                // dest = list_ptr + HEADER + list_len*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // list_len
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                // src = iterable_ptr + HEADER + i*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // iterable_ptr
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // i
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                // size = SLOT
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });

                // Increment list_len
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                // Increment i
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));

                // Loop back
                func.instruction(&Instruction::Br(0));

                // End loop
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                // Update list length
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // new length
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
            }
            IRType::None
        }
        "insert" => {
            // list.insert(index, value). Entry stack: (list_ptr). The index is
            // normalized and clamped like Python's, then the tail is moved one
            // slot to the right before the value is stored at its natural width.
            if arguments.len() >= 2 {
                let list_ptr = ctx.temp_local;
                let needle = ctx.temp_local + 1;
                let length = ctx.temp_local + 2;
                let index = ctx.temp_local + 6;

                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                let value_type = emit_collection_element(
                    &arguments[1],
                    func,
                    ctx,
                    memory_layout,
                    collection_element_type(list_type).as_ref(),
                    "list.insert()",
                );
                stash_search_needle(func, ctx, &value_type, needle);

                // Keep the index on the stack until the value has been emitted,
                // since a nested value expression may use the scratch locals.
                func.instruction(&Instruction::LocalSet(index));
                func.instruction(&Instruction::LocalSet(list_ptr));

                emit_collection_reserve(func, ctx, Reserve::Count(1), COLLECTION_SLOT);

                func.instruction(&Instruction::LocalGet(list_ptr));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(length));

                // A negative index counts from the end. Unlike item access,
                // insertion clamps the result into the inclusive [0, length]
                // range instead of raising IndexError.
                emit_normalize_and_clamp(func, index, length);

                // Shift [index, length) one slot to the right. memory.copy has
                // memmove semantics, so overlapping source and destination are
                // safe when the destination starts inside the source range.
                func.instruction(&Instruction::LocalGet(list_ptr));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(index));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(list_ptr));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(index));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(length));
                func.instruction(&Instruction::LocalGet(index));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });

                // address = data + index*SLOT
                func.instruction(&Instruction::LocalGet(list_ptr));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(index));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                store_stashed_needle(func, ctx, &value_type, needle);

                // length += 1
                func.instruction(&Instruction::LocalGet(list_ptr));
                func.instruction(&Instruction::LocalGet(length));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Store(slot_arg()));
            }
            IRType::None
        }
        "remove" => {
            // list.remove(value): find the first occurrence and shift the tail
            // down. Entry stack: (list_ptr). The match compares at the element's
            // natural width; the shift moves whole 8-byte slots with memory.copy
            // so any element type relocates intact.
            if !arguments.is_empty() {
                let elem_type = match list_type {
                    IRType::List(t) => t.as_ref().clone(),
                    _ => IRType::Unknown,
                };

                // Emit the searched value and stash it as a needle.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                // length = load(list_ptr); i = 0
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // i = 0

                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // if i >= length: break
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1));

                // slot address = list_ptr + HEADER + i*SLOT; compare to needle
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                emit_slot_eq_needle(func, ctx, &elem_type, ctx.temp_local + 1);

                // If equal, shift the tail left by one slot and decrement length.
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // j = i
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));

                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));
                // if j + 1 >= length: break
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1));

                // memory.copy(dest=list[j], src=list[j+1], SLOT)
                // dest = list_ptr + HEADER + j*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                // src = list_ptr + HEADER + (j+1)*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                // size = SLOT
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });

                // j += 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                func.instruction(&Instruction::Br(0));
                func.instruction(&Instruction::End); // shift loop
                func.instruction(&Instruction::End); // shift block

                // length -= 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::I32Store(slot_arg()));

                // i = -1 marks the value found
                func.instruction(&Instruction::I32Const(-1));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));
                func.instruction(&Instruction::Br(2)); // exit search loop
                func.instruction(&Instruction::End); // end if

                // i += 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));
                func.instruction(&Instruction::Br(0));
                func.instruction(&Instruction::End); // search loop
                func.instruction(&Instruction::End); // search block

                // A value the list does not hold raises ValueError, as in Python.
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::If(BlockType::Empty));
                let needle = if matches!(value_type, IRType::Float) {
                    ctx.temp_local_f64
                } else {
                    ctx.temp_local + 1
                };
                emit_raise(
                    func,
                    ctx,
                    "ValueError",
                    crate::compiler::exceptions::ExcArg::NotInList(needle, value_type.clone()),
                    1,
                );
                func.instruction(&Instruction::End);
            }
            IRType::None
        }
        "index" => {
            // list.index(value) -> int
            // Linear search for first occurrence
            if !arguments.is_empty() {
                // Emit the value to search for while list_ptr is still on the
                // stack, and stash it as a needle (f64 for floats) so the
                // per-slot compare matches the element width. Parking list_ptr
                // first lost it to the scratch locals the value's emission
                // used: `xs.index(ys.index(1))` searched `ys`.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                // Load length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // Initialize index to 0, and `found` to -1 so the check
                // after the loop can tell "absent" from "found at 0".
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(-1));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // found

                // Loop: check each element
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Check if current_index >= length
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // length
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1)); // Exit loop if done

                // slot address = list_ptr + HEADER + current_index*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);

                // Compare with the needle (width-aware).
                emit_slot_eq_needle(func, ctx, &value_type, ctx.temp_local + 1);

                // If equal, record the position and leave the loop. The
                // index used to be pushed and then branched over: a `br` to a
                // block whose result type is empty discards whatever sits
                // above the label, so the found index was thrown away and
                // execution fell out to the `-1` below. `[2, 3].index(2)`
                // answered -1 for a value the list held, and reported
                // success. The position goes in a local now, which survives
                // the branch.
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // found
                func.instruction(&Instruction::Br(2)); // Exit both blocks
                func.instruction(&Instruction::End);

                // Increment current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                // Loop back
                func.instruction(&Instruction::Br(0));

                // End loop
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                // A value the list does not hold raises ValueError.
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // found
                func.instruction(&Instruction::I32Const(-1));
                func.instruction(&Instruction::I32Eq);
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_raise(
                    func,
                    ctx,
                    "ValueError",
                    crate::compiler::exceptions::ExcArg::Text("list.remove(x): x not in list"),
                    1,
                );
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
            } else {
                func.instruction(&Instruction::Drop); // Drop list_ptr
                func.instruction(&Instruction::I32Const(0));
            }
            IRType::Int
        }
        "count" => {
            // list.count(value) -> int
            // Count occurrences
            if !arguments.is_empty() {
                // Emit the value to search for while list_ptr is still on the
                // stack, and stash it as a needle (f64 for floats) so the
                // per-slot compare matches the element width. Parking list_ptr
                // first lost it to the scratch locals the value's emission
                // used: `xs.index(ys.index(1))` searched `ys`.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                // Load length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // Initialize index and count
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // count

                // Loop: check each element
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Check if current_index >= length
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // length
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1)); // Exit loop if done

                // slot address = list_ptr + HEADER + current_index*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);

                // Compare with the needle (width-aware).
                emit_slot_eq_needle(func, ctx, &value_type, ctx.temp_local + 1);

                // If equal, increment count
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // count
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // count
                func.instruction(&Instruction::End);

                // Increment current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                // Loop back
                func.instruction(&Instruction::Br(0));

                // End loop
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                // Return count
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
            } else {
                func.instruction(&Instruction::Drop); // Drop list_ptr
                func.instruction(&Instruction::I32Const(0));
            }
            IRType::Int
        }
        "reverse" => {
            // list.reverse(): swap elements inwards from both ends, in place.
            // The swap moves the whole 8-byte slot as two i32 halves rather
            // than reading it at the element's type, so it is correct for any
            // element width (an f64's bit pattern included) and needs no i64
            // scratch local.
            let (ptr, data, len) = (ctx.temp_local, ctx.temp_local + 1, ctx.temp_local + 2);
            let (i, j) = (ctx.temp_local + 3, ctx.temp_local + 4);
            let (addr_i, addr_j) = (ctx.temp_local + 5, ctx.temp_local + 6);
            let (lo, hi) = (ctx.temp_local + 7, ctx.temp_local + 8);

            func.instruction(&Instruction::LocalSet(ptr));
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(len));
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalSet(data));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::LocalGet(len));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(j));

            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(j));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));

            emit_slot_addr(func, data, i, addr_i);
            emit_slot_addr(func, data, j, addr_j);

            // Save element i, copy j over it, then write the saved one to j.
            func.instruction(&Instruction::LocalGet(addr_i));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(lo));
            func.instruction(&Instruction::LocalGet(addr_i));
            func.instruction(&Instruction::I32Load(mem_off(4)));
            func.instruction(&Instruction::LocalSet(hi));

            func.instruction(&Instruction::LocalGet(addr_i));
            func.instruction(&Instruction::LocalGet(addr_j));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::LocalGet(addr_i));
            func.instruction(&Instruction::LocalGet(addr_j));
            func.instruction(&Instruction::I32Load(mem_off(4)));
            func.instruction(&Instruction::I32Store(mem_off(4)));

            func.instruction(&Instruction::LocalGet(addr_j));
            func.instruction(&Instruction::LocalGet(lo));
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::LocalGet(addr_j));
            func.instruction(&Instruction::LocalGet(hi));
            func.instruction(&Instruction::I32Store(mem_off(4)));

            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::LocalGet(j));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(j));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);

            IRType::None
        }
        "sort" => {
            // list.sort([reverse=<bool literal>]): insertion sort, in place.
            // Insertion sort rather than something asymptotically better
            // because it needs no auxiliary allocation and no recursion, and
            // the lists a compiled program sorts are small; the shape can be
            // swapped later without changing the contract.
            let elem_type = match list_type {
                IRType::List(inner) => (**inner).clone(),
                _ => IRType::Unknown,
            };
            // `reverse` reaches here as a positional argument (the converter
            // rewrites the keyword). Only a literal is accepted: a runtime flag
            // would need both orders emitted, and silently picking one is
            // exactly the class of bug this method is being fixed for.
            let descending = match arguments.first() {
                None => Some(false),
                Some(IRExpr::Const(IRConstant::Bool(b))) => Some(*b),
                Some(_) => None,
            };
            let float_elems = matches!(elem_type, IRType::Float);
            let sortable = matches!(
                elem_type,
                IRType::Int | IRType::Bool | IRType::Float | IRType::Unknown
            );

            if descending.is_none() || !sortable {
                if descending.is_none() {
                    ctx.report(
                        "list.sort()'s 'reverse' must be True or False written literally. \
                         Hint: branch on the flag and call sort() in each arm",
                    );
                } else {
                    ctx.report(format!(
                        "list.sort() does not support elements of type {} yet. \
                         Hint: sort a list of ints, floats, or bools",
                        crate::type_to_string(&elem_type)
                    ));
                }
                func.instruction(&Instruction::Drop); // list_ptr
                func.instruction(&Instruction::Unreachable);
                return IRType::None;
            }
            let descending = descending.unwrap_or(false);

            let (ptr, data, len) = (ctx.temp_local, ctx.temp_local + 1, ctx.temp_local + 2);
            let (i, j) = (ctx.temp_local + 3, ctx.temp_local + 4);
            let (addr_j, addr_next) = (ctx.temp_local + 5, ctx.temp_local + 6);
            let (key_lo, key_hi) = (ctx.temp_local + 7, ctx.temp_local + 8);
            let key_f64 = ctx.temp_local_f64;

            func.instruction(&Instruction::LocalSet(ptr));
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(len));
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalSet(data));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::LocalSet(i));

            // for i in 1..len
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(len));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));

            // key = elems[i], kept both as raw halves (to write back without
            // losing an f64's low word) and, for floats, as a typed value to
            // compare against.
            emit_slot_addr(func, data, i, addr_next);
            func.instruction(&Instruction::LocalGet(addr_next));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(key_lo));
            func.instruction(&Instruction::LocalGet(addr_next));
            func.instruction(&Instruction::I32Load(mem_off(4)));
            func.instruction(&Instruction::LocalSet(key_hi));
            if float_elems {
                func.instruction(&Instruction::LocalGet(addr_next));
                func.instruction(&Instruction::F64Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(key_f64));
            }

            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(j));

            // while j >= 0 and elems[j] is out of order against the key
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(j));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::BrIf(1));

            emit_slot_addr(func, data, j, addr_j);
            func.instruction(&Instruction::LocalGet(addr_j));
            if float_elems {
                func.instruction(&Instruction::F64Load(slot_arg()));
                func.instruction(&Instruction::LocalGet(key_f64));
                if descending {
                    func.instruction(&Instruction::F64Lt);
                } else {
                    func.instruction(&Instruction::F64Gt);
                }
            } else {
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalGet(key_lo));
                if descending {
                    func.instruction(&Instruction::I32LtS);
                } else {
                    func.instruction(&Instruction::I32GtS);
                }
            }
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::BrIf(1));

            // elems[j + 1] = elems[j]
            func.instruction(&Instruction::LocalGet(j));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(addr_next));
            emit_slot_addr(func, data, addr_next, addr_next);
            func.instruction(&Instruction::LocalGet(addr_next));
            func.instruction(&Instruction::LocalGet(addr_j));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::LocalGet(addr_next));
            func.instruction(&Instruction::LocalGet(addr_j));
            func.instruction(&Instruction::I32Load(mem_off(4)));
            func.instruction(&Instruction::I32Store(mem_off(4)));

            func.instruction(&Instruction::LocalGet(j));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(j));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);

            // elems[j + 1] = key
            func.instruction(&Instruction::LocalGet(j));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(addr_next));
            emit_slot_addr(func, data, addr_next, addr_next);
            func.instruction(&Instruction::LocalGet(addr_next));
            func.instruction(&Instruction::LocalGet(key_lo));
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::LocalGet(addr_next));
            func.instruction(&Instruction::LocalGet(key_hi));
            func.instruction(&Instruction::I32Store(mem_off(4)));

            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);

            IRType::None
        }
        _ => {
            // A list method the compiler does not implement used to be dropped
            // on the floor: the receiver was discarded, a 0 pushed in its
            // place, and compilation reported success. That is how `xs.sort()`
            // and `xs.reverse()`, which were implemented nowhere, came to
            // silently do nothing while every later read saw the unsorted list.
            // Anything that mutates or queries the receiver has to be a compile
            // error rather than a no-op; the trap keeps the module valid while
            // the error sink lets the rest of the walk finish and report every
            // such call.
            ctx.report(format!(
                "'{method_name}' is not supported on a list yet. \
                 Hint: the supported list methods are append, clear, count, \
                 extend, index, insert, pop, remove, reverse, and sort"
            ));
            func.instruction(&Instruction::Drop); // list_ptr
            func.instruction(&Instruction::Unreachable);
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}

/// Compute the address of collection slot `index_local` in the data block based
/// at `base_local`, leaving it in `dest_local`. `dest_local` may be the same
/// local as `index_local`.
fn emit_slot_addr(func: &mut Function, base_local: u32, index_local: u32, dest_local: u32) {
    func.instruction(&Instruction::LocalGet(base_local));
    func.instruction(&Instruction::LocalGet(index_local));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(dest_local));
}

/// `d.get(key[, default])`, `d.keys()`, `d.values()`, `d.items()` at runtime.
///
/// Dicts had no method arm at all, so every one of these was reported as an
/// unsupported method. `get` is the idiomatic accumulator
/// (`counts[w] = counts.get(w, 0) + 1`), and `.items()` only ever worked as a
/// `for` iterable, where lowering desugars it, never as a value.
/// `d[key] = value` on a dict, from locals: the dict pointer in
/// `ctx.temp_local`, the key in `temp_local + 1` (a float key in
/// `temp_local_f64_2`), and the value in `temp_local + 2` (a float value in
/// `temp_local_f64`). An entry with an equal key takes the value; otherwise
/// the entry is appended, growing the entry block when it is full. The search
/// compares keys of `key_type` as `==` does.
///
/// Scratch: `temp_local + 3..=temp_local + 5` and `temp_local + 14`, so the
/// caller holds nothing it needs there.
pub(crate) fn emit_dict_set(
    func: &mut Function,
    ctx: &CompilationContext,
    key_type: &IRType,
    value_is_float: bool,
) {
    let key_is_float = matches!(key_type, IRType::Float);
    let key_is_string = matches!(key_type, IRType::String | IRType::Bytes);
    let ptr = ctx.temp_local;
    let count = ctx.temp_local + 3;
    let counter = ctx.temp_local + 4;
    let found = ctx.temp_local + 5;
    let store_value = |func: &mut Function| {
        if value_is_float {
            func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
            func.instruction(&Instruction::F64Store(slot_arg()));
        } else {
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Store(slot_arg()));
        }
    };
    let entry = |func: &mut Function, index: u32, value: bool| {
        func.instruction(&Instruction::LocalGet(ptr));
        emit_data_base(func);
        func.instruction(&Instruction::LocalGet(index));
        func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        if value {
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Add);
        }
    };

    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(counter));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(found));

    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(counter));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    entry(func, counter, false);
    if key_is_float {
        func.instruction(&Instruction::F64Load(slot_arg()));
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
        func.instruction(&Instruction::F64Eq);
    } else if key_is_string {
        // Strings compare by content: an equal key built at run time sits at a
        // different offset.
        let slot_off = ctx.temp_local + 14;
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalSet(slot_off));
        emit_str_content_eq(func, ctx, slot_off, ctx.temp_local + 1);
    } else {
        emit_slot_eq_needle(func, ctx, key_type, ctx.temp_local + 1);
    }
    func.instruction(&Instruction::If(BlockType::Empty));
    entry(func, counter, true);
    store_value(func);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(counter));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(counter));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Not there: append, after making room (which reloads the count).
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_collection_reserve(func, ctx, Reserve::Count(1), DICT_ENTRY);
    entry(func, count, false);
    if key_is_float {
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
        func.instruction(&Instruction::F64Store(slot_arg()));
    } else {
        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
        func.instruction(&Instruction::I32Store(slot_arg()));
    }
    entry(func, count, true);
    store_value(func);
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::End);
}

/// Find `key` (a word in `needle`, or an f64 in `temp_local_f64_2` for a
/// float key) in the dict whose pointer is in `ptr`, leaving its entry index
/// in `found`, or -1. Scratch: `temp_local + 2`, `temp_local + 3`, and
/// `temp_local + 14`.
fn emit_dict_find(
    func: &mut Function,
    ctx: &CompilationContext,
    key_type: &IRType,
    ptr: u32,
    needle: u32,
    found: u32,
) {
    let count = ctx.temp_local + 2;
    let i = ctx.temp_local + 3;
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(count));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(ptr));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    if matches!(key_type, IRType::Float) {
        func.instruction(&Instruction::F64Load(slot_arg()));
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
        func.instruction(&Instruction::F64Eq);
    } else {
        let cmp = if matches!(key_type, IRType::Bytes) {
            IRType::String
        } else {
            key_type.clone()
        };
        emit_slot_eq_needle(func, ctx, &cmp, needle);
    }
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
}

/// Push the address of entry `index` (a local) of the dict in `ptr`, or of its
/// value slot.
fn push_dict_entry(func: &mut Function, ptr: u32, index: u32, value: bool) {
    func.instruction(&Instruction::LocalGet(ptr));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(index));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    if value {
        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
        func.instruction(&Instruction::I32Add);
    }
}

/// Remove entry `found` of the dict in `ptr`, closing the gap.
fn emit_dict_remove(func: &mut Function, ctx: &CompilationContext, ptr: u32, found: u32) {
    let count = ctx.temp_local + 2;
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(count));
    push_dict_entry(func, ptr, found, false);
    push_dict_entry(func, ptr, found, false);
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Store(slot_arg()));
}

/// Emit a dict key on its way into a search: the key the dict compares at,
/// a word in `needle` or an f64 in `temp_local_f64_2`. Returns the key type
/// the search compares at.
fn emit_dict_key(
    key: &IRExpr,
    key_type: &IRType,
    needle: u32,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let hint = if matches!(key_type, IRType::Float) {
        IRType::Float
    } else {
        IRType::Int
    };
    let t = emit_expr(key, func, ctx, memory_layout, Some(&hint));
    let cmp = match key_type {
        IRType::Unknown | IRType::Any => t.clone(),
        k => k.clone(),
    };
    if matches!(cmp, IRType::Float) {
        if matches!(t, IRType::Int | IRType::Bool) {
            func.instruction(&Instruction::F64ConvertI32S);
        }
        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
    } else {
        narrow_element_to_word(func, &t);
        func.instruction(&Instruction::LocalSet(needle));
    }
    cmp
}

/// The dict methods that change or copy the dict: `update`, `pop`,
/// `popitem`, `setdefault`, `copy`, and `clear`. The receiver's pointer is on
/// the stack.
#[allow(clippy::too_many_arguments)]
fn emit_dict_mutation(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    dict_type: &IRType,
    key_type: &IRType,
    value_type: &IRType,
) -> IRType {
    let takes = match method_name {
        "update" => 1..=1,
        "pop" => 1..=2,
        "setdefault" => 2..=2,
        _ => 0..=0,
    };
    if !takes.contains(&arguments.len()) {
        ctx.report(match method_name {
            "setdefault" if arguments.len() == 1 => format!(
                "dict.setdefault(key) inserts None, which a '{}' value has no distinct word \
                 for here. Hint: pass a default, d.setdefault(key, default)",
                crate::type_to_string(value_type)
            ),
            _ => format!(
                "dict.{method_name}() takes {} argument(s), not {}",
                if takes.start() == takes.end() {
                    takes.start().to_string()
                } else {
                    format!("{} or {}", takes.start(), takes.end())
                },
                arguments.len()
            ),
        });
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    }
    let hold = || crate::compiler::equality::hold(ctx);
    let value_float = matches!(value_type, IRType::Float);
    let load_slot = |func: &mut Function, float: bool| {
        if float {
            func.instruction(&Instruction::F64Load(slot_arg()));
        } else {
            func.instruction(&Instruction::I32Load(slot_arg()));
        }
    };

    match method_name {
        "clear" => {
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32Store(slot_arg()));
            IRType::None
        }
        "copy" => {
            let d = hold();
            let out = hold();
            func.instruction(&Instruction::LocalSet(d));
            func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
            func.instruction(&Instruction::LocalGet(d));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(out));
            store_runtime_data_ptr(func, out);
            for offset in [0, COLLECTION_CAP] {
                func.instruction(&Instruction::LocalGet(out));
                func.instruction(&Instruction::LocalGet(d));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::I32Store(mem_off(offset as u64)));
            }
            func.instruction(&Instruction::LocalGet(out));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(d));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(d));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            func.instruction(&Instruction::LocalGet(out));
            ctx.release_held();
            ctx.release_held();
            dict_type.clone()
        }
        "update" => {
            let d = hold();
            func.instruction(&Instruction::LocalSet(d));
            let other = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            let IRType::Dict(other_key, other_value) = &other else {
                ctx.report(format!(
                    "dict.update() of a '{}' is not supported. Hint: pass a dict",
                    crate::type_to_string(&other)
                ));
                if matches!(other, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
                func.instruction(&Instruction::Drop);
                ctx.release_held();
                return IRType::None;
            };
            let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
            let key = if open(key_type) {
                other_key.as_ref().clone()
            } else {
                key_type.clone()
            };
            let value = if open(value_type) {
                other_value.as_ref().clone()
            } else {
                value_type.clone()
            };
            let widen = matches!(value, IRType::Float)
                && matches!(other_value.as_ref(), IRType::Int | IRType::Bool);
            if !open(other_key) && *other_key.as_ref() != key
                || !open(other_value) && *other_value.as_ref() != value && !widen
            {
                ctx.report(format!(
                    "dict.update() of a '{}' into a '{}': the entries would be read back at \
                     another type",
                    crate::type_to_string(&other),
                    crate::type_to_string(dict_type)
                ));
            }
            let o = hold();
            let n = hold();
            let i = hold();
            func.instruction(&Instruction::LocalSet(o));
            func.instruction(&Instruction::LocalGet(o));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(n));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            ctx.enter_expr_blocks(2);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(n));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(d));
            func.instruction(&Instruction::LocalSet(ctx.temp_local));
            push_dict_entry(func, o, i, false);
            if matches!(key, IRType::Float) {
                func.instruction(&Instruction::F64Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
            } else {
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
            }
            push_dict_entry(func, o, i, true);
            let value_is_float = matches!(value, IRType::Float);
            if widen {
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::F64ConvertI32S);
            } else {
                load_slot(func, value_is_float);
            }
            func.instruction(&Instruction::LocalSet(if value_is_float {
                ctx.temp_local_f64
            } else {
                ctx.temp_local + 2
            }));
            emit_dict_set(func, ctx, &key, value_is_float);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            ctx.exit_expr_blocks(2);
            for _ in 0..4 {
                ctx.release_held();
            }
            IRType::None
        }
        "popitem" => {
            let d = hold();
            let last = hold();
            let pair = hold();
            func.instruction(&Instruction::LocalSet(d));
            func.instruction(&Instruction::LocalGet(d));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::If(BlockType::Empty));
            emit_raise(
                func,
                ctx,
                "KeyError",
                crate::compiler::exceptions::ExcArg::Text("popitem(): dictionary is empty"),
                1,
            );
            func.instruction(&Instruction::Else);
            func.instruction(&Instruction::LocalGet(d));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(last));
            // A (key, value) tuple of its own, copied slot for slot.
            func.instruction(&Instruction::I32Const(
                (COLLECTION_HEADER + 2 * COLLECTION_SLOT) as i32,
            ));
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(pair));
            store_runtime_data_ptr(func, pair);
            for offset in [0, COLLECTION_CAP] {
                func.instruction(&Instruction::LocalGet(pair));
                func.instruction(&Instruction::I32Const(2));
                func.instruction(&Instruction::I32Store(mem_off(offset as u64)));
            }
            for (slot, value) in [(0u64, false), (COLLECTION_SLOT as u64, true)] {
                func.instruction(&Instruction::LocalGet(pair));
                emit_data_base(func);
                push_dict_entry(func, d, last, value);
                func.instruction(&Instruction::I64Load(slot_arg()));
                func.instruction(&Instruction::I64Store(mem_off(slot)));
            }
            func.instruction(&Instruction::LocalGet(d));
            func.instruction(&Instruction::LocalGet(last));
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(pair));
            for _ in 0..3 {
                ctx.release_held();
            }
            IRType::Tuple(vec![key_type.clone(), value_type.clone()])
        }
        // pop and setdefault: find the key, then answer its value or the
        // default, removing the entry (pop) or inserting the default
        // (setdefault).
        _ => {
            let pop = method_name == "pop";
            let d = hold();
            let needle = hold();
            let found = hold();
            func.instruction(&Instruction::LocalSet(d));
            let cmp = emit_dict_key(&arguments[0], key_type, needle, func, ctx, memory_layout);
            if let Some(why) = crate::compiler::equality::hash_unsupported(ctx, &cmp) {
                ctx.report(format!("{why} cannot be a dict key"));
            }
            // A float key waits in a held f64 while the default is emitted.
            let float_key = matches!(cmp, IRType::Float).then(|| {
                let slot = hold_value(ctx, &IRType::Float);
                func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
                func.instruction(&Instruction::LocalSet(slot));
                slot
            });
            let result = hold_value(ctx, value_type);
            let default_ty = arguments.get(1).map(|default| {
                let expected = (!matches!(value_type, IRType::Unknown)).then_some(value_type);
                let t = emit_value(default, func, ctx, memory_layout, expected);
                if value_float && matches!(t, IRType::Int | IRType::Bool) {
                    func.instruction(&Instruction::F64ConvertI32S);
                }
                narrow_element_to_word(func, &t);
                func.instruction(&Instruction::LocalSet(result));
                t
            });
            if let Some(d_ty) = &default_ty {
                let fits = match (value_type, d_ty) {
                    (IRType::Unknown, _) => true,
                    (v, d) if v == d => true,
                    (IRType::Float, IRType::Int | IRType::Bool) => true,
                    (IRType::Class(a), IRType::Class(b)) => {
                        ctx.common_base(a, b).as_ref() == Some(a)
                    }
                    _ => false,
                };
                if !fits {
                    ctx.report(format!(
                        "dict.{method_name}()'s default is a '{}' where the dict holds '{}' \
                         values, and the answer can only be read as one of them",
                        crate::type_to_string(d_ty),
                        crate::type_to_string(value_type)
                    ));
                }
            }
            if let Some(slot) = float_key {
                func.instruction(&Instruction::LocalGet(slot));
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
            }
            emit_dict_find(func, ctx, &cmp, d, needle, found);
            func.instruction(&Instruction::LocalGet(found));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::If(BlockType::Empty));
            if pop {
                if default_ty.is_none() {
                    let key = match float_key {
                        Some(slot) => {
                            crate::compiler::exceptions::ExcArg::Value(slot, IRType::Float)
                        }
                        None => crate::compiler::exceptions::ExcArg::Value(needle, cmp.clone()),
                    };
                    emit_raise(func, ctx, "KeyError", key, 1);
                }
            } else {
                // Insert the default under the key.
                func.instruction(&Instruction::LocalGet(d));
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                func.instruction(&Instruction::LocalGet(needle));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
                func.instruction(&Instruction::LocalGet(result));
                func.instruction(&Instruction::LocalSet(if value_float {
                    ctx.temp_local_f64
                } else {
                    ctx.temp_local + 2
                }));
                emit_dict_set(func, ctx, &cmp, value_float);
            }
            func.instruction(&Instruction::Else);
            push_dict_entry(func, d, found, true);
            load_slot(func, value_float);
            func.instruction(&Instruction::LocalSet(result));
            if pop {
                emit_dict_remove(func, ctx, d, found);
            }
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(result));
            let answer = match (value_type, &default_ty) {
                (IRType::Unknown, Some(d)) => d.clone(),
                (v, _) => v.clone(),
            };
            if matches!(answer, IRType::String | IRType::Bytes) {
                recover_str_pair(func, ctx);
            }
            release_value(ctx, value_type);
            if float_key.is_some() {
                ctx.release_held_f64();
            }
            for _ in 0..3 {
                ctx.release_held();
            }
            answer
        }
    }
}

/// `dict.fromkeys(iterable[, value])`: a dict whose keys are the iterable's
/// elements, in order and each once, all mapped to `value` (None when absent).
fn emit_dict_fromkeys(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    arguments: &[IRExpr],
) -> IRType {
    if arguments.is_empty() || arguments.len() > 2 {
        ctx.report(format!(
            "dict.fromkeys() takes 1 or 2 arguments, not {}",
            arguments.len()
        ));
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    }
    let iter_ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
    let (elem, is_range) = match &iter_ty {
        IRType::List(e) | IRType::VarTuple(e) => (e.as_ref().clone(), false),
        IRType::Tuple(members) if members.windows(2).all(|w| w[0] == w[1]) => {
            (members.first().cloned().unwrap_or(IRType::Unknown), false)
        }
        IRType::Range => (IRType::Int, true),
        other => {
            if matches!(other, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
            func.instruction(&Instruction::Drop);
            ctx.report(format!(
                "dict.fromkeys() of a '{}' is not supported. Hint: pass a list or a tuple",
                crate::type_to_string(other)
            ));
            func.instruction(&Instruction::I32Const(0));
            return IRType::Unknown;
        }
    };
    if let Some(why) = crate::compiler::equality::hash_unsupported(ctx, &elem) {
        ctx.report(format!("{why} cannot be a dict key"));
    }
    let seq = crate::compiler::equality::hold(ctx);
    func.instruction(&Instruction::LocalSet(seq));
    let value_ty = match arguments.get(1) {
        Some(value) => {
            let t = emit_value(value, func, ctx, memory_layout, None);
            narrow_element_to_word(func, &t);
            t
        }
        None => {
            func.instruction(&Instruction::I32Const(0));
            IRType::None
        }
    };
    let value_float = matches!(value_ty, IRType::Float);
    let value = hold_value(ctx, &value_ty);
    func.instruction(&Instruction::LocalSet(value));
    emit_dict_literal(&[], None, func, ctx, memory_layout);
    let dict = crate::compiler::equality::hold(ctx);
    let n = crate::compiler::equality::hold(ctx);
    let i = crate::compiler::equality::hold(ctx);
    func.instruction(&Instruction::LocalSet(dict));
    emit_comp_iterable_len(func, ctx, is_range, seq);
    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    ctx.enter_expr_blocks(2);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(dict));
    func.instruction(&Instruction::LocalSet(ctx.temp_local));
    if is_range {
        func.instruction(&Instruction::LocalGet(seq));
        func.instruction(&Instruction::I32Load(mem_off(0)));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(seq));
        func.instruction(&Instruction::I32Load(mem_off(8)));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
    } else {
        func.instruction(&Instruction::LocalGet(seq));
        emit_data_base(func);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        if matches!(elem, IRType::Float) {
            func.instruction(&Instruction::F64Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
        } else {
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
        }
    }
    func.instruction(&Instruction::LocalGet(value));
    func.instruction(&Instruction::LocalSet(if value_float {
        ctx.temp_local_f64
    } else {
        ctx.temp_local + 2
    }));
    emit_dict_set(func, ctx, &elem, value_float);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    ctx.exit_expr_blocks(2);
    func.instruction(&Instruction::LocalGet(dict));
    ctx.release_held(); // i
    ctx.release_held(); // n
    ctx.release_held(); // dict
    release_value(ctx, &value_ty);
    ctx.release_held(); // seq
    IRType::Dict(Box::new(elem), Box::new(value_ty))
}

fn emit_dict_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    dict_type: &IRType,
) -> IRType {
    let (key_type, value_type) = match dict_type {
        IRType::Dict(k, v) => ((**k).clone(), (**v).clone()),
        _ => (IRType::Unknown, IRType::Unknown),
    };

    match method_name {
        "__delitem__" => {
            // `del d[k]`: find the entry, close the gap, and raise KeyError
            // for a key the dict does not hold.
            let arg_key_ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            narrow_element_to_word(func, &arg_key_ty);
            let ptr = ctx.temp_local;
            let needle = ctx.temp_local + 1;
            let count = ctx.temp_local + 2;
            let i = ctx.temp_local + 3;
            let found = ctx.temp_local + 4;
            let slot = ctx.temp_local + 6;
            func.instruction(&Instruction::LocalSet(needle));
            func.instruction(&Instruction::LocalSet(ptr));
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(count));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::I32Const(-1));
            func.instruction(&Instruction::LocalSet(found));

            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32GeU);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalTee(slot));
            let key_is_str = matches!(key_type, IRType::String | IRType::Bytes)
                || matches!(arg_key_ty, IRType::String | IRType::Bytes);
            let cmp_ty = if key_is_str {
                IRType::String
            } else {
                key_type.clone()
            };
            emit_slot_eq_needle(func, ctx, &cmp_ty, needle);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalSet(found));
            func.instruction(&Instruction::Br(2));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);

            func.instruction(&Instruction::LocalGet(found));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::If(BlockType::Empty));
            emit_raise(
                func,
                ctx,
                "KeyError",
                crate::compiler::exceptions::ExcArg::Value(needle, arg_key_ty.clone()),
                1,
            );
            func.instruction(&Instruction::Else);
            // memmove(entry found, entry found + 1, (count - found - 1) entries)
            func.instruction(&Instruction::LocalGet(slot));
            func.instruction(&Instruction::LocalGet(slot));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::LocalGet(found));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::End);
            IRType::None
        }
        "get" => {
            if arguments.is_empty() || arguments.len() > 2 {
                ctx.report("dict.get() takes one or two arguments");
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::Unreachable);
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }
            // A missing key with no default answers None: the null pointer,
            // which no instance is, or an optional number, boxed. Any other
            // value cannot be told apart from it: it answered 0, so
            // `d.get(k) == 0` held for a key `d` does not have.
            let boxes = arguments.len() == 1
                && matches!(value_type, IRType::Int | IRType::Bool | IRType::Float);
            if arguments.len() == 1 && !boxes && !matches!(value_type, IRType::Class(_)) {
                ctx.report(format!(
                    "dict.get(key) answers None for a missing key, which a '{}' value cannot \
                     be told apart from here. Hint: pass a default, d.get(key, default)",
                    crate::type_to_string(&value_type)
                ));
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::Unreachable);
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }

            // The dict pointer stays on the stack while the key is emitted,
            // and both stay in held slots while the default is: the default
            // is an ordinary argument, so Python evaluates it eagerly, and its
            // emission can use the scratch locals. `d.get(xs.index(2), -1)`
            // searched with a clobbered pointer.
            let arg_key_ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            narrow_element_to_word(func, &arg_key_ty);
            let (Some(needle), Some(ptr)) = (ctx.hold(), ctx.hold()) else {
                report_literal_too_deep(func, ctx, "dict.get()");
                return IRType::Unknown;
            };
            func.instruction(&Instruction::LocalSet(needle));
            func.instruction(&Instruction::LocalSet(ptr));

            // The result is the cell when the value is boxed.
            let value_float = matches!(value_type, IRType::Float);
            let float = value_float && !boxes;
            let result_f64 = ctx.temp_local_f64;
            let count = ctx.temp_local + 2;
            let i = ctx.temp_local + 3;
            let result = ctx.temp_local + 4;
            let slot = ctx.temp_local + 6;

            let expected = (!matches!(value_type, IRType::Unknown)).then_some(&value_type);
            let default_ty = if let Some(default) = arguments.get(1) {
                let t = emit_expr(default, func, ctx, memory_layout, expected);
                if float && matches!(t, IRType::Int | IRType::Bool) {
                    func.instruction(&Instruction::F64ConvertI32S);
                }
                narrow_element_to_word(func, &t);
                t
            } else {
                func.instruction(&Instruction::I32Const(0));
                IRType::None
            };
            let fits = match (&value_type, &default_ty) {
                _ if boxes => true,
                (IRType::Unknown, _) => true,
                (v, d) if v == d => true,
                (IRType::Float, IRType::Int | IRType::Bool) => true,
                (IRType::Class(_), IRType::None) => true,
                (IRType::Class(a), IRType::Class(b)) => ctx.common_base(a, b).as_ref() == Some(a),
                _ => false,
            };
            if !fits {
                ctx.report(format!(
                    "dict.get()'s default is a '{}' where the dict holds '{}' values, and the \
                     answer can only be read as one of them. Hint: pass a default of the value type",
                    crate::type_to_string(&default_ty),
                    crate::type_to_string(&value_type)
                ));
            }
            func.instruction(&Instruction::LocalSet(if float {
                result_f64
            } else {
                result
            }));

            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(count));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));

            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32GeU);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(slot));
            func.instruction(&Instruction::LocalGet(slot));
            // The declared key type can be Unknown for a dict that started as
            // `{}`, so the argument's own type decides too.
            let key_is_str = matches!(key_type, IRType::String | IRType::Bytes)
                || matches!(arg_key_ty, IRType::String | IRType::Bytes);
            let cmp_ty = if key_is_str {
                IRType::String
            } else {
                key_type.clone()
            };
            emit_slot_eq_needle(func, ctx, &cmp_ty, needle);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(slot));
            if boxes {
                // The value found, in a cell; a miss leaves None (0).
                if value_float {
                    func.instruction(&Instruction::F64Load(mem_off(COLLECTION_SLOT as u64)));
                } else {
                    func.instruction(&Instruction::I32Load(mem_off(COLLECTION_SLOT as u64)));
                }
                emit_box(func, ctx, &value_type);
                func.instruction(&Instruction::LocalSet(result));
            } else if float {
                func.instruction(&Instruction::F64Load(mem_off(COLLECTION_SLOT as u64)));
                func.instruction(&Instruction::LocalSet(result_f64));
            } else {
                func.instruction(&Instruction::I32Load(mem_off(COLLECTION_SLOT as u64)));
                func.instruction(&Instruction::LocalSet(result));
            }
            func.instruction(&Instruction::Br(2));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            ctx.release_held();
            ctx.release_held();

            let answer = match (&value_type, &default_ty) {
                _ if boxes => IRType::Optional(Box::new(value_type.clone())),
                (IRType::Unknown, d) => d.clone(),
                (IRType::Class(_), IRType::None) => IRType::Optional(Box::new(value_type.clone())),
                (v, _) => v.clone(),
            };
            func.instruction(&Instruction::LocalGet(if float {
                result_f64
            } else {
                result
            }));
            if matches!(answer, IRType::String | IRType::Bytes) {
                recover_str_pair(func, ctx);
            }
            answer
        }
        "keys" | "values" | "items" => {
            // Build a real list: keys, values, or two-slot tuples. As a `for`
            // iterable these are desugared during lowering; this is the path
            // that makes them work as a value, which is what `sorted(d.items())`
            // and `list(d.keys())` need.
            if !arguments.is_empty() {
                ctx.report(format!("dict.{method_name}() takes no arguments"));
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::Unreachable);
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }
            let ptr = ctx.temp_local;
            let count = ctx.temp_local + 1;
            let i = ctx.temp_local + 2;
            let out = ctx.temp_local + 3;
            let slot = ctx.temp_local + 4;
            let pair = ctx.temp_local + 5;

            func.instruction(&Instruction::LocalSet(ptr));
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(count));

            func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(out));
            store_runtime_data_ptr(func, out);
            func.instruction(&Instruction::LocalGet(out));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32Store(slot_arg()));
            func.instruction(&Instruction::LocalGet(out));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));

            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(count));
            func.instruction(&Instruction::I32GeU);
            func.instruction(&Instruction::BrIf(1));

            func.instruction(&Instruction::LocalGet(ptr));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(slot));

            // Destination slot address, pushed before the value a store pops.
            func.instruction(&Instruction::LocalGet(out));
            emit_data_base(func);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);

            match method_name {
                "keys" => {
                    func.instruction(&Instruction::LocalGet(slot));
                    func.instruction(&Instruction::I64Load(slot_arg()));
                    func.instruction(&Instruction::I64Store(slot_arg()));
                }
                "values" => {
                    func.instruction(&Instruction::LocalGet(slot));
                    func.instruction(&Instruction::I64Load(mem_off(COLLECTION_SLOT as u64)));
                    func.instruction(&Instruction::I64Store(slot_arg()));
                }
                _ => {
                    // A (key, value) tuple is a two-slot region of its own, and
                    // the list slot holds a pointer to it.
                    func.instruction(&Instruction::I32Const(
                        (COLLECTION_HEADER + 2 * COLLECTION_SLOT) as i32,
                    ));
                    func.instruction(&Instruction::Call(ctx.alloc_func_index));
                    func.instruction(&Instruction::LocalSet(pair));
                    store_runtime_data_ptr(func, pair);
                    func.instruction(&Instruction::LocalGet(pair));
                    func.instruction(&Instruction::I32Const(2));
                    func.instruction(&Instruction::I32Store(slot_arg()));
                    func.instruction(&Instruction::LocalGet(pair));
                    func.instruction(&Instruction::I32Const(2));
                    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));
                    // Slots are copied whole so an f64 key or value keeps both
                    // of its words.
                    func.instruction(&Instruction::LocalGet(pair));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(slot));
                    func.instruction(&Instruction::I64Load(slot_arg()));
                    func.instruction(&Instruction::I64Store(slot_arg()));
                    func.instruction(&Instruction::LocalGet(pair));
                    emit_data_base(func);
                    func.instruction(&Instruction::LocalGet(slot));
                    func.instruction(&Instruction::I64Load(mem_off(COLLECTION_SLOT as u64)));
                    func.instruction(&Instruction::I64Store(mem_off(COLLECTION_SLOT as u64)));
                    func.instruction(&Instruction::LocalGet(pair));
                    func.instruction(&Instruction::I32Store(slot_arg()));
                }
            }

            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);

            func.instruction(&Instruction::LocalGet(out));
            match method_name {
                "keys" => IRType::List(Box::new(key_type)),
                "values" => IRType::List(Box::new(value_type)),
                _ => IRType::List(Box::new(IRType::Tuple(vec![key_type, value_type]))),
            }
        }
        "update" | "pop" | "popitem" | "setdefault" | "copy" | "clear" => emit_dict_mutation(
            func,
            ctx,
            memory_layout,
            method_name,
            arguments,
            dict_type,
            &key_type,
            &value_type,
        ),
        _ => {
            ctx.report(format!(
                "'{method_name}' is not supported on a dict yet. \
                 Hint: the supported dict methods are get, items, keys, values, update, pop, \
                 popitem, setdefault, copy, and clear"
            ));
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::Unreachable);
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}

/// Emit WASM code for tuple method calls
/// Whether a tuple member of type `member` is compared with a search value of
/// type `needle`: `Some(true)` compares the slot, `Some(false)` skips a
/// position that can never be equal (a str against an int), and `None` is a
/// pairing whose answer this compiler cannot give, such as an int against a
/// float (`1 == 1.0` in Python) or a member of no known type.
fn tuple_member_compared(member: &IRType, needle: &IRType) -> Option<bool> {
    let int_like = |t: &IRType| matches!(t, IRType::Int | IRType::Bool);
    if member == needle || (int_like(member) && int_like(needle)) {
        return Some(!matches!(member, IRType::Unknown | IRType::Any));
    }
    let numeric = |t: &IRType| int_like(t) || matches!(t, IRType::Float);
    // An int beside a float compares by value (`emit_tuple_search`).
    if numeric(member) && numeric(needle) {
        return Some(true);
    }
    let opaque = |t: &IRType| {
        matches!(
            t,
            IRType::Unknown
                | IRType::Any
                | IRType::Class(_)
                | IRType::Union(_)
                | IRType::Optional(_)
        )
    };
    if opaque(member) || opaque(needle) {
        None
    } else {
        Some(false)
    }
}

/// `t.index(v)` and `t.count(v)`. Entry stack: (tuple_ptr).
///
/// A tuple's member types are known per position, so the search is unrolled
/// over the positions and each one is compared at its own width. A single loop
/// compared every slot at the search value's width, so a float needle read an
/// int member as half of an f64. `index` used to push the position it found
/// and branch past it, answering -1 for every value the tuple held.
fn emit_tuple_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    members: &[IRType],
) -> IRType {
    if !matches!(method_name, "index" | "count") {
        ctx.report(format!(
            "'{method_name}' is not a method of tuple. Hint: tuples support 'index' and 'count'"
        ));
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(0));
        return IRType::Unknown;
    }
    if arguments.len() != 1 {
        ctx.report(format!(
            "tuple.{method_name}() takes exactly one argument here, got {}",
            arguments.len()
        ));
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(0));
        return IRType::Int;
    }

    let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
    stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
    func.instruction(&Instruction::LocalSet(ctx.temp_local));
    let search = if method_name == "index" {
        TupleSearch::Index
    } else {
        TupleSearch::Count
    };
    emit_tuple_search(func, ctx, search, members, &value_type);
    IRType::Int
}

/// A fixed-length tuple and a `Tuple[T, ...]` whose members are all `T`:
/// both are tuples, laid out alike, and compare by their members.
fn same_tuple_kind(a: &IRType, b: &IRType) -> bool {
    match (a, b) {
        (IRType::VarTuple(m), IRType::Tuple(ms)) | (IRType::Tuple(ms), IRType::VarTuple(m)) => {
            ms.iter().all(|x| unify_open(x, m).is_some())
        }
        _ => false,
    }
}

/// What a search over a tuple's positions answers.
#[derive(Clone, Copy, PartialEq)]
enum TupleSearch {
    /// The first position holding the value, raising `ValueError` if none.
    Index,
    /// How many positions hold it.
    Count,
    /// Whether one does (`in`).
    Contains,
}

/// Search a tuple (its pointer in `temp_local`) for the needle stashed by
/// `stash_search_needle` (in `temp_local + 1`, or `temp_local_f64` for a
/// float), unrolled over the positions so each member is compared at its own
/// type. Pushes the answer.
fn emit_tuple_search(
    func: &mut Function,
    ctx: &CompilationContext,
    search: TupleSearch,
    members: &[IRType],
    value_type: &IRType,
) {
    let ptr = ctx.temp_local;
    let needle = ctx.temp_local + 1;
    let result = ctx.temp_local + 4;
    let what = match search {
        TupleSearch::Index => "tuple.index()",
        TupleSearch::Count => "tuple.count()",
        TupleSearch::Contains => "'in'",
    };

    let mut positions = Vec::new();
    for (i, member) in members.iter().enumerate() {
        match tuple_member_compared(member, value_type) {
            Some(true) => positions.push((i as u32, member)),
            Some(false) => {}
            None => {
                ctx.report(format!(
                    "{what} cannot compare a {} with the tuple's member {i} of type {}",
                    crate::type_to_string(value_type),
                    crate::type_to_string(member)
                ));
                func.instruction(&Instruction::I32Const(0));
                return;
            }
        }
    }

    let is_index = search == TupleSearch::Index;
    func.instruction(&Instruction::I32Const(if is_index { -1 } else { 0 }));
    func.instruction(&Instruction::LocalSet(result));
    func.instruction(&Instruction::Block(BlockType::Empty));
    for (i, member) in positions {
        func.instruction(&Instruction::LocalGet(ptr));
        emit_data_base(func);
        if i > 0 {
            func.instruction(&Instruction::I32Const((i * COLLECTION_SLOT) as i32));
            func.instruction(&Instruction::I32Add);
        }
        let int_like = |t: &IRType| matches!(t, IRType::Int | IRType::Bool);
        if int_like(member) != int_like(value_type) && member != value_type {
            // An int and a float: both as f64, which holds every i32 exactly.
            if int_like(member) {
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::F64ConvertI32S);
            } else {
                func.instruction(&Instruction::F64Load(slot_arg()));
            }
            if int_like(value_type) {
                func.instruction(&Instruction::LocalGet(needle));
                func.instruction(&Instruction::F64ConvertI32S);
            } else {
                func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
            }
            func.instruction(&Instruction::F64Eq);
        } else {
            emit_slot_eq_needle(func, ctx, member, needle);
        }
        func.instruction(&Instruction::If(BlockType::Empty));
        match search {
            TupleSearch::Index => {
                func.instruction(&Instruction::I32Const(i as i32));
                func.instruction(&Instruction::LocalSet(result));
                func.instruction(&Instruction::Br(1));
            }
            TupleSearch::Contains => {
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::LocalSet(result));
                func.instruction(&Instruction::Br(1));
            }
            TupleSearch::Count => {
                func.instruction(&Instruction::LocalGet(result));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(result));
            }
        }
        func.instruction(&Instruction::End);
    }
    func.instruction(&Instruction::End);

    if is_index {
        // A missing value raises ValueError, as list.index does.
        func.instruction(&Instruction::LocalGet(result));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32LtS);
        func.instruction(&Instruction::If(BlockType::Empty));
        crate::compiler::function::emit_raise(
            func,
            ctx,
            "ValueError",
            crate::compiler::exceptions::ExcArg::Text("tuple.index(x): x not in tuple"),
            1,
        );
        func.instruction(&Instruction::End);
    }
    func.instruction(&Instruction::LocalGet(result));
}
