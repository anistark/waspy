//! Standard-library code written in the Python subset.
//!
//! Some of what CPython writes in Python (or in C over strings) is longest in
//! IR builders and shortest in Python: a loop over characters, a parser. Such
//! a helper is kept here as source, lowered through the same front end as a
//! program's (`core::parser`, then `lower_ast_to_ir`, module passes included),
//! and its functions added to the module that calls them. They are then
//! compiled like the program's own code, so nothing is interpreted (critical
//! rule 6), and they hold to the subset's own rules: what the compiler would
//! refuse in a program, it refuses here, at the helper's first use.
//!
//! A helper's functions carry a prefix `ir::is_internal_function` knows, so
//! they stay out of exports and metadata, and use no lambda or nested `def`,
//! whose lifted names would collide with the program's.

use crate::ir::IRModule;
use anyhow::{Context, Result};
use rustpython_parser::ast::{Expr, Stmt, Suite};

/// Lower `source`, Python in the supported subset, to IR.
pub(crate) fn lower(source: &str) -> Result<IRModule> {
    let ast = crate::core::parser::parse_embedded(source)?;
    let ast = fold_constants(ast);
    crate::ir::lower_ast_to_ir_named(&ast, "<embedded>")
}

/// Whether a module-level name is one of a helper's constants: written in
/// capitals, as `OP_MATCH` is.
fn is_constant_name(name: &str) -> bool {
    name.len() > 1
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A helper's module-level constants (`OP_MATCH = 1`, a number assigned to a
/// name in capitals) written into the code that reads them, and their
/// definitions left out. Only a helper's functions and classes are added to
/// the module that uses it, so a constant it read by name would be a name of
/// the program's.
fn fold_constants(ast: Suite) -> Suite {
    use rustpython_parser::ast::{self, fold::Fold, text_size::TextRange};
    use std::collections::HashMap;

    fn number(value: &Expr) -> bool {
        match value {
            Expr::Constant(c) => matches!(c.value, ast::Constant::Int(_) | ast::Constant::Float(_)),
            Expr::UnaryOp(u) => matches!(u.op, ast::UnaryOp::USub) && number(&u.operand),
            _ => false,
        }
    }

    struct Constants(HashMap<String, Expr>);
    impl Fold<TextRange> for Constants {
        type TargetU = TextRange;
        type Error = std::convert::Infallible;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &TextRange) -> Self::UserContext {}
        fn map_user(
            &mut self,
            user: TextRange,
            _context: (),
        ) -> std::result::Result<TextRange, Self::Error> {
            Ok(user)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            if let Expr::Name(name) = &expr {
                if matches!(name.ctx, ast::ExprContext::Load) {
                    if let Some(value) = self.0.get(name.id.as_str()) {
                        return Ok(value.clone());
                    }
                }
            }
            ast::fold::fold_expr(self, expr)
        }
    }

    let mut constants = HashMap::new();
    let mut rest = Vec::with_capacity(ast.len());
    for stmt in ast {
        if let Stmt::Assign(assign) = &stmt {
            if let [Expr::Name(name)] = assign.targets.as_slice() {
                if is_constant_name(name.id.as_str()) && number(&assign.value) {
                    constants.insert(name.id.to_string(), (*assign.value).clone());
                    continue;
                }
            }
        }
        rest.push(stmt);
    }
    if constants.is_empty() {
        return rest;
    }
    let mut folder = Constants(constants);
    rest.into_iter()
        .map(|stmt| match folder.fold_stmt(stmt) {
            Ok(stmt) => stmt,
            Err(never) => match never {},
        })
        .collect()
}

/// Add the functions and classes of `source` to `module`, leaving out any it
/// already has by name (a merge of several files adds each once).
pub(crate) fn graft(module: &mut IRModule, source: &str) -> Result<()> {
    let lowered = lower(source)
        .context("a standard-library helper written in Python does not compile here")?;
    module.memory_layout.merge_from(&lowered.memory_layout);
    for function in lowered.functions {
        if !module.functions.iter().any(|f| f.name == function.name) {
            module.functions.push(function);
        }
    }
    for class in lowered.classes {
        if !module.classes.iter().any(|c| c.name == class.name) {
            module.classes.push(class);
        }
    }
    Ok(())
}
