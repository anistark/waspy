//! Exceptions as objects.
//!
//! Every exception is an instance of a class: the builtin ones
//! (`BaseException` and its hierarchy, as CPython defines it) are added to a
//! module that can raise, ahead of the program's own classes, so a user
//! exception class derives from them like any other base, `isinstance` and
//! `except` match subclasses through the class tags, and an exception's
//! fields and methods are reached the usual way.
//!
//! A raise records the object in `EXC_VALUE_GLOBAL` and its class id in
//! `EXC_TYPE_GLOBAL` (0 when nothing is in flight). `BaseException` keeps
//! what CPython's does with its constructor arguments, computed where they
//! are given, since their types are known there: `str(e)` (`__exc_str`), the
//! text inside `repr(e)`'s parentheses (`__exc_repr`), and `e.args` when
//! every argument is a str (`__exc_args`, with `__exc_all_str`).

use crate::compiler::context::{CompilationContext, EXC_TYPE_GLOBAL};
use crate::compiler::runtime::{function, w32, Helper};
use crate::ir::{IRBody, IRExpr, IRFunction, IRModule, IRParam, IRStatement, IRType, ParamStar};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

/// The pending exception's object, 0 when none is in flight.
pub const EXC_VALUE_GLOBAL: u32 = 4;

/// The builtin exception classes, each after its base, as CPython 3.13
/// arranges them.
pub(crate) const BUILTIN_EXCEPTIONS: &[(&str, &str)] = &[
    ("BaseException", ""),
    ("SystemExit", "BaseException"),
    ("KeyboardInterrupt", "BaseException"),
    ("GeneratorExit", "BaseException"),
    ("Exception", "BaseException"),
    ("ArithmeticError", "Exception"),
    ("FloatingPointError", "ArithmeticError"),
    ("OverflowError", "ArithmeticError"),
    ("ZeroDivisionError", "ArithmeticError"),
    ("AssertionError", "Exception"),
    ("AttributeError", "Exception"),
    ("BufferError", "Exception"),
    ("EOFError", "Exception"),
    ("ImportError", "Exception"),
    ("ModuleNotFoundError", "ImportError"),
    ("LookupError", "Exception"),
    ("IndexError", "LookupError"),
    ("KeyError", "LookupError"),
    ("MemoryError", "Exception"),
    ("NameError", "Exception"),
    ("UnboundLocalError", "NameError"),
    ("OSError", "Exception"),
    ("FileExistsError", "OSError"),
    ("FileNotFoundError", "OSError"),
    ("IsADirectoryError", "OSError"),
    ("NotADirectoryError", "OSError"),
    ("PermissionError", "OSError"),
    ("TimeoutError", "OSError"),
    ("ReferenceError", "Exception"),
    ("RuntimeError", "Exception"),
    ("NotImplementedError", "RuntimeError"),
    ("RecursionError", "RuntimeError"),
    ("StopIteration", "Exception"),
    ("StopAsyncIteration", "Exception"),
    ("SyntaxError", "Exception"),
    ("SystemError", "Exception"),
    ("TypeError", "Exception"),
    ("ValueError", "Exception"),
    ("UnicodeError", "ValueError"),
    ("UnicodeDecodeError", "UnicodeError"),
    ("UnicodeEncodeError", "UnicodeError"),
];

pub(crate) fn is_builtin_exception(name: &str) -> bool {
    BUILTIN_EXCEPTIONS.iter().any(|(n, _)| *n == name)
}

/// The fields `BaseException` adds to every exception's layout.
pub(crate) const EXC_FIELDS: &[&str] = &[
    "__exc_str",
    "__exc_repr",
    "__exc_args",
    "__exc_all_str",
    "__cause__",
];

pub(crate) fn exc_field_type(name: &str) -> IRType {
    match name {
        "__exc_str" | "__exc_repr" => IRType::String,
        "__exc_args" => IRType::VarTuple(Box::new(IRType::String)),
        "__exc_all_str" => IRType::Int,
        _ => IRType::Class("BaseException".to_string()),
    }
}

/// Whether the module needs exception objects: it raises, catches, derives
/// from an exception, or (`can_raise`) does something that can fault.
pub(crate) fn module_needs_exceptions(module: &IRModule, can_raise: bool) -> bool {
    can_raise
        || module
            .classes
            .iter()
            .any(|c| c.bases.iter().any(|b| is_builtin_exception(b)))
}

/// The module with the builtin exception classes ahead of its own.
pub(crate) fn with_builtin_exceptions(module: &IRModule) -> IRModule {
    let mut module = module.clone();
    for text in ["(", ")"] {
        module.memory_layout.add_string(text);
    }
    let self_param = || IRParam {
        name: "self".to_string(),
        param_type: IRType::Class("BaseException".to_string()),
        default_value: None,
        keyword_only: false,
        star: ParamStar::None,
    };
    let field = |name: &str| IRExpr::Attribute {
        object: Box::new(IRExpr::Variable("self".to_string())),
        attribute: name.to_string(),
    };
    let text = |s: &str| IRExpr::Const(crate::ir::IRConstant::String(s.to_string()));
    let add = |l: IRExpr, r: IRExpr| IRExpr::BinaryOp {
        left: Box::new(l),
        right: Box::new(r),
        op: crate::ir::IROp::Add,
    };
    let method = |name: &str, value: IRExpr| IRFunction {
        name: name.to_string(),
        params: vec![self_param()],
        body: IRBody {
            statements: vec![IRStatement::Return(Some(value))],
        },
        return_type: IRType::String,
        decorators: Vec::new(),
    };
    let mut classes: Vec<crate::ir::IRClass> = BUILTIN_EXCEPTIONS
        .iter()
        .map(|(name, base)| crate::ir::IRClass {
            name: name.to_string(),
            bases: if base.is_empty() {
                Vec::new()
            } else {
                vec![base.to_string()]
            },
            methods: if *name == "BaseException" {
                vec![
                    method("__str__", field("__exc_str")),
                    method(
                        "__repr__",
                        add(
                            add(
                                IRExpr::FunctionCall {
                                    function_name: CLASS_NAME_FN.to_string(),
                                    arguments: vec![IRExpr::Variable("self".to_string())],
                                },
                                text("("),
                            ),
                            add(field("__exc_repr"), text(")")),
                        ),
                    ),
                ]
            } else {
                Vec::new()
            },
            class_vars: Vec::new(),
            metaclass: None,
            position: 0,
            deferred_vars: Vec::new(),
        })
        .collect();
    classes.append(&mut module.classes);
    module.classes = classes;
    module
}

/// The intrinsic naming an instance's class: `__exc_class_name(obj)`.
pub(crate) const CLASS_NAME_FN: &str = "__exc_class_name";

pub(crate) fn is_exception_class(ctx: &CompilationContext, name: &str) -> bool {
    ctx.get_class_info("BaseException").is_some()
        && ctx.get_class_info(name).is_some()
        && ctx.is_class_or_subclass(name, "BaseException")
}

/// The class ids of `name` and everything below it.
pub(crate) fn descendant_ids(ctx: &CompilationContext, name: &str) -> Vec<i32> {
    let mut ids: Vec<i32> = ctx
        .class_map
        .values()
        .filter(|c| ctx.is_class_or_subclass(&c.name, name))
        .map(|c| c.class_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Push whether the pending exception is an instance of `name` or of one of
/// its subclasses.
pub(crate) fn emit_pending_is(func: &mut Function, ctx: &CompilationContext, name: &str) {
    let ids = descendant_ids(ctx, name);
    emit!(func, I::I32Const(0));
    for id in ids {
        emit!(
            func,
            I::GlobalGet(EXC_TYPE_GLOBAL),
            I::I32Const(id),
            I::I32Eq,
            I::I32Or
        );
    }
}

fn field_offset(ctx: &CompilationContext, name: &str) -> u64 {
    ctx.get_class_info("BaseException")
        .and_then(|c| c.field_offsets.get(name).copied())
        .unwrap_or_else(|| panic!("BaseException lays out {name}"))
}

/// Store the word on the stack into exception field `field` of the object
/// in `obj`.
fn store_field(
    func: &mut Function,
    ctx: &CompilationContext,
    obj: u32,
    field: &str,
    value: impl FnOnce(&mut Function),
) {
    emit!(func, I::LocalGet(obj));
    value(func);
    emit!(func, I::I32Store(w32(field_offset(ctx, field))));
}

/// Fill the argument fields of the exception in `obj` from the values in
/// `args` (held locals, with their types), as `BaseException(*args)` does:
/// `str(e)` is empty for no argument, `str()` of the one argument (its
/// `repr()` for a `KeyError`), or the tuple's repr; `repr(e)` shows the
/// arguments' reprs; `e.args` is kept when they are all strs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_fill_args(
    func: &mut Function,
    ctx: &CompilationContext,
    obj: u32,
    class: &str,
    args: &[(u32, IRType)],
    scratch: u32,
    tuple: u32,
) {
    let concat = ctx.helper(Helper::Concat);
    let text = |func: &mut Function, s: &str| {
        emit!(func, I::Call(ctx.helper(Helper::ExcText(s.to_string()))));
    };
    let get = |func: &mut Function, (slot, _): &(u32, IRType)| emit!(func, I::LocalGet(*slot));
    let repr_of = |func: &mut Function, arg: &(u32, IRType)| {
        get(func, arg);
        emit!(func, I::Call(ctx.helper(Helper::Repr(arg.1.clone()))));
    };
    let str_of = |func: &mut Function, arg: &(u32, IRType)| {
        get(func, arg);
        if !matches!(arg.1, IRType::String) {
            emit!(func, I::Call(ctx.helper(Helper::Str(arg.1.clone()))));
        }
    };
    // repr's inner text: the reprs, comma-separated.
    store_field(func, ctx, obj, "__exc_repr", |func| {
        if args.is_empty() {
            text(func, "");
        }
        for (k, arg) in args.iter().enumerate() {
            repr_of(func, arg);
            if k > 0 {
                emit!(func, I::LocalSet(scratch));
                text(func, ", ");
                emit!(func, I::Call(concat), I::LocalGet(scratch), I::Call(concat));
            }
        }
    });
    let key_like =
        ctx.get_class_info(class).is_some() && ctx.is_class_or_subclass(class, "KeyError");
    store_field(func, ctx, obj, "__exc_str", |func| match args {
        [] => text(func, ""),
        [one] if key_like => repr_of(func, one),
        [one] => str_of(func, one),
        _ => {
            text(func, "(");
            emit!(
                func,
                I::LocalGet(obj),
                I::I32Load(w32(field_offset(ctx, "__exc_repr"))),
                I::Call(concat)
            );
            text(func, ")");
            emit!(func, I::Call(concat));
        }
    });
    // SystemExit: no argument or None exits 0, an int with itself, anything
    // else is printed and exits 1.
    if ctx.get_class_info(class).is_some() && ctx.is_class_or_subclass(class, "SystemExit") {
        let field = |name: &str| {
            ctx.get_class_info("SystemExit")
                .and_then(|c| c.field_offsets.get(name).copied())
                .expect("SystemExit lays out its status")
        };
        let (code, print) = match args {
            [] | [(_, IRType::None)] => (None, 0),
            [(slot, IRType::Int | IRType::Bool)] => (Some(*slot), 0),
            _ => (None, 1),
        };
        emit!(func, I::LocalGet(obj));
        match code {
            Some(slot) => emit!(func, I::LocalGet(slot)),
            None => emit!(func, I::I32Const(print)),
        }
        emit!(
            func,
            I::I32Store(w32(field("__exit_code"))),
            I::LocalGet(obj),
            I::I32Const(print),
            I::I32Store(w32(field("__exit_print")))
        );
    }
    let all_str = args.iter().all(|(_, t)| matches!(t, IRType::String));
    store_field(func, ctx, obj, "__exc_all_str", |func| {
        emit!(func, I::I32Const(all_str as i32))
    });
    if all_str {
        // The tuple of the strs, laid out as a list is.
        use crate::compiler::context::{
            COLLECTION_CAP, COLLECTION_DATA, COLLECTION_HEADER, COLLECTION_SLOT,
        };
        emit!(
            func,
            I::I32Const(COLLECTION_HEADER as i32 + args.len() as i32 * COLLECTION_SLOT as i32),
            I::Call(ctx.alloc_func_index),
            I::LocalTee(tuple),
            I::I32Const(args.len() as i32),
            I::I32Store(w32(0)),
            I::LocalGet(tuple),
            I::I32Const(args.len() as i32),
            I::I32Store(w32(COLLECTION_CAP as u64)),
            I::LocalGet(tuple),
            I::LocalGet(tuple),
            I::I32Const(COLLECTION_HEADER as i32),
            I::I32Add,
            I::I32Store(w32(COLLECTION_DATA as u64))
        );
        for (k, (slot, _)) in args.iter().enumerate() {
            emit!(
                func,
                I::LocalGet(tuple),
                I::LocalGet(*slot),
                I::I32Store(w32(
                    COLLECTION_HEADER as u64 + k as u64 * COLLECTION_SLOT as u64
                ))
            );
        }
        store_field(func, ctx, obj, "__exc_args", |func| {
            emit!(func, I::LocalGet(tuple))
        });
    }
}

/// Raise the exception object in `obj`: record it and its class, then
/// transfer control.
pub(crate) fn emit_raise_object(
    func: &mut Function,
    ctx: &CompilationContext,
    obj: u32,
    extra_depth: u32,
) {
    emit!(
        func,
        I::LocalGet(obj),
        I::GlobalSet(EXC_VALUE_GLOBAL),
        I::LocalGet(obj),
        I::I32Load(w32(0)),
        I::GlobalSet(EXC_TYPE_GLOBAL)
    );
    crate::compiler::function::emit_exception_transfer(func, ctx, extra_depth);
}

/// What an exception the runtime raises carries.
pub(crate) enum ExcArg<'a> {
    /// A message: `ValueError("...")`.
    Text(&'a str),
    /// A value of the type the local holds, as `KeyError(key)` carries it.
    Value(u32, IRType),
    /// `ValueError(f"{value!r} is not in list")`.
    NotInList(u32, IRType),
    /// A message around the int a local holds: `prefix + str(n) + suffix`.
    Count(&'a str, u32, &'a str),
    /// `prefix + repr(value)`, the value in the local.
    Prefix(&'a str, u32, IRType),
}

/// Raise a builtin exception from the point of a fault, carrying what CPython
/// gives it. `extra_depth` counts the blocks the caller has opened since
/// `ctx.block_depth` was last updated.
pub(crate) fn emit_raise_builtin(
    func: &mut Function,
    ctx: &CompilationContext,
    class: &str,
    arg: ExcArg,
    extra_depth: u32,
) {
    // A module without the exception classes has no `try` and nothing the
    // analysis saw raise, so nothing could catch this: it escapes, which
    // traps, as an escaping exception does.
    if ctx.get_class_info(class).is_none() {
        emit!(func, I::Unreachable);
        return;
    }
    match arg {
        ExcArg::Text(message) => {
            emit!(
                func,
                I::Call(ctx.helper(Helper::ExcText(message.to_string()))),
                I::Call(ctx.helper(Helper::RaiseText(class.to_string())))
            );
        }
        ExcArg::Value(local, ty) => {
            emit!(
                func,
                I::LocalGet(local),
                I::Call(ctx.helper(Helper::RaiseValue {
                    class: class.to_string(),
                    ty,
                }))
            );
        }
        ExcArg::Count(prefix, local, suffix) => {
            let concat = ctx.helper(Helper::Concat);
            emit!(
                func,
                I::Call(ctx.helper(Helper::ExcText(prefix.to_string()))),
                I::LocalGet(local),
                I::Call(ctx.helper(Helper::Str(IRType::Int))),
                I::Call(concat),
                I::Call(ctx.helper(Helper::ExcText(suffix.to_string()))),
                I::Call(concat),
                I::Call(ctx.helper(Helper::RaiseText(class.to_string())))
            );
        }
        ExcArg::Prefix(prefix, local, ty) => {
            emit!(
                func,
                I::Call(ctx.helper(Helper::ExcText(prefix.to_string()))),
                I::LocalGet(local),
                I::Call(ctx.helper(Helper::Repr(ty))),
                I::Call(ctx.helper(Helper::Concat)),
                I::Call(ctx.helper(Helper::RaiseText(class.to_string())))
            );
        }
        ExcArg::NotInList(local, ty) => {
            emit!(
                func,
                I::LocalGet(local),
                I::Call(ctx.helper(Helper::Repr(ty))),
                I::Call(ctx.helper(Helper::ExcText(" is not in list".to_string()))),
                I::Call(ctx.helper(Helper::Concat)),
                I::Call(ctx.helper(Helper::RaiseText(class.to_string())))
            );
        }
    }
    crate::compiler::function::emit_exception_transfer(func, ctx, extra_depth);
}

/// `() -> str`: a constant text, built at run time (a helper has no place in
/// the module's string table).
pub(crate) fn build_text(ctx: &CompilationContext, text: &str) -> Function {
    let mut f = function(&[ValType::I32; 2]);
    crate::compiler::runtime::return_text(&mut f, ctx, text, 0, 1);
    emit!(f, I::I32Const(0), I::End);
    f
}

fn alloc_exception(f: &mut Function, ctx: &CompilationContext, class: &str, obj: u32) {
    let info = ctx
        .get_class_info(class)
        .unwrap_or_else(|| panic!("the builtin exception {class} is in the module"));
    emit!(
        f,
        I::I32Const(info.instance_size as i32),
        I::I32Const(info.class_id),
        I::Call(ctx.alloc_obj_func_index),
        I::LocalSet(obj)
    );
}

fn set_pending(f: &mut Function, obj: u32) {
    emit!(
        f,
        I::LocalGet(obj),
        I::GlobalSet(EXC_VALUE_GLOBAL),
        I::LocalGet(obj),
        I::I32Load(w32(0)),
        I::GlobalSet(EXC_TYPE_GLOBAL)
    );
}

/// `(message) -> ()`: leave a new `class(message)` pending.
pub(crate) fn build_raise_text(ctx: &CompilationContext, class: &str) -> Function {
    let mut f = function(&[ValType::I32; 3]);
    let (msg, obj) = (0, 1);
    if ctx.get_class_info(class).is_none() {
        emit!(f, I::Unreachable, I::End);
        return f;
    }
    alloc_exception(&mut f, ctx, class, obj);
    emit_fill_args(&mut f, ctx, obj, class, &[(msg, IRType::String)], 2, 3);
    set_pending(&mut f, obj);
    emit!(f, I::End);
    f
}

/// `(value) -> ()`: leave a new `class(value)` pending.
pub(crate) fn build_raise_value(ctx: &CompilationContext, class: &str, ty: &IRType) -> Function {
    let mut f = function(&[ValType::I32; 3]);
    let (value, obj) = (0, 1);
    if ctx.get_class_info(class).is_none() {
        emit!(f, I::Unreachable, I::End);
        return f;
    }
    alloc_exception(&mut f, ctx, class, obj);
    emit_fill_args(&mut f, ctx, obj, class, &[(value, ty.clone())], 2, 3);
    set_pending(&mut f, obj);
    emit!(f, I::End);
    f
}

/// `(obj) -> str`: the name of the instance's class, from its tag.
pub(crate) fn build_class_name(ctx: &CompilationContext, by_id: Option<bool>) -> Function {
    let mut f = function(&[ValType::I32; 2]);
    let (obj, len, out) = (0, 1, 2);
    let mut classes: Vec<(&String, i32)> =
        ctx.class_map.iter().map(|(n, c)| (n, c.class_id)).collect();
    classes.sort_by_key(|(_, id)| *id);
    for (name, id) in classes {
        emit!(f, I::LocalGet(obj));
        // An instance's class is its tag; a class object is its id.
        if by_id.is_none() {
            emit!(f, I::I32Load(w32(0)));
        }
        emit!(f, I::I32Const(id), I::I32Eq, I::If(BlockType::Empty));
        let text = match by_id {
            Some(true) if is_builtin_exception(name) => format!("<class '{name}'>"),
            Some(true) => format!(
                "<class '{}.{name}'>",
                ctx.class_modules
                    .get(name.as_str())
                    .map_or("__main__", String::as_str)
            ),
            _ => name.clone(),
        };
        crate::compiler::runtime::return_text(&mut f, ctx, &text, len, out);
        emit!(f, I::End);
    }
    emit!(f, I::Unreachable, I::End);
    f
}

/// `_start`'s report of an exception nothing caught, the last line CPython
/// writes for it: `Name: str(e)`, or `Name` when that is empty, to standard
/// error.
pub(crate) fn emit_report_uncaught(
    f: &mut Function,
    ctx: &CompilationContext,
    write_fd: Option<u32>,
) {
    let Some(write_fd) = write_fd else {
        return;
    };
    let concat = ctx.helper(Helper::Concat);
    let (line, text) = (0, 1);
    // A SystemExit is the program exiting: its argument printed, if that is
    // what it asks, then its status.
    if let (Some(exit), Some(info)) = (
        ctx.wasi
            .get(&crate::compiler::host::Wasi::ProcExit)
            .copied(),
        ctx.get_class_info("SystemExit"),
    ) {
        let code = info.field_offsets["__exit_code"];
        let print = info.field_offsets["__exit_print"];
        emit_pending_is(f, ctx, "SystemExit");
        emit!(
            f,
            I::If(BlockType::Empty),
            I::GlobalGet(EXC_VALUE_GLOBAL),
            I::I32Load(w32(print)),
            I::If(BlockType::Empty),
            I::I32Const(2),
            I::GlobalGet(EXC_VALUE_GLOBAL),
            I::I32Load(w32(field_offset(ctx, "__exc_str"))),
            I::Call(ctx.helper(Helper::ExcText("\n".to_string()))),
            I::Call(concat),
            I::Call(write_fd),
            I::End,
            I::GlobalGet(EXC_VALUE_GLOBAL),
            I::I32Load(w32(code)),
            I::Call(exit),
            I::End
        );
    }
    emit!(
        f,
        I::GlobalGet(EXC_VALUE_GLOBAL),
        I::Call(ctx.helper(Helper::ClassName)),
        I::LocalSet(line),
        // str(e), dispatched to the instance's own __str__
        I::GlobalGet(EXC_VALUE_GLOBAL),
        I::I32Const(0),
        I::GlobalSet(EXC_TYPE_GLOBAL),
        I::Call(ctx.helper(Helper::Str(IRType::Class("BaseException".to_string())))),
        I::LocalTee(text),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::If(BlockType::Empty),
        I::LocalGet(line),
        I::Call(ctx.helper(Helper::ExcText(": ".to_string()))),
        I::Call(concat),
        I::LocalGet(text),
        I::Call(concat),
        I::LocalSet(line),
        I::End,
        I::LocalGet(line),
        I::Call(ctx.helper(Helper::ExcText("\n".to_string()))),
        I::Call(concat),
        I::LocalSet(line),
        I::I32Const(2),
        I::LocalGet(line),
        I::Call(write_fd)
    );
}

/// `e.args` of the exception on the stack: the tuple of its arguments,
/// kept when they were all strs. Another argument has no type this place
/// can read, so that traps.
pub(crate) fn emit_args(func: &mut Function, ctx: &CompilationContext) -> IRType {
    let obj = ctx.temp_local;
    emit!(
        func,
        I::LocalTee(obj),
        I::I32Load(w32(field_offset(ctx, "__exc_all_str"))),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::LocalGet(obj),
        I::I32Load(w32(field_offset(ctx, "__exc_args")))
    );
    IRType::VarTuple(Box::new(IRType::String))
}
