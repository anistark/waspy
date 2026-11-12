//! The order a program's top levels run in.
//!
//! CPython runs a module's top level when the module is first imported, and
//! runs its statements in source order: a class statement evaluates its class
//! variables where it stands, and an `import` of a module not yet loaded runs
//! that module's top level right there. Each file the converter lowers keeps
//! its top level in that order (`IRModule::sources`), and this builds the
//! module's start from it:
//!
//! - The entry file's top level runs when the program starts, and each module
//!   it imports at its top level runs at that import, the first time, and so
//!   on through their imports. A module imported twice runs once.
//! - A module first imported in a function or the main block (an import there
//!   leaves a call of `IMPORT_MARKER_FN` in its place, `ir::imports`) starts
//!   late: it gets a start function of its own, run once behind a flag, which
//!   the marker calls. A module it imports at its top level starts from there.
//! - A file nothing imports (a project's other files, whose functions the
//!   flat namespace links) starts with the program, after the entry, as all of
//!   them used to.
//!
//! Only definitions that are evaluated once into a global take part; a plain
//! constant is inlined where it is read. Classes no file states (the builtin
//! exceptions, and the classes the standard-library passes make) have their
//! class variables evaluated first, since the code those passes write relies
//! on them.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::core::errors::{unsupported_feature, WaspyError};
use crate::ir::{
    IRBody, IRConstant, IRExpr, IRFunction, IRInitItem, IRModule, IRStatement, IRType, IRVariable,
    IMPORT_MARKER_FN, IMPORT_VALUE_KEY,
};

/// The prefix of a late module's start function; `MODULE_INIT_FN` itself is
/// the program's.
pub(crate) const LATE_START_PREFIX: &str = "__module_init__";

/// Which definitions are evaluated once, into a global, rather than inlined
/// at each read.
pub(crate) struct Evaluated<'m> {
    /// Names some function declares `global` (and a metaclass's class
    /// attributes): always globals, since something rebinds them.
    declared: HashSet<String>,
    vars: HashMap<&'m str, &'m IRExpr>,
    /// Class variables some function assigns through the class.
    written: HashSet<String>,
}

impl<'m> Evaluated<'m> {
    pub(crate) fn of(module: &'m IRModule) -> Self {
        let declared: HashSet<String> = module
            .functions
            .iter()
            .chain(module.classes.iter().flat_map(|c| c.methods.iter()))
            .flat_map(|f| crate::compiler::function::global_declarations(&f.body))
            // A metaclass's class attributes are written through the class.
            .chain(module.classes.iter().flat_map(|c| {
                c.deferred_vars
                    .iter()
                    .map(move |v| format!("{}.{v}", c.name))
            }))
            .collect();
        let vars = module
            .variables
            .iter()
            .filter(|v| !declared.contains(&v.name))
            .map(|v| (v.name.as_str(), &v.value))
            .collect();
        Evaluated {
            declared,
            vars,
            written: crate::compiler::module::written_class_vars(module),
        }
    }

    pub(crate) fn var(&self, var: &IRVariable) -> bool {
        self.declared.contains(&var.name)
            || !crate::compiler::module::is_pure_constant(&var.value, &self.vars, 0)
    }

    /// The class variable `var` of `class`, as the global it is evaluated
    /// into (`Class.var`), when it is one.
    pub(crate) fn class_var(&self, class: &str, var: &IRVariable) -> Option<IRVariable> {
        let key = format!("{class}.{}", var.name);
        (self.written.contains(&key)
            || !crate::compiler::module::is_pure_constant(&var.value, &self.vars, 0))
        .then(|| IRVariable {
            name: key,
            value: var.value.clone(),
            var_type: var.var_type.clone(),
        })
    }
}

/// The module with every top level placed, ready for its start function.
pub(crate) struct Staged {
    /// The module, with each marker made a call of the late start it names
    /// (or dropped), and the late starts added.
    pub(crate) module: IRModule,
    /// The flags behind the late starts, each set to 0 before anything runs.
    pub(crate) flags: Vec<String>,
    /// The program start's statements after those: the entry's top level and
    /// the modules it imports, then the files nothing imports.
    pub(crate) start: Vec<IRStatement>,
    /// Every global a start assigns, in order.
    pub(crate) globals: Vec<String>,
    /// The classes and definitions a file states, which the start places;
    /// the rest go first (`compile_ir_module`).
    pub(crate) placed_classes: HashSet<String>,
    pub(crate) placed_vars: HashSet<usize>,
}

/// Place every top level of `module` (see the module documentation).
pub(crate) fn stage(module: &IRModule) -> Result<Staged, WaspyError> {
    let evaluated = Evaluated::of(module);
    let sources = &module.sources;

    // The definition each `Variable` item names: the k-th item naming a
    // definition is the k-th definition of that name, as files and their
    // definitions are merged in the same order.
    let mut by_name: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (i, v) in module.variables.iter().enumerate() {
        by_name.entry(v.name.as_str()).or_default().push_back(i);
    }
    let item_vars: Vec<Vec<Option<usize>>> = sources
        .iter()
        .map(|source| {
            source
                .items
                .iter()
                .map(|item| match item {
                    IRInitItem::Variable(name) => {
                        by_name.get_mut(name.as_str()).and_then(|q| q.pop_front())
                    }
                    _ => None,
                })
                .collect()
        })
        .collect();

    // The file a module name is: its dotted name, or one ending in it (an
    // imported file is named for its path).
    let find = |name: &str| -> Option<usize> {
        let suffix = format!(".{name}");
        sources
            .iter()
            .position(|s| s.module == name || s.module.ends_with(&suffix))
    };
    let imports_of = |i: usize| -> Vec<usize> {
        sources[i]
            .items
            .iter()
            .filter_map(|item| match item {
                IRInitItem::Import(m) => find(m),
                _ => None,
            })
            .collect()
    };
    let reach = |from: usize, stop: &HashSet<usize>, into: &mut HashSet<usize>| {
        let mut pending = vec![from];
        while let Some(i) = pending.pop() {
            if stop.contains(&i) || !into.insert(i) {
                continue;
            }
            pending.extend(imports_of(i));
        }
    };

    let entry = sources.iter().position(|s| s.module == "__main__");
    // Started with the program: the entry and what its top level imports.
    let mut statics = HashSet::new();
    if let Some(entry) = entry {
        reach(entry, &HashSet::new(), &mut statics);
    }
    // Started late: what a marker names, and what those import, unless the
    // program has started it already.
    let mut marked = Vec::new();
    for body in bodies(module) {
        collect_markers(body, &mut marked);
    }
    let mut late = HashSet::new();
    for name in &marked {
        if let Some(i) = find(name) {
            reach(i, &statics, &mut late);
        }
    }

    // What a file's own top level evaluates.
    let own_globals = |i: usize| -> bool {
        sources[i]
            .items
            .iter()
            .enumerate()
            .any(|(k, item)| match item {
                IRInitItem::Variable(_) => {
                    item_vars[i][k].is_some_and(|v| evaluated.var(&module.variables[v]))
                }
                IRInitItem::Class(c) => module
                    .classes
                    .iter()
                    .find(|cls| cls.name == *c)
                    .is_some_and(|cls| {
                        cls.class_vars
                            .iter()
                            .any(|v| evaluated.class_var(c, v).is_some())
                    }),
                _ => false,
            })
    };
    // A late file needs a start of its own when it, or a late file it
    // imports, evaluates anything: to a fixpoint, since imports can cycle.
    let mut late_ordered: Vec<usize> = late.iter().copied().collect();
    late_ordered.sort_unstable();
    let mut needs: HashMap<usize, bool> =
        late_ordered.iter().map(|&i| (i, own_globals(i))).collect();
    loop {
        let mut changed = false;
        for &i in &late_ordered {
            if !needs[&i] && imports_of(i).iter().any(|j| needs.get(j) == Some(&true)) {
                needs.insert(i, true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let start_fn = |i: usize| format!("{LATE_START_PREFIX}{i}");
    let flag_of = |i: usize| format!("__module_started__{i}");
    let has_start = |i: usize| late.contains(&i) && needs.get(&i).copied().unwrap_or(false);

    // A dynamic import used in place has no statement for a late start, so
    // a module that would start there is refused.
    for (key, place) in &module.metadata {
        let Some(name) = key.strip_prefix(IMPORT_VALUE_KEY) else {
            continue;
        };
        let Some(i) = find(name) else { continue };
        if !statics.contains(&i) && (has_start(i) || own_globals(i)) {
            return Err(unsupported_feature(
                format!(
                    "the module '{name}' is imported in place in {place} \
                     (`importlib.import_module(\"{name}\")` used as a value), and its top level \
                     runs when it is first imported, which an import used in place does not \
                     mark here. Hint: assign the import to a name first"
                ),
                None,
            ));
        }
    }

    // Emit a file's top level.
    struct Emit<'a, 'm> {
        module: &'a IRModule,
        evaluated: &'a Evaluated<'m>,
        item_vars: &'a [Vec<Option<usize>>],
        globals: Vec<String>,
        placed_classes: HashSet<String>,
        placed_vars: HashSet<usize>,
    }
    impl Emit<'_, '_> {
        fn assign(&mut self, var: IRVariable, out: &mut Vec<IRStatement>) {
            self.globals.push(var.name.clone());
            out.push(IRStatement::Assign {
                target: var.name,
                value: var.value,
                var_type: var.var_type,
            });
        }

        /// File `i`'s statements, and `import(j)` for each file it imports.
        fn file(
            &mut self,
            i: usize,
            out: &mut Vec<IRStatement>,
            import: &mut dyn FnMut(usize, &mut Self, &mut Vec<IRStatement>),
            find: &dyn Fn(&str) -> Option<usize>,
        ) {
            let source = &self.module.sources[i];
            for (k, item) in source.items.iter().enumerate() {
                match item {
                    IRInitItem::Variable(_) => {
                        if let Some(v) = self.item_vars[i][k] {
                            self.placed_vars.insert(v);
                            let var = &self.module.variables[v];
                            if self.evaluated.var(var) {
                                self.assign(var.clone(), out);
                            }
                        }
                    }
                    IRInitItem::Class(name) => {
                        self.placed_classes.insert(name.clone());
                        let module = self.module;
                        if let Some(cls) = module.classes.iter().find(|c| c.name == *name) {
                            for var in &cls.class_vars {
                                if let Some(global) = self.evaluated.class_var(name, var) {
                                    self.assign(global, out);
                                }
                            }
                        }
                    }
                    IRInitItem::Def(_) => {}
                    IRInitItem::Import(name) => {
                        if let Some(j) = find(name) {
                            import(j, self, out);
                        }
                    }
                }
            }
        }
    }
    let call = |name: String| {
        IRStatement::Expression(IRExpr::FunctionCall {
            function_name: name,
            arguments: Vec::new(),
        })
    };

    let mut emit = Emit {
        module,
        evaluated: &evaluated,
        item_vars: &item_vars,
        globals: Vec::new(),
        placed_classes: HashSet::new(),
        placed_vars: HashSet::new(),
    };

    // The program's start: the entry, each module at its first import, then
    // the files nothing imports.
    let mut start = Vec::new();
    let mut started: HashSet<usize> = HashSet::new();
    fn run(
        i: usize,
        emit: &mut Emit,
        out: &mut Vec<IRStatement>,
        started: &mut HashSet<usize>,
        late: &dyn Fn(usize) -> Option<Option<IRStatement>>,
        find: &dyn Fn(&str) -> Option<usize>,
    ) {
        if !started.insert(i) {
            return;
        }
        let mut import = |j: usize, emit: &mut Emit, out: &mut Vec<IRStatement>| match late(j) {
            // A late file with nothing to start is started by nothing.
            Some(stmt) => out.extend(stmt),
            None => run(j, emit, out, started, late, find),
        };
        emit.file(i, out, &mut import, find);
    }
    let late_call = |j: usize| {
        late.contains(&j)
            .then(|| has_start(j).then(|| call(start_fn(j))))
    };
    let late_call: &dyn Fn(usize) -> Option<Option<IRStatement>> = &late_call;
    if let Some(entry) = entry {
        run(entry, &mut emit, &mut start, &mut started, late_call, &find);
    }
    for i in 0..sources.len() {
        if !late.contains(&i) {
            run(i, &mut emit, &mut start, &mut started, late_call, &find);
        }
    }

    // The late starts, each run once: its flag is set first, so an import
    // cycle back into it finds it started, as CPython's `sys.modules` does.
    let mut functions = Vec::new();
    let mut flags = Vec::new();
    for &i in &late_ordered {
        if !has_start(i) {
            continue;
        }
        let flag = flag_of(i);
        let mut body = vec![
            IRStatement::If {
                condition: IRExpr::Variable(flag.clone()),
                then_body: Box::new(IRBody {
                    statements: vec![IRStatement::Return(None)],
                }),
                else_body: None,
            },
            IRStatement::Assign {
                target: flag.clone(),
                value: IRExpr::Const(IRConstant::Int(1)),
                var_type: Some(IRType::Int),
            },
        ];
        let mut import = |j: usize, _: &mut Emit, out: &mut Vec<IRStatement>| {
            if has_start(j) {
                out.push(call(start_fn(j)));
            }
        };
        emit.file(i, &mut body, &mut import, &find);
        flags.push(flag);
        functions.push((i, body));
    }
    // Compiled where their globals' types are learned, so a late start comes
    // after the late starts it calls.
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    fn post(
        i: usize,
        imports: &dyn Fn(usize) -> Vec<usize>,
        has: &dyn Fn(usize) -> bool,
        seen: &mut HashSet<usize>,
        order: &mut Vec<usize>,
    ) {
        if !has(i) || !seen.insert(i) {
            return;
        }
        for j in imports(i) {
            post(j, imports, has, seen, order);
        }
        order.push(i);
    }
    for &(i, _) in &functions {
        post(i, &imports_of, &has_start, &mut seen, &mut order);
    }

    let mut staged = module.clone();
    let targets: HashMap<String, Option<String>> = marked
        .iter()
        .map(|name| {
            let target = find(name).filter(|&i| has_start(i)).map(start_fn);
            (name.clone(), target)
        })
        .collect();
    for body in bodies_mut(&mut staged) {
        rewrite_markers(body, &targets);
    }
    let mut bodies_by_file: HashMap<usize, Vec<IRStatement>> = functions.into_iter().collect();
    for i in order {
        let body = bodies_by_file.remove(&i).expect("a late start");
        staged.functions.push(IRFunction {
            name: start_fn(i),
            params: Vec::new(),
            body: IRBody { statements: body },
            return_type: IRType::None,
            decorators: Vec::new(),
        });
    }
    Ok(Staged {
        module: staged,
        flags,
        start,
        globals: emit.globals,
        placed_classes: emit.placed_classes,
        placed_vars: emit.placed_vars,
    })
}

/// Every function and method body.
fn bodies(module: &IRModule) -> impl Iterator<Item = &IRBody> {
    module.functions.iter().map(|f| &f.body).chain(
        module
            .classes
            .iter()
            .flat_map(|c| c.methods.iter().map(|m| &m.body)),
    )
}

fn bodies_mut(module: &mut IRModule) -> Vec<&mut IRBody> {
    let mut out: Vec<&mut IRBody> = module.functions.iter_mut().map(|f| &mut f.body).collect();
    for class in &mut module.classes {
        out.extend(class.methods.iter_mut().map(|m| &mut m.body));
    }
    out
}

/// The modules a marker statement names, if `stmt` is one.
fn marker_modules(stmt: &IRStatement) -> Option<Vec<String>> {
    let IRStatement::Expression(IRExpr::FunctionCall {
        function_name,
        arguments,
    }) = stmt
    else {
        return None;
    };
    (function_name == IMPORT_MARKER_FN).then(|| {
        arguments
            .iter()
            .filter_map(|a| match a {
                IRExpr::Const(IRConstant::String(m)) => Some(m.clone()),
                _ => None,
            })
            .collect()
    })
}

/// Apply `f` to every nested body of `stmt`.
fn nested_bodies(stmt: &mut IRStatement, f: &mut dyn FnMut(&mut IRBody)) {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            f(then_body);
            if let Some(b) = else_body {
                f(b);
            }
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => f(body),
        IRStatement::For {
            body, else_body, ..
        } => {
            f(body);
            if let Some(b) = else_body {
                f(b);
            }
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            f(try_body);
            for h in except_handlers {
                f(&mut h.body);
            }
            if let Some(b) = finally_body {
                f(b);
            }
        }
        IRStatement::FunctionDef(def) => f(&mut def.body),
        _ => {}
    }
}

fn collect_markers(body: &IRBody, out: &mut Vec<String>) {
    // Walked on a copy: the nested bodies are reached the same way as when
    // rewriting.
    let mut body = body.clone();
    fn walk(body: &mut IRBody, out: &mut Vec<String>) {
        for stmt in &mut body.statements {
            if let Some(modules) = marker_modules(stmt) {
                for m in modules {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            }
            nested_bodies(stmt, &mut |b| walk(b, out));
        }
    }
    walk(&mut body, out);
}

/// Make each marker the calls of the late starts it names; one naming none
/// is dropped.
fn rewrite_markers(body: &mut IRBody, targets: &HashMap<String, Option<String>>) {
    let statements = std::mem::take(&mut body.statements);
    for mut stmt in statements {
        if let Some(modules) = marker_modules(&stmt) {
            for m in modules {
                if let Some(Some(start)) = targets.get(&m) {
                    body.statements
                        .push(IRStatement::Expression(IRExpr::FunctionCall {
                            function_name: start.clone(),
                            arguments: Vec::new(),
                        }));
                }
            }
            continue;
        }
        nested_bodies(&mut stmt, &mut |b| rewrite_markers(b, targets));
        body.statements.push(stmt);
    }
}
