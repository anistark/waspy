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

/// Resolve a bare name used as a call/attribute receiver to a class, when it
/// statically denotes one: either a class's own name (`Counter.create()`), or
/// `cls` inside a classmethod, whose parameter is typed as the defining class
/// during IR conversion. Dispatch through the result is static, consistent
/// with the rest of the object model (no vtables).
fn static_class_target(ctx: &CompilationContext, name: &str) -> Option<String> {
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
        // Narrow a string/bytes argument to its offset word, matching the
        // calling convention used at instantiation sites.
        if matches!(t, IRType::String | IRType::Bytes) {
            func.instruction(&Instruction::Drop);
        }
    }
    emit_user_call(func, ctx, method_idx);
    // A call result is a single word; rebuild the string/bytes pair.
    if matches!(ret, IRType::String | IRType::Bytes) {
        recover_str_pair(func, ctx);
    }
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

/// Emit the ASCII case transform for the byte in `b`, leaving the transformed
/// byte back in `b`. `upper` selects the direction.
fn emit_case_byte(func: &mut Function, b: u32, upper: bool) {
    let (lo, hi, delta) = if upper {
        (b'a', b'z', -32)
    } else {
        (b'A', b'Z', 32)
    };
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(lo as i32));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(hi as i32));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(delta));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalSet(b));
}

/// Leave 1 on the stack when the byte in `b` is an ASCII letter.
fn emit_is_ascii_letter(func: &mut Function, b: u32) {
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(b'a' as i32));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(b'z' as i32));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(b'A' as i32));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(b'Z' as i32));
    func.instruction(&Instruction::I32LeU);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::I32Or);
}

/// `str.upper()` / `lower()` / `capitalize()` / `title()` at runtime.
///
/// Entry stack: `(offset, length)`. Exit stack: `(offset, length)` of a fresh
/// block holding the transformed bytes; the receiver is never mutated, matching
/// Python, where strings are immutable.
///
/// Only ASCII is transformed. A byte at or above 0x80 traps rather than passing
/// through: CPython case-folds the whole of Unicode, and passing the byte
/// through unchanged would answer `"CAFÉ".lower() == "cafÉ"` while reporting
/// success, which is the silent-wrong-answer class this method was fixed for.
fn emit_string_case(func: &mut Function, ctx: &CompilationContext, mode: CaseMode) {
    let off = ctx.temp_local;
    let len = ctx.temp_local + 1;
    let i = ctx.temp_local + 2;
    let blk = ctx.temp_local + 3;
    let b = ctx.temp_local + 4;
    let prev_letter = ctx.temp_local + 5;

    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));

    // block = __alloc(4 + len + 1); [len][bytes...][NUL]
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(blk));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(blk));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    // `title` treats a letter following a non-letter as the start of a word,
    // and position 0 always is one.
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(prev_letter));

    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    // b = load8_u(off + i)
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalSet(b));

    // Non-ASCII: trap rather than answer something CPython would not.
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Const(0x80));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);

    match mode {
        CaseMode::Upper => emit_case_byte(func, b, true),
        CaseMode::Lower => emit_case_byte(func, b, false),
        CaseMode::Capitalize => {
            // Position 0 uppercases, every later byte lowercases.
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::If(BlockType::Empty));
            emit_case_byte(func, b, true);
            func.instruction(&Instruction::Else);
            emit_case_byte(func, b, false);
            func.instruction(&Instruction::End);
        }
        CaseMode::Title => {
            // Remember whether this byte was a letter before transforming it,
            // since the transform does not change letterness but reads clearer
            // this way.
            let was_letter = ctx.temp_local + 6;
            emit_is_ascii_letter(func, b);
            func.instruction(&Instruction::LocalSet(was_letter));
            func.instruction(&Instruction::LocalGet(prev_letter));
            func.instruction(&Instruction::I32Eqz);
            func.instruction(&Instruction::If(BlockType::Empty));
            emit_case_byte(func, b, true);
            func.instruction(&Instruction::Else);
            emit_case_byte(func, b, false);
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(was_letter));
            func.instruction(&Instruction::LocalSet(prev_letter));
        }
    }

    // store8(blk + i, b)
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Store8(byte_arg()));

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // NUL-terminate, then hand back (offset, length).
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(len));
}

/// Leave 1 on the stack when the byte in `b` should be trimmed: a member of the
/// cut set when `has_chars`, otherwise ASCII whitespace.
fn emit_is_trimmable(func: &mut Function, ctx: &CompilationContext, b: u32, has_chars: bool) {
    if !has_chars {
        // Python's str.strip() with no argument trims ASCII whitespace:
        // space, \t, \n, \r, \v, \f.
        for (idx, ws) in [b' ', b'\t', b'\n', b'\r', 0x0b, 0x0c].iter().enumerate() {
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::I32Const(*ws as i32));
            func.instruction(&Instruction::I32Eq);
            if idx > 0 {
                func.instruction(&Instruction::I32Or);
            }
        }
        return;
    }
    // Membership in the cut set: scan it, leaving 1 if the byte occurs.
    let chars_off = ctx.temp_local + 7;
    let chars_len = ctx.temp_local + 8;
    let k = ctx.temp_local + 9;
    let found = ctx.temp_local + 10;

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::LocalGet(chars_len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(chars_off));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(found));
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

    // start: advance while the byte is trimmable (left-trimming modes only).
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(start));
    if mode != TrimMode::Right {
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::LocalGet(len));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Load8U(byte_arg()));
        func.instruction(&Instruction::LocalSet(b));
        emit_is_trimmable(func, ctx, b, has_chars);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(start));
        func.instruction(&Instruction::I32Const(1));
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
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Load8U(byte_arg()));
        func.instruction(&Instruction::LocalSet(b));
        emit_is_trimmable(func, ctx, b, has_chars);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(end));
        func.instruction(&Instruction::I32Const(1));
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
fn slot_arg() -> MemArg {
    MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }
}

/// Point a runtime-built region's data pointer at the block right after its
/// header, the layout every collection starts life in.
fn store_runtime_data_ptr(func: &mut Function, ptr_local: u32) {
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_DATA as u64)));
}

/// Replace a collection pointer on top of the stack with the address of its
/// first element.
fn emit_data_base(func: &mut Function) {
    func.instruction(&Instruction::I32Load(mem_off(COLLECTION_DATA as u64)));
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

/// Rebuild the `(offset, length)` pair from a bare string/bytes offset on top
/// of the stack, loading the length from the blob's prefix
/// (`load(offset - STRING_LEN_PREFIX)`). Used wherever a single offset word
/// crosses back into pair-land: a call result (functions return one word), an
/// instance field read, or a `__i32_to_str` result.
fn recover_str_pair(func: &mut Function, ctx: &CompilationContext) {
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
        _ => actual,
    }
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
        IRType::Tuple(_) | IRType::List(_) | IRType::Dict(_, _) | IRType::Set(_) | IRType::Class(_)
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

/// With a slot address on top of the stack, store the needle stashed by
/// [`stash_search_needle`] into it (float as `f64`, otherwise the i32 word). Used
/// where a search appends the searched element (e.g. set dedup insertion).
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

// --- Set hash table (#P3) ------------------------------------------------
//
// A set is an open-addressing hash table with linear probing, so membership and
// construction dedup are (amortised) constant time instead of linear scans:
//
//   [count:i32][cap:i32][bucket0][bucket1]...[bucket_{cap-1}]
//
// `cap` is a power of two (so the bucket index is `hash & (cap-1)`) and is
// always strictly greater than the member count, which guarantees that probing
// always meets an empty bucket and therefore terminates. Each bucket is
//
//   [state:i32][_pad:i32][value:8 bytes]
//
// where state 0 = empty and 1 = occupied; the value slot holds a full f64 (or an
// i32 in its low word), matching the element widths used elsewhere. The member
// count stays at offset 0, so `len(set)` is unchanged. `add` grows the table by
// rehashing into a fresh `__alloc` block when it fills, and `remove`/`discard`
// leave a tombstone (state 2) so the members that probed past the removed
// bucket stay reachable.

/// Bytes of set header: `count`, `cap`, `used`, and a pointer to the bucket
/// block, which also keeps the block that follows 8-byte aligned so every value
/// slot is. The buckets sit behind a pointer for the same reason a list's
/// elements do: rehashing swaps the block without moving the set, so every name
/// for it keeps seeing the members.
const SET_HEADER: u32 = 16;
/// Byte offset of the table capacity within the header.
const SET_CAP: u32 = 4;
/// Byte offset of the bucket-block pointer within the set header.
const SET_DATA: u32 = 12;
/// Byte offset of `used` within the header: occupied buckets plus tombstones.
/// Growth is decided on this rather than on the member count, because a
/// tombstone still costs a probe step, and an insert probe only stops at an
/// empty bucket. Rehashing drops the tombstones and resets it to the count.
const SET_USED: u32 = 8;
/// Bucket states. A removed member leaves a tombstone rather than an empty
/// bucket, so members that probed past it are still reachable.
const SET_EMPTY: i32 = 0;
const SET_LIVE: i32 = 1;
const SET_DEAD: i32 = 2;
/// Bytes per bucket: `state` (i32) + padding + an 8-byte value.
const SET_BUCKET: u32 = 16;
/// Byte offset of the value within a bucket (past the state word + padding).
const SET_BUCKET_VALUE: u32 = 8;

/// Replace a set pointer on top of the stack with the address of its first
/// bucket.
fn emit_set_base(func: &mut Function) {
    func.instruction(&Instruction::I32Load(mem_off(SET_DATA as u64)));
}

/// Point a set's bucket pointer at the block right after its header.
fn store_set_data_ptr(func: &mut Function, ptr_local: u32) {
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::LocalGet(ptr_local));
    func.instruction(&Instruction::I32Const(SET_HEADER as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(mem_off(SET_DATA as u64)));
}

/// Power-of-two capacity for a set literal of `n` elements. Kept at >= 2*n (load
/// factor <= 0.5) and >= 1 so a probe always finds an empty bucket.
fn set_capacity(n: usize) -> u32 {
    let target = (n as u32).saturating_mul(2).max(1);
    let mut cap = 1u32;
    while cap < target {
        cap <<= 1;
    }
    cap
}

/// Push the i32 hash of the needle stashed by [`stash_search_needle`]. For floats
/// the f64 bit pattern's two halves are folded together (small floats like 1.5 /
/// 2.5 share their low 32 bits, so hashing only the low word would collide every
/// one). The caller masks the result with `cap - 1`.
fn emit_set_hash(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_type: &IRType,
    needle_i32: u32,
) {
    if matches!(elem_type, IRType::Float) {
        // high32(bits) ^ low32(bits)
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
        func.instruction(&Instruction::I64ReinterpretF64);
        func.instruction(&Instruction::I64Const(32));
        func.instruction(&Instruction::I64ShrU);
        func.instruction(&Instruction::I32WrapI64);
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
        func.instruction(&Instruction::I64ReinterpretF64);
        func.instruction(&Instruction::I32WrapI64);
        func.instruction(&Instruction::I32Xor);
    } else {
        // Equal values must land in the same bucket. A string hashed by its
        // offset, so one built at runtime missed an equal member already in
        // the set; a tuple hashed by its pointer, so no two were ever equal.
        if let Some(why) = crate::compiler::equality::hash_unsupported(ctx, elem_type) {
            ctx.report(format!("{why} cannot be a set member or dict key here"));
            func.instruction(&Instruction::I32Const(0));
            return;
        }
        crate::compiler::equality::emit_value_hash(func, ctx, elem_type, needle_i32);
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
// The result block comes from `__alloc`, so — unlike literal template regions —
// a comprehension built inside a loop is automatically a fresh region per
// iteration. Helper locals are reserved by the scan pass keyed on
// comprehension nesting depth (see `scan_expr_locals`); with multiple
// generators the inner iterable expressions are evaluated once per outer
// iteration in *both* passes, so side effects in them run twice (documented
// limitation, matching the "no observable side effects in iterables" reality
// of the current language subset).

/// `MemArg` for an access at `offset` with the collection alignment hint.
/// Which character class a string predicate tests.
#[derive(Clone, Copy, PartialEq)]
enum ClassMode {
    Digit,
    Alpha,
    Alnum,
    Space,
    Upper,
    Lower,
}

/// Leave 1 on the stack when the byte in `b` is in the class `mode` tests.
/// `Upper`/`Lower` push letter-ness here; the "is it cased at all" half is
/// handled by the caller, which has to track whether any cased byte was seen.
fn emit_byte_in_class(func: &mut Function, b: u32, mode: ClassMode) {
    fn range(func: &mut Function, b: u32, lo: u8, hi: u8) {
        func.instruction(&Instruction::LocalGet(b));
        func.instruction(&Instruction::I32Const(lo as i32));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::LocalGet(b));
        func.instruction(&Instruction::I32Const(hi as i32));
        func.instruction(&Instruction::I32LeU);
        func.instruction(&Instruction::I32And);
    }
    match mode {
        ClassMode::Digit => range(func, b, b'0', b'9'),
        ClassMode::Upper => range(func, b, b'A', b'Z'),
        ClassMode::Lower => range(func, b, b'a', b'z'),
        ClassMode::Alpha => {
            range(func, b, b'a', b'z');
            range(func, b, b'A', b'Z');
            func.instruction(&Instruction::I32Or);
        }
        ClassMode::Alnum => {
            range(func, b, b'a', b'z');
            range(func, b, b'A', b'Z');
            func.instruction(&Instruction::I32Or);
            range(func, b, b'0', b'9');
            func.instruction(&Instruction::I32Or);
        }
        ClassMode::Space => {
            for (idx, ws) in [b' ', b'\t', b'\n', b'\r', 0x0b, 0x0c].iter().enumerate() {
                func.instruction(&Instruction::LocalGet(b));
                func.instruction(&Instruction::I32Const(*ws as i32));
                func.instruction(&Instruction::I32Eq);
                if idx > 0 {
                    func.instruction(&Instruction::I32Or);
                }
            }
        }
    }
}

/// `str.isdigit()` and friends, on a runtime string.
///
/// Entry stack: `(offset, length)`. Exit stack: an i32 bool. Python answers
/// False for the empty string in every one of these, and `isupper`/`islower`
/// additionally require at least one cased character, so `"123".isupper()` is
/// False while `"A1".isupper()` is True.
fn emit_string_predicate(func: &mut Function, ctx: &CompilationContext, mode: ClassMode) {
    let off = ctx.temp_local;
    let len = ctx.temp_local + 1;
    let i = ctx.temp_local + 2;
    let b = ctx.temp_local + 3;
    let ok = ctx.temp_local + 4;
    let seen_cased = ctx.temp_local + 5;
    let cased = matches!(mode, ClassMode::Upper | ClassMode::Lower);

    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(ok));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(seen_cased));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));

    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(byte_arg()));
    func.instruction(&Instruction::LocalSet(b));

    if cased {
        // A cased byte of the wrong case makes the answer False outright; an
        // uncased byte (a digit, a space) is simply ignored.
        let other = if mode == ClassMode::Upper {
            ClassMode::Lower
        } else {
            ClassMode::Upper
        };
        emit_byte_in_class(func, b, other);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(ok));
        func.instruction(&Instruction::Br(2));
        func.instruction(&Instruction::End);
        emit_byte_in_class(func, b, mode);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::LocalSet(seen_cased));
        func.instruction(&Instruction::End);
    } else {
        emit_byte_in_class(func, b, mode);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(ok));
        func.instruction(&Instruction::Br(2));
        func.instruction(&Instruction::End);
    }

    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // The empty string is False; a cased predicate also needs a cased byte.
    func.instruction(&Instruction::LocalGet(ok));
    if cased {
        func.instruction(&Instruction::LocalGet(seen_cased));
    } else {
        func.instruction(&Instruction::LocalGet(len));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32GtU);
    }
    func.instruction(&Instruction::I32And);
}

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
                emit_raise(func, ctx, "ValueError", 1);
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

/// Build the one-character string at byte `idx` of the string at `off`, and
/// leave its `(offset, length)` pair on the stack.
///
/// The result is a fresh one-byte blob with its own length prefix, never a
/// pointer into the source. Codegen narrows a string to its offset wherever it
/// must fit in a word (a function argument, a field, a collection element, a
/// dict key, a return value) and recovers the length from the four bytes before
/// it, so an interior pointer reads the preceding characters as its length.
/// Both `s[i]` and `for ch in s` go through here so they cannot drift apart.
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
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Load8U(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    func.instruction(&Instruction::LocalSet(byte));

    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(one));
    emit_alloc_string(func, ctx, one, blk);
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(byte));
    func.instruction(&Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(1));
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

    // No digits, or a trailing underscore, is a ValueError too.
    func.instruction(&Instruction::LocalGet(bad));
    func.instruction(&Instruction::LocalGet(digits));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::LocalGet(last_us));
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_raise(func, ctx, "ValueError", 1);
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
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::LocalGet(h_len));
        func.instruction(&Instruction::I32GeU);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(h_off));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Load8U(byte_arg()));
        func.instruction(&Instruction::LocalSet(b));
        emit_byte_in_class(func, b, ClassMode::Space);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
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
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Load8U(byte_arg()));
        func.instruction(&Instruction::LocalSet(b));
        emit_byte_in_class(func, b, ClassMode::Space);
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
fn emit_string_pad(func: &mut Function, ctx: &CompilationContext, mode: PadMode) {
    let off = ctx.temp_local;
    let len = ctx.temp_local + 1;
    let width = ctx.temp_local + 2;
    let newlen = ctx.temp_local + 3;
    let blk = ctx.temp_local + 4;
    let lead = ctx.temp_local + 5;
    let i = ctx.temp_local + 6;

    func.instruction(&Instruction::LocalSet(width));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));

    // newlen = max(len, width)
    func.instruction(&Instruction::LocalGet(width));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::LocalGet(width));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalSet(newlen));

    emit_alloc_string(func, ctx, newlen, blk);

    // Fill the whole block with spaces, then drop the text at its offset.
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(newlen));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(b' ' as i32));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    match mode {
        PadMode::Left => {
            func.instruction(&Instruction::I32Const(0));
        }
        PadMode::Right => {
            func.instruction(&Instruction::LocalGet(newlen));
            func.instruction(&Instruction::LocalGet(len));
            func.instruction(&Instruction::I32Sub);
        }
        PadMode::Center => {
            // Python's center() puts the odd space on the right.
            func.instruction(&Instruction::LocalGet(newlen));
            func.instruction(&Instruction::LocalGet(len));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Const(2));
            func.instruction(&Instruction::I32DivU);
        }
    }
    func.instruction(&Instruction::LocalSet(lead));

    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(lead));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(len));
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
        IRType::List(_) | IRType::Dict(_, _) | IRType::Set(_) | IRType::Tuple(_) => {
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
                (IRType::List(inner), _) => (**inner).clone(),
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
        IRExpr::BinaryOp { left, right, .. } => {
            key_needs_param_type(left, param) || key_needs_param_type(right, param)
        }
        IRExpr::TupleLiteral(items) | IRExpr::ListLiteral(items) => {
            items.iter().any(|i| key_needs_param_type(i, param))
        }
        _ => false,
    }
}

/// Render an `f64` with a fixed number of decimal places, as `f"{x:.2f}"` does.
///
/// Entry stack: the value (an `f64`) and the precision (an `i32` literal, which
/// the lowering guarantees). Exit stack: the string's `(offset, length)`.
///
/// The integer and fractional parts are handled separately so no 64-bit
/// arithmetic is needed: the fraction is scaled by `10^p` and rounded to
/// nearest with ties to even, which is what IEEE-754 and CPython both do, and
/// a fraction that rounds up to a whole carries into the integer part. A
/// magnitude too large for an `i32` traps rather than printing something
/// wrong.
fn emit_format_fixed(func: &mut Function, ctx: &CompilationContext, precision: u32) {
    let value = ctx.temp_local_f64;
    let ipart = ctx.temp_local_f64_2;
    let frac = ctx.temp_local_f64_3;
    let neg = ctx.temp_local + 32;
    let blk = ctx.temp_local + 33;
    let pos = ctx.temp_local + 34;
    let int_i = ctx.temp_local + 35;
    let frac_i = ctx.temp_local + 36;
    let digit = ctx.temp_local + 37;
    let k = ctx.temp_local + 38;
    let len = ctx.temp_local + 39;

    let scale = 10f64.powi(precision as i32);

    func.instruction(&Instruction::LocalSet(value));

    // neg = value < 0; work with the magnitude from here on.
    func.instruction(&Instruction::LocalGet(value));
    func.instruction(&Instruction::F64Const(f64_const(0.0)));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(neg));
    func.instruction(&Instruction::LocalGet(value));
    func.instruction(&Instruction::F64Abs);
    func.instruction(&Instruction::LocalSet(value));

    if precision == 0 {
        // No fraction digit is kept, so there is none whose parity could break
        // a tie: the digit being rounded into is the integer part's last one.
        // Splitting the value and rounding the fraction alone loses that, and
        // `F64Nearest(0.5)` is 0 whatever sits to its left, so `3.5` came out
        // as `3` where CPython answers `4`. `2.5` and `4.5` were right only
        // because their integer part was already even. Rounding the whole
        // magnitude keeps the tie-break on the digit that decides it.
        func.instruction(&Instruction::LocalGet(value));
        func.instruction(&Instruction::F64Nearest);
        func.instruction(&Instruction::LocalSet(ipart));
        func.instruction(&Instruction::F64Const(f64_const(0.0)));
        func.instruction(&Instruction::LocalSet(frac));
    } else {
        // ipart = floor(value); frac = round((value - ipart) * 10^p). Here the
        // last kept fraction digit is inside the scaled value, so
        // `F64Nearest`'s ties-to-even lands on the right digit.
        func.instruction(&Instruction::LocalGet(value));
        func.instruction(&Instruction::F64Floor);
        func.instruction(&Instruction::LocalSet(ipart));
        func.instruction(&Instruction::LocalGet(value));
        func.instruction(&Instruction::LocalGet(ipart));
        func.instruction(&Instruction::F64Sub);
        func.instruction(&Instruction::F64Const(f64_const(scale)));
        func.instruction(&Instruction::F64Mul);
        func.instruction(&Instruction::F64Nearest);
        func.instruction(&Instruction::LocalSet(frac));

        // A fraction that rounded up to a whole carries.
        func.instruction(&Instruction::LocalGet(frac));
        func.instruction(&Instruction::F64Const(f64_const(scale)));
        func.instruction(&Instruction::F64Ge);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::F64Const(f64_const(0.0)));
        func.instruction(&Instruction::LocalSet(frac));
        func.instruction(&Instruction::LocalGet(ipart));
        func.instruction(&Instruction::F64Const(f64_const(1.0)));
        func.instruction(&Instruction::F64Add);
        func.instruction(&Instruction::LocalSet(ipart));
        func.instruction(&Instruction::End);
    }

    func.instruction(&Instruction::LocalGet(ipart));
    func.instruction(&Instruction::I32TruncF64S);
    func.instruction(&Instruction::LocalSet(int_i));
    func.instruction(&Instruction::LocalGet(frac));
    func.instruction(&Instruction::I32TruncF64S);
    func.instruction(&Instruction::LocalSet(frac_i));

    // A 32-byte scratch block, filled from the right so the digits come out in
    // order, then shuffled to the front.
    func.instruction(&Instruction::I32Const(4 + 32 + 1));
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(blk));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(blk));
    func.instruction(&Instruction::I32Const(32));
    func.instruction(&Instruction::LocalSet(pos));

    if precision > 0 {
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(k));
        func.instruction(&Instruction::Block(BlockType::Empty));
        func.instruction(&Instruction::Loop(BlockType::Empty));
        func.instruction(&Instruction::LocalGet(k));
        func.instruction(&Instruction::I32Const(precision as i32));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::BrIf(1));
        func.instruction(&Instruction::LocalGet(frac_i));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32RemS);
        func.instruction(&Instruction::LocalSet(digit));
        func.instruction(&Instruction::LocalGet(frac_i));
        func.instruction(&Instruction::I32Const(10));
        func.instruction(&Instruction::I32DivS);
        func.instruction(&Instruction::LocalSet(frac_i));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(blk));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalGet(digit));
        func.instruction(&Instruction::I32Const(b'0' as i32));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Store8(byte_arg()));
        func.instruction(&Instruction::LocalGet(k));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(k));
        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);

        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(pos));
        func.instruction(&Instruction::LocalGet(blk));
        func.instruction(&Instruction::LocalGet(pos));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(b'.' as i32));
        func.instruction(&Instruction::I32Store8(byte_arg()));
    }

    // Integer digits, at least one so zero prints as "0".
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(int_i));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32RemS);
    func.instruction(&Instruction::LocalSet(digit));
    func.instruction(&Instruction::LocalGet(int_i));
    func.instruction(&Instruction::I32Const(10));
    func.instruction(&Instruction::I32DivS);
    func.instruction(&Instruction::LocalSet(int_i));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(digit));
    func.instruction(&Instruction::I32Const(b'0' as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(int_i));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(neg));
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(pos));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(b'-' as i32));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::End);

    // Shuffle the digits to the front of the block and finish the header.
    func.instruction(&Instruction::I32Const(32));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(pos));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::I32Const(4));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Store8(byte_arg()));
    func.instruction(&Instruction::LocalGet(blk));
    func.instruction(&Instruction::LocalGet(len));
}

/// MemArg for a single-byte access. A byte's natural alignment is 1, so the
/// alignment hint must be 0; anything larger is rejected by the validator.
fn byte_arg() -> MemArg {
    MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }
}

fn mem_off(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
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

        // target += step
        func.instruction(&Instruction::LocalGet(target));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(8)));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(target));

        func.instruction(&Instruction::Br(0));
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
                IRType::List(inner) => (**inner).clone(),
                _ => IRType::Unknown,
            };
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

        func.instruction(&Instruction::LocalGet(idx));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(idx));

        func.instruction(&Instruction::Br(0));
        func.instruction(&Instruction::End);
        func.instruction(&Instruction::End);
    }
}

/// Insert the element on top of the stack into the runtime-built set hash
/// table at local `res` (dedup on insert). Mirrors the set-literal insertion,
/// but the table pointer and `cap - 1` mask are runtime locals instead of
/// compile-time constants. Bumps the member count at `res[0]` only when a new
/// bucket is occupied.
fn emit_runtime_set_insert(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_ty: &IRType,
    res: u32,
    mask: u32,
    hidx: u32,
    bkt: u32,
) {
    stash_search_needle(func, ctx, elem_ty, ctx.temp_local + 1);
    emit_stashed_set_insert(func, ctx, elem_ty, res, mask, hidx, bkt);
}

/// The insertion half of [`emit_runtime_set_insert`], for callers that have
/// already stashed the needle (`s.add(v)`, which must survive a rehash, and the
/// rehash itself, which re-inserts values read straight out of the old
/// buckets rather than off the stack).
fn emit_stashed_set_insert(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_ty: &IRType,
    res: u32,
    mask: u32,
    hidx: u32,
    bkt: u32,
) {
    let needle = ctx.temp_local + 1;
    emit_set_hash(func, ctx, elem_ty, needle);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(hidx));

    func.instruction(&Instruction::Block(BlockType::Empty)); // $done
    func.instruction(&Instruction::Loop(BlockType::Empty)); // $probe

    // bkt = buckets(res) + hidx*SET_BUCKET
    func.instruction(&Instruction::LocalGet(res));
    emit_set_base(func);
    func.instruction(&Instruction::LocalGet(hidx));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(bkt));

    // Empty bucket? -> occupy it, bump count and used, exit. Tombstones are
    // probed past rather than reused: reuse would need a second pass to prove
    // the value is not already further along the chain, and the table is
    // compacted by the next rehash anyway.
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    store_stashed_needle(func, ctx, elem_ty, needle);
    for offset in [0, SET_USED] {
        func.instruction(&Instruction::LocalGet(res));
        func.instruction(&Instruction::LocalGet(res));
        func.instruction(&Instruction::I32Load(mem_off(offset as u64)));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Store(mem_off(offset as u64)));
    }
    func.instruction(&Instruction::Br(2)); // $done
    func.instruction(&Instruction::End);

    // Occupied by the same value? -> duplicate, exit. A tombstone never
    // matches, so its stale value cannot resurrect a removed member.
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    emit_slot_eq_needle(func, ctx, elem_ty, needle);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Br(2)); // $done
    func.instruction(&Instruction::End);

    // Collision: advance and re-probe.
    func.instruction(&Instruction::LocalGet(hidx));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(hidx));
    func.instruction(&Instruction::Br(0)); // $probe

    func.instruction(&Instruction::End); // loop
    func.instruction(&Instruction::End); // block
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

    if matches!(ret, IRType::String | IRType::Bytes) {
        recover_str_pair(func, ctx);
    }
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
        let mut current = ctx
            .get_class_info(class_name)
            .and_then(|ci| ci.base.clone());
        while let Some(base) = current {
            if let Some((_, name)) = arms.iter().find(|(t, _)| *t == IRType::Class(base.clone())) {
                return Some(name);
            }
            current = ctx.get_class_info(&base).and_then(|ci| ci.base.clone());
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
) {
    let len = ctx.temp_local + INDEX_LEN;
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(len));
    emit_index_check(func, ctx, idx, len);
}

/// Turn a (pointer, index) pair on the stack into the address of that element,
/// with the index normalized and bounds-checked first. Used by list and tuple
/// indexing, whose layouts are identical.
fn emit_sequence_address(func: &mut Function, ctx: &CompilationContext) {
    let idx = ctx.temp_local;
    let ptr = ctx.temp_local + INDEX_PTR;
    let len = ctx.temp_local + INDEX_LEN;
    func.instruction(&Instruction::LocalSet(idx));
    func.instruction(&Instruction::LocalSet(ptr));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalSet(len));
    emit_index_check(func, ctx, idx, len);

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
fn emit_zero_division_guard(func: &mut Function, ctx: &CompilationContext, is_float: bool) {
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
    emit_raise(func, ctx, "ZeroDivisionError", 1);
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
pub(crate) fn emit_index_check(func: &mut Function, ctx: &CompilationContext, idx: u32, len: u32) {
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
    emit_raise(func, ctx, "IndexError", 1);
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
            let mask = comp_local(ctx, comp_local_name("mask", depth));
            let hidx = comp_local(ctx, comp_local_name("hidx", depth));

            // cap2 = 1 << (32 - clz(2*cap + 1)): the smallest power of two
            // strictly greater than 2*cap (load factor < 0.5, never zero), so
            // probing always terminates. `hidx` temporarily holds cap2.
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Const(32));
            func.instruction(&Instruction::LocalGet(cap));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Shl);
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Or);
            func.instruction(&Instruction::I32Clz);
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Shl);
            func.instruction(&Instruction::LocalSet(hidx));

            // res = __alloc(SET_HEADER + cap2*SET_BUCKET); store cap2 and the
            // bucket-block pointer.
            func.instruction(&Instruction::LocalGet(hidx));
            func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Const(SET_HEADER as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(res));
            func.instruction(&Instruction::LocalGet(res));
            func.instruction(&Instruction::LocalGet(hidx));
            func.instruction(&Instruction::I32Store(mem_off(SET_CAP as u64)));
            store_set_data_ptr(func, res);
            func.instruction(&Instruction::LocalGet(hidx));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(mask));
        }
    }

    // --- Fill phase ------------------------------------------------------------
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(widx));

    let mut elem_ty = IRType::Unknown;
    let mut val_ty = IRType::Unknown;
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
                    let mask = comp_local(ctx, comp_local_name("mask", depth));
                    let hidx = comp_local(ctx, comp_local_name("hidx", depth));
                    let bkt = comp_local(ctx, comp_local_name("bkt", depth));
                    let t = emit_expr(element, func, ctx, memory_layout, None);
                    emit_runtime_set_insert(func, ctx, &t, res, mask, hidx, bkt);
                    *elem_ty = t;
                }
            },
        );
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

                    // Cast to i32 if an integer is expected
                    if let Some(IRType::Int) = expected_type {
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
                    // Get the string's offset in memory
                    let offset = memory_layout.string_offsets.get(s).unwrap_or(&0); // Default to offset 0 if not found

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
                IRConstant::List(_) => {
                    // Temporary implementation - return a default list
                    func.instruction(&Instruction::I32Const(0));
                    IRType::List(Box::new(IRType::Unknown))
                }
                IRConstant::Dict(_) => {
                    // Temporary implementation - return a default dict
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Dict(Box::new(IRType::Unknown), Box::new(IRType::Unknown))
                }
                IRConstant::Tuple(_) => {
                    // Temporary implementation - return a default value
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Tuple(vec![IRType::Unknown])
                }
                IRConstant::Bytes(b) => {
                    // Get the bytes' offset in memory
                    let offset = memory_layout.bytes_offsets.get(b).unwrap_or(&0);

                    // Push the bytes' memory offset and length onto the stack
                    func.instruction(&Instruction::I32Const(*offset as i32));
                    func.instruction(&Instruction::I32Const(b.len() as i32));

                    IRType::Bytes
                }
                IRConstant::Set(_) => {
                    // Set stored as identifier (set_id)
                    // TODO: Proper set implementation with element storage
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Set(Box::new(IRType::Unknown))
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
            } else {
                // Unknown variable
                func.instruction(&Instruction::I32Const(-999));
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
                    IROp::Mod
                        if right_type == IRType::String
                            || right_type == IRType::Int
                            || right_type == IRType::Float =>
                    {
                        // String formatting: "format %s" % (value,) or "format %s" % value
                        // TODO: Implement string formatting with placeholders
                        // For now, drop the right value and return the format string
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        return IRType::String;
                    }
                    _ => {}
                }
            }

            // Handle datetime arithmetic operations
            // datetime + timedelta -> datetime
            // datetime - timedelta -> datetime
            // datetime - datetime -> timedelta
            // date + timedelta -> date
            // date - timedelta -> date
            // date - date -> timedelta (days only)
            if left_type == IRType::Datetime
                || left_type == IRType::Date
                || left_type == IRType::Timedelta
            {
                match op {
                    IROp::Add => {
                        // datetime/date + timedelta
                        if left_type == IRType::Datetime && right_type == IRType::Timedelta {
                            // Stack: [dt: 7 i32s][td: 3 i32s]
                            // For compile-time simplicity, just keep the datetime unchanged
                            // Drop the timedelta values
                            func.instruction(&Instruction::Drop); // microseconds
                            func.instruction(&Instruction::Drop); // seconds
                            func.instruction(&Instruction::Drop); // days
                            return IRType::Datetime;
                        }
                        if left_type == IRType::Date && right_type == IRType::Timedelta {
                            // Stack: [date: 3 i32s][td: 3 i32s]
                            // Drop the timedelta values
                            func.instruction(&Instruction::Drop); // microseconds
                            func.instruction(&Instruction::Drop); // seconds
                            func.instruction(&Instruction::Drop); // days
                            return IRType::Date;
                        }
                        if left_type == IRType::Timedelta && right_type == IRType::Timedelta {
                            // timedelta + timedelta -> timedelta
                            // Stack: [td1: days, seconds, microseconds][td2: days, seconds, microseconds]
                            // Save td2
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // td2.microseconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // td2.seconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // td2.days
                                                                                      // Save td1
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 5)); // td1.microseconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // td1.seconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // td1.days
                                                                                          // Add: days
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local));
                            func.instruction(&Instruction::I32Add);
                            // Add: seconds
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::I32Add);
                            // Add: microseconds
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                            func.instruction(&Instruction::I32Add);
                            return IRType::Timedelta;
                        }
                    }
                    IROp::Sub => {
                        // datetime - timedelta -> datetime
                        if left_type == IRType::Datetime && right_type == IRType::Timedelta {
                            func.instruction(&Instruction::Drop); // microseconds
                            func.instruction(&Instruction::Drop); // seconds
                            func.instruction(&Instruction::Drop); // days
                            return IRType::Datetime;
                        }
                        // datetime - datetime -> timedelta
                        if left_type == IRType::Datetime && right_type == IRType::Datetime {
                            // Drop both datetimes and return a zero timedelta
                            for _ in 0..14 {
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::I32Const(0)); // days
                            func.instruction(&Instruction::I32Const(0)); // seconds
                            func.instruction(&Instruction::I32Const(0)); // microseconds
                            return IRType::Timedelta;
                        }
                        // date - timedelta -> date
                        if left_type == IRType::Date && right_type == IRType::Timedelta {
                            func.instruction(&Instruction::Drop); // microseconds
                            func.instruction(&Instruction::Drop); // seconds
                            func.instruction(&Instruction::Drop); // days
                            return IRType::Date;
                        }
                        // date - date -> timedelta
                        if left_type == IRType::Date && right_type == IRType::Date {
                            // Drop both dates and return a zero timedelta
                            for _ in 0..6 {
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::I32Const(0)); // days
                            func.instruction(&Instruction::I32Const(0)); // seconds
                            func.instruction(&Instruction::I32Const(0)); // microseconds
                            return IRType::Timedelta;
                        }
                        // timedelta - timedelta -> timedelta
                        if left_type == IRType::Timedelta && right_type == IRType::Timedelta {
                            // Save td2
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // td2.microseconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // td2.seconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // td2.days
                                                                                      // Save td1
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 5)); // td1.microseconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // td1.seconds
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // td1.days
                                                                                          // Sub: days
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local));
                            func.instruction(&Instruction::I32Sub);
                            // Sub: seconds
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::I32Sub);
                            // Sub: microseconds
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                            func.instruction(&Instruction::I32Sub);
                            return IRType::Timedelta;
                        }
                    }
                    _ => {}
                }
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
                            emit_zero_division_guard(func, ctx, true);
                        }
                        func.instruction(&Instruction::F64Div);
                    }
                    IROp::Mod => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, true);
                        }
                        emit_float_modulo_operation(func, ctx);
                    }
                    IROp::FloorDiv => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, true);
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
                            emit_zero_division_guard(func, ctx, false);
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
                            emit_zero_division_guard(func, ctx, false);
                        }
                        emit_floor_mod_i32(func, ctx);
                    }
                    IROp::FloorDiv => {
                        if !divisor_is_never_zero(right) {
                            emit_zero_division_guard(func, ctx, false);
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

            // Cast the result to expected type if needed
            if let Some(expected) = expected_type {
                if *expected == IRType::Int && result_type == IRType::Float {
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
            let operand_type = emit_expr(operand, func, ctx, memory_layout, None);

            match operand_type {
                IRType::Float => {
                    match op {
                        IRUnaryOp::Neg => {
                            // Negate float: -x
                            func.instruction(&Instruction::F64Const(f64_const(-1.0)));
                            func.instruction(&Instruction::F64Mul);
                        }
                        IRUnaryOp::Not => {
                            // Logical not for float: convert to bool first
                            func.instruction(&Instruction::F64Const(f64_const(0.0)));
                            func.instruction(&Instruction::F64Eq);
                            // Invert (1->0, 0->1)
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Xor);
                        }
                        IRUnaryOp::Invert => {
                            // Not meaningful for floats
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
                    || matches!(container_type, IRType::List(_) | IRType::Tuple(_));

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

                // Stack: (container_ptr); the needle is already stashed.
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // container_ptr
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // found

                if is_set {
                    // Hash-table membership: probe from the home bucket until the
                    // value is found or an empty bucket is reached.
                    let mask = ctx.temp_local + 2;
                    let idx = ctx.temp_local + 3;
                    let bucket = ctx.temp_local + 5;
                    let probes = ctx.temp_local + 6;

                    // mask = cap - 1
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Const(SET_CAP as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::LocalSet(mask));
                    // idx = hash(needle) & mask
                    emit_set_hash(func, ctx, &elem_type, ctx.temp_local + 1);
                    func.instruction(&Instruction::LocalGet(mask));
                    func.instruction(&Instruction::I32And);
                    func.instruction(&Instruction::LocalSet(idx));
                    // probes = 0
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalSet(probes));

                    func.instruction(&Instruction::Block(BlockType::Empty));
                    func.instruction(&Instruction::Loop(BlockType::Empty));
                    // Examined every bucket without a match -> stop.
                    func.instruction(&Instruction::LocalGet(probes));
                    func.instruction(&Instruction::LocalGet(mask));
                    func.instruction(&Instruction::I32GtU);
                    func.instruction(&Instruction::BrIf(1));
                    // bucket = buckets(container_ptr) + idx*SET_BUCKET
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    emit_set_base(func);
                    func.instruction(&Instruction::LocalGet(idx));
                    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(bucket));
                    // Empty bucket -> not present, stop.
                    func.instruction(&Instruction::LocalGet(bucket));
                    func.instruction(&Instruction::I32Load(slot_arg())); // state
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::Br(2));
                    func.instruction(&Instruction::End);
                    // A live bucket holding the value -> found, stop. The state
                    // test matters: a tombstone keeps the removed member's
                    // value, so comparing without it would report a member the
                    // program has already discarded.
                    func.instruction(&Instruction::LocalGet(bucket));
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::I32Const(SET_LIVE));
                    func.instruction(&Instruction::I32Eq);
                    func.instruction(&Instruction::LocalGet(bucket));
                    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
                    func.instruction(&Instruction::I32Add);
                    emit_slot_eq_needle(func, ctx, &elem_type, ctx.temp_local + 1);
                    func.instruction(&Instruction::I32And);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // found
                    func.instruction(&Instruction::Br(2));
                    func.instruction(&Instruction::End);
                    // idx = (idx + 1) & mask; probes += 1
                    func.instruction(&Instruction::LocalGet(idx));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalGet(mask));
                    func.instruction(&Instruction::I32And);
                    func.instruction(&Instruction::LocalSet(idx));
                    func.instruction(&Instruction::LocalGet(probes));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalSet(probes));
                    func.instruction(&Instruction::Br(0));
                    func.instruction(&Instruction::End); // loop
                    func.instruction(&Instruction::End); // block
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
                    emit_slot_eq_needle(func, ctx, &elem_type, ctx.temp_local + 1);
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
                if matches!(op, IRCompareOp::NotIn) {
                    func.instruction(&Instruction::I32Eqz);
                }
                return IRType::Bool;
            }

            let left_type = emit_expr(left, func, ctx, memory_layout, None);
            let right_type = emit_expr(right, func, ctx, memory_layout, Some(&left_type));

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
                    IRType::List(_) | IRType::Tuple(_) | IRType::Dict(_, _) | IRType::Set(_)
                )
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
                    // New operations
                    IRCompareOp::In | IRCompareOp::NotIn | IRCompareOp::Is | IRCompareOp::IsNot => {
                        // These comparisons aren't directly supported for floats in WebAssembly
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
                    // New operations
                    IRCompareOp::In | IRCompareOp::NotIn | IRCompareOp::Is | IRCompareOp::IsNot => {
                        // These operations aren't directly supported in WebAssembly
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
                    emit_expr(right, func, ctx, memory_layout, Some(&IRType::Bool));
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
                    emit_expr(right, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::End);
                }
            }

            IRType::Bool
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

            // Iterator-protocol intrinsic (see `ir::generators`): leave the
            // current StopIteration flag (global 1) on the stack and clear it.
            if function_name == crate::ir::STOP_CHECK_FN {
                func.instruction(&Instruction::GlobalGet(1));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::GlobalSet(1));
                return IRType::Bool;
            }

            // Positional dict-entry access backing `for k, v in d.items()`
            // (see `ir::converter`): entry i's key sits at
            // HEADER + i*DICT_ENTRY, its value one slot later. Loaded as the
            // i32 slot word (f64 values keep only their low word — the
            // existing tuple-unpack limitation).
            if function_name == crate::ir::DICT_KEY_AT_FN
                || function_name == crate::ir::DICT_VAL_AT_FN
            {
                if let [dict, index] = arguments.as_slice() {
                    emit_expr(dict, func, ctx, memory_layout, None);
                    emit_expr(index, func, ctx, memory_layout, Some(&IRType::Int));
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Add);
                    let slot = if function_name == crate::ir::DICT_VAL_AT_FN {
                        COLLECTION_SLOT
                    } else {
                        0
                    };
                    func.instruction(&Instruction::I32Load(MemArg {
                        offset: (COLLECTION_HEADER + slot) as u64,
                        align: 2,
                        memory_index: 0,
                    }));
                } else {
                    func.instruction(&Instruction::I32Const(0));
                }
                return IRType::Int;
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
            // argument code is emitted at all. Handled before the generic
            // argument emission below, which would treat the class names as
            // unknown variables.
            if function_name == "issubclass" {
                if let (Some(IRExpr::Variable(sub)), Some(IRExpr::Variable(base))) =
                    (arguments.first(), arguments.get(1))
                {
                    if ctx.get_class_info(sub).is_some() && ctx.get_class_info(base).is_some() {
                        let result = ctx.is_class_or_subclass(sub, base);
                        func.instruction(&Instruction::I32Const(result as i32));
                        return IRType::Bool;
                    }
                }
                func.instruction(&Instruction::I32Const(0));
                return IRType::Bool;
            }

            // `isinstance(obj, ClassName)` — the second argument is a bare
            // class-name token (never emitted); the first is evaluated and, if
            // it is a class instance, its tag word (class id at offset 0,
            // stamped by `__alloc_obj`) is compared against the ids assignable
            // to `ClassName` (itself plus every subclass). Also handled before
            // the generic argument emission.
            if function_name == "isinstance" {
                if let (Some(obj), Some(IRExpr::Variable(target))) =
                    (arguments.first(), arguments.get(1))
                {
                    if ctx.get_class_info(target).is_some() {
                        let obj_type = emit_expr(obj, func, ctx, memory_layout, None);
                        return match obj_type {
                            IRType::Class(_) => {
                                // tag = *(obj + 0); fold `tag == id` over the
                                // assignable ids with `or`. The tag sits in a
                                // scratch local only while the flat comparison
                                // chain is emitted (no nested emit_expr).
                                let ids = ctx.assignable_class_ids(target);
                                func.instruction(&Instruction::I32Load(MemArg {
                                    offset: 0,
                                    align: 2,
                                    memory_index: 0,
                                }));
                                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                                func.instruction(&Instruction::I32Const(0));
                                for id in ids {
                                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                                    func.instruction(&Instruction::I32Const(id));
                                    func.instruction(&Instruction::I32Eq);
                                    func.instruction(&Instruction::I32Or);
                                }
                                IRType::Bool
                            }
                            // A non-instance value is never an instance of a
                            // user class: discard it and answer False.
                            IRType::String | IRType::Bytes => {
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                                IRType::Bool
                            }
                            IRType::Float => {
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                                IRType::Bool
                            }
                            _ => {
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                                IRType::Bool
                            }
                        };
                    }
                }
                // Unknown target (e.g. `isinstance(x, int)`): not supported
                // yet; answer False without emitting the arguments.
                func.instruction(&Instruction::I32Const(0));
                return IRType::Bool;
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
                let is_closure_callee = ctx.get_local_index(function_name).is_some()
                    || ctx.get_module_var(function_name).is_some();
                if is_closure_callee {
                    if let Some(type_base) = ctx.closure_type_base {
                        if arguments.len() as u32 <= ctx.closure_max_arity {
                            for arg in arguments {
                                let t = emit_expr(arg, func, ctx, memory_layout, None);
                                // Closure parameters are single i32 words.
                                match t {
                                    IRType::String | IRType::Bytes => {
                                        func.instruction(&Instruction::Drop);
                                    }
                                    IRType::Float => {
                                        func.instruction(&Instruction::I32TruncF64S);
                                    }
                                    _ => {}
                                }
                            }
                            // Environment pointer: last argument, then reused
                            // to load the table slot from its first word.
                            emit_expr(
                                &IRExpr::Variable(function_name.clone()),
                                func,
                                ctx,
                                memory_layout,
                                None,
                            );
                            func.instruction(&Instruction::LocalTee(ctx.temp_local));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local));
                            func.instruction(&Instruction::I32Load(slot_arg()));
                            func.instruction(&Instruction::CallIndirect {
                                type_index: type_base + arguments.len() as u32,
                                table_index: 0,
                            });
                            return IRType::Unknown;
                        }
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
                    if matches!(t, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                }
                emit_user_call(func, ctx, index);
                if matches!(return_type, IRType::String | IRType::Bytes) {
                    recover_str_pair(func, ctx);
                }
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
            let is_user_fn = ctx.get_function_info(function_name.as_str()).is_some();
            let mut arg_types = Vec::new();
            for arg in arguments {
                let arg_type = emit_expr(arg, func, ctx, memory_layout, None);
                if is_user_fn && matches!(arg_type, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop);
                }
                arg_types.push(arg_type);
            }

            // Look up the function index if it exists in our context
            if let Some(func_info) = ctx.get_function_info(function_name.as_str()) {
                let return_type = func_info.return_type.clone();
                emit_user_call(func, ctx, func_info.index);
                // A function returns a single word; a string/bytes result is
                // its offset, so rebuild the (offset, length) pair consumers
                // expect from the blob prefix.
                if matches!(return_type, IRType::String | IRType::Bytes) {
                    recover_str_pair(func, ctx);
                }
                return_type
            } else {
                // Built-in functions
                match function_name.as_str() {
                    "len" => {
                        if arg_types.len() != 1 {
                            return IRType::Unknown;
                        }
                        match &arg_types[0] {
                            IRType::String | IRType::Bytes => {
                                // (offset, length) on stack with length on top.
                                // Keep the length, discard the offset below it.
                                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                                IRType::Int
                            }
                            IRType::Float => {
                                // len() of a scalar is invalid Python; consume
                                // the value and answer 0.
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::I32Const(0));
                                IRType::Int
                            }
                            // Lists, dicts, sets and tuples are all pointers
                            // that store their element/entry count in the
                            // first 4 bytes. An Unknown value (e.g. a
                            // collection read back out of an instance field)
                            // is a single i32 word, so treating it as such a
                            // pointer is the correct default.
                            _ => {
                                func.instruction(&Instruction::I32Load(MemArg {
                                    offset: 0,
                                    align: 2,
                                    memory_index: 0,
                                }));
                                IRType::Int
                            }
                        }
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
                    "min" => {
                        if arg_types.is_empty() {
                            return IRType::Unknown;
                        }
                        if arg_types.len() == 1 {
                            // min(iterable) - not yet supported, requires iteration
                            // For now, just pop the argument and return 0
                            func.instruction(&Instruction::Drop);
                            return IRType::Int;
                        }
                        // min(a, b, ...) - fold the args (top of stack down) into
                        // a running minimum. Each step replaces the top two with
                        // their minimum via a result-typed if.
                        let result_type = arg_types[0].clone();
                        for _ in 1..arg_types.len() {
                            // Stack: ..., running, next
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // next
                            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // running
                            func.instruction(&Instruction::LocalGet(ctx.temp_local));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::I32LtS); // running < next
                            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local)); // keep running
                            func.instruction(&Instruction::Else);
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // keep next
                            func.instruction(&Instruction::End);
                        }
                        result_type
                    }
                    "max" => {
                        if arg_types.is_empty() {
                            return IRType::Unknown;
                        }
                        if arg_types.len() == 1 {
                            // max(iterable) - not yet supported, requires iteration
                            // For now, just pop the argument and return 0
                            func.instruction(&Instruction::Drop);
                            return IRType::Int;
                        }
                        // max(a, b, ...) - fold the args into a running maximum.
                        let result_type = arg_types[0].clone();
                        for _ in 1..arg_types.len() {
                            // Stack: ..., running, next
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // next
                            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // running
                            func.instruction(&Instruction::LocalGet(ctx.temp_local));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::I32GtS); // running > next
                            func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local)); // keep running
                            func.instruction(&Instruction::Else);
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // keep next
                            func.instruction(&Instruction::End);
                        }
                        result_type
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
                                // Parsing a decimal string into the double
                                // CPython would produce needs correct rounding,
                                // which a digit loop does not give. This used to
                                // convert the string's length and strand its
                                // offset, so it is refused rather than
                                // approximated.
                                ctx.report(
                                    "float() of a string is not supported yet. Hint: parse the \
                                     integer and fractional parts with int() and combine them",
                                );
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::F64Const(0.0.into()));
                            }
                            _ => {
                                func.instruction(&Instruction::F64ConvertI32S);
                            }
                        }
                        IRType::Float
                    }
                    "str" => {
                        // str(x): strings/bytes pass through as their existing
                        // (offset, length) pair; ints and bools render their
                        // decimal digits at runtime via `__i32_to_str` (bools
                        // as 0/1). Floats have no runtime formatter yet, so
                        // they yield an empty string rather than invalid WASM.
                        match arg_types.first() {
                            Some(IRType::String) | Some(IRType::Bytes) => IRType::String,
                            // Ints render their decimal digits at runtime. A
                            // value whose type codegen could not resolve is a
                            // single i32 word like any other, so it renders the
                            // same way rather than silently yielding "".
                            Some(IRType::Int) | Some(IRType::Unknown) => {
                                func.instruction(&Instruction::Call(ctx.i32_to_str_func_index));
                                recover_str_pair(func, ctx);
                                IRType::String
                            }
                            other => {
                                // Anything else has no runtime rendering: a
                                // bool would come out as "1"/"0" rather than
                                // Python's "True"/"False", a float needs a
                                // formatter this runtime does not have, and a
                                // collection needs its elements rendered too.
                                // Saying so beats returning an empty string.
                                let what = match other {
                                    Some(ty) => crate::type_to_string(ty),
                                    None => "nothing".to_string(),
                                };
                                ctx.report(format!(
                                    "str() of {what} is not supported yet; an f-string \
                                     placeholder renders its value through str() too. \
                                     Hint: str() renders ints and passes strings through"
                                ));
                                if matches!(other, Some(IRType::String) | Some(IRType::Bytes)) {
                                    func.instruction(&Instruction::Drop);
                                }
                                if other.is_some() {
                                    func.instruction(&Instruction::Drop);
                                }
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                IRType::String
                            }
                        }
                    }
                    "__format_fixed" => {
                        // f"{x:.2f}": the value and a literal precision are on
                        // the stack; the precision is compile-time so the digit
                        // loop can be unrolled against it.
                        let Some(IRExpr::Const(IRConstant::Int(precision))) = arguments.get(1)
                        else {
                            ctx.report("an f-string's '.Nf' precision must be a literal");
                            func.instruction(&Instruction::Unreachable);
                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::I32Const(0));
                            return IRType::String;
                        };
                        // The literal precision was pushed as a word; the
                        // formatter reads it from the instruction stream.
                        func.instruction(&Instruction::Drop);
                        if !matches!(arg_types[0], IRType::Float) {
                            // An int renders through the same path, so
                            // f"{3:.2f}" is "3.00" as Python has it.
                            func.instruction(&Instruction::F64ConvertI32S);
                        }
                        emit_format_fixed(func, ctx, *precision as u32);
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
                            IRType::List(inner) => (**inner).clone(),
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
                                if let IRExpr::ClosureMake { lambda_name, .. } = k {
                                    if let Some((param, body)) = ctx.lambda_bodies.get(lambda_name)
                                    {
                                        if key_needs_param_type(body, param) {
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
                                let inferred = infer_key_type(k, &elem_ty, ctx);
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
                            func.instruction(&Instruction::I32Load(slot_arg()));
                            func.instruction(&Instruction::LocalGet(env));
                            func.instruction(&Instruction::LocalGet(env));
                            func.instruction(&Instruction::I32Load(slot_arg()));
                            func.instruction(&Instruction::CallIndirect {
                                type_index: type_base + 1,
                                table_index: 0,
                            });
                            func.instruction(&Instruction::I32Store(slot_arg()));
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
                            IRType::List(elem) => (**elem).clone(),
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
                    "namedtuple" => {
                        // namedtuple(typename, field_names) -> class
                        // Returns a callable that creates namedtuple instances
                        // For now, just drop arguments and return a pointer
                        for arg_type in &arg_types {
                            match arg_type {
                                IRType::String => {
                                    func.instruction(&Instruction::Drop);
                                    func.instruction(&Instruction::Drop);
                                }
                                _ => {
                                    func.instruction(&Instruction::Drop);
                                }
                            }
                        }
                        // Return a callable reference (just use 0 as placeholder)
                        func.instruction(&Instruction::I32Const(0));
                        IRType::Unknown
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

            let mut elem_type = IRType::Unknown;
            let mut element_types = Vec::with_capacity(elements.len());
            for (i, elem) in elements.iter().enumerate() {
                // A store pops the value, then the address, so the block pointer
                // goes first and the slot offset rides in the store's offset.
                func.instruction(&Instruction::LocalGet(blk));
                let ty = emit_expr(elem, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &ty);
                store_collection_word_at(func, &ty, COLLECTION_HEADER + i as u32 * COLLECTION_SLOT);
                if i == 0 {
                    elem_type = ty.clone();
                }
                element_types.push(ty);
            }
            ctx.release_held();
            check_uniform_slot_width(ctx, "list literal", &element_types);
            func.instruction(&Instruction::LocalGet(blk));
            IRType::List(Box::new(elem_type))
        }
        IRExpr::SetLiteral(elements) => {
            // Build the set as an open-addressing hash table (see the SET_*
            // helpers): dedup-on-insert and `in` are constant time instead of
            // linear scans. `cap` is a compile-time power of two >= 2*len, so a
            // probe always meets an empty bucket.
            let cap = set_capacity(elements.len());
            let mask = (cap - 1) as i32;
            let set_size = SET_HEADER + cap * SET_BUCKET;
            // Built into a fresh block per evaluation; see the list literal.
            let Some(blk) = ctx.hold() else {
                report_literal_too_deep(func, ctx, "set");
                return IRType::Set(Box::new(IRType::Unknown));
            };
            emit_literal_block(func, ctx, blk, set_size);

            // A fresh block is zeroed, so every bucket starts empty (state 0)
            // and count and used are 0. Store the capacity and point the bucket
            // pointer at the block right after the header.
            func.instruction(&Instruction::LocalGet(blk));
            func.instruction(&Instruction::I32Const(cap as i32));
            func.instruction(&Instruction::I32Store(mem_off(SET_CAP as u64)));
            func.instruction(&Instruction::LocalGet(blk));
            func.instruction(&Instruction::LocalGet(blk));
            func.instruction(&Instruction::I32Const(SET_HEADER as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::I32Store(mem_off(SET_DATA as u64)));

            let idx = ctx.temp_local + 2;
            let bucket = ctx.temp_local + 3;

            let mut elem_type = IRType::Unknown;
            let mut member_types = Vec::with_capacity(elements.len());
            for (i, elem) in elements.iter().enumerate() {
                // Evaluate the element, stash it as a needle (f64 for floats), and
                // compute its home bucket: idx = hash(elem) & (cap - 1).
                let ty = emit_expr(elem, func, ctx, memory_layout, None);
                if i == 0 {
                    elem_type = ty.clone();
                }
                member_types.push(ty.clone());
                stash_search_needle(func, ctx, &ty, ctx.temp_local + 1);
                emit_set_hash(func, ctx, &ty, ctx.temp_local + 1);
                func.instruction(&Instruction::I32Const(mask));
                func.instruction(&Instruction::I32And);
                func.instruction(&Instruction::LocalSet(idx));

                // Linear-probe to the home bucket or a free one (skip on dup).
                func.instruction(&Instruction::Block(BlockType::Empty)); // $done
                func.instruction(&Instruction::Loop(BlockType::Empty)); // $probe

                // bucket = set_ptr's block + idx*SET_BUCKET, which for a fresh
                // literal is the space right after its header.
                func.instruction(&Instruction::LocalGet(blk));
                func.instruction(&Instruction::I32Const(SET_HEADER as i32));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(idx));
                func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(bucket));

                // Empty bucket? -> occupy it and bump count, then exit.
                func.instruction(&Instruction::LocalGet(bucket));
                func.instruction(&Instruction::I32Load(slot_arg())); // state
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::If(BlockType::Empty));
                // state = live
                func.instruction(&Instruction::LocalGet(bucket));
                func.instruction(&Instruction::I32Const(SET_LIVE));
                func.instruction(&Instruction::I32Store(slot_arg()));
                // value = needle (at bucket + SET_BUCKET_VALUE)
                func.instruction(&Instruction::LocalGet(bucket));
                func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
                func.instruction(&Instruction::I32Add);
                store_stashed_needle(func, ctx, &ty, ctx.temp_local + 1);
                // count += 1, used += 1
                for offset in [0, SET_USED] {
                    func.instruction(&Instruction::LocalGet(blk));
                    func.instruction(&Instruction::LocalGet(blk));
                    func.instruction(&Instruction::I32Load(mem_off(offset as u64)));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Store(mem_off(offset as u64)));
                }
                func.instruction(&Instruction::Br(2)); // $done
                func.instruction(&Instruction::End);

                // Occupied by the same value? -> already a member, exit.
                func.instruction(&Instruction::LocalGet(bucket));
                func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
                func.instruction(&Instruction::I32Add);
                emit_slot_eq_needle(func, ctx, &ty, ctx.temp_local + 1);
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::Br(2)); // $done
                func.instruction(&Instruction::End);

                // Collision: advance idx = (idx + 1) & mask and re-probe.
                func.instruction(&Instruction::LocalGet(idx));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Const(mask));
                func.instruction(&Instruction::I32And);
                func.instruction(&Instruction::LocalSet(idx));
                func.instruction(&Instruction::Br(0)); // $probe
                func.instruction(&Instruction::End); // loop
                func.instruction(&Instruction::End); // block
            }

            ctx.release_held();
            check_uniform_slot_width(ctx, "set literal", &member_types);
            func.instruction(&Instruction::LocalGet(blk));
            IRType::Set(Box::new(elem_type))
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
            check_uniform_slot_width(ctx, "tuple literal", &element_types);
            func.instruction(&Instruction::LocalGet(blk));
            IRType::Tuple(element_types)
        }
        IRExpr::DictLiteral(pairs) => {
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
                let v_type = emit_expr(val_expr, func, ctx, memory_layout, None);
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

            let key_type = key_types.first().cloned().unwrap_or(IRType::Unknown);
            let value_type = value_types.first().cloned().unwrap_or(IRType::Unknown);
            func.instruction(&Instruction::LocalGet(blk));
            IRType::Dict(Box::new(key_type), Box::new(value_type))
        }
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
            func.instruction(&Instruction::LocalGet(index));
            func.instruction(&Instruction::I32Load(mem_off(0)));
            IRType::Unknown
        }
        IRExpr::CellStore { cell, value } => {
            let Some(index) = ctx.get_local_index(cell) else {
                ctx.report(format!(
                    "closure cell '{cell}' is not in scope; this is a code generation bug"
                ));
                func.instruction(&Instruction::Unreachable);
                return IRType::None;
            };
            func.instruction(&Instruction::LocalGet(index));
            let value_type = emit_expr(value, func, ctx, memory_layout, None);
            match value_type {
                // A string or bytes value is an (offset, length) pair; the cell
                // keeps the offset, the same word a collection slot keeps.
                IRType::String | IRType::Bytes => {
                    // (offset, length) with the length on top: drop it and keep
                    // the offset, the same word a collection slot keeps.
                    func.instruction(&Instruction::Drop);
                }
                IRType::Float => {
                    ctx.report(
                        "a float captured by a closure is not supported yet. \
                         Hint: capture an int, or pass the value as an argument"
                            .to_string(),
                    );
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                }
                _ => {}
            }
            func.instruction(&Instruction::I32Store(mem_off(0)));
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
                    let idx = ctx.temp_local;
                    let off = ctx.temp_local + INDEX_PTR;
                    let len = ctx.temp_local + INDEX_LEN;
                    func.instruction(&Instruction::LocalSet(idx));
                    func.instruction(&Instruction::LocalSet(len));
                    func.instruction(&Instruction::LocalSet(off));
                    emit_index_check(func, ctx, idx, len);
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
                    emit_index_check(func, ctx, idx, len);

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
                IRType::List(element_type) => {
                    // List indexing: list is stored as [length:i32][elem0][elem1]...
                    // Stack: (list_ptr, index). Address = list_ptr + HEADER + index*SLOT.
                    emit_sequence_address(func, ctx);
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
                    emit_raise(func, ctx, "KeyError", 1);
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
                    emit_sequence_address(func, ctx);

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
                _ => {
                    // Unknown container types
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
                    // `select` pops (v1, v2, cond) and yields v1 when cond != 0.
                    let normalize_and_clamp = |func: &mut Function, bound: u32| {
                        // bound = bound < 0 ? bound + length : bound
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32LtS);
                        func.instruction(&Instruction::Select);
                        func.instruction(&Instruction::LocalSet(bound));
                        // bound = max(bound, 0)
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32GtS);
                        func.instruction(&Instruction::Select);
                        func.instruction(&Instruction::LocalSet(bound));
                        // bound = min(bound, length)
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::I32LtS);
                        func.instruction(&Instruction::Select);
                        func.instruction(&Instruction::LocalSet(bound));
                    };
                    normalize_and_clamp(func, lo);
                    normalize_and_clamp(func, hi);

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
                IRType::List(elem_type) => {
                    // List slicing: `xs[a:b]`, returning a new list.
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
                    for bound in [lo, hi] {
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32LtS);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(bound));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32LtS);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(bound));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::LocalGet(bound));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::I32GtS);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        func.instruction(&Instruction::LocalGet(len));
                        func.instruction(&Instruction::LocalSet(bound));
                        func.instruction(&Instruction::End);
                    }

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
                    IRType::List(elem_type.clone())
                }
                _ => IRType::Unknown,
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
                    // Unknown attribute on a user module: yield 0.
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
                    crate::stdlib::StdlibValue::List(_) => {
                        func.instruction(&Instruction::I32Const(10000));
                        IRType::List(Box::new(IRType::String))
                    }
                    crate::stdlib::StdlibValue::Dict(_) => {
                        func.instruction(&Instruction::I32Const(10000));
                        IRType::Dict(Box::new(IRType::String), Box::new(IRType::String))
                    }
                    crate::stdlib::StdlibValue::None => {
                        func.instruction(&Instruction::I32Const(0));
                        IRType::None
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
                    let key = format!("{class_name}.{attribute}");
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
                    let base = ctx
                        .current_class
                        .as_ref()
                        .and_then(|c| ctx.get_class_info(c))
                        .and_then(|ci| ci.base.clone());
                    let resolved = base.as_deref().and_then(|b| {
                        let ci = ctx.get_class_info(b)?;
                        let idx = ci.methods.get(method_name.as_str()).copied()?;
                        let owner = ci
                            .method_owner
                            .get(method_name.as_str())
                            .cloned()
                            .unwrap_or_else(|| b.to_string());
                        Some((idx, owner))
                    });
                    if let Some((method_idx, owner)) = resolved {
                        let (param_types, ret) = ctx
                            .get_function_info(&format!("{owner}::{method_name}"))
                            .map(|f| (f.param_types.clone(), f.return_type.clone()))
                            .unwrap_or((Vec::new(), IRType::Unknown));
                        func.instruction(&Instruction::LocalGet(0)); // self
                        for (i, arg) in arguments.iter().enumerate() {
                            let t =
                                emit_expr(arg, func, ctx, memory_layout, param_types.get(i + 1));
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
                        if matches!(ret, IRType::String | IRType::Bytes) {
                            recover_str_pair(func, ctx);
                        }
                        return ret;
                    }
                    // No base or unknown method: evaluate nothing, yield 0.
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

            // Check if this is a stdlib module method call (e.g., os.getcwd())
            if let IRExpr::Variable(module_name) = &**object {
                if crate::stdlib::is_stdlib_module(module_name) {
                    // Handle os module functions
                    if module_name == "os" {
                        if let Some(os_func) = crate::stdlib::os::get_function(method_name) {
                            return match os_func {
                                crate::stdlib::os::OsFunction::Getcwd => {
                                    // getcwd() returns current working directory as string
                                    // For WASM, return "/" as default
                                    let cwd = "/".to_string();
                                    let offset = memory_layout
                                        .string_offsets
                                        .get(&cwd)
                                        .copied()
                                        .unwrap_or(0);
                                    func.instruction(&Instruction::I32Const(offset as i32));
                                    func.instruction(&Instruction::I32Const(cwd.len() as i32));
                                    IRType::String
                                }
                                crate::stdlib::os::OsFunction::Getenv => {
                                    // getenv(key) returns environment variable value or None
                                    // For now, drop arguments and return None
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop); // Drop string (offset, length)
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::None
                                }
                                crate::stdlib::os::OsFunction::Getpid => {
                                    // getpid() returns process ID
                                    // For WASM, return fixed PID
                                    func.instruction(&Instruction::I32Const(1));
                                    IRType::Int
                                }
                                crate::stdlib::os::OsFunction::Urandom => {
                                    // urandom(n) returns n random bytes
                                    // For now, return empty bytes
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0)); // offset
                                    func.instruction(&Instruction::I32Const(0)); // length
                                    IRType::Bytes
                                }
                            };
                        }
                    }

                    // Handle json module functions
                    if module_name == "json" {
                        if let Some(json_func) = crate::stdlib::json::get_function(method_name) {
                            return match json_func {
                                crate::stdlib::json::JsonFunction::Dumps => {
                                    // json.dumps(obj) - serialize Python object to JSON string
                                    // For now, we'll handle basic types and return a JSON string
                                    // TODO: Implement full serialization for all types
                                    if arguments.is_empty() {
                                        // Return empty JSON object string
                                        let json_str = "{}".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&json_str)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(
                                            json_str.len() as i32
                                        ));
                                    } else {
                                        // Emit the argument and for now return a placeholder JSON string
                                        // In a full implementation, this would serialize the value
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);

                                        // Drop the emitted value and return placeholder
                                        // NOTE: This is a simplified implementation
                                        func.instruction(&Instruction::Drop);

                                        let json_str = "{}".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&json_str)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(
                                            json_str.len() as i32
                                        ));
                                    }
                                    IRType::String
                                }
                                crate::stdlib::json::JsonFunction::Loads => {
                                    // json.loads(s) - parse JSON string to Python object
                                    // For now, return an empty dict as placeholder
                                    // TODO: Implement full JSON parsing at runtime
                                    if !arguments.is_empty() {
                                        // Emit and drop the string argument
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }

                                    // Return empty dict placeholder
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Dict(
                                        Box::new(IRType::String),
                                        Box::new(IRType::Unknown),
                                    )
                                }
                                crate::stdlib::json::JsonFunction::Load => {
                                    // json.load(fp) - load JSON from file object
                                    // Drop file argument and return empty dict
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Dict(
                                        Box::new(IRType::String),
                                        Box::new(IRType::Unknown),
                                    )
                                }
                                crate::stdlib::json::JsonFunction::Dump => {
                                    // json.dump(obj, fp) - serialize object to file
                                    // Drop all arguments and return None
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::None
                                }
                                crate::stdlib::json::JsonFunction::JSONEncoder => {
                                    // JSONEncoder class - return placeholder
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown
                                }
                                crate::stdlib::json::JsonFunction::JSONDecoder => {
                                    // JSONDecoder class - return placeholder
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown
                                }
                            };
                        }
                    }

                    // Handle logging module functions
                    if module_name == "logging" {
                        if let Some(log_func) = crate::stdlib::logging::get_function(method_name) {
                            // Emit and drop all arguments
                            for arg in arguments {
                                let arg_type = emit_expr(arg, func, ctx, memory_layout, None);
                                match arg_type {
                                    IRType::String => {
                                        // Strings are (offset, length)
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    _ => {
                                        func.instruction(&Instruction::Drop);
                                    }
                                }
                            }

                            return match log_func {
                                crate::stdlib::logging::LoggingFunction::Debug
                                | crate::stdlib::logging::LoggingFunction::Info
                                | crate::stdlib::logging::LoggingFunction::Warning
                                | crate::stdlib::logging::LoggingFunction::Error
                                | crate::stdlib::logging::LoggingFunction::Critical
                                | crate::stdlib::logging::LoggingFunction::Exception
                                | crate::stdlib::logging::LoggingFunction::Log
                                | crate::stdlib::logging::LoggingFunction::BasicConfig
                                | crate::stdlib::logging::LoggingFunction::SetLevel
                                | crate::stdlib::logging::LoggingFunction::Disable
                                | crate::stdlib::logging::LoggingFunction::AddHandler
                                | crate::stdlib::logging::LoggingFunction::RemoveHandler => {
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::None
                                }
                                crate::stdlib::logging::LoggingFunction::GetLogger
                                | crate::stdlib::logging::LoggingFunction::Logger
                                | crate::stdlib::logging::LoggingFunction::Handler
                                | crate::stdlib::logging::LoggingFunction::StreamHandler
                                | crate::stdlib::logging::LoggingFunction::FileHandler
                                | crate::stdlib::logging::LoggingFunction::Formatter
                                | crate::stdlib::logging::LoggingFunction::Filter
                                | crate::stdlib::logging::LoggingFunction::LogRecord => {
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown
                                }
                            };
                        }
                    }

                    // Handle re module functions
                    if module_name == "re" {
                        if let Some(re_func) = crate::stdlib::re::get_function(method_name) {
                            return match re_func {
                                crate::stdlib::re::ReFunction::Compile => {
                                    // re.compile(pattern, flags=0) - compile pattern for reuse
                                    // For compile-time constant patterns, we can pre-validate
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(pattern)) =
                                            &arguments[0]
                                        {
                                            // Validate pattern at compile time
                                            let flags = if arguments.len() > 1 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[1]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            // Store pattern in memory (lookup existing or use 0)
                                            let offset = memory_layout
                                                .string_offsets
                                                .get(pattern)
                                                .copied()
                                                .unwrap_or(0);
                                            func.instruction(&Instruction::I32Const(offset as i32));
                                            func.instruction(&Instruction::I32Const(
                                                pattern.len() as i32
                                            ));
                                            func.instruction(&Instruction::I32Const(flags));
                                            return IRType::Unknown; // Pattern object
                                        }
                                    }
                                    // Drop all arguments for non-constant patterns
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown
                                }
                                crate::stdlib::re::ReFunction::Search => {
                                    // re.search(pattern, string, flags=0) - search for pattern
                                    // Returns Match object or None
                                    if arguments.len() >= 2 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1])
                                        {
                                            let flags = if arguments.len() > 2 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[2]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            // Execute search at compile time
                                            if let Some(result) =
                                                crate::stdlib::re::search(pattern, text, flags)
                                            {
                                                // Return match info (using text offset if available)
                                                let offset = memory_layout
                                                    .string_offsets
                                                    .get(&result.group)
                                                    .copied()
                                                    .unwrap_or(0);
                                                func.instruction(&Instruction::I32Const(
                                                    offset as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.group.len() as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.start as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.end as i32,
                                                ));
                                                return IRType::Unknown; // Match object
                                            } else {
                                                // No match - return None indicator
                                                func.instruction(&Instruction::I32Const(0));
                                                func.instruction(&Instruction::I32Const(0));
                                                func.instruction(&Instruction::I32Const(-1));
                                                func.instruction(&Instruction::I32Const(-1));
                                                return IRType::None;
                                            }
                                        }
                                    }
                                    // Runtime search - drop args and return placeholder
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(-1));
                                    func.instruction(&Instruction::I32Const(-1));
                                    IRType::None
                                }
                                crate::stdlib::re::ReFunction::Match => {
                                    // re.match(pattern, string, flags=0) - match at beginning
                                    if arguments.len() >= 2 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1])
                                        {
                                            let flags = if arguments.len() > 2 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[2]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            if let Some(result) =
                                                crate::stdlib::re::match_start(pattern, text, flags)
                                            {
                                                let offset = memory_layout
                                                    .string_offsets
                                                    .get(&result.group)
                                                    .copied()
                                                    .unwrap_or(0);
                                                func.instruction(&Instruction::I32Const(
                                                    offset as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.group.len() as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.start as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.end as i32,
                                                ));
                                                return IRType::Unknown;
                                            } else {
                                                func.instruction(&Instruction::I32Const(0));
                                                func.instruction(&Instruction::I32Const(0));
                                                func.instruction(&Instruction::I32Const(-1));
                                                func.instruction(&Instruction::I32Const(-1));
                                                return IRType::None;
                                            }
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(-1));
                                    func.instruction(&Instruction::I32Const(-1));
                                    IRType::None
                                }
                                crate::stdlib::re::ReFunction::Fullmatch => {
                                    // re.fullmatch(pattern, string, flags=0) - full string match
                                    if arguments.len() >= 2 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1])
                                        {
                                            let flags = if arguments.len() > 2 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[2]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            if let Some(result) =
                                                crate::stdlib::re::fullmatch(pattern, text, flags)
                                            {
                                                let offset = memory_layout
                                                    .string_offsets
                                                    .get(&result.group)
                                                    .copied()
                                                    .unwrap_or(0);
                                                func.instruction(&Instruction::I32Const(
                                                    offset as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.group.len() as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.start as i32,
                                                ));
                                                func.instruction(&Instruction::I32Const(
                                                    result.end as i32,
                                                ));
                                                return IRType::Unknown;
                                            } else {
                                                func.instruction(&Instruction::I32Const(0));
                                                func.instruction(&Instruction::I32Const(0));
                                                func.instruction(&Instruction::I32Const(-1));
                                                func.instruction(&Instruction::I32Const(-1));
                                                return IRType::None;
                                            }
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(-1));
                                    func.instruction(&Instruction::I32Const(-1));
                                    IRType::None
                                }
                                crate::stdlib::re::ReFunction::Findall => {
                                    // re.findall(pattern, string, flags=0) - find all matches
                                    if arguments.len() >= 2 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1])
                                        {
                                            let flags = if arguments.len() > 2 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[2]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            let results =
                                                crate::stdlib::re::findall(pattern, text, flags);
                                            // Return list pointer and count (placeholder)
                                            func.instruction(&Instruction::I32Const(0));
                                            func.instruction(&Instruction::I32Const(
                                                results.len() as i32
                                            ));
                                            return IRType::List(Box::new(IRType::String));
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::List(Box::new(IRType::String))
                                }
                                crate::stdlib::re::ReFunction::Finditer => {
                                    // re.finditer(pattern, string, flags=0) - iterator of matches
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown // Iterator
                                }
                                crate::stdlib::re::ReFunction::Split => {
                                    // re.split(pattern, string, maxsplit=0, flags=0)
                                    if arguments.len() >= 2 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1])
                                        {
                                            let maxsplit = if arguments.len() > 2 {
                                                if let IRExpr::Const(IRConstant::Int(m)) =
                                                    &arguments[2]
                                                {
                                                    if *m > 0 {
                                                        Some(*m as usize)
                                                    } else {
                                                        None
                                                    }
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            };
                                            let flags = if arguments.len() > 3 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[3]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            let results = crate::stdlib::re::split(
                                                pattern, text, maxsplit, flags,
                                            );
                                            // Return list pointer and count (placeholder)
                                            func.instruction(&Instruction::I32Const(0));
                                            func.instruction(&Instruction::I32Const(
                                                results.len() as i32
                                            ));
                                            return IRType::List(Box::new(IRType::String));
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::List(Box::new(IRType::String))
                                }
                                crate::stdlib::re::ReFunction::Sub => {
                                    // re.sub(pattern, repl, string, count=0, flags=0)
                                    if arguments.len() >= 3 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(repl)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1], &arguments[2])
                                        {
                                            let count = if arguments.len() > 3 {
                                                if let IRExpr::Const(IRConstant::Int(c)) =
                                                    &arguments[3]
                                                {
                                                    if *c > 0 {
                                                        Some(*c as usize)
                                                    } else {
                                                        None
                                                    }
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            };
                                            let flags = if arguments.len() > 4 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[4]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            let result = crate::stdlib::re::sub(
                                                pattern, repl, text, count, flags,
                                            );
                                            // Return string offset and length (placeholder)
                                            let offset = memory_layout
                                                .string_offsets
                                                .get(&result)
                                                .copied()
                                                .unwrap_or(0);
                                            func.instruction(&Instruction::I32Const(offset as i32));
                                            func.instruction(&Instruction::I32Const(
                                                result.len() as i32
                                            ));
                                            return IRType::String;
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::String
                                }
                                crate::stdlib::re::ReFunction::Subn => {
                                    // re.subn(pattern, repl, string, count=0, flags=0)
                                    // Returns (new_string, num_substitutions)
                                    if arguments.len() >= 3 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(pattern)),
                                            IRExpr::Const(IRConstant::String(repl)),
                                            IRExpr::Const(IRConstant::String(text)),
                                        ) = (&arguments[0], &arguments[1], &arguments[2])
                                        {
                                            let count = if arguments.len() > 3 {
                                                if let IRExpr::Const(IRConstant::Int(c)) =
                                                    &arguments[3]
                                                {
                                                    if *c > 0 {
                                                        Some(*c as usize)
                                                    } else {
                                                        None
                                                    }
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            };
                                            let flags = if arguments.len() > 4 {
                                                if let IRExpr::Const(IRConstant::Int(f)) =
                                                    &arguments[4]
                                                {
                                                    *f
                                                } else {
                                                    0
                                                }
                                            } else {
                                                0
                                            };
                                            let (result, num_subs) = crate::stdlib::re::subn(
                                                pattern, repl, text, count, flags,
                                            );
                                            // Return string offset and length (placeholder)
                                            let offset = memory_layout
                                                .string_offsets
                                                .get(&result)
                                                .copied()
                                                .unwrap_or(0);
                                            func.instruction(&Instruction::I32Const(offset as i32));
                                            func.instruction(&Instruction::I32Const(
                                                result.len() as i32
                                            ));
                                            func.instruction(&Instruction::I32Const(
                                                num_subs as i32,
                                            ));
                                            return IRType::Tuple(vec![
                                                IRType::String,
                                                IRType::Int,
                                            ]);
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Tuple(vec![IRType::String, IRType::Int])
                                }
                                crate::stdlib::re::ReFunction::Escape => {
                                    // re.escape(pattern) - escape special characters
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(pattern)) =
                                            &arguments[0]
                                        {
                                            let escaped = crate::stdlib::re::escape(pattern);
                                            // Return escaped string offset and length (placeholder)
                                            let offset = memory_layout
                                                .string_offsets
                                                .get(&escaped)
                                                .copied()
                                                .unwrap_or(0);
                                            func.instruction(&Instruction::I32Const(offset as i32));
                                            func.instruction(&Instruction::I32Const(
                                                escaped.len() as i32
                                            ));
                                            return IRType::String;
                                        }
                                    }
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::String
                                }
                                crate::stdlib::re::ReFunction::Purge => {
                                    // re.purge() - clear regex cache (no-op in our implementation)
                                    for arg in arguments {
                                        let arg_type =
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                        if arg_type == IRType::String {
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        } else {
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::None
                                }
                            };
                        }
                    }

                    // Handle datetime module constructor functions (datetime.datetime(), datetime.date(), etc.)
                    if module_name == "datetime" {
                        if let Some(dt_func) = crate::stdlib::datetime::get_function(method_name) {
                            return match dt_func {
                                crate::stdlib::datetime::DatetimeFunction::Datetime => {
                                    // datetime.datetime(year, month, day, hour=0, minute=0, second=0, microsecond=0)
                                    // For now, evaluate args and return a tuple
                                    let mut arg_values = Vec::new();
                                    for arg in arguments {
                                        emit_expr(
                                            arg,
                                            func,
                                            ctx,
                                            memory_layout,
                                            Some(&IRType::Int),
                                        );
                                        arg_values.push(());
                                    }
                                    // Pad to 7 values (year, month, day, hour, minute, second, microsecond)
                                    for _ in arg_values.len()..7 {
                                        func.instruction(&Instruction::I32Const(0));
                                    }
                                    IRType::Datetime
                                }
                                crate::stdlib::datetime::DatetimeFunction::Date => {
                                    // datetime.date(year, month, day)
                                    let mut arg_count = 0;
                                    for arg in arguments {
                                        emit_expr(
                                            arg,
                                            func,
                                            ctx,
                                            memory_layout,
                                            Some(&IRType::Int),
                                        );
                                        arg_count += 1;
                                    }
                                    for _ in arg_count..3 {
                                        func.instruction(&Instruction::I32Const(0));
                                    }
                                    IRType::Date
                                }
                                crate::stdlib::datetime::DatetimeFunction::Time => {
                                    // datetime.time(hour=0, minute=0, second=0, microsecond=0)
                                    let mut arg_count = 0;
                                    for arg in arguments {
                                        emit_expr(
                                            arg,
                                            func,
                                            ctx,
                                            memory_layout,
                                            Some(&IRType::Int),
                                        );
                                        arg_count += 1;
                                    }
                                    for _ in arg_count..4 {
                                        func.instruction(&Instruction::I32Const(0));
                                    }
                                    IRType::Time
                                }
                                crate::stdlib::datetime::DatetimeFunction::Timedelta => {
                                    // datetime.timedelta(days=0, seconds=0, microseconds=0, ...)
                                    let mut arg_count = 0;
                                    for arg in arguments {
                                        emit_expr(
                                            arg,
                                            func,
                                            ctx,
                                            memory_layout,
                                            Some(&IRType::Int),
                                        );
                                        arg_count += 1;
                                    }
                                    // Pad to 3 values (days, seconds, microseconds)
                                    for _ in arg_count..3 {
                                        func.instruction(&Instruction::I32Const(0));
                                    }
                                    IRType::Timedelta
                                }
                                crate::stdlib::datetime::DatetimeFunction::Timezone => {
                                    // datetime.timezone(offset, name=None)
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown
                                }
                                crate::stdlib::datetime::DatetimeFunction::Tzinfo => {
                                    // datetime.tzinfo - abstract base class
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Unknown
                                }
                            };
                        }
                    }
                }
            }

            // Check if this is an os.path method call
            if let IRExpr::Attribute {
                object: attr_obj,
                attribute: attr_name,
            } = &**object
            {
                if let IRExpr::Variable(module_name) = &**attr_obj {
                    if crate::stdlib::is_stdlib_submodule(module_name, attr_name)
                        && module_name == "os"
                        && attr_name == "path"
                    {
                        if let Some(path_func) = crate::stdlib::os::path::get_function(method_name)
                        {
                            return match path_func {
                                crate::stdlib::os::path::PathFunction::Join => {
                                    // join(*paths) - joins path components
                                    // For simplicity, just return first argument or "/"
                                    if arguments.is_empty() {
                                        let path = "/".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&path)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(path.len() as i32));
                                    } else {
                                        // Return first argument as simplified implementation
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                        // Drop remaining arguments
                                        for arg in arguments.iter().skip(1) {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                            func.instruction(&Instruction::Drop);
                                        }
                                    }
                                    IRType::String
                                }
                                crate::stdlib::os::path::PathFunction::Exists => {
                                    // exists(path) - check if path exists
                                    // For WASM, always return False
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Bool
                                }
                                crate::stdlib::os::path::PathFunction::Isfile => {
                                    // isfile(path) - check if path is a file
                                    // For WASM, always return False
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Bool
                                }
                                crate::stdlib::os::path::PathFunction::Isdir => {
                                    // isdir(path) - check if path is a directory
                                    // For WASM, always return False
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Bool
                                }
                                crate::stdlib::os::path::PathFunction::Basename => {
                                    // basename(path) - get the base name
                                    // For simplicity, return the input path
                                    if arguments.is_empty() {
                                        let path = "".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&path)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(path.len() as i32));
                                    } else {
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                    }
                                    IRType::String
                                }
                                crate::stdlib::os::path::PathFunction::Dirname => {
                                    // dirname(path) - get the directory name
                                    // For simplicity, return "/"
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    let path = "/".to_string();
                                    let offset = memory_layout
                                        .string_offsets
                                        .get(&path)
                                        .copied()
                                        .unwrap_or(0);
                                    func.instruction(&Instruction::I32Const(offset as i32));
                                    func.instruction(&Instruction::I32Const(path.len() as i32));
                                    IRType::String
                                }
                                crate::stdlib::os::path::PathFunction::Abspath => {
                                    // abspath(path) - get absolute path
                                    // For simplicity, return input path
                                    if arguments.is_empty() {
                                        let path = "/".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&path)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(path.len() as i32));
                                    } else {
                                        emit_expr(&arguments[0], func, ctx, memory_layout, None);
                                    }
                                    IRType::String
                                }
                                crate::stdlib::os::path::PathFunction::Split => {
                                    // split(path) - split into (head, tail)
                                    // Return tuple as simplified implementation
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Tuple(vec![IRType::String, IRType::String])
                                }
                                crate::stdlib::os::path::PathFunction::Splitext => {
                                    // splitext(path) - split into (root, ext)
                                    // Return tuple as simplified implementation
                                    for arg in arguments {
                                        emit_expr(arg, func, ctx, memory_layout, None);
                                        func.instruction(&Instruction::Drop);
                                        func.instruction(&Instruction::Drop);
                                    }
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Tuple(vec![IRType::String, IRType::String])
                                }
                            };
                        }
                    }
                }
            }

            // Handle datetime module class method calls (datetime.datetime.now(), datetime.date.today(), etc.)
            if let IRExpr::Attribute {
                object: attr_obj,
                attribute: class_name,
            } = &**object
            {
                if let IRExpr::Variable(module_name) = &**attr_obj {
                    if module_name == "datetime" {
                        // Handle datetime.datetime.method() calls
                        if class_name == "datetime" {
                            if let Some(dt_method) =
                                crate::stdlib::datetime::get_datetime_method(method_name)
                            {
                                return match dt_method {
                                    crate::stdlib::datetime::DatetimeMethod::Now => {
                                        // datetime.datetime.now() - returns current datetime
                                        // Get current time at compile time using chrono
                                        let timestamp =
                                            crate::stdlib::datetime::datetime_now_local();
                                        if let Some((year, month, day, hour, minute, second)) =
                                            crate::stdlib::datetime::datetime_from_timestamp(
                                                timestamp,
                                            )
                                        {
                                            // Return as tuple: (year, month, day, hour, minute, second, microsecond)
                                            func.instruction(&Instruction::I32Const(year));
                                            func.instruction(&Instruction::I32Const(month as i32));
                                            func.instruction(&Instruction::I32Const(day as i32));
                                            func.instruction(&Instruction::I32Const(hour as i32));
                                            func.instruction(&Instruction::I32Const(minute as i32));
                                            func.instruction(&Instruction::I32Const(second as i32));
                                            func.instruction(&Instruction::I32Const(0));
                                        // microsecond
                                        } else {
                                            // Fallback to epoch
                                            for _ in 0..7 {
                                                func.instruction(&Instruction::I32Const(0));
                                            }
                                        }
                                        IRType::Datetime
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Today => {
                                        // datetime.datetime.today() - same as now() for datetime
                                        let timestamp =
                                            crate::stdlib::datetime::datetime_now_local();
                                        if let Some((year, month, day, hour, minute, second)) =
                                            crate::stdlib::datetime::datetime_from_timestamp(
                                                timestamp,
                                            )
                                        {
                                            func.instruction(&Instruction::I32Const(year));
                                            func.instruction(&Instruction::I32Const(month as i32));
                                            func.instruction(&Instruction::I32Const(day as i32));
                                            func.instruction(&Instruction::I32Const(hour as i32));
                                            func.instruction(&Instruction::I32Const(minute as i32));
                                            func.instruction(&Instruction::I32Const(second as i32));
                                            func.instruction(&Instruction::I32Const(0));
                                        } else {
                                            for _ in 0..7 {
                                                func.instruction(&Instruction::I32Const(0));
                                            }
                                        }
                                        IRType::Datetime
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Fromtimestamp => {
                                        // datetime.datetime.fromtimestamp(ts) - create datetime from timestamp
                                        if !arguments.is_empty() {
                                            emit_expr(
                                                &arguments[0],
                                                func,
                                                ctx,
                                                memory_layout,
                                                Some(&IRType::Int),
                                            );
                                            func.instruction(&Instruction::Drop);
                                        }
                                        // Return placeholder datetime tuple
                                        let timestamp =
                                            crate::stdlib::datetime::datetime_now_local();
                                        if let Some((year, month, day, hour, minute, second)) =
                                            crate::stdlib::datetime::datetime_from_timestamp(
                                                timestamp,
                                            )
                                        {
                                            func.instruction(&Instruction::I32Const(year));
                                            func.instruction(&Instruction::I32Const(month as i32));
                                            func.instruction(&Instruction::I32Const(day as i32));
                                            func.instruction(&Instruction::I32Const(hour as i32));
                                            func.instruction(&Instruction::I32Const(minute as i32));
                                            func.instruction(&Instruction::I32Const(second as i32));
                                            func.instruction(&Instruction::I32Const(0));
                                        } else {
                                            for _ in 0..7 {
                                                func.instruction(&Instruction::I32Const(0));
                                            }
                                        }
                                        IRType::Datetime
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Fromisoformat => {
                                        // datetime.datetime.fromisoformat(date_string)
                                        for arg in arguments {
                                            let arg_type =
                                                emit_expr(arg, func, ctx, memory_layout, None);
                                            if arg_type == IRType::String {
                                                func.instruction(&Instruction::Drop);
                                                func.instruction(&Instruction::Drop);
                                            } else {
                                                func.instruction(&Instruction::Drop);
                                            }
                                        }
                                        // Return placeholder datetime
                                        for _ in 0..7 {
                                            func.instruction(&Instruction::I32Const(0));
                                        }
                                        IRType::Datetime
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Strptime => {
                                        // datetime.datetime.strptime(date_string, format)
                                        for arg in arguments {
                                            let arg_type =
                                                emit_expr(arg, func, ctx, memory_layout, None);
                                            if arg_type == IRType::String {
                                                func.instruction(&Instruction::Drop);
                                                func.instruction(&Instruction::Drop);
                                            } else {
                                                func.instruction(&Instruction::Drop);
                                            }
                                        }
                                        for _ in 0..7 {
                                            func.instruction(&Instruction::I32Const(0));
                                        }
                                        IRType::Datetime
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Strftime
                                    | crate::stdlib::datetime::DatetimeMethod::Isoformat => {
                                        // Instance methods - return empty string placeholder
                                        for arg in arguments {
                                            let arg_type =
                                                emit_expr(arg, func, ctx, memory_layout, None);
                                            if arg_type == IRType::String {
                                                func.instruction(&Instruction::Drop);
                                                func.instruction(&Instruction::Drop);
                                            } else {
                                                func.instruction(&Instruction::Drop);
                                            }
                                        }
                                        let s = "".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&s)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::String
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Replace => {
                                        // datetime.replace(...) - returns new datetime
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        for _ in 0..7 {
                                            func.instruction(&Instruction::I32Const(0));
                                        }
                                        IRType::Datetime
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Timestamp => {
                                        // datetime.timestamp() - returns Unix timestamp as float
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        let timestamp =
                                            crate::stdlib::datetime::datetime_now_local();
                                        func.instruction(&Instruction::F64Const(
                                            (timestamp as f64).into(),
                                        ));
                                        IRType::Float
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Weekday => {
                                        // datetime.weekday() - returns 0-6 (Monday=0)
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::Int
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Isoweekday => {
                                        // datetime.isoweekday() - returns 1-7 (Monday=1)
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        func.instruction(&Instruction::I32Const(1));
                                        IRType::Int
                                    }
                                };
                            }
                        }
                        // Handle datetime.date.method() calls
                        else if class_name == "date" {
                            if let Some(dt_method) =
                                crate::stdlib::datetime::get_datetime_method(method_name)
                            {
                                return match dt_method {
                                    crate::stdlib::datetime::DatetimeMethod::Today => {
                                        // datetime.date.today() - returns current date
                                        let (year, month, day) =
                                            crate::stdlib::datetime::date_today();
                                        func.instruction(&Instruction::I32Const(year));
                                        func.instruction(&Instruction::I32Const(month as i32));
                                        func.instruction(&Instruction::I32Const(day as i32));
                                        IRType::Date
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Fromtimestamp => {
                                        // datetime.date.fromtimestamp(ts)
                                        if !arguments.is_empty() {
                                            emit_expr(
                                                &arguments[0],
                                                func,
                                                ctx,
                                                memory_layout,
                                                Some(&IRType::Int),
                                            );
                                            func.instruction(&Instruction::Drop);
                                        }
                                        let (year, month, day) =
                                            crate::stdlib::datetime::date_today();
                                        func.instruction(&Instruction::I32Const(year));
                                        func.instruction(&Instruction::I32Const(month as i32));
                                        func.instruction(&Instruction::I32Const(day as i32));
                                        IRType::Date
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Fromisoformat => {
                                        // datetime.date.fromisoformat(date_string)
                                        for arg in arguments {
                                            let arg_type =
                                                emit_expr(arg, func, ctx, memory_layout, None);
                                            if arg_type == IRType::String {
                                                func.instruction(&Instruction::Drop);
                                                func.instruction(&Instruction::Drop);
                                            } else {
                                                func.instruction(&Instruction::Drop);
                                            }
                                        }
                                        let (year, month, day) =
                                            crate::stdlib::datetime::date_today();
                                        func.instruction(&Instruction::I32Const(year));
                                        func.instruction(&Instruction::I32Const(month as i32));
                                        func.instruction(&Instruction::I32Const(day as i32));
                                        IRType::Date
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Strftime
                                    | crate::stdlib::datetime::DatetimeMethod::Isoformat => {
                                        for arg in arguments {
                                            let arg_type =
                                                emit_expr(arg, func, ctx, memory_layout, None);
                                            if arg_type == IRType::String {
                                                func.instruction(&Instruction::Drop);
                                                func.instruction(&Instruction::Drop);
                                            } else {
                                                func.instruction(&Instruction::Drop);
                                            }
                                        }
                                        let s = "".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&s)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::String
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Replace => {
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        let (year, month, day) =
                                            crate::stdlib::datetime::date_today();
                                        func.instruction(&Instruction::I32Const(year));
                                        func.instruction(&Instruction::I32Const(month as i32));
                                        func.instruction(&Instruction::I32Const(day as i32));
                                        IRType::Date
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Weekday => {
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::Int
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Isoweekday => {
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        func.instruction(&Instruction::I32Const(1));
                                        IRType::Int
                                    }
                                    _ => {
                                        // Other methods not applicable to date
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::None
                                    }
                                };
                            }
                        }
                        // Handle datetime.time.method() calls
                        else if class_name == "time" {
                            if let Some(dt_method) =
                                crate::stdlib::datetime::get_datetime_method(method_name)
                            {
                                return match dt_method {
                                    crate::stdlib::datetime::DatetimeMethod::Strftime
                                    | crate::stdlib::datetime::DatetimeMethod::Isoformat => {
                                        for arg in arguments {
                                            let arg_type =
                                                emit_expr(arg, func, ctx, memory_layout, None);
                                            if arg_type == IRType::String {
                                                func.instruction(&Instruction::Drop);
                                                func.instruction(&Instruction::Drop);
                                            } else {
                                                func.instruction(&Instruction::Drop);
                                            }
                                        }
                                        let s = "".to_string();
                                        let offset = memory_layout
                                            .string_offsets
                                            .get(&s)
                                            .copied()
                                            .unwrap_or(0);
                                        func.instruction(&Instruction::I32Const(offset as i32));
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::String
                                    }
                                    crate::stdlib::datetime::DatetimeMethod::Replace => {
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        // Return time tuple (hour, minute, second, microsecond)
                                        for _ in 0..4 {
                                            func.instruction(&Instruction::I32Const(0));
                                        }
                                        IRType::Time
                                    }
                                    _ => {
                                        for arg in arguments {
                                            emit_expr(arg, func, ctx, memory_layout, None);
                                            func.instruction(&Instruction::Drop);
                                        }
                                        func.instruction(&Instruction::I32Const(0));
                                        IRType::None
                                    }
                                };
                            }
                        }
                        // Handle datetime.timedelta constructor call
                        else if class_name == "timedelta" {
                            // timedelta(days=0, seconds=0, microseconds=0, ...)
                            let mut arg_count = 0;
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::Int));
                                arg_count += 1;
                            }
                            // Pad to 3 values (days, seconds, microseconds)
                            for _ in arg_count..3 {
                                func.instruction(&Instruction::I32Const(0));
                            }
                            return IRType::Timedelta;
                        }
                    }
                }
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

                        "isdigit" => {
                            // isdigit(): every character is in the class, and the
                            // string is non-empty. Constant receivers are folded
                            // during lowering; this is the runtime path, which
                            // used to answer False unconditionally.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_predicate(func, ctx, ClassMode::Digit);
                            IRType::Bool
                        }

                        "isalpha" => {
                            // isalpha(): every character is in the class, and the
                            // string is non-empty. Constant receivers are folded
                            // during lowering; this is the runtime path, which
                            // used to answer False unconditionally.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_predicate(func, ctx, ClassMode::Alpha);
                            IRType::Bool
                        }

                        "isalnum" => {
                            // isalnum(): every character is in the class, and the
                            // string is non-empty. Constant receivers are folded
                            // during lowering; this is the runtime path, which
                            // used to answer False unconditionally.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_predicate(func, ctx, ClassMode::Alnum);
                            IRType::Bool
                        }

                        "isspace" => {
                            // isspace(): every character is in the class, and the
                            // string is non-empty. Constant receivers are folded
                            // during lowering; this is the runtime path, which
                            // used to answer False unconditionally.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_predicate(func, ctx, ClassMode::Space);
                            IRType::Bool
                        }

                        "isupper" => {
                            // isupper(): every character is in the class, and the
                            // string is non-empty. Constant receivers are folded
                            // during lowering; this is the runtime path, which
                            // used to answer False unconditionally.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_predicate(func, ctx, ClassMode::Upper);
                            IRType::Bool
                        }

                        "islower" => {
                            // islower(): every character is in the class, and the
                            // string is non-empty. Constant receivers are folded
                            // during lowering; this is the runtime path, which
                            // used to answer False unconditionally.
                            if !arguments.is_empty() {
                                ctx.report(format!("str.{method_name}() takes no arguments"));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::Bool;
                            }
                            emit_string_predicate(func, ctx, ClassMode::Lower);
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
                                IRType::List(inner) => {
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

                        "ljust" => {
                            // ljust(width): pad to width with spaces, never
                            // truncating. A custom fill character is rejected
                            // rather than ignored.
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument. \
                                     Hint: a custom fill character is not supported"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            let t = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(t, IRType::Int | IRType::Bool) {
                                ctx.report(format!(
                                    "str.{}() takes an int width, got {}",
                                    method_name,
                                    crate::type_to_string(&t)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_pad(func, ctx, PadMode::Left);
                            IRType::String
                        }

                        "rjust" => {
                            // rjust(width): pad to width with spaces, never
                            // truncating. A custom fill character is rejected
                            // rather than ignored.
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument. \
                                     Hint: a custom fill character is not supported"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            let t = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(t, IRType::Int | IRType::Bool) {
                                ctx.report(format!(
                                    "str.{}() takes an int width, got {}",
                                    method_name,
                                    crate::type_to_string(&t)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_pad(func, ctx, PadMode::Right);
                            IRType::String
                        }

                        "center" => {
                            // center(width): pad to width with spaces, never
                            // truncating. A custom fill character is rejected
                            // rather than ignored.
                            if arguments.len() != 1 {
                                ctx.report(format!(
                                    "str.{method_name}() takes exactly one argument. \
                                     Hint: a custom fill character is not supported"
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            let t = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                            if !matches!(t, IRType::Int | IRType::Bool) {
                                ctx.report(format!(
                                    "str.{}() takes an int width, got {}",
                                    method_name,
                                    crate::type_to_string(&t)
                                ));
                                func.instruction(&Instruction::Unreachable);
                                func.instruction(&Instruction::I32Const(0));
                                func.instruction(&Instruction::I32Const(0));
                                return IRType::String;
                            }
                            emit_string_pad(func, ctx, PadMode::Center);
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
                IRType::Tuple(_element_types) => {
                    emit_tuple_method_call(func, ctx, memory_layout, method_name, arguments)
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
                        if matches!(ret, IRType::String | IRType::Bytes) {
                            recover_str_pair(func, ctx);
                        }
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
            // Unreachable in the normal pipeline: the finalize pass lifts every
            // lambda into `ClosureMake`. Kept as a harmless placeholder for IR
            // built without finalization.
            let param_types = params.iter().map(|p| p.param_type.clone()).collect();
            func.instruction(&Instruction::I32Const(1)); // Lambda function reference

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
                    _ => {
                        func.instruction(&Instruction::I32Const(0));
                    }
                }
                func.instruction(&Instruction::I32Store(mem_off(
                    (COLLECTION_HEADER + k as u32 * COLLECTION_SLOT) as u64,
                )));
            }

            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            IRType::Callable {
                params: Vec::new(),
                return_type: Box::new(IRType::Unknown),
            }
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
    emit_raise(func, ctx, "ZeroDivisionError", 2);
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
                let t = emit_expr(size_arg, func, ctx, memory_layout, Some(&IRType::Int));
                if t == IRType::Float {
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

/// Smallest capacity a grown set table is given.
const SET_GROWTH_FLOOR: i32 = 8;

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
    let declared = match set_type {
        IRType::Set(elem) if !matches!(elem.as_ref(), IRType::Unknown) => Some(elem.as_ref()),
        _ => None,
    };

    if !matches!(method_name, "add" | "remove" | "discard") {
        ctx.report(format!(
            "'{method_name}' is not supported on a set yet. \
             Hint: 'add', 'remove', and 'discard' are the supported set methods"
        ));
        func.instruction(&Instruction::Drop);
        for arg in arguments {
            let arg_type = emit_expr(arg, func, ctx, memory_layout, None);
            func.instruction(&Instruction::Drop);
            if matches!(arg_type, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
        }
        func.instruction(&Instruction::Unreachable);
        return IRType::None;
    }

    if arguments.len() != 1 {
        ctx.report(format!(
            "{method_name}() takes exactly one argument on a set, got {}",
            arguments.len()
        ));
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::Unreachable);
        return IRType::None;
    }

    let ptr = ctx.temp_local;
    let needle = ctx.temp_local + 1;
    let cap = ctx.temp_local + 8;
    let used = ctx.temp_local + 9;
    let newp = ctx.temp_local + 10;
    let idx = ctx.temp_local + 11;
    let srcb = ctx.temp_local + 12;
    let mask = ctx.temp_local + 13;
    let hidx = ctx.temp_local + 14;
    let bkt = ctx.temp_local + 15;
    let saved = ctx.temp_local + 16;

    // The element, then the receiver pointer: the argument is evaluated with
    // the set pointer already under it on the stack.
    let value_ty = emit_collection_element(
        &arguments[0],
        func,
        ctx,
        memory_layout,
        declared,
        &format!("set.{method_name}()"),
    );
    let elem_ty = declared.cloned().unwrap_or(value_ty);
    stash_search_needle(func, ctx, &elem_ty, needle);
    func.instruction(&Instruction::LocalSet(ptr));

    if method_name == "add" {
        // used = load(ptr + SET_USED); cap = load(ptr + SET_CAP)
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(SET_USED as u64)));
        func.instruction(&Instruction::LocalSet(used));
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(SET_CAP as u64)));
        func.instruction(&Instruction::LocalSet(cap));

        // Room for one more at a load factor of 1/2? An insert probe only ever
        // stops at an empty bucket, so the table must never fill.
        func.instruction(&Instruction::LocalGet(used));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(2));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::LocalGet(cap));
        func.instruction(&Instruction::I32GtS);
        func.instruction(&Instruction::If(BlockType::Empty));

        {
            // The pending element is parked first: the rehash below re-stashes
            // a needle per member as it re-inserts them.
            if matches!(elem_ty, IRType::Float) {
                func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
            } else {
                func.instruction(&Instruction::LocalGet(needle));
                func.instruction(&Instruction::LocalSet(saved));
            }

            // Remember the block being replaced, then size the new one:
            // newcap = max(cap * 2, SET_GROWTH_FLOOR), still a power of two.
            func.instruction(&Instruction::LocalGet(ptr));
            emit_set_base(func);
            func.instruction(&Instruction::LocalSet(used)); // old bucket block
            func.instruction(&Instruction::LocalGet(cap));
            func.instruction(&Instruction::I32Const(2));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::LocalTee(mask));
            func.instruction(&Instruction::I32Const(SET_GROWTH_FLOOR));
            func.instruction(&Instruction::LocalGet(mask));
            func.instruction(&Instruction::I32Const(SET_GROWTH_FLOOR));
            func.instruction(&Instruction::I32GtS);
            func.instruction(&Instruction::Select);
            func.instruction(&Instruction::LocalSet(idx)); // newcap, briefly

            // buckets = __alloc(newcap * SET_BUCKET), zeroed so every bucket
            // starts empty.
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::Call(ctx.alloc_func_index));
            func.instruction(&Instruction::LocalSet(newp));
            func.instruction(&Instruction::LocalGet(newp));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::MemoryFill(0));

            // The set now owns the new block and is empty again; re-inserting
            // the live members below fills the counts back in. The header
            // itself does not move, so every name for this set sees all of it.
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Store(mem_off(SET_CAP as u64)));
            func.instruction(&Instruction::LocalGet(ptr));
            func.instruction(&Instruction::LocalGet(newp));
            func.instruction(&Instruction::I32Store(mem_off(SET_DATA as u64)));
            for offset in [0, SET_USED] {
                func.instruction(&Instruction::LocalGet(ptr));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Store(mem_off(offset as u64)));
            }
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::LocalSet(mask));

            // for i in 0..oldcap: re-insert every live bucket. Tombstones and
            // empties are skipped, so the rehash compacts as it goes.
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(idx));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::LocalGet(cap));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));

            func.instruction(&Instruction::LocalGet(used));
            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(srcb));

            func.instruction(&Instruction::LocalGet(srcb));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::I32Const(SET_LIVE));
            func.instruction(&Instruction::I32Eq);
            func.instruction(&Instruction::If(BlockType::Empty));
            // The bucket holds exactly the stashed representation, so the
            // member goes straight into the needle without a round trip
            // through the stack (which a string member could not make: its
            // bucket keeps one word, not the (offset, length) pair).
            func.instruction(&Instruction::LocalGet(srcb));
            if matches!(elem_ty, IRType::Float) {
                func.instruction(&Instruction::F64Load(mem_off(SET_BUCKET_VALUE as u64)));
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
            } else {
                func.instruction(&Instruction::I32Load(mem_off(SET_BUCKET_VALUE as u64)));
                func.instruction(&Instruction::LocalSet(needle));
            }
            emit_stashed_set_insert(func, ctx, &elem_ty, ptr, mask, hidx, bkt);
            func.instruction(&Instruction::End);

            func.instruction(&Instruction::LocalGet(idx));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(idx));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End); // loop
            func.instruction(&Instruction::End); // block

            if matches!(elem_ty, IRType::Float) {
                func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
                func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
            } else {
                func.instruction(&Instruction::LocalGet(saved));
                func.instruction(&Instruction::LocalSet(needle));
            }
        }
        func.instruction(&Instruction::End); // if (needs growth)

        // mask = cap - 1, re-read because the table may have just moved.
        func.instruction(&Instruction::LocalGet(ptr));
        func.instruction(&Instruction::I32Load(mem_off(SET_CAP as u64)));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::LocalSet(mask));
        emit_stashed_set_insert(func, ctx, &elem_ty, ptr, mask, hidx, bkt);
        return IRType::None;
    }

    // remove / discard: probe for a live bucket holding the value and turn it
    // into a tombstone. `idx` counts probes so a table with no empty bucket
    // left (every one occupied or tombstoned) still terminates.
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(mem_off(SET_CAP as u64)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(mask));
    emit_set_hash(func, ctx, &elem_ty, needle);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(hidx));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(idx));
    // found = 0
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(saved));

    func.instruction(&Instruction::Block(BlockType::Empty)); // $done
    func.instruction(&Instruction::Loop(BlockType::Empty)); // $probe
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::BrIf(1)); // whole table probed: not a member

    func.instruction(&Instruction::LocalGet(ptr));
    emit_set_base(func);
    func.instruction(&Instruction::LocalGet(hidx));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(bkt));

    // An empty bucket ends the chain: the value was never inserted.
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(SET_EMPTY));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::BrIf(1));

    // A live bucket holding the value: tombstone it and drop the count.
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    emit_slot_eq_needle(func, ctx, &elem_ty, needle);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(bkt));
    func.instruction(&Instruction::I32Const(SET_DEAD));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::LocalGet(ptr));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(saved)); // found
    func.instruction(&Instruction::Br(2)); // $done
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(hidx));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(hidx));
    func.instruction(&Instruction::LocalGet(idx));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(idx));
    func.instruction(&Instruction::Br(0)); // $probe
    func.instruction(&Instruction::End); // loop
    func.instruction(&Instruction::End); // block

    if method_name == "remove" {
        // Python raises KeyError here. Nothing in the compiled module can carry
        // one, so trap: failing loudly beats quietly not removing anything.
        func.instruction(&Instruction::LocalGet(saved));
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::Unreachable);
        func.instruction(&Instruction::End);
    }

    IRType::None
}

/// How many positional arguments each supported list method takes, as
/// `(minimum, maximum)`. `sort`'s optional slot is its `reverse` keyword, which
/// lowering rewrites into a positional argument.
fn list_method_arity(method_name: &str) -> Option<(usize, usize)> {
    Some(match method_name {
        "append" | "count" | "extend" | "index" | "remove" => (1, 1),
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

            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

            // length = load(list_ptr)
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // length

            if !arguments.is_empty() {
                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index

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

            // Python raises IndexError for a position the list does not have,
            // an empty list included. Trap rather than reading a neighbouring
            // slot, which is what an out-of-range index read does too.
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32LtS);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::I32Or);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::Unreachable);
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
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit the iterable
                let iterable_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);

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

                match iterable_type {
                    IRType::List(_) => {
                        // Save iterable_ptr
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

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
                    _ => {
                        // For non-list iterables, just drop
                        func.instruction(&Instruction::Drop);
                    }
                }
            }
            IRType::None
        }
        "insert" => {
            // list.insert(index, value). Entry stack: (list_ptr). The index is
            // normalized and clamped like Python's, then the tail is moved one
            // slot to the right before the value is stored at its natural width.
            if arguments.len() >= 2 {
                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                let value_type = emit_collection_element(
                    &arguments[1],
                    func,
                    ctx,
                    memory_layout,
                    collection_element_type(list_type).as_ref(),
                    "list.insert()",
                );
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);

                // Keep the index on the stack until the value has been emitted,
                // since a nested value expression may use the scratch locals.
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 6)); // index
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                emit_collection_reserve(func, ctx, Reserve::Count(1), COLLECTION_SLOT);

                // length = load(list_ptr)
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // A negative index counts from the end. Unlike item access,
                // insertion clamps the result into the inclusive [0, length]
                // range instead of raising IndexError.
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32LtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 6));

                // Clamp below zero.
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32LtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 6));

                // Clamp above the current length.
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32GtS);
                func.instruction(&Instruction::Select);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 6));

                // Shift [index, length) one slot to the right. memory.copy has
                // memmove semantics, so overlapping source and destination are
                // safe when the destination starts inside the source range.
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });

                // address = data + index*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 6));
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
            }
            IRType::None
        }
        "index" => {
            // list.index(value) -> int
            // Linear search for first occurrence
            if !arguments.is_empty() {
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit value to search for and stash it as a needle (f64 for
                // floats) so the per-slot compare matches the element width.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);

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

                // Python raises ValueError when the value is absent. Nothing
                // in the compiled module can carry one, so trap, the way
                // `set.remove` of a missing member does: failing loudly beats
                // answering an index the caller would then index with.
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // found
                func.instruction(&Instruction::I32Const(-1));
                func.instruction(&Instruction::I32Eq);
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::Unreachable);
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
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit value to search for and stash it as a needle (f64 for
                // floats) so the per-slot compare matches the element width.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);

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
        "get" => {
            if arguments.is_empty() || arguments.len() > 2 {
                ctx.report("dict.get() takes one or two arguments");
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::Unreachable);
                func.instruction(&Instruction::I32Const(0));
                return IRType::Unknown;
            }
            let ptr = ctx.temp_local;
            let needle = ctx.temp_local + 1;
            let count = ctx.temp_local + 2;
            let i = ctx.temp_local + 3;
            let result = ctx.temp_local + 4;
            let found = ctx.temp_local + 5;
            let slot = ctx.temp_local + 6;

            func.instruction(&Instruction::LocalSet(ptr));
            let arg_key_ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            narrow_element_to_word(func, &arg_key_ty);
            func.instruction(&Instruction::LocalSet(needle));

            // The default is evaluated up front: Python evaluates it eagerly
            // too, since it is an ordinary argument.
            let default_ty = if arguments.len() == 2 {
                let t = emit_expr(&arguments[1], func, ctx, memory_layout, None);
                narrow_element_to_word(func, &t);
                func.instruction(&Instruction::LocalSet(result));
                t
            } else {
                // Missing and no default is Python's `None`; there is no None
                // value here, so it is zero, matching what an absent key used
                // to answer before dict reads started raising KeyError.
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(result));
                IRType::Unknown
            };
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(found));

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
            func.instruction(&Instruction::I32Load(mem_off(COLLECTION_SLOT as u64)));
            func.instruction(&Instruction::LocalSet(result));
            func.instruction(&Instruction::I32Const(1));
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

            func.instruction(&Instruction::LocalGet(result));
            // A found value has the dict's value type; otherwise the default's.
            if matches!(value_type, IRType::Unknown) {
                default_ty
            } else {
                value_type
            }
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
        _ => {
            ctx.report(format!(
                "'{method_name}' is not supported on a dict yet. \
                 Hint: the supported dict methods are get, items, keys, and values"
            ));
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::Unreachable);
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}

/// Emit WASM code for tuple method calls
fn emit_tuple_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
) -> IRType {
    match method_name {
        "index" => {
            // tuple.index(value) -> int
            // Linear search for first occurrence (same as list)
            if !arguments.is_empty() {
                // Emit the searched value (tuple_ptr stays on the stack below it)
                // and stash it as a width-aware needle.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // tuple_ptr

                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // i

                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1));

                // slot address = tuple_ptr + HEADER + i*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);

                emit_slot_eq_needle(func, ctx, &value_type, ctx.temp_local + 1);

                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::Br(2));
                func.instruction(&Instruction::End);

                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                func.instruction(&Instruction::Br(0));

                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                func.instruction(&Instruction::I32Const(-1));
            } else {
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::I32Const(0));
            }
            IRType::Int
        }
        "count" => {
            // tuple.count(value) -> int
            // Count occurrences (same as list)
            if !arguments.is_empty() {
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // tuple_ptr

                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // i
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // count

                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1));

                // slot address = tuple_ptr + HEADER + i*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                emit_data_base(func);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);

                emit_slot_eq_needle(func, ctx, &value_type, ctx.temp_local + 1);

                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                func.instruction(&Instruction::End);

                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                func.instruction(&Instruction::Br(0));

                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
            } else {
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::I32Const(0));
            }
            IRType::Int
        }
        _ => {
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}
