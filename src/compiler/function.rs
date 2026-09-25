use crate::compiler::context::{
    comp_gen_local_name, comp_local_name, strlen_local_name, CompilationContext, LoopContext,
    CALL_DEPTH_GLOBAL, COLLECTION_CAP, COLLECTION_DATA, COLLECTION_HEADER, COLLECTION_SLOT,
    DICT_ENTRY, EXC_TYPE_GLOBAL, HELD_F64_LOCALS, HELD_LOCALS, SCRATCH_LOCALS,
};
use crate::compiler::expression::emit_expr;
use crate::ir::{IRBody, IRConstant, IRExpr, IRFunction, IROp, IRStatement, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

/// Compile an IR function into a WebAssembly function
pub fn compile_function(
    ir_func: &IRFunction,
    ctx: &mut CompilationContext,
    memory_layout: &MemoryLayout,
    return_type: &IRType,
    owning_class: Option<&str>,
) -> Function {
    ctx.locals_map.clear();
    ctx.local_count = 0;
    // Errors reported from expression codegen are located by this name.
    ctx.current_function = Some(match owning_class {
        Some(class) => format!("{class}.{}", ir_func.name),
        None => ir_func.name.clone(),
    });

    // An `__init__` method returns `self` (its first parameter) so the
    // instantiation site receives the freshly allocated instance pointer as
    // the constructor call's result. Python guarantees `__init__` returns
    // None, so no user return value competes with this.
    ctx.return_self = ir_func.name == "__init__"
        && matches!(ir_func.params.first(), Some(p) if matches!(p.param_type, IRType::Class(_)));

    // `super().method(...)` resolves the base class of the class whose method
    // body is being compiled.
    ctx.current_class = owning_class.map(str::to_string);
    ctx.current_return_type = return_type.clone();

    for param in &ir_func.params {
        ctx.add_local(&param.name, param.param_type.clone());
    }
    // A string/bytes value is an (offset, length) pair everywhere else in
    // codegen, but a parameter arrives as the offset alone. Without a
    // companion, reading the parameter pushed one word where the rest of
    // codegen expects two, so narrowing it to a single word (as a dict key or a
    // collection element does) dropped the offset and kept whatever was
    // underneath. The companion is filled from the blob's own length prefix in
    // the prologue below. These are added only after every parameter has its
    // index: WASM parameters are locals 0..n-1, so a companion interleaved
    // among them would take the index the next parameter must have.
    for param in &ir_func.params {
        if matches!(param.param_type, IRType::String | IRType::Bytes) {
            ctx.add_local(&strlen_local_name(&param.name), IRType::Int);
        }
    }

    // Scan for variable declarations to allocate locals. The for-loop counter
    // is advanced during the scan and replayed during codegen, so reset it here.
    ctx.for_loop_seq = 0;
    scan_and_allocate_locals(&ir_func.body, ctx);

    // Reserve scratch locals after all params and named locals so temporary
    // calculations never clobber real variables. The i32 scratch run is absent
    // from locals_map (defaults to i32); the f64 scratch is registered so it is
    // declared as f64 and used for operand juggling during int/float coercion.
    ctx.temp_local = ctx.local_count;
    ctx.local_count += SCRATCH_LOCALS;
    // Held slots (see `HELD_LOCALS`): values that must survive the emission of
    // nested expressions, apart from the scratch run that emission uses.
    ctx.held_local_base = ctx.local_count;
    ctx.local_count += HELD_LOCALS;
    ctx.held_depth.set(0);
    ctx.temp_local_f64 = ctx.add_local("__f64_scratch", IRType::Float);
    ctx.temp_local_f64_2 = ctx.add_local("__f64_scratch2", IRType::Float);
    ctx.temp_local_f64_3 = ctx.add_local("__f64_scratch3", IRType::Float);
    ctx.held_f64_locals = (0..HELD_F64_LOCALS)
        .map(|i| ctx.add_local(&format!("__held_f64_{i}"), IRType::Float))
        .collect();
    ctx.held_f64_depth.set(0);
    // Held by an item assignment across its key and value emission. A statement
    // cannot nest inside its own sub-expressions, so one set per function does.
    ctx.assign_container_local = ctx.add_local("__assign_container", IRType::Int);
    ctx.assign_key_local = ctx.add_local("__assign_key", IRType::Int);
    ctx.assign_key_f64_local = ctx.add_local("__assign_key_f64", IRType::Float);

    // Declare locals in index order, coalescing adjacent same-type runs. The
    // local index assigned by `add_local` must match the WASM declaration
    // order, so grouping all i32s then all f64s (which reorders indices) is
    // wrong once a function mixes int and float locals.
    let num_params = ir_func.params.len() as u32;
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for i in num_params..ctx.local_count {
        let val_type = match get_local_type_by_index(ctx, i) {
            IRType::Float => ValType::F64,
            _ => ValType::I32,
        };
        match locals.last_mut() {
            Some((count, last)) if *last == val_type => *count += 1,
            _ => locals.push((1, val_type)),
        }
    }

    let mut func = Function::new(locals);

    // Replay the same for-loop numbering used by the scan above so codegen
    // resolves the matching iterator helper locals.
    ctx.for_loop_seq = 0;

    // Loop-control bookkeeping starts empty for each function.
    ctx.block_depth = 0;
    ctx.loop_stack.clear();
    ctx.comp_depth.set(0);

    // Prologue: recover each string parameter's length from the four bytes
    // before its data, so the companion local reserved above is live before any
    // read of the parameter.
    for param in &ir_func.params {
        if !matches!(param.param_type, IRType::String | IRType::Bytes) {
            continue;
        }
        let (Some(off_idx), Some(len_idx)) = (
            ctx.get_local_index(&param.name),
            ctx.get_local_index(&strlen_local_name(&param.name)),
        ) else {
            continue;
        };
        func.instruction(&Instruction::LocalGet(off_idx));
        func.instruction(&Instruction::I32Const(crate::ir::STRING_LEN_PREFIX as i32));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        func.instruction(&Instruction::LocalSet(len_idx));
    }

    // Compile the function body
    compile_body(&ir_func.body, &mut func, ctx, memory_layout);

    // Add default return value if no explicit return. Use the resolved return
    // type (which may have been inferred from the body) so the fall-through
    // value matches the function's declared WASM result. `__init__` falls
    // through to `self` so instantiation receives the instance pointer.
    match return_type {
        IRType::Float => {
            func.instruction(&Instruction::F64Const(0.0_f64.into()));
        }
        _ if ctx.return_self => {
            func.instruction(&Instruction::LocalGet(0));
        }
        _ => {
            func.instruction(&Instruction::I32Const(0));
        }
    }

    func.instruction(&Instruction::End);

    func
}

/// Map a built-in exception type name to the integer code used by the
/// try/except dispatch. Shared by `raise` and the handler matching so the two
/// always agree. Unknown names get a sentinel that no specific handler matches.
/// Type code for a bare `raise` and for exception types the table does not
/// name. It has to be nonzero: 0 is what the pending-exception global holds
/// when nothing is in flight.
const GENERIC_EXCEPTION: i32 = 99;

fn exception_type_code(name: &str) -> i32 {
    match name {
        "ZeroDivisionError" => 1,
        "ValueError" => 2,
        "TypeError" => 3,
        "KeyError" => 4,
        "IndexError" => 5,
        "AttributeError" => 6,
        "RuntimeError" => 7,
        "StopIteration" => 8,
        // Anything else, a user-defined exception class above all, gets a
        // stable code derived from its name, so two different ones do not
        // share a code and catch each other. `raise` and `except` both come
        // through here, so they agree by construction.
        other => name_code(other),
    }
}

/// A stable, positive code for an exception name outside the table, kept well
/// clear of the fixed codes and of [`GENERIC_EXCEPTION`].
fn name_code(name: &str) -> i32 {
    // FNV-1a, then folded into the positive range starting at 1000.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    1000 + (hash % 1_000_000) as i32
}

/// Push this function's default result value, the value a frame returns while
/// unwinding: nothing observes it, because every caller checks the pending
/// exception before using a result.
pub(crate) fn emit_default_result(func: &mut Function, ctx: &CompilationContext) {
    match ctx.current_return_type {
        IRType::Float => {
            func.instruction(&Instruction::F64Const(0.0_f64.into()));
        }
        IRType::None => {}
        _ => {
            func.instruction(&Instruction::I32Const(0));
        }
    }
}

/// Transfer control to wherever a pending exception has to go next.
///
/// Inside a `try`, that is the innermost enclosing `try` body's exception
/// block, whose end is the handler dispatch. Outside every `try`, the
/// exception leaves the function: the frame returns its default value and the
/// caller (which checked after the call) carries on unwinding. With no user
/// call below this frame the exception has escaped the program, so it traps
/// there rather than handing the host a value as if nothing had happened.
///
/// `extra_depth` is how many block frames the caller has opened since
/// `ctx.block_depth` was last updated (an `if` wrapping the check, typically),
/// so the branch target is counted from the right place.
pub(crate) fn emit_exception_transfer(
    func: &mut Function,
    ctx: &CompilationContext,
    extra_depth: u32,
) {
    if let Some(level) = ctx.try_stack.last().copied() {
        func.instruction(&Instruction::Br(ctx.block_depth + extra_depth - level));
        return;
    }

    func.instruction(&Instruction::GlobalGet(CALL_DEPTH_GLOBAL));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Unreachable);
    func.instruction(&Instruction::End);
    emit_default_result(func, ctx);
    func.instruction(&Instruction::Return);
}

/// Raise `name` from the point of the call: record the type and transfer
/// control, exactly as a `raise` statement does. `extra_depth` counts the block
/// frames the caller has opened since `ctx.block_depth` was last updated.
pub(crate) fn emit_raise(
    func: &mut Function,
    ctx: &CompilationContext,
    name: &str,
    extra_depth: u32,
) {
    func.instruction(&Instruction::I32Const(exception_type_code(name)));
    func.instruction(&Instruction::GlobalSet(EXC_TYPE_GLOBAL));
    emit_exception_transfer(func, ctx, extra_depth);
}

/// The check emitted after a call that can raise: if the callee left an
/// exception pending, keep unwinding instead of using its result.
pub(crate) fn emit_post_call_check(func: &mut Function, ctx: &CompilationContext) {
    func.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
    func.instruction(&Instruction::If(BlockType::Empty));
    // The `if` frame itself is one level deeper than the recorded block depth.
    emit_exception_transfer(func, ctx, 1);
    func.instruction(&Instruction::End);
}

/// Maintain the call-depth counter around a call that can raise. `delta` is +1
/// before the call and -1 after it.
pub(crate) fn emit_call_depth_step(func: &mut Function, delta: i32) {
    func.instruction(&Instruction::GlobalGet(CALL_DEPTH_GLOBAL));
    func.instruction(&Instruction::I32Const(delta));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::GlobalSet(CALL_DEPTH_GLOBAL));
}

/// The static type of a plain path (a name, or attributes off one) without
/// emitting any code for it: a local's type, then each field's type in turn.
fn static_path_type(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRType> {
    match expr {
        IRExpr::Variable(name) | IRExpr::Param(name) => {
            ctx.get_local_info(name).map(|info| info.var_type.clone())
        }
        IRExpr::Attribute { object, attribute } => match static_path_type(object, ctx)? {
            IRType::Class(class_name) => lookup_field(ctx, &class_name, attribute).map(|(_, t)| t),
            _ => None,
        },
        _ => None,
    }
}

/// Resolve a class field to its `(byte offset, value type)`, if known.
pub(crate) fn lookup_field(
    ctx: &CompilationContext,
    class_name: &str,
    field: &str,
) -> Option<(u64, IRType)> {
    let class_info = ctx.get_class_info(class_name)?;
    let offset = *class_info.field_offsets.get(field)?;
    let ty = class_info
        .field_types
        .get(field)
        .cloned()
        .unwrap_or(IRType::Unknown);
    Some((offset, ty))
}

/// Store instruction for a field of the given type (f64 for floats, i32 else).
fn store_field_instr(ty: &IRType, offset: u64) -> Instruction<'static> {
    let mem = MemArg {
        offset,
        align: if matches!(ty, IRType::Float) { 3 } else { 2 },
        memory_index: 0,
    };
    if matches!(ty, IRType::Float) {
        Instruction::F64Store(mem)
    } else {
        Instruction::I32Store(mem)
    }
}

/// Load instruction for a field of the given type (f64 for floats, i32 else).
pub(crate) fn load_field_instr(ty: &IRType, offset: u64) -> Instruction<'static> {
    let mem = MemArg {
        offset,
        align: if matches!(ty, IRType::Float) { 3 } else { 2 },
        memory_index: 0,
    };
    if matches!(ty, IRType::Float) {
        Instruction::F64Load(mem)
    } else {
        Instruction::I32Load(mem)
    }
}

/// Get the type of a local variable by its index
fn get_local_type_by_index(ctx: &CompilationContext, index: u32) -> IRType {
    for local_info in ctx.locals_map.values() {
        if local_info.index == index {
            return local_info.var_type.clone();
        }
    }
    IRType::Int // Default to i32
}

/// Reserve a named local if it has not been allocated yet. Used by the scan to
/// pre-declare the compiler's internal helper locals (exception state, context
/// managers, ...) so codegen never adds locals after the function's local
/// vector is fixed.
fn ensure_local(ctx: &mut CompilationContext, name: &str, var_type: IRType) {
    if ctx.get_local_index(name).is_none() {
        ctx.add_local(name, var_type);
    }
}

/// Best-effort type inference for an unannotated assignment value. Used to
/// decide a local's WASM value type (f64 vs i32) and to recognise string/bytes
/// locals so a companion length local can be reserved for them. It only needs
/// to recognise float- and string/bytes-producing expressions confidently;
/// anything else is left `Unknown` (an i32 slot, which collections and pointers
/// also use).
fn infer_value_type(value: &IRExpr, ctx: &CompilationContext) -> IRType {
    match value {
        IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
        IRExpr::Const(IRConstant::String(_)) => IRType::String,
        IRExpr::Const(IRConstant::Bytes(_)) => IRType::Bytes,
        IRExpr::BinaryOp { left, right, op } => {
            let lt = infer_value_type(left, ctx);
            let rt = infer_value_type(right, ctx);
            if lt == IRType::Float || rt == IRType::Float {
                IRType::Float
            } else if matches!(op, IROp::Add) && matches!(lt, IRType::String | IRType::Bytes) {
                // String/bytes concatenation yields the same kind.
                lt
            } else {
                IRType::Unknown
            }
        }
        IRExpr::UnaryOp { operand, .. } => infer_value_type(operand, ctx),
        // A tuple carries one type per position. `for k, v in pairs` needs
        // these so a string member binds as a string rather than a bare word.
        IRExpr::TupleLiteral(items) => {
            IRType::Tuple(items.iter().map(|e| infer_value_type(e, ctx)).collect())
        }
        // Float-valued stdlib constants (e.g. `math.pi`, `math.e`) must make
        // their local an f64; otherwise the f64 store lands in an i32 slot.
        IRExpr::Attribute { object, attribute } => match object.as_ref() {
            IRExpr::Variable(module)
                if matches!(
                    crate::stdlib::get_stdlib_attributes(module, attribute),
                    Some(crate::stdlib::StdlibValue::Float(_))
                ) =>
            {
                IRType::Float
            }
            _ => IRType::Unknown,
        },
        // Slicing a string/bytes yields the same kind; indexing a string yields
        // a one-character string (bytes/list indexing yields a scalar).
        IRExpr::Slicing { container, .. } => match infer_value_type(container, ctx) {
            t @ (IRType::String | IRType::Bytes) => t,
            _ => IRType::Unknown,
        },
        IRExpr::Indexing { container, .. }
            if infer_value_type(container, ctx) == IRType::String =>
        {
            IRType::String
        }
        IRExpr::Variable(name) => ctx
            .get_local_info(name)
            .map(|info| info.var_type.clone())
            .unwrap_or(IRType::Unknown),
        // A comprehension's element type isn't resolved until codegen, but the
        // result is always a pointer-shaped collection (an i32 slot), which is
        // what the local's WASM type needs to know.
        IRExpr::Comprehension { kind, .. } => match kind {
            crate::ir::IRComprehensionKind::List => IRType::List(Box::new(IRType::Unknown)),
            crate::ir::IRComprehensionKind::Set => IRType::Set(Box::new(IRType::Unknown)),
            crate::ir::IRComprehensionKind::Dict => {
                IRType::Dict(Box::new(IRType::Unknown), Box::new(IRType::Unknown))
            }
        },
        // A collection literal's element type decides how its elements are
        // loaded back out (`for x in xs` over a float list must bind an f64
        // loop variable), so keep the element type instead of collapsing the
        // literal to a bare pointer.
        IRExpr::ListLiteral(elems) => IRType::List(Box::new(literal_elem_type(elems, ctx))),
        IRExpr::SetLiteral(elems) => IRType::Set(Box::new(literal_elem_type(elems, ctx))),
        IRExpr::FunctionCall { function_name, .. } if function_name == "float" => IRType::Float,
        // `open()` yields a file handle; its local must be typed so file
        // method calls dispatch to the host I/O lowering.
        IRExpr::FunctionCall { function_name, .. }
            if function_name == "open" && ctx.get_function_info("open").is_none() =>
        {
            IRType::File
        }
        // `c = C()` types the local as an instance, which is what lets a later
        // `c.method()` resolve the class it belongs to.
        IRExpr::FunctionCall { function_name, .. }
            if ctx.get_class_info(function_name).is_some() =>
        {
            IRType::Class(function_name.clone())
        }
        IRExpr::FunctionCall { function_name, .. } => ctx
            .get_function_info(function_name)
            .map(|f| f.return_type.clone())
            .filter(|t| *t == IRType::Float)
            .unwrap_or(IRType::Unknown),
        // An unannotated local assigned from a method call takes the method's
        // declared return type. Without this an f64-returning method stored
        // into an i32 local produced a module Binaryen rejected outright.
        IRExpr::MethodCall {
            object,
            method_name,
            ..
        } => method_return_type(object, method_name, ctx).unwrap_or(IRType::Unknown),
        _ => IRType::Unknown,
    }
}

/// The type of the value a `a, b = value` statement unpacks, as far as the
/// scan can tell before any code is generated: everything
/// [`infer_value_type`] knows, plus a user function's declared return type,
/// `xs.pop()` and `xs[i]` on a typed list, and a typed local.
fn infer_unpack_source(value: &IRExpr, ctx: &CompilationContext) -> IRType {
    let direct = infer_value_type(value, ctx);
    if !matches!(direct, IRType::Unknown) {
        return direct;
    }
    let list_elem = |expr: &IRExpr| match expr {
        IRExpr::Variable(name) | IRExpr::Param(name) => match ctx.get_local_info(name) {
            Some(info) => match &info.var_type {
                IRType::List(e) => Some(e.as_ref().clone()),
                _ => None,
            },
            None => None,
        },
        _ => None,
    };
    match value {
        IRExpr::FunctionCall { function_name, .. } => ctx
            .get_function_info(function_name)
            .map(|f| f.return_type.clone())
            .unwrap_or(IRType::Unknown),
        IRExpr::MethodCall {
            object,
            method_name,
            ..
        } if method_name == "pop" => list_elem(object).unwrap_or(IRType::Unknown),
        IRExpr::Indexing { container, .. } => list_elem(container).unwrap_or(IRType::Unknown),
        IRExpr::Param(name) => ctx
            .get_local_info(name)
            .map(|info| info.var_type.clone())
            .unwrap_or(IRType::Unknown),
        _ => IRType::Unknown,
    }
}

/// The type of target `i` of `count` when unpacking a value of type `source`.
fn unpack_member_type(source: &IRType, i: usize, count: usize, starred: Option<usize>) -> IRType {
    match source {
        IRType::Tuple(members) if starred.is_none() && members.len() == count => members[i].clone(),
        IRType::Tuple(members) if starred.is_some() => {
            let star = starred.unwrap_or(0);
            let after = count - 1 - star;
            if i < star {
                members.get(i).cloned().unwrap_or(IRType::Unknown)
            } else {
                members
                    .len()
                    .checked_sub(after - (i - star - 1))
                    .and_then(|j| members.get(j).cloned())
                    .unwrap_or(IRType::Unknown)
            }
        }
        IRType::List(elem) => elem.as_ref().clone(),
        _ => IRType::Unknown,
    }
}

/// Declared return type of `object.method()`, when the receiver's class is
/// known. Inherited methods are registered under the class that defines them,
/// so the lookup goes through `method_owner` the same way codegen's dispatch
/// does.
fn method_return_type(
    object: &IRExpr,
    method_name: &str,
    ctx: &CompilationContext,
) -> Option<IRType> {
    let class_name = match object {
        IRExpr::Variable(name) => match ctx.get_local_info(name)?.var_type.clone() {
            IRType::Class(class_name) => class_name,
            _ => return None,
        },
        IRExpr::Attribute { object, attribute } => {
            let IRExpr::Variable(obj_name) = object.as_ref() else {
                return None;
            };
            let IRType::Class(owner) = ctx.get_local_info(obj_name)?.var_type.clone() else {
                return None;
            };
            match lookup_field(ctx, &owner, attribute)?.1 {
                IRType::Class(class_name) => class_name,
                _ => return None,
            }
        }
        _ => return None,
    };
    let class_info = ctx.get_class_info(&class_name)?;
    let owner = class_info
        .method_owner
        .get(method_name)
        .cloned()
        .unwrap_or(class_name);
    Some(
        ctx.get_function_info(&format!("{owner}::{method_name}"))?
            .return_type
            .clone(),
    )
}

/// Fill an element-less collection annotation from the assigned value's
/// inferred type. A bare `xs: list = [1.5, 2.5]` annotates as `List(Unknown)`,
/// which would lose the float element type that `List[float]` carries and bind
/// `for x in xs` as an i32. Anything the annotation states concretely wins.
fn refine_annotation(annotated: IRType, inferred: &IRType) -> IRType {
    match (&annotated, inferred) {
        (IRType::List(elem), IRType::List(_)) | (IRType::Set(elem), IRType::Set(_))
            if **elem == IRType::Unknown =>
        {
            inferred.clone()
        }
        _ => annotated,
    }
}

/// Element type of a collection literal: the type shared by every element, or
/// `Unknown` for an empty or mixed literal. A mixed literal has no single WASM
/// slot width, so it keeps the i32 default rather than mis-typing some elements.
fn literal_elem_type(elems: &[IRExpr], ctx: &CompilationContext) -> IRType {
    let mut types = elems.iter().map(|e| infer_value_type(e, ctx));
    let Some(first) = types.next() else {
        return IRType::Unknown;
    };
    if types.all(|t| t == first) {
        first
    } else {
        IRType::Unknown
    }
}

/// Best-effort element type of a `for`-loop iterable, used to decide whether the
/// loop variable must be an f64 local. Literal iterables and collection locals
/// whose element type the assignment scan resolved (including
/// `for x in <float-list-variable>`) are recognised; anything else is `Unknown`,
/// which keeps the i32 binding.
fn infer_iterable_elem_type(iterable: &IRExpr, ctx: &CompilationContext) -> IRType {
    match iterable {
        IRExpr::ListLiteral(elems) | IRExpr::SetLiteral(elems) => literal_elem_type(elems, ctx),
        IRExpr::Variable(name) => match ctx.get_local_info(name).map(|i| i.var_type.clone()) {
            Some(IRType::List(t)) | Some(IRType::Set(t)) => *t,
            // `for k in d` binds the dict's keys.
            Some(IRType::Dict(k, _)) => *k,
            _ => IRType::Unknown,
        },
        // `for c, w in top_words(...)`: the callee's declared return type.
        // Without this a comprehension over a call binds untyped targets, so a
        // string member rendered as its pointer inside an f-string.
        IRExpr::FunctionCall { function_name, .. } => {
            match ctx
                .get_function_info(function_name)
                .map(|f| f.return_type.clone())
            {
                Some(IRType::List(t)) | Some(IRType::Set(t)) => *t,
                Some(IRType::Dict(k, _)) => *k,
                _ => IRType::Unknown,
            }
        }
        // `for x in self.items`: the field's declared element type.
        IRExpr::Attribute { object, attribute } => {
            let IRExpr::Variable(obj_name) = object.as_ref() else {
                return IRType::Unknown;
            };
            let Some(IRType::Class(owner)) =
                ctx.get_local_info(obj_name).map(|i| i.var_type.clone())
            else {
                return IRType::Unknown;
            };
            match lookup_field(ctx, &owner, attribute).map(|(_, ty)| ty) {
                Some(IRType::List(t)) | Some(IRType::Set(t)) => *t,
                _ => IRType::Unknown,
            }
        }
        _ => IRType::Unknown,
    }
}

/// Resolve a function's WASM result type. An explicit annotation wins; otherwise
/// the type is inferred from the body's `return` statements so that, e.g., a
/// function returning `math.pi` gets an f64 result instead of a default i32 (an
/// f64 return value into an i32 result fails validation and aborts Binaryen).
///
/// `known_returns` carries the already-resolved return types of other functions
/// so a `return some_call()` resolves; callees defined earlier are resolved
/// first, and a second resolution pass handles forward references.
pub(crate) fn resolve_return_type(
    ir_func: &IRFunction,
    known_returns: &std::collections::HashMap<String, IRType>,
) -> IRType {
    if !matches!(ir_func.return_type, IRType::Unknown) {
        return ir_func.return_type.clone();
    }

    // Build a scratch context with the params and known function return types,
    // then run the local scan so local types (including float stdlib constants)
    // are available to the return-expression inference.
    let mut ctx = CompilationContext::new();
    for (name, ret) in known_returns {
        ctx.add_function(name, 0, Vec::new(), ret.clone());
    }
    for param in &ir_func.params {
        ctx.add_local(&param.name, param.param_type.clone());
    }
    ctx.for_loop_seq = 0;
    scan_and_allocate_locals(&ir_func.body, &mut ctx);

    let mut inferred = IRType::Unknown;
    collect_return_type(&ir_func.body, &ctx, &mut inferred);
    inferred
}

/// Fold the inferred types of a body's `return` expressions into `out`. A float
/// return forces an f64 result; otherwise the first concrete type seen wins.
fn collect_return_type(body: &IRBody, ctx: &CompilationContext, out: &mut IRType) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Return(Some(expr)) => {
                let t = infer_value_type(expr, ctx);
                if t == IRType::Float {
                    *out = IRType::Float;
                } else if matches!(out, IRType::Unknown) && !matches!(t, IRType::Unknown) {
                    *out = t;
                }
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_return_type(then_body, ctx, out);
                if let Some(else_body) = else_body {
                    collect_return_type(else_body, ctx, out);
                }
            }
            IRStatement::While { body, .. } => collect_return_type(body, ctx, out),
            IRStatement::For { body, .. } => collect_return_type(body, ctx, out),
            _ => {}
        }
    }
}

/// Reserve the helper locals a comprehension needs (result pointer, write
/// index, capacity, iterator state per generator, and the generator target
/// variables themselves), then recurse into its subexpressions one nesting
/// level deeper. Locals are keyed by comprehension nesting depth — codegen
/// tracks the same depth in `ctx.comp_depth` — so sibling comprehensions share
/// a depth's locals (their evaluations never overlap) while nested ones get
/// their own.
fn scan_expr_locals(expr: &IRExpr, ctx: &mut CompilationContext, depth: u32) {
    match expr {
        // A closure environment read touches no locals of its own.
        IRExpr::EnvRead { .. } | IRExpr::CellNew | IRExpr::CellLoad { .. } => {}
        IRExpr::CellStore { value, .. } => scan_expr_locals(value, ctx, 0),
        IRExpr::Comprehension {
            kind,
            element,
            value,
            generators,
        } => {
            ensure_local(ctx, &comp_local_name("res", depth), IRType::Int);
            ensure_local(ctx, &comp_local_name("widx", depth), IRType::Int);
            ensure_local(ctx, &comp_local_name("cap", depth), IRType::Int);
            ensure_local(ctx, &comp_local_name("elem", depth), IRType::Int);
            if matches!(kind, crate::ir::IRComprehensionKind::Set) {
                ensure_local(ctx, &comp_local_name("mask", depth), IRType::Int);
                ensure_local(ctx, &comp_local_name("hidx", depth), IRType::Int);
                ensure_local(ctx, &comp_local_name("bkt", depth), IRType::Int);
            }
            for (g, generator) in generators.iter().enumerate() {
                ensure_local(ctx, &comp_gen_local_name("ptr", depth, g), IRType::Int);
                ensure_local(ctx, &comp_gen_local_name("idx", depth, g), IRType::Int);
                ensure_local(ctx, &comp_gen_local_name("len", depth, g), IRType::Int);
                // The loop target binds each element, so a float iterable needs
                // an f64 local (same rule as the `for` statement's target).
                if let [target] = generator.targets.as_slice() {
                    // The target takes the element's type where it can be
                    // inferred: an f64 element needs an f64 local, and a string
                    // element needs a `String` local so `len(w)` reads the
                    // companion length rather than treating the offset as a
                    // collection pointer.
                    let target_ty = match infer_iterable_elem_type(&generator.iterable, ctx) {
                        ty @ (IRType::Float | IRType::String | IRType::Bytes) => ty,
                        class @ IRType::Class(_) => class,
                        _ => IRType::Unknown,
                    };
                    ensure_local(ctx, target, target_ty);
                    // A string element binds as an (offset, length) pair, and
                    // the local vector is fixed before codegen runs, so the
                    // companion has to be reserved here even though the element
                    // type is not known until then.
                    ensure_local(ctx, &strlen_local_name(target), IRType::Int);
                } else {
                    // `for k, v in pairs`: each target takes its own position's
                    // type from the tuple, so a string member is a `String`
                    // local rather than an untyped word.
                    let member_types = match infer_iterable_elem_type(&generator.iterable, ctx) {
                        IRType::Tuple(types) if types.len() == generator.targets.len() => {
                            Some(types)
                        }
                        _ => None,
                    };
                    for (j, target) in generator.targets.iter().enumerate() {
                        let ty = match &member_types {
                            Some(types) => match &types[j] {
                                t @ (IRType::String | IRType::Bytes | IRType::Float) => t.clone(),
                                class @ IRType::Class(_) => class.clone(),
                                _ => IRType::Unknown,
                            },
                            None => IRType::Unknown,
                        };
                        ensure_local(ctx, target, ty);
                        // A string element binds as an (offset, length) pair,
                        // and the local vector is fixed before codegen runs, so
                        // the companion has to be reserved here even though the
                        // element type is not known until then.
                        ensure_local(ctx, &strlen_local_name(target), IRType::Int);
                    }
                }
                scan_expr_locals(&generator.iterable, ctx, depth + 1);
                for condition in &generator.conditions {
                    scan_expr_locals(condition, ctx, depth + 1);
                }
            }
            scan_expr_locals(element, ctx, depth + 1);
            if let Some(value) = value {
                scan_expr_locals(value, ctx, depth + 1);
            }
        }
        // A module-level variable read is inlined as its initializer at
        // codegen, so any comprehension inside that initializer needs its
        // locals reserved here too. Locals shadow module vars, and a module
        // var's initializer cannot reference itself, so one level of lookup
        // (with the recursion below) suffices.
        IRExpr::Variable(name) => {
            if ctx.get_local_index(name).is_none() && !ctx.module_global_index.contains_key(name) {
                if let Some((_, init)) = ctx.get_module_var(name) {
                    let init = init.clone();
                    scan_expr_locals(&init, ctx, depth);
                }
            }
        }
        IRExpr::Const(_) | IRExpr::Param(_) => {}
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            scan_expr_locals(left, ctx, depth);
            scan_expr_locals(right, ctx, depth);
        }
        IRExpr::UnaryOp { operand, .. } => scan_expr_locals(operand, ctx, depth),
        IRExpr::FunctionCall { arguments, .. } => {
            for arg in arguments {
                scan_expr_locals(arg, ctx, depth);
            }
        }
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            for item in items {
                scan_expr_locals(item, ctx, depth);
            }
        }
        IRExpr::DictLiteral(entries) => {
            for (key, value) in entries {
                scan_expr_locals(key, ctx, depth);
                scan_expr_locals(value, ctx, depth);
            }
        }
        IRExpr::Indexing { container, index } => {
            scan_expr_locals(container, ctx, depth);
            scan_expr_locals(index, ctx, depth);
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            scan_expr_locals(container, ctx, depth);
            for bound in [start, end, step].into_iter().flatten() {
                scan_expr_locals(bound, ctx, depth);
            }
        }
        IRExpr::Attribute { object, .. } => scan_expr_locals(object, ctx, depth),
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            scan_expr_locals(object, ctx, depth);
            for arg in arguments {
                scan_expr_locals(arg, ctx, depth);
            }
        }
        IRExpr::DynamicImportExpr { module_name } => scan_expr_locals(module_name, ctx, depth),
        IRExpr::RangeCall { start, stop, step } => {
            for bound in [start, step].into_iter().flatten() {
                scan_expr_locals(bound, ctx, depth);
            }
            scan_expr_locals(stop, ctx, depth);
        }
        IRExpr::Lambda { body, .. } => scan_expr_locals(body, ctx, depth),
        // Captured names reference existing locals; nothing to reserve.
        IRExpr::ClosureMake { .. } => {}
    }
}

/// Run [`scan_expr_locals`] over every expression embedded in a statement.
/// Nested statement bodies are *not* visited here — `scan_and_allocate_locals`
/// already recurses into them and calls this for each inner statement.
fn scan_stmt_exprs(stmt: &IRStatement, ctx: &mut CompilationContext) {
    match stmt {
        IRStatement::Return(Some(expr))
        | IRStatement::Expression(expr)
        | IRStatement::Assign { value: expr, .. }
        | IRStatement::AugAssign { value: expr, .. }
        | IRStatement::TupleUnpack { value: expr, .. }
        | IRStatement::If {
            condition: expr, ..
        }
        | IRStatement::While {
            condition: expr, ..
        }
        | IRStatement::For { iterable: expr, .. }
        | IRStatement::Raise {
            exception: Some(expr),
        }
        | IRStatement::With {
            context_expr: expr, ..
        }
        | IRStatement::DynamicImport {
            module_name: expr, ..
        }
        | IRStatement::Yield { value: Some(expr) } => scan_expr_locals(expr, ctx, 0),
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            scan_expr_locals(object, ctx, 0);
            scan_expr_locals(value, ctx, 0);
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            scan_expr_locals(container, ctx, 0);
            scan_expr_locals(index, ctx, 0);
            scan_expr_locals(value, ctx, 0);
        }
        _ => {}
    }
}

/// Scan the function body for variable declarations and allocate local variables
pub fn scan_and_allocate_locals(body: &IRBody, ctx: &mut CompilationContext) {
    for stmt in &body.statements {
        scan_stmt_exprs(stmt, ctx);
        match stmt {
            IRStatement::Assign { target, .. }
                if ctx.in_module_init && ctx.module_global_index.contains_key(target) => {}
            IRStatement::Assign {
                target,
                var_type,
                value,
            } => {
                if ctx.get_local_index(target).is_none() {
                    // Use the annotation if present; otherwise infer the type
                    // from the value so unannotated float locals become f64.
                    let inferred = infer_value_type(value, ctx);
                    let var_type = var_type
                        .clone()
                        .map(|annotated| refine_annotation(annotated, &inferred))
                        .unwrap_or(inferred);
                    // String/bytes locals carry an (offset, length) pair, so they
                    // need a companion local for the length. Reserve one for
                    // `Unknown` locals too: a stdlib call like `os.path.join`
                    // infers as `Unknown` here but is upgraded to `String` during
                    // codegen, and the companion can't be added after the local
                    // vector is fixed.
                    let needs_companion =
                        matches!(var_type, IRType::String | IRType::Bytes | IRType::Unknown);
                    ctx.add_local(target, var_type);
                    if needs_companion {
                        ctx.add_local(&strlen_local_name(target), IRType::Int);
                    }
                }
            }
            IRStatement::TupleUnpack {
                targets,
                starred,
                value,
            } => {
                // Each target takes its member's type, which is what lets a
                // float member get an f64 local (it used to bind as its low 32
                // bits) and a string or tuple member be used as one.
                let source = infer_unpack_source(value, ctx);
                for (i, target) in targets.iter().enumerate() {
                    if ctx.get_local_index(target).is_none() {
                        // The starred target collects the middle elements as a
                        // list, so type it as one for later len()/indexing.
                        let ty = if Some(i) == *starred {
                            match &source {
                                IRType::List(e) => IRType::List(e.clone()),
                                _ => IRType::List(Box::new(IRType::Unknown)),
                            }
                        } else {
                            unpack_member_type(&source, i, targets.len(), *starred)
                        };
                        ctx.add_local(target, ty);
                    }
                }
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                scan_and_allocate_locals(then_body, ctx);
                if let Some(else_body) = else_body {
                    scan_and_allocate_locals(else_body, ctx);
                }
            }
            IRStatement::While { body, .. } => {
                scan_and_allocate_locals(body, ctx);
            }
            IRStatement::For {
                target,
                iterable,
                body,
                else_body,
            } => {
                // Allocate the loop variable. Iterating a float collection binds
                // each element as an f64, so the loop variable must be a float
                // local; otherwise its WASM type (fixed here) would mismatch the
                // f64 the element load pushes. Only literal iterables are typed
                // confidently at scan time (list vars are still Unknown here), so
                // `for x in some_float_list` remains an i32 bind for now.
                if ctx.get_local_index(target).is_none() {
                    let target_ty = match infer_iterable_elem_type(iterable, ctx) {
                        IRType::Float => IRType::Float,
                        // Iterating a list of instances keeps the element's
                        // class, so `for it in items: it.field` resolves the
                        // field instead of reading 0 off an untyped pointer.
                        class @ IRType::Class(_) => class,
                        _ => IRType::Unknown,
                    };
                    let target_ty_for_companion = target_ty.clone();
                    ctx.add_local(target, target_ty);
                    // A string element binds as an (offset, length) pair, the
                    // same as an assignment target, so it needs the companion
                    // length local. Without it a read of the loop variable
                    // pushed one word where the rest of codegen expects two,
                    // and using it as a dict key dropped the offset instead of
                    // the length. The element type is not known until codegen,
                    // so the companion is reserved for every non-float target.
                    if !matches!(target_ty_for_companion, IRType::Float) {
                        ctx.add_local(&strlen_local_name(target), IRType::Int);
                    }
                }
                // Reserve this loop's iterator helper locals up front (codegen
                // can't add locals after the function's local vector is fixed).
                // Keyed by sequence number so nested loops get distinct locals.
                let seq = ctx.for_loop_seq;
                ctx.for_loop_seq += 1;
                ctx.add_local(&format!("__iter_ptr_{seq}"), IRType::Unknown);
                ctx.add_local(&format!("__iter_idx_{seq}"), IRType::Int);
                ctx.add_local(&format!("__iter_len_{seq}"), IRType::Int);
                scan_and_allocate_locals(body, ctx);
                if let Some(else_body) = else_body {
                    scan_and_allocate_locals(else_body, ctx);
                }
            }
            IRStatement::Raise { .. } => {
                // Raise uses the shared exception-state locals; reserve them so
                // codegen never has to add locals after the local set is fixed.
                ensure_local(ctx, "__exception_flag", IRType::Int);
                ensure_local(ctx, "__exception_type", IRType::Int);
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                ensure_local(ctx, "__exception_flag", IRType::Int);
                ensure_local(ctx, "__exception_type", IRType::Int);
                scan_and_allocate_locals(try_body, ctx);

                for handler in except_handlers {
                    // Allocate exception variable if it exists
                    if let Some(name) = &handler.name {
                        if ctx.get_local_index(name).is_none() {
                            ctx.add_local(name, IRType::Unknown);
                        }
                    }
                    scan_and_allocate_locals(&handler.body, ctx);
                }

                if let Some(finally_body) = finally_body {
                    scan_and_allocate_locals(finally_body, ctx);
                }
            }
            // No `IRStatement::With` arm: `ir::context_managers` rewrites every
            // `with` into `__enter__`/`__exit__` calls over ordinary
            // assignments before the compiler sees the body.
            _ => {}
        }
    }
}

/// Compile a function body into WebAssembly instructions
pub fn compile_body(
    body: &IRBody,
    func: &mut Function,
    ctx: &mut CompilationContext,
    memory_layout: &MemoryLayout,
) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Return(expr_opt) => {
                if ctx.return_self {
                    // Inside `__init__` every return yields `self` (local 0) so
                    // the instantiation site receives the instance pointer.
                    // Python only allows `return` / `return None` here; any
                    // expression is still evaluated for effect, then discarded.
                    if let Some(expr) = expr_opt {
                        let ty = emit_expr(expr, func, ctx, memory_layout, None);
                        match ty {
                            IRType::None => {}
                            IRType::String | IRType::Bytes => {
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            _ => {
                                func.instruction(&Instruction::Drop);
                            }
                        }
                    }
                    func.instruction(&Instruction::LocalGet(0));
                } else if let Some(expr) = expr_opt {
                    // The declared return type is the expected type, so a value
                    // of the other numeric width is converted rather than
                    // returned as-is. `return 10 / n` from a function declared
                    // `-> int` is the case that matters: true division makes
                    // that an f64, and an f64 in an i32 result does not
                    // validate.
                    let ret_ty = ctx.current_return_type.clone();
                    let ty = emit_expr(expr, func, ctx, memory_layout, Some(&ret_ty));
                    match (&ret_ty, &ty) {
                        (IRType::Int | IRType::Bool, IRType::Float) => {
                            func.instruction(&Instruction::I32TruncF64S);
                        }
                        (IRType::Float, IRType::Int | IRType::Bool) => {
                            func.instruction(&Instruction::F64ConvertI32S);
                        }
                        // A string/bytes value is an (offset, length) pair, but
                        // a function returns a single word. Drop the length (on
                        // top) and return the offset; the length-prefixed blob
                        // lets a caller recover the length via
                        // `load(offset - 4)`.
                        (_, IRType::String | IRType::Bytes) => {
                            func.instruction(&Instruction::Drop);
                        }
                        _ => {}
                    }
                } else {
                    func.instruction(&Instruction::I32Const(0));
                }
                func.instruction(&Instruction::Return);
            }
            IRStatement::Assign {
                target,
                value,
                var_type,
            } if ctx.in_module_init && ctx.module_global_index.contains_key(target) => {
                // A module definition evaluated once: store it in its global
                // and record the type every reader will see.
                let global = ctx.module_global_index[target];
                let emitted = emit_expr(value, func, ctx, memory_layout, var_type.as_ref());
                let ty = match var_type {
                    Some(declared) if !matches!(declared, IRType::Unknown) => declared.clone(),
                    _ => emitted.clone(),
                };
                if matches!(emitted, IRType::String | IRType::Bytes) {
                    func.instruction(&Instruction::Drop); // the length; the offset carries it
                }
                if matches!(ty, IRType::Float) != matches!(emitted, IRType::Float) {
                    ctx.report(format!(
                        "module-level '{target}' is declared {} but its value is a {}",
                        crate::type_to_string(&ty),
                        crate::type_to_string(&emitted)
                    ));
                }
                func.instruction(&Instruction::GlobalSet(global));
                ctx.module_global_types.insert(target.clone(), ty);
            }
            IRStatement::Assign {
                target,
                value,
                var_type,
            } => {
                // Get the expected type for the assignment
                let expected_type = var_type
                    .as_ref()
                    .cloned()
                    .or_else(|| ctx.get_local_info(target).map(|info| info.var_type.clone()));

                // Emit code for the value
                let value_type = emit_expr(value, func, ctx, memory_layout, expected_type.as_ref());

                // An unannotated local is allocated as Unknown (an i32 slot).
                // Recover the element/entry types of collections so later
                // indexing knows how to load each slot. Only types that live in
                // that same i32 slot are adopted, so the already-fixed local
                // layout is undisturbed: every pointer-shaped type, plus `int`
                // and `bool`. (`float` is an f64 slot and cannot be adopted
                // here.) A local the scan could only type as a collection of
                // Unknown (e.g. a comprehension result, whose element type is
                // resolved during codegen) is upgraded the same way once the
                // emitted value reports the concrete element type.
                //
                // Adopting `int` is what lets a value pass through a local and
                // still be widened when it is written into a float collection:
                // `n = 3` followed by `xs.append(n)` used to reach the write
                // path as Unknown, which is one word wide, and stored 4 bytes
                // into an 8-byte slot (#123).
                if let Some(info) = ctx.locals_map.get_mut(target) {
                    let adopt = match (&info.var_type, &value_type) {
                        (IRType::Unknown, _) => matches!(
                            value_type,
                            IRType::List(_)
                                | IRType::Tuple(_)
                                | IRType::Dict(_, _)
                                | IRType::Set(_)
                                | IRType::String
                                | IRType::Bytes
                                | IRType::Class(_)
                                | IRType::Int
                                | IRType::Bool
                        ),
                        (IRType::List(elem), IRType::List(_))
                        | (IRType::Set(elem), IRType::Set(_)) => **elem == IRType::Unknown,
                        (IRType::Dict(key, val), IRType::Dict(_, _)) => {
                            **key == IRType::Unknown && **val == IRType::Unknown
                        }
                        _ => false,
                    };
                    if adopt {
                        info.var_type = value_type.clone();
                    }
                }

                if let Some(local_idx) = ctx.get_local_index(target) {
                    // A string/bytes value is an (offset, length) pair with the
                    // length on top of the stack. Store the length into the
                    // companion local first, then the offset into the named one.
                    if matches!(value_type, IRType::String | IRType::Bytes) {
                        match ctx.get_local_index(&strlen_local_name(target)) {
                            Some(len_idx) => func.instruction(&Instruction::LocalSet(len_idx)),
                            // No companion was reserved (inference missed this
                            // string local); drop the length to keep the stack
                            // balanced rather than leaving it stranded.
                            None => func.instruction(&Instruction::Drop),
                        };
                    }
                    func.instruction(&Instruction::LocalSet(local_idx));
                } else {
                    // Handle the case where the variable is not found in the context
                    panic!("Variable {target} not found in context");
                }
            }
            IRStatement::TupleUnpack {
                targets,
                value,
                starred,
            } => {
                // Emit code for the value (a tuple or list pointer; both share
                // the [len:i32][slot0][slot1]... layout)
                let value_type = emit_expr(value, func, ctx, memory_layout, None);

                // What each target holds, now that the value's type is known
                // exactly. An untyped target adopts an i32-slot member type
                // (the local's width is fixed, and every such type shares it);
                // a float member needs the f64 local the scan should have
                // allocated, and is refused rather than truncated into an i32.
                let mut target_types = Vec::with_capacity(targets.len());
                for (i, target) in targets.iter().enumerate() {
                    if Some(i) == *starred {
                        target_types.push(IRType::Unknown);
                        continue;
                    }
                    let member = unpack_member_type(&value_type, i, targets.len(), *starred);
                    let local_ty = ctx
                        .get_local_info(target)
                        .map(|info| info.var_type.clone())
                        .unwrap_or(IRType::Unknown);
                    if matches!(member, IRType::Float) && !matches!(local_ty, IRType::Float) {
                        ctx.report(format!(
                            "unpacking a float into '{target}', which holds a {} here. \
                             Hint: annotate the value being unpacked, or the variable",
                            crate::type_to_string(&local_ty)
                        ));
                    }
                    if matches!(local_ty, IRType::Unknown) && !matches!(member, IRType::Float) {
                        if let Some(info) = ctx.locals_map.get_mut(target) {
                            info.var_type = member.clone();
                        }
                    }
                    target_types.push(
                        ctx.get_local_info(target)
                            .map(|info| info.var_type.clone())
                            .unwrap_or(IRType::Unknown),
                    );
                }

                // Keep the pointer, then load the element count
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Bind a target variable from the value on the stack.
                let set_target = |func: &mut Function, ctx: &CompilationContext, target: &str| {
                    if let Some(local_idx) = ctx.get_local_index(target) {
                        func.instruction(&Instruction::LocalSet(local_idx));
                    } else {
                        panic!("Variable {target} not found in context");
                    }
                };

                if let Some(star) = starred {
                    // Extended unpacking `a, *b, c = xs`: the scalars bind
                    // positionally from the front and back, and the starred
                    // target collects the middle as a fresh runtime list.
                    let before = *star;
                    let after = targets.len() - 1 - before;
                    let n = ctx.temp_local + 1;
                    let mid_len = ctx.temp_local + 2;
                    let mid_ptr = ctx.temp_local + 3;
                    func.instruction(&Instruction::LocalSet(n));

                    // Too few values for the targets around the star is a
                    // ValueError in Python; the targets used to read past the end.
                    func.instruction(&Instruction::LocalGet(n));
                    func.instruction(&Instruction::I32Const((before + after) as i32));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    emit_raise(func, ctx, "ValueError", 1);
                    func.instruction(&Instruction::End);

                    // Front targets: element i at HEADER + i*SLOT. (Float
                    // members still bind as their i32 low word — the existing
                    // tuple-unpack limitation.)
                    for (i, target) in targets[..before].iter().enumerate() {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: ((i as u32) * COLLECTION_SLOT) as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        set_target(func, ctx, target);
                    }

                    // mid_len = max(n - before - after, 0); the clamp keeps a
                    // too-short value (a runtime ValueError in Python) from
                    // trapping on a negative-size allocation.
                    func.instruction(&Instruction::LocalGet(n));
                    func.instruction(&Instruction::I32Const((before + after) as i32));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::LocalTee(mid_len));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalGet(mid_len));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32GtS);
                    func.instruction(&Instruction::Select);
                    func.instruction(&Instruction::LocalSet(mid_len));

                    // mid_ptr = __alloc(HEADER + mid_len*SLOT), header = mid_len
                    func.instruction(&Instruction::LocalGet(mid_len));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::Call(ctx.alloc_func_index));
                    func.instruction(&Instruction::LocalSet(mid_ptr));
                    func.instruction(&Instruction::LocalGet(mid_ptr));
                    func.instruction(&Instruction::LocalGet(mid_len));
                    func.instruction(&Instruction::I32Store(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                    // The slice block is sized exactly for its elements, so its
                    // capacity equals its length.
                    func.instruction(&Instruction::LocalGet(mid_ptr));
                    func.instruction(&Instruction::LocalGet(mid_len));
                    func.instruction(&Instruction::I32Store(MemArg {
                        offset: COLLECTION_CAP as u64,
                        align: 2,
                        memory_index: 0,
                    }));
                    // Its elements start immediately after its own header.
                    func.instruction(&Instruction::LocalGet(mid_ptr));
                    func.instruction(&Instruction::LocalGet(mid_ptr));
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Store(MemArg {
                        offset: COLLECTION_DATA as u64,
                        align: 2,
                        memory_index: 0,
                    }));

                    // Slots are contiguous, so the middle slice is one
                    // memory.copy from source slot `before`.
                    func.instruction(&Instruction::LocalGet(mid_ptr));
                    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Load(MemArg {
                        offset: COLLECTION_DATA as u64,
                        align: 2,
                        memory_index: 0,
                    }));
                    func.instruction(&Instruction::I32Const(
                        (before as u32 * COLLECTION_SLOT) as i32,
                    ));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::LocalGet(mid_len));
                    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });

                    func.instruction(&Instruction::LocalGet(mid_ptr));
                    set_target(func, ctx, &targets[before]);

                    // Back targets: element (n - after + k) for the k-th
                    // target after the star; the index is runtime because n is.
                    for (k, target) in targets[before + 1..].iter().enumerate() {
                        func.instruction(&Instruction::LocalGet(n));
                        func.instruction(&Instruction::I32Const(k as i32 - after as i32));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        set_target(func, ctx, target);
                    }
                } else {
                    // A value of the wrong length is a ValueError ("too many" or
                    // "not enough values to unpack"). It was ignored, so the
                    // targets read past the end or silently dropped the rest.
                    func.instruction(&Instruction::I32Const(targets.len() as i32));
                    func.instruction(&Instruction::I32Ne);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    emit_raise(func, ctx, "ValueError", 1);
                    func.instruction(&Instruction::End);

                    // Extract each element from the tuple and assign to target
                    // variables. Element i sits at HEADER + i*SLOT. A float target
                    // would need an f64 load here; tuple-unpack targets are not yet
                    // type-inferred, so float members still bind as their i32 low
                    // word (a documented follow-up, mirroring the loop-var case).
                    for (i, target) in targets.iter().enumerate() {
                        // Load the element block, then the slot inside it.
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));

                        // Add offset to get element (i*SLOT)
                        func.instruction(&Instruction::I32Const(
                            ((i as u32) * COLLECTION_SLOT) as i32,
                        ));
                        func.instruction(&Instruction::I32Add);

                        // Load the element at the target's width: a float
                        // member is a whole f64 slot, anything else its word.
                        let member = unpack_member_type(&value_type, i, targets.len(), None);
                        let target_is_float = matches!(target_types[i], IRType::Float);
                        if target_is_float && matches!(member, IRType::Float) {
                            func.instruction(&Instruction::F64Load(MemArg {
                                offset: 0,
                                align: 2,
                                memory_index: 0,
                            }));
                        } else {
                            func.instruction(&Instruction::I32Load(MemArg {
                                offset: 0,
                                align: 2,
                                memory_index: 0,
                            }));
                            if target_is_float {
                                func.instruction(&Instruction::F64ConvertI32S);
                            }
                        }

                        // Store in target variable
                        set_target(func, ctx, target);
                    }
                }
            }
            IRStatement::If {
                condition,
                then_body,
                else_body,
            } => {
                // Emit the condition and reduce it to Python's truth value:
                // a str is an (offset, length) pair and a collection is a
                // pointer, neither of which is a bool on its own.
                let cond_ty = emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
                crate::compiler::expression::emit_truthiness(func, ctx, &cond_ty);

                // If-else block with no result value
                func.instruction(&Instruction::If(BlockType::Empty));

                // The `if`/`else` frame wraps both branches, so a break/continue
                // nested here is one level deeper than the surrounding loop body.
                ctx.block_depth += 1;

                // branch
                compile_body(then_body, func, ctx, memory_layout);

                if let Some(else_body) = else_body {
                    func.instruction(&Instruction::Else);
                    // branch
                    compile_body(else_body, func, ctx, memory_layout);
                }

                ctx.block_depth -= 1;
                func.instruction(&Instruction::End);
            }

            IRStatement::Raise { exception } => {
                // `raise StopIteration` signals iterator exhaustion across the
                // call boundary: set the module-wide stop flag (global 1) and
                // return this function's default value. The caller's drive
                // loop reads the flag through the `__waspy_stop_check`
                // intrinsic and breaks.
                let raised_name = match exception {
                    Some(IRExpr::FunctionCall { function_name, .. }) => {
                        Some(function_name.as_str())
                    }
                    Some(IRExpr::Variable(name)) | Some(IRExpr::Param(name)) => Some(name.as_str()),
                    _ => None,
                };
                if raised_name == Some("StopIteration") {
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::GlobalSet(1));
                    if matches!(ctx.current_return_type, IRType::Float) {
                        func.instruction(&Instruction::F64Const(0.0_f64.into()));
                    } else {
                        func.instruction(&Instruction::I32Const(0));
                    }
                    func.instruction(&Instruction::Return);
                    continue;
                }

                // Everything else records the exception's type in the module
                // global and transfers control: to the enclosing `try`'s
                // handler dispatch, or out of the function when there is none.
                if let Some(exc_expr) = exception {
                    // Resolve the exception to its type code by name — the same
                    // table the handler dispatch uses — instead of emitting the
                    // expression. We do not model exception objects, and emitting
                    // a constructor like `ValueError("msg")` would leave its string
                    // argument on the stack (invalid WASM). Both a bare name
                    // (`raise ValueError`) and a constructor call
                    // (`raise ValueError("msg")`) resolve by name.
                    let code = match exc_expr {
                        IRExpr::FunctionCall { function_name, .. } => {
                            exception_type_code(function_name)
                        }
                        IRExpr::Variable(name) | IRExpr::Param(name) => exception_type_code(name),
                        _ => 0,
                    };
                    // A type code of 0 would read as "nothing pending", so an
                    // unrecognized name takes the generic code like a bare
                    // `raise` does.
                    let code = if code == 0 { GENERIC_EXCEPTION } else { code };
                    func.instruction(&Instruction::I32Const(code));
                    func.instruction(&Instruction::GlobalSet(EXC_TYPE_GLOBAL));
                } else {
                    // Bare `raise`: generic exception code.
                    func.instruction(&Instruction::I32Const(GENERIC_EXCEPTION));
                    func.instruction(&Instruction::GlobalSet(EXC_TYPE_GLOBAL));
                }

                emit_exception_transfer(func, ctx, 0);
            }

            IRStatement::While { condition, body } => {
                // Outer block: `break` branches here to exit the loop.
                func.instruction(&Instruction::Block(BlockType::Empty));
                ctx.block_depth += 1;
                let break_level = ctx.block_depth;

                func.instruction(&Instruction::Loop(BlockType::Empty));
                ctx.block_depth += 1;

                // Condition check: exit the loop when the condition is false.
                // Emitted before the inner continue block so this `BrIf(1)` still
                // targets the outer break block from inside the loop.
                let cond_ty = emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
                crate::compiler::expression::emit_truthiness(func, ctx, &cond_ty);
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::BrIf(1));

                // Inner block: `continue` branches to its end, which falls through
                // to the back-edge below and re-evaluates the loop condition.
                func.instruction(&Instruction::Block(BlockType::Empty));
                ctx.block_depth += 1;
                let continue_level = ctx.block_depth;

                // Loop body
                ctx.loop_stack.push(LoopContext {
                    break_level,
                    continue_level,
                });
                compile_body(body, func, ctx, memory_layout);
                ctx.loop_stack.pop();

                ctx.block_depth -= 1;
                func.instruction(&Instruction::End); // end continue block

                // Jump back to the start of the loop
                func.instruction(&Instruction::Br(0));

                // End of loop and outer break block
                ctx.block_depth -= 1;
                func.instruction(&Instruction::End);
                ctx.block_depth -= 1;
                func.instruction(&Instruction::End);
            }
            IRStatement::Expression(expr) => {
                // Discard the result only when the expression actually leaves a
                // value. Calls like print() return None and push nothing, so an
                // unconditional drop would underflow the stack. String/bytes
                // values (e.g. a docstring statement) are an (offset, length)
                // pair and need two drops.
                let result_type = emit_expr(expr, func, ctx, memory_layout, None);
                match result_type {
                    IRType::None => {}
                    IRType::String | IRType::Bytes => {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                    }
                    _ => {
                        func.instruction(&Instruction::Drop);
                    }
                }
            }
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
                ..
            } => {
                // Emit the object reference (the store address) first; a WASM
                // store pops the value, then the address.
                let obj_type = emit_expr(object, func, ctx, memory_layout, None);

                // `obj.attr = v` where `attr` is a `@property` compiles to its
                // setter: the instance pointer is already on the stack, so the
                // coerced value completes the (self, value) argument pair.
                if let IRType::Class(class_name) = &obj_type {
                    if let Some((setter_idx, owner)) = ctx
                        .get_class_info(class_name)
                        .and_then(|ci| ci.property_setters.get(attribute.as_str()).cloned())
                    {
                        let param_types = ctx
                            .get_function_info(&format!("{owner}::{attribute}::setter"))
                            .map(|f| f.param_types.clone())
                            .unwrap_or_default();
                        let t = emit_expr(value, func, ctx, memory_layout, param_types.get(1));
                        if matches!(t, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                        crate::compiler::expression::emit_user_call(func, ctx, setter_idx);
                        // The setter's WASM result (implicit 0) is unused.
                        func.instruction(&Instruction::Drop);
                        continue;
                    }
                }

                let field = match &obj_type {
                    IRType::Class(class_name) => lookup_field(ctx, class_name, attribute),
                    _ => None,
                };

                if let Some((field_offset, field_ty)) = field {
                    // Stack: object_ptr. Emit the value coerced to the field's
                    // type, then store with the matching width (f64 for float
                    // fields, i32 otherwise). A string/bytes value is an
                    // (offset, length) pair but the field slot holds one word:
                    // drop the length and store the offset (reads rebuild the
                    // pair from the blob prefix).
                    let value_ty = emit_expr(value, func, ctx, memory_layout, Some(&field_ty));
                    if matches!(value_ty, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                    func.instruction(&store_field_instr(&field_ty, field_offset));
                } else {
                    // A field the class does not declare in any method, or an
                    // object whose type is not known. The write used to be
                    // dropped here with nothing said; reading it back is already
                    // refused, and a write that goes nowhere is the same defect.
                    let what = match &obj_type {
                        IRType::Class(class_name) => format!(
                            "'{class_name}' has no attribute '{attribute}' to assign. Hint: \
                             set it in __init__ so the class knows the field"
                        ),
                        other => format!(
                            "cannot assign attribute '{attribute}' of a value of type '{}'. \
                             Hint: annotate the parameter or variable it is set through",
                            crate::type_to_string(other)
                        ),
                    };
                    ctx.report(what);
                    let t = emit_expr(value, func, ctx, memory_layout, None);
                    if matches!(t, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop);
                    }
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::Drop);
                }
            }

            IRStatement::AugAssign { target, value, op } => {
                // `x OP= v` is `x = x OP v` for every immutable value, so it is
                // lowered to exactly that and shares the binary operator's
                // codegen. It used to carry a second, divergent copy of the
                // arithmetic: `line += "#"` added a length to an offset and left
                // `len(line)` at 0, `x /= 2` on an int divided as integers
                // (3.0 for 7 / 2), `x %= 2.0` subtracted the wrong way round,
                // and `%=` and `//=` truncated where Python floors.
                let local_ty = ctx
                    .get_local_info(target)
                    .map(|info| info.var_type.clone())
                    .unwrap_or(IRType::Unknown);
                match &local_ty {
                    // A list is the one mutable target: `xs += ys` extends it in
                    // place, so another name for the same list sees the growth.
                    // Rebinding to `xs + ys` would leave that other name behind.
                    IRType::List(_) if matches!(op, IROp::Add) => {
                        let extend = IRStatement::Expression(IRExpr::MethodCall {
                            object: Box::new(IRExpr::Variable(target.clone())),
                            method_name: "extend".to_string(),
                            arguments: vec![value.clone()],
                        });
                        compile_body(
                            &IRBody {
                                statements: vec![extend],
                            },
                            func,
                            ctx,
                            memory_layout,
                        );
                    }
                    IRType::List(_) | IRType::Dict(_, _) | IRType::Set(_) | IRType::Tuple(_) => {
                        ctx.report(format!(
                            "augmented assignment '{target} {}= ...' on a {} is not supported",
                            crate::compiler::expression::operator_symbol(op),
                            crate::type_to_string(&local_ty)
                        ));
                    }
                    // `x /= y` on an int makes `x` a float in Python, and a
                    // local's width is fixed for the whole function: storing
                    // 3.5 back into an i32 slot would answer 3.
                    IRType::Int | IRType::Bool if matches!(op, IROp::Div) => {
                        ctx.report(format!(
                            "'{target} /= ...' makes '{target}' a float, but it holds an int \
                             here. Hint: start it as a float (for example '{target} = 7.0'), or \
                             use '//=' for floor division"
                        ));
                    }
                    _ => {
                        let rebind = IRStatement::Assign {
                            target: target.clone(),
                            value: IRExpr::BinaryOp {
                                left: Box::new(IRExpr::Variable(target.clone())),
                                right: Box::new(value.clone()),
                                op: op.clone(),
                            },
                            var_type: None,
                        };
                        compile_body(
                            &IRBody {
                                statements: vec![rebind],
                            },
                            func,
                            ctx,
                            memory_layout,
                        );
                    }
                }
            }

            IRStatement::AttributeAugAssign {
                object,
                attribute,
                value,
                op,
            } => {
                // `obj.attr OP= v` is `obj.attr = obj.attr OP v`, and is lowered
                // to exactly that, so it shares the binary operator's codegen and
                // the ordinary attribute read and write (a `@property` goes
                // through its getter and setter, a missing field is refused).
                // The arm this replaces had its own arithmetic, with every defect
                // of the local-variable version, held the object pointer in a
                // scratch local while the value was emitted (so a value that did
                // any work overwrote it), and silently did nothing for a field
                // the class does not have.
                //
                // The rewrite evaluates `obj` twice, which is only Python's
                // meaning when evaluating it has no effect: a name, or a chain of
                // attributes off one.
                fn is_plain_path(expr: &IRExpr) -> bool {
                    match expr {
                        IRExpr::Variable(_) | IRExpr::Param(_) => true,
                        IRExpr::Attribute { object, .. } => is_plain_path(object),
                        _ => false,
                    }
                }
                if !is_plain_path(object) {
                    ctx.report(format!(
                        "augmented assignment to '.{attribute}' of a computed object is not \
                         supported. Hint: assign the object to a variable first"
                    ));
                    continue;
                }
                let field_ty = static_path_type(object, ctx)
                    .and_then(|ty| match ty {
                        IRType::Class(class_name) => {
                            lookup_field(ctx, &class_name, attribute).map(|(_, t)| t)
                        }
                        _ => None,
                    })
                    .unwrap_or(IRType::Unknown);
                let target = IRExpr::Attribute {
                    object: Box::new(object.clone()),
                    attribute: attribute.clone(),
                };
                let lowered = match (&field_ty, op) {
                    // A list field is extended in place, as `xs += ys` is.
                    (IRType::List(_), IROp::Add) => IRStatement::Expression(IRExpr::MethodCall {
                        object: Box::new(target),
                        method_name: "extend".to_string(),
                        arguments: vec![value.clone()],
                    }),
                    (
                        IRType::List(_) | IRType::Dict(_, _) | IRType::Set(_) | IRType::Tuple(_),
                        _,
                    ) => {
                        ctx.report(format!(
                            "augmented assignment '.{attribute} {}= ...' on a {} is not supported",
                            crate::compiler::expression::operator_symbol(op),
                            crate::type_to_string(&field_ty)
                        ));
                        continue;
                    }
                    (IRType::Int | IRType::Bool, IROp::Div) => {
                        ctx.report(format!(
                            "'.{attribute} /= ...' makes the field a float, but it holds an int. \
                             Hint: start it as a float, or use '//=' for floor division"
                        ));
                        continue;
                    }
                    _ => IRStatement::AttributeAssign {
                        object: object.clone(),
                        attribute: attribute.clone(),
                        value: IRExpr::BinaryOp {
                            left: Box::new(target),
                            right: Box::new(value.clone()),
                            op: op.clone(),
                        },
                        annotation: None,
                    },
                };
                compile_body(
                    &IRBody {
                        statements: vec![lowered],
                    },
                    func,
                    ctx,
                    memory_layout,
                );
            }

            IRStatement::For {
                target,
                iterable,
                body,
                else_body: _,
            } => {
                // Proper for loop implementation that iterates over lists
                // Allocate locals for loop variables:
                // - iterator_ptr: pointer to the list/iterable
                // - loop_counter: current index in the list
                // - list_length: length of the list

                // Reuse the iterator helper locals reserved for this loop during
                // the scan, replaying the same sequence numbering.
                let seq = ctx.for_loop_seq;
                ctx.for_loop_seq += 1;
                let iterator_ptr_idx = ctx
                    .get_local_index(&format!("__iter_ptr_{seq}"))
                    .expect("iterator ptr local not reserved");
                let loop_counter_idx = ctx
                    .get_local_index(&format!("__iter_idx_{seq}"))
                    .expect("iterator idx local not reserved");
                let list_length_idx = ctx
                    .get_local_index(&format!("__iter_len_{seq}"))
                    .expect("iterator len local not reserved");
                let target_idx = ctx
                    .get_local_index(target)
                    .expect("Target variable not found");

                // Evaluate the iterable (should return a pointer to list or value)
                let iterable_type = emit_expr(iterable, func, ctx, memory_layout, None);

                // Give the loop variable the element's type. The scan pass runs
                // before any list variable has a known element type, so it left
                // the target `Unknown` and a string element could not have a
                // method called on it. Codegen does know, so record it here,
                // before the body is compiled.
                // Iterating a dict binds its *keys*, so the loop variable
                // takes the key type, exactly as a list's binds the element.
                let bound_elem = match &iterable_type {
                    IRType::List(elem) => Some((**elem).clone()),
                    IRType::Dict(key, _) => Some((**key).clone()),
                    _ => None,
                };
                // Every element type that lives in the loop variable's i32 slot
                // is adopted, not only strings: a list of lists bound each row
                // untyped, so `sum(row)` over `for row in board` answered the
                // row's pointer. A float element is an f64 slot, which the scan
                // already allocated as such, so it is left alone here.
                if let Some(elem) = bound_elem {
                    if matches!(
                        elem,
                        IRType::String
                            | IRType::Bytes
                            | IRType::List(_)
                            | IRType::Dict(_, _)
                            | IRType::Set(_)
                            | IRType::Tuple(_)
                            | IRType::Class(_)
                            | IRType::Int
                            | IRType::Bool
                    ) {
                        if let Some(info) = ctx.locals_map.get_mut(target) {
                            // A string element always wins, as it did before;
                            // the other kinds only fill in a variable the scan
                            // left untyped.
                            let is_str = matches!(elem, IRType::String | IRType::Bytes);
                            if is_str || matches!(info.var_type, IRType::Unknown) {
                                info.var_type = elem;
                            }
                        }
                    }
                }

                match iterable_type {
                    IRType::String => {
                        // `for ch in s` binds each character as its own
                        // one-character string. This used to share the
                        // collection arm below, which reads a length from
                        // `ptr + 0` and a data pointer from the collection
                        // header; a string has neither (it is an `(offset,
                        // length)` pair), so the loop walked unrelated memory
                        // and trapped. The loop state lives in the per-loop
                        // locals, and each character is built before the body
                        // runs, so the scratch locals are free to use here.
                        func.instruction(&Instruction::LocalSet(list_length_idx));
                        func.instruction(&Instruction::LocalSet(iterator_ptr_idx));
                        if let Some(info) = ctx.locals_map.get_mut(target) {
                            info.var_type = IRType::String;
                        }
                        let len_companion = ctx.get_local_index(&strlen_local_name(target));

                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let break_level = ctx.block_depth;
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        ctx.block_depth += 1;

                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1));

                        crate::compiler::expression::emit_char_at(
                            func,
                            ctx,
                            iterator_ptr_idx,
                            loop_counter_idx,
                            ctx.temp_local,
                            ctx.temp_local + 1,
                            ctx.temp_local + 2,
                        );
                        match len_companion {
                            Some(len_idx) => func.instruction(&Instruction::LocalSet(len_idx)),
                            None => func.instruction(&Instruction::Drop),
                        };
                        func.instruction(&Instruction::LocalSet(target_idx));

                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let continue_level = ctx.block_depth;
                        ctx.loop_stack.push(LoopContext {
                            break_level,
                            continue_level,
                        });
                        compile_body(body, func, ctx, memory_layout);
                        ctx.loop_stack.pop();
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);

                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));
                        func.instruction(&Instruction::Br(0));

                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                    }
                    ref iter_ty @ (IRType::List(_) | IRType::Dict(_, _)) => {
                        // Lists store one COLLECTION_SLOT (8 bytes) per element;
                        // strings keep their legacy 4-byte-per-codepoint stride.
                        // A dict entry is a key slot followed by a value slot,
                        // and `for k in d` walks the keys, so it strides two
                        // slots at a time. Iterating a dict used to fall through
                        // to the branch below and read whatever the stride
                        // happened to land on, so `for k in d` over three keys
                        // counted 131072.
                        let elem_stride = match iter_ty {
                            IRType::Dict(_, _) => DICT_ENTRY as i32,
                            IRType::List(_) => COLLECTION_SLOT as i32,
                            _ => 4,
                        };
                        // A float list binds each element as f64 (the loop var was
                        // typed Float by the scan); everything else loads an i32.
                        let target_is_float =
                            matches!(get_local_type_by_index(ctx, target_idx), IRType::Float);
                        // Store the pointer to the list/string
                        func.instruction(&Instruction::LocalSet(iterator_ptr_idx));

                        // Get list length: load from memory at ptr+0
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(list_length_idx));

                        // Initialize loop counter to 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        // Loop structure. Outer block is the `break` target.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let break_level = ctx.block_depth;
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        ctx.block_depth += 1;

                        // Check if counter >= length
                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1)); // Break if true

                        // Load element from list[counter]. The elements live in
                        // the block the region's data pointer names, so the
                        // address is that block plus counter*stride; growing the
                        // list mid-iteration moves the block, and reading it
                        // through the header each time is what keeps the loop
                        // looking at the live elements.
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::I32Const(elem_stride));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        let elem_arg = MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        };
                        if target_is_float {
                            func.instruction(&Instruction::F64Load(elem_arg));
                        } else {
                            func.instruction(&Instruction::I32Load(elem_arg));
                        }

                        // Store element in target variable
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // A string element is stored as its offset alone, so the
                        // loop variable's length comes from the blob's own prefix
                        // word. Without it `len(w)` read whatever the companion
                        // happened to hold.
                        if matches!(
                            get_local_type_by_index(ctx, target_idx),
                            IRType::String | IRType::Bytes
                        ) {
                            if let Some(len_idx) = ctx.get_local_index(&strlen_local_name(target)) {
                                func.instruction(&Instruction::LocalGet(target_idx));
                                func.instruction(&Instruction::I32Const(
                                    crate::ir::STRING_LEN_PREFIX as i32,
                                ));
                                func.instruction(&Instruction::I32Sub);
                                func.instruction(&Instruction::I32Load(MemArg {
                                    offset: 0,
                                    align: 2,
                                    memory_index: 0,
                                }));
                                func.instruction(&Instruction::LocalSet(len_idx));
                            }
                        }

                        // Inner block: `continue` lands at its end, which falls
                        // through to the counter increment below.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let continue_level = ctx.block_depth;

                        // Execute the loop body
                        ctx.loop_stack.push(LoopContext {
                            break_level,
                            continue_level,
                        });
                        compile_body(body, func, ctx, memory_layout);
                        ctx.loop_stack.pop();
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End); // end continue block

                        // Increment counter
                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        // Loop back
                        func.instruction(&Instruction::Br(0));

                        // End of loop and break block
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                    }
                    IRType::Range => {
                        // Range object layout: [start:i32][stop:i32][step:i32][current:i32]
                        func.instruction(&Instruction::LocalSet(iterator_ptr_idx));

                        // Load start value into target
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Initialize loop counter to 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        // Loop structure. Outer block is the `break` target.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let break_level = ctx.block_depth;
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        ctx.block_depth += 1;

                        // Load stop and step for comparison
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 4,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(list_length_idx));

                        // Break condition depends on the sign of step, which may
                        // be dynamic, so branch on it at runtime:
                        //   step > 0  -> stop iterating once current >= stop
                        //   step <= 0 -> stop iterating once current <= stop
                        // (A single ascending `current >= stop` test would make a
                        // descending range, e.g. range(10, 0, -1), exit immediately.)
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 8,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32GtS); // step > 0
                        func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32GeS); // current >= stop
                        func.instruction(&Instruction::Else);
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32LeS); // current <= stop
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::BrIf(1)); // Break if true

                        // Inner block: `continue` lands at its end, which falls
                        // through to the step increment below.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let continue_level = ctx.block_depth;

                        // Execute the loop body
                        ctx.loop_stack.push(LoopContext {
                            break_level,
                            continue_level,
                        });
                        compile_body(body, func, ctx, memory_layout);
                        ctx.loop_stack.pop();
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End); // end continue block

                        // Increment by step
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 8,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Loop back
                        func.instruction(&Instruction::Br(0));

                        // End of loop and break block
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                    }
                    _ => {
                        // For non-list iterables, fall back to simple counting
                        // Treat the value as a count (integer)
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Simple loop: counter from 1 to value. Outer block is
                        // the `break` target.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let break_level = ctx.block_depth;
                        func.instruction(&Instruction::Loop(BlockType::Empty));
                        ctx.block_depth += 1;

                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32LeS);
                        func.instruction(&Instruction::BrIf(1));

                        // Inner block: `continue` lands at its end, which falls
                        // through to the decrement below.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        ctx.block_depth += 1;
                        let continue_level = ctx.block_depth;

                        // Execute body
                        ctx.loop_stack.push(LoopContext {
                            break_level,
                            continue_level,
                        });
                        compile_body(body, func, ctx, memory_layout);
                        ctx.loop_stack.pop();
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End); // end continue block

                        // Decrement
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::LocalSet(target_idx));

                        func.instruction(&Instruction::Br(0));
                        // End of loop and break block
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                        ctx.block_depth -= 1;
                        func.instruction(&Instruction::End);
                    }
                }
            }

            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                // The try body runs inside a block whose end is the handler
                // dispatch below. A `raise` in the body (or a call that comes
                // back with an exception pending) branches there, so the rest
                // of the body is skipped, exactly as Python skips it. Falling
                // off the end of the body reaches the same dispatch with no
                // exception pending, which is the ordinary path.
                func.instruction(&Instruction::Block(BlockType::Empty));
                ctx.block_depth += 1;
                ctx.try_stack.push(ctx.block_depth);

                compile_body(try_body, func, ctx, memory_layout);

                // Leaving the body ends the region this `try` protects: an
                // exception raised in a handler belongs to the enclosing try,
                // not to this one.
                ctx.try_stack.pop();
                ctx.block_depth -= 1;
                func.instruction(&Instruction::End);

                // Dispatch. Nothing pending means the body completed.
                func.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
                func.instruction(&Instruction::I32Eqz);

                // If no exception (flag == 0), skip all except handlers and go to finally
                func.instruction(&Instruction::If(BlockType::Empty));
                // The dispatch if/else frame wraps every handler body, so a
                // break/continue inside a handler is one level deeper.
                ctx.block_depth += 1;

                // If an exception occurred, check handlers
                func.instruction(&Instruction::Else);

                // Try to match exception handlers
                for handler in except_handlers.iter() {
                    // A bare `except:`, and `except Exception:` (or
                    // `BaseException`), catch anything pending. Python's real
                    // rule is subclass matching, and every exception is a
                    // subclass of those two.
                    let catch_all = handler.exception_types.is_empty()
                        || handler
                            .exception_types
                            .iter()
                            .any(|name| matches!(name.as_str(), "Exception" | "BaseException"));

                    if catch_all {
                        if let Some(var_name) = &handler.name {
                            let handler_var_idx = ctx
                                .get_local_index(var_name)
                                .unwrap_or_else(|| ctx.add_local(var_name, IRType::Unknown));
                            // Bind the exception's type code; there are no
                            // exception objects to bind.
                            func.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
                            func.instruction(&Instruction::LocalSet(handler_var_idx));
                        }

                        // Caught: nothing is pending any more, so clear it
                        // before the handler runs (the body may raise again).
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::GlobalSet(EXC_TYPE_GLOBAL));

                        compile_body(&handler.body, func, ctx, memory_layout);
                        // Nothing after a catch-all can run.
                        break;
                    }

                    func.instruction(&Instruction::Block(BlockType::Empty));
                    // This per-handler block wraps the handler body.
                    ctx.block_depth += 1;

                    // Does the pending type match any of the names this
                    // handler lists? `except (A, B):` matches either.
                    for (i, name) in handler.exception_types.iter().enumerate() {
                        func.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
                        func.instruction(&Instruction::I32Const(exception_type_code(name)));
                        func.instruction(&Instruction::I32Eq);
                        if i > 0 {
                            func.instruction(&Instruction::I32Or);
                        }
                    }
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::BrIf(0)); // no match: next handler

                    if let Some(var_name) = &handler.name {
                        let handler_var_idx = ctx
                            .get_local_index(var_name)
                            .unwrap_or_else(|| ctx.add_local(var_name, IRType::Unknown));
                        func.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
                        func.instruction(&Instruction::LocalSet(handler_var_idx));
                    }

                    // Caught: clear before running the body, which may raise an
                    // exception of its own.
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::GlobalSet(EXC_TYPE_GLOBAL));

                    compile_body(&handler.body, func, ctx, memory_layout);

                    ctx.block_depth -= 1;
                    func.instruction(&Instruction::End);
                }

                // Close the exception-dispatch if/else. Each typed handler opens
                // and closes its own block, so only this `If` remains open here;
                // a second `End` would close the function frame early.
                ctx.block_depth -= 1;
                func.instruction(&Instruction::End);

                // `finally` runs on every path that reaches here: the body
                // completed, a handler caught, or no handler matched and the
                // exception is still pending. (The `return`/`break`/`continue`
                // paths jump past this point, so `ir::context_managers` puts a
                // copy of the body ahead of each of them.)
                if let Some(finally_body) = finally_body {
                    compile_body(finally_body, func, ctx, memory_layout);
                }

                // Still pending means no handler matched, so this `try` does
                // not stop the exception: keep unwinding.
                func.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_exception_transfer(func, ctx, 1);
                func.instruction(&Instruction::End);
            }

            IRStatement::With { .. } => {
                // `with` is rewritten into explicit `__enter__`/`__exit__`
                // calls by `ir::context_managers` before codegen runs, so a
                // surviving one means that pass missed a body.
                unreachable!(
                    "`with` statement reached codegen; it should have been desugared by \
                     ir::context_managers::desugar_with_statements"
                );
            }

            IRStatement::DynamicImport {
                target,
                module_name,
            } => {
                // Emit code to evaluate the module name expression
                emit_expr(module_name, func, ctx, memory_layout, None);

                // Get the target local index or create one if it doesn't exist
                let local_idx = ctx
                    .get_local_index(target)
                    .unwrap_or_else(|| ctx.add_local(target, IRType::Unknown));

                // Store the result (currently just a placeholder) in the target variable
                func.instruction(&Instruction::LocalSet(local_idx));
            }

            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                // Get container type to determine storage strategy
                let container_type = emit_expr(container, func, ctx, memory_layout, None);

                // The container and the key are held across the emission of the
                // key and the value, which is arbitrary codegen over the scratch
                // run, so they live in locals reserved for this statement and are
                // moved into `temp_local` / `temp_local + 1` only once the value
                // is stashed. They used to sit in those scratch locals the whole
                // time, so `xs[1] = ys[2]` or `d[s[i]] = 1` had the container
                // pointer overwritten by the nested index and wrote through
                // whatever was left in it.
                func.instruction(&Instruction::LocalSet(ctx.assign_container_local));

                // A float-keyed dict keeps its key as an f64 so `d[1.5] = x`
                // matches the f64-stored key; list/tuple indices and other dict
                // keys stay i32. Hint the index accordingly.
                let key_is_float = matches!(
                    &container_type,
                    IRType::Dict(k, _) if matches!(k.as_ref(), IRType::Float)
                );
                let index_hint = if key_is_float {
                    IRType::Float
                } else {
                    IRType::Int
                };
                let key_type = emit_expr(index, func, ctx, memory_layout, Some(&index_hint));
                // The dict's declared key type is authoritative, but an empty
                // dict literal has none, so the key expression's own type
                // decides when the container has not been typed yet.
                let key_is_string = matches!(
                    &container_type,
                    IRType::Dict(k, _) if matches!(k.as_ref(), IRType::String | IRType::Bytes)
                ) || matches!(key_type, IRType::String | IRType::Bytes);

                // A string key is an (offset, length) pair but a dict slot holds
                // one word, so the length is dropped here. Without this the pair
                // left an extra value on the stack and the module did not
                // validate; it only showed up once loop variables over a list of
                // strings started being typed as strings.
                crate::compiler::expression::narrow_element_to_word(func, &key_type);

                // Save index / key at its natural width. A float key is an f64 in
                // the second f64 scratch, leaving `temp_local_f64` free for a
                // float value below.
                if key_is_float {
                    func.instruction(&Instruction::LocalSet(ctx.assign_key_f64_local));
                } else {
                    func.instruction(&Instruction::LocalSet(ctx.assign_key_local));
                }

                // Emit value expression, then stash it in a type-appropriate
                // scratch local: a float value is an f64 and must live in the
                // dedicated f64 scratch, not an i32 local. String/bytes values
                // collapse to their offset word (the length on top is dropped).
                // The value takes the container's element width, not its own,
                // or `xs[0] = 3` on a `List[float]` writes an i32 over the low
                // half of the slot and the element reads back wrong (#123).
                let element_type =
                    crate::compiler::expression::collection_element_type(&container_type);
                let what = match &container_type {
                    IRType::Dict(_, _) => "a dict value assignment",
                    _ => "an item assignment",
                };
                let value_type = crate::compiler::expression::emit_collection_element(
                    value,
                    func,
                    ctx,
                    memory_layout,
                    element_type.as_ref(),
                    what,
                );
                let value_is_float = matches!(value_type, IRType::Float);
                if value_is_float {
                    func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
                } else {
                    if matches!(value_type, IRType::String | IRType::Bytes) {
                        func.instruction(&Instruction::Drop); // length
                    }
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 2));
                }

                // Everything below is straight-line, so the container and key can
                // move back into the scratch slots the store and search use.
                func.instruction(&Instruction::LocalGet(ctx.assign_container_local));
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                if key_is_float {
                    func.instruction(&Instruction::LocalGet(ctx.assign_key_f64_local));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local_f64_2));
                } else {
                    func.instruction(&Instruction::LocalGet(ctx.assign_key_local));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
                }

                // Push the stashed value (matching its width) onto the stack; the
                // caller has already pushed the destination address.
                let push_value = |func: &mut Function| {
                    if value_is_float {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64));
                        func.instruction(&Instruction::F64Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                    } else {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                    }
                };

                // Load the dict key at the address on top of the stack and push 1
                // if it equals the stashed search key, else 0 — at the key's
                // natural width (f64 for a float key, i32 word otherwise).
                // The key type the dict compares at: its declared one, or for a
                // dict not typed yet, the key expression's own.
                let dict_key_type = match &container_type {
                    IRType::Dict(k, _) if !matches!(k.as_ref(), IRType::Unknown) => {
                        k.as_ref().clone()
                    }
                    _ => key_type.clone(),
                };
                if matches!(container_type, IRType::Dict(_, _)) {
                    if let Some(why) =
                        crate::compiler::equality::hash_unsupported(ctx, &dict_key_type)
                    {
                        ctx.report(format!("{why} cannot be a dict key"));
                    }
                }
                let cmp_key = |func: &mut Function| {
                    if key_is_float {
                        func.instruction(&Instruction::F64Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
                        func.instruction(&Instruction::F64Eq);
                    } else if key_is_string {
                        // Strings compare by content: an equal key built at
                        // runtime sits at a different offset, so comparing the
                        // stored words appended a second entry for a key the
                        // dict already held.
                        let slot_off = ctx.temp_local + 14;
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(slot_off));
                        crate::compiler::expression::emit_str_content_eq(
                            func,
                            ctx,
                            slot_off,
                            ctx.temp_local + 1,
                        );
                    } else {
                        // The shared comparison, so a tuple key matches an equal
                        // one instead of adding a second entry for it.
                        crate::compiler::expression::emit_slot_eq_needle(
                            func,
                            ctx,
                            &dict_key_type,
                            ctx.temp_local + 1,
                        );
                    }
                };

                // Store the stashed search key into the slot at the address on top
                // of the stack (used when appending a new entry), width-aware.
                let store_key = |func: &mut Function| {
                    if key_is_float {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local_f64_2));
                        func.instruction(&Instruction::F64Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                    } else {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                    }
                };

                match container_type {
                    IRType::List(_) => {
                        // `xs[-1] = v` means the last element, and an index past
                        // the end is an error rather than a write into whatever
                        // follows the region: normalize and check before
                        // computing the address.
                        crate::compiler::expression::emit_stored_index_check(
                            func,
                            ctx,
                            ctx.temp_local,
                            ctx.temp_local + 1,
                        );

                        // Address: data + (index * SLOT)
                        func.instruction(&Instruction::LocalGet(ctx.temp_local)); // container_ptr
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // index
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Mul); // index * SLOT
                        func.instruction(&Instruction::I32Add); // data + index*SLOT

                        // Store the value at its natural width.
                        push_value(func);
                    }
                    IRType::Dict(_key_type, _value_type) => {
                        // Dictionary assignment via linear search.
                        // Layout: [num_entries:i32][key0][val0][key1][val1]...
                        //   temp_local     = dict_ptr
                        //   temp_local + 1 = key
                        //   temp_local + 2 = value
                        //   temp_local + 3 = num_entries
                        //   temp_local + 4 = counter
                        //   temp_local + 5 = found flag (0/1)

                        // num_entries = load(dict_ptr)
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                        // counter = 0; found = 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 5));

                        // Search for an existing entry with a matching key.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));

                        // if counter >= num_entries: break
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1));

                        // key address = data + counter*DICT_ENTRY
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);

                        // if key_at == key: update value and break
                        cmp_key(func);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        // value address = data + counter*DICT_ENTRY + SLOT
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Add);
                        push_value(func); // value (width-aware)
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 5)); // found = 1
                        func.instruction(&Instruction::Br(2)); // exit the loop
                        func.instruction(&Instruction::End);

                        // counter += 1; continue
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                        func.instruction(&Instruction::Br(0));
                        func.instruction(&Instruction::End); // loop
                        func.instruction(&Instruction::End); // block

                        // If the key was not present, append a new entry at slot
                        // num_entries and bump the entry count.
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                        func.instruction(&Instruction::I32Eqz);
                        func.instruction(&Instruction::If(BlockType::Empty));

                        // A new key needs an entry the region may not have room
                        // for; reserve it first, which moves the dict's entry
                        // block (never the dict itself) when it is full. The
                        // helper reloads the entry count into temp_local + 3, so
                        // the stores below still address the right slot, and the
                        // search locals (counter, found) are dead by now.
                        crate::compiler::expression::emit_collection_reserve(
                            func,
                            ctx,
                            crate::compiler::expression::Reserve::Count(1),
                            DICT_ENTRY,
                        );

                        // store key at data + num_entries*DICT_ENTRY
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        store_key(func); // key (width-aware)

                        // store value at data + num_entries*DICT_ENTRY + SLOT
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: COLLECTION_DATA as u64,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Add);
                        push_value(func); // value (width-aware)
                                          // num_entries += 1; store back to dict_ptr
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::End);
                    }
                    // Everything else cannot be assigned into. Python raises
                    // TypeError for a tuple or a string ("does not support item
                    // assignment"), and a container whose type codegen cannot
                    // resolve has no layout to write through. Both used to be
                    // silent: the write was dropped on the floor for a string,
                    // and a tuple left the stack unbalanced, so the module
                    // failed validation while the compiler reported success.
                    other => {
                        // A string or bytes container left its length word on
                        // the stack; anything else left nothing.
                        if matches!(other, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                        let what = match other {
                            IRType::String => "'str' object".to_string(),
                            IRType::Bytes => "'bytes' object".to_string(),
                            IRType::Tuple(_) => "'tuple' object".to_string(),
                            other => format!("a value of type {}", crate::type_to_string(&other)),
                        };
                        ctx.report(format!(
                            "{what} does not support item assignment. \
                             Hint: build a new value instead, or use a list"
                        ));
                    }
                }
            }

            IRStatement::Yield { value } => {
                // Emit the yielded value expression
                if let Some(val) = value {
                    emit_expr(val, func, ctx, memory_layout, None);
                } else {
                    // yield without a value yields None
                    func.instruction(&Instruction::I32Const(0));
                }

                // For generator support, the yielded value would be stored
                // in a generator state and execution would be paused.
                // For now, this is a placeholder that just drops the value.
                func.instruction(&Instruction::Drop);
            }

            IRStatement::ImportModule { module_name, alias } => {
                // Create a variable to hold the imported module
                let var_name = alias.as_ref().unwrap_or(module_name);
                let _local_idx = ctx.add_local(var_name, IRType::Module(module_name.clone()));

                // For now, store a dummy module reference
                // Full implementation would load and execute the module
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(_local_idx));
            }

            IRStatement::Break => {
                // Branch out of the innermost loop's outer block. The relative
                // depth accounts for any `if`/`try` frames between here and the
                // loop. A `break` outside any loop is invalid Python; the parser
                // rejects it, so emit nothing rather than a malformed branch.
                if let Some(loop_ctx) = ctx.loop_stack.last().copied() {
                    func.instruction(&Instruction::Br(ctx.block_depth - loop_ctx.break_level));
                }
            }

            IRStatement::Continue => {
                // Branch to the end of the innermost loop's continue block, which
                // falls through to the iterator step / back-edge so the next
                // iteration runs.
                if let Some(loop_ctx) = ctx.loop_stack.last().copied() {
                    func.instruction(&Instruction::Br(ctx.block_depth - loop_ctx.continue_level));
                }
            }
        }
    }
}
