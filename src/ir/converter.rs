use crate::ir::types::*;
use anyhow::{anyhow, Context, Result};
use rustpython_parser::ast::{ArgWithDefault, Arguments, Expr, Stmt, Suite};

/// Lower a Python AST (Suite) into our IR.
pub fn lower_ast_to_ir(ast: &Suite) -> Result<IRModule> {
    let mut module = IRModule::new();
    let mut memory_layout = MemoryLayout::new();

    for stmt in ast {
        match stmt {
            Stmt::FunctionDef(fundef) => {
                // Process function definition
                let name = fundef.name.to_string();
                let params = process_function_params(&fundef.args)?;
                let return_type = if let Some(returns) = &fundef.returns {
                    type_annotation_to_ir_type(returns)?
                } else {
                    IRType::Unknown
                };
                
                // Extract decorators if any
                let decorators = fundef.decorator_list
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
                let imports = process_import(stmt)?;
                module.imports.extend(imports);
            }
            Stmt::ImportFrom(_) => {
                // Process from import
                let imports = process_import_from(stmt)?;
                module.imports.extend(imports);
            }
            Stmt::Expr(_) => {
                // Skip module-level expressions like docstrings
                continue;
            }
            _ => {
                // Ignore other module-level statements for now
                // But don't error out so we can compile more files
                continue;
            }
        }
    }

    Ok(module)
}

/// Process a class definition
fn process_class_definition(stmt: &Stmt, memory_layout: &mut MemoryLayout) -> Result<IRClass> {
    if let Stmt::ClassDef(classdef) = stmt {
        let name = classdef.name.to_string();
        
        // Extract base classes
        let bases = classdef.bases
            .iter()
            .filter_map(|base| {
                if let Expr::Name(name) = base {
                    Some(name.id.to_string())
                } else {
                    None
                }
            })
            .collect();
        
        let mut methods = Vec::new();
        let mut class_vars = Vec::new();
        
        // Process class body
        for stmt in &classdef.body {
            match stmt {
                Stmt::FunctionDef(method_def) => {
                    // Process method (similar to function but with 'self' parameter)
                    let method_name = method_def.name.to_string();
                    let params = process_function_params(&method_def.args)?;
                    let return_type = if let Some(returns) = &method_def.returns {
                        type_annotation_to_ir_type(returns)?
                    } else {
                        IRType::Unknown
                    };
                    
                    let decorators = method_def.decorator_list
                        .iter()
                        .filter_map(|dec| {
                            if let Expr::Name(name) = dec {
                                Some(name.id.to_string())
                            } else {
                                None
                            }
                        })
                        .collect();
                    
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

/// Process a module-level assignment
fn process_module_level_assign(stmt: &Stmt, memory_layout: &mut MemoryLayout) -> Result<Option<IRVariable>> {
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
fn process_module_level_ann_assign(stmt: &Stmt, memory_layout: &mut MemoryLayout) -> Result<Option<IRVariable>> {
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
                IRType::List(_) => IRExpr::ListLiteral(Vec::new()),
                IRType::Dict(_, _) => IRExpr::DictLiteral(Vec::new()),
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
fn process_import(stmt: &Stmt) -> Result<Vec<IRImport>> {
    if let Stmt::Import(import) = stmt {
        let mut imports = Vec::new();
        
        for alias in &import.names {
            let module = alias.name.to_string();
            let alias = alias.asname.as_ref().map(|a| a.to_string());
            
            imports.push(IRImport {
                module,
                name: None,
                alias,
                is_from_import: false,
            });
        }
        
        Ok(imports)
    } else {
        Ok(Vec::new())
    }
}

/// Process a from-import statement
fn process_import_from(stmt: &Stmt) -> Result<Vec<IRImport>> {
    if let Stmt::ImportFrom(import_from) = stmt {
        let mut imports = Vec::new();
        
        let module = match &import_from.module {
            Some(module) => module.to_string(),
            None => return Ok(imports), // Skip relative imports for now
        };
        
        for alias in &import_from.names {
            let name = alias.name.to_string();
            let alias = alias.asname.as_ref().map(|a| a.to_string());
            
            imports.push(IRImport {
                module: module.clone(),
                name: Some(name),
                alias,
                is_from_import: true,
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
        Expr::Name(name) => {
            match name.id.as_str() {
                "int" => Ok(IRType::Int),
                "float" => Ok(IRType::Float),
                "bool" => Ok(IRType::Bool),
                "str" => Ok(IRType::String),
                "None" => Ok(IRType::None),
                "Any" => Ok(IRType::Any),
                _ => Ok(IRType::Class(name.id.to_string())),
            }
        }
        Expr::Subscript(subscript) => {
            // Handle generic types like List[int]
            if let Expr::Name(container) = &*subscript.value {
                match container.id.as_str() {
                    "List" | "list" => {
                        let element_type = type_annotation_to_ir_type(&subscript.slice)?;
                        Ok(IRType::List(Box::new(element_type)))
                    }
                    "Dict" | "dict" => {
                        if let Expr::Tuple(tuple) = &*subscript.slice {
                            if tuple.elts.len() == 2 {
                                let key_type = type_annotation_to_ir_type(&tuple.elts[0])?;
                                let value_type = type_annotation_to_ir_type(&tuple.elts[1])?;
                                Ok(IRType::Dict(Box::new(key_type), Box::new(value_type)))
                            } else {
                                Err(anyhow!("Dict type annotation should have exactly 2 elements"))
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
                            Ok(IRType::Tuple(vec![type_annotation_to_ir_type(&subscript.slice)?]))
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

/// Process function parameters with possible type annotations
fn process_function_params(args: &Arguments) -> Result<Vec<IRParam>> {
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
                let mut memory_layout = MemoryLayout::new();
                Some(lower_expr(default, &mut memory_layout)?)
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
                    _ => return Err(anyhow!("Only variable or attribute assignment supported")),
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
                        
                        ir_statements.push(IRStatement::AugAssign {
                            target,
                            value,
                            op,
                        });
                    },
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
                    },
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
            Stmt::While(while_stmt) => {
                let condition = lower_expr(&while_stmt.test, memory_layout)?;
                let body = Box::new(lower_function_body(&while_stmt.body, memory_layout)?);

                ir_statements.push(IRStatement::While { condition, body });
            }
            Stmt::Expr(expr_stmt) => {
                let expr = lower_expr(&expr_stmt.value, memory_layout)?;
                ir_statements.push(IRStatement::Expression(expr));
            }
            Stmt::For(for_stmt) => {
                // Handle for loops (only simple variable target for now)
                let target = match &*for_stmt.target {
                    Expr::Name(name) => name.id.to_string(),
                    _ => return Err(anyhow!("Only simple variable targets supported in for loops")),
                };
                
                let iterable = lower_expr(&for_stmt.iter, memory_layout)?;
                let body = Box::new(lower_function_body(&for_stmt.body, memory_layout)?);
                let else_body = if !for_stmt.orelse.is_empty() {
                    Some(Box::new(lower_function_body(&for_stmt.orelse, memory_layout)?))
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
                for _handler in &try_stmt.handlers {
                    // Since we don't know the exact structure of ExceptHandler in this version
                    // of rustpython_parser, we'll create a minimal handler with default values
                    
                    // Default values for exception type and name
                    let exception_type = None;
                    let name = None;
                    
                    // Create an empty body since we can't access the actual handler body
                    let body = IRBody { statements: Vec::new() };
                    
                    except_handlers.push(IRExceptHandler {
                        exception_type,
                        name,
                        body,
                    });
                }
                
                let finally_body = if !try_stmt.finalbody.is_empty() {
                    Some(Box::new(lower_function_body(&try_stmt.finalbody, memory_layout)?))
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
            _ => {
                return Err(anyhow!("Unsupported statement type: {:?}", stmt));
            }
        }
    }

    Ok(IRBody {
        statements: ir_statements,
    })
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
                rustpython_parser::ast::Constant::None => Ok(IRExpr::Const(IRConstant::None)),
                rustpython_parser::ast::Constant::Tuple(items) => {
                    let mut tuple_items = Vec::new();
                    for item in items {
                        match item {
                            rustpython_parser::ast::Constant::Int(i) => {
                                let i32_value = i.to_string().parse::<i32>()
                                    .context("Integer in tuple too large for i32")?;
                                tuple_items.push(IRConstant::Int(i32_value));
                            },
                            rustpython_parser::ast::Constant::Float(f) => {
                                tuple_items.push(IRConstant::Float(*f));
                            },
                            rustpython_parser::ast::Constant::Bool(b) => {
                                tuple_items.push(IRConstant::Bool(*b));
                            },
                            rustpython_parser::ast::Constant::Str(s) => {
                                memory_layout.add_string(s);
                                tuple_items.push(IRConstant::String(s.clone()));
                            },
                            rustpython_parser::ast::Constant::None => {
                                tuple_items.push(IRConstant::None);
                            },
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
            match call.func.as_ref() {
                Expr::Name(name) => {
                    // Direct function call like func()
                    let function_name = name.id.to_string();
                    
                    // Type conversion functions like int, float, str
                    let type_conversions = ["int", "float", "str", "bool"];
                    if type_conversions.contains(&function_name.as_str()) {
                        if call.args.len() != 1 {
                            return Err(anyhow!(
                                "Type conversion function expects exactly one argument"
                            ));
                        }
                        return lower_expr(&call.args[0], memory_layout);
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
                    let object = Box::new(lower_expr(&attr.value, memory_layout)?);
                    let method_name = attr.attr.to_string();
                    
                    let mut arguments = Vec::new();
                    for arg in &call.args {
                        arguments.push(lower_expr(arg, memory_layout)?);
                    }
                    
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
        Expr::Subscript(subscript) => Ok(IRExpr::Indexing {
            container: Box::new(lower_expr(&subscript.value, memory_layout)?),
            index: Box::new(lower_expr(&subscript.slice, memory_layout)?),
        }),
        Expr::Attribute(attr) => Ok(IRExpr::Attribute {
            object: Box::new(lower_expr(&attr.value, memory_layout)?),
            attribute: attr.attr.to_string(),
        }),
        Expr::ListComp(comp) => {
            // Basic list comprehension support
            if comp.generators.len() != 1 {
                return Err(anyhow!("Only single generator list comprehensions supported"));
            }
            
            let generator = &comp.generators[0];
            if generator.ifs.len() > 0 {
                return Err(anyhow!("List comprehension filters not supported yet"));
            }
            
            // Get target name (only simple variable targets for now)
            let var_name = match &generator.target {
                Expr::Name(name) => name.id.to_string(),
                _ => return Err(anyhow!("Only simple variable targets supported in list comprehensions")),
            };
            
            Ok(IRExpr::ListComp {
                expr: Box::new(lower_expr(&comp.elt, memory_layout)?),
                var_name,
                iterable: Box::new(lower_expr(&generator.iter, memory_layout)?),
            })
        }
        _ => Err(anyhow!("Unsupported expression type: {:?}", expr)),
    }
}
