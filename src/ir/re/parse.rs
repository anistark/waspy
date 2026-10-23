//! A port of CPython's `re/_parser.py` (Python 3.13) for str patterns: the
//! pattern parsed into the same nodes, with the same `re.error` messages and
//! positions, which the compiler reports where CPython would raise them.

use std::collections::HashMap;

pub const FLAG_IGNORECASE: u32 = 2;
pub const FLAG_LOCALE: u32 = 4;
pub const FLAG_MULTILINE: u32 = 8;
pub const FLAG_DOTALL: u32 = 16;
pub const FLAG_UNICODE: u32 = 32;
pub const FLAG_VERBOSE: u32 = 64;
pub const FLAG_DEBUG: u32 = 128;
pub const FLAG_ASCII: u32 = 256;
const TYPE_FLAGS: u32 = FLAG_ASCII | FLAG_LOCALE | FLAG_UNICODE;
const MAXGROUPS: usize = 1_073_741_823;
pub const MAXREPEAT: u32 = u32::MAX;
const MAXWIDTH: u64 = 1 << 63;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Digit,
    NotDigit,
    Space,
    NotSpace,
    Word,
    NotWord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetItem {
    Negate,
    Literal(u32),
    Range(u32, u32),
    Category(Category),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    Beginning,
    BeginningString,
    End,
    EndString,
    Boundary,
    NonBoundary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Max,
    Min,
    Possessive,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Literal(u32),
    NotLiteral(u32),
    Any,
    In(Vec<SetItem>),
    At(At),
    Branch(Vec<Vec<Node>>),
    Subpattern {
        group: Option<usize>,
        add: u32,
        del: u32,
        body: Vec<Node>,
    },
    Repeat {
        kind: Repeat,
        min: u32,
        max: u32,
        body: Vec<Node>,
    },
    Assert {
        behind: bool,
        negate: bool,
        body: Vec<Node>,
    },
    Failure,
    Atomic(Vec<Node>),
    GroupRef(usize),
    GroupRefExists {
        group: usize,
        yes: Vec<Node>,
        no: Option<Vec<Node>>,
    },
}

/// A pattern, parsed.
#[derive(Debug)]
pub struct Parsed {
    pub nodes: Vec<Node>,
    /// The pattern's flags with its global inline flags and UNICODE (or
    /// ASCII), as `Pattern.flags` reads.
    pub flags: u32,
    /// The groups, group 0 included.
    pub groups: usize,
    pub groupdict: Vec<(String, usize)>,
}

/// `re.error`'s message: the text, the position, and its line and column
/// when the pattern has several lines.
pub fn error_message(msg: &str, pattern: &str, pos: usize) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = format!("{msg} at position {pos}");
    if chars.contains(&'\n') {
        let before = &chars[..pos.min(chars.len())];
        let line = before.iter().filter(|&&c| c == '\n').count() + 1;
        let col = match before.iter().rposition(|&c| c == '\n') {
            Some(nl) => pos - nl,
            None => pos + 1,
        };
        out = format!("{out} (line {line}, column {col})");
    }
    out
}

pub type Error = String;

/// The tokenizer: a character, or a backslash and the character after it.
struct Source<'a> {
    pattern: &'a str,
    chars: Vec<char>,
    index: usize,
    next: Option<String>,
}

impl<'a> Source<'a> {
    fn new(pattern: &'a str) -> Result<Source<'a>, Error> {
        let mut s = Source {
            pattern,
            chars: pattern.chars().collect(),
            index: 0,
            next: None,
        };
        s.advance()?;
        Ok(s)
    }
    fn advance(&mut self) -> Result<(), Error> {
        let index = self.index;
        let Some(&c) = self.chars.get(index) else {
            self.next = None;
            return Ok(());
        };
        let mut token = c.to_string();
        let mut end = index + 1;
        if c == '\\' {
            match self.chars.get(index + 1) {
                Some(&d) => {
                    token.push(d);
                    end += 1;
                }
                None => {
                    return Err(error_message(
                        "bad escape (end of pattern)",
                        self.pattern,
                        self.chars.len() - 1,
                    ))
                }
            }
        }
        self.index = end;
        self.next = Some(token);
        Ok(())
    }
    fn next_is(&self, s: &str) -> bool {
        self.next.as_deref() == Some(s)
    }
    fn next_in(&self, set: &str) -> bool {
        matches!(&self.next, Some(t) if t.chars().count() == 1 && set.contains(t.as_str()))
    }
    fn matches(&mut self, s: &str) -> Result<bool, Error> {
        if self.next_is(s) {
            self.advance()?;
            return Ok(true);
        }
        Ok(false)
    }
    fn get(&mut self) -> Result<Option<String>, Error> {
        let this = self.next.take();
        self.advance()?;
        Ok(this)
    }
    fn getwhile(&mut self, n: usize, set: &str) -> Result<String, Error> {
        let mut out = String::new();
        for _ in 0..n {
            if !self.next_in(set) {
                break;
            }
            out.push_str(self.next.as_deref().unwrap_or(""));
            self.advance()?;
        }
        Ok(out)
    }
    fn getuntil(&mut self, terminator: &str, name: &str) -> Result<String, Error> {
        let mut out = String::new();
        loop {
            let c = self.next.clone();
            self.advance()?;
            match c {
                None => {
                    if out.is_empty() {
                        return Err(self.error(&format!("missing {name}"), 0));
                    }
                    return Err(self.error(
                        &format!("missing {terminator}, unterminated name"),
                        out.chars().count(),
                    ));
                }
                Some(c) if c == terminator => {
                    if out.is_empty() {
                        return Err(self.error(&format!("missing {name}"), 1));
                    }
                    break;
                }
                Some(c) => out.push_str(&c),
            }
        }
        Ok(out)
    }
    fn tell(&self) -> usize {
        self.index - self.next.as_ref().map_or(0, |t| t.chars().count())
    }
    fn seek(&mut self, index: usize) -> Result<(), Error> {
        self.index = index;
        self.advance()
    }
    fn error(&self, msg: &str, offset: usize) -> Error {
        error_message(msg, self.pattern, self.tell().saturating_sub(offset))
    }
    fn checkgroupname(&self, name: &str, offset: usize) -> Result<(), Error> {
        if !is_identifier(name) {
            return Err(self.error(
                &format!("bad character in group name {}", py_repr(name)),
                name.chars().count() + offset,
            ));
        }
        Ok(())
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Python's `repr()` of a short string, as error messages quote names.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

struct State {
    flags: u32,
    groupdict: Vec<(String, usize)>,
    groupwidths: Vec<Option<(u64, u64)>>,
    lookbehindgroups: Option<usize>,
    grouprefpos: HashMap<usize, usize>,
}

impl State {
    fn groups(&self) -> usize {
        self.groupwidths.len()
    }
    fn opengroup(&mut self, name: Option<&str>) -> Result<usize, String> {
        let gid = self.groups();
        self.groupwidths.push(None);
        if self.groups() > MAXGROUPS {
            return Err("too many groups".to_string());
        }
        if let Some(name) = name {
            if let Some((_, ogid)) = self.groupdict.iter().find(|(n, _)| n == name) {
                return Err(format!(
                    "redefinition of group name {} as group {gid}; was group {ogid}",
                    py_repr(name)
                ));
            }
            self.groupdict.push((name.to_string(), gid));
        }
        Ok(gid)
    }
    fn checkgroup(&self, gid: usize) -> bool {
        gid < self.groups() && self.groupwidths[gid].is_some()
    }
    fn checklookbehindgroup(&self, gid: usize, source: &Source) -> Result<(), Error> {
        if let Some(lb) = self.lookbehindgroups {
            if !self.checkgroup(gid) {
                return Err(source.error("cannot refer to an open group", 0));
            }
            if gid >= lb {
                return Err(source.error(
                    "cannot refer to group defined in the same lookbehind subpattern",
                    0,
                ));
            }
        }
        Ok(())
    }
    fn group_id(&self, name: &str) -> Option<usize> {
        self.groupdict
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, g)| *g)
    }
}

const SPECIAL: &str = ".\\[{()*+?^$|";
const DIGITS: &str = "0123456789";
const OCTDIGITS: &str = "01234567";
const HEXDIGITS: &str = "0123456789abcdefABCDEF";
const WHITESPACE: &str = " \t\n\r\x0b\x0c";

fn escape_literal(e: &str) -> Option<u32> {
    Some(match e {
        "\\a" => 7,
        "\\b" => 8,
        "\\f" => 12,
        "\\n" => 10,
        "\\r" => 13,
        "\\t" => 9,
        "\\v" => 11,
        "\\\\" => 92,
        _ => return None,
    })
}

enum Escaped {
    Node(Node),
    Set(SetItem),
}

fn category(e: &str) -> Option<Escaped> {
    Some(match e {
        "\\A" => Escaped::Node(Node::At(At::BeginningString)),
        "\\b" => Escaped::Node(Node::At(At::Boundary)),
        "\\B" => Escaped::Node(Node::At(At::NonBoundary)),
        "\\d" => Escaped::Set(SetItem::Category(Category::Digit)),
        "\\D" => Escaped::Set(SetItem::Category(Category::NotDigit)),
        "\\s" => Escaped::Set(SetItem::Category(Category::Space)),
        "\\S" => Escaped::Set(SetItem::Category(Category::NotSpace)),
        "\\w" => Escaped::Set(SetItem::Category(Category::Word)),
        "\\W" => Escaped::Set(SetItem::Category(Category::NotWord)),
        "\\Z" => Escaped::Node(Node::At(At::EndString)),
        _ => return None,
    })
}

/// `\x`, `\u`, `\U`: exactly `n` hex digits, as a code point.
fn hex_escape(source: &mut Source, escape: &str, n: usize) -> Result<u32, Error> {
    let digits = source.getwhile(n, HEXDIGITS)?;
    let full = format!("{escape}{digits}");
    if digits.len() != n {
        return Err(source.error(&format!("incomplete escape {full}"), full.chars().count()));
    }
    let c = u32::from_str_radix(&digits, 16).expect("hex digits");
    if char::from_u32(c).is_none() && n == 8 {
        return Err(source.error(&format!("bad escape {full}"), full.chars().count()));
    }
    Ok(c)
}

fn class_escape(source: &mut Source, escape: &str) -> Result<SetItem, Error> {
    if let Some(c) = escape_literal(escape) {
        return Ok(SetItem::Literal(c));
    }
    if let Some(Escaped::Set(item)) = category(escape) {
        return Ok(item);
    }
    let c = escape.chars().nth(1).expect("an escape");
    match c {
        'x' => return Ok(SetItem::Literal(hex_escape(source, escape, 2)?)),
        'u' => return Ok(SetItem::Literal(hex_escape(source, escape, 4)?)),
        'U' => return Ok(SetItem::Literal(hex_escape(source, escape, 8)?)),
        'N' => {
            return Err(source.error(
                "\\N{...} named characters are not supported by this compiler",
                2,
            ))
        }
        c if OCTDIGITS.contains(c) => {
            let more = source.getwhile(2, OCTDIGITS)?;
            let full = format!("{escape}{more}");
            let v = u32::from_str_radix(&full[1..], 8).expect("octal");
            if v > 0o377 {
                return Err(source.error(
                    &format!("octal escape value {full} outside of range 0-0o377"),
                    full.chars().count(),
                ));
            }
            return Ok(SetItem::Literal(v));
        }
        c if DIGITS.contains(c) => {}
        c if c.is_ascii_alphabetic() => {}
        c => return Ok(SetItem::Literal(c as u32)),
    }
    Err(source.error(&format!("bad escape {escape}"), escape.chars().count()))
}

fn escape(source: &mut Source, escape: &str, state: &State) -> Result<Node, Error> {
    if let Some(cat) = category(escape) {
        return Ok(match cat {
            Escaped::Node(n) => n,
            Escaped::Set(item) => Node::In(vec![item]),
        });
    }
    if let Some(c) = escape_literal(escape) {
        return Ok(Node::Literal(c));
    }
    let c = escape.chars().nth(1).expect("an escape");
    match c {
        'x' => return Ok(Node::Literal(hex_escape(source, escape, 2)?)),
        'u' => return Ok(Node::Literal(hex_escape(source, escape, 4)?)),
        'U' => return Ok(Node::Literal(hex_escape(source, escape, 8)?)),
        'N' => {
            return Err(source.error(
                "\\N{...} named characters are not supported by this compiler",
                2,
            ))
        }
        '0' => {
            let more = source.getwhile(2, OCTDIGITS)?;
            let v = u32::from_str_radix(&format!("0{more}"), 8).expect("octal");
            return Ok(Node::Literal(v));
        }
        c if DIGITS.contains(c) => {
            let mut full = escape.to_string();
            if source.next_in(DIGITS) {
                full.push_str(&source.get()?.expect("a digit"));
                let b: Vec<char> = full.chars().collect();
                if OCTDIGITS.contains(b[1]) && OCTDIGITS.contains(b[2]) && source.next_in(OCTDIGITS)
                {
                    full.push_str(&source.get()?.expect("a digit"));
                    let v = u32::from_str_radix(&full[1..], 8).expect("octal");
                    if v > 0o377 {
                        return Err(source.error(
                            &format!("octal escape value {full} outside of range 0-0o377"),
                            full.chars().count(),
                        ));
                    }
                    return Ok(Node::Literal(v));
                }
            }
            let group: usize = full[1..].parse().expect("digits");
            if group < state.groups() {
                if !state.checkgroup(group) {
                    return Err(source.error("cannot refer to an open group", full.chars().count()));
                }
                state.checklookbehindgroup(group, source)?;
                return Ok(Node::GroupRef(group));
            }
            return Err(source.error(
                &format!("invalid group reference {group}"),
                full.chars().count() - 1,
            ));
        }
        c if c.is_ascii_alphabetic() => {}
        c => return Ok(Node::Literal(c as u32)),
    }
    Err(source.error(&format!("bad escape {escape}"), escape.chars().count()))
}

fn getwidth(nodes: &[Node], state: &State) -> (u64, u64) {
    let (mut lo, mut hi): (u64, u64) = (0, 0);
    for node in nodes {
        let (i, j) = match node {
            Node::Branch(items) => {
                let mut i = MAXWIDTH;
                let mut j = 0;
                for item in items {
                    let (l, h) = getwidth(item, state);
                    i = i.min(l);
                    j = j.max(h);
                }
                (i, j)
            }
            Node::Atomic(body) | Node::Subpattern { body, .. } => getwidth(body, state),
            Node::Repeat { min, max, body, .. } => {
                let (i, j) = getwidth(body, state);
                let lo_add = i.saturating_mul(*min as u64);
                let hi_add = if *max == MAXREPEAT && j > 0 {
                    MAXWIDTH
                } else {
                    j.saturating_mul(if *max == MAXREPEAT { 0 } else { *max as u64 })
                };
                (lo_add, hi_add)
            }
            Node::Literal(_) | Node::NotLiteral(_) | Node::Any | Node::In(_) => (1, 1),
            Node::GroupRef(g) => state.groupwidths[*g].unwrap_or((0, 0)),
            Node::GroupRefExists { yes, no, .. } => {
                let (i, j) = getwidth(yes, state);
                match no {
                    Some(no) => {
                        let (l, h) = getwidth(no, state);
                        (i.min(l), j.max(h))
                    }
                    None => (0, j),
                }
            }
            Node::At(_) | Node::Assert { .. } | Node::Failure => (0, 0),
        };
        lo = lo.saturating_add(i).min(MAXWIDTH);
        hi = hi.saturating_add(j).min(MAXWIDTH);
    }
    (lo, hi)
}

fn parse_sub(
    source: &mut Source,
    state: &mut State,
    verbose: bool,
    nested: usize,
) -> Result<Vec<Node>, Error> {
    let mut items = Vec::new();
    let mut verbose = verbose;
    loop {
        let first = nested == 0 && items.is_empty();
        items.push(parse(source, state, verbose, nested + 1, first)?);
        if !source.matches("|")? {
            break;
        }
        if nested == 0 {
            verbose = state.flags & FLAG_VERBOSE != 0;
        }
    }
    if items.len() == 1 {
        return Ok(items.pop().expect("an item"));
    }
    let mut out = Vec::new();
    // A prefix every branch shares moves out of the branch.
    while let Some(prefix) = items[0].first().cloned() {
        if items.iter().all(|item| item.first() == Some(&prefix)) {
            for item in &mut items {
                item.remove(0);
            }
            out.push(prefix);
            continue;
        }
        break;
    }
    // Branches of one character each are a set.
    let mut set = Vec::new();
    let mut all = true;
    for item in &items {
        match item.as_slice() {
            [Node::Literal(c)] => set.push(SetItem::Literal(*c)),
            [Node::In(av)] if av.first() != Some(&SetItem::Negate) => {
                set.extend(av.iter().copied())
            }
            _ => {
                all = false;
                break;
            }
        }
    }
    if all {
        let mut uniq = Vec::new();
        for item in set {
            if !uniq.contains(&item) {
                uniq.push(item);
            }
        }
        out.push(Node::In(uniq));
        return Ok(out);
    }
    out.push(Node::Branch(items));
    Ok(out)
}

fn parse_flags(
    source: &mut Source,
    state: &mut State,
    mut c: String,
) -> Result<Option<(u32, u32)>, Error> {
    let flag_of = |c: &str| -> Option<u32> {
        Some(match c {
            "i" => FLAG_IGNORECASE,
            "L" => FLAG_LOCALE,
            "m" => FLAG_MULTILINE,
            "s" => FLAG_DOTALL,
            "x" => FLAG_VERBOSE,
            "a" => FLAG_ASCII,
            "u" => FLAG_UNICODE,
            _ => return None,
        })
    };
    let is_alpha = |c: &str| c.chars().all(char::is_alphabetic);
    let (mut add, mut del) = (0, 0);
    if c != "-" {
        loop {
            let flag = flag_of(&c).expect("a flag");
            if c == "L" {
                return Err(source.error(
                    "bad inline flags: cannot use 'L' flag with a str pattern",
                    0,
                ));
            }
            add |= flag;
            if flag & TYPE_FLAGS != 0 && add & TYPE_FLAGS != flag {
                return Err(source.error(
                    "bad inline flags: flags 'a', 'u' and 'L' are incompatible",
                    0,
                ));
            }
            c = match source.get()? {
                None => return Err(source.error("missing -, : or )", 0)),
                Some(c) => c,
            };
            if c == ")" || c == "-" || c == ":" {
                break;
            }
            if flag_of(&c).is_none() {
                let msg = if is_alpha(&c) {
                    "unknown flag"
                } else {
                    "missing -, : or )"
                };
                return Err(source.error(msg, c.chars().count()));
            }
        }
    }
    if c == ")" {
        state.flags |= add;
        return Ok(None);
    }
    if c == "-" {
        c = match source.get()? {
            None => return Err(source.error("missing flag", 0)),
            Some(c) => c,
        };
        if flag_of(&c).is_none() {
            let msg = if is_alpha(&c) {
                "unknown flag"
            } else {
                "missing flag"
            };
            return Err(source.error(msg, c.chars().count()));
        }
        loop {
            let flag = flag_of(&c).expect("a flag");
            if flag & TYPE_FLAGS != 0 {
                return Err(source.error(
                    "bad inline flags: cannot turn off flags 'a', 'u' and 'L'",
                    0,
                ));
            }
            del |= flag;
            c = match source.get()? {
                None => return Err(source.error("missing :", 0)),
                Some(c) => c,
            };
            if c == ":" {
                break;
            }
            if flag_of(&c).is_none() {
                let msg = if is_alpha(&c) {
                    "unknown flag"
                } else {
                    "missing :"
                };
                return Err(source.error(msg, c.chars().count()));
            }
        }
    }
    if add & del != 0 {
        return Err(source.error("bad inline flags: flag turned on and off", 1));
    }
    Ok(Some((add, del)))
}

fn parse(
    source: &mut Source,
    state: &mut State,
    verbose: bool,
    nested: usize,
    first: bool,
) -> Result<Vec<Node>, Error> {
    let mut out: Vec<Node> = Vec::new();
    let mut verbose = verbose;
    while let Some(this) = source.next.clone() {
        if this == "|" || this == ")" {
            break;
        }
        source.get()?;
        if verbose {
            if WHITESPACE.contains(this.as_str()) && this.chars().count() == 1 {
                continue;
            }
            if this == "#" {
                loop {
                    match source.get()? {
                        None => break,
                        Some(c) if c == "\n" => break,
                        _ => {}
                    }
                }
                continue;
            }
        }
        let c0 = this.chars().next().expect("a token");
        if c0 == '\\' {
            out.push(escape(source, &this, state)?);
        } else if !SPECIAL.contains(c0) {
            out.push(Node::Literal(c0 as u32));
        } else if this == "[" {
            let here = source.tell() - 1;
            if source.next_is("[") {
                return Err(source.error(
                    "a possibly nested set ('[[') makes CPython print a FutureWarning, which is not supported",
                    0,
                ));
            }
            let negate = source.matches("^")?;
            let mut set: Vec<SetItem> = Vec::new();
            loop {
                let Some(this) = source.get()? else {
                    return Err(source.error("unterminated character set", source.tell() - here));
                };
                if this == "]" && !set.is_empty() {
                    break;
                }
                let code1 = if this.starts_with('\\') {
                    class_escape(source, &this)?
                } else {
                    if !set.is_empty() && "-&~|".contains(this.as_str()) && source.next_is(&this) {
                        return Err(source.error(
                            &format!("a possible set operation ('{this}{this}') makes CPython print a FutureWarning, which is not supported"),
                            0,
                        ));
                    }
                    SetItem::Literal(this.chars().next().expect("a char") as u32)
                };
                if source.matches("-")? {
                    let Some(that) = source.get()? else {
                        return Err(
                            source.error("unterminated character set", source.tell() - here)
                        );
                    };
                    if that == "]" {
                        set.push(code1);
                        set.push(SetItem::Literal('-' as u32));
                        break;
                    }
                    let code2 = if that.starts_with('\\') {
                        class_escape(source, &that)?
                    } else {
                        if that == "-" {
                            return Err(source.error(
                                "a possible set difference ('--') makes CPython print a FutureWarning, which is not supported",
                                0,
                            ));
                        }
                        SetItem::Literal(that.chars().next().expect("a char") as u32)
                    };
                    let (SetItem::Literal(lo), SetItem::Literal(hi)) = (code1, code2) else {
                        return Err(source.error(
                            &format!("bad character range {this}-{that}"),
                            this.chars().count() + 1 + that.chars().count(),
                        ));
                    };
                    if hi < lo {
                        return Err(source.error(
                            &format!("bad character range {this}-{that}"),
                            this.chars().count() + 1 + that.chars().count(),
                        ));
                    }
                    set.push(SetItem::Range(lo, hi));
                } else {
                    set.push(code1);
                }
            }
            let mut uniq: Vec<SetItem> = Vec::new();
            for item in set {
                if !uniq.contains(&item) {
                    uniq.push(item);
                }
            }
            match (uniq.as_slice(), negate) {
                ([SetItem::Literal(c)], true) => out.push(Node::NotLiteral(*c)),
                ([SetItem::Literal(c)], false) => out.push(Node::Literal(*c)),
                _ => {
                    if negate {
                        uniq.insert(0, SetItem::Negate);
                    }
                    out.push(Node::In(uniq));
                }
            }
        } else if "*+?{".contains(c0) {
            let here = source.tell();
            let (min, max) = match c0 {
                '?' => (0, 1),
                '*' => (0, MAXREPEAT),
                '+' => (1, MAXREPEAT),
                _ => {
                    if source.next_is("}") {
                        out.push(Node::Literal('{' as u32));
                        continue;
                    }
                    let mut lo = String::new();
                    let mut hi = String::new();
                    while source.next_in(DIGITS) {
                        lo.push_str(&source.get()?.expect("a digit"));
                    }
                    if source.matches(",")? {
                        while source.next_in(DIGITS) {
                            hi.push_str(&source.get()?.expect("a digit"));
                        }
                    } else {
                        hi = lo.clone();
                    }
                    if !source.matches("}")? {
                        out.push(Node::Literal('{' as u32));
                        source.seek(here)?;
                        continue;
                    }
                    let min = if lo.is_empty() {
                        0
                    } else {
                        match lo.parse::<u32>() {
                            Ok(v) if v < MAXREPEAT => v,
                            _ => {
                                return Err(
                                    "OverflowError: the repetition number is too large".to_string()
                                )
                            }
                        }
                    };
                    let max = if hi.is_empty() {
                        MAXREPEAT
                    } else {
                        match hi.parse::<u32>() {
                            Ok(v) if v < MAXREPEAT => v,
                            _ => {
                                return Err(
                                    "OverflowError: the repetition number is too large".to_string()
                                )
                            }
                        }
                    };
                    if max < min {
                        return Err(source
                            .error("min repeat greater than max repeat", source.tell() - here));
                    }
                    (min, max)
                }
            };
            let len_this = this.chars().count();
            let Some(item) = out.pop() else {
                return Err(source.error("nothing to repeat", source.tell() - here + len_this));
            };
            if matches!(item, Node::At(_)) {
                return Err(source.error("nothing to repeat", source.tell() - here + len_this));
            }
            if matches!(item, Node::Repeat { .. }) {
                return Err(source.error("multiple repeat", source.tell() - here + len_this));
            }
            let body = match item {
                Node::Subpattern {
                    group: None,
                    add: 0,
                    del: 0,
                    body,
                } => body,
                other => vec![other],
            };
            let kind = if source.matches("?")? {
                Repeat::Min
            } else if source.matches("+")? {
                Repeat::Possessive
            } else {
                Repeat::Max
            };
            out.push(Node::Repeat {
                kind,
                min,
                max,
                body,
            });
        } else if this == "." {
            out.push(Node::Any);
        } else if this == "(" {
            let start = source.tell() - 1;
            let mut capture = true;
            let mut atomic = false;
            let mut name: Option<String> = None;
            let (mut add, mut del) = (0, 0);
            if source.matches("?")? {
                let Some(char) = source.get()? else {
                    return Err(source.error("unexpected end of pattern", 0));
                };
                match char.as_str() {
                    "P" => {
                        if source.matches("<")? {
                            let n = source.getuntil(">", "group name")?;
                            source.checkgroupname(&n, 1)?;
                            name = Some(n);
                        } else if source.matches("=")? {
                            let n = source.getuntil(")", "group name")?;
                            source.checkgroupname(&n, 1)?;
                            let Some(gid) = state.group_id(&n) else {
                                return Err(source.error(
                                    &format!("unknown group name {}", py_repr(&n)),
                                    n.chars().count() + 1,
                                ));
                            };
                            if !state.checkgroup(gid) {
                                return Err(source.error(
                                    "cannot refer to an open group",
                                    n.chars().count() + 1,
                                ));
                            }
                            state.checklookbehindgroup(gid, source)?;
                            out.push(Node::GroupRef(gid));
                            continue;
                        } else {
                            let Some(c) = source.get()? else {
                                return Err(source.error("unexpected end of pattern", 0));
                            };
                            return Err(source.error(
                                &format!("unknown extension ?P{c}"),
                                c.chars().count() + 2,
                            ));
                        }
                    }
                    ":" => capture = false,
                    "#" => {
                        loop {
                            if source.next.is_none() {
                                return Err(source.error(
                                    "missing ), unterminated comment",
                                    source.tell() - start,
                                ));
                            }
                            if source.get()?.as_deref() == Some(")") {
                                break;
                            }
                        }
                        continue;
                    }
                    "=" | "!" | "<" => {
                        let mut dir_behind = false;
                        let mut kind = char.clone();
                        let mut lookbehind_set = false;
                        if kind == "<" {
                            let Some(c) = source.get()? else {
                                return Err(source.error("unexpected end of pattern", 0));
                            };
                            if c != "=" && c != "!" {
                                return Err(source.error(
                                    &format!("unknown extension ?<{c}"),
                                    c.chars().count() + 2,
                                ));
                            }
                            kind = c;
                            dir_behind = true;
                            if state.lookbehindgroups.is_none() {
                                state.lookbehindgroups = Some(state.groups());
                                lookbehind_set = true;
                            }
                        }
                        let p = parse_sub(source, state, verbose, nested + 1)?;
                        if lookbehind_set {
                            state.lookbehindgroups = None;
                        }
                        if !source.matches(")")? {
                            return Err(source.error(
                                "missing ), unterminated subpattern",
                                source.tell() - start,
                            ));
                        }
                        if dir_behind {
                            let (lo, hi) = getwidth(&p, state);
                            // CPython's compiler raises this, without a position.
                            if lo != hi {
                                return Err("look-behind requires fixed-width pattern".to_string());
                            }
                        }
                        if kind == "=" {
                            out.push(Node::Assert {
                                behind: dir_behind,
                                negate: false,
                                body: p,
                            });
                        } else if !p.is_empty() {
                            out.push(Node::Assert {
                                behind: dir_behind,
                                negate: true,
                                body: p,
                            });
                        } else {
                            out.push(Node::Failure);
                        }
                        continue;
                    }
                    "(" => {
                        let condname = source.getuntil(")", "group name")?;
                        let decimal =
                            !condname.is_empty() && condname.chars().all(|c| c.is_ascii_digit());
                        let condgroup = if !decimal {
                            source.checkgroupname(&condname, 1)?;
                            match state.group_id(&condname) {
                                Some(g) => g,
                                None => {
                                    return Err(source.error(
                                        &format!("unknown group name {}", py_repr(&condname)),
                                        condname.chars().count() + 1,
                                    ))
                                }
                            }
                        } else {
                            let g: usize = condname.parse().unwrap_or(usize::MAX);
                            if g == 0 {
                                return Err(
                                    source.error("bad group number", condname.chars().count() + 1)
                                );
                            }
                            if g >= MAXGROUPS {
                                return Err(source.error(
                                    &format!("invalid group reference {g}"),
                                    condname.chars().count() + 1,
                                ));
                            }
                            state
                                .grouprefpos
                                .entry(g)
                                .or_insert(source.tell() - condname.chars().count() - 1);
                            g
                        };
                        state.checklookbehindgroup(condgroup, source)?;
                        let yes = parse(source, state, verbose, nested + 1, false)?;
                        let no = if source.matches("|")? {
                            let no = parse(source, state, verbose, nested + 1, false)?;
                            if source.next_is("|") {
                                return Err(source
                                    .error("conditional backref with more than two branches", 0));
                            }
                            Some(no)
                        } else {
                            None
                        };
                        if !source.matches(")")? {
                            return Err(source.error(
                                "missing ), unterminated subpattern",
                                source.tell() - start,
                            ));
                        }
                        out.push(Node::GroupRefExists {
                            group: condgroup,
                            yes,
                            no,
                        });
                        continue;
                    }
                    ">" => {
                        capture = false;
                        atomic = true;
                    }
                    c if "iLmsxau-".contains(c) && c.chars().count() == 1 => {
                        match parse_flags(source, state, c.to_string())? {
                            None => {
                                if !first || !out.is_empty() {
                                    return Err(source.error(
                                        "global flags not at the start of the expression",
                                        source.tell() - start,
                                    ));
                                }
                                verbose = state.flags & FLAG_VERBOSE != 0;
                                continue;
                            }
                            Some((a, d)) => {
                                add = a;
                                del = d;
                                capture = false;
                            }
                        }
                    }
                    c => {
                        return Err(
                            source.error(&format!("unknown extension ?{c}"), c.chars().count() + 1)
                        );
                    }
                }
            }
            let group = if capture {
                match state.opengroup(name.as_deref()) {
                    Ok(g) => Some(g),
                    Err(msg) => {
                        return Err(
                            source.error(&msg, name.as_ref().map_or(0, |n| n.chars().count()) + 1)
                        )
                    }
                }
            } else {
                None
            };
            let sub_verbose = (verbose || add & FLAG_VERBOSE != 0) && del & FLAG_VERBOSE == 0;
            let p = parse_sub(source, state, sub_verbose, nested + 1)?;
            if !source.matches(")")? {
                return Err(
                    source.error("missing ), unterminated subpattern", source.tell() - start)
                );
            }
            if let Some(g) = group {
                state.groupwidths[g] = Some(getwidth(&p, state));
            }
            if atomic {
                out.push(Node::Atomic(p));
            } else {
                out.push(Node::Subpattern {
                    group,
                    add,
                    del,
                    body: p,
                });
            }
        } else if this == "^" {
            out.push(Node::At(At::Beginning));
        } else if this == "$" {
            out.push(Node::At(At::End));
        }
    }
    // Non-capturing groups without flags are unpacked.
    let mut unpacked = Vec::with_capacity(out.len());
    for node in out {
        match node {
            Node::Subpattern {
                group: None,
                add: 0,
                del: 0,
                body,
            } => unpacked.extend(body),
            other => unpacked.push(other),
        }
    }
    Ok(unpacked)
}

/// Parse a str pattern under `flags`, as `re.compile` does.
pub fn parse_pattern(pattern: &str, flags: u32) -> Result<Parsed, Error> {
    let mut source = Source::new(pattern)?;
    let mut state = State {
        flags,
        groupdict: Vec::new(),
        groupwidths: vec![None],
        lookbehindgroups: None,
        grouprefpos: HashMap::new(),
    };
    let nodes = parse_sub(&mut source, &mut state, flags & FLAG_VERBOSE != 0, 0)?;
    // fix_flags, for a str pattern.
    let mut flags = state.flags;
    if flags & FLAG_LOCALE != 0 {
        return Err("ValueError: cannot use LOCALE flag with a str pattern".to_string());
    }
    if flags & FLAG_ASCII == 0 {
        flags |= FLAG_UNICODE;
    } else if flags & FLAG_UNICODE != 0 {
        return Err("ValueError: ASCII and UNICODE flags are incompatible".to_string());
    }
    if source.next.is_some() {
        return Err(source.error("unbalanced parenthesis", 0));
    }
    let mut refs: Vec<(&usize, &usize)> = state.grouprefpos.iter().collect();
    refs.sort();
    for (g, pos) in refs {
        if *g >= state.groups() {
            return Err(error_message(
                &format!("invalid group reference {g}"),
                pattern,
                *pos,
            ));
        }
    }
    if flags & FLAG_DEBUG != 0 {
        return Err("re.DEBUG prints the parsed pattern, which is not supported".to_string());
    }
    Ok(Parsed {
        nodes,
        flags,
        groups: state.groups(),
        groupdict: state.groupdict,
    })
}
