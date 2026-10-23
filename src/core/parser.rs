//! Python parsing (Stage 1 of the pipeline) and early syntax validation.
//!
//! [`parse_python`] turns source text into a rustpython AST and then runs a
//! quick validation sweep that rejects Python constructs Waspy does not
//! compile (`async`, `match`, `del` of a name,
//! and the like) with a located error and a hint,
//! instead of letting them fail deep inside IR conversion or codegen (or,
//! worse, compile to silently wrong code).

use crate::core::errors::{parse_error, unsupported_feature, ErrorLocation};
use crate::log_verbose;
use anyhow::Result;
use rustpython_parser::ast::{Expr, Stmt, Suite};
use rustpython_parser::Parse;

/// Parse Python source code into an AST and validate that it only uses
/// syntax Waspy supports.
///
/// # Errors
///
/// Returns [`crate::core::errors::ChakraError::ParseError`] (with line and
/// column) when the source is not valid Python, and
/// [`crate::core::errors::ChakraError::UnsupportedFeature`] (with location
/// and a hint) when it parses but uses a construct outside the supported
/// subset.
pub fn parse_python(source: &str) -> Result<Suite> {
    LINE_STARTS.with(|starts| {
        let mut starts = starts.borrow_mut();
        starts.clear();
        starts.push(0);
        starts.extend(source.match_indices('\n').map(|(at, _)| at + 1));
    });
    let ast = Suite::parse(source, "<module>").map_err(|e| {
        let (line, column) = line_col(source, e.offset.into());
        parse_error(e.error.to_string(), Some(line), Some(column))
    })?;

    validate_supported(&ast, source)?;

    // Log AST in verbose mode
    if crate::utils::logging::get_verbosity().is_verbose() {
        log_verbose!("Python AST:");
        log_verbose!("{:#?}", ast);
    }

    Ok(ast)
}

thread_local! {
    /// Where each line of the source most recently parsed on this thread
    /// starts, so the converter can give a node its line (`logging` records
    /// carry the line they were logged on).
    static LINE_STARTS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The 1-based line of a byte offset into the source most recently parsed
/// on this thread, when one was.
pub(crate) fn line_of(offset: usize) -> Option<usize> {
    LINE_STARTS.with(|starts| {
        let starts = starts.borrow();
        if starts.is_empty() {
            return None;
        }
        Some(starts.partition_point(|&s| s <= offset))
    })
}

/// Map a byte offset into 1-based (line, column) coordinates.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(source.len());
    let prefix = &source[..clamped];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = prefix
        .rfind('\n')
        .map(|nl| clamped - nl)
        .unwrap_or(clamped + 1);
    (line, column)
}

/// Walk the module and reject unsupported syntax with located errors.
///
/// This is deliberately a *syntax*-level check: anything that needs type
/// information (or per-construct detail) stays in the IR converter. The goal
/// is that a program using a known-unsupported statement fails here, at the
/// front door, with an actionable message.
pub fn validate_supported(ast: &Suite, source: &str) -> Result<()> {
    validate_body(ast, source, None)?;
    validate_module_level(ast, source)
}

/// Reject module-level statements whose effects the compiler silently drops.
///
/// A WebAssembly module has no implicit "run the file top to bottom" step:
/// the host calls exported functions. Waspy compiles module-level variable
/// definitions by inlining their initializers into the functions that read
/// them, so `X = 1`, `X = helper()`, and `X = ClassName()` all work. Anything
/// else written at module level, a loop, a bare call, an augmented or
/// unpacking assignment, a write through a subscript or attribute, used to
/// compile "successfully" and then do nothing at all, so the program read the
/// pre-statement value and got a silently wrong answer.
///
/// Those shapes are rejected here with a hint pointing at the two things that
/// do work: move the code into a function, or drive it from an
/// `if __name__ == "__main__":` entry point.
fn validate_module_level(body: &[Stmt], source: &str) -> Result<()> {
    for stmt in body {
        let (what, offset) = match stmt {
            Stmt::For(stmt) => ("'for' loops", stmt.range.start()),
            Stmt::While(stmt) => ("'while' loops", stmt.range.start()),
            Stmt::With(stmt) => ("'with' blocks", stmt.range.start()),
            Stmt::Try(stmt) => {
                // An import guarded by try/except ImportError is a supported
                // and common shape; it carries no runtime effect to drop.
                if is_import_guard(stmt) {
                    continue;
                }
                ("'try' blocks", stmt.range.start())
            }
            Stmt::If(stmt) => {
                // `if __name__ == "__main__":` is the entry-point marker, not
                // executable module-level code, so it stays allowed.
                if mentions_dunder_name(&stmt.test) {
                    continue;
                }
                ("'if' statements", stmt.range.start())
            }
            Stmt::AugAssign(stmt) => ("augmented assignments", stmt.range.start()),
            Stmt::Expr(stmt) => {
                // Docstrings and other bare constants have no effect to drop.
                if matches!(stmt.value.as_ref(), Expr::Constant(_)) {
                    continue;
                }
                ("call statements", stmt.range.start())
            }
            Stmt::Assign(stmt) => {
                if stmt.targets.iter().all(|t| matches!(t, Expr::Name(_))) {
                    continue;
                }
                (
                    "assignments to anything but a plain name",
                    stmt.range.start(),
                )
            }
            Stmt::AnnAssign(stmt) => {
                if matches!(stmt.target.as_ref(), Expr::Name(_)) {
                    continue;
                }
                (
                    "assignments to anything but a plain name",
                    stmt.range.start(),
                )
            }
            _ => continue,
        };

        return Err(unsupported(
            format!(
                "{what} at module level are not supported: a WebAssembly module has no \
                 top-level run step, so the statement would be compiled away and later reads \
                 would silently see the value from before it. Hint: move this into a function \
                 and call it, or drive it from 'if __name__ == \"__main__\":'. \
                 Module-level definitions (X = 1, X = helper(), X = ClassName()) do work"
            ),
            source,
            offset.into(),
            None,
        ));
    }
    Ok(())
}

/// True for `try:` blocks whose body is nothing but imports, the guarded-import
/// idiom (`try: import ujson except ImportError: import json`).
fn is_import_guard(stmt: &rustpython_parser::ast::StmtTry) -> bool {
    let is_imports = |body: &[Stmt]| {
        !body.is_empty()
            && body
                .iter()
                .all(|s| matches!(s, Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::Pass(_)))
    };
    is_imports(&stmt.body)
        && stmt.handlers.iter().all(|handler| {
            let rustpython_parser::ast::ExceptHandler::ExceptHandler(h) = handler;
            is_imports(&h.body)
        })
}

/// True when the expression reads `__name__`, which marks the entry-point
/// guard rather than ordinary module-level code.
fn mentions_dunder_name(test: &Expr) -> bool {
    match test {
        Expr::Name(name) => name.id.as_str() == "__name__",
        Expr::Compare(compare) => {
            mentions_dunder_name(&compare.left)
                || compare.comparators.iter().any(mentions_dunder_name)
        }
        Expr::BoolOp(op) => op.values.iter().any(mentions_dunder_name),
        Expr::UnaryOp(op) => mentions_dunder_name(&op.operand),
        _ => false,
    }
}

/// Build the located `UnsupportedFeature` error for a node.
fn unsupported(
    message: impl Into<String>,
    source: &str,
    offset: usize,
    function: Option<&str>,
) -> anyhow::Error {
    let (line, column) = line_col(source, offset);
    unsupported_feature(
        message,
        Some(ErrorLocation {
            file: None,
            line,
            column: Some(column),
            function: function.map(str::to_string),
        }),
    )
    .into()
}

fn validate_body(body: &[Stmt], source: &str, function: Option<&str>) -> Result<()> {
    for stmt in body {
        validate_stmt(stmt, source, function)?;
    }
    Ok(())
}

fn validate_stmt(stmt: &Stmt, source: &str, function: Option<&str>) -> Result<()> {
    match stmt {
        Stmt::AsyncFunctionDef(def) => Err(unsupported(
            format!(
                "'async def {}' — async functions are not supported (planned after 1.0). \
                 Hint: define a regular 'def' function",
                def.name
            ),
            source,
            def.range.start().into(),
            function,
        )),
        Stmt::AsyncFor(stmt) => Err(unsupported(
            "'async for' is not supported (planned after 1.0). Hint: use a regular 'for' loop",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::AsyncWith(stmt) => Err(unsupported(
            "'async with' is not supported (planned after 1.0). Hint: use a regular 'with' block",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::Match(stmt) => Err(unsupported(
            "'match' statements are not supported. Hint: use an if/elif chain",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::Global(stmt) if function.is_none() => Err(unsupported(
            "the 'global' statement is only supported inside a function",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::Nonlocal(stmt) if function.is_none() => Err(unsupported(
            "nonlocal declaration not allowed at module level",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::Delete(stmt) if !stmt.targets.iter().all(|t| matches!(t, Expr::Subscript(_))) => {
            Err(unsupported(
                "'del' of a name or an attribute is not supported. Hint: 'del' works on a \
                 list index or slice and a dict key",
                source,
                stmt.range.start().into(),
                function,
            ))
        }
        Stmt::TypeAlias(stmt) => Err(unsupported(
            "'type' alias statements are not supported. Hint: use the type directly",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::TryStar(stmt) => Err(unsupported(
            "'except*' exception groups are not supported. Hint: use a plain 'except' clause",
            source,
            stmt.range.start().into(),
            function,
        )),
        Stmt::ImportFrom(import) => {
            if import.names.iter().any(|alias| alias.name.as_str() == "*") {
                return Err(unsupported(
                    "'from module import *' is not supported. Hint: import the names you use \
                     explicitly ('from module import a, b')",
                    source,
                    import.range.start().into(),
                    function,
                ));
            }
            Ok(())
        }
        Stmt::FunctionDef(def) => validate_body(&def.body, source, Some(def.name.as_str())),
        Stmt::ClassDef(def) => {
            // `metaclass=Name` is the one class keyword with a meaning here.
            let only_metaclass = def.keywords.len() == 1
                && def.keywords[0].arg.as_deref() == Some("metaclass")
                && matches!(def.keywords[0].value, Expr::Name(_));
            if !def.keywords.is_empty() && !only_metaclass {
                return Err(unsupported(
                    format!(
                        "class '{}' uses class keywords other than 'metaclass=Name' \
                         (e.g. __init_subclass__ arguments), which are not supported",
                        def.name
                    ),
                    source,
                    def.range.start().into(),
                    function,
                ));
            }
            validate_body(&def.body, source, function)
        }
        Stmt::For(stmt) => {
            validate_body(&stmt.body, source, function)?;
            validate_body(&stmt.orelse, source, function)
        }
        Stmt::While(stmt) => {
            validate_body(&stmt.body, source, function)?;
            validate_body(&stmt.orelse, source, function)
        }
        Stmt::If(stmt) => {
            validate_body(&stmt.body, source, function)?;
            validate_body(&stmt.orelse, source, function)
        }
        Stmt::With(stmt) => validate_body(&stmt.body, source, function),
        Stmt::Try(stmt) => {
            validate_body(&stmt.body, source, function)?;
            for handler in &stmt.handlers {
                let rustpython_parser::ast::ExceptHandler::ExceptHandler(h) = handler;
                validate_body(&h.body, source, function)?;
            }
            validate_body(&stmt.orelse, source, function)?;
            validate_body(&stmt.finalbody, source, function)
        }
        Stmt::Expr(expr_stmt) => validate_expr(&expr_stmt.value, source, function),
        Stmt::Assign(assign) => validate_expr(&assign.value, source, function),
        Stmt::Return(ret) => match &ret.value {
            Some(value) => validate_expr(value, source, function),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Expression-level checks. Only constructs the compiler is known to handle
/// incorrectly are rejected; everything else is left to the IR converter.
fn validate_expr(expr: &Expr, source: &str, function: Option<&str>) -> Result<()> {
    match expr {
        Expr::Await(await_expr) => Err(unsupported(
            "'await' is not supported (planned after 1.0). Hint: call the function directly",
            source,
            await_expr.range.start().into(),
            function,
        )),
        Expr::Call(call) => {
            for arg in &call.args {
                validate_expr(arg, source, function)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
