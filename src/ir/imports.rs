//! Module bindings, resolved on the AST before it is lowered.
//!
//! The rest of the compiler knows a module by its own name: a standard-library
//! call is `math.floor(...)`, each standard-library pass finds `re` or
//! `logging` through the module's imports, and code generation links a user
//! module's `geometry.area(...)` to the merged `area`. Imports that bind a
//! module any other way are turned into that form here, once, so nothing
//! downstream has to know them:
//!
//! - `import math as m` binds `m` to `math`. Each read of `m` becomes `math`
//!   and the statement becomes `import math`, for a standard-library module.
//!   A user module keeps its alias, which code generation resolves.
//! - `importlib.import_module("x")` and `__import__("x")` with a constant name
//!   are the import they name. The statement `m = importlib.import_module("x")`
//!   is `import x as m`, and the call anywhere else is the module itself.
//!   `__import__("a.b")` answers the package `a`, as CPython's does. A name
//!   computed at run time is refused: there is no loader at run time, and every
//!   module is linked into the program when it is compiled.
//! - An import in a function binds the name in that function only. Modules are
//!   linked when the program starts, so the import moves to module level under
//!   the same name, and reading that name where CPython has not bound it is
//!   refused, as is a read before the import or an import that only runs on
//!   some paths (inside an `if` or a loop). The imports in the
//!   `if __name__ == "__main__":` block move to module level the same way,
//!   since that block is compiled as a function.
//! - A standard-library module's name read where no import binds it is refused,
//!   where CPython raises `NameError`. It used to compile as if imported.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rustpython_parser::ast::{
    self, fold::Fold, text_size::TextRange, Alias, Constant, Expr, ExprContext, Identifier, Stmt,
    Suite,
};

use crate::core::errors::unsupported_feature;
use crate::stdlib::{is_stdlib_module, is_stdlib_module_path};

/// What an import binds a name to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// The module of this dotted name.
    Module(String),
    /// `from module import name`.
    Member(String, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Module,
    Function,
    Class,
    /// A lambda or a comprehension, which binds its own names and imports
    /// nothing.
    Inner,
}

#[derive(Debug)]
struct Scope {
    kind: Kind,
    /// The function's name, for messages.
    name: Option<String>,
    /// Names an import binds, each to one target.
    bindings: HashMap<String, Target>,
    /// Names bound otherwise: parameters, assignments, definitions. At module
    /// level also a name imported as two different modules, or both imported
    /// and assigned, which is left as it is written.
    locals: HashSet<String>,
    /// `global` declarations, read from the module scope.
    globals: HashSet<String>,
    /// A function's bindings whose import has run, in source order.
    bound: HashSet<String>,
    /// At module level: the names only the main block's imports bind, which
    /// are the block's own, as every name it binds is (see the Limitations
    /// page), and whether the code being read is in that block.
    main_only: HashSet<String>,
    reading_main: bool,
}

impl Scope {
    fn new(kind: Kind, name: Option<String>) -> Self {
        Scope {
            kind,
            name,
            bindings: HashMap::new(),
            locals: HashSet::new(),
            globals: HashSet::new(),
            bound: HashSet::new(),
            main_only: HashSet::new(),
            reading_main: false,
        }
    }
}

/// What a name read in a scope refers to.
enum Res {
    /// An import's binding, in the scope at this index of the stack.
    Binding(usize, Target),
    /// Any other binding, in the scope at this index.
    Local(usize),
    Unbound,
}

/// Resolve a read of `name` against a stack of scopes, innermost last. A
/// class body's names are not seen from the functions inside it.
fn resolve(stack: &[Scope], name: &str) -> Res {
    let innermost = stack.len().saturating_sub(1);
    for (i, scope) in stack.iter().enumerate().rev() {
        if scope.kind == Kind::Class && i != innermost {
            continue;
        }
        if scope.globals.contains(name) {
            return resolve_in(stack, 0, name);
        }
        if i == 0 && !scope.reading_main && scope.main_only.contains(name) {
            continue;
        }
        if let Some(target) = scope.bindings.get(name) {
            return Res::Binding(i, target.clone());
        }
        if scope.locals.contains(name) {
            return Res::Local(i);
        }
    }
    Res::Unbound
}

fn resolve_in(stack: &[Scope], i: usize, name: &str) -> Res {
    match stack.get(i) {
        Some(scope) if i == 0 && !scope.reading_main && scope.main_only.contains(name) => {
            Res::Unbound
        }
        Some(scope) => match scope.bindings.get(name) {
            Some(target) => Res::Binding(i, target.clone()),
            None if scope.locals.contains(name) => Res::Local(i),
            None => Res::Unbound,
        },
        None => Res::Unbound,
    }
}

/// A dynamic import: the module it loads and the module it answers, which
/// for `__import__("a.b")` is the package `a`.
struct Dynamic {
    loaded: String,
    bound: String,
}

/// A call of `importlib.import_module` or `__import__`: None when `expr` is
/// not one, else the modules it names, or why it cannot be compiled.
fn dynamic_import(
    stack: &[Scope],
    constants: &HashMap<String, String>,
    expr: &Expr,
) -> Option<std::result::Result<Dynamic, String>> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let is_import_module = match &*call.func {
        Expr::Attribute(a) if a.attr.as_str() == "import_module" => match &*a.value {
            Expr::Name(n) => matches!(
                resolve(stack, n.id.as_str()),
                Res::Binding(_, Target::Module(m)) if m == "importlib"
            ),
            _ => false,
        },
        Expr::Name(n) => match resolve(stack, n.id.as_str()) {
            Res::Binding(_, Target::Member(m, f)) => m == "importlib" && f == "import_module",
            Res::Unbound if n.id.as_str() == "__import__" => false,
            _ => return None,
        },
        _ => return None,
    };
    let is_dunder =
        !is_import_module && matches!(&*call.func, Expr::Name(n) if n.id.as_str() == "__import__");
    if !is_import_module && !is_dunder {
        return None;
    }
    let spelled = if is_dunder {
        "__import__()"
    } else {
        "importlib.import_module()"
    };
    if call.args.len() != 1 || !call.keywords.is_empty() {
        return Some(Err(format!(
            "{spelled} is supported with the module's name as its only argument"
        )));
    }
    let name = match &call.args[0] {
        Expr::Constant(c) => match &c.value {
            Constant::Str(s) => Some(s.clone()),
            _ => None,
        },
        // A module-level constant, not a local of the same name.
        Expr::Name(n) if matches!(resolve(stack, n.id.as_str()), Res::Local(0)) => {
            constants.get(n.id.as_str()).cloned()
        }
        _ => None,
    };
    let Some(name) = name else {
        return Some(Err(format!(
            "{spelled} of a name computed at run time is not supported: every module is \
             linked into the program when it is compiled, and there is no loader at run time. \
             Hint: pass the module's name as a string literal"
        )));
    };
    if name.is_empty() {
        return Some(Err(format!(
            "{spelled} of an empty name: CPython raises ValueError"
        )));
    }
    if name.starts_with('.') {
        return Some(Err(format!(
            "{spelled} of the relative name '{name}' is not supported. Hint: name the module \
             absolutely"
        )));
    }
    let bound = if is_dunder {
        name.split('.').next().unwrap_or_default().to_string()
    } else {
        name.clone()
    };
    Some(Ok(Dynamic {
        loaded: name,
        bound,
    }))
}

/// An import found in a scope, before its scope is built.
struct Found {
    binding: String,
    target: Target,
    /// The statement it moves to module level as, when it moves.
    hoist: Stmt,
    range: TextRange,
    /// At the top of its scope, or only inside `try` bodies from there: the
    /// import runs whenever the code after it does.
    unconditional: bool,
    /// Inside the module's `if __name__ == "__main__":` block.
    in_main: bool,
}

/// What a body binds, gathered before its scope is built.
#[derive(Default)]
struct Gathered<'s> {
    imports: Vec<Found>,
    /// `name = value`, each a possible dynamic import.
    assigns: Vec<(String, &'s Expr, TextRange, bool, bool)>,
    stores: HashSet<String>,
    globals: HashSet<String>,
    nonlocals: HashSet<String>,
    /// A relative import or an import in a class body: refused where met.
    refused: Vec<(TextRange, &'static str)>,
}

fn import_stmt(name: &str, asname: Option<&str>, range: TextRange) -> Stmt {
    Stmt::Import(ast::StmtImport {
        range,
        names: vec![Alias {
            range,
            name: Identifier::new(name),
            asname: asname.map(Identifier::new),
        }],
    })
}

fn from_stmt(module: &str, name: &str, asname: Option<&str>, range: TextRange) -> Stmt {
    Stmt::ImportFrom(ast::StmtImportFrom {
        range,
        module: Some(Identifier::new(module)),
        names: vec![Alias {
            range,
            name: Identifier::new(name),
            asname: asname.map(Identifier::new),
        }],
        level: Some(ast::Int::new(0)),
    })
}

/// The statement a dynamic import binding `name` stands for.
fn dynamic_stmt(dynamic: &Dynamic, name: &str, range: TextRange) -> Stmt {
    if is_stdlib_module_path(&dynamic.loaded) {
        import_stmt(&dynamic.loaded, None, range)
    } else {
        let alias = (dynamic.bound != name).then_some(name);
        import_stmt(&dynamic.bound, alias, range)
    }
}

/// Every name a binding target stores to.
fn stored_names(target: &Expr, out: &mut HashSet<String>) {
    match target {
        Expr::Name(n) => {
            out.insert(n.id.to_string());
        }
        Expr::Tuple(t) => t.elts.iter().for_each(|e| stored_names(e, out)),
        Expr::List(l) => l.elts.iter().for_each(|e| stored_names(e, out)),
        Expr::Starred(s) => stored_names(&s.value, out),
        _ => {}
    }
}

fn param_names(args: &ast::Arguments) -> Vec<String> {
    let mut names: Vec<String> = args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
        .map(|a| a.def.arg.to_string())
        .collect();
    names.extend(args.vararg.iter().map(|a| a.arg.to_string()));
    names.extend(args.kwarg.iter().map(|a| a.arg.to_string()));
    names
}

/// Gather what `body` binds. `unconditional`: the statements run whenever
/// the scope's later code does; `in_main`: inside the module's main block.
fn gather<'s>(
    kind: Kind,
    body: &'s [Stmt],
    unconditional: bool,
    in_main: bool,
    g: &mut Gathered<'s>,
) {
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                g.stores.insert(f.name.to_string());
            }
            Stmt::AsyncFunctionDef(f) => {
                g.stores.insert(f.name.to_string());
            }
            Stmt::ClassDef(c) => {
                g.stores.insert(c.name.to_string());
            }
            Stmt::Import(i) => {
                if kind == Kind::Class {
                    g.refused.push((i.range, "an import in a class body"));
                    continue;
                }
                for a in &i.names {
                    let full = a.name.to_string();
                    let first = full.split('.').next().unwrap_or_default().to_string();
                    let (binding, module) = match &a.asname {
                        Some(asname) => (asname.to_string(), full.clone()),
                        None => (first.clone(), first.clone()),
                    };
                    let hoist = if is_stdlib_module_path(&full) {
                        import_stmt(&full, None, i.range)
                    } else {
                        import_stmt(&full, a.asname.as_ref().map(|n| n.as_str()), i.range)
                    };
                    g.imports.push(Found {
                        binding,
                        target: Target::Module(module),
                        hoist,
                        range: i.range,
                        unconditional,
                        in_main,
                    });
                }
            }
            Stmt::ImportFrom(i) => {
                if kind == Kind::Class {
                    g.refused.push((i.range, "an import in a class body"));
                    continue;
                }
                let relative = i.level.as_ref().is_some_and(|l| l.to_u32() > 0);
                let Some(module) = i.module.as_ref().filter(|_| !relative) else {
                    if kind != Kind::Module || in_main {
                        g.refused
                            .push((i.range, "a relative import inside a function"));
                    }
                    for a in &i.names {
                        g.stores
                            .insert(a.asname.as_ref().unwrap_or(&a.name).to_string());
                    }
                    continue;
                };
                for a in &i.names {
                    if a.name.as_str() == "*" {
                        continue;
                    }
                    let binding = a.asname.as_ref().unwrap_or(&a.name).to_string();
                    g.imports.push(Found {
                        binding,
                        target: Target::Member(module.to_string(), a.name.to_string()),
                        hoist: from_stmt(
                            module.as_str(),
                            a.name.as_str(),
                            a.asname.as_ref().map(|n| n.as_str()),
                            i.range,
                        ),
                        range: i.range,
                        unconditional,
                        in_main,
                    });
                }
            }
            Stmt::Assign(a) => {
                if let [Expr::Name(n)] = a.targets.as_slice() {
                    g.assigns
                        .push((n.id.to_string(), &a.value, a.range, unconditional, in_main));
                } else {
                    a.targets
                        .iter()
                        .for_each(|t| stored_names(t, &mut g.stores));
                }
            }
            Stmt::AugAssign(a) => stored_names(&a.target, &mut g.stores),
            Stmt::AnnAssign(a) => stored_names(&a.target, &mut g.stores),
            Stmt::Delete(d) => d
                .targets
                .iter()
                .for_each(|t| stored_names(t, &mut g.stores)),
            Stmt::For(f) => {
                stored_names(&f.target, &mut g.stores);
                gather(kind, &f.body, false, in_main, g);
                gather(kind, &f.orelse, false, in_main, g);
            }
            Stmt::AsyncFor(f) => {
                stored_names(&f.target, &mut g.stores);
                gather(kind, &f.body, false, in_main, g);
                gather(kind, &f.orelse, false, in_main, g);
            }
            Stmt::While(w) => {
                gather(kind, &w.body, false, in_main, g);
                gather(kind, &w.orelse, false, in_main, g);
            }
            Stmt::If(s) => {
                // At module level the only `if` is the main block, whose top
                // runs whenever it does.
                let main = kind == Kind::Module && !in_main;
                gather(kind, &s.body, main, in_main || main, g);
                gather(kind, &s.orelse, false, in_main || main, g);
            }
            Stmt::With(w) => {
                for item in &w.items {
                    if let Some(v) = &item.optional_vars {
                        stored_names(v, &mut g.stores);
                    }
                }
                gather(kind, &w.body, false, in_main, g);
            }
            Stmt::AsyncWith(w) => {
                for item in &w.items {
                    if let Some(v) = &item.optional_vars {
                        stored_names(v, &mut g.stores);
                    }
                }
                gather(kind, &w.body, false, in_main, g);
            }
            Stmt::Try(t) => {
                gather(kind, &t.body, unconditional, in_main, g);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    if let Some(name) = &h.name {
                        g.stores.insert(name.to_string());
                    }
                    gather(kind, &h.body, false, in_main, g);
                }
                gather(kind, &t.orelse, false, in_main, g);
                gather(kind, &t.finalbody, false, in_main, g);
            }
            Stmt::TryStar(t) => {
                gather(kind, &t.body, unconditional, in_main, g);
                for h in &t.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    if let Some(name) = &h.name {
                        g.stores.insert(name.to_string());
                    }
                    gather(kind, &h.body, false, in_main, g);
                }
                gather(kind, &t.orelse, false, in_main, g);
                gather(kind, &t.finalbody, false, in_main, g);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    gather(kind, &case.body, false, in_main, g);
                }
            }
            Stmt::Global(s) => g.globals.extend(s.names.iter().map(|n| n.to_string())),
            Stmt::Nonlocal(s) => g.nonlocals.extend(s.names.iter().map(|n| n.to_string())),
            _ => {}
        }
    }
}

/// An import moved to module level.
struct Hoist {
    /// The module-level name it binds.
    key: String,
    target: Target,
    stmt: Stmt,
    /// The function it was written in, or None for the main block.
    function: Option<String>,
    range: TextRange,
}

fn located(range: TextRange, function: Option<&str>, message: String) -> anyhow::Error {
    unsupported_feature(
        message,
        crate::core::parser::location_of(range.start().into(), function),
    )
    .into()
}

/// The module-level name an import binds, once its uses are rewritten: a
/// standard-library module is reached through its own name.
fn hoist_key(binding: &str, target: &Target) -> String {
    match target {
        Target::Module(path) if is_stdlib_module_path(path) => {
            path.split('.').next().unwrap_or_default().to_string()
        }
        _ => binding.to_string(),
    }
}

/// The scan before rewriting: every scope's bindings, by the offset of the
/// definition that opens it.
struct Prescan {
    stack: Vec<Scope>,
    functions: HashMap<u32, Scope>,
    classes: HashMap<u32, Scope>,
    hoists: Vec<Hoist>,
    constants: HashMap<String, String>,
    /// The modules imported at the top of the module (not in a function or
    /// the main block).
    top_level: HashSet<String>,
}

impl Prescan {
    /// Build the scope `body` opens.
    fn build(
        &mut self,
        kind: Kind,
        name: Option<String>,
        params: Vec<String>,
        body: &[Stmt],
    ) -> Result<Scope> {
        let mut g = Gathered::default();
        gather(kind, body, true, false, &mut g);
        let function = name.clone();
        if let Some((range, what)) = g.refused.first() {
            return Err(located(
                *range,
                function.as_deref(),
                format!("{what} is not supported. Hint: import at the top of the module"),
            ));
        }
        let mut scope = Scope::new(kind, name);
        let params: HashSet<String> = params.into_iter().collect();
        scope.locals.extend(params.iter().cloned());
        scope.locals.extend(g.stores.iter().cloned());
        scope.globals = g.globals;
        let mut ambiguous = HashSet::new();
        let mut found = g.imports;
        // A dynamic import assigned to a name is an import of it. The call
        // sees the imports this scope makes, and every name it assigns as a
        // local (so a local `NAME` is not taken for a module constant).
        let assigned: HashSet<String> = g.assigns.iter().map(|a| a.0.clone()).collect();
        scope.locals.extend(assigned.iter().cloned());
        for f in &found {
            scope
                .bindings
                .entry(f.binding.clone())
                .or_insert_with(|| f.target.clone());
        }
        self.stack.push(scope);
        let mut dynamic_names = HashSet::new();
        let mut plain = HashSet::new();
        for (target, value, range, unconditional, in_main) in g.assigns {
            // A class attribute is not a binding to move; the import is
            // refused where it is met.
            if kind == Kind::Module {
                self.stack.last_mut().expect("pushed").reading_main = in_main;
            }
            let dynamic = match kind {
                Kind::Class => None,
                _ => dynamic_import(&self.stack, &self.constants, value),
            };
            match dynamic {
                Some(Ok(dynamic)) => {
                    dynamic_names.insert(target.clone());
                    found.push(Found {
                        binding: target.clone(),
                        target: Target::Module(dynamic.bound.clone()),
                        hoist: dynamic_stmt(&dynamic, &target, range),
                        range,
                        unconditional,
                        in_main,
                    });
                }
                _ => {
                    plain.insert(target);
                }
            }
        }
        let mut scope = self.stack.pop().expect("pushed");
        scope.bindings.clear();
        scope.reading_main = false;
        // A name assigned only by dynamic imports is a binding, not a local.
        for name in &dynamic_names {
            if !plain.contains(name) && !g.stores.contains(name) && !params.contains(name) {
                scope.locals.remove(name);
            }
        }

        for f in &found {
            match scope.bindings.get(&f.binding) {
                None => {
                    scope.bindings.insert(f.binding.clone(), f.target.clone());
                }
                Some(t) if *t == f.target => {}
                Some(_) if kind == Kind::Module => {
                    ambiguous.insert(f.binding.clone());
                }
                Some(_) => {
                    return Err(located(
                        f.range,
                        function.as_deref(),
                        format!(
                            "'{}' is imported as two different things in this function, \
                             which is not supported. Hint: give one import another name",
                            f.binding
                        ),
                    ))
                }
            }
        }
        let names: Vec<String> = scope.bindings.keys().cloned().collect();
        for binding in names {
            let assigned = scope.locals.contains(&binding);
            let declared = scope.globals.contains(&binding) || g.nonlocals.contains(&binding);
            if kind == Kind::Module {
                if assigned || ambiguous.contains(&binding) {
                    scope.bindings.remove(&binding);
                    scope.locals.insert(binding);
                }
            } else if assigned || declared {
                let f = found.iter().find(|f| f.binding == binding).expect("found");
                return Err(located(
                    f.range,
                    function.as_deref(),
                    format!(
                        "'{binding}' is bound by an import and also {} in this function, \
                         which is not supported. Hint: give the import another name",
                        if declared {
                            "declared global or nonlocal"
                        } else {
                            "assigned"
                        }
                    ),
                ));
            }
        }

        if kind == Kind::Module {
            for f in &found {
                let top = found.iter().any(|g| g.binding == f.binding && !g.in_main);
                if f.in_main && !top && scope.bindings.contains_key(&f.binding) {
                    scope.main_only.insert(f.binding.clone());
                }
            }
        }

        // Imports that move to module level: a function's, and the main
        // block's.
        for f in found {
            if kind == Kind::Module && !f.in_main {
                self.top_level.insert(stmt_module(&f.hoist));
            }
            let moves = kind != Kind::Module || f.in_main;
            if !moves || !scope.bindings.contains_key(&f.binding) {
                continue;
            }
            if !f.unconditional {
                return Err(located(
                    f.range,
                    function.as_deref(),
                    format!(
                        "an import inside a block that may not run (an 'if', a loop, a \
                         'with', or an 'except' or 'finally' body) is not supported: \
                         modules are linked when the program starts, so '{}' would be \
                         bound even where the import did not run. Hint: import at the top \
                         of the function or module",
                        f.binding
                    ),
                ));
            }
            self.hoists.push(Hoist {
                key: hoist_key(&f.binding, &f.target),
                target: f.target,
                stmt: f.hoist,
                function: function.clone().filter(|_| kind != Kind::Module),
                range: f.range,
            });
        }
        Ok(scope)
    }

    /// Scan the definitions in `body`, at any depth of its statements.
    fn defs(&mut self, body: &[Stmt]) -> Result<()> {
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    self.def(f.range, &f.name, &f.args, &f.body, Kind::Function)?
                }
                Stmt::AsyncFunctionDef(f) => {
                    self.def(f.range, &f.name, &f.args, &f.body, Kind::Function)?
                }
                Stmt::ClassDef(c) => {
                    let scope = self.build(Kind::Class, None, Vec::new(), &c.body)?;
                    self.stack.push(scope);
                    let scanned = self.defs(&c.body);
                    let scope = self.stack.pop().expect("pushed");
                    scanned?;
                    self.classes.insert(c.range.start().into(), scope);
                }
                Stmt::If(s) => {
                    self.defs(&s.body)?;
                    self.defs(&s.orelse)?;
                }
                Stmt::For(s) => {
                    self.defs(&s.body)?;
                    self.defs(&s.orelse)?;
                }
                Stmt::While(s) => {
                    self.defs(&s.body)?;
                    self.defs(&s.orelse)?;
                }
                Stmt::AsyncFor(s) => {
                    self.defs(&s.body)?;
                    self.defs(&s.orelse)?;
                }
                Stmt::With(s) => self.defs(&s.body)?,
                Stmt::AsyncWith(s) => self.defs(&s.body)?,
                Stmt::Match(m) => {
                    for case in &m.cases {
                        self.defs(&case.body)?;
                    }
                }
                Stmt::Try(t) => {
                    self.defs(&t.body)?;
                    for h in &t.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = h;
                        self.defs(&h.body)?;
                    }
                    self.defs(&t.orelse)?;
                    self.defs(&t.finalbody)?;
                }
                Stmt::TryStar(t) => {
                    self.defs(&t.body)?;
                    for h in &t.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = h;
                        self.defs(&h.body)?;
                    }
                    self.defs(&t.orelse)?;
                    self.defs(&t.finalbody)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn def(
        &mut self,
        range: TextRange,
        name: &Identifier,
        args: &ast::Arguments,
        body: &[Stmt],
        kind: Kind,
    ) -> Result<()> {
        let scope = self.build(kind, Some(name.to_string()), param_names(args), body)?;
        self.stack.push(scope);
        let scanned = self.defs(body);
        let scope = self.stack.pop().expect("pushed");
        scanned?;
        self.functions.insert(range.start().into(), scope);
        Ok(())
    }
}

/// Module-level names assigned a string once and never rebound: a
/// dynamic import of one names its module as a literal would.
fn module_constants(ast: &Suite) -> HashMap<String, String> {
    fn globals(body: &[Stmt], out: &mut HashSet<String>) {
        for stmt in body {
            match stmt {
                Stmt::Global(s) => out.extend(s.names.iter().map(|n| n.to_string())),
                Stmt::FunctionDef(f) => globals(&f.body, out),
                Stmt::AsyncFunctionDef(f) => globals(&f.body, out),
                Stmt::ClassDef(c) => globals(&c.body, out),
                Stmt::If(s) => {
                    globals(&s.body, out);
                    globals(&s.orelse, out);
                }
                Stmt::For(s) => globals(&s.body, out),
                Stmt::While(s) => globals(&s.body, out),
                Stmt::With(s) => globals(&s.body, out),
                Stmt::Try(t) => {
                    globals(&t.body, out);
                    for h in &t.handlers {
                        let ast::ExceptHandler::ExceptHandler(h) = h;
                        globals(&h.body, out);
                    }
                    globals(&t.orelse, out);
                    globals(&t.finalbody, out);
                }
                _ => {}
            }
        }
    }
    let mut g = Gathered::default();
    gather(Kind::Module, ast, true, false, &mut g);
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (name, ..) in &g.assigns {
        *counts.entry(name.as_str()).or_default() += 1;
    }
    let mut declared = HashSet::new();
    globals(ast, &mut declared);
    let mut values = HashMap::new();
    for stmt in ast {
        if let Stmt::Assign(a) = stmt {
            if let ([Expr::Name(n)], Expr::Constant(c)) = (a.targets.as_slice(), &*a.value) {
                if let Constant::Str(s) = &c.value {
                    let name = n.id.as_str();
                    if counts.get(name) == Some(&1)
                        && !g.stores.contains(name)
                        && !declared.contains(name)
                    {
                        values.insert(name.to_string(), s.clone());
                    }
                }
            }
        }
    }
    values
}

/// The rewrite itself.
struct Resolver {
    stack: Vec<Scope>,
    functions: HashMap<u32, Scope>,
    classes: HashMap<u32, Scope>,
    constants: HashMap<String, String>,
    /// Names a function's import binds, which reads elsewhere must not see:
    /// name -> the function.
    hoisted: HashMap<String, String>,
    /// Statements to add at module level, and what each binds there.
    added: Vec<Stmt>,
    added_keys: HashMap<String, Target>,
    /// Inside the module's main block.
    in_main: usize,
    /// User modules first imported in a function or the main block, and
    /// where.
    deferred: Vec<(String, String)>,
    top_level: HashSet<String>,
}

impl Resolver {
    /// Record that `module` is first imported at `place`, unless it is a
    /// standard-library module or the module imports it at the top.
    fn defer(&mut self, module: String, place: String) {
        if is_stdlib_module_path(&module)
            || self.top_level.contains(&module)
            || self.deferred.iter().any(|(m, _)| *m == module)
        {
            return;
        }
        self.deferred.push((module, place));
    }

    fn function_name(&self) -> Option<&str> {
        self.stack
            .iter()
            .rev()
            .find(|s| s.kind == Kind::Function)
            .and_then(|s| s.name.as_deref())
    }

    fn error(&self, range: TextRange, message: String) -> anyhow::Error {
        located(range, self.function_name(), message)
    }

    fn scanned_function(&mut self, range: TextRange) -> Result<Scope> {
        self.functions
            .remove(&range.start().into())
            .ok_or_else(|| self.error(range, UNSCANNED.to_string()))
    }

    fn kind(&self) -> Kind {
        self.stack.last().map(|s| s.kind).unwrap_or(Kind::Module)
    }

    /// A read of a function's binding must come after its import.
    fn check_bound(&self, name: &str, range: TextRange) -> Result<()> {
        if let Res::Binding(i, _) = resolve(&self.stack, name) {
            let scope = &self.stack[i];
            if scope.kind == Kind::Function && !scope.bound.contains(name) {
                return Err(self.error(
                    range,
                    format!(
                        "'{name}' is read before the import that binds it in this function: \
                         CPython raises UnboundLocalError"
                    ),
                ));
            }
        }
        Ok(())
    }

    /// The expression for a standard-library module, by its own name.
    fn module_expr(&self, path: &str, range: TextRange) -> Result<Expr> {
        let mut parts = path.split('.');
        let root = parts.next().unwrap_or_default();
        match resolve(&self.stack, root) {
            Res::Unbound => {}
            Res::Binding(_, Target::Module(m)) if m == root => {}
            _ => {
                return Err(self.error(
                    range,
                    format!(
                        "the module '{path}' is reached here through another name, but '{root}' \
                         is bound to something else in this scope, which is not supported. \
                         Hint: rename the variable '{root}'"
                    ),
                ))
            }
        }
        let mut expr = Expr::Name(ast::ExprName {
            range,
            id: Identifier::new(root),
            ctx: ExprContext::Load,
        });
        for part in parts {
            expr = Expr::Attribute(ast::ExprAttribute {
                range,
                value: Box::new(expr),
                attr: Identifier::new(part),
                ctx: ExprContext::Load,
            });
        }
        Ok(expr)
    }

    /// Add `stmt`, which binds `key` to `target`, at module level.
    fn add(&mut self, key: String, target: Target, stmt: Stmt, range: TextRange) -> Result<()> {
        let module = &self.stack[0];
        let clash = module.locals.contains(&key)
            || module
                .bindings
                .get(&key)
                .is_some_and(|t| !same_binding(&key, t, &target))
            || self
                .added_keys
                .get(&key)
                .is_some_and(|t| !same_binding(&key, t, &target));
        if clash {
            return Err(self.error(
                range,
                format!(
                    "this import binds '{key}', which the module binds to something else, \
                     which is not supported. Hint: give the import another name"
                ),
            ));
        }
        self.added_keys.entry(key).or_insert(target);
        let shape = stmt_shape(&stmt);
        if !self.added.iter().any(|s| stmt_shape(s) == shape) {
            self.added.push(stmt);
        }
        Ok(())
    }

    fn load(&mut self, n: ast::ExprName) -> Result<Expr> {
        let name = n.id.as_str();
        match resolve(&self.stack, name) {
            Res::Binding(_, target) => {
                self.check_bound(name, n.range)?;
                match target {
                    Target::Module(path) if is_stdlib_module_path(&path) => {
                        self.module_expr(&path, n.range)
                    }
                    _ => Ok(Expr::Name(n)),
                }
            }
            Res::Local(_) => Ok(Expr::Name(n)),
            Res::Unbound => {
                if self.stack[0].main_only.contains(name) {
                    return Err(self.error(
                        n.range,
                        format!(
                            "name '{name}' is imported only in {MAIN_BLOCK_PLACE}, and a name \
                             that block binds is its own here, where CPython makes it a module \
                             global. Hint: import it at the top of the module"
                        ),
                    ));
                }
                if is_stdlib_module(name) {
                    return Err(self.error(
                        n.range,
                        format!(
                            "name '{name}' is not defined here: CPython raises NameError. \
                             Hint: import {name}"
                        ),
                    ));
                }
                if let Some(place) = self.hoisted.get(name) {
                    return Err(self.error(
                        n.range,
                        format!(
                            "name '{name}' is not defined here: it is imported in {place}, \
                             which binds it there only. Hint: import it here too, or at the \
                             top of the module"
                        ),
                    ));
                }
                Ok(Expr::Name(n))
            }
        }
    }

    /// Check the name a dynamic import's callee is read through.
    fn check_callee(&self, func: &Expr) -> Result<()> {
        let name = match func {
            Expr::Attribute(a) => match &*a.value {
                Expr::Name(n) => n,
                _ => return Ok(()),
            },
            Expr::Name(n) => n,
            _ => return Ok(()),
        };
        self.check_bound(name.id.as_str(), name.range)
    }

    /// A dynamic import used as a value: the module itself.
    fn dynamic_value(&mut self, call: &ast::ExprCall, dynamic: Dynamic) -> Result<Expr> {
        self.check_callee(&call.func)?;
        let range = call.range;
        if self.kind() == Kind::Class {
            return Err(self.error(
                range,
                "an import in a class body is not supported. Hint: import at the top of the \
                 module"
                    .to_string(),
            ));
        }
        if self.kind() != Kind::Module || self.in_main > 0 {
            let place = match self.function_name() {
                Some(f) => format!("function '{f}'"),
                None => MAIN_BLOCK_PLACE.to_string(),
            };
            self.defer(dynamic.loaded.clone(), place);
        }
        if is_stdlib_module_path(&dynamic.loaded) {
            let key = dynamic
                .bound
                .split('.')
                .next()
                .unwrap_or_default()
                .to_string();
            self.add(
                key,
                Target::Module(dynamic.bound.clone()),
                import_stmt(&dynamic.loaded, None, range),
                range,
            )?;
            return self.module_expr(&dynamic.bound, range);
        }
        match resolve(&self.stack, &dynamic.bound) {
            Res::Unbound => {}
            Res::Binding(_, Target::Module(m)) if m == dynamic.bound => {}
            _ => {
                return Err(self.error(
                    range,
                    format!(
                        "the module '{}' is imported here as a value, but '{}' is bound to \
                         something else in this scope, which is not supported. Hint: assign \
                         the import to a name",
                        dynamic.loaded, dynamic.bound
                    ),
                ))
            }
        }
        self.add(
            dynamic.bound.clone(),
            Target::Module(dynamic.bound.clone()),
            import_stmt(&dynamic.bound, None, range),
            range,
        )?;
        Ok(Expr::Name(ast::ExprName {
            range,
            id: Identifier::new(dynamic.bound),
            ctx: ExprContext::Load,
        }))
    }

    fn import(&mut self, i: ast::StmtImport) -> Result<Stmt> {
        let range = i.range;
        if self.kind() != Kind::Module || self.in_main > 0 {
            let scope = self.stack.last_mut().expect("scope");
            for a in &i.names {
                let binding = match &a.asname {
                    Some(n) => n.to_string(),
                    None => a.name.split('.').next().unwrap_or_default().to_string(),
                };
                scope.bound.insert(binding);
            }
            return Ok(Stmt::Pass(ast::StmtPass { range }));
        }
        // A standard-library module's alias reads through the module's own
        // name, so the statement binds that.
        let module = &self.stack[0];
        let names = i
            .names
            .into_iter()
            .map(|a| match &a.asname {
                Some(asname)
                    if is_stdlib_module_path(a.name.as_str())
                        && module.bindings.get(asname.as_str())
                            == Some(&Target::Module(a.name.to_string())) =>
                {
                    Alias {
                        range: a.range,
                        name: a.name,
                        asname: None,
                    }
                }
                _ => a,
            })
            .collect();
        Ok(Stmt::Import(ast::StmtImport { range, names }))
    }

    fn import_from(&mut self, i: ast::StmtImportFrom) -> Result<Stmt> {
        let range = i.range;
        if self.kind() != Kind::Module || self.in_main > 0 {
            let scope = self.stack.last_mut().expect("scope");
            for a in &i.names {
                scope
                    .bound
                    .insert(a.asname.as_ref().unwrap_or(&a.name).to_string());
            }
            return Ok(Stmt::Pass(ast::StmtPass { range }));
        }
        Ok(Stmt::ImportFrom(i))
    }

    /// `name = <dynamic import>` where the scan bound `name` to it.
    fn dynamic_binding(&mut self, a: &ast::StmtAssign) -> Option<Result<Stmt>> {
        let [Expr::Name(n)] = a.targets.as_slice() else {
            return None;
        };
        let name = n.id.as_str();
        let scope = self.stack.last()?;
        scope.bindings.get(name)?;
        let dynamic = match dynamic_import(&self.stack, &self.constants, &a.value)? {
            Ok(d) => d,
            Err(message) => return Some(Err(self.error(a.range, message))),
        };
        if let Expr::Call(call) = &*a.value {
            if let Err(e) = self.check_callee(&call.func) {
                return Some(Err(e));
            }
        }
        let range = a.range;
        if self.kind() != Kind::Module || self.in_main > 0 {
            self.stack
                .last_mut()
                .expect("scope")
                .bound
                .insert(name.to_string());
            return Some(Ok(Stmt::Pass(ast::StmtPass { range })));
        }
        Some(Ok(dynamic_stmt(&dynamic, name, range)))
    }

    fn comprehension(
        &mut self,
        generators: Vec<ast::Comprehension>,
    ) -> Result<Vec<ast::Comprehension>> {
        let mut out = Vec::with_capacity(generators.len());
        for (i, g) in generators.into_iter().enumerate() {
            // The first iterable is evaluated in the enclosing scope.
            let iter = if i == 0 {
                let scope = self.stack.pop().expect("comprehension scope");
                let iter = self.fold(g.iter);
                self.stack.push(scope);
                iter?
            } else {
                self.fold(g.iter)?
            };
            out.push(ast::Comprehension {
                range: g.range,
                target: self.fold(g.target)?,
                iter,
                ifs: self.fold(g.ifs)?,
                is_async: g.is_async,
            });
        }
        Ok(out)
    }

    fn push_inner(&mut self, names: HashSet<String>) {
        let mut scope = Scope::new(Kind::Inner, None);
        scope.locals = names;
        self.stack.push(scope);
    }
}

/// True when binding `key` to `a` and to `b` reach the same thing: a
/// standard-library module's name bound by importing it or a submodule.
fn same_binding(key: &str, a: &Target, b: &Target) -> bool {
    match (a, b) {
        (Target::Module(x), Target::Module(y))
            if is_stdlib_module_path(x) && is_stdlib_module_path(y) =>
        {
            x.split('.').next() == Some(key) && y.split('.').next() == Some(key)
        }
        _ => a == b,
    }
}

fn comprehension_targets(generators: &[ast::Comprehension]) -> HashSet<String> {
    let mut names = HashSet::new();
    for g in generators {
        stored_names(&g.target, &mut names);
    }
    names
}

impl Fold<TextRange> for Resolver {
    type TargetU = TextRange;
    type Error = anyhow::Error;
    type UserContext = ();

    fn will_map_user(&mut self, _user: &TextRange) -> Self::UserContext {}

    fn map_user(&mut self, user: TextRange, _context: ()) -> Result<TextRange> {
        Ok(user)
    }

    fn fold_stmt(&mut self, stmt: Stmt) -> Result<Stmt> {
        match stmt {
            Stmt::FunctionDef(f) => {
                let decorator_list = self.fold(f.decorator_list)?;
                let args = Box::new(self.fold(*f.args)?);
                let returns = self.fold(f.returns)?;
                let scope = self.scanned_function(f.range)?;
                self.stack.push(scope);
                let body = self.fold(f.body);
                self.stack.pop();
                Ok(Stmt::FunctionDef(ast::StmtFunctionDef {
                    range: f.range,
                    name: f.name,
                    args,
                    body: body?,
                    decorator_list,
                    returns,
                    type_comment: f.type_comment,
                    type_params: f.type_params,
                }))
            }
            Stmt::AsyncFunctionDef(f) => {
                let decorator_list = self.fold(f.decorator_list)?;
                let args = Box::new(self.fold(*f.args)?);
                let returns = self.fold(f.returns)?;
                let scope = self.scanned_function(f.range)?;
                self.stack.push(scope);
                let body = self.fold(f.body);
                self.stack.pop();
                Ok(Stmt::AsyncFunctionDef(ast::StmtAsyncFunctionDef {
                    range: f.range,
                    name: f.name,
                    args,
                    body: body?,
                    decorator_list,
                    returns,
                    type_comment: f.type_comment,
                    type_params: f.type_params,
                }))
            }
            Stmt::ClassDef(c) => {
                let decorator_list = self.fold(c.decorator_list)?;
                let bases = self.fold(c.bases)?;
                let keywords = self.fold(c.keywords)?;
                let scope = self
                    .classes
                    .remove(&c.range.start().into())
                    .ok_or_else(|| self.error(c.range, UNSCANNED.to_string()))?;
                self.stack.push(scope);
                let body = self.fold(c.body);
                self.stack.pop();
                Ok(Stmt::ClassDef(ast::StmtClassDef {
                    range: c.range,
                    name: c.name,
                    bases,
                    keywords,
                    body: body?,
                    decorator_list,
                    type_params: c.type_params,
                }))
            }
            Stmt::Import(i) => self.import(i),
            Stmt::ImportFrom(i) => self.import_from(i),
            Stmt::Assign(a) => match self.dynamic_binding(&a) {
                Some(stmt) => stmt,
                None => ast::fold::fold_stmt(self, Stmt::Assign(a)),
            },
            // A dynamic import whose module is not kept loads what the
            // module's imports already link.
            Stmt::Expr(e)
                if (self.kind() != Kind::Module || self.in_main > 0)
                    && self.kind() != Kind::Class
                    && dynamic_import(&self.stack, &self.constants, &e.value)
                        .is_some_and(|d| d.is_ok()) =>
            {
                if let Expr::Call(call) = &*e.value {
                    self.check_callee(&call.func)?;
                }
                Ok(Stmt::Pass(ast::StmtPass { range: e.range }))
            }
            Stmt::If(s) if self.kind() == Kind::Module && self.in_main == 0 => {
                self.in_main += 1;
                self.stack[0].reading_main = true;
                let folded = ast::fold::fold_stmt(self, Stmt::If(s));
                self.stack[0].reading_main = false;
                self.in_main -= 1;
                folded
            }
            other => ast::fold::fold_stmt(self, other),
        }
    }

    fn fold_expr(&mut self, expr: Expr) -> Result<Expr> {
        match expr {
            Expr::Name(n) if n.ctx == ExprContext::Load => self.load(n),
            Expr::Call(call) => {
                let expr = Expr::Call(call);
                match dynamic_import(&self.stack, &self.constants, &expr) {
                    Some(Ok(dynamic)) => {
                        let Expr::Call(call) = expr else {
                            unreachable!()
                        };
                        self.dynamic_value(&call, dynamic)
                    }
                    Some(Err(message)) => Err(self.error(expr_range(&expr), message)),
                    None => ast::fold::fold_expr(self, expr),
                }
            }
            Expr::Lambda(l) => {
                let args = Box::new(self.fold(*l.args)?);
                self.push_inner(param_names(&args).into_iter().collect());
                let body = self.fold(l.body);
                self.stack.pop();
                Ok(Expr::Lambda(ast::ExprLambda {
                    range: l.range,
                    args,
                    body: body?,
                }))
            }
            Expr::ListComp(c) => {
                self.push_inner(comprehension_targets(&c.generators));
                let generators = self.comprehension(c.generators);
                let elt = generators.as_ref().ok().map(|_| self.fold(c.elt));
                self.stack.pop();
                Ok(Expr::ListComp(ast::ExprListComp {
                    range: c.range,
                    generators: generators?,
                    elt: elt.expect("folded")?,
                }))
            }
            Expr::SetComp(c) => {
                self.push_inner(comprehension_targets(&c.generators));
                let generators = self.comprehension(c.generators);
                let elt = generators.as_ref().ok().map(|_| self.fold(c.elt));
                self.stack.pop();
                Ok(Expr::SetComp(ast::ExprSetComp {
                    range: c.range,
                    generators: generators?,
                    elt: elt.expect("folded")?,
                }))
            }
            Expr::GeneratorExp(c) => {
                self.push_inner(comprehension_targets(&c.generators));
                let generators = self.comprehension(c.generators);
                let elt = generators.as_ref().ok().map(|_| self.fold(c.elt));
                self.stack.pop();
                Ok(Expr::GeneratorExp(ast::ExprGeneratorExp {
                    range: c.range,
                    generators: generators?,
                    elt: elt.expect("folded")?,
                }))
            }
            Expr::DictComp(c) => {
                self.push_inner(comprehension_targets(&c.generators));
                let generators = self.comprehension(c.generators);
                let parts = generators
                    .as_ref()
                    .ok()
                    .map(|_| (self.fold(c.key), self.fold(c.value)));
                self.stack.pop();
                let (key, value) = parts.expect("folded");
                Ok(Expr::DictComp(ast::ExprDictComp {
                    range: c.range,
                    generators: generators?,
                    key: key?,
                    value: value?,
                }))
            }
            other => ast::fold::fold_expr(self, other),
        }
    }
}

/// An import statement as written, without its position.
fn stmt_shape(stmt: &Stmt) -> String {
    let aliases = |names: &[Alias]| {
        names
            .iter()
            .map(|a| match &a.asname {
                Some(asname) => format!("{} as {asname}", a.name),
                None => a.name.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    match stmt {
        Stmt::Import(i) => format!("import {}", aliases(&i.names)),
        Stmt::ImportFrom(i) => format!(
            "from {} import {}",
            i.module.as_ref().map(|m| m.as_str()).unwrap_or_default(),
            aliases(&i.names)
        ),
        _ => String::new(),
    }
}

fn expr_range(expr: &Expr) -> TextRange {
    use ast::Ranged;
    expr.range()
}

/// Resolve the module bindings of `ast` (see the module documentation).
pub(crate) fn resolve_module_bindings(ast: &Suite) -> Result<Resolved> {
    let constants = module_constants(ast);
    let mut prescan = Prescan {
        stack: Vec::new(),
        functions: HashMap::new(),
        classes: HashMap::new(),
        hoists: Vec::new(),
        constants,
        top_level: HashSet::new(),
    };
    let module = prescan.build(Kind::Module, None, Vec::new(), ast)?;
    prescan.stack.push(module);
    prescan.defs(ast)?;
    let module = prescan.stack.pop().expect("pushed");

    // A module-level alias of a standard-library module binds the module's
    // own name once rewritten, which nothing else at module level may.
    for (binding, target) in &module.bindings {
        let key = hoist_key(binding, target);
        if key != *binding && module.locals.contains(&key) {
            return Err(unsupported_feature(
                format!(
                    "the module imported as '{binding}' is reached through its own name '{key}', \
                     which this module also defines, which is not supported. Hint: rename \
                     '{key}'"
                ),
                None,
            )
            .into());
        }
    }

    // The names an import in a function or the main block binds there only.
    let mut hoisted = HashMap::new();
    for h in &prescan.hoists {
        let place = match &h.function {
            Some(function) => format!("function '{function}'"),
            None => MAIN_BLOCK_PLACE.to_string(),
        };
        let own = match &h.function {
            Some(_) => !module.bindings.contains_key(&h.key),
            None => module
                .main_only
                .iter()
                .any(|b| hoist_key(b, &h.target) == h.key),
        };
        if own && !is_stdlib_module_path_target(&h.target) {
            hoisted.insert(h.key.clone(), place);
        }
    }
    let hoists = std::mem::take(&mut prescan.hoists);
    let mut resolver = Resolver {
        stack: vec![module],
        functions: prescan.functions,
        classes: prescan.classes,
        constants: prescan.constants,
        hoisted,
        added: Vec::new(),
        added_keys: HashMap::new(),
        in_main: 0,
        deferred: Vec::new(),
        top_level: std::mem::take(&mut prescan.top_level),
    };
    // A standard-library alias at module level binds the module's own name.
    let canonical: Vec<(String, Target)> = resolver.stack[0]
        .bindings
        .iter()
        .filter(|(_, t)| is_stdlib_module_path_target(t))
        .map(|(b, t)| (hoist_key(b, t), t.clone()))
        .collect();
    for (key, target) in canonical {
        resolver.added_keys.entry(key).or_insert(target);
    }
    for h in &hoists {
        let place = match &h.function {
            Some(f) => format!("function '{f}'"),
            None => MAIN_BLOCK_PLACE.to_string(),
        };
        resolver.defer(stmt_module(&h.stmt), place);
    }
    for h in hoists {
        let function = h.function.clone();
        resolver
            .add(h.key, h.target, h.stmt, h.range)
            .map_err(|e| {
                // Located in the function that wrote the import.
                match function {
                    Some(f) => e.context(format!("in function '{f}'")),
                    None => e,
                }
            })?;
    }
    let mut body = Vec::with_capacity(ast.len());
    for stmt in ast.iter().cloned() {
        body.push(resolver.fold_stmt(stmt)?);
    }
    let mut out = std::mem::take(&mut resolver.added);
    out.extend(body);
    Ok(Resolved {
        ast: out,
        deferred: resolver.deferred,
    })
}

/// A module with its bindings resolved.
pub(crate) struct Resolved {
    pub(crate) ast: Suite,
    /// The user modules it imports only in a function or in its main block,
    /// each with the place: CPython runs such a module's top level when that
    /// code first runs, where here it runs when the program starts.
    pub(crate) deferred: Vec<(String, String)>,
}

/// A definition the scan did not reach: a statement holding a body that
/// `Prescan::defs` does not walk.
const UNSCANNED: &str = "internal error: a definition here was not scanned for its imports";

const MAIN_BLOCK_PLACE: &str = "the 'if __name__ == \"__main__\":' block";

/// The module an import statement loads.
fn stmt_module(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Import(i) => i
            .names
            .first()
            .map(|a| a.name.to_string())
            .unwrap_or_default(),
        Stmt::ImportFrom(i) => i.module.as_ref().map(|m| m.to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

fn is_stdlib_module_path_target(target: &Target) -> bool {
    matches!(target, Target::Module(path) if is_stdlib_module_path(path))
}

/// Every module `ast` imports, at any depth, including the constant names of
/// its dynamic imports, for finding the user modules a program links. A
/// relative import keeps its leading dots.
pub(crate) fn module_dependencies(ast: &Suite) -> Vec<String> {
    struct Collect {
        constants: HashMap<String, String>,
        found: Vec<String>,
    }
    impl Collect {
        fn push(&mut self, name: String) {
            if !name.is_empty() && !self.found.contains(&name) {
                self.found.push(name);
            }
        }
    }
    impl Fold<TextRange> for Collect {
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
            match &stmt {
                Stmt::Import(i) => {
                    for a in &i.names {
                        self.push(a.name.to_string());
                    }
                }
                Stmt::ImportFrom(i) => {
                    let dots = ".".repeat(i.level.as_ref().map_or(0, |l| l.to_usize()));
                    let module = i.module.as_ref().map(|m| m.to_string()).unwrap_or_default();
                    self.push(format!("{dots}{module}"));
                }
                _ => {}
            }
            ast::fold::fold_stmt(self, stmt)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            if let Expr::Call(call) = &expr {
                let named = match &*call.func {
                    Expr::Attribute(a) => a.attr.as_str() == "import_module",
                    Expr::Name(n) => matches!(n.id.as_str(), "import_module" | "__import__"),
                    _ => false,
                };
                if named {
                    let name = match call.args.first() {
                        Some(Expr::Constant(c)) => match &c.value {
                            Constant::Str(s) => Some(s.clone()),
                            _ => None,
                        },
                        Some(Expr::Name(n)) => self.constants.get(n.id.as_str()).cloned(),
                        _ => None,
                    };
                    if let Some(name) = name {
                        self.push(name);
                    }
                }
            }
            ast::fold::fold_expr(self, expr)
        }
    }
    let mut collect = Collect {
        constants: module_constants(ast),
        found: Vec::new(),
    };
    for stmt in ast.iter().cloned() {
        let _ = collect.fold_stmt(stmt);
    }
    collect.found
}

/// The first definition at the top of `module` that runs code when the
/// module starts (a call, a comprehension, a subscript, a decorator, or a
/// metaclass), if any: when it runs is observable, and CPython runs it at the
/// first import. The standard-library passes' own definitions are not counted.
pub(crate) fn top_level_code(module: &crate::ir::IRModule) -> Option<String> {
    use crate::ir::{IRBody, IRExpr, IRStatement};
    fn runs_code(expr: &IRExpr) -> bool {
        let mut body = IRBody {
            statements: vec![IRStatement::Expression(expr.clone())],
        };
        let mut found = false;
        let _ = crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
            found |= matches!(
                e,
                IRExpr::FunctionCall { .. }
                    | IRExpr::MethodCall { .. }
                    | IRExpr::Comprehension { .. }
                    | IRExpr::Indexing { .. }
                    | IRExpr::Slicing { .. }
            );
            Ok(())
        });
        found
    }
    for var in &module.variables {
        if !var.name.starts_with("__") && runs_code(&var.value) {
            return Some(format!("{} = ...", var.name));
        }
    }
    for class in module.classes.iter().filter(|c| !c.name.contains('.')) {
        if class.metaclass.is_some() {
            return Some(format!("class {}", class.name));
        }
        for var in &class.class_vars {
            if runs_code(&var.value) {
                return Some(format!("{}.{} = ...", class.name, var.name));
            }
        }
    }
    module
        .user_decorations
        .first()
        .map(|d| format!("the decorators of {}", d.function))
}
