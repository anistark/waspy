//! Merging one file's module into the program's.
//!
//! Each file of a program is lowered on its own, module passes included, so
//! two files that use the same standard-library module each carry what its
//! pass wrote out: the module's functions and classes, and whatever state the
//! pass keeps under a shared name. Appending the second file's copy to the
//! first's gave every function twice, which the merge refused as two
//! definitions of one name, so a program could use `re`, `logging`,
//! `datetime`, `collections`, or `asyncio` in one of its files only.
//!
//! `reconcile` makes the incoming file's module one that can be appended:
//!
//! - a function or class the program already has, written the same, is left
//!   out of it (the program keeps one copy, and with it one copy of a
//!   module's state, as CPython has one `logging`);
//! - a class both have that differs is the union of the two, when what they
//!   share is the same (a pass adds a method or a class variable for what a
//!   file uses);
//! - the patterns `re` numbered from zero in each file are numbered apart
//!   (`re::renumber_cache`), so a file never reads another file's pattern.
//!
//! Anything else that is defined twice and differently is an error naming
//! it: keeping either copy would give one of the files the other's code.

use crate::ir::{IRBody, IRClass, IRExpr, IRInitItem, IRModule, IRStatement};
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};

/// The prefix of a function lifted out of the one it was written in: a
/// lambda, or a nested `def` (`ir::finalize`). The number after it is the
/// compilation's, so two files' copies of one standard-library method name
/// the lambda inside it differently.
const LIFTED: &str = "__lambda_";

/// A piece of IR as it is written, with the numbers of the functions lifted
/// out of it left out, and those numbers in the order it reads them.
fn shape<T: std::fmt::Debug>(ir: &T) -> (String, Vec<usize>) {
    let text = format!("{ir:?}");
    let mut out = String::with_capacity(text.len());
    let mut numbers = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find(LIFTED) {
        let tail = &rest[at + LIFTED.len()..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        out.push_str(&rest[..at + LIFTED.len()]);
        if let Ok(number) = tail[..digits].parse() {
            numbers.push(number);
            out.push('#');
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    (out, numbers)
}

/// The number of the lifted function a name is, when it is one.
fn lifted_number(name: &str) -> Option<usize> {
    let tail = name.strip_prefix(LIFTED)?;
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    tail[..digits].parse().ok()
}

/// The functions lifted out of a module's code, by number, each as its
/// shape.
type Lifted = HashMap<usize, (String, Vec<usize>)>;

fn lifted_functions(module: &IRModule) -> Lifted {
    module
        .functions
        .iter()
        .filter_map(|f| Some((lifted_number(&f.name)?, shape(f))))
        .collect()
}

/// Compares what the program has with what a file brings.
struct Matcher {
    /// The program's lifted functions, and the file's.
    kept: Lifted,
    incoming: Lifted,
    /// The file's lifted functions that a definition left out of it read:
    /// the program's copy reads its own.
    unread: HashSet<usize>,
}

impl Matcher {
    /// Whether two pieces of IR are written the same, the functions lifted
    /// out of them too. When they are, the file's lifted ones are noted as
    /// no longer read.
    fn same<T: std::fmt::Debug>(&mut self, kept: &T, incoming: &T) -> bool {
        let mut pairs = Vec::new();
        if !self.alike(shape(kept), shape(incoming), &mut pairs) {
            return false;
        }
        self.unread
            .extend(pairs.into_iter().map(|(_, theirs)| theirs));
        true
    }

    fn alike(
        &self,
        kept: (String, Vec<usize>),
        incoming: (String, Vec<usize>),
        pairs: &mut Vec<(usize, usize)>,
    ) -> bool {
        if kept.0 != incoming.0 {
            return false;
        }
        for (ours, theirs) in kept.1.into_iter().zip(incoming.1) {
            if ours == theirs || pairs.contains(&(ours, theirs)) {
                continue;
            }
            let (Some(a), Some(b)) = (self.kept.get(&ours), self.incoming.get(&theirs)) else {
                return false;
            };
            pairs.push((ours, theirs));
            if !self.alike(a.clone(), b.clone(), pairs) {
                return false;
            }
        }
        true
    }
}

/// The classes a module's files state with a `class` statement: the
/// program's own, as opposed to the ones a pass wrote.
fn stated_classes(module: &IRModule) -> HashSet<&str> {
    module
        .sources
        .iter()
        .flat_map(|source| &source.items)
        .filter_map(|item| match item {
            IRInitItem::Class(name) => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

/// The names a module's functions declare `global`: module variables they
/// can rebind.
fn rebound_globals(module: &IRModule) -> HashSet<String> {
    fn scan(body: &IRBody, out: &mut HashSet<String>) {
        for stmt in &body.statements {
            if let IRStatement::Global(names) = stmt {
                out.extend(names.iter().cloned());
            }
            for nested in crate::ir::infer::nested_bodies(stmt) {
                scan(nested, out);
            }
        }
    }
    let mut out = HashSet::new();
    let methods = module.classes.iter().flat_map(|c| &c.methods);
    for function in module.functions.iter().chain(methods) {
        scan(&function.body, &mut out);
    }
    out
}

fn differs(what: &str, name: &str, filename: &str) -> anyhow::Error {
    anyhow!(
        "{filename} and a file merged before it each use the standard library in a way that \
         writes {what} '{name}' differently, and the two cannot be merged yet. Hint: use that \
         part of the module in one of the files"
    )
}

/// The union of two classes a pass wrote under one name: `incoming`'s
/// methods and class variables added to `kept`, where the ones both have are
/// the same.
fn unite(
    kept: &mut IRClass,
    incoming: IRClass,
    filename: &str,
    matcher: &mut Matcher,
) -> Result<()> {
    if kept.bases != incoming.bases || kept.metaclass != incoming.metaclass {
        return Err(differs("the class", &kept.name, filename));
    }
    for method in incoming.methods {
        match kept.methods.iter().find(|m| m.name == method.name) {
            Some(existing) if matcher.same(existing, &method) => {}
            Some(_) => {
                return Err(differs(
                    "the method",
                    &format!("{}.{}", kept.name, method.name),
                    filename,
                ))
            }
            None => kept.methods.push(method),
        }
    }
    for var in incoming.class_vars {
        match kept.class_vars.iter().find(|v| v.name == var.name) {
            Some(existing) if matcher.same(existing, &var) => {}
            Some(_) => {
                return Err(differs(
                    "the class variable",
                    &format!("{}.{}", kept.name, var.name),
                    filename,
                ))
            }
            None => kept.class_vars.push(var),
        }
    }
    for name in incoming.deferred_vars {
        if !kept.deferred_vars.contains(&name) {
            kept.deferred_vars.push(name);
        }
    }
    Ok(())
}

/// Take out of `file` what `program` already has, so the rest can be
/// appended to it: see the module's description. A class the program's files
/// state twice is an error, as a function defined twice is.
pub(crate) fn reconcile(program: &mut IRModule, file: &mut IRModule, filename: &str) -> Result<()> {
    crate::ir::re::renumber_cache(program, file);
    let mut matcher = Matcher {
        kept: lifted_functions(program),
        incoming: lifted_functions(file),
        unread: HashSet::new(),
    };

    let mut functions = Vec::with_capacity(file.functions.len());
    for function in std::mem::take(&mut file.functions) {
        // What a pass wrote, as opposed to the program's own: the passes'
        // names all start so, and a program's function that does is one
        // function too when it is written the same.
        let internal = function.name.starts_with("__");
        match program.functions.iter().find(|f| f.name == function.name) {
            Some(existing) if internal => {
                if !matcher.same(existing, &function) {
                    return Err(differs("the function", &function.name, filename));
                }
            }
            // The program's own function defined twice: the merge's error.
            _ => functions.push(function),
        }
    }

    let stated: HashSet<String> = stated_classes(program)
        .into_iter()
        .chain(stated_classes(file))
        .map(str::to_string)
        .collect();
    let mut classes = Vec::with_capacity(file.classes.len());
    for class in std::mem::take(&mut file.classes) {
        let Some(kept) = program.classes.iter_mut().find(|c| c.name == class.name) else {
            classes.push(class);
            continue;
        };
        if stated.contains(&class.name) {
            return Err(anyhow!(
                "duplicate class '{}' in {filename}: another module in this compilation \
                 already defines it, and merged modules share one namespace. Hint: rename one \
                 of them",
                class.name
            ));
        }
        if !matcher.same(kept, &class) {
            unite(kept, class, filename, &mut matcher)?;
        }
    }
    file.classes = classes;

    // A module variable two files define. Merged modules share one
    // namespace, so both files would read one of the two: `NAME = "a"` in
    // one file and `NAME = "b"` in another answered "a" in both. The same
    // constant in both is one constant, unless a function rebinds it.
    let rebound: HashSet<String> = rebound_globals(program)
        .into_iter()
        .chain(rebound_globals(file))
        .collect();
    let mut variables = Vec::with_capacity(file.variables.len());
    for variable in std::mem::take(&mut file.variables) {
        match program.variables.iter().find(|v| v.name == variable.name) {
            Some(existing)
                if matches!(variable.value, IRExpr::Const(_))
                    && !rebound.contains(&variable.name)
                    && matcher.same(existing, &variable) => {}
            Some(_) => {
                return Err(anyhow!(
                    "duplicate variable '{}' in {filename}: another module in this compilation \
                     already defines it, and merged modules share one namespace. Hint: rename \
                     one of them",
                    variable.name
                ));
            }
            None => variables.push(variable),
        }
    }
    file.variables = variables;
    // The functions lifted out of what was left out are read by nothing now.
    functions.retain(|f| !lifted_number(&f.name).is_some_and(|n| matcher.unread.contains(&n)));
    file.functions = functions;
    Ok(())
}
