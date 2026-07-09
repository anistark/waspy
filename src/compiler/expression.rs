use crate::compiler::context::{
    strlen_local_name, CompilationContext, COLLECTION_HEADER, COLLECTION_SLOT, DICT_ENTRY,
};
use crate::compiler::function::{load_field_instr, lookup_field};
use crate::ir::{
    IRBoolOp, IRCompareOp, IRConstant, IRExpr, IROp, IRType, IRUnaryOp, MemoryLayout, MethodKind,
    STRING_LEN_PREFIX,
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
    func.instruction(&Instruction::Call(method_idx));
    ret
}

// Helper to convert f64 to Ieee64
#[inline]
fn f64_const(value: f64) -> wasm_encoder::Ieee64 {
    value.into()
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

/// Collections reserve one [`COLLECTION_SLOT`]-byte slot per element, but strings
/// and bytes leave two values on the stack (offset, length). After emitting such
/// an element, drop the length so only the offset remains; identical string
/// literals are interned to the same offset, so offset comparison preserves
/// value equality.
fn narrow_element_to_word(func: &mut Function, elem_type: &IRType) {
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

/// With a slot address on top of the stack, load the slot per `elem_type` and
/// push an i32 `1` if it equals the needle stashed by [`stash_search_needle`],
/// else `0`. Floats compare as `f64` (so members dedup and `in` work by value,
/// not by a lossy bit pattern); everything else compares the i32 low word.
fn emit_slot_eq_needle(
    func: &mut Function,
    ctx: &CompilationContext,
    elem_type: &IRType,
    needle_i32: u32,
) {
    if matches!(elem_type, IRType::Float) {
        func.instruction(&Instruction::F64Load(slot_arg()));
        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
        func.instruction(&Instruction::F64Eq);
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
// count stays at offset 0, so `len(set)` is unchanged. Sets are only ever built
// as literals and then queried with `in`/`len` (no iteration, no `.add`), so the
// table never needs to grow or rehash.

/// Bytes of set header: `count` then `cap`, both i32.
const SET_HEADER: u32 = 8;
/// Bytes per bucket: `state` (i32) + padding + an 8-byte value.
const SET_BUCKET: u32 = 16;
/// Byte offset of the value within a bucket (past the state word + padding).
const SET_BUCKET_VALUE: u32 = 8;

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
        func.instruction(&Instruction::LocalGet(needle_i32));
    }
}

/// Finalize a collection literal that was built into the compile-time template
/// region at `template_ptr` (the `size` bytes reserved by `alloc_collection`),
/// leaving the pointer the expression evaluates to on the stack.
///
/// Outside any loop the template region is unique to this literal, so the
/// template pointer is the result directly (the historical behavior). Inside a
/// loop the single template is rebuilt every iteration, so a per-iteration
/// literal that escapes the loop would alias every other iteration's. Copy the
/// freshly built region into a runtime `__alloc` block and return that pointer
/// instead, giving each iteration its own region (#14). Nested literals compose:
/// an inner literal stores its own runtime pointer into the outer template
/// before the outer region is copied out.
fn emit_collection_result(
    func: &mut Function,
    ctx: &CompilationContext,
    template_ptr: u32,
    size: u32,
) {
    if ctx.loop_stack.is_empty() {
        func.instruction(&Instruction::I32Const(template_ptr as i32));
        return;
    }

    // Round to the same 8-byte granularity `alloc_collection` used so the copy
    // stays inside the reserved template and the runtime block is aligned.
    let aligned = (size + 7) & !7;
    let dst = ctx.temp_local + 7;

    // dst = __alloc(aligned)
    func.instruction(&Instruction::I32Const(aligned as i32));
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(dst));

    // memory.copy(dst, template_ptr, aligned)
    func.instruction(&Instruction::LocalGet(dst));
    func.instruction(&Instruction::I32Const(template_ptr as i32));
    func.instruction(&Instruction::I32Const(aligned as i32));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(dst));
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
                    IROp::Mod => {
                        // String formatting: "format %s" % (value,) or "format %s" % value
                        // TODO: Implement string formatting with placeholders
                        // For now, drop the right value and return the format string
                        if right_type == IRType::String
                            || right_type == IRType::Int
                            || right_type == IRType::Float
                        {
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Drop);
                            return IRType::String;
                        }
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
                        func.instruction(&Instruction::F64Div);
                    }
                    IROp::Mod => {
                        emit_float_modulo_operation(func);
                    }
                    IROp::FloorDiv => {
                        func.instruction(&Instruction::F64Div);
                        func.instruction(&Instruction::F64Floor);
                    }
                    IROp::Pow => {
                        emit_float_power_operation(func);
                    }
                    // New operations - placeholder implementations
                    IROp::MatMul => {
                        // Matrix multiplication not supported yet for floats
                        func.instruction(&Instruction::F64Const(f64_const(0.0)));
                    }
                    IROp::LShift | IROp::RShift | IROp::BitOr | IROp::BitXor | IROp::BitAnd => {
                        // Bitwise operations not supported for floats
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
                        func.instruction(&Instruction::I32DivS);
                    }
                    IROp::Mod => {
                        func.instruction(&Instruction::I32RemS);
                    }
                    IROp::FloorDiv => {
                        func.instruction(&Instruction::I32DivS);
                    }
                    IROp::Pow => {
                        emit_integer_power_operation(func);
                    }
                    // New operations
                    IROp::MatMul => {
                        // Not implemented yet for integers
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
                IRType::Int
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
                            // Logical not: ensure it's 0 or 1 first
                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::I32Ne);
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
                // Stash the searched element in a type-appropriate scratch local
                // (f64 for floats) before emitting the container, so the slot
                // compare below matches list/set storage and works by value.
                stash_search_needle(func, ctx, &elem_type, ctx.temp_local + 1);
                let container_type = emit_expr(right, func, ctx, memory_layout, None);

                // A set is a hash table (constant-time probe); a list is a linear
                // scan. Other containers fall back to a conservative constant.
                let is_set = matches!(container_type, IRType::Set(_));
                let searchable = is_set || matches!(container_type, IRType::List(_));

                if !searchable {
                    func.instruction(&Instruction::Drop); // container pointer
                    func.instruction(&Instruction::I32Const(i32::from(matches!(
                        op,
                        IRCompareOp::NotIn
                    ))));
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

                    // mask = cap - 1  (cap at container_ptr + 4)
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Const(4));
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
                    // bucket = container_ptr + SET_HEADER + idx*SET_BUCKET
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Const(SET_HEADER as i32));
                    func.instruction(&Instruction::I32Add);
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
                    // Value matches -> found, stop.
                    func.instruction(&Instruction::LocalGet(bucket));
                    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
                    func.instruction(&Instruction::I32Add);
                    emit_slot_eq_needle(func, ctx, &elem_type, ctx.temp_local + 1);
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
                    // slot address = container_ptr + HEADER + counter*SLOT
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
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
                        // Ordering/identity on str/bytes isn't supported yet; drop
                        // both (offset, length) pairs and yield a constant.
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
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
                    func.instruction(&Instruction::Call(init_idx));
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
                func.instruction(&Instruction::Call(func_info.index));
                func_info.return_type.clone()
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
                            // Lists, dicts, sets and tuples are all pointers that
                            // store their element/entry count in the first 4 bytes.
                            IRType::List(_)
                            | IRType::Dict(_, _)
                            | IRType::Set(_)
                            | IRType::Tuple(_) => {
                                func.instruction(&Instruction::I32Load(MemArg {
                                    offset: 0,
                                    align: 2,
                                    memory_index: 0,
                                }));
                                IRType::Int
                            }
                            _ => {
                                // Unknown type, return 0
                                func.instruction(&Instruction::I32Const(0));
                                IRType::Int
                            }
                        }
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
                        // int(x): truncate a float to i32; ints pass through.
                        if matches!(arg_types.first(), Some(IRType::Float)) {
                            func.instruction(&Instruction::I32TruncF64S);
                        }
                        IRType::Int
                    }
                    "float" => {
                        // float(x): widen an int to f64; floats pass through.
                        if !matches!(arg_types.first(), Some(IRType::Float)) {
                            func.instruction(&Instruction::F64ConvertI32S);
                        }
                        IRType::Float
                    }
                    "sum" => {
                        if arg_types.is_empty() {
                            return IRType::Unknown;
                        }
                        if arg_types.len() == 1 {
                            // sum(iterable) or sum(iterable, start)
                            match &arg_types[0] {
                                IRType::List(_elem_type) => {
                                    // For now, return a dummy value; proper implementation requires iteration
                                    func.instruction(&Instruction::I32Const(0));
                                    IRType::Int
                                }
                                _ => IRType::Unknown,
                            }
                        } else {
                            // sum(iterable, start)
                            // Pop the iterable, keep the start value as result
                            func.instruction(&Instruction::Drop);
                            arg_types[1].clone()
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
                        // Unknown function, return default value
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

            if elements.is_empty() {
                // Empty list: a header with length 0.
                let list_ptr = ctx.alloc_collection(COLLECTION_HEADER);
                func.instruction(&Instruction::I32Const(list_ptr as i32));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Store(slot_arg()));
                emit_collection_result(func, ctx, list_ptr, COLLECTION_HEADER);
                return IRType::List(Box::new(IRType::Unknown));
            }

            let list_size = COLLECTION_HEADER + elements.len() as u32 * COLLECTION_SLOT;
            let list_ptr = ctx.alloc_collection(list_size);

            // Store length at the beginning
            func.instruction(&Instruction::I32Const(list_ptr as i32));
            func.instruction(&Instruction::I32Const(elements.len() as i32));
            func.instruction(&Instruction::I32Store(slot_arg()));

            // Store each element. A WASM store pops the value first, then the
            // address, so the destination address must be pushed *before* the
            // value. list_ptr and the slot offset are compile-time constants,
            // so we fold them into a single address constant.
            let mut elem_type = IRType::Unknown;
            for (i, elem) in elements.iter().enumerate() {
                let addr = list_ptr + COLLECTION_HEADER + (i as u32 * COLLECTION_SLOT);

                func.instruction(&Instruction::I32Const(addr as i32));
                let ty = emit_expr(elem, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &ty);
                store_collection_word(func, &ty);

                if i == 0 {
                    elem_type = ty;
                }
            }

            // Return pointer to the list
            emit_collection_result(func, ctx, list_ptr, list_size);
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
            let set_ptr = ctx.alloc_collection(set_size);

            // Zero the whole region so every bucket starts empty (state 0) and
            // count is 0. This also clears any stale state when the same template
            // region is rebuilt on each iteration of an enclosing loop.
            func.instruction(&Instruction::I32Const(set_ptr as i32));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::I32Const(set_size as i32));
            func.instruction(&Instruction::MemoryFill(0));
            // Store the capacity at offset 4 (count at offset 0 stays 0).
            func.instruction(&Instruction::I32Const((set_ptr + 4) as i32));
            func.instruction(&Instruction::I32Const(cap as i32));
            func.instruction(&Instruction::I32Store(slot_arg()));

            let idx = ctx.temp_local + 2;
            let bucket = ctx.temp_local + 3;

            let mut elem_type = IRType::Unknown;
            for (i, elem) in elements.iter().enumerate() {
                // Evaluate the element, stash it as a needle (f64 for floats), and
                // compute its home bucket: idx = hash(elem) & (cap - 1).
                let ty = emit_expr(elem, func, ctx, memory_layout, None);
                if i == 0 {
                    elem_type = ty.clone();
                }
                stash_search_needle(func, ctx, &ty, ctx.temp_local + 1);
                emit_set_hash(func, ctx, &ty, ctx.temp_local + 1);
                func.instruction(&Instruction::I32Const(mask));
                func.instruction(&Instruction::I32And);
                func.instruction(&Instruction::LocalSet(idx));

                // Linear-probe to the home bucket or a free one (skip on dup).
                func.instruction(&Instruction::Block(BlockType::Empty)); // $done
                func.instruction(&Instruction::Loop(BlockType::Empty)); // $probe

                // bucket = set_ptr + SET_HEADER + idx*SET_BUCKET
                func.instruction(&Instruction::I32Const((set_ptr + SET_HEADER) as i32));
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
                // state = 1
                func.instruction(&Instruction::LocalGet(bucket));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Store(slot_arg()));
                // value = needle (at bucket + SET_BUCKET_VALUE)
                func.instruction(&Instruction::LocalGet(bucket));
                func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
                func.instruction(&Instruction::I32Add);
                store_stashed_needle(func, ctx, &ty, ctx.temp_local + 1);
                // count += 1
                func.instruction(&Instruction::I32Const(set_ptr as i32));
                func.instruction(&Instruction::I32Const(set_ptr as i32));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Store(slot_arg()));
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

            // Return pointer to the set
            emit_collection_result(func, ctx, set_ptr, set_size);
            IRType::Set(Box::new(elem_type))
        }
        IRExpr::TupleLiteral(elements) => {
            // Tuple layout in memory: [length:i32][elem0][elem1]... One
            // COLLECTION_SLOT per element, matching list storage.

            if elements.is_empty() {
                let tuple_ptr = ctx.alloc_collection(COLLECTION_HEADER);
                func.instruction(&Instruction::I32Const(tuple_ptr as i32));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                emit_collection_result(func, ctx, tuple_ptr, 4);
                return IRType::Tuple(vec![]);
            }

            let tuple_size = COLLECTION_HEADER + elements.len() as u32 * COLLECTION_SLOT;
            let tuple_ptr = ctx.alloc_collection(tuple_size);

            // Store length at the beginning
            func.instruction(&Instruction::I32Const(tuple_ptr as i32));
            func.instruction(&Instruction::I32Const(elements.len() as i32));
            func.instruction(&Instruction::I32Store(slot_arg()));

            // Track element types for heterogeneous tuples
            let mut element_types = Vec::new();

            // Store each element. The destination address is pushed before the
            // value (WASM stores pop the value first, then the address).
            for (i, elem) in elements.iter().enumerate() {
                let addr = tuple_ptr + COLLECTION_HEADER + (i as u32 * COLLECTION_SLOT);

                func.instruction(&Instruction::I32Const(addr as i32));
                let elem_type = emit_expr(elem, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &elem_type);
                store_collection_word(func, &elem_type);
                element_types.push(elem_type);
            }

            emit_collection_result(func, ctx, tuple_ptr, tuple_size);
            IRType::Tuple(element_types)
        }
        IRExpr::DictLiteral(pairs) => {
            // Dict layout in memory: [num_entries:i32][key0][val0][key1][val1]...
            // Each key and value occupies one COLLECTION_SLOT, so an entry is
            // DICT_ENTRY bytes wide (float values round-trip losslessly).
            let dict_size = COLLECTION_HEADER + pairs.len() as u32 * DICT_ENTRY;
            let dict_ptr = ctx.alloc_collection(dict_size);

            // Store number of entries
            func.instruction(&Instruction::I32Const(dict_ptr as i32));
            func.instruction(&Instruction::I32Const(pairs.len() as i32));
            func.instruction(&Instruction::I32Store(slot_arg()));

            // Determine key and value types from first pair
            let (key_type, value_type) = if !pairs.is_empty() {
                let key_type = emit_expr(&pairs[0].0, func, ctx, memory_layout, None);
                // We need to drop the key value that was just pushed
                func.instruction(&Instruction::Drop);
                let value_type = emit_expr(&pairs[0].1, func, ctx, memory_layout, None);
                func.instruction(&Instruction::Drop);
                (key_type, value_type)
            } else {
                (IRType::Unknown, IRType::Unknown)
            };

            // Store each key-value pair. The destination address is pushed
            // before the value (WASM stores pop the value first, then address).
            for (i, (key_expr, val_expr)) in pairs.iter().enumerate() {
                let key_addr = dict_ptr + COLLECTION_HEADER + (i as u32 * DICT_ENTRY);
                let val_addr = key_addr + COLLECTION_SLOT;

                // Store key (one slot; floats keep full f64 precision, matching
                // list/tuple element storage so reads recover the value).
                func.instruction(&Instruction::I32Const(key_addr as i32));
                let k_type = emit_expr(key_expr, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &k_type);
                store_collection_word(func, &k_type);

                // Store value
                func.instruction(&Instruction::I32Const(val_addr as i32));
                let v_type = emit_expr(val_expr, func, ctx, memory_layout, None);
                narrow_element_to_word(func, &v_type);
                store_collection_word(func, &v_type);
            }

            // Return pointer to the dict
            emit_collection_result(func, ctx, dict_ptr, dict_size);
            IRType::Dict(Box::new(key_type), Box::new(value_type))
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
            let _index_type = emit_expr(index, func, ctx, memory_layout, Some(&index_hint));

            match container_type {
                IRType::String => {
                    // String indexing returns a single character string.
                    // Stack: (offset, length, index) -> result (char_offset, 1)
                    func.instruction(&Instruction::LocalSet(ctx.temp_local)); // index
                    func.instruction(&Instruction::Drop); // discard length
                                                          // Stack: (offset)
                    func.instruction(&Instruction::LocalGet(ctx.temp_local)); // index
                    func.instruction(&Instruction::I32Add); // offset + index
                                                            // Length is always 1 for a single character
                    func.instruction(&Instruction::I32Const(1));

                    IRType::String
                }
                IRType::Bytes => {
                    // Bytes indexing returns an integer (byte value 0-255).
                    // Stack: (offset, length, index) -> result (byte)
                    func.instruction(&Instruction::LocalSet(ctx.temp_local)); // index
                    func.instruction(&Instruction::Drop); // discard length
                                                          // Stack: (offset)
                    func.instruction(&Instruction::LocalGet(ctx.temp_local)); // index
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
                    func.instruction(&Instruction::LocalSet(ctx.temp_local)); // index
                                                                              // Stack: (list_ptr)
                    func.instruction(&Instruction::LocalGet(ctx.temp_local)); // index
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul); // index * SLOT
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add); // index*SLOT + HEADER
                    func.instruction(&Instruction::I32Add); // list_ptr + HEADER + index*SLOT

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

                    // Loop: while counter < num_entries
                    func.instruction(&Instruction::Block(BlockType::Empty));
                    func.instruction(&Instruction::Loop(BlockType::Empty));

                    // Check if counter >= num_entries
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32GeS);
                    func.instruction(&Instruction::BrIf(1)); // Break loop

                    // Load key at offset: dict_ptr + HEADER + counter*DICT_ENTRY
                    func.instruction(&Instruction::LocalGet(ctx.temp_local)); // dict_ptr
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // counter
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Add);
                    // Load the slot's key and compare with search_key at the key's
                    // natural width (f64 for float keys, i32 word otherwise).
                    if is_float_key {
                        func.instruction(&Instruction::F64Load(slot_arg()));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
                        func.instruction(&Instruction::F64Eq);
                    } else {
                        func.instruction(&Instruction::I32Load(slot_arg()));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                        func.instruction(&Instruction::I32Eq);
                    }

                    // If equal, capture the value and break out of the loop.
                    // Value slot = dict_ptr + HEADER + counter*DICT_ENTRY + SLOT.
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Const(
                        (COLLECTION_HEADER + COLLECTION_SLOT) as i32,
                    ));
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

                    // Push the looked-up value (0 / 0.0 if the key was not found).
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
                    func.instruction(&Instruction::LocalSet(ctx.temp_local)); // index
                                                                              // Stack: (tuple_ptr)
                    func.instruction(&Instruction::LocalGet(ctx.temp_local)); // index
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul); // index * SLOT
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add); // index*SLOT + HEADER
                    func.instruction(&Instruction::I32Add); // tuple_ptr + HEADER + index*SLOT

                    // For homogeneous indexing, use first element type
                    // In practice, we'd need to track which index is being accessed
                    let elem_type = if !element_types.is_empty() {
                        element_types[0].clone()
                    } else {
                        IRType::Unknown
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
                    // List slicing: list[start:end:step]
                    // For now, allocate a new list and copy elements based on slice bounds
                    // Stack: (list_ptr)

                    // Load list length from first 4 bytes
                    func.instruction(&Instruction::LocalTee(ctx.temp_local));
                    func.instruction(&Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                    // Stack: (list_ptr, list_length)
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // Save list_length

                    // Default start to 0
                    if let Some(s) = start {
                        emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
                    } else {
                        func.instruction(&Instruction::I32Const(0));
                    }
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // Save start

                    // Default end to list_length
                    if let Some(e) = end {
                        emit_expr(e, func, ctx, memory_layout, Some(&IRType::Int));
                    } else {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                    }
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // Save end

                    // Handle step (if provided, compute slice length differently)
                    let step_val = if let Some(st) = step {
                        emit_expr(st, func, ctx, memory_layout, Some(&IRType::Int));
                        // For now, assume step = 1
                        func.instruction(&Instruction::Drop);
                        1
                    } else {
                        1
                    };

                    // Calculate slice length: max(0, (end - start) / step)
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // end
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // start
                    func.instruction(&Instruction::I32Sub); // end - start
                    if step_val != 1 {
                        func.instruction(&Instruction::I32Const(step_val));
                        func.instruction(&Instruction::I32DivS);
                    }
                    // Clamp to >= 0
                    func.instruction(&Instruction::LocalTee(ctx.temp_local + 4)); // Save computed length
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32GeS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::End);
                    // Stack: (slice_length)

                    // For now, return a dummy list (proper implementation would copy elements)
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0)); // Return null pointer for now

                    IRType::List(elem_type.clone())
                }
                _ => IRType::Unknown,
            }
        }
        IRExpr::Attribute { object, attribute } => {
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
            // class-level variable; inline its value.
            if let IRExpr::Variable(name) = &**object {
                if let Some(class_name) = static_class_target(ctx, name) {
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
                        func.instruction(&Instruction::Call(getter_idx));
                        return ret;
                    }

                    // Instance field read: load with the field's width and report
                    // its type so float fields participate in f64 arithmetic.
                    if let Some((field_offset, field_ty)) = lookup_field(ctx, class_name, attribute)
                    {
                        func.instruction(&load_field_instr(&field_ty, field_offset));
                        field_ty
                    } else {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                        IRType::Unknown
                    }
                }
                _ => {
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::ListComp {
            expr: _,
            var_name: _,
            iterable,
        } => {
            // [expr for var_name in iterable]
            // Emit code for the iterable evaluation
            emit_expr(iterable, func, ctx, memory_layout, None);
            // Drop the iterable result for now
            func.instruction(&Instruction::Drop);
            // Return an empty list pointer as placeholder
            func.instruction(&Instruction::I32Const(0));
            IRType::List(Box::new(IRType::Unknown))
        }
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
                        func.instruction(&Instruction::Call(method_idx));
                        return ret;
                    }
                    // No base or unknown method: evaluate nothing, yield 0.
                    func.instruction(&Instruction::I32Const(0));
                    return IRType::Unknown;
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
                            // upper(): Convert string to uppercase
                            // Stack: (offset, length) -> (new_offset, new_length)
                            // Proper implementation: iterate through chars and convert to uppercase
                            // For now: return unchanged string (char transformation in WASM is complex)
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "lower" => {
                            // lower(): Convert string to lowercase
                            // Stack: (offset, length) -> (new_offset, new_length)
                            // Proper implementation: iterate through chars and convert to lowercase
                            // For now: return unchanged string
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "strip" => {
                            // strip(): Remove leading/trailing whitespace
                            // Proper implementation: find first/last non-space char
                            // For now: return unchanged string
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "lstrip" => {
                            // lstrip(): Remove leading whitespace
                            // Proper implementation: find first non-space char
                            // For now: return unchanged string
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "rstrip" => {
                            // rstrip(): Remove trailing whitespace
                            // Proper implementation: find last non-space char
                            // For now: return unchanged string
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "capitalize" => {
                            // capitalize(): Uppercase first character, lowercase rest
                            // Proper implementation: conditional char transformation
                            // For now: return unchanged string
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "title" => {
                            // title(): Titlecase string (uppercase after whitespace)
                            // Proper implementation: track whitespace and uppercase following chars
                            // For now: return unchanged string
                            for _arg in arguments {
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::String
                        }
                        "split" => {
                            // split(sep): Returns list (represented as array pointer for now)
                            // For runtime: store result as list would require proper list allocation
                            // For now: evaluate args and return default list value
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return list pointer
                            IRType::List(Box::new(IRType::String))
                        }
                        "find" => {
                            // find(sub): Returns index of substring or -1
                            // Naive implementation: linear search
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(-1)); // Not found
                            IRType::Int
                        }
                        "index" => {
                            // index(sub): Like find but returns 0 if not found
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Int
                        }
                        "count" => {
                            // count(sub): Count occurrences
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Default count
                            IRType::Int
                        }
                        "startswith" => {
                            // startswith(prefix): Check if string starts with prefix
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Default false
                            IRType::Bool
                        }
                        "endswith" => {
                            // endswith(suffix): Check if string ends with suffix
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Default false
                            IRType::Bool
                        }
                        "replace" => {
                            // replace(old, new): Replace occurrences
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return offset=0
                            IRType::String
                        }
                        "isdigit" => {
                            // isdigit(): Check if all characters are digits
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isalpha" => {
                            // isalpha(): Check if all characters are alphabetic
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isalnum" => {
                            // isalnum(): Check if all characters are alphanumeric
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isspace" => {
                            // isspace(): Check if all characters are whitespace
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isupper" => {
                            // isupper(): Check if all cased characters are uppercase
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "islower" => {
                            // islower(): Check if all cased characters are lowercase
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "join" => {
                            // join(iterable): Join list of strings with separator
                            // For now: evaluate iterable and return default value
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string (separator)
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "format" => {
                            // format(*args, **kwargs): Format string
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "ljust" => {
                            // ljust(width, fillchar=' '): Left justify
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "rjust" => {
                            // rjust(width, fillchar=' '): Right justify
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "center" => {
                            // center(width, fillchar=' '): Center justify
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        _ => {
                            // Unknown method
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Drop);
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::Unknown
                        }
                    }
                }
                IRType::List(_element_type) => emit_list_method_call(
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
                            emit_expr(arg, func, ctx, memory_layout, param_types.get(i + arg_base));
                        }
                        func.instruction(&Instruction::Call(method_idx));
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
                _ => {
                    // Non-string/list/class methods not yet supported
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::Drop);
                    for arg in arguments {
                        emit_expr(arg, func, ctx, memory_layout, None);
                        func.instruction(&Instruction::Drop);
                    }
                    IRType::Unknown
                }
            }
        }
        IRExpr::RangeCall { start, stop, step } => {
            // Range object layout in memory: [start:i32][stop:i32][step:i32][current:i32]
            let range_ptr = ctx.alloc_collection(16);

            // Each field store pushes the destination address *before* the value
            // (a WASM store pops the value first, then the address). range_ptr is
            // a constant, so the field offset goes in the store's MemArg.
            let store_field = |func: &mut Function, offset: u64| {
                func.instruction(&Instruction::I32Store(MemArg {
                    offset,
                    align: 2,
                    memory_index: 0,
                }));
            };

            // start (default 0) at offset 0
            func.instruction(&Instruction::I32Const(range_ptr as i32));
            if let Some(s) = start {
                emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
            } else {
                func.instruction(&Instruction::I32Const(0));
            }
            store_field(func, 0);

            // stop at offset 4
            func.instruction(&Instruction::I32Const(range_ptr as i32));
            emit_expr(stop, func, ctx, memory_layout, Some(&IRType::Int));
            store_field(func, 4);

            // step (default 1) at offset 8
            func.instruction(&Instruction::I32Const(range_ptr as i32));
            if let Some(s) = step {
                emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
            } else {
                func.instruction(&Instruction::I32Const(1));
            }
            store_field(func, 8);

            // current = start at offset 12
            func.instruction(&Instruction::I32Const(range_ptr as i32));
            func.instruction(&Instruction::I32Const(range_ptr as i32));
            func.instruction(&Instruction::I32Load(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            store_field(func, 12);

            // Return pointer to range object
            func.instruction(&Instruction::I32Const(range_ptr as i32));
            IRType::Range
        }
        IRExpr::DynamicImportExpr { module_name } => {
            // Emit code to evaluate the module name
            emit_expr(module_name, func, ctx, memory_layout, None);

            // TODO: dynamic imports requires more extensive runtime support
            func.instruction(&Instruction::Drop); // Drop the module name
            func.instruction(&Instruction::I32Const(0)); // Return a dummy value

            IRType::Unknown
        }
        IRExpr::Lambda {
            params,
            body: _,
            captured_vars: _,
        } => {
            // Lambdas with closures: capture variables and return a callable
            let param_types = params.iter().map(|p| p.param_type.clone()).collect();
            func.instruction(&Instruction::I32Const(1)); // Lambda function reference

            IRType::Callable {
                params: param_types,
                return_type: Box::new(IRType::Unknown),
            }
        }
    }
}

/// Emit WebAssembly instructions for the integer power operation (a ** b)
pub fn emit_integer_power_operation(func: &mut Function) {
    // Power operation: a ** b

    // Save the base value to a local
    func.instruction(&Instruction::LocalSet(0));
    func.instruction(&Instruction::LocalSet(1));

    // Initialize result to 1
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(2));

    // Check if exponent is 0, if so return 1
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // Handle negative exponent as special case (return 0 for now)
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // Start loop to calculate power
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));

    // Check if exponent is 0, if so break out of loop
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));

    // result *= base
    func.instruction(&Instruction::LocalGet(2));
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(2));

    // exponent--
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(0));

    // Loop back
    func.instruction(&Instruction::Br(0));

    // End loop
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Push result to stack
    func.instruction(&Instruction::LocalGet(2));
}

/// Emit WebAssembly instructions for the float power operation (a ** b)
pub fn emit_float_power_operation(func: &mut Function) {
    // Float power operation: base ** exp
    // Handles special cases: exp=0, exp=1, exp=2, exp=-1
    // For integer exponents: repeated multiplication
    // For fractional exponents: returns base as approximation

    func.instruction(&Instruction::LocalSet(0)); // Save exp to local 0
    func.instruction(&Instruction::LocalTee(1)); // Save base to local 1, keep on stack

    // Handle special case: base ** 0 = 1
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(0.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Handle special case: base ** 1 = base
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Handle special case: base ** 2 = base * base
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(2.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalTee(2));
    func.instruction(&Instruction::LocalGet(2));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Handle special case: base ** -1 = 1 / base
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(-1.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // For other exponents: return base as approximation
    func.instruction(&Instruction::LocalGet(1));
}

/// Emit WebAssembly instructions for float modulo operation (a % b)
pub fn emit_float_modulo_operation(func: &mut Function) {
    // Float modulo: a % b = a - b * floor(a / b)

    // Stack starts with: a b

    // Save b to local 0
    func.instruction(&Instruction::LocalSet(0));

    // Save a to local 1
    func.instruction(&Instruction::LocalSet(1));

    // Compute floor(a / b)
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::F64Floor);

    // Compute b * floor(a / b)
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Mul);

    // Compute a - b * floor(a / b)
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::F64Sub);
}

/// Emit WebAssembly instructions for list method calls
pub fn emit_list_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    list_type: &IRType,
) -> IRType {
    match method_name {
        "append" => {
            // list.append(value). Entry stack: (list_ptr). Each element occupies
            // one COLLECTION_SLOT; the value is stored at its natural width so a
            // float keeps full f64 precision. The element grows the list past its
            // literal capacity (a known limitation — no runtime regrow yet).
            if !arguments.is_empty() {
                // Emit the value while list_ptr stays safely on the stack below
                // it, then stash it into a type-appropriate scratch local.
                let value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                // length = load(list_ptr)
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // address = list_ptr + HEADER + length*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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
            // list.pop([index]) — pop the given index, else the last element.
            // Entry stack: (list_ptr). The length is decremented first so the
            // width-aware element load can be the final value left on the stack.
            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

            // length = load(list_ptr)
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Load(slot_arg()));
            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // length

            if !arguments.is_empty() {
                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index
            } else {
                // Last element: index = length - 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index
            }

            // length -= 1
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Store(slot_arg()));

            // address = list_ptr + HEADER + index*SLOT
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);

            // Load and return the popped element at its natural width.
            let elem_type = match list_type {
                IRType::List(t) => t.as_ref().clone(),
                _ => IRType::Unknown,
            };
            load_collection_word(func, &elem_type, ctx.temp_local + 3);
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
                        func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // list_len
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        // src = iterable_ptr + HEADER + i*SLOT
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // iterable_ptr
                        func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                        func.instruction(&Instruction::I32Add);
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
            // list.insert(index, value). Simplified: append at the end (element
            // shifting to honour the position is not implemented yet). Entry
            // stack: (list_ptr). The value is stored at its natural width.
            if arguments.len() >= 2 {
                // Index is evaluated for side effects but not yet honoured.
                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::Drop);

                let value_type = emit_expr(&arguments[1], func, ctx, memory_layout, None);
                stash_search_needle(func, ctx, &value_type, ctx.temp_local + 1);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // list_ptr

                // length = load(list_ptr)
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // address = list_ptr + HEADER + length*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Add);
                // src = list_ptr + HEADER + (j+1)*SLOT
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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

                // Initialize index to 0
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // current_index

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
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Add);

                // Compare with the needle (width-aware).
                emit_slot_eq_needle(func, ctx, &value_type, ctx.temp_local + 1);

                // If equal, return current_index
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
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

                // Not found, return -1
                func.instruction(&Instruction::I32Const(-1));
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
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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
        _ => {
            // Unknown method
            func.instruction(&Instruction::Drop); // Drop list_ptr
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
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                func.instruction(&Instruction::I32Add);
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
