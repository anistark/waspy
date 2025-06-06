use crate::ir::{IRBody, IRConstant, IRExpr, IRFunction, IRStatement, IRType};
use std::collections::HashMap;

/// Types of built-in decorators
#[derive(Debug, Clone, PartialEq)]
pub enum DecoratorType {
    /// Memoization decorator that caches function results
    Memoize,
    /// Logging decorator that logs function calls
    Debug,
    /// Timing decorator that measures function execution time
    Timer,
    /// Default value decorator to provide defaults for parameters
    DefaultValue,
    /// Type checking decorator to verify parameter types at runtime
    TypeCheck,
    /// Pure function decorator (no side effects, only depends on inputs)
    Pure,
    /// Custom decorator with user-defined behavior
    Custom(String),
}

impl From<String> for DecoratorType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "memoize" => DecoratorType::Memoize,
            "debug" => DecoratorType::Debug,
            "timer" => DecoratorType::Timer,
            "default_value" => DecoratorType::DefaultValue,
            "type_check" => DecoratorType::TypeCheck,
            "pure" => DecoratorType::Pure,
            _ => DecoratorType::Custom(s),
        }
    }
}

/// Decorator registry for managing and applying function decorators
pub struct DecoratorRegistry {
    /// Map of custom decorator names to their implementations
    custom_decorators: HashMap<String, Box<dyn Fn(IRFunction) -> IRFunction>>,
}

impl Default for DecoratorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoratorRegistry {
    pub fn new() -> Self {
        DecoratorRegistry {
            custom_decorators: HashMap::new(),
        }
    }

    /// Register a custom decorator
    pub fn register<F>(&mut self, name: &str, decorator: F)
    where
        F: Fn(IRFunction) -> IRFunction + 'static,
    {
        self.custom_decorators
            .insert(name.to_string(), Box::new(decorator));
    }

    /// Apply decorators to a function
    pub fn apply_decorators(&self, mut func: IRFunction) -> IRFunction {
        let decorator_names: Vec<String> = func.decorators.clone();
        for decorator_name in decorator_names.iter().rev() {
            let decorator_type = DecoratorType::from(decorator_name.clone());
            func = self.apply_decorator(decorator_type, func);
        }
        func.decorators.clear();
        func
    }

    /// Apply a specific decorator to a function
    fn apply_decorator(&self, decorator_type: DecoratorType, func: IRFunction) -> IRFunction {
        match decorator_type {
            DecoratorType::Memoize => self.apply_memoize_decorator(func),
            DecoratorType::Debug => self.apply_debug_decorator(func),
            DecoratorType::Timer => self.apply_timer_decorator(func),
            DecoratorType::DefaultValue => self.apply_default_value_decorator(func),
            DecoratorType::TypeCheck => self.apply_type_check_decorator(func),
            DecoratorType::Pure => self.apply_pure_decorator(func),
            DecoratorType::Custom(name) => self.apply_custom_decorator(&name, func),
        }
    }

    /// Apply the memoize decorator to cache function results
    fn apply_memoize_decorator(&self, func: IRFunction) -> IRFunction {
        // For simplicity, we'll just handle integer parameters for now
        // In a full implementation, you'd need more complex caching logic

        // Create a new function with memoization wrapper
        let mut memoized_func = IRFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            decorators: Vec::new(), // Clear decorators as they've been applied
            body: IRBody {
                statements: Vec::new(),
            },
        };

        // Create cache initialization at the start
        // This is simplified - in reality, you'd need proper WebAssembly memory management
        // For now, we'll just use local variables to simulate a simple cache

        // Add a cache check at the beginning
        let cache_check = IRStatement::If {
            // Simple condition for cache hit (would be more complex in reality)
            condition: IRExpr::BoolOp {
                left: Box::new(IRExpr::Const(IRConstant::Bool(false))),
                right: Box::new(IRExpr::Const(IRConstant::Bool(false))),
                op: crate::ir::IRBoolOp::Or,
            },
            then_body: Box::new(IRBody {
                statements: vec![
                    // Return cached value
                    IRStatement::Return(Some(IRExpr::Variable("_cached_result".to_string()))),
                ],
            }),
            else_body: None,
        };

        // Add the cache check to the beginning
        memoized_func.body.statements.push(cache_check);

        // Add the original function body
        for stmt in &func.body.statements {
            // For Return statements, store in cache before returning
            if let IRStatement::Return(Some(expr)) = stmt {
                // Store result in cache
                memoized_func.body.statements.push(IRStatement::Assign {
                    target: "_cached_result".to_string(),
                    value: expr.clone(),
                    var_type: Some(func.return_type.clone()),
                });

                // Return the stored result
                memoized_func
                    .body
                    .statements
                    .push(IRStatement::Return(Some(IRExpr::Variable(
                        "_cached_result".to_string(),
                    ))));
            } else {
                memoized_func.body.statements.push(stmt.clone());
            }
        }

        memoized_func
    }

    /// Apply the debug decorator to log function calls
    fn apply_debug_decorator(&self, func: IRFunction) -> IRFunction {
        // Create a new function with debug wrapper
        let mut debug_func = IRFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            decorators: Vec::new(),
            body: IRBody {
                statements: Vec::new(),
            },
        };

        // Add debug log at entry
        debug_func
            .body
            .statements
            .push(IRStatement::Expression(IRExpr::FunctionCall {
                function_name: "print".to_string(),
                arguments: vec![IRExpr::Const(IRConstant::String(format!(
                    "Entering function: {}",
                    func.name
                )))],
            }));

        // Add the original function body
        for stmt in &func.body.statements {
            if let IRStatement::Return(Some(expr)) = stmt {
                // Store result before logging
                debug_func.body.statements.push(IRStatement::Assign {
                    target: "_return_value".to_string(),
                    value: expr.clone(),
                    var_type: Some(func.return_type.clone()),
                });

                // Log the return value
                debug_func
                    .body
                    .statements
                    .push(IRStatement::Expression(IRExpr::FunctionCall {
                        function_name: "print".to_string(),
                        arguments: vec![
                            IRExpr::Const(IRConstant::String(format!(
                                "Exiting function: {} with result: ",
                                func.name
                            ))),
                            IRExpr::Variable("_return_value".to_string()),
                        ],
                    }));

                // Return the stored result
                debug_func
                    .body
                    .statements
                    .push(IRStatement::Return(Some(IRExpr::Variable(
                        "_return_value".to_string(),
                    ))));
            } else {
                debug_func.body.statements.push(stmt.clone());
            }
        }

        debug_func
    }

    /// Apply the timer decorator to measure function execution time
    fn apply_timer_decorator(&self, func: IRFunction) -> IRFunction {
        // Create a new function with timing wrapper
        let mut timed_func = IRFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            decorators: Vec::new(),
            body: IRBody {
                statements: Vec::new(),
            },
        };

        // In a real implementation, you'd need to access the system time
        // For now, we'll just add placeholders for the timing logic

        // Add code to record start time
        timed_func.body.statements.push(IRStatement::Assign {
            target: "_start_time".to_string(),
            value: IRExpr::Const(IRConstant::Int(0)), // Placeholder for time
            var_type: Some(IRType::Int),
        });

        // Add the original function body
        for stmt in &func.body.statements {
            if let IRStatement::Return(Some(expr)) = stmt {
                // Store result before timing
                timed_func.body.statements.push(IRStatement::Assign {
                    target: "_return_value".to_string(),
                    value: expr.clone(),
                    var_type: Some(func.return_type.clone()),
                });

                // Record end time and calculate duration
                timed_func.body.statements.push(IRStatement::Assign {
                    target: "_end_time".to_string(),
                    value: IRExpr::Const(IRConstant::Int(0)), // Placeholder for time
                    var_type: Some(IRType::Int),
                });

                timed_func
                    .body
                    .statements
                    .push(IRStatement::Expression(IRExpr::FunctionCall {
                        function_name: "print".to_string(),
                        arguments: vec![
                            IRExpr::Const(IRConstant::String(format!(
                                "Function {} execution time: ",
                                func.name
                            ))),
                            IRExpr::BinaryOp {
                                left: Box::new(IRExpr::Variable("_end_time".to_string())),
                                right: Box::new(IRExpr::Variable("_start_time".to_string())),
                                op: crate::ir::IROp::Sub,
                            },
                            IRExpr::Const(IRConstant::String(" ms".to_string())),
                        ],
                    }));

                // Return the stored result
                timed_func
                    .body
                    .statements
                    .push(IRStatement::Return(Some(IRExpr::Variable(
                        "_return_value".to_string(),
                    ))));
            } else {
                timed_func.body.statements.push(stmt.clone());
            }
        }

        timed_func
    }

    /// Apply the default value decorator
    fn apply_default_value_decorator(&self, func: IRFunction) -> IRFunction {
        // Create a new function
        let mut default_func = IRFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            decorators: Vec::new(),
            body: IRBody {
                statements: Vec::new(),
            },
        };

        // Add checks for parameters and assign defaults if needed
        for param in &func.params {
            // Only handle parameters without default values
            if param.default_value.is_none() {
                // Generate a reasonable default based on the type
                let default_value = match &param.param_type {
                    IRType::Int => IRExpr::Const(IRConstant::Int(0)),
                    IRType::Float => IRExpr::Const(IRConstant::Float(0.0)),
                    IRType::Bool => IRExpr::Const(IRConstant::Bool(false)),
                    IRType::String => IRExpr::Const(IRConstant::String("".to_string())),
                    _ => continue, // Skip complex types
                };

                // Add check for "undefined" state and assign default
                // This is simplified - in real code you'd need to check a specific pattern
                let param_check = IRStatement::If {
                    condition: IRExpr::CompareOp {
                        left: Box::new(IRExpr::Variable(param.name.clone())),
                        right: Box::new(IRExpr::Const(IRConstant::Int(-9999))), // Placeholder "undefined" value
                        op: crate::ir::IRCompareOp::Eq,
                    },
                    then_body: Box::new(IRBody {
                        statements: vec![IRStatement::Assign {
                            target: param.name.clone(),
                            value: default_value,
                            var_type: Some(param.param_type.clone()),
                        }],
                    }),
                    else_body: None,
                };

                default_func.body.statements.push(param_check);
            }
        }

        // Add the original function body
        default_func
            .body
            .statements
            .extend(func.body.statements.clone());

        default_func
    }

    /// Apply the type check decorator
    fn apply_type_check_decorator(&self, func: IRFunction) -> IRFunction {
        // Create a new function
        let mut typecheck_func = IRFunction {
            name: func.name.clone(),
            params: func.params.clone(),
            return_type: func.return_type.clone(),
            decorators: Vec::new(),
            body: IRBody {
                statements: Vec::new(),
            },
        };

        // Add type checks for each parameter
        for param in &func.params {
            if let Some(param_type) = Self::get_type_check_expr(&param.name, &param.param_type) {
                // Add a conditional that checks the type
                let type_check = IRStatement::If {
                    condition: param_type,
                    then_body: Box::new(IRBody {
                        statements: Vec::new(), // Do nothing on type match
                    }),
                    else_body: Some(Box::new(IRBody {
                        statements: vec![
                            // On type mismatch, return error value or print error
                            IRStatement::Expression(IRExpr::FunctionCall {
                                function_name: "print".to_string(),
                                arguments: vec![IRExpr::Const(IRConstant::String(format!(
                                    "Type error: Parameter {} should be {}",
                                    param.name,
                                    Self::type_to_string(&param.param_type)
                                )))],
                            }),
                            // Return a default value instead of failing
                            IRStatement::Return(Some(IRExpr::Const(IRConstant::Int(-1)))),
                        ],
                    })),
                };

                typecheck_func.body.statements.push(type_check);
            }
        }

        // Add the original function body
        typecheck_func
            .body
            .statements
            .extend(func.body.statements.clone());

        // Add return value type check if needed
        if let Some(last_stmt_idx) = typecheck_func
            .body
            .statements
            .iter()
            .position(|stmt| matches!(stmt, IRStatement::Return(Some(_))))
        {
            if let IRStatement::Return(Some(return_expr)) =
                &typecheck_func.body.statements[last_stmt_idx]
            {
                // Store return value to check its type
                typecheck_func.body.statements[last_stmt_idx] = IRStatement::Assign {
                    target: "_return_value".to_string(),
                    value: return_expr.clone(),
                    var_type: Some(func.return_type.clone()),
                };

                // Check return value type
                if let Some(type_check_expr) =
                    Self::get_type_check_expr("_return_value", &func.return_type)
                {
                    let return_type_check = IRStatement::If {
                        condition: type_check_expr,
                        then_body: Box::new(IRBody {
                            statements: vec![
                                // Return the value if type matches
                                IRStatement::Return(Some(IRExpr::Variable(
                                    "_return_value".to_string(),
                                ))),
                            ],
                        }),
                        else_body: Some(Box::new(IRBody {
                            statements: vec![
                                // Print error and return anyway (could be more strict)
                                IRStatement::Expression(IRExpr::FunctionCall {
                                    function_name: "print".to_string(),
                                    arguments: vec![IRExpr::Const(IRConstant::String(format!(
                                        "Type error: Return value should be {}",
                                        Self::type_to_string(&func.return_type)
                                    )))],
                                }),
                                IRStatement::Return(Some(IRExpr::Variable(
                                    "_return_value".to_string(),
                                ))),
                            ],
                        })),
                    };

                    typecheck_func.body.statements.push(return_type_check);
                } else {
                    // Just return without type checking
                    typecheck_func
                        .body
                        .statements
                        .push(IRStatement::Return(Some(IRExpr::Variable(
                            "_return_value".to_string(),
                        ))));
                }
            }
        }

        typecheck_func
    }

    /// Apply the pure function decorator (optimization hint)
    fn apply_pure_decorator(&self, mut func: IRFunction) -> IRFunction {
        // For pure functions, we just add metadata
        // The actual optimization would happen in the WebAssembly generation phase
        func.body.statements.insert(
            0,
            IRStatement::Expression(IRExpr::FunctionCall {
                function_name: "_mark_pure".to_string(),
                arguments: Vec::new(),
            }),
        );

        func
    }

    /// Apply a custom decorator by name
    fn apply_custom_decorator(&self, name: &str, func: IRFunction) -> IRFunction {
        if let Some(decorator) = self.custom_decorators.get(name) {
            decorator(func)
        } else {
            // If the decorator isn't registered, return the function unchanged
            func
        }
    }

    /// Helper to convert IR type to string representation
    fn type_to_string(ir_type: &IRType) -> String {
        match ir_type {
            IRType::Int => "int".to_string(),
            IRType::Float => "float".to_string(),
            IRType::Bool => "bool".to_string(),
            IRType::String => "string".to_string(),
            IRType::List(elem_type) => format!("list of {}", Self::type_to_string(elem_type)),
            IRType::Dict(key_type, val_type) => format!(
                "dict with {} keys and {} values",
                Self::type_to_string(key_type),
                Self::type_to_string(val_type)
            ),
            IRType::Tuple(types) => {
                let type_strs: Vec<String> = types.iter().map(Self::type_to_string).collect();
                format!("tuple of ({})", type_strs.join(", "))
            }
            IRType::Class(name) => name.clone(),
            _ => "unknown".to_string(),
        }
    }

    /// Helper to generate type checking expression for a variable
    fn get_type_check_expr(var_name: &str, var_type: &IRType) -> Option<IRExpr> {
        match var_type {
            IRType::Int => Some(IRExpr::FunctionCall {
                function_name: "_is_int".to_string(),
                arguments: vec![IRExpr::Variable(var_name.to_string())],
            }),
            IRType::Float => Some(IRExpr::FunctionCall {
                function_name: "_is_float".to_string(),
                arguments: vec![IRExpr::Variable(var_name.to_string())],
            }),
            IRType::Bool => Some(IRExpr::FunctionCall {
                function_name: "_is_bool".to_string(),
                arguments: vec![IRExpr::Variable(var_name.to_string())],
            }),
            IRType::String => Some(IRExpr::FunctionCall {
                function_name: "_is_string".to_string(),
                arguments: vec![IRExpr::Variable(var_name.to_string())],
            }),
            _ => None, // More complex types would need more specialized checks
        }
    }
}
