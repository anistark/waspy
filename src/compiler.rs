use wasm_encoder::{
    CodeSection, Function, FunctionSection, Instruction, Module, TypeSection, ValType,
};
use crate::ir::{IRExpr, IRFunction};

pub fn compile_ir(ir: &IRFunction) -> Vec<u8> {
    let mut module = Module::new();

    // 1. Build type section
    let mut types = TypeSection::new();
    let params = vec![ValType::I32, ValType::I32];
    let results = vec![ValType::I32];
    types.ty().function(params, results);
    module.section(&types);

    // 2. Build function section
    let mut functions = FunctionSection::new();
    let type_index = 0;
    functions.function(type_index);
    module.section(&functions);

    // 3. Build code section
    let mut codes = CodeSection::new();
    let mut func = Function::new(vec![]);

    emit_expr(&ir.body, &mut func);

    func.instruction(&Instruction::End);
    codes.function(&func);
    module.section(&codes);

    module.finish()
}

fn emit_expr(expr: &IRExpr, func: &mut Function) {
    match expr {
        IRExpr::Const(i) => {
            func.instruction(&Instruction::I32Const(*i));
        }
        IRExpr::Param(_) => {
            func.instruction(&Instruction::LocalGet(0));
        }
        IRExpr::BinaryOp { left, right, op } => {
            emit_expr(left, func);
            emit_expr(right, func);
            match op {
                crate::ir::IROp::Add => {
                    func.instruction(&Instruction::I32Add);
                }
                crate::ir::IROp::Sub => {
                    func.instruction(&Instruction::I32Sub);
                }
                crate::ir::IROp::Mul => {
                    func.instruction(&Instruction::I32Mul);
                }
                crate::ir::IROp::Div => {
                    func.instruction(&Instruction::I32DivS);
                }
            }
        }
    }
}