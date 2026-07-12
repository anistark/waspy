use crate::ir::decorators::MethodKind;
use crate::ir::types::*;
use anyhow::{anyhow, Context, Result};
use rustpython_parser::ast::{ArgWithDefault, Arguments, ExceptHandler, Expr, Stmt, Suite};
use std::collections::HashSet;

/// Lower a Python AST (Suite) into our IR.
pub fn lower_ast_to_ir(ast: &Suite) -> Result<IRModule> {
    let mut module = IRModule::new();
    let mut memory_layout = MemoryLayout::new();
    let mut in_try_block = false;
    let mut conditional_fallbacks = Vec::new();

    for stmt in ast {
        match stmt {
            Stmt::FunctionDef(fundef) => {
                // Process function definition
                let name = fundef.name.to_string();
                let params = process_function_params(&fundef.args, &mut memory_layout)?;
                let return_type = if let Some(returns) = &fundef.returns {
                    type_annotation_to_ir_type(returns)?
                } else {
                    IRType::Unknown
                };

                // Extract decorators if any
                let decorators = fundef
                    .decorator_list
                    .iter()
                    .filter_map(|dec| {
                        if let Expr::Name(name) = dec {
                            Some(name.id.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();

                let body = lower_function_body(&fundef.body, &mut memory_layout)?;

                module.functions.push(IRFunction {
                    name,
                    params,
                    body,
                    return_type,
                    decorators,
                });
            }
            Stmt::ClassDef(_) => {
                // Process class definition
                let class = process_class_definition(stmt, &mut memory_layout)?;
                module.classes.push(class);
            }
            Stmt::Assign(_) => {
                // Process module-level assignment
                if let Some(var) = process_module_level_assign(stmt, &mut memory_layout)? {
                    module.variables.push(var);
                }
            }
            Stmt::AnnAssign(_) => {
                // Process module-level typed assignment
                if let Some(var) = process_module_level_ann_assign(stmt, &mut memory_layout)? {
                    module.variables.push(var);
                }
            }
            Stmt::Import(_) => {
                // Process direct import
                let imports = process_import(stmt, in_try_block, &conditional_fallbacks)?;
                module.imports.extend(imports);
            }
            Stmt::ImportFrom(_) => {
                // Process from import
                let imports = process_import_from(stmt, in_try_block, &conditional_fallbacks)?;
                module.imports.extend(imports);
            }
            Stmt::Expr(_) => {
                // Skip module-level expressions like docstrings
                // But check for dynamic imports
                if let Some(dynamic_import) = process_dynamic_import(stmt, &mut memory_layout)? {
                    module.imports.push(dynamic_import);
                }
                continue;
            }
            Stmt::Try(_) => {
                // Mark that we're entering a try block to track conditional imports
                // in_try_block = true;

                // Process try-except blocks for conditional imports
                let (imports, fallbacks) = process_try_except_imports(stmt)?;

                // Add imports from the try block
                module.imports.extend(imports);

                // Store fallbacks for except blocks
                conditional_fallbacks = fallbacks;

                // We're no longer in a try block after processing it
                in_try_block = false;
            }
            _ => {
                // Reset try block state for other statements
                in_try_block = false;
                conditional_fallbacks.clear();

                // Ignore other module-level statements for now
                // But don't error out so we can compile more files
                continue;
            }
        }
    }

    // Process circular imports
    process_circular_imports(&mut module);

    // Carry the populated string/bytes layout to the compiler, which needs the
    // resolved offsets to emit loads and the data section.
    module.memory_layout = memory_layout;

    // Whole-module checks and rewrites (abstract-class instantiation,
    // call-site parameter defaults) that need every declaration converted.
    crate::ir::finalize::finalize_module(&mut module)?;

    Ok(module)
}

/// Process a dynamic import expression (using __import__ or importlib)
fn process_dynamic_import(
    stmt: &Stmt,
    _memory_layout: &mut MemoryLayout,
) -> Result<Option<IRImport>> {
    if let Stmt::Expr(expr_stmt) = stmt {
        // Properly handle Box<Expr> by dereferencing it first
        if let Expr::Call(call) = &*expr_stmt.value {
            match &*call.func {
                Expr::Name(name) if name.id.to_string() == "__import__" => {
                    // Handle direct __import__ call
                    if !call.args.is_empty() {
                        if let Expr::Constant(constant) = &call.args[0] {
                            if let rustpython_parser::ast::Constant::Str(module_name) =
                                &constant.value
                            {
                                return Ok(Some(IRImport {
                                    module: module_name.clone(),
                                    name: None,
                                    alias: None,
                                    is_from_import: false,
                                    is_star_import: false,
                                    is_conditional: false,
                                    is_dynamic: true,
                                    conditional_fallbacks: Vec::new(),
                                }));
                            }
                        }
                    }
                }
                Expr::Attribute(attr) => {
                    // Handle importlib.import_module
                    if let Expr::Name(obj) = &*attr.value {
                        if obj.id.to_string() == "importlib"
                            && attr.attr.to_string() == "import_module"
                            && !call.args.is_empty()
                        {
                            if let Expr::Constant(constant) = &call.args[0] {
                                if let rustpython_parser::ast::Constant::Str(module_name) =
                                    &constant.value
                                {
                                    return Ok(Some(IRImport {
                                        module: module_name.clone(),
                                        name: None,
                                        alias: None,
                                        is_from_import: false,
                                        is_star_import: false,
                                        is_conditional: false,
                                        is_dynamic: true,
                                        conditional_fallbacks: Vec::new(),
                                    }));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(None)
}

/// Process try-except blocks that may contain conditional imports
fn process_try_except_imports(stmt: &Stmt) -> Result<(Vec<IRImport>, Vec<String>)> {
    let mut imports = Vec::new();
    let mut fallbacks = Vec::new();

    if let Stmt::Try(try_stmt) = stmt {
        // Process imports in the try block
        for try_stmt in &try_stmt.body {
            match try_stmt {
                Stmt::Import(_) => {
                    let try_imports = process_import(try_stmt, true, &Vec::new())?;
                    imports.extend(try_imports);
                }
                Stmt::ImportFrom(_) => {
                    let try_imports = process_import_from(try_stmt, true, &Vec::new())?;
                    imports.extend(try_imports);
                }
                _ => {}
            }
        }

        // Collect potential fallback modules from except blocks
        for handler in &try_stmt.handlers {
            // Get the handler body - it's a tuple variant in this version
            let ExceptHandler::ExceptHandler(handler_data) = handler;
            let _typ = handler_data.type_.as_ref();
            let _name = handler_data.name.as_ref().map(|n| n.to_string());
            let body = &handler_data.body;
            for except_stmt in body {
                match except_stmt {
                    Stmt::Import(import) => {
                        // Only record the module names as fallbacks
                        for alias in &import.names {
                            fallbacks.push(alias.name.to_string());

                            // Also add these imports as conditionals
                            imports.push(IRImport {
                                module: alias.name.to_string(),
                                name: None,
                                alias: alias.asname.as_ref().map(|a| a.to_string()),
                                is_from_import: false,
                                is_star_import: false,
                                is_conditional: true,
                                is_dynamic: false,
                                conditional_fallbacks: Vec::new(),
                            });
                        }
                    }
                    Stmt::ImportFrom(import_from) => {
                        // Only record the module names as fallbacks
                        if let Some(module) = &import_from.module {
                            fallbacks.push(module.to_string());

                            // Also add these imports as conditionals
                            for alias in &import_from.names {
                                imports.push(IRImport {
                                    module: module.to_string(),
                                    name: Some(alias.name.to_string()),
                                    alias: alias.asname.as_ref().map(|a| a.to_string()),
                                    is_from_import: true,
                                    is_star_import: alias.name.to_string() == "*",
                                    is_conditional: true,
                                    is_dynamic: false,
                                    conditional_fallbacks: Vec::new(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok((imports, fallbacks))
}

/// Process circular imports
fn process_circular_imports(module: &mut IRModule) {
    let mut imported_modules = HashSet::new();
    let mut potentially_circular = Vec::new();

    // Collect all module names
    for import in &module.imports {
        imported_modules.insert(import.module.clone());
    }

    // Detect circular imports
    for (idx, import) in module.imports.iter().enumerate() {
        for other_import in &module.imports {
            if import.module == other_import.module {
                continue;
            }

            if imported_modules.contains(&other_import.module)
                && other_import.module == import.module
            {
                potentially_circular.push((idx, import.module.clone()));
                break;
            }
        }
    }

    for (_, module_name) in potentially_circular {
        module
            .metadata
            .insert(format!("circular_import_{module_name}"), "true".to_string());
    }
}

/// Process a class definition
fn process_class_definition(stmt: &Stmt, memory_layout: &mut MemoryLayout) -> Result<IRClass> {
    if let Stmt::ClassDef(classdef) = stmt {
        let name = classdef.name.to_string();

        // Extract base classes: bare names (`Shape`) and attribute paths
        // (`abc.ABC`), which only appear for module-qualified marker bases.
        let bases: Vec<String> = classdef
            .bases
            .iter()
            .filter_map(|base| match base {
                Expr::Name(name) => Some(name.id.to_string()),
                Expr::Attribute(attr) => match &*attr.value {
                    Expr::Name(module) => Some(format!("{}.{}", module.id, attr.attr)),
                    _ => None,
                },
                _ => None,
            })
            .collect();

        // Only single inheritance is supported: reject multiple bases loudly
        // rather than silently compiling a class with a broken field layout.
        // `object` is every class's implicit root and `ABC` is a marker (it
        // contributes no fields or methods), so neither counts.
        let real_bases: Vec<&String> = bases
            .iter()
            .filter(|b| !matches!(b.as_str(), "object" | "ABC" | "abc.ABC"))
            .collect();
        if real_bases.len() > 1 {
            return Err(anyhow!(
                "class '{name}' declares multiple base classes ({}); only single inheritance is supported",
                real_bases
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Class decorators: `@dataclass`, `@dataclasses.dataclass`, and the
        // bare call form `@dataclass()`. The argument form (`frozen=True`,
        // `order=True`, ...) changes semantics we don't implement, so it is
        // rejected loudly instead of being silently ignored.
        let mut is_dataclass = false;
        for dec in &classdef.decorator_list {
            let (dec_name, has_args) = match dec {
                Expr::Name(n) => (n.id.to_string(), false),
                Expr::Attribute(attr) => match &*attr.value {
                    Expr::Name(base) => (format!("{}.{}", base.id, attr.attr), false),
                    _ => continue,
                },
                Expr::Call(call) => {
                    let callee = match &*call.func {
                        Expr::Name(n) => n.id.to_string(),
                        Expr::Attribute(attr) => match &*attr.value {
                            Expr::Name(base) => format!("{}.{}", base.id, attr.attr),
                            _ => continue,
                        },
                        _ => continue,
                    };
                    (callee, !call.args.is_empty() || !call.keywords.is_empty())
                }
                _ => continue,
            };
            if dec_name == "dataclass" || dec_name == "dataclasses.dataclass" {
                if has_args {
                    return Err(crate::core::errors::unsupported_feature(
                        format!(
                            "@dataclass arguments (frozen, order, ...) are not supported \
                             on class '{name}'"
                        ),
                        None,
                    )
                    .into());
                }
                is_dataclass = true;
            }
        }

        // Dataclass fields come from the annotated class-level assignments, in
        // source order, validated against Python's dataclass rules.
        let dataclass_fields = if is_dataclass {
            collect_dataclass_fields(&name, &classdef.body, memory_layout)?
        } else {
            Vec::new()
        };

        let mut methods = Vec::new();
        let mut class_vars = Vec::new();
        // (method name, kind) in source order, for post-validation below.
        let mut method_kinds: Vec<(String, MethodKind)> = Vec::new();

        // Process class body
        for stmt in &classdef.body {
            match stmt {
                Stmt::FunctionDef(method_def) => {
                    // Process method (similar to function but with 'self' parameter)
                    let method_name = method_def.name.to_string();
                    let mut params = process_function_params(&method_def.args, memory_layout)?;

                    // Capture both bare-name decorators (`@property`) and the
                    // attribute form used by property setters (`@radius.setter`).
                    let decorators: Vec<String> = method_def
                        .decorator_list
                        .iter()
                        .filter_map(|dec| match dec {
                            Expr::Name(name) => Some(name.id.to_string()),
                            Expr::Attribute(attr) => match &*attr.value {
                                Expr::Name(base) => Some(format!("{}.{}", base.id, attr.attr)),
                                _ => None,
                            },
                            _ => None,
                        })
                        .collect();

                    // Classify the method's binding up front so an unsupported
                    // or conflicting decorator stack fails compilation with a
                    // clear error instead of mis-dispatching at call sites.
                    let kind =
                        crate::ir::decorators::method_kind(&name, &method_name, &decorators)?;

                    // The implicit first parameter is untyped in source; type it
                    // as the enclosing class so attribute access/assignment can
                    // resolve field offsets and types. `cls` in a classmethod is
                    // typed the same way, which lets `cls(...)`/`cls.attr` inside
                    // the body resolve to the defining class (static dispatch).
                    // Static methods have no implicit parameter.
                    match kind {
                        MethodKind::Static => {}
                        MethodKind::Class => {
                            if let Some(first) = params.first_mut() {
                                first.param_type = IRType::Class(name.clone());
                            }
                        }
                        _ => {
                            if let Some(first) = params.first_mut() {
                                if first.name == "self" {
                                    first.param_type = IRType::Class(name.clone());
                                }
                            }
                        }
                    }
                    method_kinds.push((method_name.clone(), kind));

                    let return_type = if let Some(returns) = &method_def.returns {
                        type_annotation_to_ir_type(returns)?
                    } else {
                        IRType::Unknown
                    };

                    let body = lower_function_body(&method_def.body, memory_layout)?;

                    methods.push(IRFunction {
                        name: method_name,
                        params,
                        body,
                        return_type,
                        decorators,
                    });
                }
                Stmt::Assign(_) => {
                    // Process class variable
                    if let Some(var) = process_module_level_assign(stmt, memory_layout)? {
                        class_vars.push(var);
                    }
                }
                Stmt::AnnAssign(_) => {
                    // Process typed class variable
                    if let Some(var) = process_module_level_ann_assign(stmt, memory_layout)? {
                        class_vars.push(var);
                    }
                }
                _ => {
                    // Ignore other class body statements for now
                }
            }
        }

        // A setter is reachable only through attribute assignment, which
        // resolves the property via its getter; a setter without a matching
        // `@property` getter can never fire, so reject it loudly.
        for (method_name, kind) in &method_kinds {
            if *kind == MethodKind::PropertySetter
                && !method_kinds
                    .iter()
                    .any(|(n, k)| n == method_name && *k == MethodKind::PropertyGetter)
            {
                return Err(crate::core::errors::unsupported_feature(
                    format!(
                        "setter '@{method_name}.setter' in class '{name}' has no matching \
                         '@property' getter named '{method_name}'"
                    ),
                    None,
                )
                .into());
            }
        }

        // A dataclass gets `__init__`, `__eq__`, and `__repr__` generated from
        // its annotated fields; a method the user wrote in the class body wins
        // over the generated one, matching CPython's dataclass behavior.
        if is_dataclass {
            synthesize_dataclass_methods(&name, &dataclass_fields, &mut methods, memory_layout);
        }

        Ok(IRClass {
            name,
            bases,
            methods,
            class_vars,
        })
    } else {
        Err(anyhow!("Expected ClassDef statement"))
    }
}

/// One `name: type [= default]` field of a `@dataclass` body, in source order.
struct DataclassField {
    name: String,
    ty: IRType,
    default: Option<IRExpr>,
}

/// Collect the annotated class-level assignments of a `@dataclass` body as its
/// fields, enforcing Python's dataclass rules at compile time: a field without
/// a default may not follow one with a default, mutable defaults (list/dict/set
/// literals) are rejected, and `dataclasses.field(...)` is unsupported.
/// `ClassVar`-annotated names are class variables, not fields.
fn collect_dataclass_fields(
    class_name: &str,
    body: &[Stmt],
    memory_layout: &mut MemoryLayout,
) -> Result<Vec<DataclassField>> {
    let mut fields: Vec<DataclassField> = Vec::new();
    let mut first_defaulted: Option<String> = None;

    for stmt in body {
        let Stmt::AnnAssign(ann) = stmt else {
            continue;
        };
        let Expr::Name(target) = &*ann.target else {
            continue;
        };
        let field_name = target.id.to_string();

        // `x: ClassVar[int] = 0` stays a class variable (already handled by the
        // class-var path); it never becomes an instance field.
        let is_class_var = match &*ann.annotation {
            Expr::Name(n) => n.id.as_str() == "ClassVar",
            Expr::Subscript(sub) => {
                matches!(&*sub.value, Expr::Name(n) if n.id.as_str() == "ClassVar")
            }
            _ => false,
        };
        if is_class_var {
            continue;
        }

        let default = match ann.value.as_deref() {
            Some(value) => {
                // `field(...)` configures per-field behavior (default_factory,
                // compare, ...) that we don't implement.
                if let Expr::Call(call) = value {
                    let callee = match &*call.func {
                        Expr::Name(n) => n.id.to_string(),
                        Expr::Attribute(attr) => match &*attr.value {
                            Expr::Name(base) => format!("{}.{}", base.id, attr.attr),
                            _ => String::new(),
                        },
                        _ => String::new(),
                    };
                    if callee == "field" || callee == "dataclasses.field" {
                        return Err(crate::core::errors::unsupported_feature(
                            format!(
                                "dataclasses.field(...) on field '{field_name}' of dataclass \
                                 '{class_name}' is not supported"
                            ),
                            None,
                        )
                        .into());
                    }
                }
                // Python rejects mutable defaults (they'd be shared across
                // instances); mirror that instead of compiling sharing bugs.
                if matches!(
                    value,
                    Expr::List(_) | Expr::Dict(_) | Expr::Set(_) | Expr::ListComp(_)
                ) {
                    return Err(crate::core::errors::type_error(
                        format!(
                            "mutable default for field '{field_name}' of dataclass \
                             '{class_name}' is not allowed"
                        ),
                        None,
                    )
                    .into());
                }
                Some(lower_expr(value, memory_layout)?)
            }
            None => None,
        };

        match (&default, &first_defaulted) {
            (Some(_), None) => first_defaulted = Some(field_name.clone()),
            (None, Some(defaulted)) => {
                return Err(crate::core::errors::type_error(
                    format!(
                        "non-default argument '{field_name}' follows default argument \
                         '{defaulted}' in dataclass '{class_name}'"
                    ),
                    None,
                )
                .into());
            }
            _ => {}
        }

        fields.push(DataclassField {
            name: field_name,
            ty: type_annotation_to_ir_type(&ann.annotation)?,
            default,
        });
    }

    Ok(fields)
}

/// Append the generated `__init__`, `__eq__`, and `__repr__` of a `@dataclass`
/// to its method list, skipping any the user defined explicitly.
///
/// `__init__` takes one parameter per field (carrying the field's default) and
/// assigns `self.<field> = <field>`, so the regular field-discovery and
/// instantiation machinery applies unchanged. `__eq__` compares fields
/// pairwise (callers dispatch `==`/`!=` on class operands to it). `__repr__`
/// builds `Name(field=value, ...)` at runtime and is only generated when every
/// field can be stringified (int/bool via `str()`, str spliced directly) —
/// float formatting has no runtime support yet.
fn synthesize_dataclass_methods(
    class_name: &str,
    fields: &[DataclassField],
    methods: &mut Vec<IRFunction>,
    memory_layout: &mut MemoryLayout,
) {
    let has_method = |methods: &[IRFunction], name: &str| methods.iter().any(|m| m.name == name);
    let self_param = || IRParam {
        name: "self".to_string(),
        param_type: IRType::Class(class_name.to_string()),
        default_value: None,
    };
    let self_field = |field: &str| IRExpr::Attribute {
        object: Box::new(IRExpr::Variable("self".to_string())),
        attribute: field.to_string(),
    };

    if !has_method(methods, "__init__") {
        let mut params = vec![self_param()];
        let mut statements = Vec::new();
        for field in fields {
            params.push(IRParam {
                name: field.name.clone(),
                param_type: field.ty.clone(),
                default_value: field.default.clone(),
            });
            statements.push(IRStatement::AttributeAssign {
                object: IRExpr::Variable("self".to_string()),
                attribute: field.name.clone(),
                value: IRExpr::Variable(field.name.clone()),
            });
        }
        methods.push(IRFunction {
            name: "__init__".to_string(),
            params,
            body: IRBody { statements },
            return_type: IRType::Unknown,
            decorators: Vec::new(),
        });
    }

    if !has_method(methods, "__eq__") {
        let other_param = IRParam {
            name: "other".to_string(),
            param_type: IRType::Class(class_name.to_string()),
            default_value: None,
        };
        let other_field = |field: &str| IRExpr::Attribute {
            object: Box::new(IRExpr::Variable("other".to_string())),
            attribute: field.to_string(),
        };
        // Fold the per-field comparisons into an `and` chain; a fieldless
        // dataclass compares equal to any instance of its class.
        let mut comparison: Option<IRExpr> = None;
        for field in fields {
            let field_eq = IRExpr::CompareOp {
                left: Box::new(self_field(&field.name)),
                right: Box::new(other_field(&field.name)),
                op: IRCompareOp::Eq,
            };
            comparison = Some(match comparison {
                Some(chain) => IRExpr::BoolOp {
                    left: Box::new(chain),
                    right: Box::new(field_eq),
                    op: IRBoolOp::And,
                },
                None => field_eq,
            });
        }
        let result = comparison.unwrap_or(IRExpr::Const(IRConstant::Bool(true)));
        methods.push(IRFunction {
            name: "__eq__".to_string(),
            params: vec![self_param(), other_param],
            body: IRBody {
                statements: vec![IRStatement::Return(Some(result))],
            },
            return_type: IRType::Bool,
            decorators: Vec::new(),
        });
    }

    let reprable = fields
        .iter()
        .all(|f| matches!(f.ty, IRType::Int | IRType::Bool | IRType::String));
    if reprable && !has_method(methods, "__repr__") {
        // Alternate constant labels with runtime field values, merging adjacent
        // constants so each interned label is a single blob. Every constant is
        // registered in the module layout here, since these strings never pass
        // through the normal literal-lowering path.
        let mut text = format!("{class_name}(");
        let mut parts: Vec<IRExpr> = Vec::new();
        let flush =
            |text: &mut String, parts: &mut Vec<IRExpr>, memory_layout: &mut MemoryLayout| {
                if !text.is_empty() {
                    memory_layout.add_string(text);
                    parts.push(IRExpr::Const(IRConstant::String(std::mem::take(text))));
                }
            };
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                text.push_str(", ");
            }
            text.push_str(&field.name);
            text.push('=');
            if field.ty == IRType::String {
                // repr() quotes string values.
                text.push('\'');
                flush(&mut text, &mut parts, memory_layout);
                parts.push(self_field(&field.name));
                text.push('\'');
            } else {
                flush(&mut text, &mut parts, memory_layout);
                parts.push(IRExpr::FunctionCall {
                    function_name: "str".to_string(),
                    arguments: vec![self_field(&field.name)],
                });
            }
        }
        text.push(')');
        flush(&mut text, &mut parts, memory_layout);

        let repr = parts
            .into_iter()
            .reduce(|acc, part| IRExpr::BinaryOp {
                left: Box::new(acc),
                right: Box::new(part),
                op: IROp::Add,
            })
            .expect("repr always has at least the constant shell");
        methods.push(IRFunction {
            name: "__repr__".to_string(),
            params: vec![self_param()],
            body: IRBody {
                statements: vec![IRStatement::Return(Some(repr))],
            },
            return_type: IRType::String,
            decorators: Vec::new(),
        });
    }
}

/// Process a module-level assignment
fn process_module_level_assign(
    stmt: &Stmt,
    memory_layout: &mut MemoryLayout,
) -> Result<Option<IRVariable>> {
    if let Stmt::Assign(assign) = stmt {
        // Handle only simple assignments for now (single target)
        if assign.targets.len() != 1 {
            return Ok(None);
        }

        let target = match &assign.targets[0] {
            Expr::Name(name) => name.id.to_string(),
            _ => return Ok(None), // Skip complex assignments
        };

        let value = lower_expr(&assign.value, memory_layout)?;

        Ok(Some(IRVariable {
            name: target,
            value,
            var_type: None,
        }))
    } else {
        Ok(None)
    }
}

/// Process a module-level typed assignment
fn process_module_level_ann_assign(
    stmt: &Stmt,
    memory_layout: &mut MemoryLayout,
) -> Result<Option<IRVariable>> {
    if let Stmt::AnnAssign(ann_assign) = stmt {
        let target = match &*ann_assign.target {
            Expr::Name(name) => name.id.to_string(),
            _ => return Ok(None), // Skip complex assignments
        };

        let var_type = type_annotation_to_ir_type(&ann_assign.annotation)?;

        let value = if let Some(value) = &ann_assign.value {
            lower_expr(value, memory_layout)?
        } else {
            // Create a default value based on the type
            match var_type {
                IRType::Int => IRExpr::Const(IRConstant::Int(0)),
                IRType::Float => IRExpr::Const(IRConstant::Float(0.0)),
                IRType::Bool => IRExpr::Const(IRConstant::Bool(false)),
                IRType::String => IRExpr::Const(IRConstant::String(String::new())),
                IRType::Bytes => IRExpr::Const(IRConstant::Bytes(Vec::new())),
                IRType::List(_) => IRExpr::ListLiteral(Vec::new()),
                IRType::Dict(_, _) => IRExpr::DictLiteral(Vec::new()),
                IRType::Set(_) => IRExpr::SetLiteral(Vec::new()),
                _ => IRExpr::Const(IRConstant::None),
            }
        };

        Ok(Some(IRVariable {
            name: target,
            value,
            var_type: Some(var_type),
        }))
    } else {
        Ok(None)
    }
}

/// Process an import statement
fn process_import(
    stmt: &Stmt,
    is_conditional: bool,
    conditional_fallbacks: &[String],
) -> Result<Vec<IRImport>> {
    if let Stmt::Import(import) = stmt {
        let mut imports = Vec::new();

        for alias in &import.names {
            let module = alias.name.to_string();
            let alias_name = alias.asname.as_ref().map(|a| a.to_string());

            imports.push(IRImport {
                module,
                name: None,
                alias: alias_name,
                is_from_import: false,
                is_star_import: false,
                is_conditional,
                is_dynamic: false,
                conditional_fallbacks: conditional_fallbacks.to_vec(),
            });
        }

        Ok(imports)
    } else {
        Ok(Vec::new())
    }
}

/// Process a from-import statement
fn process_import_from(
    stmt: &Stmt,
    is_conditional: bool,
    conditional_fallbacks: &[String],
) -> Result<Vec<IRImport>> {
    if let Stmt::ImportFrom(import_from) = stmt {
        let mut imports = Vec::new();

        let module = match &import_from.module {
            Some(module) => module.to_string(),
            None => return Ok(imports), // Skip relative imports for now
        };

        // Handle star imports (from module import *)
        let _is_star_import = import_from
            .names
            .iter()
            .any(|alias| alias.name.to_string() == "*");

        for alias in &import_from.names {
            let name = alias.name.to_string();

            // For star imports, we process differently
            if name == "*" {
                imports.push(IRImport {
                    module: module.clone(),
                    name: Some(name),
                    alias: None,
                    is_from_import: true,
                    is_star_import: true,
                    is_conditional,
                    is_dynamic: false,
                    conditional_fallbacks: conditional_fallbacks.to_vec(),
                });
                // Star import should be the only import in this statement
                break;
            }

            let alias_name = alias.asname.as_ref().map(|a| a.to_string());

            imports.push(IRImport {
                module: module.clone(),
                name: Some(name),
                alias: alias_name,
                is_from_import: true,
                is_star_import: false,
                is_conditional,
                is_dynamic: false,
                conditional_fallbacks: conditional_fallbacks.to_vec(),
            });
        }

        Ok(imports)
    } else {
        Ok(Vec::new())
    }
}

/// Convert type annotations to IR types
fn type_annotation_to_ir_type(expr: &Expr) -> Result<IRType> {
    match expr {
        // A quoted forward reference (`-> "Counter"`) names a class defined
        // in the same module; treat it exactly like the bare name.
        Expr::Constant(c) => match &c.value {
            rustpython_parser::ast::Constant::Str(s) => Ok(IRType::Class(s.to_string())),
            rustpython_parser::ast::Constant::None => Ok(IRType::None),
            _ => Ok(IRType::Any),
        },
        Expr::Name(name) => match name.id.to_string().as_str() {
            "int" => Ok(IRType::Int),
            "float" => Ok(IRType::Float),
            "bool" => Ok(IRType::Bool),
            "str" => Ok(IRType::String),
            "bytes" => Ok(IRType::Bytes),
            "None" => Ok(IRType::None),
            "Any" => Ok(IRType::Any),
            // Bare builtin collection annotations (element types unknown).
            "list" => Ok(IRType::List(Box::new(IRType::Unknown))),
            "set" => Ok(IRType::Set(Box::new(IRType::Unknown))),
            "tuple" => Ok(IRType::Tuple(vec![])),
            "dict" => Ok(IRType::Dict(
                Box::new(IRType::Unknown),
                Box::new(IRType::Unknown),
            )),
            _ => Ok(IRType::Class(name.id.to_string())),
        },
        Expr::Subscript(subscript) => {
            // Handle generic types like List[int]
            if let Expr::Name(container) = &*subscript.value {
                match container.id.to_string().as_str() {
                    "List" | "list" => {
                        let element_type = type_annotation_to_ir_type(&subscript.slice)?;
                        Ok(IRType::List(Box::new(element_type)))
                    }
                    "Set" | "set" => {
                        let element_type = type_annotation_to_ir_type(&subscript.slice)?;
                        Ok(IRType::Set(Box::new(element_type)))
                    }
                    "Dict" | "dict" => {
                        if let Expr::Tuple(tuple) = &*subscript.slice {
                            if tuple.elts.len() == 2 {
                                let key_type = type_annotation_to_ir_type(&tuple.elts[0])?;
                                let value_type = type_annotation_to_ir_type(&tuple.elts[1])?;
                                Ok(IRType::Dict(Box::new(key_type), Box::new(value_type)))
                            } else {
                                Err(anyhow!(
                                    "Dict type annotation should have exactly 2 elements"
                                ))
                            }
                        } else {
                            Err(anyhow!("Invalid Dict type annotation"))
                        }
                    }
                    "Optional" => {
                        let inner_type = type_annotation_to_ir_type(&subscript.slice)?;
                        Ok(IRType::Optional(Box::new(inner_type)))
                    }
                    "Tuple" => {
                        if let Expr::Tuple(tuple) = &*subscript.slice {
                            let mut types = Vec::new();
                            for elem in &tuple.elts {
                                types.push(type_annotation_to_ir_type(elem)?);
                            }
                            Ok(IRType::Tuple(types))
                        } else {
                            Ok(IRType::Tuple(vec![type_annotation_to_ir_type(
                                &subscript.slice,
                            )?]))
                        }
                    }
                    "Union" => {
                        if let Expr::Tuple(tuple) = &*subscript.slice {
                            let mut types = Vec::new();
                            for elem in &tuple.elts {
                                types.push(type_annotation_to_ir_type(elem)?);
                            }
                            Ok(IRType::Union(types))
                        } else {
                            Err(anyhow!("Union type annotation should have multiple types"))
                        }
                    }
                    _ => Ok(IRType::Class(container.id.to_string())),
                }
            } else {
                Ok(IRType::Any)
            }
        }
        _ => Ok(IRType::Any),
    }
}

/// Process function parameters with possible type annotations. Defaults are
/// lowered against the module's real memory layout so that (e.g.) a string
/// default's offset is valid when the default is later inlined at a call site
/// that omits the argument.
fn process_function_params(
    args: &Arguments,
    memory_layout: &mut MemoryLayout,
) -> Result<Vec<IRParam>> {
    args.args
        .iter()
        .map(|arg_with_default: &ArgWithDefault| {
            let name = arg_with_default.def.arg.to_string();

            // Check for type annotation
            let param_type = if let Some(annotation) = &arg_with_default.def.annotation {
                type_annotation_to_ir_type(annotation)?
            } else {
                IRType::Unknown
            };

            // Check for default value
            let default_value = if let Some(default) = &arg_with_default.default {
                Some(lower_expr(default, memory_layout)?)
            } else {
                None
            };

            Ok(IRParam {
                name,
                param_type,
                default_value,
            })
        })
        .collect()
}

/// Lower a function body (sequence of statements) to IR
fn lower_function_body(stmts: &[Stmt], memory_layout: &mut MemoryLayout) -> Result<IRBody> {
    let mut ir_statements = Vec::new();

    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                let expr = if let Some(value) = &ret.value {
                    Some(lower_expr(value, memory_layout)?)
                } else {
                    None
                };
                ir_statements.push(IRStatement::Return(expr));
            }
            Stmt::Assign(assign) => {
                // Handle assignment like "x = 5" or "self.width = width"
                if assign.targets.len() != 1 {
                    return Err(anyhow!("Only single target assignments supported"));
                }

                match &assign.targets[0] {
                    Expr::Name(name) => {
                        let target = name.id.to_string();
                        let value = lower_expr(&assign.value, memory_layout)?;
                        ir_statements.push(IRStatement::Assign {
                            target,
                            value,
                            var_type: None,
                        });
                    }
                    Expr::Tuple(tuple_expr) => {
                        // Handle tuple unpacking like "a, b = (1, 2)"
                        let targets: Result<Vec<String>, _> = tuple_expr
                            .elts
                            .iter()
                            .map(|elt| match elt {
                                Expr::Name(name) => Ok(name.id.to_string()),
                                _ => Err(anyhow!(
                                    "Only simple variable names supported in tuple unpacking"
                                )),
                            })
                            .collect();
                        let targets = targets?;
                        let value = lower_expr(&assign.value, memory_layout)?;

                        ir_statements.push(IRStatement::TupleUnpack { targets, value });
                    }
                    Expr::Attribute(attr) => {
                        // Handle attribute assignment like "self.width = width"
                        let object = lower_expr(&attr.value, memory_layout)?;
                        let attribute = attr.attr.to_string();
                        let value = lower_expr(&assign.value, memory_layout)?;

                        ir_statements.push(IRStatement::AttributeAssign {
                            object,
                            attribute,
                            value,
                        });
                    }
                    Expr::Subscript(subscript) => {
                        // Handle subscript assignment like "list[0] = value" or "dict[key] = value"
                        let container = lower_expr(&subscript.value, memory_layout)?;
                        let index = lower_expr(&subscript.slice, memory_layout)?;
                        let value = lower_expr(&assign.value, memory_layout)?;

                        ir_statements.push(IRStatement::IndexAssign {
                            container,
                            index,
                            value,
                        });
                    }
                    _ => {
                        return Err(anyhow!(
                            "Only variable, attribute, subscript, or tuple assignment supported"
                        ))
                    }
                }
            }
            Stmt::AnnAssign(ann_assign) => {
                // Handle typed assignment like "x: int = 5"
                let target = match &*ann_assign.target {
                    Expr::Name(name) => name.id.to_string(),
                    _ => return Err(anyhow!("Only variable assignment supported")),
                };

                let var_type = type_annotation_to_ir_type(&ann_assign.annotation)?;

                let value = if let Some(value) = &ann_assign.value {
                    lower_expr(value, memory_layout)?
                } else {
                    // Handle declarations without assignment ("x: int")
                    match &var_type {
                        IRType::Int => IRExpr::Const(IRConstant::Int(0)),
                        IRType::Float => IRExpr::Const(IRConstant::Float(0.0)),
                        IRType::Bool => IRExpr::Const(IRConstant::Bool(false)),
                        IRType::String => IRExpr::Const(IRConstant::String(String::new())),
                        IRType::Bytes => IRExpr::Const(IRConstant::Bytes(Vec::new())),
                        IRType::Set(_) => IRExpr::SetLiteral(Vec::new()),
                        IRType::None => IRExpr::Const(IRConstant::None),
                        _ => IRExpr::Const(IRConstant::None),
                    }
                };

                ir_statements.push(IRStatement::Assign {
                    target,
                    value,
                    var_type: Some(var_type),
                });
            }
            Stmt::AugAssign(aug_assign) => {
                // Handle augmented assignment like "x += 5" or "self.width *= factor"
                // Convert the operator to our IR operator
                let op = match aug_assign.op {
                    rustpython_parser::ast::Operator::Add => IROp::Add,
                    rustpython_parser::ast::Operator::Sub => IROp::Sub,
                    rustpython_parser::ast::Operator::Mult => IROp::Mul,
                    rustpython_parser::ast::Operator::Div => IROp::Div,
                    rustpython_parser::ast::Operator::Mod => IROp::Mod,
                    rustpython_parser::ast::Operator::FloorDiv => IROp::FloorDiv,
                    rustpython_parser::ast::Operator::Pow => IROp::Pow,
                    rustpython_parser::ast::Operator::MatMult => IROp::MatMul,
                    rustpython_parser::ast::Operator::LShift => IROp::LShift,
                    rustpython_parser::ast::Operator::RShift => IROp::RShift,
                    rustpython_parser::ast::Operator::BitOr => IROp::BitOr,
                    rustpython_parser::ast::Operator::BitXor => IROp::BitXor,
                    rustpython_parser::ast::Operator::BitAnd => IROp::BitAnd,
                };

                // Handle different types of targets
                match &*aug_assign.target {
                    Expr::Name(name) => {
                        let target = name.id.to_string();
                        let value = lower_expr(&aug_assign.value, memory_layout)?;

                        ir_statements.push(IRStatement::AugAssign { target, value, op });
                    }
                    Expr::Attribute(attr) => {
                        let object = lower_expr(&attr.value, memory_layout)?;
                        let attribute = attr.attr.to_string();
                        let value = lower_expr(&aug_assign.value, memory_layout)?;

                        ir_statements.push(IRStatement::AttributeAugAssign {
                            object,
                            attribute,
                            value,
                            op,
                        });
                    }
                    _ => return Err(anyhow!("Unsupported augmented assignment target")),
                }
            }
            Stmt::If(if_stmt) => {
                let condition = lower_expr(&if_stmt.test, memory_layout)?;
                let then_body = Box::new(lower_function_body(&if_stmt.body, memory_layout)?);

                let else_body = if !if_stmt.orelse.is_empty() {
                    Some(Box::new(lower_function_body(
                        &if_stmt.orelse,
                        memory_layout,
                    )?))
                } else {
                    None
                };

                ir_statements.push(IRStatement::If {
                    condition,
                    then_body,
                    else_body,
                });
            }
            Stmt::Raise(raise_stmt) => {
                let expr = if let Some(exc) = &raise_stmt.exc {
                    Some(lower_expr(exc, memory_layout)?)
                } else {
                    None
                };

                // print!("Expression generated by parse: {:?}\n", expr);
                ir_statements.push(IRStatement::Raise { exception: expr });
            }
            Stmt::While(while_stmt) => {
                let condition = lower_expr(&while_stmt.test, memory_layout)?;
                let body = Box::new(lower_function_body(&while_stmt.body, memory_layout)?);

                ir_statements.push(IRStatement::While { condition, body });
            }
            Stmt::Break(_) => {
                ir_statements.push(IRStatement::Break);
            }
            Stmt::Continue(_) => {
                ir_statements.push(IRStatement::Continue);
            }
            Stmt::Expr(expr_stmt) => {
                // Check for yield statements
                if let Expr::Yield(yield_expr) = &*expr_stmt.value {
                    let value = if let Some(val) = &yield_expr.value {
                        Some(lower_expr(val, memory_layout)?)
                    } else {
                        None
                    };
                    ir_statements.push(IRStatement::Yield { value });
                } else if let Some(dynamic_import) =
                    check_for_dynamic_import_expr(&expr_stmt.value, memory_layout)?
                {
                    ir_statements.push(dynamic_import);
                } else {
                    // Regular expression statement
                    let expr = lower_expr(&expr_stmt.value, memory_layout)?;
                    ir_statements.push(IRStatement::Expression(expr));
                }
            }
            Stmt::For(for_stmt) => {
                // Handle for loops (only simple variable target for now)
                let target = match &*for_stmt.target {
                    Expr::Name(name) => name.id.to_string(),
                    _ => {
                        return Err(anyhow!(
                            "Only simple variable targets supported in for loops"
                        ))
                    }
                };

                let iterable = lower_expr(&for_stmt.iter, memory_layout)?;
                let body = Box::new(lower_function_body(&for_stmt.body, memory_layout)?);
                let else_body = if !for_stmt.orelse.is_empty() {
                    Some(Box::new(lower_function_body(
                        &for_stmt.orelse,
                        memory_layout,
                    )?))
                } else {
                    None
                };

                ir_statements.push(IRStatement::For {
                    target,
                    iterable,
                    body,
                    else_body,
                });
            }
            Stmt::Try(try_stmt) => {
                // Handle try-except-finally statements
                let try_body = Box::new(lower_function_body(&try_stmt.body, memory_layout)?);

                let mut except_handlers = Vec::new();
                for handler in &try_stmt.handlers {
                    // Get the handler fields using tuple variant pattern matching
                    let ExceptHandler::ExceptHandler(handler_data) = handler;
                    let typ = handler_data.type_.as_ref();
                    let name = handler_data.name.as_ref().map(|n| n.to_string());
                    let body = &handler_data.body;
                    // Extract exception type from the type expression
                    let exception_type = if let Some(typ) = typ {
                        match &**typ {
                            Expr::Name(name) => Some(name.id.to_string()),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Extract name if present
                    let handler_name = name.as_ref().map(|n| n.to_string());

                    // Process the body
                    let handler_body = lower_function_body(body, memory_layout)?;

                    except_handlers.push(IRExceptHandler {
                        exception_type,
                        name: handler_name,
                        body: handler_body,
                    });
                }

                let finally_body = if !try_stmt.finalbody.is_empty() {
                    Some(Box::new(lower_function_body(
                        &try_stmt.finalbody,
                        memory_layout,
                    )?))
                } else {
                    None
                };

                ir_statements.push(IRStatement::TryExcept {
                    try_body,
                    except_handlers,
                    finally_body,
                });
            }
            Stmt::With(with_stmt) => {
                // Handle with statements (simple case)
                if with_stmt.items.len() != 1 {
                    return Err(anyhow!("Only single context manager supported"));
                }

                let context_item = &with_stmt.items[0];
                let context_expr = lower_expr(&context_item.context_expr, memory_layout)?;

                // Handle the optional variable
                let optional_vars = if let Some(var_expr) = &context_item.optional_vars {
                    match &**var_expr {
                        Expr::Name(name) => Some(name.id.to_string()),
                        _ => None, // Skip complex variable patterns
                    }
                } else {
                    None
                };

                let body = Box::new(lower_function_body(&with_stmt.body, memory_layout)?);

                ir_statements.push(IRStatement::With {
                    context_expr,
                    optional_vars,
                    body,
                });
            }
            Stmt::Import(_) => {
                // Handle inline imports within functions
                if let Ok(imports) = process_import(stmt, false, &Vec::new()) {
                    for import in imports {
                        // Convert import to a DynamicImport statement
                        ir_statements.push(IRStatement::DynamicImport {
                            target: import.alias.unwrap_or_else(|| import.module.clone()),
                            module_name: IRExpr::Const(IRConstant::String(import.module)),
                        });
                    }
                }
            }
            Stmt::ImportFrom(_) => {
                // Handle inline imports within functions
                if let Ok(imports) = process_import_from(stmt, false, &Vec::new()) {
                    for import in imports {
                        // Only handle simple from imports in functions
                        if import.is_star_import {
                            continue;
                        }
                        if let Some(name) = import.name {
                            let module_name = import.module;
                            let target = import.alias.unwrap_or_else(|| name.clone());

                            // Create qualified name
                            let qualified_name = format!("{module_name}.{name}");

                            ir_statements.push(IRStatement::DynamicImport {
                                target,
                                module_name: IRExpr::Const(IRConstant::String(qualified_name)),
                            });
                        }
                    }
                }
            }
            Stmt::Pass(_) => {
                // `pass` is a syntactic no-op (e.g. an @abstractmethod body).
            }
            _ => {
                return Err(anyhow!("Unsupported statement type: {stmt:?}"));
            }
        }
    }

    Ok(IRBody {
        statements: ir_statements,
    })
}

/// Check for dynamic imports in expressions
fn check_for_dynamic_import_expr(
    expr: &Expr,
    memory_layout: &mut MemoryLayout,
) -> Result<Option<IRStatement>> {
    if let Expr::Call(call) = expr {
        match &*call.func {
            Expr::Name(name) if name.id.to_string() == "__import__" => {
                // Handle direct __import__ call
                if !call.args.is_empty() {
                    let module_expr = lower_expr(&call.args[0], memory_layout)?;

                    // Check for assignment to this import
                    // For simplicity, we'll use a generic target name
                    return Ok(Some(IRStatement::DynamicImport {
                        target: "_dynamic_import".to_string(),
                        module_name: module_expr,
                    }));
                }
            }
            Expr::Attribute(attr) => {
                // Handle importlib.import_module
                if let Expr::Name(obj) = &*attr.value {
                    if obj.id.to_string() == "importlib"
                        && attr.attr.to_string() == "import_module"
                        && !call.args.is_empty()
                    {
                        let module_expr = lower_expr(&call.args[0], memory_layout)?;

                        // For simplicity, we'll use a generic target name
                        return Ok(Some(IRStatement::DynamicImport {
                            target: "_dynamic_import".to_string(),
                            module_name: module_expr,
                        }));
                    }
                }
            }
            _ => {}
        }
    }

    Ok(None)
}

/// Lower a Python expression into an IR expression
pub fn lower_expr(expr: &Expr, memory_layout: &mut MemoryLayout) -> Result<IRExpr> {
    match expr {
        Expr::BinOp(binop) => {
            let op = match &binop.op {
                rustpython_parser::ast::Operator::Add => IROp::Add,
                rustpython_parser::ast::Operator::Sub => IROp::Sub,
                rustpython_parser::ast::Operator::Mult => IROp::Mul,
                rustpython_parser::ast::Operator::Div => IROp::Div,
                rustpython_parser::ast::Operator::Mod => IROp::Mod,
                rustpython_parser::ast::Operator::FloorDiv => IROp::FloorDiv,
                rustpython_parser::ast::Operator::Pow => IROp::Pow,
                rustpython_parser::ast::Operator::MatMult => IROp::MatMul,
                rustpython_parser::ast::Operator::LShift => IROp::LShift,
                rustpython_parser::ast::Operator::RShift => IROp::RShift,
                rustpython_parser::ast::Operator::BitOr => IROp::BitOr,
                rustpython_parser::ast::Operator::BitXor => IROp::BitXor,
                rustpython_parser::ast::Operator::BitAnd => IROp::BitAnd,
            };

            // Optimize: compile-time string operations for constants
            if op == IROp::Add {
                if let (Expr::Constant(left_c), Expr::Constant(right_c)) =
                    (&*binop.left, &*binop.right)
                {
                    if let (
                        rustpython_parser::ast::Constant::Str(left_str),
                        rustpython_parser::ast::Constant::Str(right_str),
                    ) = (&left_c.value, &right_c.value)
                    {
                        // Compile-time string concatenation
                        let concat_str = format!("{left_str}{right_str}");
                        memory_layout.add_string(&concat_str);
                        return Ok(IRExpr::Const(IRConstant::String(concat_str)));
                    }
                }
            } else if op == IROp::Mod {
                // % formatting for strings
                if let Expr::Constant(left_c) = &*binop.left {
                    if let rustpython_parser::ast::Constant::Str(format_str) = &left_c.value {
                        // Try to extract arguments
                        let mut format_args = Vec::new();

                        // Handle single argument or tuple of arguments
                        match &*binop.right {
                            Expr::Constant(c) => {
                                // Single constant argument
                                match &c.value {
                                    rustpython_parser::ast::Constant::Str(s) => {
                                        format_args.push(s.clone())
                                    }
                                    rustpython_parser::ast::Constant::Int(i) => {
                                        format_args.push(i.to_string())
                                    }
                                    rustpython_parser::ast::Constant::Float(f) => {
                                        format_args.push(f.to_string())
                                    }
                                    rustpython_parser::ast::Constant::Bool(b) => {
                                        format_args.push(b.to_string())
                                    }
                                    _ => {}
                                }
                            }
                            Expr::Tuple(tuple) => {
                                // Tuple of arguments
                                for elt in &tuple.elts {
                                    if let Expr::Constant(c) = elt {
                                        match &c.value {
                                            rustpython_parser::ast::Constant::Str(s) => {
                                                format_args.push(s.clone())
                                            }
                                            rustpython_parser::ast::Constant::Int(i) => {
                                                format_args.push(i.to_string())
                                            }
                                            rustpython_parser::ast::Constant::Float(f) => {
                                                format_args.push(f.to_string())
                                            }
                                            rustpython_parser::ast::Constant::Bool(b) => {
                                                format_args.push(b.to_string())
                                            }
                                            _ => break,
                                        }
                                    } else {
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }

                        // If we extracted arguments successfully, process the format string
                        if !format_args.is_empty() {
                            if let Ok(result) = process_percent_format(format_str, &format_args) {
                                memory_layout.add_string(&result);
                                return Ok(IRExpr::Const(IRConstant::String(result)));
                            }
                        }
                    }
                }
            }

            Ok(IRExpr::BinaryOp {
                left: Box::new(lower_expr(&binop.left, memory_layout)?),
                right: Box::new(lower_expr(&binop.right, memory_layout)?),
                op,
            })
        }
        Expr::UnaryOp(unaryop) => {
            let op = match &unaryop.op {
                rustpython_parser::ast::UnaryOp::USub => IRUnaryOp::Neg,
                rustpython_parser::ast::UnaryOp::Not => IRUnaryOp::Not,
                rustpython_parser::ast::UnaryOp::Invert => IRUnaryOp::Invert,
                rustpython_parser::ast::UnaryOp::UAdd => IRUnaryOp::UAdd,
            };

            Ok(IRExpr::UnaryOp {
                operand: Box::new(lower_expr(&unaryop.operand, memory_layout)?),
                op,
            })
        }
        Expr::Compare(compare) => {
            if compare.ops.len() != 1 || compare.comparators.len() != 1 {
                return Err(anyhow!("Only single comparisons supported"));
            }

            let op = match &compare.ops[0] {
                rustpython_parser::ast::CmpOp::Eq => IRCompareOp::Eq,
                rustpython_parser::ast::CmpOp::NotEq => IRCompareOp::NotEq,
                rustpython_parser::ast::CmpOp::Lt => IRCompareOp::Lt,
                rustpython_parser::ast::CmpOp::LtE => IRCompareOp::LtE,
                rustpython_parser::ast::CmpOp::Gt => IRCompareOp::Gt,
                rustpython_parser::ast::CmpOp::GtE => IRCompareOp::GtE,
                rustpython_parser::ast::CmpOp::In => IRCompareOp::In,
                rustpython_parser::ast::CmpOp::NotIn => IRCompareOp::NotIn,
                rustpython_parser::ast::CmpOp::Is => IRCompareOp::Is,
                rustpython_parser::ast::CmpOp::IsNot => IRCompareOp::IsNot,
            };

            Ok(IRExpr::CompareOp {
                left: Box::new(lower_expr(&compare.left, memory_layout)?),
                right: Box::new(lower_expr(&compare.comparators[0], memory_layout)?),
                op,
            })
        }
        Expr::BoolOp(boolop) => {
            if boolop.values.len() != 2 {
                return Err(anyhow!("Only binary boolean operations supported"));
            }

            let op = match boolop.op {
                rustpython_parser::ast::BoolOp::And => IRBoolOp::And,
                rustpython_parser::ast::BoolOp::Or => IRBoolOp::Or,
            };

            Ok(IRExpr::BoolOp {
                left: Box::new(lower_expr(&boolop.values[0], memory_layout)?),
                right: Box::new(lower_expr(&boolop.values[1], memory_layout)?),
                op,
            })
        }
        Expr::Constant(c) => {
            match &c.value {
                rustpython_parser::ast::Constant::Int(i) => {
                    // Convert to i32 more safely
                    let i32_value = i
                        .to_string()
                        .parse::<i32>()
                        .context("Integer too large for i32")?;
                    Ok(IRExpr::Const(IRConstant::Int(i32_value)))
                }
                rustpython_parser::ast::Constant::Float(f) => {
                    Ok(IRExpr::Const(IRConstant::Float(*f)))
                }
                rustpython_parser::ast::Constant::Bool(b) => {
                    Ok(IRExpr::Const(IRConstant::Bool(*b)))
                }
                rustpython_parser::ast::Constant::Str(s) => {
                    // Register the string in memory layout
                    memory_layout.add_string(s);
                    Ok(IRExpr::Const(IRConstant::String(s.clone())))
                }
                rustpython_parser::ast::Constant::Bytes(b) => {
                    memory_layout.add_bytes(b);
                    Ok(IRExpr::Const(IRConstant::Bytes(b.clone())))
                }
                rustpython_parser::ast::Constant::None => Ok(IRExpr::Const(IRConstant::None)),
                rustpython_parser::ast::Constant::Tuple(items) => {
                    let mut tuple_items = Vec::new();
                    for item in items {
                        match item {
                            rustpython_parser::ast::Constant::Int(i) => {
                                let i32_value = i
                                    .to_string()
                                    .parse::<i32>()
                                    .context("Integer in tuple too large for i32")?;
                                tuple_items.push(IRConstant::Int(i32_value));
                            }
                            rustpython_parser::ast::Constant::Float(f) => {
                                tuple_items.push(IRConstant::Float(*f));
                            }
                            rustpython_parser::ast::Constant::Bool(b) => {
                                tuple_items.push(IRConstant::Bool(*b));
                            }
                            rustpython_parser::ast::Constant::Str(s) => {
                                memory_layout.add_string(s);
                                tuple_items.push(IRConstant::String(s.clone()));
                            }
                            rustpython_parser::ast::Constant::None => {
                                tuple_items.push(IRConstant::None);
                            }
                            _ => return Err(anyhow!("Unsupported constant type in tuple")),
                        }
                    }
                    Ok(IRExpr::Const(IRConstant::Tuple(tuple_items)))
                }
                _ => Err(anyhow!("Unsupported constant type")),
            }
        }
        Expr::Name(name) => Ok(IRExpr::Variable(name.id.to_string())),
        Expr::Call(call) => {
            // Check for dynamic imports
            match &*call.func {
                Expr::Name(name) if name.id.to_string() == "__import__" => {
                    // Dynamic import using __import__
                    if !call.args.is_empty() {
                        return Ok(IRExpr::DynamicImportExpr {
                            module_name: Box::new(lower_expr(&call.args[0], memory_layout)?),
                        });
                    }
                }
                Expr::Attribute(attr) => {
                    // Check for importlib.import_module
                    if let Expr::Name(obj) = &*attr.value {
                        if obj.id.to_string() == "importlib"
                            && attr.attr.to_string() == "import_module"
                            && !call.args.is_empty()
                        {
                            return Ok(IRExpr::DynamicImportExpr {
                                module_name: Box::new(lower_expr(&call.args[0], memory_layout)?),
                            });
                        }
                    }
                }
                _ => {}
            }

            // Regular function call
            match call.func.as_ref() {
                Expr::Name(name) => {
                    // Direct function call like func()
                    let function_name = name.id.to_string();

                    // Numeric conversions int()/float() must actually convert,
                    // so keep them as calls for the compiler to coerce. str()
                    // and bool() currently pass the value through unchanged.
                    if function_name == "int" || function_name == "float" {
                        if call.args.len() != 1 {
                            return Err(anyhow!(
                                "Type conversion function expects exactly one argument"
                            ));
                        }
                        let arg = lower_expr(&call.args[0], memory_layout)?;
                        return Ok(IRExpr::FunctionCall {
                            function_name,
                            arguments: vec![arg],
                        });
                    }
                    let type_conversions = ["str", "bool"];
                    if type_conversions.contains(&function_name.as_str()) {
                        if call.args.len() != 1 {
                            return Err(anyhow!(
                                "Type conversion function expects exactly one argument"
                            ));
                        }
                        return lower_expr(&call.args[0], memory_layout);
                    }

                    // range() function
                    if function_name == "range" {
                        match call.args.len() {
                            1 => {
                                // range(stop)
                                return Ok(IRExpr::RangeCall {
                                    start: None,
                                    stop: Box::new(lower_expr(&call.args[0], memory_layout)?),
                                    step: None,
                                });
                            }
                            2 => {
                                // range(start, stop)
                                return Ok(IRExpr::RangeCall {
                                    start: Some(Box::new(lower_expr(
                                        &call.args[0],
                                        memory_layout,
                                    )?)),
                                    stop: Box::new(lower_expr(&call.args[1], memory_layout)?),
                                    step: None,
                                });
                            }
                            3 => {
                                // range(start, stop, step)
                                return Ok(IRExpr::RangeCall {
                                    start: Some(Box::new(lower_expr(
                                        &call.args[0],
                                        memory_layout,
                                    )?)),
                                    stop: Box::new(lower_expr(&call.args[1], memory_layout)?),
                                    step: Some(Box::new(lower_expr(&call.args[2], memory_layout)?)),
                                });
                            }
                            _ => {
                                return Err(anyhow!("range() takes 1 to 3 positional arguments"));
                            }
                        }
                    }

                    // namedtuple() function - returns a class factory
                    if function_name == "namedtuple" {
                        // namedtuple(typename, field_names) -> class
                        // For simplicity, we treat it as a function call
                        // The arguments are (typename: str, field_names: str or list)
                        // It returns a callable that creates instances
                        if call.args.is_empty() {
                            return Err(anyhow!("namedtuple() requires at least 1 argument"));
                        }
                        // Process arguments
                        let mut arguments = Vec::new();
                        for arg in &call.args {
                            arguments.push(lower_expr(arg, memory_layout)?);
                        }
                        // Return as a function call - at runtime it will be a callable
                        return Ok(IRExpr::FunctionCall {
                            function_name: "namedtuple".to_string(),
                            arguments,
                        });
                    }

                    let mut arguments = Vec::new();
                    for arg in &call.args {
                        arguments.push(lower_expr(arg, memory_layout)?);
                    }

                    Ok(IRExpr::FunctionCall {
                        function_name,
                        arguments,
                    })
                }
                Expr::Attribute(attr) => {
                    // Method call like obj.method()
                    let method_name = attr.attr.to_string();
                    let mut arguments = Vec::new();
                    for arg in &call.args {
                        arguments.push(lower_expr(arg, memory_layout)?);
                    }

                    // Compile-time optimization for string method calls on constants
                    if let Expr::Constant(const_expr) = &*attr.value {
                        if let rustpython_parser::ast::Constant::Str(s) = &const_expr.value {
                            // Optimize string methods on constants
                            match method_name.as_str() {
                                // Transforming methods
                                "upper" => {
                                    let result = s.to_uppercase();
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                "lower" => {
                                    let result = s.to_lowercase();
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                "capitalize" => {
                                    let mut chars = s.chars();
                                    let result = match chars.next() {
                                        None => String::new(),
                                        Some(first) => {
                                            first.to_uppercase().collect::<String>()
                                                + &chars.collect::<String>().to_lowercase()
                                        }
                                    };
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                "title" => {
                                    let mut result = String::new();
                                    let mut capitalize_next = true;
                                    for c in s.chars() {
                                        if c.is_whitespace() {
                                            result.push(c);
                                            capitalize_next = true;
                                        } else if capitalize_next {
                                            result.push_str(&c.to_uppercase().to_string());
                                            capitalize_next = false;
                                        } else {
                                            result.push_str(&c.to_lowercase().to_string());
                                        }
                                    }
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                "strip" => {
                                    let result = s.trim().to_string();
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                "lstrip" => {
                                    let result = s.trim_start().to_string();
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                "rstrip" => {
                                    let result = s.trim_end().to_string();
                                    memory_layout.add_string(&result);
                                    return Ok(IRExpr::Const(IRConstant::String(result)));
                                }
                                // Non-transforming (test) methods - return boolean as constant
                                "isdigit" => {
                                    let result =
                                        !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
                                    return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                }
                                "isalpha" => {
                                    let result =
                                        !s.is_empty() && s.chars().all(|c| c.is_alphabetic());
                                    return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                }
                                "isalnum" => {
                                    let result =
                                        !s.is_empty() && s.chars().all(|c| c.is_alphanumeric());
                                    return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                }
                                "isspace" => {
                                    let result =
                                        !s.is_empty() && s.chars().all(|c| c.is_whitespace());
                                    return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                }
                                "isupper" => {
                                    let result = !s.is_empty()
                                        && s.chars()
                                            .filter(|c| c.is_alphabetic())
                                            .all(|c| c.is_uppercase());
                                    return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                }
                                "islower" => {
                                    let result = !s.is_empty()
                                        && s.chars()
                                            .filter(|c| c.is_alphabetic())
                                            .all(|c| c.is_lowercase());
                                    return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                }
                                "split" => {
                                    // split(sep) - compile-time for constant separator
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(sep)) =
                                            &arguments[0]
                                        {
                                            let parts: Vec<&str> = s.split(sep.as_str()).collect();
                                            // For now, convert to simple string representation for constants
                                            // Real list support would require list IR representation
                                            let result = format!(
                                                "[{}]",
                                                parts
                                                    .iter()
                                                    .map(|p| format!("'{p}'"))
                                                    .collect::<Vec<_>>()
                                                    .join(", ")
                                            );
                                            memory_layout.add_string(&result);
                                            return Ok(IRExpr::Const(IRConstant::String(result)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "find" => {
                                    // find(sub) - return index of substring
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(sub)) =
                                            &arguments[0]
                                        {
                                            let index = s
                                                .find(sub.as_str())
                                                .map(|i| i as i32)
                                                .unwrap_or(-1);
                                            return Ok(IRExpr::Const(IRConstant::Int(index)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "index" => {
                                    // index(sub) - like find but raises on not found
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(sub)) =
                                            &arguments[0]
                                        {
                                            let index =
                                                s.find(sub.as_str()).map(|i| i as i32).unwrap_or(0);
                                            return Ok(IRExpr::Const(IRConstant::Int(index)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "count" => {
                                    // count(sub) - count occurrences
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(sub)) =
                                            &arguments[0]
                                        {
                                            if !sub.is_empty() {
                                                let count = s.matches(sub.as_str()).count() as i32;
                                                return Ok(IRExpr::Const(IRConstant::Int(count)));
                                            }
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "startswith" => {
                                    // startswith(prefix) - check if starts with
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(prefix)) =
                                            &arguments[0]
                                        {
                                            let result = s.starts_with(prefix.as_str());
                                            return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "endswith" => {
                                    // endswith(suffix) - check if ends with
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::String(suffix)) =
                                            &arguments[0]
                                        {
                                            let result = s.ends_with(suffix.as_str());
                                            return Ok(IRExpr::Const(IRConstant::Bool(result)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "replace" => {
                                    // replace(old, new) - replace first occurrence
                                    if arguments.len() >= 2 {
                                        if let (
                                            IRExpr::Const(IRConstant::String(old)),
                                            IRExpr::Const(IRConstant::String(new)),
                                        ) = (&arguments[0], &arguments[1])
                                        {
                                            let result = s.replacen(old.as_str(), new.as_str(), 1);
                                            memory_layout.add_string(&result);
                                            return Ok(IRExpr::Const(IRConstant::String(result)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "join" => {
                                    // join(iterable) - join list items with separator
                                    // For compile-time, would need list literal support
                                    // Fall through to runtime handling
                                }
                                "ljust" | "rjust" | "center" => {
                                    // Justify methods - width, fillchar
                                    if !arguments.is_empty() {
                                        if let IRExpr::Const(IRConstant::Int(width)) = &arguments[0]
                                        {
                                            let w = *width as usize;
                                            let result = match method_name.as_str() {
                                                "ljust" => format!("{s:<w$}"),
                                                "rjust" => format!("{s:>w$}"),
                                                "center" => {
                                                    let padding =
                                                        if w > s.len() { w - s.len() } else { 0 };
                                                    let left = padding / 2;
                                                    let right = padding - left;
                                                    format!(
                                                        "{}{}{}",
                                                        " ".repeat(left),
                                                        s,
                                                        " ".repeat(right)
                                                    )
                                                }
                                                _ => s.to_string(),
                                            };
                                            memory_layout.add_string(&result);
                                            return Ok(IRExpr::Const(IRConstant::String(result)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                "format" => {
                                    // Compile-time format string processing
                                    let mut format_args = Vec::new();
                                    for arg in &arguments {
                                        // Try to extract constant string values
                                        if let IRExpr::Const(IRConstant::String(arg_str)) = arg {
                                            format_args.push(arg_str.clone());
                                        } else if let IRExpr::Const(IRConstant::Int(i)) = arg {
                                            format_args.push(i.to_string());
                                        } else if let IRExpr::Const(IRConstant::Float(f)) = arg {
                                            format_args.push(f.to_string());
                                        } else if let IRExpr::Const(IRConstant::Bool(b)) = arg {
                                            format_args.push(b.to_string());
                                        } else {
                                            // Non-constant argument, can't optimize
                                            break;
                                        }
                                    }

                                    // If all arguments are constants, process format string
                                    if format_args.len() == arguments.len() {
                                        if let Ok(result) = process_format_string(s, &format_args) {
                                            memory_layout.add_string(&result);
                                            return Ok(IRExpr::Const(IRConstant::String(result)));
                                        }
                                    }
                                    // Fall through to runtime handling
                                }
                                // Default: no optimization
                                _ => {}
                            };
                        }
                    }

                    let object = Box::new(lower_expr(&attr.value, memory_layout)?);

                    Ok(IRExpr::MethodCall {
                        object,
                        method_name,
                        arguments,
                    })
                }
                _ => Err(anyhow!("Unsupported function call type")),
            }
        }
        Expr::List(list) => {
            let mut elements = Vec::new();
            for item in &list.elts {
                elements.push(lower_expr(item, memory_layout)?);
            }
            Ok(IRExpr::ListLiteral(elements))
        }
        Expr::Set(set) => {
            let mut elements = Vec::new();
            for item in &set.elts {
                elements.push(lower_expr(item, memory_layout)?);
            }
            Ok(IRExpr::SetLiteral(elements))
        }
        Expr::Tuple(tuple) => {
            let mut elements = Vec::new();
            for item in &tuple.elts {
                elements.push(lower_expr(item, memory_layout)?);
            }
            Ok(IRExpr::TupleLiteral(elements))
        }
        Expr::Dict(dict) => {
            let mut pairs = Vec::new();
            for (key, value) in dict.keys.iter().zip(dict.values.iter()) {
                if let Some(key) = key {
                    pairs.push((
                        lower_expr(key, memory_layout)?,
                        lower_expr(value, memory_layout)?,
                    ));
                }
            }
            Ok(IRExpr::DictLiteral(pairs))
        }
        Expr::Subscript(subscript) => {
            // A plain `x[a:b:c]` parses as an `Expr::Slice` (lower/upper/step),
            // distinct from the extended `Expr::Tuple` form handled below.
            if let Expr::Slice(slice_expr) = &*subscript.slice {
                let lower = slice_expr
                    .lower
                    .as_ref()
                    .map(|e| lower_expr(e, memory_layout))
                    .transpose()?
                    .map(Box::new);
                let upper = slice_expr
                    .upper
                    .as_ref()
                    .map(|e| lower_expr(e, memory_layout))
                    .transpose()?
                    .map(Box::new);
                let step = slice_expr
                    .step
                    .as_ref()
                    .map(|e| lower_expr(e, memory_layout))
                    .transpose()?
                    .map(Box::new);
                return Ok(IRExpr::Slicing {
                    container: Box::new(lower_expr(&subscript.value, memory_layout)?),
                    start: lower,
                    end: upper,
                    step,
                });
            }

            // Check if this is a slice expression (start:end:step)
            // In rustpython, slices are represented as Tuple expressions with None for missing bounds
            if let Expr::Tuple(tuple_expr) = &*subscript.slice {
                if tuple_expr.elts.len() >= 2 {
                    // This looks like a slice (start:end) or (start:end:step)
                    let start = if matches!(&tuple_expr.elts[0], Expr::Constant(c) if matches!(c.value, rustpython_parser::ast::Constant::None))
                    {
                        None
                    } else {
                        Some(Box::new(lower_expr(&tuple_expr.elts[0], memory_layout)?))
                    };

                    let end = if matches!(&tuple_expr.elts[1], Expr::Constant(c) if matches!(c.value, rustpython_parser::ast::Constant::None))
                    {
                        None
                    } else {
                        Some(Box::new(lower_expr(&tuple_expr.elts[1], memory_layout)?))
                    };

                    let step = if tuple_expr.elts.len() >= 3 {
                        if matches!(&tuple_expr.elts[2], Expr::Constant(c) if matches!(c.value, rustpython_parser::ast::Constant::None))
                        {
                            None
                        } else {
                            Some(Box::new(lower_expr(&tuple_expr.elts[2], memory_layout)?))
                        }
                    } else {
                        None
                    };

                    return Ok(IRExpr::Slicing {
                        container: Box::new(lower_expr(&subscript.value, memory_layout)?),
                        start,
                        end,
                        step,
                    });
                }
            }

            // Otherwise, it's a regular indexing operation
            Ok(IRExpr::Indexing {
                container: Box::new(lower_expr(&subscript.value, memory_layout)?),
                index: Box::new(lower_expr(&subscript.slice, memory_layout)?),
            })
        }
        Expr::Attribute(attr) => Ok(IRExpr::Attribute {
            object: Box::new(lower_expr(&attr.value, memory_layout)?),
            attribute: attr.attr.to_string(),
        }),
        Expr::ListComp(comp) => {
            // List comprehension support: [expr for var in iterable if condition]
            if comp.generators.len() != 1 {
                return Err(anyhow!(
                    "Only single generator list comprehensions supported"
                ));
            }

            let generator = &comp.generators[0];

            // Get target name (only simple variable targets for now)
            let var_name = match &generator.target {
                Expr::Name(name) => name.id.to_string(),
                _ => {
                    return Err(anyhow!(
                        "Only simple variable targets supported in list comprehensions"
                    ))
                }
            };

            // TODO: Handle filter conditions if present (for now, ignore them)
            // Filters would require runtime conditional evaluation

            Ok(IRExpr::ListComp {
                expr: Box::new(lower_expr(&comp.elt, memory_layout)?),
                var_name,
                iterable: Box::new(lower_expr(&generator.iter, memory_layout)?),
            })
        }
        Expr::JoinedStr(joined_str) => {
            // F-string support: f"string {expr} more"
            // JoinedStr contains a list of values: some are Constant strings, some are FormattedValues (expressions)
            let mut parts = Vec::new();
            let mut combined_string = String::new();
            let mut has_variables = false;

            for value in &joined_str.values {
                match value {
                    Expr::Constant(const_expr) => {
                        // Plain string part
                        if let rustpython_parser::ast::Constant::Str(s) = &const_expr.value {
                            combined_string.push_str(s);
                        }
                    }
                    Expr::FormattedValue(fv) => {
                        // Variable/expression part like {expr}
                        has_variables = true;

                        // If we have accumulated string, add it
                        if !combined_string.is_empty() {
                            parts.push(IRExpr::Const(IRConstant::String(combined_string.clone())));
                            combined_string.clear();
                        }

                        // Add the formatted value (convert to string)
                        let expr_ir = lower_expr(&fv.value, memory_layout)?;
                        parts.push(expr_ir);
                    }
                    _ => {
                        return Err(anyhow!("Unsupported element in f-string"));
                    }
                }
            }

            // Add any remaining string
            if !combined_string.is_empty() {
                parts.push(IRExpr::Const(IRConstant::String(combined_string)));
            }

            // If no variables, return as single string constant
            if !has_variables {
                memory_layout.add_string(
                    &parts
                        .iter()
                        .filter_map(|p| {
                            if let IRExpr::Const(IRConstant::String(s)) = p {
                                Some(s.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                );

                let combined = parts
                    .into_iter()
                    .filter_map(|p| {
                        if let IRExpr::Const(IRConstant::String(s)) = p {
                            Some(s)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                return Ok(IRExpr::Const(IRConstant::String(combined)));
            }

            // If all parts are constants, we can optimize
            let mut all_const = true;
            let mut const_parts = Vec::new();

            for part in &parts {
                match part {
                    IRExpr::Const(IRConstant::String(s)) => {
                        const_parts.push(s.clone());
                    }
                    IRExpr::Const(IRConstant::Int(i)) => {
                        const_parts.push(i.to_string());
                    }
                    IRExpr::Const(IRConstant::Float(f)) => {
                        const_parts.push(f.to_string());
                    }
                    IRExpr::Const(IRConstant::Bool(b)) => {
                        const_parts.push(b.to_string());
                    }
                    _ => {
                        all_const = false;
                        break;
                    }
                }
            }

            if all_const {
                let result = const_parts.join("");
                memory_layout.add_string(&result);
                return Ok(IRExpr::Const(IRConstant::String(result)));
            }

            // For dynamic f-strings, concatenate parts at runtime
            // We'll return a list of parts to be joined at runtime
            // For now, return the first part or a placeholder
            if !parts.is_empty() {
                Ok(parts.into_iter().next().unwrap())
            } else {
                Ok(IRExpr::Const(IRConstant::String(String::new())))
            }
        }
        Expr::Lambda(lambda) => {
            let params = process_function_params(&lambda.args, memory_layout)?;
            let body = Box::new(lower_expr(&lambda.body, memory_layout)?);

            // TODO: Analyze body to detect captured variables from outer scope
            // For now, assume no captured variables (would require full scope analysis)
            let captured_vars = Vec::new();

            Ok(IRExpr::Lambda {
                params,
                body,
                captured_vars,
            })
        }
        _ => Err(anyhow!("Unsupported expression type: {expr:?}")),
    }
}

/// Simple format string processor for basic placeholders
/// Handles {}, {0}, {1}, {name}, etc.
pub fn process_format_string(format_str: &str, args: &[String]) -> Result<String> {
    let mut result = String::new();
    let mut chars = format_str.chars().peekable();
    let mut arg_index = 0;

    while let Some(ch) = chars.next() {
        if ch == '{' {
            if chars.peek() == Some(&'{') {
                // Escaped brace {{
                chars.next();
                result.push('{');
            } else {
                // Placeholder {, {0}, {name}, etc.
                let mut placeholder = String::new();
                while let Some(&next_ch) = chars.peek() {
                    if next_ch == '}' {
                        chars.next();
                        break;
                    }
                    placeholder.push(chars.next().unwrap());
                }

                // Process placeholder
                if placeholder.is_empty() {
                    // {} - use positional args
                    if arg_index < args.len() {
                        result.push_str(&args[arg_index]);
                        arg_index += 1;
                    }
                } else if let Ok(idx) = placeholder.parse::<usize>() {
                    // {0}, {1}, etc.
                    if idx < args.len() {
                        result.push_str(&args[idx]);
                    }
                } else {
                    // {name} - named args not supported yet, just leave placeholder
                    result.push('{');
                    result.push_str(&placeholder);
                    result.push('}');
                }
            }
        } else if ch == '}' {
            if chars.peek() == Some(&'}') {
                // Escaped brace }}
                chars.next();
                result.push('}');
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}

/// Simple % formatter for basic placeholders
/// Handles %s, %d, %f, %%, etc.
pub fn process_percent_format(format_str: &str, args: &[String]) -> Result<String> {
    let mut result = String::new();
    let mut chars = format_str.chars().peekable();
    let mut arg_index = 0;

    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(&next_ch) = chars.peek() {
                match next_ch {
                    '%' => {
                        chars.next();
                        result.push('%');
                    }
                    's' | 'd' | 'f' | 'x' | 'o' => {
                        chars.next();
                        if arg_index < args.len() {
                            result.push_str(&args[arg_index]);
                            arg_index += 1;
                        }
                    }
                    _ => {
                        result.push(ch);
                    }
                }
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    Ok(result)
}
