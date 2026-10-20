use crate::compiler::context::FileIoImports;
use crate::compiler::context::{
    common_class_base, ClassInfo, CompilationContext, EXC_TYPE_GLOBAL, FIRST_MODULE_GLOBAL,
    MODULE_INIT_FN,
};
use crate::compiler::function::{compile_function, resolve_return_type};
use crate::core::errors::ChakraError;
use crate::ir::{
    method_kind, IRBody, IRConstant, IRExpr, IRFunction, IRModule, IROp, IRStatement, IRType,
    MethodKind, STRING_LEN_PREFIX,
};
use std::collections::{HashMap, HashSet};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, DataSection, ElementSection, Elements,
    EntityType, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    Instruction, MemorySection, MemoryType, Module, RefType, StartSection, TableSection, TableType,
    TypeSection, ValType,
};

/// Name of the custom section carrying the Python comments of the compiled
/// sources.
pub const COMMENTS_SECTION_NAME: &str = "python.comments";

/// Map IR type to WebAssembly ValType
fn ir_type_to_wasm_type(ir_type: &IRType) -> ValType {
    match ir_type {
        IRType::Float => ValType::F64,
        IRType::Int | IRType::Bool | IRType::String => ValType::I32,
        IRType::Class(_) => ValType::I32, // References to classes are pointers (i32)
        IRType::List(_) | IRType::Dict(_, _) | IRType::Tuple(_) => ValType::I32, // Collections are pointers
        IRType::Optional(_) | IRType::Union(_) => ValType::I32, // References to optionals/unions
        _ => ValType::I32,
    }
}

/// Method-kind classification for a method whose decorator stack was already
/// validated during IR conversion, so failure cannot recur here.
fn kind_of(class_name: &str, method: &IRFunction) -> MethodKind {
    method_kind(class_name, &method.name, &method.decorators).unwrap_or(MethodKind::Instance)
}

/// Registry key for a method's signature (`Class::name`). A property's getter
/// and setter share a Python name, so the setter is keyed `Class::name::setter`
/// to keep both signatures addressable.
fn method_registry_key(class_name: &str, method: &IRFunction) -> String {
    match kind_of(class_name, method) {
        MethodKind::PropertySetter => format!("{class_name}::{}::setter", method.name),
        _ => format!("{class_name}::{}", method.name),
    }
}

/// Is this expression a reference to the implicit `self` parameter?
fn is_self_ref(expr: &IRExpr) -> bool {
    matches!(expr, IRExpr::Variable(n) | IRExpr::Param(n) if n == "self")
}

/// Best-effort type inference for a class field initializer. Only needs to
/// distinguish floats (an f64 slot) from the i32-shaped default; `params` maps
/// the enclosing method's parameter names to their annotated types so that
/// `self.width = width` adopts `width`'s declared type. `classes` names the
/// module's classes, so `self.origin = Point()` records the instance's class
/// rather than an untyped pointer.
fn infer_field_value_type(
    value: &IRExpr,
    params: &HashMap<String, IRType>,
    classes: &HashSet<String>,
) -> IRType {
    match value {
        IRExpr::FunctionCall { function_name, .. } if classes.contains(function_name) => {
            IRType::Class(function_name.clone())
        }
        IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
        IRExpr::Const(IRConstant::Int(_)) => IRType::Int,
        IRExpr::Const(IRConstant::Bool(_)) => IRType::Bool,
        // A string field started from a literal (`self.name = ""`) was typed
        // Unknown, so every read of it lost the string's length: `len()` of an
        // untyped value loads its first word as a collection count, and
        // `c.s = "abc"; len(c.s)` answered 6513249, which is "abc" read as an
        // integer. Only fields set from an annotated parameter were right.
        IRExpr::Const(IRConstant::String(_)) => IRType::String,
        IRExpr::Const(IRConstant::Bytes(_)) => IRType::Bytes,
        // Builtins whose result type does not depend on their argument.
        IRExpr::FunctionCall { function_name, .. }
            if matches!(
                function_name.as_str(),
                "str" | "int" | "len" | "float" | "bool"
            ) =>
        {
            match function_name.as_str() {
                "str" => IRType::String,
                "float" => IRType::Float,
                "bool" => IRType::Bool,
                _ => IRType::Int,
            }
        }
        IRExpr::Variable(name) | IRExpr::Param(name) => {
            params.get(name).cloned().unwrap_or(IRType::Unknown)
        }
        IRExpr::Attribute { object, attribute } if is_self_ref(object) => params
            .get(&format!("self.{attribute}"))
            .cloned()
            .unwrap_or(IRType::Unknown),
        // An element (a suspending `for` in a generator walks its sequence
        // by index into a field).
        IRExpr::Indexing { container, .. } => {
            match infer_field_value_type(container, params, classes) {
                IRType::List(element) | IRType::VarTuple(element) | IRType::Set(element) => {
                    *element
                }
                IRType::Dict(_, value) => *value,
                IRType::String => IRType::String,
                _ => IRType::Unknown,
            }
        }
        // A field of an instance of a class laid out already (`self.n.children`
        // in a generator, whose parameter `n` is a field of its own).
        IRExpr::Attribute { object, attribute } => {
            match infer_field_value_type(object, params, classes) {
                IRType::Class(class) => params
                    .get(&format!("{FIELD_OF}{class}.{attribute}"))
                    .cloned()
                    .unwrap_or(IRType::Unknown),
                _ => IRType::Unknown,
            }
        }
        // A dict loop's key or value (see the dict loops in `ir::converter`).
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if function_name == crate::ir::DICT_KEY_AT_FN
            || function_name == crate::ir::DICT_VAL_AT_FN =>
        {
            match arguments
                .first()
                .map(|d| infer_field_value_type(d, params, classes))
            {
                Some(IRType::Dict(k, _)) if function_name == crate::ir::DICT_KEY_AT_FN => *k,
                Some(IRType::Dict(_, v)) => *v,
                _ => IRType::Unknown,
            }
        }
        IRExpr::BinaryOp { left, right, op } => {
            let lt = infer_field_value_type(left, params, classes);
            let rt = infer_field_value_type(right, params, classes);
            // `/` is true division whatever its operands (#132).
            if lt == IRType::Float || rt == IRType::Float || matches!(op, IROp::Div) {
                IRType::Float
            } else {
                lt
            }
        }
        IRExpr::UnaryOp { operand, .. } => infer_field_value_type(operand, params, classes),
        IRExpr::Conditional { body, orelse, .. } => match (
            infer_field_value_type(body, params, classes),
            infer_field_value_type(orelse, params, classes),
        ) {
            (a, b) if a == b => a,
            // An int arm the inference cannot see would be widened.
            (IRType::Unknown, t) | (t, IRType::Unknown) if t != IRType::Float => t,
            _ => IRType::Unknown,
        },
        // Collection-valued fields (e.g. a dict literal stored by a lifted
        // generator local) keep their kind so consumers like len() and
        // indexing know the slot holds a collection pointer.
        IRExpr::ListLiteral(elems) => {
            IRType::List(Box::new(shared_element_type(elems, params, classes)))
        }
        IRExpr::SetLiteral(elems) => {
            IRType::Set(Box::new(shared_element_type(elems, params, classes)))
        }
        IRExpr::TupleLiteral(_) => IRType::Tuple(Vec::new()),
        IRExpr::DictLiteral(entries) => {
            let (keys, values): (Vec<IRExpr>, Vec<IRExpr>) = entries.iter().cloned().unzip();
            IRType::Dict(
                Box::new(shared_element_type(&keys, params, classes)),
                Box::new(shared_element_type(&values, params, classes)),
            )
        }
        IRExpr::RangeCall { .. } => IRType::Range,
        _ => IRType::Unknown,
    }
}

/// The type every element of a collection literal shares, or `Unknown` when the
/// literal is empty or mixed.
fn shared_element_type(
    elems: &[IRExpr],
    params: &HashMap<String, IRType>,
    classes: &HashSet<String>,
) -> IRType {
    let mut types = elems
        .iter()
        .map(|e| infer_field_value_type(e, params, classes));
    let Some(first) = types.next() else {
        return IRType::Unknown;
    };
    if types.all(|t| t == first) {
        first
    } else {
        IRType::Unknown
    }
}

/// Collect the element types a method contributes to `self.<field>` collections
/// through mutation: `self.items.append(v)` and `self.items.insert(i, v)`.
///
/// A field initialized to an empty literal (`self.items = []`, the ordinary way
/// to start a collection) has no element type of its own, so what the class puts
/// into it is the only evidence there is. Without this, iterating the field
/// binds an untyped element, and reading a member or calling a method on that
/// element resolves to nothing.
///
/// `env` starts as the method's parameters and grows as local assignments are
/// walked in order, so the common two-step form resolves too:
///
/// ```text
/// item = Item(name, price)
/// self.items.append(item)
/// ```
fn collect_self_field_elements(
    body: &IRBody,
    env: &mut HashMap<String, IRType>,
    classes: &HashSet<String>,
    out: &mut Vec<(String, IRType)>,
) {
    for stmt in &body.statements {
        if let IRStatement::Assign {
            target,
            value,
            var_type,
        } = stmt
        {
            let ty = var_type
                .clone()
                .unwrap_or_else(|| infer_field_value_type(value, env, classes));
            env.insert(target.clone(), ty);
        }

        if let IRStatement::Expression(IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        }) = stmt
        {
            // `append(v)` takes the element first; `insert(i, v)` second.
            let element = match method_name.as_str() {
                "append" => arguments.first(),
                "insert" => arguments.get(1),
                _ => None,
            };
            if let (
                IRExpr::Attribute {
                    object: receiver,
                    attribute,
                },
                Some(element),
            ) = (object.as_ref(), element)
            {
                if is_self_ref(receiver) {
                    out.push((
                        attribute.clone(),
                        infer_field_value_type(element, env, classes),
                    ));
                }
            }
        }

        for nested in nested_bodies(stmt) {
            collect_self_field_elements(nested, &mut env.clone(), classes, out);
        }
    }
}

/// Which functions can raise, directly or through anything they call.
///
/// Only calls to these need an exception check after them, and only these need
/// the propagation code on their exit paths, so a program that never raises
/// compiles to exactly what it did before exceptions transferred control.
///
/// Resolution is by name and deliberately conservative: a method call marks
/// every method of that name, and a call to a name that resolves to nothing
/// (a closure reached through a variable) counts as raising as soon as the
/// module contains any raiser at all. Over-approximating costs a few checks;
/// under-approximating would drop an exception on the floor.
fn functions_that_can_raise(ir_module: &IRModule) -> HashSet<String> {
    // Registry key -> (raises directly, names it calls).
    let mut facts: HashMap<String, (bool, HashSet<String>)> = HashMap::new();
    // A called name -> the registry keys it could reach.
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();

    let sources = OptionalSources::of(ir_module);
    let record = |facts: &mut HashMap<String, (bool, HashSet<String>)>,
                  by_name: &mut HashMap<String, Vec<String>>,
                  key: String,
                  body: &IRBody,
                  params: &[crate::ir::IRParam],
                  call_name: &str| {
        let mut raises = may_unbox_none(body, params, &sources);
        let mut calls = HashSet::new();
        scan_raise_and_calls(body, &mut raises, &mut calls);
        by_name
            .entry(call_name.to_string())
            .or_default()
            .push(key.clone());
        facts.insert(key, (raises, calls));
    };

    for func in &ir_module.functions {
        record(
            &mut facts,
            &mut by_name,
            func.name.clone(),
            &func.body,
            &func.params,
            &func.name,
        );
    }
    let nothing = IRBody {
        statements: Vec::new(),
    };
    for cls in &ir_module.classes {
        for method in &cls.methods {
            let key = method_registry_key(&cls.name, method);
            // A builtin exception's own methods cannot raise.
            let body = if crate::compiler::exceptions::is_builtin_exception(&cls.name) {
                &nothing
            } else {
                &method.body
            };
            record(
                &mut facts,
                &mut by_name,
                key.clone(),
                body,
                &method.params,
                &method.name,
            );
            // `ClassName(...)` runs `__init__`.
            if method.name == "__init__" {
                by_name.entry(cls.name.clone()).or_default().push(key);
            }
        }
    }

    let mut raisers: HashSet<String> = facts
        .iter()
        .filter(|(_, (raises, _))| *raises)
        .map(|(key, _)| key.clone())
        .collect();
    // A call that resolves to nothing is a closure or a builtin. Only closures
    // can raise, and only if the module raises anywhere at all.
    let unresolved_can_raise = !raisers.is_empty();

    loop {
        let mut grew = false;
        for (key, (_, calls)) in &facts {
            if raisers.contains(key) {
                continue;
            }
            let reaches = calls.iter().any(|name| match by_name.get(name) {
                Some(keys) => keys.iter().any(|k| raisers.contains(k)),
                None => unresolved_can_raise,
            });
            if reaches {
                raisers.insert(key.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    raisers
}

/// Whether any body in the module has a `try` or a `raise`.
fn module_handles_exceptions(ir_module: &IRModule) -> bool {
    fn body_has(body: &IRBody) -> bool {
        body.statements.iter().any(|stmt| {
            matches!(
                stmt,
                IRStatement::TryExcept { .. } | IRStatement::Raise { .. }
            ) || nested_bodies(stmt).into_iter().any(body_has)
        })
    }
    ir_module
        .functions
        .iter()
        .chain(ir_module.classes.iter().flat_map(|c| c.methods.iter()))
        .any(|f| body_has(&f.body))
}

/// The positions of the module's lifted closures, each after the closure
/// that creates it (when one does), so its creator is always compiled first.
pub(crate) fn closures_outermost_first(ir_module: &IRModule) -> Vec<usize> {
    let mut creator: HashMap<String, String> = HashMap::new();
    for func in &ir_module.functions {
        let mut raises = false;
        let mut calls = HashSet::new();
        scan_raise_and_calls(&func.body, &mut raises, &mut calls);
        for name in calls {
            if name.starts_with("__lambda_") && name != func.name {
                creator.insert(name, func.name.clone());
            }
        }
    }
    let depth = |name: &str| {
        let mut depth = 0;
        let mut current = name.to_string();
        while let Some(parent) = creator.get(&current) {
            depth += 1;
            if depth > ir_module.functions.len() {
                break;
            }
            current = parent.clone();
        }
        depth
    };
    let mut closures: Vec<(usize, usize)> = ir_module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name.starts_with("__lambda_"))
        .map(|(i, f)| (depth(&f.name), i))
        .collect();
    closures.sort();
    closures.into_iter().map(|(_, i)| i).collect()
}

/// Collect whether a body raises, and every name it calls.
fn scan_raise_and_calls(body: &IRBody, raises: &mut bool, calls: &mut HashSet<String>) {
    for stmt in &body.statements {
        if let IRStatement::Raise { exception, .. } = stmt {
            // `raise StopIteration` is the generator protocol's own signal and
            // travels on its own global, not the exception path.
            let name = match exception {
                Some(IRExpr::FunctionCall { function_name, .. }) => Some(function_name.as_str()),
                Some(IRExpr::Variable(name)) | Some(IRExpr::Param(name)) => Some(name.as_str()),
                _ => None,
            };
            if name != Some("StopIteration") {
                *raises = true;
            }
        }
        if let IRStatement::For {
            iterable: IRExpr::Attribute { attribute, .. },
            ..
        } = stmt
        {
            // `for line in sys.stdin` reads, and decodes, standard input.
            if attribute == "stdin" {
                *raises = true;
            }
        }
        if matches!(stmt, IRStatement::IndexAssign { .. }) {
            // An assignment through an index bounds-checks and can raise
            // IndexError, exactly as a read does.
            *raises = true;
        }
        // Unpacking a value of the wrong length raises ValueError, unless the
        // value is a literal whose length is visibly right.
        if let IRStatement::TupleUnpack {
            targets,
            value,
            starred,
        } = stmt
        {
            let visibly_right = match value {
                IRExpr::TupleLiteral(items) | IRExpr::ListLiteral(items) => match starred {
                    None => items.len() == targets.len(),
                    Some(_) => items.len() + 1 >= targets.len(),
                },
                _ => false,
            };
            if !visibly_right {
                *raises = true;
            }
        }
        for expr in statement_exprs(stmt) {
            scan_expr_calls(expr, raises, calls);
        }
        for nested in nested_bodies(stmt) {
            scan_raise_and_calls(nested, raises, calls);
        }
    }
}

/// Every call name an expression mentions, recursively, and whether it
/// contains anything that can raise on its own. Indexing is the one that does:
/// an out-of-range index raises `IndexError` and a missing key `KeyError`, and
/// the container's type is not known here, so any indexing counts.
/// What can hold an optional number: functions returning one and fields
/// annotated as one, across the module.
struct OptionalSources {
    functions: HashSet<String>,
    fields: HashSet<String>,
}

fn optional_number(t: &IRType) -> bool {
    matches!(t, IRType::Optional(inner) if matches!(**inner, IRType::Int | IRType::Bool | IRType::Float))
}

impl OptionalSources {
    fn of(module: &IRModule) -> Self {
        let functions = module
            .functions
            .iter()
            .filter(|f| optional_number(&f.return_type))
            .map(|f| f.name.clone())
            .collect();
        let mut fields = HashSet::new();
        for class in &module.classes {
            for var in &class.class_vars {
                if var.var_type.as_ref().is_some_and(optional_number) {
                    fields.insert(var.name.clone());
                }
            }
            for method in &class.methods {
                let mut annotated = Vec::new();
                collect_annotated_self_fields(&method.body, &mut annotated);
                fields.extend(
                    annotated
                        .into_iter()
                        .filter(|(_, t)| optional_number(t))
                        .map(|(n, _)| n),
                );
            }
        }
        OptionalSources { functions, fields }
    }
}

/// Whether a body may unbox an optional number that is None, which raises
/// TypeError: it reads one (a parameter, local, field, or call result
/// declared optional, or a one-argument `dict.get`) and does arithmetic, an
/// ordering, or a negation. Types are not known yet here, so this is the
/// conservative reading.
fn may_unbox_none(body: &IRBody, params: &[crate::ir::IRParam], sources: &OptionalSources) -> bool {
    let mut locals: HashSet<String> = params
        .iter()
        .filter(|p| optional_number(&p.param_type))
        .map(|p| p.name.clone())
        .collect();
    fn annotated(body: &IRBody, out: &mut HashSet<String>) {
        for stmt in &body.statements {
            if let IRStatement::Assign {
                target,
                var_type: Some(t),
                ..
            } = stmt
            {
                if optional_number(t) {
                    out.insert(target.clone());
                }
            }
            for nested in nested_bodies(stmt) {
                annotated(nested, out);
            }
        }
    }
    annotated(body, &mut locals);
    let mut reads = !locals.is_empty();
    let mut operates = false;
    let mut probe = body.clone();
    let _ = crate::ir::visit_body(&mut probe, &mut |e: &mut IRExpr| {
        match e {
            IRExpr::FunctionCall { function_name, .. }
                if sources.functions.contains(function_name) =>
            {
                reads = true
            }
            IRExpr::Attribute { attribute, .. } if sources.fields.contains(attribute) => {
                reads = true
            }
            IRExpr::MethodCall {
                method_name,
                arguments,
                ..
            } if method_name == "get" && arguments.len() == 1 => reads = true,
            IRExpr::BinaryOp { .. } => operates = true,
            IRExpr::CompareOp {
                op:
                    crate::ir::IRCompareOp::Lt
                    | crate::ir::IRCompareOp::LtE
                    | crate::ir::IRCompareOp::Gt
                    | crate::ir::IRCompareOp::GtE,
                ..
            } => operates = true,
            IRExpr::UnaryOp { op, .. } if !matches!(op, crate::ir::IRUnaryOp::Not) => {
                operates = true
            }
            _ => {}
        }
        Ok(())
    });
    reads && operates
}

fn scan_expr_calls(expr: &IRExpr, raises: &mut bool, calls: &mut HashSet<String>) {
    match expr {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            calls.insert(function_name.clone());
            // `int(s)` raises ValueError when `s` is a string that does not
            // spell an integer. Types are not known yet here, so anything but a
            // plainly numeric argument counts: an extra check after a call costs
            // a load and a branch, and a missing one strands the exception.
            // `float(s)` likewise, for a string that does not spell a float.
            // `input()` raises EOFError at the end of input.
            // `input()` and `open()` (a missing file is FileNotFoundError).
            if function_name == "input" || function_name == "open" {
                *raises = true;
            }
            if function_name == "int" || function_name == "float" {
                let plainly_numeric = matches!(
                    arguments.first(),
                    Some(
                        IRExpr::Const(
                            IRConstant::Int(_) | IRConstant::Float(_) | IRConstant::Bool(_)
                        ) | IRExpr::BinaryOp { .. }
                            | IRExpr::UnaryOp { .. }
                    )
                );
                if !plainly_numeric {
                    *raises = true;
                }
            }
            for arg in arguments {
                scan_expr_calls(arg, raises, calls);
            }
        }
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            calls.insert(method_name.clone());
            // `index()` and `remove()` raise when the value is absent (ValueError,
            // or KeyError from a set) and `pop()` raises IndexError, so a
            // function calling one can raise and its callers must check on the
            // way back.
            if matches!(
                method_name.as_str(),
                "index" | "remove" | "pop" | crate::ir::DEL_ITEM_METHOD
            ) {
                *raises = true;
            }
            // Reading standard input raises UnicodeDecodeError on bytes that
            // are not UTF-8.
            if matches!(method_name.as_str(), "read" | "readline" | "readlines") {
                *raises = true;
            }
            // `sys.exit()` raises SystemExit.
            if method_name == "exit" && matches!(object.as_ref(), IRExpr::Variable(m) if m == "sys")
            {
                *raises = true;
            }
            scan_expr_calls(object, raises, calls);
            for arg in arguments {
                scan_expr_calls(arg, raises, calls);
            }
        }
        IRExpr::BinaryOp { left, right, op } => {
            // Division by zero raises ZeroDivisionError, unless the divisor is
            // a nonzero literal and cannot be zero.
            if matches!(op, IROp::Div | IROp::Mod | IROp::FloorDiv)
                && !crate::compiler::expression::divisor_is_never_zero(right)
            {
                *raises = true;
            }
            scan_expr_calls(left, raises, calls);
            scan_expr_calls(right, raises, calls);
        }
        IRExpr::CompareOp { left, right, .. } | IRExpr::BoolOp { left, right, .. } => {
            scan_expr_calls(left, raises, calls);
            scan_expr_calls(right, raises, calls);
        }
        IRExpr::UnaryOp { operand, .. } => scan_expr_calls(operand, raises, calls),
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            scan_expr_calls(condition, raises, calls);
            scan_expr_calls(body, raises, calls);
            scan_expr_calls(orelse, raises, calls);
        }
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            for item in items {
                scan_expr_calls(item, raises, calls);
            }
        }
        IRExpr::DictLiteral(pairs) => {
            for (k, v) in pairs {
                scan_expr_calls(k, raises, calls);
                scan_expr_calls(v, raises, calls);
            }
        }
        IRExpr::Indexing { container, index } => {
            *raises = true;
            scan_expr_calls(container, raises, calls);
            scan_expr_calls(index, raises, calls);
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            scan_expr_calls(container, raises, calls);
            for part in [start, end, step].into_iter().flatten() {
                scan_expr_calls(part, raises, calls);
            }
        }
        IRExpr::Attribute { object, .. } => scan_expr_calls(object, raises, calls),
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            scan_expr_calls(element, raises, calls);
            if let Some(value) = value {
                scan_expr_calls(value, raises, calls);
            }
            for generator in generators {
                scan_expr_calls(&generator.iterable, raises, calls);
                for condition in &generator.conditions {
                    scan_expr_calls(condition, raises, calls);
                }
            }
        }
        IRExpr::RangeCall { start, stop, step } => {
            scan_expr_calls(stop, raises, calls);
            for part in [start, step].into_iter().flatten() {
                scan_expr_calls(part, raises, calls);
            }
        }
        IRExpr::Lambda { body, .. } => scan_expr_calls(body, raises, calls),
        IRExpr::ClosureMake { lambda_name, .. } => {
            calls.insert(lambda_name.clone());
        }
        IRExpr::DynamicImportExpr { module_name } => scan_expr_calls(module_name, raises, calls),
        _ => {}
    }
}

/// Every expression a statement evaluates directly (nested bodies are walked
/// separately).
fn statement_exprs(stmt: &IRStatement) -> Vec<&IRExpr> {
    match stmt {
        IRStatement::Return(expr) | IRStatement::Yield { value: expr } => expr.iter().collect(),
        IRStatement::Raise { exception, cause } => exception.iter().chain(cause.iter()).collect(),
        IRStatement::Assign { value, .. }
        | IRStatement::AugAssign { value, .. }
        | IRStatement::Expression(value)
        | IRStatement::TupleUnpack { value, .. } => vec![value],
        IRStatement::If { condition, .. } | IRStatement::While { condition, .. } => vec![condition],
        IRStatement::For { iterable, .. } => vec![iterable],
        IRStatement::With { context_expr, .. } => vec![context_expr],
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => vec![object, value],
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => vec![container, index, value],
        IRStatement::DynamicImport { module_name, .. } => vec![module_name],
        _ => Vec::new(),
    }
}

/// The bodies a statement owns, for the recursive field scans.
fn nested_bodies(stmt: &IRStatement) -> Vec<&IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut bodies = vec![then_body.as_ref()];
            bodies.extend(else_body.as_deref());
            bodies
        }
        IRStatement::While { body, .. } => vec![body.as_ref()],
        IRStatement::For {
            body, else_body, ..
        } => {
            let mut bodies = vec![body.as_ref()];
            bodies.extend(else_body.as_deref());
            bodies
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let mut bodies = vec![try_body.as_ref()];
            bodies.extend(except_handlers.iter().map(|h| &h.body));
            bodies.extend(finally_body.as_deref());
            bodies
        }
        _ => Vec::new(),
    }
}

/// The prefix of the entries giving field inference the fields of the
/// classes laid out before the one being laid out (`#Class.field`).
const FIELD_OF: &str = "#";

/// Every `self.x: T = v` in a method body, recursing into nested blocks, with
/// its annotated type.
fn collect_annotated_self_fields(body: &IRBody, out: &mut Vec<(String, IRType)>) {
    for stmt in &body.statements {
        if let IRStatement::AttributeAssign {
            object,
            attribute,
            annotation: Some(ty),
            ..
        } = stmt
        {
            if is_self_ref(object) {
                out.push((attribute.clone(), ty.clone()));
            }
        }
        for nested in nested_bodies(stmt) {
            collect_annotated_self_fields(nested, out);
        }
    }
}

/// Collect `self.<field> = value` assignments (including augmented ones) from a
/// method body, recursing into nested blocks, with each field's inferred type.
fn collect_self_fields(
    body: &IRBody,
    params: &mut HashMap<String, IRType>,
    classes: &HashSet<String>,
    out: &mut Vec<(String, IRType)>,
) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
                annotation,
            } if is_self_ref(object) => {
                let ty = annotation
                    .clone()
                    .unwrap_or_else(|| infer_field_value_type(value, params, classes));
                // A later field's value may read this one (`self.k =
                // __dict_key_at(self.__dict_0, i)` in a generator).
                params
                    .entry(format!("self.{attribute}"))
                    .or_insert_with(|| ty.clone());
                out.push((attribute.clone(), ty));
            }
            IRStatement::AttributeAugAssign {
                object,
                attribute,
                value,
                ..
            } if is_self_ref(object) => {
                out.push((
                    attribute.clone(),
                    infer_field_value_type(value, params, classes),
                ));
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_self_fields(then_body, params, classes, out);
                if let Some(else_body) = else_body {
                    collect_self_fields(else_body, params, classes, out);
                }
            }
            IRStatement::While { body, .. } => collect_self_fields(body, params, classes, out),
            IRStatement::For {
                target,
                iterable,
                body,
                ..
            } => {
                // The loop variable holds an element, which a field copied
                // from it holds too.
                let element = match iterable {
                    IRExpr::RangeCall { .. } => IRType::Int,
                    other => match infer_field_value_type(other, params, classes) {
                        IRType::List(e) | IRType::VarTuple(e) | IRType::Set(e) => *e,
                        IRType::Dict(k, _) => *k,
                        IRType::String => IRType::String,
                        _ => IRType::Unknown,
                    },
                };
                params.insert(target.clone(), element);
                collect_self_fields(body, params, classes, out)
            }
            IRStatement::TupleUnpack { targets, value, .. } => {
                let members = match infer_field_value_type(value, params, classes) {
                    IRType::Tuple(members) => members,
                    IRType::List(e) | IRType::VarTuple(e) => vec![*e; targets.len()],
                    _ => Vec::new(),
                };
                for (k, target) in targets.iter().enumerate() {
                    params.insert(
                        target.clone(),
                        members.get(k).cloned().unwrap_or(IRType::Unknown),
                    );
                }
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                collect_self_fields(try_body, params, classes, out);
                for handler in except_handlers {
                    // The `as` name holds the handler's class, or the
                    // nearest base of several, which for a tuple of builtin
                    // exceptions can only be said as BaseException here.
                    if let Some(name) = &handler.name {
                        let class = match handler.exception_types.as_slice() {
                            [one] => one.clone(),
                            _ => "BaseException".to_string(),
                        };
                        params.insert(name.clone(), IRType::Class(class));
                    }
                    collect_self_fields(&handler.body, params, classes, out);
                }
                if let Some(finally_body) = finally_body {
                    collect_self_fields(finally_body, params, classes, out);
                }
            }
            _ => {}
        }
    }
}

/// The class variables some function assigns through its class's name
/// (`Counter.count += 1`), as `Owner.var` keys. Each lives in a global even
/// when it starts as a constant, which every read would otherwise inline.
fn written_class_vars(module: &IRModule) -> HashSet<String> {
    let owner_of = |class: &str, var: &str| -> Option<String> {
        let mut pending = vec![class.to_string()];
        let mut seen = HashSet::new();
        while let Some(c) = pending.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            let cls = module.classes.iter().find(|k| k.name == c)?;
            if cls.class_vars.iter().any(|v| v.name == var) {
                return Some(c);
            }
            pending.extend(cls.bases.iter().rev().cloned());
        }
        None
    };
    let mut out = HashSet::new();
    fn walk(body: &IRBody, found: &mut Vec<(String, String)>) {
        for stmt in &body.statements {
            if let IRStatement::AttributeAssign {
                object, attribute, ..
            }
            | IRStatement::AttributeAugAssign {
                object, attribute, ..
            } = stmt
            {
                if let IRExpr::Variable(name) = object {
                    found.push((name.clone(), attribute.clone()));
                }
            }
            for nested in nested_bodies(stmt) {
                walk(nested, found);
            }
        }
    }
    let mut found = Vec::new();
    for f in module
        .functions
        .iter()
        .chain(module.classes.iter().flat_map(|c| c.methods.iter()))
    {
        walk(&f.body, &mut found);
    }
    for (class, var) in found {
        if let Some(owner) = owner_of(&class, &var) {
            out.insert(format!("{owner}.{var}"));
        }
    }
    out
}

/// Build the runtime bump allocator `__alloc(size: i32) -> i32`.
///
/// The next-free pointer lives in global 0 (initialized to the post-codegen
/// high-water mark). Each call rounds `size` up to 8 bytes, grows linear memory
/// if the request would run past the current end, advances the global, and
/// returns the old pointer. There is no `free`; this is a monotonic bump
/// allocator backing runtime-built strings/bytes (e.g. concatenation results).
fn build_alloc_function() -> Function {
    // locals 1..4 (local 0 is the `size` parameter): aligned size, old ptr,
    // new ptr, current memory size in bytes.
    let mut f = Function::new([(4u32, ValType::I32)]);

    // aligned = (size + 7) & ~7  -> local 1
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(7));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Const(!7));
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::LocalSet(1));

    // old = global 0 -> local 2
    f.instruction(&Instruction::GlobalGet(0));
    f.instruction(&Instruction::LocalSet(2));

    // new = old + aligned -> local 3
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));

    // cur_bytes = memory.size * 65536 -> local 4
    f.instruction(&Instruction::MemorySize(0));
    f.instruction(&Instruction::I32Const(16));
    f.instruction(&Instruction::I32Shl);
    f.instruction(&Instruction::LocalSet(4));

    // if new > cur_bytes: grow by ceil((new - cur_bytes) / 65536) pages
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::I32Const(65535));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Const(16));
    f.instruction(&Instruction::I32ShrU);
    f.instruction(&Instruction::MemoryGrow(0));
    f.instruction(&Instruction::Drop); // grow failure (-1) is left to trap on use
    f.instruction(&Instruction::End);

    // global 0 = new; return old
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::GlobalSet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::End);
    f
}

/// Build `__alloc_obj(size: i32, class_id: i32) -> i32`: allocate an instance
/// via `__alloc` and stamp `class_id` into the tag word at offset 0 (the slot
/// every instance layout reserves), returning the instance pointer. Doing the
/// stamp in a helper keeps the `ClassName(...)` call sequence stack-only, so
/// nested instantiations compose without any scratch local.
fn build_alloc_obj_function(alloc_func_index: u32) -> Function {
    // local 2 (locals 0-1 are the parameters): the allocated pointer.
    let mut f = Function::new([(1u32, ValType::I32)]);

    // ptr = __alloc(size) -> local 2
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::Call(alloc_func_index));
    f.instruction(&Instruction::LocalSet(2));

    // *(ptr + 0) = class_id
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    // return ptr
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::End);
    f
}

/// Build `__i32_to_str(value: i32) -> i32`: render `value` as decimal digits
/// in a fresh `__alloc` blob laid out like every other string
/// (`[len:i32][digits][nul]`), returning the offset just past the length
/// prefix. This is the runtime half of `str(int)` / `str(bool)`; callers
/// recover the length via `load(offset - 4)`, the standard convention.
/// Negative values are handled through the unsigned magnitude, so `i32::MIN`
/// (whose negation wraps) renders correctly.
fn build_i32_to_str_function(alloc_func_index: u32) -> Function {
    // Locals (local 0 is the `value` parameter): 1 = unsigned magnitude,
    // 2 = sign flag, 3 = total length (digits + sign), 4 = blob base,
    // 5 = write cursor, 6 = digit-count scratch.
    let mut f = Function::new([(6u32, ValType::I32)]);
    let byte_arg = |offset: u64| wasm_encoder::MemArg {
        offset,
        align: 0,
        memory_index: 0,
    };

    // neg = value < 0
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32LtS);
    f.instruction(&Instruction::LocalSet(2));

    // uval = neg ? 0 - value : value (u32 magnitude; wraps correctly for MIN)
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::Else);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalSet(1));

    // len = 1; tmp = uval; while tmp >= 10 { tmp /= 10; len += 1 }
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::LocalSet(6));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(6));
    f.instruction(&Instruction::I32Const(10));
    f.instruction(&Instruction::I32LtU);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(6));
    f.instruction(&Instruction::I32Const(10));
    f.instruction(&Instruction::I32DivU);
    f.instruction(&Instruction::LocalSet(6));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End); // loop
    f.instruction(&Instruction::End); // block

    // len += neg (room for the '-')
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));

    // block = __alloc(prefix + len + 1 for the NUL)
    f.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32 + 1));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::Call(alloc_func_index));
    f.instruction(&Instruction::LocalSet(4));

    // Length prefix at the block start.
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    // pos = block + prefix + len: the NUL goes there, digits fill backwards
    // from it.
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32Store8(byte_arg(0)));

    // do { pos -= 1; *pos = '0' + uval % 10; uval /= 10 } while uval != 0
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::LocalSet(5));
    f.instruction(&Instruction::LocalGet(5));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Const(10));
    f.instruction(&Instruction::I32RemU);
    f.instruction(&Instruction::I32Const('0' as i32));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Store8(byte_arg(0)));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Const(10));
    f.instruction(&Instruction::I32DivU);
    f.instruction(&Instruction::LocalTee(1));
    f.instruction(&Instruction::BrIf(0));
    f.instruction(&Instruction::End);

    // The sign, if any, lands in the first data byte.
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const('-' as i32));
    f.instruction(&Instruction::I32Store8(byte_arg(STRING_LEN_PREFIX as u64)));
    f.instruction(&Instruction::End);

    // Return the data offset (past the length prefix).
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::End);
    f
}

/// Does this expression (or any sub-expression) call `open()`?
fn expr_uses_open(expr: &IRExpr) -> bool {
    match expr {
        IRExpr::EnvRead { .. } | IRExpr::CellNew | IRExpr::CellLoad { .. } => false,
        IRExpr::CellStore { value, .. } | IRExpr::Keyword { value, .. } => expr_uses_open(value),
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => function_name == "open" || arguments.iter().any(expr_uses_open),
        IRExpr::MethodCall {
            object, arguments, ..
        } => expr_uses_open(object) || arguments.iter().any(expr_uses_open),
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => expr_uses_open(left) || expr_uses_open(right),
        IRExpr::UnaryOp { operand, .. } => expr_uses_open(operand),
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => expr_uses_open(condition) || expr_uses_open(body) || expr_uses_open(orelse),
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            items.iter().any(expr_uses_open)
        }
        IRExpr::DictLiteral(entries) => entries
            .iter()
            .any(|(k, v)| expr_uses_open(k) || expr_uses_open(v)),
        IRExpr::Indexing { container, index } => expr_uses_open(container) || expr_uses_open(index),
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            expr_uses_open(container)
                || [start, end, step]
                    .into_iter()
                    .flatten()
                    .any(|e| expr_uses_open(e))
        }
        IRExpr::Attribute { object, .. } => expr_uses_open(object),
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            expr_uses_open(element)
                || value.as_deref().is_some_and(expr_uses_open)
                || generators
                    .iter()
                    .any(|g| expr_uses_open(&g.iterable) || g.conditions.iter().any(expr_uses_open))
        }
        IRExpr::RangeCall { start, stop, step } => {
            expr_uses_open(stop)
                || [start, step]
                    .into_iter()
                    .flatten()
                    .any(|e| expr_uses_open(e))
        }
        IRExpr::Lambda { body, .. } => expr_uses_open(body),
        IRExpr::DynamicImportExpr { module_name } => expr_uses_open(module_name),
        IRExpr::Const(_) | IRExpr::Param(_) | IRExpr::Variable(_) | IRExpr::ClosureMake { .. } => {
            false
        }
    }
}

/// Does this body (recursively) call `open()` anywhere?
fn body_uses_open(body: &IRBody) -> bool {
    body.statements.iter().any(|stmt| match stmt {
        IRStatement::Return(expr) => expr.as_ref().is_some_and(expr_uses_open),
        IRStatement::Assign { value, .. } | IRStatement::AugAssign { value, .. } => {
            expr_uses_open(value)
        }
        IRStatement::Expression(expr) => expr_uses_open(expr),
        IRStatement::If {
            condition,
            then_body,
            else_body,
        } => {
            expr_uses_open(condition)
                || body_uses_open(then_body)
                || else_body.as_deref().is_some_and(body_uses_open)
        }
        IRStatement::While { condition, body } => expr_uses_open(condition) || body_uses_open(body),
        IRStatement::For {
            iterable,
            body,
            else_body,
            ..
        } => {
            expr_uses_open(iterable)
                || body_uses_open(body)
                || else_body.as_deref().is_some_and(body_uses_open)
        }
        IRStatement::With {
            context_expr, body, ..
        } => expr_uses_open(context_expr) || body_uses_open(body),
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            body_uses_open(try_body)
                || except_handlers.iter().any(|h| body_uses_open(&h.body))
                || finally_body.as_deref().is_some_and(body_uses_open)
        }
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            expr_uses_open(object) || expr_uses_open(value)
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => expr_uses_open(container) || expr_uses_open(index) || expr_uses_open(value),
        IRStatement::TupleUnpack { value, .. } => expr_uses_open(value),
        IRStatement::Raise { exception, cause } => {
            exception.as_ref().is_some_and(expr_uses_open)
                || cause.as_ref().is_some_and(expr_uses_open)
        }
        IRStatement::Yield { value } => value.as_ref().is_some_and(expr_uses_open),
        IRStatement::DynamicImport { module_name, .. } => expr_uses_open(module_name),
        IRStatement::ImportModule { .. }
        | IRStatement::Break
        | IRStatement::Continue
        | IRStatement::Global(_)
        | IRStatement::Nonlocal(_) => false,
        IRStatement::FunctionDef(def) => body_uses_open(&def.body),
    })
}

/// Does the module use file I/O anywhere (an `open()` call in any function,
/// method, or module-variable initializer)? Only then is the `waspy_host`
/// import section emitted, so programs without file I/O keep instantiating
/// with no imports at all.
pub(crate) fn module_uses_file_io(ir_module: &IRModule) -> bool {
    ir_module.functions.iter().any(|f| body_uses_open(&f.body))
        || ir_module
            .classes
            .iter()
            .any(|c| c.methods.iter().any(|m| body_uses_open(&m.body)))
        || ir_module.variables.iter().any(|v| expr_uses_open(&v.value))
}

/// Is `expr` a value that is the same every time it is evaluated and cannot
/// be mutated: a number, bool, string, bytes, or `None` constant, or
/// arithmetic over those and over other such module definitions? Those are
/// inlined at each read, as before. Anything else is evaluated once.
fn is_pure_constant(
    expr: &IRExpr,
    vars: &std::collections::HashMap<&str, &IRExpr>,
    depth: u32,
) -> bool {
    if depth > 16 {
        return false;
    }
    match expr {
        IRExpr::Const(c) => matches!(
            c,
            IRConstant::Int(_)
                | IRConstant::Float(_)
                | IRConstant::Bool(_)
                | IRConstant::String(_)
                | IRConstant::Bytes(_)
                | IRConstant::None
        ),
        IRExpr::UnaryOp { operand, .. } => is_pure_constant(operand, vars, depth + 1),
        IRExpr::BinaryOp { left, right, .. } => {
            is_pure_constant(left, vars, depth + 1) && is_pure_constant(right, vars, depth + 1)
        }
        IRExpr::Variable(name) => vars
            .get(name.as_str())
            .is_some_and(|init| is_pure_constant(init, vars, depth + 1)),
        _ => false,
    }
}

/// Compile an IR module into WebAssembly binary format.
///
/// A module-level definition that is not a plain constant (a collection, an
/// instance, a call, a comprehension) is evaluated once, in source order, by a
/// synthesized `__module_init` that the module's start function runs, and read
/// through a WASM global. Every definition used to be inlined at each read,
/// which made each read a new object: `G.append(2); len(G)` answered 1,
/// `G[0] = 9; G[0]` answered G's first element, and `C = Counter()` was a fresh
/// instance wherever it was mentioned. Arbitrary module-level statements stay
/// refused; this covers definitions, which is what Python runs at import.
pub fn compile_ir_module(ir_module: &IRModule) -> Result<Vec<u8>, ChakraError> {
    // A module that can raise carries the builtin exception classes, ahead of
    // its own, so every exception is an object of one.
    let with_exceptions;
    let ir_module = if crate::compiler::exceptions::module_needs_exceptions(
        ir_module,
        !functions_that_can_raise(ir_module).is_empty() || module_handles_exceptions(ir_module),
    ) {
        with_exceptions = crate::compiler::exceptions::with_builtin_exceptions(ir_module);
        &with_exceptions
    } else {
        ir_module
    };
    // A definition some function declares `global` is a global even when it
    // is a plain constant, since that function rebinds it; so is anything
    // computed from one, which reads its import-time value.
    let declared: std::collections::HashSet<String> = ir_module
        .functions
        .iter()
        .chain(ir_module.classes.iter().flat_map(|c| c.methods.iter()))
        .flat_map(|f| crate::compiler::function::global_declarations(&f.body))
        // A metaclass's class attributes are written through the class.
        .chain(ir_module.classes.iter().flat_map(|c| {
            c.deferred_vars
                .iter()
                .map(move |v| format!("{}.{v}", c.name))
        }))
        .collect();
    let vars: std::collections::HashMap<&str, &IRExpr> = ir_module
        .variables
        .iter()
        .filter(|v| !declared.contains(&v.name))
        .map(|v| (v.name.as_str(), &v.value))
        .collect();
    // Class variables the same way, under `Class.var`: `class C: items = []`
    // then `C.items.append(1)` read back an empty list, since every mention of
    // `C.items` rebuilt it. They come first, because a module definition such
    // as `C = Counter()` runs class code that may read them.
    let written = written_class_vars(ir_module);
    let mut globals: Vec<crate::ir::IRVariable> = Vec::new();
    for cls in &ir_module.classes {
        for var in &cls.class_vars {
            let key = format!("{}.{}", cls.name, var.name);
            if written.contains(&key) || !is_pure_constant(&var.value, &vars, 0) {
                globals.push(crate::ir::IRVariable {
                    name: format!("{}.{}", cls.name, var.name),
                    value: var.value.clone(),
                    var_type: var.var_type.clone(),
                });
            }
        }
    }
    globals.extend(
        ir_module
            .variables
            .iter()
            .filter(|v| declared.contains(&v.name) || !is_pure_constant(&v.value, &vars, 0))
            .cloned(),
    );
    if globals.is_empty() {
        return compile_module(ir_module, &[]);
    }
    let names: Vec<String> = globals.iter().map(|v| v.name.clone()).collect();
    let mut with_init = ir_module.clone();
    with_init.functions.push(crate::ir::IRFunction {
        name: MODULE_INIT_FN.to_string(),
        params: Vec::new(),
        body: crate::ir::IRBody {
            statements: globals
                .iter()
                .map(|v| IRStatement::Assign {
                    target: v.name.clone(),
                    value: v.value.clone(),
                    var_type: v.var_type.clone(),
                })
                .collect(),
        },
        return_type: IRType::None,
        decorators: Vec::new(),
    });
    compile_module(&with_init, &names)
}

fn compile_module(ir_module: &IRModule, module_globals: &[String]) -> Result<Vec<u8>, ChakraError> {
    // Unannotated closure parameters are typed from their calls, and captured
    // cells from their stores, before any signature is fixed.
    let (typed, capture_types) = crate::compiler::closures::type_closures(ir_module);
    let ir_module = &typed;
    let mut module = Module::new();
    let mut ctx = CompilationContext::new();
    ctx.capture_types = capture_types.clone();
    for (i, name) in module_globals.iter().enumerate() {
        ctx.module_global_index
            .insert(name.clone(), FIRST_MODULE_GLOBAL + i as u32);
    }
    // String/bytes offsets are resolved during lowering and carried on the IR
    // module; the compiler reuses that layout to emit loads and the data section.
    let memory_layout = ir_module.memory_layout.clone();

    // Register module-level variables so functions can inline their values.
    for var in &ir_module.variables {
        ctx.add_module_var(&var.name, var.var_type.clone(), var.value.clone());
    }

    // Imported user-written modules are statically linked into this single
    // module by the multi-file merge, so record their bindings for call-site
    // resolution: `import mod [as m]` binds a module-namespace name (making
    // `mod.f(...)` resolve to the merged `f`), and `from mod import f as g`
    // aliases `g` to the merged `f`.
    for imp in &ir_module.imports {
        if crate::stdlib::is_stdlib_module(&imp.module) {
            if let (true, Some(name)) = (imp.is_from_import, &imp.name) {
                let binding = imp.alias.clone().unwrap_or_else(|| name.clone());
                ctx.stdlib_imports
                    .insert(binding, (imp.module.clone(), name.clone()));
            }
            continue;
        }
        if imp.is_dynamic {
            continue;
        }
        if imp.is_from_import {
            if let (Some(name), Some(alias)) = (&imp.name, &imp.alias) {
                if name != "*" {
                    ctx.import_aliases.insert(alias.clone(), name.clone());
                }
            }
        } else {
            let binding = imp.alias.clone().unwrap_or_else(|| imp.module.clone());
            ctx.user_modules.insert(binding, imp.module.clone());
        }
    }

    for (key, module) in &ir_module.metadata {
        if let Some(class) = key.strip_prefix(crate::ir::CLASS_MODULE_KEY) {
            ctx.class_modules.insert(class.to_string(), module.clone());
        }
    }
    for table in &ir_module.dispatch_tables {
        ctx.dispatch_tables
            .insert(table.base.clone(), table.arms.clone());
    }

    // File I/O (#25): when the program calls `open()` anywhere, the module
    // imports the four `waspy_host` functions. Imported functions occupy the
    // lowest indices of the function index space, so every defined function's
    // index shifts up by the import count.
    // File I/O is WASI (helpers over its imports), or with the deprecated
    // option the four `waspy_host` imports.
    let uses_file_io = module_uses_file_io(ir_module);
    let legacy_file_io = uses_file_io && crate::compiler::host::legacy_file_io(ir_module);
    let file_io_count: u32 = if legacy_file_io { 4 } else { 0 };
    // The WASI imports after them: whatever the program prints, reads, or
    // asks of the host, and what the `__main__` block needs to report an
    // exception it does not catch.
    let main_can_raise = ir_module.functions.iter().any(|f| f.name == "main")
        && ir_module
            .metadata
            .get("has_entry_point")
            .map(String::as_str)
            == Some("true")
        && functions_that_can_raise(ir_module).contains("main");
    let wasi_imports = crate::compiler::host::wasi_needed(ir_module, main_can_raise);
    for (k, import) in wasi_imports.iter().enumerate() {
        ctx.wasi.insert(*import, file_io_count + k as u32);
    }
    let import_count: u32 = file_io_count + wasi_imports.len() as u32;
    if legacy_file_io {
        ctx.file_io = Some(FileIoImports {
            open: 0,
            read: 1,
            write: 2,
            close: 3,
        });
    }

    // Resolve each function's (and method's) return type up front, inferring
    // unannotated ones from their bodies, so the type section, the registered
    // signatures, and the emitted code all use the same result type. Two passes
    // let a function's inferred return type depend on a callee resolved later in
    // the first pass.
    let mut resolved_returns: HashMap<String, IRType> = HashMap::new();
    let function_params: HashMap<String, Vec<IRType>> = ir_module
        .functions
        .iter()
        .map(|f| {
            let params = f.params.iter().map(|p| p.param_type.clone()).collect();
            (f.name.clone(), params)
        })
        .collect();
    // A third pass lets a function returning a closure see the closure's
    // result type, which is resolved in the pass before.
    for _ in 0..3 {
        for func in &ir_module.functions {
            let rt = resolve_return_type(func, &resolved_returns, &function_params, &capture_types);
            resolved_returns.insert(func.name.clone(), rt);
        }
        for cls in &ir_module.classes {
            for method in &cls.methods {
                let rt = resolve_return_type(
                    method,
                    &resolved_returns,
                    &function_params,
                    &capture_types,
                );
                resolved_returns.insert(method_registry_key(&cls.name, method), rt);
            }
        }
    }
    let module_return = |func: &crate::ir::IRFunction| -> IRType {
        resolved_returns
            .get(&func.name)
            .cloned()
            .unwrap_or_else(|| func.return_type.clone())
    };
    let method_return = |class: &str, method: &crate::ir::IRFunction| -> IRType {
        resolved_returns
            .get(&method_registry_key(class, method))
            .cloned()
            .unwrap_or_else(|| method.return_type.clone())
    };

    // Build type section
    let mut types = TypeSection::new();

    // Calculate total number of functions (module functions + all class methods)
    let mut total_function_count = ir_module.functions.len();
    for cls in &ir_module.classes {
        total_function_count += cls.methods.len();
    }

    // The runtime helpers — allocator `__alloc`, instance allocator
    // `__alloc_obj`, and decimal renderer `__i32_to_str` — are appended after
    // all user functions and methods, so they take the next three function
    // indices (offset by any host imports, which occupy the lowest indices).
    // Record them so codegen can emit calls to each.
    ctx.alloc_func_index = import_count + total_function_count as u32;
    ctx.alloc_obj_func_index = import_count + total_function_count as u32 + 1;
    ctx.i32_to_str_func_index = import_count + total_function_count as u32 + 2;

    // Create function types for module functions
    for func in &ir_module.functions {
        // Map IR parameter types to WebAssembly types
        let params: Vec<ValType> = func
            .params
            .iter()
            .map(|param| ir_type_to_wasm_type(&param.param_type))
            .collect();

        // Map IR return type to WebAssembly type
        let results = vec![ir_type_to_wasm_type(&module_return(func))];

        // Add the type to the type section
        types.ty().function(params, results);
    }

    // The signature of every module function, whose type index is its own
    // position (defined function `i` has type `i`). A call through a typed
    // closure value looks its signature up here; closures are always module
    // functions.
    for (i, func) in ir_module.functions.iter().enumerate() {
        let params = func
            .params
            .iter()
            .map(|param| ir_type_to_wasm_type(&param.param_type))
            .collect();
        let result = ir_type_to_wasm_type(&module_return(func));
        ctx.signature_types
            .entry((params, result))
            .or_insert(i as u32);
    }

    // Create function types for class methods
    for cls in &ir_module.classes {
        for method in &cls.methods {
            let params: Vec<ValType> = method
                .params
                .iter()
                .map(|param| ir_type_to_wasm_type(&param.param_type))
                .collect();
            let results = vec![ir_type_to_wasm_type(&method_return(&cls.name, method))];
            types.ty().function(params, results);
        }
    }

    // Types for the runtime helpers: `__alloc(size: i32) -> i32`,
    // `__alloc_obj(size: i32, class_id: i32) -> i32`, and
    // `__i32_to_str(value: i32) -> i32`.
    types.ty().function([ValType::I32], [ValType::I32]);
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]);
    types.ty().function([ValType::I32], [ValType::I32]);

    // Lifted lambdas (closures, #43): each `__lambda_{n}` function gets a slot
    // in a funcref table so closure values can dispatch via `call_indirect`.
    // The call signature for a closure of user-arity `a` is `(a i32 args +
    // env ptr) -> i32`; one such type per arity 0..=max is appended after the
    // helper types, at `closure_type_base + a`.
    let lambda_functions: Vec<(u32, &crate::ir::IRFunction)> = ir_module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name.starts_with("__lambda_"))
        .map(|(i, f)| (i as u32, f))
        .collect();
    for (slot, (_, f)) in lambda_functions.iter().enumerate() {
        ctx.lambda_slots.insert(f.name.clone(), slot as u32);
    }
    if !lambda_functions.is_empty() {
        let max_arity = lambda_functions
            .iter()
            .map(|(_, f)| f.params.len().saturating_sub(1))
            .max()
            .unwrap_or(0);
        ctx.closure_type_base = Some(total_function_count as u32 + 3);
        ctx.closure_max_arity = max_arity as u32;
        for arity in 0..=max_arity {
            let params = vec![ValType::I32; arity + 1];
            types.ty().function(params, [ValType::I32]);
        }
    }

    // Types for the `waspy_host` file-I/O imports, appended after every other
    // type so the existing "defined function i has type i" mapping is
    // untouched. `open`/`read`/`write` share the 3-arg shape; `close` takes
    // one argument.
    let mut host_imports = ImportSection::new();
    if legacy_file_io {
        let closure_type_count = ctx
            .closure_type_base
            .map(|_| ctx.closure_max_arity + 1)
            .unwrap_or(0);
        let host3_type = total_function_count as u32 + 3 + closure_type_count;
        let host1_type = host3_type + 1;
        types
            .ty()
            .function([ValType::I32, ValType::I32, ValType::I32], [ValType::I32]);
        types.ty().function([ValType::I32], [ValType::I32]);

        // Import order fixes the function indices recorded in `ctx.file_io`:
        // open=0, read=1, write=2, close=3.
        host_imports.import("waspy_host", "open", EntityType::Function(host3_type));
        host_imports.import("waspy_host", "read", EntityType::Function(host3_type));
        host_imports.import("waspy_host", "write", EntityType::Function(host3_type));
        host_imports.import("waspy_host", "close", EntityType::Function(host1_type));
    }
    for import in &wasi_imports {
        let (params, results) = import.signature();
        let type_index = types.len();
        types.ty().function(params, results);
        host_imports.import(
            crate::compiler::host::Wasi::MODULE,
            import.name(),
            EntityType::Function(type_index),
        );
    }

    // The start function's `() -> ()` type, after every other type so the
    // existing "defined function i has type i" mapping is untouched.
    let start_type_index = if module_globals.is_empty() {
        None
    } else {
        let index = types.len();
        types.ty().function([], []);
        Some(index)
    };

    // Build function section (one entry per user function/method, referencing
    // the matching type, plus trailing entries for `__alloc` / `__alloc_obj`).
    let mut functions = FunctionSection::new();
    for _ in 0..total_function_count {
        functions.function(functions.len());
    }
    functions.function(total_function_count as u32); // __alloc
    functions.function(total_function_count as u32 + 1); // __alloc_obj
    functions.function(total_function_count as u32 + 2); // __i32_to_str
    if let Some(start_type) = start_type_index {
        functions.function(start_type); // the start wrapper
    }
    // The runtime helpers code generation asks for come after all of these.
    ctx.helper_base =
        import_count + total_function_count as u32 + 3 + u32::from(start_type_index.is_some());
    if uses_file_io && !legacy_file_io {
        use crate::compiler::runtime::Helper;
        ctx.file_io = Some(FileIoImports {
            open: ctx.helper(Helper::FileOpen),
            read: ctx.helper(Helper::FileRead),
            write: ctx.helper(Helper::FileWrite),
            close: ctx.helper(Helper::FileClose),
        });
    }

    // Export section - export both functions and memory
    let mut exports = ExportSection::new();

    // Register module functions. Defined functions start after any host
    // imports in the function index space.
    let mut func_idx = import_count;
    for func in &ir_module.functions {
        // Register function in context
        let param_types = func.params.iter().map(|p| p.param_type.clone()).collect();

        ctx.add_function(&func.name, func_idx, param_types, module_return(func));

        // Export the function. Lifted lambdas are internal — they are only
        // reachable through the funcref table.
        if !func.name.starts_with("__lambda_")
            && !crate::ir::is_internal_function(&func.name)
            && func.name != MODULE_INIT_FN
        {
            exports.export(&func.name, wasm_encoder::ExportKind::Func, func_idx);
        }
        func_idx += 1;
    }

    // Register and export class methods. Classes appear in source order and
    // Python requires a base to be defined before a subclass references it, so
    // a base's ClassInfo is always registered by the time its subclass is
    // processed.
    // Every class name in the module, so a field initialized or filled with
    // instances records the element's class instead of a bare pointer.
    let class_names: HashSet<String> = ir_module.classes.iter().map(|c| c.name.clone()).collect();
    // Each class's base, for widening a field assigned instances of different
    // classes before every class is registered.
    let class_bases: HashMap<String, String> = ir_module
        .classes
        .iter()
        .filter_map(|c| {
            let base = c.bases.iter().find(|b| class_names.contains(b.as_str()))?;
            Some((c.name.clone(), base.clone()))
        })
        .collect();

    // The classes connected by inheritance where some class has several
    // bases. Their fields cannot all be laid out as a prefix of each base, so
    // in such a group each field name has one slot every class shares, which
    // is the one attribute per name Python's instances have anyway.
    let mut group_of: HashMap<String, String> = HashMap::new();
    fn root_of(group_of: &HashMap<String, String>, name: &str) -> String {
        let mut current = name.to_string();
        while let Some(parent) = group_of.get(&current) {
            if *parent == current {
                break;
            }
            current = parent.clone();
        }
        current
    }
    let mut multiple_groups: HashSet<String> = HashSet::new();
    for cls in &ir_module.classes {
        group_of
            .entry(cls.name.clone())
            .or_insert_with(|| cls.name.clone());
        let known: Vec<&String> = cls
            .bases
            .iter()
            .filter(|b| class_names.contains(*b))
            .collect();
        for b in &known {
            let (ra, rb) = (root_of(&group_of, &cls.name), root_of(&group_of, b));
            if ra != rb {
                group_of.insert(ra, rb);
            }
        }
        if known.len() > 1 {
            multiple_groups.insert(cls.name.clone());
        }
    }
    let multiple_groups: HashSet<String> = multiple_groups
        .iter()
        .map(|c| root_of(&group_of, c))
        .collect();
    // Per such group, each field name's offset and the next free one.
    let mut group_slots: HashMap<String, (HashMap<String, u64>, u64)> = HashMap::new();

    for (class_id, cls) in (1i32..).zip(ir_module.classes.iter()) {
        // The bases, then the method resolution order (C3, as CPython's).
        // `object` is the implicit root, not a base.
        let bases: Vec<String> = cls
            .bases
            .iter()
            .filter(|b| b.as_str() != "object" && ctx.get_class_info(b).is_some())
            .cloned()
            .collect();
        let base = bases.first().cloned();
        let linearized: Vec<(String, Vec<String>)> = bases
            .iter()
            .map(|b| {
                (
                    b.clone(),
                    ctx.get_class_info(b)
                        .map(|i| i.mro.clone())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let mro = match crate::compiler::context::c3_linearization(&cls.name, &linearized) {
            Some(mro) => mro,
            None => {
                ctx.report(format!(
                    "class {} raises TypeError: Cannot create a consistent method resolution \
                     order (MRO) for bases {}",
                    cls.name,
                    bases.join(", ")
                ));
                vec![cls.name.clone()]
            }
        };
        let group = root_of(&group_of, &cls.name);
        let in_multiple = multiple_groups.contains(&group);

        let mut class_info = ClassInfo {
            name: cls.name.clone(),
            base: base.clone(),
            class_id,
            mro: mro.clone(),
            methods: HashMap::new(),
            method_owner: HashMap::new(),
            method_kinds: HashMap::new(),
            property_setters: HashMap::new(),
            field_offsets: HashMap::new(),
            field_types: HashMap::new(),
            class_var_values: HashMap::new(),
            class_var_owner: HashMap::new(),
            instance_size: 0,
        };

        // Each field is an 8-byte slot (holds an i32 or an f64); the first 4
        // bytes hold the class-id tag stamped by `__alloc_obj`. `add_field`
        // assigns the next slot the first time a field name is seen and
        // records its value type.
        //
        // A subclass inherits its base's layout as a prefix: base fields keep
        // their exact offsets and the subclass's own fields append after
        // `base.instance_size`, so a base method reading `self.x` works
        // unchanged on a subclass instance. Inherited methods are seeded with
        // the base's already-resolved function indices (the same compiled WASM
        // function — safe because of the prefix layout); the subclass's own
        // registration below overwrites any it redefines (override).
        let mut current_offset = 4u64;
        if let Some(base_info) = base.as_deref().and_then(|b| ctx.get_class_info(b)) {
            class_info.field_offsets = base_info.field_offsets.clone();
            class_info.field_types = base_info.field_types.clone();
            current_offset = base_info.instance_size as u64;
        }
        // Methods, class variables, and properties come from the classes of
        // the MRO, a nearer one's own definition winning: for one base that
        // is the base's table, for several CPython's order.
        for ancestor in mro.iter().skip(1).rev() {
            let Some(info) = ctx.get_class_info(ancestor) else {
                continue;
            };
            for (name, index) in &info.methods {
                if info.method_owner.get(name) == Some(ancestor) {
                    class_info.methods.insert(name.clone(), *index);
                    class_info
                        .method_owner
                        .insert(name.clone(), ancestor.clone());
                    if let Some(kind) = info.method_kinds.get(name) {
                        class_info.method_kinds.insert(name.clone(), *kind);
                    }
                }
            }
            for (name, setter) in &info.property_setters {
                if setter.1 == *ancestor {
                    class_info
                        .property_setters
                        .insert(name.clone(), setter.clone());
                }
            }
            for (name, value) in &info.class_var_values {
                if info.class_var_owner.get(name) == Some(ancestor) {
                    class_info
                        .class_var_values
                        .insert(name.clone(), value.clone());
                    class_info
                        .class_var_owner
                        .insert(name.clone(), ancestor.clone());
                }
            }
        }
        // In a group with multiple inheritance, every base's fields, at the
        // group's slots.
        if in_multiple {
            for b in &bases {
                let Some(info) = ctx.get_class_info(b) else {
                    continue;
                };
                for (name, offset) in &info.field_offsets {
                    class_info.field_offsets.insert(name.clone(), *offset);
                }
                for (name, ty) in &info.field_types {
                    match class_info.field_types.get(name) {
                        Some(seen)
                            if seen != ty
                                && !matches!(seen, IRType::Unknown)
                                && !matches!(ty, IRType::Unknown) =>
                        {
                            ctx.report(format!(
                                "class {}: its bases give the field '{name}' two types ({} and \
                                 {}), which one instance cannot hold",
                                cls.name,
                                crate::type_to_string(seen),
                                crate::type_to_string(ty)
                            ));
                        }
                        Some(seen) if !matches!(seen, IRType::Unknown) => {}
                        _ => {
                            class_info.field_types.insert(name.clone(), ty.clone());
                        }
                    }
                }
            }
        }
        let slots = in_multiple.then(|| {
            group_slots
                .entry(group.clone())
                .or_insert((HashMap::new(), 4))
        });
        let slots = std::cell::RefCell::new(slots);
        let add_field = |info: &mut ClassInfo, name: &str, ty: IRType, current_offset: &mut u64| {
            if !info.field_offsets.contains_key(name) {
                let mut slots = slots.borrow_mut();
                match slots.as_mut() {
                    Some((by_name, next)) => {
                        let offset = *by_name.entry(name.to_string()).or_insert_with(|| {
                            let offset = *next;
                            *next += 8;
                            offset
                        });
                        info.field_offsets.insert(name.to_string(), offset);
                    }
                    None => {
                        info.field_offsets.insert(name.to_string(), *current_offset);
                        *current_offset += 8;
                    }
                }
            }
            // Prefer a concrete type over an earlier `Unknown` inference.
            let entry = info
                .field_types
                .entry(name.to_string())
                .or_insert(IRType::Unknown);
            match (&*entry, &ty) {
                (IRType::Unknown, _) => *entry = ty,
                // Instances of two classes: the field holds either, so it takes
                // their nearest common base (or no type, without one) rather
                // than the first, whose methods would run on the other (#128).
                (IRType::Class(old), IRType::Class(new)) if old != new => {
                    *entry = common_class_base(old, new, |c| class_bases.get(c).cloned())
                        .map_or(IRType::Unknown, IRType::Class);
                }
                _ => {}
            }
        };

        // `SystemExit` keeps the status it exits with, and whether its
        // argument is printed.
        if cls.name == "SystemExit" {
            for name in ["__exit_code", "__exit_print"] {
                add_field(&mut class_info, name, IRType::Int, &mut current_offset);
            }
        }
        // `BaseException` holds what every exception keeps of its arguments.
        if cls.name == "BaseException" {
            for name in crate::compiler::exceptions::EXC_FIELDS {
                add_field(
                    &mut class_info,
                    name,
                    crate::compiler::exceptions::exc_field_type(name),
                    &mut current_offset,
                );
            }
        }

        // Class-level variables occupy instance slots and are also accessible as
        // `ClassName.var`; keep their initializers for that read path.
        // A class variable is read through an instance from the class
        // (`self.label`), unless some method gives the instance an attribute
        // of that name, which then needs a field.
        let mut self_assigned = Vec::new();
        for method in &cls.methods {
            collect_self_fields(
                &method.body,
                &mut HashMap::new(),
                &class_names,
                &mut self_assigned,
            );
        }
        for var in &cls.class_vars {
            if self_assigned.iter().any(|(name, _)| *name == var.name) {
                let ty = var.var_type.clone().unwrap_or_else(|| {
                    infer_field_value_type(&var.value, &HashMap::new(), &class_names)
                });
                add_field(&mut class_info, &var.name, ty, &mut current_offset);
            }
            class_info
                .class_var_values
                .insert(var.name.clone(), var.value.clone());
            class_info
                .class_var_owner
                .insert(var.name.clone(), cls.name.clone());
        }
        // The attributes its metaclass gives it, whose globals the module
        // definitions `Class.attr` hold.
        for name in &cls.deferred_vars {
            class_info.class_var_values.remove(name);
            class_info
                .class_var_owner
                .insert(name.clone(), cls.name.clone());
        }

        // Instance fields are discovered from `self.<field> = ...` assignments in
        // method bodies (their types inferred from the method's parameters).
        // A name managed by a property (own or inherited) never becomes a
        // field: `self.attr = v` routes through the setter, whose body assigns
        // the real backing field (conventionally `self._attr`).
        let property_names: HashSet<String> = cls
            .methods
            .iter()
            .filter(|m| {
                matches!(
                    kind_of(&cls.name, m),
                    MethodKind::PropertyGetter | MethodKind::PropertySetter
                )
            })
            .map(|m| m.name.clone())
            .chain(
                class_info
                    .method_kinds
                    .iter()
                    .filter(|(_, k)| **k == MethodKind::PropertyGetter)
                    .map(|(n, _)| n.clone()),
            )
            .collect();
        let mut field_elements: Vec<(String, IRType)> = Vec::new();
        // An annotated field (`self.items: Dict[str, Item] = {}`) takes its
        // annotation, whichever method assigns it first: `add_field` keeps the
        // first concrete type it sees, and `{}` is already one.
        for method in &cls.methods {
            let mut annotated = Vec::new();
            collect_annotated_self_fields(&method.body, &mut annotated);
            for (name, ty) in annotated {
                if !property_names.contains(&name) {
                    add_field(&mut class_info, &name, ty, &mut current_offset);
                }
            }
        }
        let laid_out: Vec<(String, IRType)> = ctx
            .class_map
            .values()
            .flat_map(|info| {
                info.field_types
                    .iter()
                    .map(move |(f, t)| (format!("{FIELD_OF}{}.{f}", info.name), t.clone()))
            })
            .collect();
        for method in &cls.methods {
            let mut params: HashMap<String, IRType> = laid_out.iter().cloned().collect();
            // The fields earlier methods (`__init__` first) gave this class.
            params.extend(
                class_info
                    .field_types
                    .iter()
                    .filter(|(_, t)| !matches!(t, IRType::Unknown))
                    .map(|(f, t)| (format!("self.{f}"), t.clone())),
            );
            params.extend(
                method
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), p.param_type.clone())),
            );
            let mut fields = Vec::new();
            collect_self_fields(&method.body, &mut params.clone(), &class_names, &mut fields);
            for (name, ty) in fields {
                if property_names.contains(&name) {
                    continue;
                }
                add_field(&mut class_info, &name, ty, &mut current_offset);
            }
            collect_self_field_elements(
                &method.body,
                &mut params.clone(),
                &class_names,
                &mut field_elements,
            );
        }

        // Fill in the element type of a collection field that was declared
        // element-less (`self.items = []`) from what the class appends to it.
        // Only an unresolved element type is filled: an initializer that
        // already said what it holds wins.
        for (name, element) in field_elements {
            if matches!(element, IRType::Unknown) {
                continue;
            }
            match class_info.field_types.get_mut(&name) {
                Some(IRType::List(elem)) | Some(IRType::Set(elem)) if **elem == IRType::Unknown => {
                    **elem = element;
                }
                _ => {}
            }
        }

        class_info.instance_size = match slots.borrow().as_ref() {
            // Every slot the group has handed out so far, which covers every
            // field this class has.
            Some((_, next)) => *next as u32,
            None => current_offset as u32,
        };

        // Register methods. A property setter is registered under its own
        // `Class::name::setter` key (its getter owns the plain name in the
        // method table); every other kind lands in `methods`/`method_owner`
        // with its kind recorded for call-site dispatch.
        for method in &cls.methods {
            let param_types = method.params.iter().map(|p| p.param_type.clone()).collect();
            let qualified_name = method_registry_key(&cls.name, method);

            ctx.add_function(
                &qualified_name,
                func_idx,
                param_types,
                method_return(&cls.name, method),
            );

            match kind_of(&cls.name, method) {
                MethodKind::PropertySetter => {
                    class_info
                        .property_setters
                        .insert(method.name.clone(), (func_idx, cls.name.clone()));
                }
                kind => {
                    class_info.methods.insert(method.name.clone(), func_idx);
                    class_info
                        .method_owner
                        .insert(method.name.clone(), cls.name.clone());
                    class_info.method_kinds.insert(method.name.clone(), kind);
                }
            }

            // Export method with qualified name; a builtin exception's are
            // the runtime's own.
            if !crate::compiler::exceptions::is_builtin_exception(&cls.name) {
                exports.export(&qualified_name, wasm_encoder::ExportKind::Func, func_idx);
            }
            func_idx += 1;
        }

        // Add class to context
        ctx.add_class(class_info);
    }

    // Virtual method dispatch. Dispatch used to be static everywhere: a call
    // was compiled to the implementation the receiver's *declared* class holds,
    // so `self.kind()` inside a base method reached the base's version even
    // when the instance was a subclass that overrode it, and the plain
    // template-method pattern answered with the wrong implementation while
    // reporting success.
    //
    // A method name is given a vtable column when some class overrides an
    // ancestor's definition of it. Every other call keeps its direct `call`, so
    // a program with no overrides is byte for byte what it was.
    //
    // The rows are indexed by the class id already stamped into every instance
    // at offset 0, which is what makes the lookup a load and two arithmetic
    // ops: row `class_id - 1` (ids start at 1), column per method name.
    let mut virtual_names: Vec<String> = Vec::new();
    for cls in &ir_module.classes {
        let Some(info) = ctx.get_class_info(&cls.name) else {
            continue;
        };
        for (name, owner) in &info.method_owner {
            // `__init__` is only ever reached from an instantiation site, whose
            // class is known exactly, and `super().__init__()`, which Python
            // defines as non-virtual. Property accessors and the static and
            // class method kinds do not dispatch on an instance at all.
            if name == "__init__" || virtual_names.contains(name) {
                continue;
            }
            let dispatches_on_instance = info
                .method_kinds
                .get(name)
                .is_some_and(|k| matches!(k, MethodKind::Instance | MethodKind::PropertyGetter));
            if !dispatches_on_instance {
                continue;
            }
            // Virtual when another class of this one's MRO defines the name
            // too: an ancestor this class overrides, or (with several bases)
            // a sibling whose own methods call it on an instance of this one.
            let other_definer = info.mro.iter().any(|ancestor| {
                ancestor != owner
                    && ctx
                        .get_class_info(ancestor)
                        .is_some_and(|a| a.method_owner.get(name) == Some(ancestor))
            });
            if other_definer {
                virtual_names.push(name.clone());
            }
        }
    }
    virtual_names.sort();

    // Every implementation of a virtual name is reached through one
    // `call_indirect`, which names a single type: they must agree on their
    // WebAssembly signature. Python does not require that, so a disagreement is
    // reported rather than left to trap on the call at runtime.
    let signature_of = |owner: &str, name: &str| -> Option<(Vec<ValType>, ValType)> {
        let info = ctx.get_function_info(&format!("{owner}::{name}"))?;
        Some((
            info.param_types.iter().map(ir_type_to_wasm_type).collect(),
            ir_type_to_wasm_type(&info.return_type),
        ))
    };
    let mut virtual_slots: Vec<(String, u32)> = Vec::new();
    for name in &virtual_names {
        // The root of the hierarchy that introduced the name fixes the
        // signature, and its function index doubles as the type index: the type
        // section emits one type per defined function, in function order.
        let mut owners: Vec<(String, u32)> = Vec::new();
        for cls in &ir_module.classes {
            let Some(ci) = ctx.get_class_info(&cls.name) else {
                continue;
            };
            if ci.method_owner.get(name) == Some(&cls.name) {
                if let Some(idx) = ci.methods.get(name) {
                    owners.push((cls.name.clone(), *idx));
                }
            }
        }
        let Some((root_owner, root_idx)) = owners.first().cloned() else {
            continue;
        };
        let Some(root_sig) = signature_of(&root_owner, name) else {
            continue;
        };
        let mut agreed = true;
        for (owner, _) in owners.iter().skip(1) {
            if signature_of(owner, name).as_ref() != Some(&root_sig) {
                ctx.report(format!(
                    "'{owner}.{name}' overrides '{root_owner}.{name}' with a different \
                     signature, so a call through a '{root_owner}' cannot reach both. \
                     Hint: give the override the same parameter and return types"
                ));
                agreed = false;
            }
        }
        if agreed {
            virtual_slots.push((name.clone(), root_idx - import_count));
        }
    }

    let vtable_stride = virtual_slots.len() as u32;
    let max_class_id = ir_module
        .classes
        .iter()
        .filter_map(|c| ctx.get_class_info(&c.name).map(|ci| ci.class_id))
        .max()
        .unwrap_or(0);
    let mut vtable_entries: Vec<u32> = Vec::new();
    // The rows sit after the closure slots in the shared table, and every call
    // site bakes this base in, so it has to be known before any body is
    // compiled rather than when the table is finally written out.
    ctx.vtable_base = lambda_functions.len() as u32;
    if vtable_stride > 0 {
        for (column, (name, type_index)) in virtual_slots.iter().enumerate() {
            ctx.virtual_slots
                .insert(name.clone(), (column as u32, *type_index));
        }
        // Row per class id, column per virtual name, holding that class's
        // resolved implementation. `ClassInfo::methods` already carries every
        // inherited method, so a class that does not override the name lands on
        // the one it inherited. A class outside the hierarchy has no
        // implementation to point at; its cell takes the root's, which no call
        // can reach (the receiver's static type decides that) and which keeps
        // the segment free of null entries.
        for class_id in 1..=max_class_id {
            let resolved: Vec<u32> = virtual_slots
                .iter()
                .map(|(name, type_index)| {
                    ctx.class_map
                        .values()
                        .find(|ci| ci.class_id == class_id)
                        .and_then(|ci| ci.methods.get(name).copied())
                        .unwrap_or(*type_index + import_count)
                })
                .collect();
            vtable_entries.extend(resolved);
        }
        ctx.vtable_stride = vtable_stride;
        ctx.vtable_entries = vtable_entries.clone();
    }

    // A program with a `__main__` block is a WASI command: `_start` runs it.
    if ir_module
        .metadata
        .get("has_entry_point")
        .map(String::as_str)
        == Some("true")
        && ctx.get_function_info("main").is_some()
    {
        let start = ctx.helper(crate::compiler::runtime::Helper::Start);
        exports.export("_start", wasm_encoder::ExportKind::Func, start);
    }

    // Export memory
    exports.export("memory", wasm_encoder::ExportKind::Memory, 0);

    // Data section for string and bytes constants.
    let mut data = DataSection::new();

    // String data lives from offset 0. Each blob is `[len:i32][bytes][nul]`;
    // the recorded offset points at the bytes (past the length prefix), so
    // emitting the strings in offset order — each prefixed by its length and
    // followed by a NUL — reproduces those exact offsets. The prefix lets a
    // string's length be recovered as `load(offset - 4)` when only its offset
    // word survives (e.g. read back out of a collection slot).
    if !memory_layout.string_offsets.is_empty() {
        let mut offsets: Vec<(&String, u32)> = memory_layout
            .string_offsets
            .iter()
            .map(|(s, &offset)| (s, offset))
            .collect();
        offsets.sort_by_key(|(_s, offset)| *offset);

        let mut all_strings = Vec::new();
        for (s, _) in offsets {
            all_strings.extend_from_slice(&(s.len() as u32).to_le_bytes()); // length prefix
            all_strings.extend_from_slice(s.as_bytes());
            all_strings.push(0); // Null terminator
        }
        data.active(0, &ConstExpr::i32_const(0), all_strings);
    }

    // Bytes data lives from `next_bytes_offset`'s base (32768). Each blob is
    // `[len:i32][bytes]` (no terminator); the recorded offset points at the
    // bytes, so the data segment starts one prefix before the lowest offset.
    if !memory_layout.bytes_offsets.is_empty() {
        let mut offsets: Vec<(&Vec<u8>, u32)> = memory_layout
            .bytes_offsets
            .iter()
            .map(|(b, &offset)| (b, offset))
            .collect();
        offsets.sort_by_key(|(_b, offset)| *offset);

        let base = offsets[0].1 - STRING_LEN_PREFIX;
        let mut all_bytes = Vec::new();
        for (b, _) in offsets {
            all_bytes.extend_from_slice(&(b.len() as u32).to_le_bytes()); // length prefix
            all_bytes.extend_from_slice(b);
        }
        data.active(0, &ConstExpr::i32_const(base as i32), all_bytes);
    }

    // Which functions can raise, as WASM indices, so call sites know whether an
    // exception check is needed after them. Every function is registered by
    // now, so the names the analysis works in translate straight to indices.
    // A lifted lambda's body, recovered from the function the finalize pass
    // created for it, so a sort key's result type can be inferred at its call
    // site. The body is the expression the synthesized function returns, after
    // any captured-cell prelude.
    for f in &ir_module.functions {
        if !f.name.starts_with("__lambda_") {
            continue;
        }
        let Some(param) = f.params.first() else {
            continue;
        };
        if let Some(crate::ir::IRStatement::Return(Some(expr))) = f.body.statements.last() {
            ctx.lambda_bodies
                .insert(f.name.clone(), (param.name.clone(), expr.clone()));
        }
    }

    let raisers = functions_that_can_raise(ir_module);
    ctx.can_raise = raisers
        .iter()
        .filter_map(|key| ctx.function_map.get(key).map(|info| info.index))
        .collect();

    // Code section
    let mut codes = CodeSection::new();

    // `__module_init` is compiled first, whatever its position: it is where
    // each module global's type is learned, and every other function reads
    // those globals.
    let compiled_init = ir_module
        .functions
        .iter()
        .find(|f| f.name == MODULE_INIT_FN)
        .map(|init| {
            ctx.in_module_init = true;
            let return_type = module_return(init);
            let compiled = compile_function(init, &mut ctx, &memory_layout, &return_type, None);
            ctx.in_module_init = false;
            compiled
        });

    // A lifted closure reads its captured cells at the types the function
    // creating it stored, so it is compiled after that function: every other
    // function and method first, then the closures, outermost first. The code
    // section is still written in index order.
    let mut compiled_functions: Vec<Option<Function>> =
        (0..ir_module.functions.len()).map(|_| None).collect();
    let mut compiled_init = compiled_init;
    for (i, func_ir) in ir_module.functions.iter().enumerate() {
        if func_ir.name == MODULE_INIT_FN {
            compiled_functions[i] = compiled_init.take();
            continue;
        }
        if func_ir.name.starts_with("__lambda_") {
            continue;
        }
        let return_type = module_return(func_ir);
        compiled_functions[i] = Some(compile_function(
            func_ir,
            &mut ctx,
            &memory_layout,
            &return_type,
            None,
        ));
    }

    let mut compiled_methods = Vec::new();
    for cls in &ir_module.classes {
        for method in &cls.methods {
            let return_type = method_return(&cls.name, method);
            compiled_methods.push(compile_function(
                method,
                &mut ctx,
                &memory_layout,
                &return_type,
                Some(&cls.name),
            ));
        }
    }

    for i in closures_outermost_first(ir_module) {
        let func_ir = &ir_module.functions[i];
        let return_type = module_return(func_ir);
        compiled_functions[i] = Some(compile_function(
            func_ir,
            &mut ctx,
            &memory_layout,
            &return_type,
            None,
        ));
    }

    for compiled in compiled_functions.iter().flatten() {
        codes.function(compiled);
    }
    for compiled in &compiled_methods {
        codes.function(compiled);
    }

    // Runtime helpers, last so their indices match `ctx.alloc_func_index`,
    // `ctx.alloc_obj_func_index`, and `ctx.i32_to_str_func_index`.
    codes.function(&build_alloc_function());
    codes.function(&build_alloc_obj_function(ctx.alloc_func_index));
    codes.function(&build_i32_to_str_function(ctx.alloc_func_index));

    // The start function: run `__module_init`, and trap if it left an
    // exception pending, as any exception nothing catches does.
    let start_function_index = start_type_index.map(|_| {
        let init_index = ctx
            .get_function_info(MODULE_INIT_FN)
            .map(|f| f.index)
            .unwrap_or_default();
        let mut start = Function::new([]);
        start.instruction(&Instruction::Call(init_index));
        start.instruction(&Instruction::Drop);
        start.instruction(&Instruction::GlobalGet(EXC_TYPE_GLOBAL));
        start.instruction(&Instruction::If(BlockType::Empty));
        start.instruction(&Instruction::Unreachable);
        start.instruction(&Instruction::End);
        start.instruction(&Instruction::End);
        codes.function(&start);
        import_count + total_function_count as u32 + 3
    });

    // The runtime helpers the code called, each built once; a helper may call
    // others, which join the end of the list.
    let mut built = 0;
    while built < ctx.helpers.borrow().len() {
        let helper = ctx.helpers.borrow()[built].clone();
        let (params, results) = crate::compiler::runtime::signature(&helper);
        let type_index = types.len();
        types.ty().function(params, results);
        functions.function(type_index);
        codes.function(&crate::compiler::runtime::build(&helper, &ctx));
        built += 1;
    }

    module.section(&types);
    // The import section must sit between the type and function sections.
    if import_count > 0 {
        module.section(&host_imports);
    }
    module.section(&functions);

    // Memory holds the static data (strings and bytes) below
    // COLLECTION_HEAP_BASE, and the runtime bump allocator starts there and
    // grows memory on demand. Collection literals used to be carved out of a
    // static region above the base at compile time; every one is a runtime
    // allocation now, so nothing sits between the data and the heap.
    // The Unicode tables the helpers read sit at the start of the heap, and
    // the allocator starts past them.
    let (unicode_segments, runtime_heap_base) =
        crate::compiler::unicode::data_for(&ctx.helpers.borrow());
    for (at, bytes) in unicode_segments {
        data.active(0, &ConstExpr::i32_const(at as i32), bytes);
    }
    let min_pages = (((runtime_heap_base as u64) + 65535) / 65536).max(2);
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: min_pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    // Global 0: the runtime allocator's next-free pointer.
    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(runtime_heap_base as i32),
    );

    // Global 1: the StopIteration flag. Set by `raise StopIteration` (which
    // also returns from the raising function) and read-and-cleared by the
    // `__waspy_stop_check` intrinsic, so iterator exhaustion crosses the
    // `__next__` call boundary.
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(0),
    );

    // Global 2: the pending exception's type code (0 = none). `raise` sets it
    // and transfers control; a `try` reads it to pick a handler, and a handler
    // that matches clears it. It is a module global rather than a local so an
    // exception raised inside a call is visible to the caller, which is what
    // makes propagation work.
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(0),
    );

    // Global 3: how many user-function calls are on the stack below the frame
    // running now. Only calls that can raise maintain it. A frame propagating
    // an exception with nothing below it is the outermost one, so the
    // exception has escaped the program: it traps there instead of handing the
    // host a default value as if nothing happened.
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(0),
    );

    // Global 4: the pending exception's object (0 = none).
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(0),
    );

    // One global per module definition that is evaluated once, in index
    // order, typed by what its initializer produced in `__module_init` (an f64
    // for a float, one word for everything else, a string by its offset).
    for name in module_globals {
        let val_type = match ctx.module_global_types.get(name) {
            Some(IRType::Float) => ValType::F64,
            _ => ValType::I32,
        };
        let init = match val_type {
            ValType::F64 => ConstExpr::f64_const(0.0.into()),
            _ => ConstExpr::i32_const(0),
        };
        globals.global(
            GlobalType {
                val_type,
                mutable: true,
                shared: false,
            },
            &init,
        );
    }

    // One funcref table carries both indirect-call mechanisms: the closure
    // slots first (slot i holds lifted lambda i), then the vtable rows, so
    // `call_indirect` always names table 0 and a module needing either gets
    // exactly one table.
    let mut tables = TableSection::new();
    let mut elements = ElementSection::new();
    let mut table_entries: Vec<u32> = lambda_functions
        .iter()
        .map(|(idx, _)| *idx + import_count)
        .collect();
    debug_assert_eq!(
        ctx.vtable_base as usize,
        table_entries.len(),
        "vtable rows must start where the call sites were told they do"
    );
    table_entries.extend(vtable_entries.iter().copied());
    if !table_entries.is_empty() {
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: table_entries.len() as u64,
            maximum: Some(table_entries.len() as u64),
            shared: false,
        });
        elements.active(
            None,
            &ConstExpr::i32_const(0),
            Elements::Functions(table_entries.clone().into()),
        );
    }

    // Section order follows the WASM spec: Table (4), Memory (5), Global (6),
    // Export (7), Element (9), Code (10), Data (11). Code must precede Data or
    // strict validators (and Binaryen's reader) reject the module, which
    // previously disabled optimization.
    if !table_entries.is_empty() {
        module.section(&tables);
    }
    module.section(&memories);
    module.section(&globals);
    module.section(&exports);
    // Start (8) sits between Export (7) and Element (9).
    if let Some(function_index) = start_function_index {
        module.section(&StartSection { function_index });
    }
    if !table_entries.is_empty() {
        module.section(&elements);
    }
    module.section(&codes);
    module.section(&data);

    // Custom sections carry no code, so this leaves execution untouched.
    if !ir_module.comments.is_empty() {
        let payload = crate::core::comments::encode_comment_section(&ir_module.comments);
        module.section(&CustomSection {
            name: COMMENTS_SECTION_NAME.into(),
            data: payload.as_bytes().into(),
        });
    }

    // Code generation reports what it cannot generate code for through the
    // context's error sink (see `CompilationContext::report`). The whole module
    // is walked first so one compile surfaces every such call, then the
    // compilation fails rather than returning a module that traps at runtime.
    let mut errors = ctx.errors.take();
    if !errors.is_empty() {
        let first = errors.remove(0);
        if errors.is_empty() {
            return Err(first);
        }
        // Several: keep the first one's wording and list the rest under it, so
        // one compile run reports every construct codegen could not emit.
        let rest = errors
            .iter()
            .map(|e| format!("\n  also: {e}"))
            .collect::<String>();
        let ChakraError::UnsupportedFeature { message, location } = first else {
            return Err(first);
        };
        return Err(ChakraError::UnsupportedFeature {
            message: format!("{message}{rest}"),
            location,
        });
    }

    Ok(module.finish())
}
