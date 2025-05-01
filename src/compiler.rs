use crate::ir::{
    IRBody, IRBoolOp, IRCompareOp, IRConstant, IRExpr, IRFunction, IRModule, IROp, IRStatement,
    IRUnaryOp,
};
use std::collections::HashMap;
use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
    MemorySection, MemoryType, Module, TypeSection, ValType,
};

struct CompilationContext {
    locals: HashMap<String, u32>,
    local_count: u32,
    function_types: HashMap<String, u32>,
}

impl CompilationContext {
    fn new() -> Self {
        CompilationContext {
            locals: HashMap::new(),
            local_count: 0,
            function_types: HashMap::new(),
        }
    }

    fn add_local(&mut self, name: &str) -> u32 {
        let idx = self.local_count;
        self.locals.insert(name.to_string(), idx);
        self.local_count += 1;
        idx
    }

    fn get_local(&self, name: &str) -> Option<u32> {
        self.locals.get(name).copied()
    }
}

pub fn compile_ir(ir_module: &IRModule) -> Vec<u8> {
    let mut module = Module::new();
    let mut ctx = CompilationContext::new();

    // Build type section
    let mut types = TypeSection::new();

    // Create function types for each function
    for (idx, func) in ir_module.functions.iter().enumerate() {
        // For now, all functions take i32 params and return i32
        let param_count = func.params.len();
        let params = vec![ValType::I32; param_count];
        let results = vec![ValType::I32]; // Assuming all functions return i32 for now

        types.ty().function(params, results);
        ctx.function_types.insert(func.name.clone(), idx as u32);
    }

    module.section(&types);

    // Build function section
    let mut functions = FunctionSection::new();
    for idx in 0..ir_module.functions.len() {
        functions.function(idx as u32);
    }
    module.section(&functions);

    // Export section
    let mut exports = ExportSection::new();
    for (idx, func) in ir_module.functions.iter().enumerate() {
        exports.export(func.name.as_str(), ExportKind::Func, idx as u32);
    }
    module.section(&exports);

    // Memory section for string storage
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,       // Start with 1 page (64KB)
        maximum: Some(1), // Also set maximum to 1 page for now
        memory64: false,
        shared: false,
        page_size_log2: Some(16), // Standard WebAssembly page size (64KB = 2^16 bytes)
    });
    module.section(&memories);

    // Memory export
    let mut memory_exports = ExportSection::new();
    memory_exports.export("memory", ExportKind::Memory, 0);
    module.section(&memory_exports);

    // Code section
    let mut codes = CodeSection::new();

    for func_ir in &ir_module.functions {
        codes.function(&compile_function(func_ir, &mut ctx));
    }

    module.section(&codes);

    module.finish()
}

fn compile_function(ir_func: &IRFunction, _ctx: &mut CompilationContext) -> Function {
    let mut local_ctx = CompilationContext::new();

    // Track function parameters as locals
    for param in &ir_func.params {
        local_ctx.add_local(param);
    }

    // Scan for variable declarations to allocate locals
    scan_and_allocate_locals(&ir_func.body, &mut local_ctx);

    // Locals will be the function parameters and any local variables
    // For now, we'll use i32 for all locals
    let locals = vec![(
        local_ctx.local_count - ir_func.params.len() as u32,
        ValType::I32,
    )];

    let mut func = Function::new(locals);

    // Compile the function body
    compile_body(&ir_func.body, &mut func, &local_ctx);

    // If no explicit return, add a default return
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);

    func
}

fn scan_and_allocate_locals(body: &IRBody, ctx: &mut CompilationContext) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign { target, .. } => {
                if ctx.get_local(target).is_none() {
                    ctx.add_local(target);
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
            _ => {}
        }
    }
}

fn compile_body(body: &IRBody, func: &mut Function, ctx: &CompilationContext) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Return(expr_opt) => {
                if let Some(expr) = expr_opt {
                    emit_expr(expr, func, ctx);
                } else {
                    func.instruction(&Instruction::I32Const(0));
                }
                func.instruction(&Instruction::Return);
            }
            IRStatement::Assign { target, value } => {
                emit_expr(value, func, ctx);
                if let Some(local_idx) = ctx.get_local(target) {
                    func.instruction(&Instruction::LocalSet(local_idx));
                } else {
                    // This should not happen if we properly allocated locals
                    panic!("Variable {} not found in context", target);
                }
            }
            IRStatement::If {
                condition,
                then_body,
                else_body,
            } => {
                emit_expr(condition, func, ctx);

                // If-else block with no result value
                func.instruction(&Instruction::If(BlockType::Empty));

                // Then branch
                compile_body(then_body, func, ctx);

                if let Some(else_body) = else_body {
                    // Else branch
                    func.instruction(&Instruction::Else);
                    compile_body(else_body, func, ctx);
                }

                func.instruction(&Instruction::End);
            }
            IRStatement::While { condition, body } => {
                // Loop block
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Condition check
                emit_expr(condition, func, ctx);
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::BrIf(1)); // Break out of the loop if condition is false

                // Loop body
                compile_body(body, func, ctx);

                // Jump back to the start of the loop
                func.instruction(&Instruction::Br(0));

                // End of loop and block
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);
            }
            IRStatement::Expression(expr) => {
                emit_expr(expr, func, ctx);
                // We don't need the result value, so we just let it fall off the stack
            }
        }
    }
}

fn emit_expr(expr: &IRExpr, func: &mut Function, ctx: &CompilationContext) {
    match expr {
        IRExpr::Const(constant) => {
            match constant {
                IRConstant::Int(i) => {
                    func.instruction(&Instruction::I32Const(*i));
                }
                IRConstant::Float(f) => {
                    // For simplicity, we'll convert float to int
                    let truncated = *f as i32;
                    func.instruction(&Instruction::I32Const(truncated));
                }
                IRConstant::Bool(b) => {
                    func.instruction(&Instruction::I32Const(if *b { 1 } else { 0 }));
                }
                IRConstant::String(_) => {
                    // String handling will be added later
                    func.instruction(&Instruction::I32Const(0));
                }
            }
        }
        IRExpr::Param(name) => {
            if let Some(local_idx) = ctx.get_local(name) {
                func.instruction(&Instruction::LocalGet(local_idx));
            } else {
                panic!("Parameter {} not found in context", name);
            }
        }
        IRExpr::Variable(name) => {
            if let Some(local_idx) = ctx.get_local(name) {
                func.instruction(&Instruction::LocalGet(local_idx));
            } else {
                panic!("Variable {} not found in context", name);
            }
        }
        IRExpr::BinaryOp { left, right, op } => {
            emit_expr(left, func, ctx);
            emit_expr(right, func, ctx);

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
                    // Modulo operation: a % b
                    func.instruction(&Instruction::I32RemS);
                }
                IROp::FloorDiv => {
                    // Floor division: a // b
                    // In WebAssembly, I32DivS already does floor division for signed integers
                    func.instruction(&Instruction::I32DivS);
                }
                IROp::Pow => {
                    // Power operation: a ** b
                    // WebAssembly doesn't have a direct power instruction, so we implement it
                    // via a loop for integer powers

                    // Save the base value to a local (we'll use local 0 for simplicity)
                    func.instruction(&Instruction::LocalSet(0)); // Save exponent
                    func.instruction(&Instruction::LocalSet(1)); // Save base

                    // Initialize result to 1
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::LocalSet(2)); // result = 1

                    // Check if exponent is 0, if so return 1
                    func.instruction(&Instruction::LocalGet(0)); // Get exponent
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::Return);
                    func.instruction(&Instruction::End);

                    // Handle negative exponent as special case (return 0 for now)
                    func.instruction(&Instruction::LocalGet(0)); // Get exponent
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::Return);
                    func.instruction(&Instruction::End);

                    // Start loop to calculate power
                    func.instruction(&Instruction::Block(BlockType::Empty));
                    func.instruction(&Instruction::Loop(BlockType::Empty));

                    // Check if exponent is 0, if so break out of loop
                    func.instruction(&Instruction::LocalGet(0)); // Get exponent
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::BrIf(1)); // Break out of loop

                    // result *= base
                    func.instruction(&Instruction::LocalGet(2)); // Get result
                    func.instruction(&Instruction::LocalGet(1)); // Get base
                    func.instruction(&Instruction::I32Mul);
                    func.instruction(&Instruction::LocalSet(2)); // Update result

                    // exponent--
                    func.instruction(&Instruction::LocalGet(0)); // Get exponent
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Sub);
                    func.instruction(&Instruction::LocalSet(0)); // Update exponent

                    // Loop back
                    func.instruction(&Instruction::Br(0));

                    // End loop
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::End);

                    // Push result to stack
                    func.instruction(&Instruction::LocalGet(2));
                }
            }
        }
        IRExpr::UnaryOp { operand, op } => {
            emit_expr(operand, func, ctx);

            match op {
                IRUnaryOp::Neg => {
                    // Negate: -x
                    // WebAssembly doesn't have a direct negate instruction,
                    // but we can use 0 - x
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32Sub);
                }
                IRUnaryOp::Not => {
                    // Logical not: not x
                    // In WebAssembly, we can use the eqz instruction (which is true if x == 0)
                    // We first convert our value to either 0 or 1 (boolean), then flip it

                    // First ensure it's 0 or 1
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32Ne);

                    // Then invert it (1 becomes 0, 0 becomes 1)
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Xor);
                }
            }
        }
        IRExpr::CompareOp { left, right, op } => {
            emit_expr(left, func, ctx);
            emit_expr(right, func, ctx);

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
            }
        }
        IRExpr::BoolOp { left, right, op } => {
            match op {
                IRBoolOp::And => {
                    // Simpler implementation of AND operation
                    emit_expr(left, func, ctx);
                    // Store in temporary local
                    func.instruction(&Instruction::LocalSet(0));
                    // Get it back
                    func.instruction(&Instruction::LocalGet(0));

                    // If-else pattern for short-circuit evaluation
                    func.instruction(&Instruction::If(BlockType::Empty));
                    emit_expr(right, func, ctx);
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::I32Const(0)); // False
                    func.instruction(&Instruction::End);
                }
                IRBoolOp::Or => {
                    // Simpler implementation of OR operation
                    emit_expr(left, func, ctx);
                    // Store in temporary local
                    func.instruction(&Instruction::LocalSet(0));
                    // Get it back
                    func.instruction(&Instruction::LocalGet(0));

                    // If-else pattern for short-circuit evaluation
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(1)); // True
                    func.instruction(&Instruction::Else);
                    emit_expr(right, func, ctx);
                    func.instruction(&Instruction::End);
                }
            }
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            // Push arguments onto the stack in order
            for arg in arguments {
                emit_expr(arg, func, ctx);
            }

            // Look up the function index if it exists in our context
            if let Some(func_idx) = ctx.function_types.get(function_name.as_str()) {
                func.instruction(&Instruction::Call(*func_idx));
            } else {
                // We don't need special handling for "int" since we handle it in the IR conversion
                
                // For unknown functions, we'll just return 0 for now
                func.instruction(&Instruction::I32Const(0));
            }
        }
    }
}
