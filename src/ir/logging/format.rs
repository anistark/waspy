//! A `logging.Formatter`'s format string, compiled: the three styles CPython
//! reads (`%`, `{`, and `$`), each into an expression that renders a
//! `LogRecord` named `record`, and the error its `validate()` raises.
//!
//! Formats are compiled where the program spells them out, so a field the
//! record does not have (which CPython reports from inside the handler, as a
//! "Logging error" with a traceback) is refused here instead.

use crate::ir::build::*;
use crate::ir::IRExpr;

/// A record attribute a format can name, and how it reads.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Str,
    Int,
    Float,
    /// `taskName`, None outside asyncio.
    None,
}

fn field(name: &str) -> Result<Field, String> {
    Ok(match name {
        "name" | "levelname" | "message" | "asctime" | "funcName" | "processName"
        | "threadName" | "msg" => Field::Str,
        "levelno" | "lineno" | "process" => Field::Int,
        "created" | "msecs" | "relativeCreated" => Field::Float,
        "taskName" => Field::None,
        "pathname" | "filename" | "module" => {
            return Err(format!(
                "the record attribute '{name}' names the source file, which a compiled \
                 module does not know"
            ))
        }
        "thread" => {
            return Err(
                "the record attribute 'thread' is a thread identifier, which a \
                 WebAssembly host does not have"
                    .to_string(),
            )
        }
        "exc_info" | "exc_text" | "stack_info" | "args" => {
            return Err(format!("the record attribute '{name}' is not supported"))
        }
        _ => {
            return Err(format!(
                "a LogRecord has no attribute '{name}', so CPython reports a logging error \
                 for every record"
            ))
        }
    })
}

fn read(name: &str) -> IRExpr {
    attr(v("record"), name)
}

/// The text a field renders as with `str()` (`s`) or `repr()` (`r`).
fn as_text(name: &str, kind: Field, conversion: char) -> IRExpr {
    match (kind, conversion) {
        (Field::None, _) => text("None"),
        (_, 'r') => call("repr", vec![read(name)]),
        (_, 'a') => call("ascii", vec![read(name)]),
        _ => call("str", vec![read(name)]),
    }
}

fn formatted(value: IRExpr, spec: &str) -> IRExpr {
    if spec.is_empty() {
        value
    } else {
        call("format", vec![value, text(spec)])
    }
}

/// What a format string compiles to.
pub(super) struct Compiled {
    /// The expression rendering `record`, or why it cannot be compiled.
    pub render: Result<IRExpr, String>,
    /// The ValueError `Formatter(..., validate=True)` raises for it.
    pub invalid: Option<String>,
}

pub(super) fn compile(fmt: &str, style: &str) -> Compiled {
    match style {
        "%" => percent(fmt),
        "{" => braces(fmt),
        _ => template(fmt),
    }
}

/// Literal text and rendered fields, concatenated.
struct Parts {
    out: Vec<IRExpr>,
    literal: String,
}

impl Parts {
    fn new() -> Parts {
        Parts {
            out: Vec::new(),
            literal: String::new(),
        }
    }
    fn push(&mut self, e: IRExpr) {
        if !self.literal.is_empty() {
            self.out.push(text(&std::mem::take(&mut self.literal)));
        }
        self.out.push(e);
    }
    fn finish(mut self) -> IRExpr {
        if !self.literal.is_empty() || self.out.is_empty() {
            self.out.push(text(&std::mem::take(&mut self.literal)));
        }
        self.out
            .into_iter()
            .reduce(|a, b| bin(a, crate::ir::IROp::Add, b))
            .expect("a part")
    }
}

/// `fmt % record.__dict__`.
fn percent(fmt: &str) -> Compiled {
    let chars: Vec<char> = fmt.chars().collect();
    let mut parts = Parts::new();
    let mut fields = 0;
    let mut i = 0;
    let mut render: Result<(), String> = Ok(());
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            parts.literal.push(c);
            continue;
        }
        match chars.get(i) {
            Some('%') => {
                parts.literal.push('%');
                i += 1;
                continue;
            }
            Some('(') => {}
            _ => {
                render = Err(
                    "a conversion without a field name reads the whole record as \
                     its value"
                        .to_string(),
                );
                break;
            }
        }
        let Some(close) = chars[i..].iter().position(|&c| c == ')') else {
            render = Err("an unterminated field name".to_string());
            break;
        };
        let name: String = chars[i + 1..i + close].iter().collect();
        i += close + 1;
        let mut flags = String::new();
        while let Some(&f) = chars.get(i).filter(|c| "#0- +".contains(**c)) {
            flags.push(f);
            i += 1;
        }
        let mut width = String::new();
        while let Some(&d) = chars.get(i).filter(|c| c.is_ascii_digit()) {
            width.push(d);
            i += 1;
        }
        let mut precision = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            let mut p = String::new();
            while let Some(&d) = chars.get(i).filter(|c| c.is_ascii_digit()) {
                p.push(d);
                i += 1;
            }
            precision = Some(p);
        }
        while chars.get(i).is_some_and(|c| "hlL".contains(*c)) {
            i += 1;
        }
        let Some(&conversion) = chars.get(i) else {
            render = Err("an incomplete conversion".to_string());
            break;
        };
        i += 1;
        let word = name.chars().all(|c| c.is_alphanumeric() || c == '_') && !name.is_empty();
        if word && "diouxefgcrsa%".contains(conversion.to_ascii_lowercase()) {
            fields += 1;
        }
        if render.is_err() {
            continue;
        }
        match percent_field(&name, &flags, &width, precision.as_deref(), conversion) {
            Ok(e) => parts.push(e),
            Err(why) => render = Err(why),
        }
    }
    let invalid = (fields == 0).then(|| format!("Invalid format '{fmt}' for '%' style"));
    Compiled {
        render: render.map(|_| parts.finish()),
        invalid,
    }
}

fn percent_field(
    name: &str,
    flags: &str,
    width: &str,
    precision: Option<&str>,
    conversion: char,
) -> Result<IRExpr, String> {
    let kind = field(name)?;
    let left = flags.contains('-');
    let sign = if flags.contains('+') {
        "+"
    } else if flags.contains(' ') {
        " "
    } else {
        ""
    };
    let alt = if flags.contains('#') { "#" } else { "" };
    let zero = if flags.contains('0') && !left {
        "0"
    } else {
        ""
    };
    let align = if left { "<" } else { "" };
    let precision_spec = precision.map(|p| format!(".{}", if p.is_empty() { "0" } else { p }));
    match conversion {
        's' | 'r' | 'a' => {
            let spec = if width.is_empty() && precision.is_none() {
                String::new()
            } else {
                format!(
                    "{}{width}{}",
                    if left { "<" } else { ">" },
                    precision_spec.unwrap_or_default()
                )
            };
            Ok(formatted(as_text(name, kind, conversion), &spec))
        }
        'd' | 'i' | 'u' => {
            if precision.is_some() {
                return Err("a precision on an integer conversion".to_string());
            }
            let value = match kind {
                Field::Int => read(name),
                Field::Float => call("int", vec![read(name)]),
                _ => {
                    return Err(format!(
                        "'%({name}){conversion}' raises TypeError: %{conversion} format: a \
                         real number is required, not str"
                    ))
                }
            };
            Ok(call(
                "format",
                vec![value, text(&format!("{align}{sign}{zero}{width}d"))],
            ))
        }
        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
            let value = match kind {
                Field::Int => call("float", vec![read(name)]),
                Field::Float => read(name),
                _ => {
                    return Err(format!(
                        "'%({name}){conversion}' raises TypeError: must be real number, not str"
                    ))
                }
            };
            Ok(call(
                "format",
                vec![
                    value,
                    text(&format!(
                        "{align}{sign}{alt}{zero}{width}{}{conversion}",
                        precision_spec.unwrap_or_default()
                    )),
                ],
            ))
        }
        'x' | 'X' | 'o' => {
            if precision.is_some() || kind != Field::Int {
                return Err(format!(
                    "'%({name}){conversion}' of a value that is not an int"
                ));
            }
            Ok(call(
                "format",
                vec![
                    read(name),
                    text(&format!("{align}{sign}{alt}{zero}{width}{conversion}")),
                ],
            ))
        }
        other => Err(format!("the conversion '%{other}'")),
    }
}

/// `fmt.format(**record.__dict__)`.
fn braces(fmt: &str) -> Compiled {
    let chars: Vec<char> = fmt.chars().collect();
    let mut parts = Parts::new();
    let mut fields = 0;
    let mut invalid = None;
    let mut render: Result<(), String> = Ok(());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '{' if chars.get(i) == Some(&'{') => {
                parts.literal.push('{');
                i += 1;
            }
            '}' if chars.get(i) == Some(&'}') => {
                parts.literal.push('}');
                i += 1;
            }
            '}' => {
                render = Err("a single '}'".to_string());
                break;
            }
            '{' => {
                let Some(close) = chars[i..].iter().position(|&c| c == '}') else {
                    render = Err("an unterminated '{'".to_string());
                    break;
                };
                let inner: String = chars[i..i + close].iter().collect();
                i += close + 1;
                if inner.contains('{') {
                    render = Err("a nested replacement field".to_string());
                    break;
                }
                let (head, spec) = match inner.split_once(':') {
                    Some((h, s)) => (h, s),
                    None => (inner.as_str(), ""),
                };
                let (name, conversion) = match head.split_once('!') {
                    Some((n, c)) => (n, Some(c)),
                    None => (head, None),
                };
                if !name.is_empty() {
                    let word = name.chars().all(|c| c.is_alphanumeric() || c == '_');
                    if !word && invalid.is_none() {
                        invalid = Some(format!(
                            "invalid format: invalid field name/expression: '{name}'"
                        ));
                    }
                    fields += 1;
                }
                if let Some(conv) = conversion {
                    if !matches!(conv, "r" | "s" | "a") && invalid.is_none() {
                        invalid = Some(format!("invalid format: invalid conversion: '{conv}'"));
                    }
                }
                if render.is_err() {
                    continue;
                }
                if name.is_empty() || name.chars().all(|c| c.is_ascii_digit()) {
                    render = Err("a positional field".to_string());
                    continue;
                }
                if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    render = Err(format!("the field '{name}'"));
                    continue;
                }
                let kind = match field(name) {
                    Ok(k) => k,
                    Err(why) => {
                        render = Err(why);
                        continue;
                    }
                };
                let value = match conversion {
                    Some(c) => as_text(name, kind, c.chars().next().unwrap_or('s')),
                    None if kind == Field::None => text("None"),
                    None => read(name),
                };
                let value = if spec.is_empty() && conversion.is_none() {
                    call("str", vec![value])
                } else {
                    formatted(value, spec)
                };
                parts.push(value);
            }
            other => parts.literal.push(other),
        }
    }
    if invalid.is_none() && fields == 0 {
        invalid = Some("invalid format: no fields".to_string());
    }
    Compiled {
        render: render.map(|_| parts.finish()),
        invalid,
    }
}

/// `string.Template(fmt).substitute(**record.__dict__)`.
fn template(fmt: &str) -> Compiled {
    let chars: Vec<char> = fmt.chars().collect();
    let mut parts = Parts::new();
    let mut fields = 0;
    let mut bare = false;
    let mut render: Result<(), String> = Ok(());
    let ident_start = |c: char| c.is_ascii_alphabetic() || c == '_';
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '$' {
            parts.literal.push(c);
            continue;
        }
        let name = match chars.get(i) {
            Some('$') => {
                parts.literal.push('$');
                i += 1;
                continue;
            }
            Some('{') => {
                let close = chars[i + 1..].iter().position(|&c| c == '}');
                match close {
                    Some(n)
                        if n > 0
                            && ident_start(chars[i + 1])
                            && chars[i + 1..i + 1 + n].iter().all(|&c| ident(c)) =>
                    {
                        let name: String = chars[i + 1..i + 1 + n].iter().collect();
                        i += n + 2;
                        name
                    }
                    _ => {
                        bare = true;
                        render = Err("an invalid placeholder".to_string());
                        continue;
                    }
                }
            }
            Some(&c) if ident_start(c) => {
                let start = i;
                while chars.get(i).is_some_and(|&c| ident(c)) {
                    i += 1;
                }
                chars[start..i].iter().collect()
            }
            _ => {
                bare = true;
                render = Err("an invalid placeholder".to_string());
                continue;
            }
        };
        fields += 1;
        if render.is_err() {
            continue;
        }
        match field(&name) {
            Ok(kind) => parts.push(as_text(&name, kind, 's')),
            Err(why) => render = Err(why),
        }
    }
    let invalid = if bare {
        Some("invalid format: bare '$' not allowed".to_string())
    } else if fields == 0 {
        Some("invalid format: no fields".to_string())
    } else {
        None
    };
    Compiled {
        render: render.map(|_| parts.finish()),
        invalid,
    }
}
