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
/// Returns [`crate::core::errors::WaspyError::ParseError`] (with line and
/// column) when the source is not valid Python, and
/// [`crate::core::errors::WaspyError::UnsupportedFeature`] (with location
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

/// Parse standard-library code written in the supported subset
/// (`ir::embedded`), leaving the line table of the program being compiled as
/// it was.
pub(crate) fn parse_embedded(source: &str) -> Result<Suite> {
    let saved = LINE_STARTS.with(|starts| starts.borrow().clone());
    let ast = parse_python(source);
    LINE_STARTS.with(|starts| *starts.borrow_mut() = saved);
    ast
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

/// The location of a byte offset into the source most recently parsed on
/// this thread (its column counted in bytes), for a converter error.
pub(crate) fn location_of(offset: usize, function: Option<&str>) -> Option<ErrorLocation> {
    LINE_STARTS.with(|starts| {
        let starts = starts.borrow();
        let line = starts.partition_point(|&s| s <= offset);
        let start = *starts.get(line.checked_sub(1)?)?;
        Some(ErrorLocation {
            file: None,
            line,
            column: Some(offset - start + 1),
            function: function.map(str::to_string),
        })
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
    validate_generator_expressions(ast, source)
}

/// The builtins that would show a generator expression for what it is: its
/// repr names a generator object, it has no length, it is always true, and
/// its type is not a list's.
const GENERATOR_OBSERVERS: [&str; 13] = [
    "print",
    "str",
    "repr",
    "ascii",
    "format",
    "len",
    "bool",
    "type",
    "isinstance",
    "reversed",
    "iter",
    "id",
    "hash",
];

/// The builtins that walk an iterable argument once, so a list of what a
/// generator expression yields is the same to them.
const GENERATOR_CONSUMERS: [&str; 17] = [
    "sum",
    "min",
    "max",
    "sorted",
    "list",
    "tuple",
    "set",
    "frozenset",
    "dict",
    "any",
    "all",
    "enumerate",
    "zip",
    "map",
    "filter",
    "bytes",
    "bytearray",
];

/// The methods of the builtin collections and of str that walk an iterable
/// argument once.
const GENERATOR_CONSUMING_METHODS: [&str; 16] = [
    "join",
    "extend",
    "extendleft",
    "update",
    "union",
    "intersection",
    "difference",
    "symmetric_difference",
    "intersection_update",
    "difference_update",
    "symmetric_difference_update",
    "issubset",
    "issuperset",
    "isdisjoint",
    "fromkeys",
    "subtract",
];

/// A generator expression is accepted where something that walks it once
/// consumes it as it is written: a builtin or a standard-library function
/// taking an iterable, a collection's method, a `for` statement, or a
/// comprehension.
///
/// It is built as a list (a `for` statement and `any()` / `all()` aside,
/// which pull it), and a list is not CPython's generator: kept in a variable
/// or handed to the program's own code it can be walked twice where a
/// generator is exhausted, it has a length and a truth value, it prints as a
/// list, and it read the variables it uses when it was made, not when it is
/// walked. Those were answered, silently. Anywhere a generator expression
/// could be told from the list, it is refused.
fn validate_generator_expressions(ast: &Suite, source: &str) -> Result<()> {
    use rustpython_parser::ast::{self, fold::Fold, text_size::TextRange, Ranged};
    use std::collections::HashSet;

    /// The names the program's own code defines, and the ones its imports
    /// bind to the standard library.
    #[derive(Default)]
    struct Names {
        defined: HashSet<String>,
        library_modules: HashSet<String>,
        library_names: HashSet<String>,
        /// Folding a class's own body, whose functions are its methods.
        in_class_body: bool,
    }
    impl Fold<TextRange> for Names {
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
        fn fold_stmt(&mut self, stmt: Stmt) -> std::result::Result<Stmt, Self::Error> {
            // A method is reached as an attribute, never by its bare name, so
            // `sum(...)` in a method named `sum` is still the builtin; the
            // functions a method defines are names in its body.
            let in_class_body =
                std::mem::replace(&mut self.in_class_body, matches!(stmt, Stmt::ClassDef(_)));
            match &stmt {
                Stmt::FunctionDef(def) if !in_class_body => {
                    self.defined.insert(def.name.to_string());
                }
                Stmt::AsyncFunctionDef(def) if !in_class_body => {
                    self.defined.insert(def.name.to_string());
                }
                Stmt::ClassDef(def) => {
                    self.defined.insert(def.name.to_string());
                }
                Stmt::Import(import) => {
                    for alias in &import.names {
                        let module = alias.name.as_str();
                        let root = module.split('.').next().unwrap_or(module);
                        if crate::stdlib::is_stdlib_module(root) {
                            // `import a.b` binds `a`; `import a.b as c` binds
                            // the submodule.
                            let bound = alias.asname.as_ref().map_or(root, |n| n.as_str());
                            self.library_modules.insert(bound.to_string());
                        }
                    }
                }
                Stmt::ImportFrom(import) => {
                    let module = import.module.as_ref().map_or("", |m| m.as_str());
                    let root = module.split('.').next().unwrap_or(module);
                    if import.level.as_ref().map_or(0, |l| l.to_usize()) == 0
                        && crate::stdlib::is_stdlib_module(root)
                    {
                        for alias in &import.names {
                            let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                            self.library_names.insert(bound.to_string());
                        }
                    }
                }
                _ => {}
            }
            let folded = ast::fold::fold_stmt(self, stmt);
            self.in_class_body = in_class_body;
            folded
        }
    }

    struct Check<'a> {
        source: &'a str,
        names: Names,
        /// Where each accepted generator expression starts.
        consumed: HashSet<usize>,
        functions: Vec<String>,
    }
    impl Check<'_> {
        fn consume(&mut self, expr: &Expr) {
            if let Expr::GeneratorExp(g) = expr {
                self.consumed.insert(g.range.start().into());
            }
        }

        /// Whether a call of `func` walks an iterable argument once: a
        /// builtin that does, something of the standard library, or a
        /// collection's method.
        fn walks_once(&self, func: &Expr) -> bool {
            match func {
                Expr::Name(name) => {
                    let name = name.id.as_str();
                    !self.names.defined.contains(name)
                        && (GENERATOR_CONSUMERS.contains(&name)
                            || self.names.library_names.contains(name))
                }
                Expr::Attribute(attribute) => {
                    let mut root = &*attribute.value;
                    while let Expr::Attribute(inner) = root {
                        root = &inner.value;
                    }
                    let library = matches!(root, Expr::Name(name)
                        if self.names.library_modules.contains(name.id.as_str())
                            || self.names.library_names.contains(name.id.as_str()));
                    library || GENERATOR_CONSUMING_METHODS.contains(&attribute.attr.as_str())
                }
                _ => false,
            }
        }
    }
    impl Fold<TextRange> for Check<'_> {
        type TargetU = TextRange;
        type Error = anyhow::Error;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &TextRange) -> Self::UserContext {}
        fn map_user(&mut self, user: TextRange, _context: ()) -> Result<TextRange> {
            Ok(user)
        }
        fn fold_stmt(&mut self, stmt: Stmt) -> Result<Stmt> {
            let function = match &stmt {
                Stmt::FunctionDef(def) => Some(def.name.to_string()),
                Stmt::AsyncFunctionDef(def) => Some(def.name.to_string()),
                _ => None,
            };
            if let Stmt::For(for_stmt) = &stmt {
                self.consume(&for_stmt.iter);
            }
            if let Some(name) = &function {
                self.functions.push(name.clone());
            }
            let folded = ast::fold::fold_stmt(self, stmt);
            if function.is_some() {
                self.functions.pop();
            }
            folded
        }
        fn fold_expr(&mut self, expr: Expr) -> Result<Expr> {
            let function = self.functions.last().cloned();
            let function = function.as_deref();
            match &expr {
                Expr::Call(call) => {
                    let observer = match &*call.func {
                        Expr::Name(name) if !self.names.defined.contains(name.id.as_str()) => {
                            GENERATOR_OBSERVERS.iter().find(|o| **o == name.id.as_str())
                        }
                        _ => None,
                    };
                    let walks_once = self.walks_once(&call.func);
                    let arguments = call
                        .args
                        .iter()
                        .chain(call.keywords.iter().map(|k| &k.value));
                    for argument in arguments {
                        // `f(*(x for x in xs))` unpacks it into the call's
                        // arguments, whatever is called.
                        if let Expr::Starred(starred) = argument {
                            self.consume(&starred.value);
                            continue;
                        }
                        let inner = argument;
                        let Expr::GeneratorExp(g) = inner else {
                            continue;
                        };
                        if let Some(observer) = observer {
                            return Err(unsupported(
                                format!(
                                    "{observer}() of a generator expression is not supported: it \
                                     is built as a list here, which {observer}() would answer \
                                     for, where CPython answers for a generator object. Hint: \
                                     write a list comprehension, [...]"
                                ),
                                self.source,
                                g.range.start().into(),
                                function,
                            ));
                        }
                        if !walks_once {
                            return Err(unsupported(
                                "a generator expression passed to the program's own code is not \
                                 supported: it is built as a list here, and what receives it \
                                 could tell (a list can be walked twice and measured, where \
                                 CPython's generator cannot). Hint: pass a list comprehension, \
                                 [...], or give it straight to a builtin that walks it \
                                 (sum(), sorted(), \", \".join())",
                                self.source,
                                g.range.start().into(),
                                function,
                            ));
                        }
                        self.consume(inner);
                    }
                }
                Expr::ListComp(ast::ExprListComp { generators, .. })
                | Expr::SetComp(ast::ExprSetComp { generators, .. })
                | Expr::DictComp(ast::ExprDictComp { generators, .. })
                | Expr::GeneratorExp(ast::ExprGeneratorExp { generators, .. }) => {
                    for generator in generators {
                        self.consume(&generator.iter);
                    }
                }
                _ => {}
            }
            if let Expr::GeneratorExp(g) = &expr {
                let start: usize = g.range().start().into();
                if !self.consumed.contains(&start) {
                    return Err(unsupported(
                        "a generator expression kept in a variable, returned, or stored is not \
                         supported: it is built as a list here, which can be walked twice, \
                         measured, and printed where CPython's generator cannot. Hint: pass it \
                         straight to what consumes it (sum(x for x in xs)), write a list \
                         comprehension, [...], for a list, or a generator function for a lazy \
                         sequence",
                        self.source,
                        start,
                        function,
                    ));
                }
            }
            ast::fold::fold_expr(self, expr)
        }
    }
    let mut names = Names::default();
    for stmt in ast.iter().cloned() {
        let _ = names.fold_stmt(stmt);
    }
    let mut check = Check {
        source,
        names,
        consumed: HashSet::new(),
        functions: Vec::new(),
    };
    for stmt in ast.iter().cloned() {
        check.fold_stmt(stmt)?;
    }
    Ok(())
}

/// Modules a program may import for its annotations alone: nothing of them
/// runs, and their names are read only where types are written.
const ANNOTATION_MODULES: &[&str] = &[
    "typing",
    "typing_extensions",
    "__future__",
    "collections.abc",
];

/// What a program can import, for the refusal of anything else.
const IMPORTABLE: &str = "its own modules, the standard-library modules sys, os, math, random, \
                          json, re, datetime, collections, itertools, functools, logging, \
                          asyncio, abc, dataclasses, importlib, typing, base64, bisect, csv, \
                          hashlib, heapq, io, operator, pathlib, string, textwrap, time, \
                          urllib.parse, and uuid, and js and pyodide for the JavaScript bridge";

/// Refuse an import of a module the program cannot have: neither a module the
/// compiler supports nor one of the program's own (`provides`). A
/// third-party package (`import numpy as np`) was skipped with a warning, and
/// every use of it then failed apart from the import, with hints that could
/// not help (pass `axis` positionally, name 'stats' is not defined). An
/// import guarded by `try`/`except ImportError` is the program's own fallback
/// and is left alone, as is a relative import, which names a file of the
/// program.
pub fn check_imports(ast: &Suite, source: &str, provides: &dyn Fn(&str) -> bool) -> Result<()> {
    fn check(
        name: &str,
        offset: usize,
        source: &str,
        provides: &dyn Fn(&str) -> bool,
        function: Option<&str>,
    ) -> Result<()> {
        if name.is_empty()
            || crate::stdlib::is_stdlib_module_path(name)
            || ANNOTATION_MODULES.contains(&name)
            || provides(name)
        {
            return Ok(());
        }
        Err(unsupported(
            format!(
                "cannot import '{name}': a program can import {IMPORTABLE}. A third-party \
                 package such as numpy or pandas cannot be compiled into the program, and \
                 nothing runs in it that was not compiled"
            ),
            source,
            offset,
            function,
        ))
    }
    fn walk(
        body: &[Stmt],
        source: &str,
        provides: &dyn Fn(&str) -> bool,
        function: Option<&str>,
    ) -> Result<()> {
        for stmt in body {
            match stmt {
                Stmt::Import(i) => {
                    for alias in &i.names {
                        check(
                            alias.name.as_str(),
                            i.range.start().into(),
                            source,
                            provides,
                            function,
                        )?;
                    }
                }
                Stmt::ImportFrom(i) if i.level.as_ref().map_or(0, |l| l.to_usize()) == 0 => {
                    let module = i.module.as_ref().map(|m| m.to_string()).unwrap_or_default();
                    check(&module, i.range.start().into(), source, provides, function)?;
                }
                Stmt::FunctionDef(d) => walk(&d.body, source, provides, Some(d.name.as_str()))?,
                Stmt::AsyncFunctionDef(d) => {
                    walk(&d.body, source, provides, Some(d.name.as_str()))?
                }
                Stmt::ClassDef(c) => walk(&c.body, source, provides, function)?,
                Stmt::If(s) => {
                    walk(&s.body, source, provides, function)?;
                    walk(&s.orelse, source, provides, function)?;
                }
                Stmt::For(s) => {
                    walk(&s.body, source, provides, function)?;
                    walk(&s.orelse, source, provides, function)?;
                }
                Stmt::AsyncFor(s) => {
                    walk(&s.body, source, provides, function)?;
                    walk(&s.orelse, source, provides, function)?;
                }
                Stmt::While(s) => {
                    walk(&s.body, source, provides, function)?;
                    walk(&s.orelse, source, provides, function)?;
                }
                Stmt::With(s) => walk(&s.body, source, provides, function)?,
                Stmt::AsyncWith(s) => walk(&s.body, source, provides, function)?,
                Stmt::Try(s) if is_import_guard(s) => {}
                Stmt::Try(s) => {
                    walk(&s.body, source, provides, function)?;
                    for handler in &s.handlers {
                        let rustpython_parser::ast::ExceptHandler::ExceptHandler(h) = handler;
                        walk(&h.body, source, provides, function)?;
                    }
                    walk(&s.orelse, source, provides, function)?;
                    walk(&s.finalbody, source, provides, function)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
    walk(ast, source, provides, None)
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
        // A coroutine function inside another function would be a closure
        // making coroutines, which is not supported.
        Stmt::AsyncFunctionDef(def) if function.is_some() => Err(unsupported(
            format!(
                "'async def {}' inside a function is not supported. Hint: define it at module \
                 level",
                def.name
            ),
            source,
            def.range.start().into(),
            function,
        )),
        Stmt::AsyncFunctionDef(def) => validate_body(&def.body, source, Some(def.name.as_str())),
        Stmt::AsyncFor(stmt) => {
            validate_body(&stmt.body, source, function)?;
            validate_body(&stmt.orelse, source, function)
        }
        Stmt::AsyncWith(stmt) => validate_body(&stmt.body, source, function),
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
        Stmt::Delete(stmt)
            if !stmt
                .targets
                .iter()
                .all(|t| matches!(t, Expr::Subscript(_) | Expr::Name(_))) =>
        {
            Err(unsupported(
                "'del' of an attribute is not supported. Hint: assign None to it; 'del' works \
                 on a name, a list index or slice, and a dict key",
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
        // Expressions are the converter's to check (where an `await` may
        // stand included).
        _ => Ok(()),
    }
}
