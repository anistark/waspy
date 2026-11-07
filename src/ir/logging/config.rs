//! `logging.config.dictConfig()`, written out where it is called.
//!
//! The dict is read at compile time, as `basicConfig()`'s arguments are: its
//! values have no one type (a level, a list of names, a nested dict), so no
//! dict here could hold them, and what it configures is a sequence of calls
//! of the module's own classes. So the dict must be written out in the call,
//! or be a module constant that nothing else reads. The statements follow
//! `DictConfigurator.configure()`: formatters, filters, handlers sorted by
//! name with a `MemoryHandler`'s target first, then the loggers and the root.

use super::{class_name, classes, const_str, fname, module_call, Formats, LEVELS};
use crate::ir::build::*;
use crate::ir::{IRExpr, IRStatement};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};

type Pairs = [(IRExpr, IRExpr)];

/// A section's entries: each name, and what its dict holds.
type Entries = Vec<(String, Vec<(IRExpr, IRExpr)>)>;

fn get<'e>(pairs: &'e Pairs, key: &str) -> Option<&'e IRExpr> {
    pairs
        .iter()
        .find(|(k, _)| const_str(k) == Some(key))
        .map(|(_, value)| value)
}

fn section<'e>(pairs: &'e Pairs, key: &str) -> Result<&'e Pairs> {
    match get(pairs, key) {
        None => Ok(&[]),
        Some(IRExpr::DictLiteral(items)) => Ok(items),
        Some(_) => Err(refused(format!(
            "dictConfig()'s '{key}' is a dict written out here"
        ))),
    }
}

fn names_in(value: Option<&IRExpr>, what: &str) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(IRExpr::ListLiteral(items)) | Some(IRExpr::TupleLiteral(items)) => items
            .iter()
            .map(|i| {
                const_str(i)
                    .map(str::to_string)
                    .ok_or_else(|| refused(format!("dictConfig()'s {what} are named by strings")))
            })
            .collect(),
        Some(_) => Err(refused(format!(
            "dictConfig()'s {what} are a list written out here"
        ))),
    }
}

fn entries(pairs: &Pairs, what: &str) -> Result<Entries> {
    pairs
        .iter()
        .map(|(k, value)| {
            let name = const_str(k)
                .ok_or_else(|| refused(format!("dictConfig()'s {what} are named by strings")))?;
            match value {
                IRExpr::DictLiteral(items) => Ok((name.to_string(), items.clone())),
                _ => Err(refused(format!(
                    "dictConfig()'s {what} '{name}' is a dict written out here"
                ))),
            }
        })
        .collect()
}

/// A level as `logging._checkLevel()` takes it.
fn level_of(value: &IRExpr) -> Result<IRExpr> {
    Ok(match const_str(value) {
        Some(name) => match LEVELS.iter().find(|(l, _)| *l == name) {
            Some(&(_, n)) => int(n),
            // A name `addLevelName()` may have added, looked up when it runs.
            None => call(&fname("level_of"), vec![value.clone()]),
        },
        None => value.clone(),
    })
}

/// The keywords a factory is called with: what the entry holds but `taken`.
fn factory_arguments(pairs: &Pairs, taken: &[&str]) -> Vec<IRExpr> {
    pairs
        .iter()
        .filter_map(|(k, value)| {
            let name = const_str(k)?;
            (!taken.contains(&name)).then(|| IRExpr::Keyword {
                name: name.to_string(),
                value: Box::new(value.clone()),
            })
        })
        .collect()
}

/// A `"()": Class` entry's call, when it has one.
fn factory(pairs: &Pairs, taken: &[&str], what: &str) -> Result<Option<IRExpr>> {
    let Some(made) = get(pairs, "()") else {
        return Ok(None);
    };
    match made {
        IRExpr::Variable(class) => Ok(Some(call(class, factory_arguments(pairs, taken)))),
        _ => Err(refused(format!(
            "dictConfig()'s {what} '()' names a class of the program here, as a name rather \
             than a string"
        ))),
    }
}

fn formatter(pairs: &Pairs, formats: &mut Formats) -> Result<IRExpr> {
    const KNOWN: &[&str] = &["format", "datefmt", "style", "validate", "class", "()"];
    if let Some(made) = factory(pairs, &["()"], "formatter's")? {
        return Ok(made);
    }
    for (k, _) in pairs {
        if !const_str(k).is_some_and(|n| KNOWN.contains(&n)) {
            return Err(refused(format!(
                "dictConfig()'s formatter option {k:?} is not supported"
            )));
        }
    }
    if let Some(class) = get(pairs, "class") {
        if const_str(class) != Some("logging.Formatter") {
            return Err(refused(
                "dictConfig()'s formatter 'class' is logging.Formatter here. Hint: name the \
                 class with '()'"
                    .to_string(),
            ));
        }
    }
    let style = match get(pairs, "style") {
        Some(s) => const_str(s)
            .ok_or_else(|| refused("dictConfig()'s style is written as a literal".to_string()))?,
        None => "%",
    };
    let fmt = get(pairs, "format").cloned().unwrap_or_else(none);
    if let Some(f) = const_str(&fmt) {
        formats.literal(f, style)?;
    }
    Ok(call(
        &class_name("Formatter"),
        vec![
            fmt,
            get(pairs, "datefmt").cloned().unwrap_or_else(none),
            text(style),
            get(pairs, "validate")
                .cloned()
                .unwrap_or_else(|| boolean(true)),
        ],
    ))
}

fn filter(pairs: &Pairs) -> Result<IRExpr> {
    if let Some(made) = factory(pairs, &["()"], "filter's")? {
        return Ok(made);
    }
    Ok(call(
        &class_name("Filter"),
        vec![get(pairs, "name").cloned().unwrap_or_else(|| text(""))],
    ))
}

/// What the class named by a handler's `class` is called with.
fn handler_call(
    pairs: &Pairs,
    locals: &BTreeMap<String, String>,
    formats: &mut Formats,
) -> Result<IRExpr> {
    const TAKEN: &[&str] = &["class", "formatter", "level", "filters", "()", "."];
    if get(pairs, ".").is_some() {
        return Err(refused(
            "dictConfig()'s '.' attributes are not supported".to_string(),
        ));
    }
    if let Some(made) = factory(pairs, TAKEN, "handler's")? {
        return Ok(made);
    }
    let class = get(pairs, "class")
        .and_then(const_str)
        .ok_or_else(|| refused("dictConfig()'s handler needs a 'class' string".to_string()))?;
    let keywords = factory_arguments(pairs, TAKEN);
    let short = match class.rsplit_once('.') {
        Some(("logging" | "logging.handlers", short)) => short,
        Some(("__main__", short)) => return Ok(call(short, keywords)),
        _ => {
            return Err(refused(format!(
                "dictConfig()'s handler class '{class}' is not one this module can name. \
                 Hint: name a class of the program with '()'"
            )))
        }
    };
    match short {
        "StreamHandler" => {
            let fd =
                match get(pairs, "stream").and_then(const_str) {
                    None if get(pairs, "stream").is_none() => 2,
                    Some("ext://sys.stdout") => 1,
                    Some("ext://sys.stderr") => 2,
                    _ => return Err(refused(
                        "dictConfig()'s stream is 'ext://sys.stdout' or 'ext://sys.stderr' here"
                            .to_string(),
                    )),
                };
            Ok(call(&class_name("StreamHandler"), vec![int(fd)]))
        }
        "MemoryHandler" => {
            let mut arguments = Vec::new();
            for a in keywords {
                let IRExpr::Keyword { name, value } = a else {
                    continue;
                };
                let value = match name.as_str() {
                    "flushLevel" => level_of(&value)?,
                    "target" => {
                        let target = const_str(&value).unwrap_or_default();
                        match locals.get(target) {
                            Some(local) => v(local),
                            None => {
                                return Err(refused(format!(
                                    "dictConfig() raises ValueError: Unable to set target \
                                     handler {target:?}"
                                )))
                            }
                        }
                    }
                    _ => *value,
                };
                arguments.push(IRExpr::Keyword {
                    name,
                    value: Box::new(value),
                });
            }
            module_call(short, arguments, formats)
        }
        _ => module_call(short, keywords, formats),
    }
}

/// The statements that give `target` what a logger's entry says.
fn common_logger(
    target: &str,
    pairs: &Pairs,
    handlers: &BTreeMap<String, String>,
    filters: &BTreeMap<String, String>,
    known: &[&str],
    out: &mut Vec<IRStatement>,
) -> Result<()> {
    for (k, _) in pairs {
        if !const_str(k).is_some_and(|n| known.contains(&n)) {
            return Err(refused(format!(
                "dictConfig()'s logger option {k:?} is not supported"
            )));
        }
    }
    if let Some(level) = get(pairs, "level") {
        out.push(expr_stmt(mcall(
            v(target),
            "setLevel",
            vec![level_of(level)?],
        )));
    }
    out.push(expr_stmt(call(&fname("drop_handlers"), vec![v(target)])));
    for name in names_in(get(pairs, "handlers"), "handlers")? {
        let local = handlers.get(&name).ok_or_else(|| {
            refused(format!(
                "dictConfig() raises ValueError: Unable to add handler {name:?}"
            ))
        })?;
        out.push(expr_stmt(mcall(v(target), "addHandler", vec![v(local)])));
    }
    for name in names_in(get(pairs, "filters"), "filters")? {
        let local = filters.get(&name).ok_or_else(|| {
            refused(format!(
                "dictConfig() raises ValueError: Unable to add filter {name:?}"
            ))
        })?;
        out.push(expr_stmt(mcall(v(target), "addFilter", vec![v(local)])));
    }
    Ok(())
}

/// What `dictConfig(config)` does, as statements. `constants` are the
/// module's dicts, for a `config` that names one.
pub(super) fn dict_config(
    config: &IRExpr,
    constants: &HashMap<String, IRExpr>,
    formats: &mut Formats,
) -> Result<IRStatement> {
    let config = match config {
        IRExpr::Variable(name) => constants.get(name).unwrap_or(config),
        other => other,
    };
    let IRExpr::DictLiteral(pairs) = config else {
        return Err(refused(
            "dictConfig() reads its dict at compile time, so it is written out in the call or \
             a module constant here"
                .to_string(),
        ));
    };
    const KNOWN: &[&str] = &[
        "version",
        "formatters",
        "filters",
        "handlers",
        "loggers",
        "root",
        "incremental",
        "disable_existing_loggers",
    ];
    for (k, _) in pairs {
        if !const_str(k).is_some_and(|n| KNOWN.contains(&n)) {
            return Err(refused(format!(
                "dictConfig()'s key {k:?} is not supported"
            )));
        }
    }
    match get(pairs, "version") {
        None => {
            return Err(refused(
                "dictConfig() raises ValueError: dictionary doesn't specify a version".to_string(),
            ))
        }
        Some(IRExpr::Const(crate::ir::IRConstant::Int(1))) => {}
        Some(other) => {
            return Err(refused(format!(
                "dictConfig() raises ValueError: Unsupported version: {other:?}"
            )))
        }
    }
    if !matches!(
        get(pairs, "incremental"),
        None | Some(IRExpr::Const(crate::ir::IRConstant::Bool(false)))
    ) {
        return Err(refused(
            "dictConfig()'s incremental=True is not supported".to_string(),
        ));
    }
    let disable = get(pairs, "disable_existing_loggers")
        .cloned()
        .unwrap_or_else(|| boolean(true));

    let mut out = vec![expr_stmt(call(&fname("config_reset"), Vec::new()))];
    let mut at = 0;
    let mut local = |kind: &str| {
        at += 1;
        format!("__logging_{kind}_{at}")
    };

    let mut formatters = BTreeMap::new();
    for (name, spec) in entries(section(pairs, "formatters")?, "formatter")? {
        let made = local("formatter");
        out.push(assign_typed(
            &made,
            formatter(&spec, formats)?,
            &classes::ty("Formatter"),
        ));
        formatters.insert(name, made);
    }
    let mut filters = BTreeMap::new();
    for (name, spec) in entries(section(pairs, "filters")?, "filter")? {
        let made = local("filter");
        out.push(assign_typed(&made, filter(&spec)?, &classes::ty("Filter")));
        filters.insert(name, made);
    }

    // Sorted by name, a handler whose target is not made yet after the rest.
    let mut pending = entries(section(pairs, "handlers")?, "handler")?;
    pending.sort_by(|a, b| a.0.cmp(&b.0));
    let (later, first): (Vec<_>, Vec<_>) = pending
        .into_iter()
        .partition(|(_, spec)| get(spec, "target").is_some());
    let mut handlers = BTreeMap::new();
    for (name, spec) in first.into_iter().chain(later) {
        let made = local("handler");
        out.push(assign_typed(
            &made,
            handler_call(&spec, &handlers, formats)?,
            &classes::ty("Handler"),
        ));
        if let Some(chosen) = get(&spec, "formatter").and_then(const_str) {
            let formatter = formatters.get(chosen).ok_or_else(|| {
                refused(format!(
                    "dictConfig() raises ValueError: Unable to set formatter {chosen:?}"
                ))
            })?;
            out.push(expr_stmt(mcall(
                v(&made),
                "setFormatter",
                vec![v(formatter)],
            )));
        }
        if let Some(level) = get(&spec, "level") {
            out.push(expr_stmt(mcall(
                v(&made),
                "setLevel",
                vec![level_of(level)?],
            )));
        }
        for chosen in names_in(get(&spec, "filters"), "filters")? {
            let filter = filters.get(&chosen).ok_or_else(|| {
                refused(format!(
                    "dictConfig() raises ValueError: Unable to add filter {chosen:?}"
                ))
            })?;
            out.push(expr_stmt(mcall(v(&made), "addFilter", vec![v(filter)])));
        }
        handlers.insert(name, made);
    }

    let loggers = entries(section(pairs, "loggers")?, "logger")?;
    out.push(expr_stmt(call(
        &fname("config_existing"),
        vec![
            typed_names(loggers.iter().map(|(n, _)| n.as_str())),
            disable,
        ],
    )));
    for (name, spec) in &loggers {
        let made = local("logger");
        out.push(assign_typed(
            &made,
            call(&fname("get_logger"), vec![text(name)]),
            &classes::ty("Logger"),
        ));
        common_logger(
            &made,
            spec,
            &handlers,
            &filters,
            &["level", "handlers", "filters", "propagate"],
            &mut out,
        )?;
        out.push(set_attr(v(&made), "disabled", boolean(false)));
        if let Some(propagate) = get(spec, "propagate") {
            out.push(set_attr(v(&made), "propagate", propagate.clone()));
        }
    }
    if let Some(IRExpr::DictLiteral(root)) = get(pairs, "root") {
        let made = local("logger");
        out.push(assign_typed(
            &made,
            call(&fname("get_logger"), Vec::new()),
            &classes::ty("Logger"),
        ));
        common_logger(
            &made,
            root,
            &handlers,
            &filters,
            &["level", "handlers", "filters"],
            &mut out,
        )?;
    }
    Ok(if_(boolean(true), out))
}

/// A list of strings; an empty one would have no element type.
fn typed_names<'n>(names: impl Iterator<Item = &'n str>) -> IRExpr {
    let mut items: Vec<IRExpr> = names.map(text).collect();
    // A name no logger has, so the list is never empty.
    items.push(text(""));
    IRExpr::ListLiteral(items)
}
