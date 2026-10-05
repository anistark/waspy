use crate::stdlib::StdlibValue;
use regex::{Regex, RegexBuilder};

pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "IGNORECASE" | "I" => Some(StdlibValue::Int(2)),
        "MULTILINE" | "M" => Some(StdlibValue::Int(8)),
        "DOTALL" | "S" => Some(StdlibValue::Int(16)),
        "VERBOSE" | "X" => Some(StdlibValue::Int(64)),
        "ASCII" | "A" => Some(StdlibValue::Int(256)),
        "UNICODE" | "U" => Some(StdlibValue::Int(32)),
        _ => None,
    }
}

pub fn get_function(func: &str) -> Option<ReFunction> {
    match func {
        "compile" => Some(ReFunction::Compile),
        "search" => Some(ReFunction::Search),
        "match" => Some(ReFunction::Match),
        "fullmatch" => Some(ReFunction::Fullmatch),
        "findall" => Some(ReFunction::Findall),
        "finditer" => Some(ReFunction::Finditer),
        "split" => Some(ReFunction::Split),
        "sub" => Some(ReFunction::Sub),
        "subn" => Some(ReFunction::Subn),
        "escape" => Some(ReFunction::Escape),
        "purge" => Some(ReFunction::Purge),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum ReFunction {
    Compile,
    Search,
    Match,
    Fullmatch,
    Findall,
    Finditer,
    Split,
    Sub,
    Subn,
    Escape,
    Purge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReFlags {
    pub ignorecase: bool,
    pub multiline: bool,
    pub dotall: bool,
    pub verbose: bool,
    pub ascii: bool,
    pub unicode: bool,
}

impl Default for ReFlags {
    fn default() -> Self {
        Self::new()
    }
}

impl ReFlags {
    pub fn new() -> Self {
        ReFlags {
            ignorecase: false,
            multiline: false,
            dotall: false,
            verbose: false,
            ascii: false,
            unicode: true,
        }
    }

    pub fn from_int(flags: i32) -> Self {
        ReFlags {
            ignorecase: (flags & 2) != 0,
            multiline: (flags & 8) != 0,
            dotall: (flags & 16) != 0,
            verbose: (flags & 64) != 0,
            ascii: (flags & 256) != 0,
            unicode: (flags & 32) != 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub matched: bool,
    pub start: usize,
    pub end: usize,
    pub group: String,
    pub groups: Vec<Option<String>>,
}

impl Default for MatchResult {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchResult {
    pub fn new() -> Self {
        MatchResult {
            matched: false,
            start: 0,
            end: 0,
            group: String::new(),
            groups: Vec::new(),
        }
    }

    pub fn from_captures(
        text: &str,
        captures: &regex::Captures,
        overall_match: &regex::Match,
    ) -> Self {
        let mut groups = Vec::new();
        for i in 1..captures.len() {
            groups.push(captures.get(i).map(|m| m.as_str().to_string()));
        }

        MatchResult {
            matched: true,
            start: overall_match.start(),
            end: overall_match.end(),
            group: text[overall_match.start()..overall_match.end()].to_string(),
            groups,
        }
    }

    pub fn span(&self) -> (usize, usize) {
        (self.start, self.end)
    }
}

fn build_regex(pattern: &str, flags: ReFlags) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(flags.ignorecase)
        .multi_line(flags.multiline)
        .dot_matches_new_line(flags.dotall)
        .unicode(!flags.ascii)
        .build()
        .map_err(|e| format!("regex error: {e}"))
}

pub fn compile(pattern: &str, flags: i32) -> Result<CompiledPattern, String> {
    let re_flags = ReFlags::from_int(flags);
    let regex = build_regex(pattern, re_flags)?;
    Ok(CompiledPattern {
        pattern: pattern.to_string(),
        regex,
        flags: re_flags,
    })
}

#[derive(Debug, Clone)]
pub struct CompiledPattern {
    pub pattern: String,
    pub regex: Regex,
    pub flags: ReFlags,
}

impl CompiledPattern {
    pub fn search(&self, text: &str) -> Option<MatchResult> {
        self.regex.captures(text).map(|caps| {
            let m = caps.get(0).unwrap();
            MatchResult::from_captures(text, &caps, &m)
        })
    }

    pub fn match_start(&self, text: &str) -> Option<MatchResult> {
        let anchored_pattern = format!("^(?:{})", self.pattern);
        if let Ok(re) = build_regex(&anchored_pattern, self.flags) {
            re.captures(text).map(|caps| {
                let m = caps.get(0).unwrap();
                MatchResult::from_captures(text, &caps, &m)
            })
        } else {
            None
        }
    }

    pub fn fullmatch(&self, text: &str) -> Option<MatchResult> {
        let anchored_pattern = format!("^(?:{})$", self.pattern);
        if let Ok(re) = build_regex(&anchored_pattern, self.flags) {
            re.captures(text).map(|caps| {
                let m = caps.get(0).unwrap();
                MatchResult::from_captures(text, &caps, &m)
            })
        } else {
            None
        }
    }

    pub fn findall(&self, text: &str) -> Vec<String> {
        self.regex
            .captures_iter(text)
            .map(|caps| {
                if caps.len() > 1 {
                    caps.get(1)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default()
                } else {
                    caps.get(0)
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_default()
                }
            })
            .collect()
    }

    pub fn finditer(&self, text: &str) -> Vec<MatchResult> {
        self.regex
            .captures_iter(text)
            .map(|caps| {
                let m = caps.get(0).unwrap();
                MatchResult::from_captures(text, &caps, &m)
            })
            .collect()
    }

    pub fn split(&self, text: &str, maxsplit: Option<usize>) -> Vec<String> {
        match maxsplit {
            Some(0) => vec![text.to_string()],
            Some(n) => self
                .regex
                .splitn(text, n + 1)
                .map(|s| s.to_string())
                .collect(),
            None => self.regex.split(text).map(|s| s.to_string()).collect(),
        }
    }
}

pub fn search(pattern: &str, text: &str, flags: i32) -> Option<MatchResult> {
    let compiled = compile(pattern, flags).ok()?;
    compiled.search(text)
}

pub fn match_start(pattern: &str, text: &str, flags: i32) -> Option<MatchResult> {
    let compiled = compile(pattern, flags).ok()?;
    compiled.match_start(text)
}

pub fn fullmatch(pattern: &str, text: &str, flags: i32) -> Option<MatchResult> {
    let compiled = compile(pattern, flags).ok()?;
    compiled.fullmatch(text)
}

pub fn findall(pattern: &str, text: &str, flags: i32) -> Vec<String> {
    match compile(pattern, flags) {
        Ok(compiled) => compiled.findall(text),
        Err(_) => Vec::new(),
    }
}

pub fn finditer(pattern: &str, text: &str, flags: i32) -> Vec<MatchResult> {
    match compile(pattern, flags) {
        Ok(compiled) => compiled.finditer(text),
        Err(_) => Vec::new(),
    }
}

pub fn split(pattern: &str, text: &str, maxsplit: Option<usize>, flags: i32) -> Vec<String> {
    match compile(pattern, flags) {
        Ok(compiled) => compiled.split(text, maxsplit),
        Err(_) => vec![text.to_string()],
    }
}

/// `re.escape`, character for character: CPython 3.7+ escapes exactly these
/// characters and no others (`regex::escape` escapes a different set, leaving
/// space and the whitespace controls bare).
pub fn escape(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Flags whose meaning the `regex` crate shares with CPython's `re` for the
/// patterns [`fold_sub`] accepts: IGNORECASE, MULTILINE, DOTALL, UNICODE, and
/// ASCII. VERBOSE strips whitespace by rules of its own and is refused.
const FOLDABLE_FLAGS: i32 = 2 | 8 | 16 | 32 | 256;

/// `re.sub(pattern, repl, text, count, flags)` over constants, folded at
/// compile time, or the reason the fold could differ from CPython.
///
/// The `regex` crate is not CPython's engine, so the fold is limited to the
/// subset where the two provably agree, and refuses everything else rather
/// than guess:
///
/// - ASCII pattern, replacement, and text (the Unicode classes differ at the
///   edges, and `\s` in Python also matches `\x1c`..`\x1f`);
/// - escapes limited to the classes, anchors, and escaped punctuation both
///   read the same way; no backreferences, lookaround, inline flags, nested or
///   set-operation classes, or `{` that is not a counted repetition;
/// - a pattern that cannot match the empty string, since the two engines
///   resume differently after an empty match;
/// - `$` only where the text has no newline (Python's also matches before a
///   final one);
/// - Python's replacement syntax, expanded here: `\1`..`\9`, `\g<n>`,
///   `\g<name>`, and the character escapes.
pub fn fold_sub(
    pattern: &str,
    repl: &str,
    text: &str,
    count: i32,
    flags: i32,
) -> Result<String, String> {
    if flags & !FOLDABLE_FLAGS != 0 {
        return Err("only the IGNORECASE, MULTILINE, DOTALL, and ASCII flags are folded".into());
    }
    if count < 0 {
        return Err("a negative count is not folded".into());
    }
    for (what, s) in [("pattern", pattern), ("replacement", repl), ("text", text)] {
        if !s.is_ascii() || s.chars().any(|c| ('\x1c'..='\x1f').contains(&c)) {
            return Err(format!("the {what} is not plain ASCII"));
        }
    }
    check_pattern(pattern)?;
    let re_flags = ReFlags::from_int(flags);
    if pattern.contains('$') && !re_flags.multiline && text.contains('\n') {
        return Err("'$' over text containing a newline is not folded".into());
    }
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(re_flags.ignorecase)
        .multi_line(re_flags.multiline)
        .dot_matches_new_line(re_flags.dotall)
        .unicode(!re_flags.ascii)
        .build()
        .parse(pattern)
        .map_err(|e| format!("the pattern is not one this compiler can fold: {e}"))?;
    if hir.properties().minimum_len().unwrap_or(0) == 0 {
        return Err("a pattern that can match the empty string is not folded".into());
    }
    let regex = build_regex(pattern, re_flags)?;
    let template = parse_template(repl, &regex)?;

    let mut out = String::new();
    let mut last = 0;
    for (n, caps) in regex.captures_iter(text).enumerate() {
        if count > 0 && n as i32 >= count {
            break;
        }
        let whole = caps.get(0).expect("group 0 always participates");
        out.push_str(&text[last..whole.start()]);
        for piece in &template {
            match piece {
                TemplatePiece::Text(t) => out.push_str(t),
                TemplatePiece::Group(g) => {
                    if let Some(m) = caps.get(*g) {
                        out.push_str(m.as_str());
                    }
                }
            }
        }
        last = whole.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// Lexical check of a pattern against the subset [`fold_sub`] folds.
fn check_pattern(pattern: &str) -> Result<(), String> {
    let refuse = |what: &str| Err(format!("{what} in a pattern is not folded"));
    let bytes = pattern.as_bytes();
    let mut i = 0;
    let mut in_class = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        let next = bytes.get(i + 1).map(|&b| b as char);
        match c {
            '\\' => {
                let Some(e) = next else {
                    return refuse("a trailing backslash");
                };
                let class_escape = "dDwWsSntrfv".contains(e);
                let anchor_escape = !in_class && "bBA".contains(e);
                if !(class_escape || anchor_escape || (e.is_ascii_punctuation())) {
                    return refuse(&format!("the escape '\\{e}'"));
                }
                i += 2;
                continue;
            }
            '[' if in_class => return refuse("a nested '['"),
            '[' => {
                in_class = true;
                // A leading ']' (or '^]') is a literal in Python.
                if next == Some('^') {
                    i += 1;
                }
                if bytes.get(i + 1) == Some(&b']') {
                    return refuse("a ']' first in a class");
                }
            }
            ']' if in_class => in_class = false,
            '&' | '-' | '~' if in_class && next == Some(c) => {
                return refuse("a doubled set operator in a class")
            }
            '(' if !in_class && next == Some('?') => {
                let rest = &pattern[i + 2..];
                if !(rest.starts_with(':') || rest.starts_with("P<")) {
                    return refuse("a '(?' group other than '(?:' and '(?P<name>'");
                }
            }
            '{' if !in_class => {
                let close = pattern[i..].find('}').map(|j| i + j);
                let body = close.map(|j| &pattern[i + 1..j]).unwrap_or("");
                let counted = !body.is_empty()
                    && !body.starts_with(',')
                    && body.chars().all(|ch| ch.is_ascii_digit() || ch == ',')
                    && body.matches(',').count() <= 1;
                if !counted {
                    return refuse("a '{' that is not a counted repetition");
                }
                i = close.expect("a counted repetition is closed") + 1;
                if bytes.get(i) == Some(&b'+') {
                    return refuse("a possessive quantifier");
                }
                continue;
            }
            '*' | '+' | '?' if !in_class && next == Some('+') => {
                return refuse("a possessive quantifier")
            }
            _ => {}
        }
        i += 1;
    }
    if in_class {
        return refuse("an unterminated class");
    }
    Ok(())
}

enum TemplatePiece {
    Text(String),
    Group(usize),
}

/// Parse a Python replacement string the way `re` does, refusing the forms
/// CPython rejects or reads as octal.
fn parse_template(repl: &str, regex: &Regex) -> Result<Vec<TemplatePiece>, String> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut chars = repl.chars().peekable();
    let group_count = regex.captures_len();
    while let Some(c) = chars.next() {
        if c != '\\' {
            text.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            return Err("the replacement ends in a backslash".into());
        };
        let group = match e {
            '1'..='9' => {
                if chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                    return Err(
                        "a two-digit group reference in the replacement is not folded".into(),
                    );
                }
                Some(e as usize - '0' as usize)
            }
            'g' => {
                if chars.next() != Some('<') {
                    return Err("a malformed '\\g' in the replacement".into());
                }
                let name: String = chars.by_ref().take_while(|&ch| ch != '>').collect();
                let index = match name.parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => regex
                        .capture_names()
                        .position(|n| n == Some(name.as_str()))
                        .ok_or_else(|| format!("unknown group name '{name}' in the replacement"))?,
                };
                Some(index)
            }
            'a' => {
                text.push('\x07');
                None
            }
            'b' => {
                text.push('\x08');
                None
            }
            'f' => {
                text.push('\x0c');
                None
            }
            'n' => {
                text.push('\n');
                None
            }
            'r' => {
                text.push('\r');
                None
            }
            't' => {
                text.push('\t');
                None
            }
            'v' => {
                text.push('\x0b');
                None
            }
            '\\' => {
                text.push('\\');
                None
            }
            other if other.is_ascii_alphanumeric() => {
                return Err(format!("the replacement escape '\\{other}' is not folded"));
            }
            other => {
                text.push('\\');
                text.push(other);
                None
            }
        };
        if let Some(g) = group {
            if g >= group_count {
                return Err(format!(
                    "the replacement refers to group {g}, which the pattern lacks"
                ));
            }
            if !text.is_empty() {
                pieces.push(TemplatePiece::Text(std::mem::take(&mut text)));
            }
            pieces.push(TemplatePiece::Group(g));
        }
    }
    if !text.is_empty() {
        pieces.push(TemplatePiece::Text(text));
    }
    Ok(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_and_search() {
        let compiled = compile(r"\d+", 0).unwrap();
        let result = compiled.search("abc123def").unwrap();
        assert!(result.matched);
        assert_eq!(result.group, "123");
        assert_eq!(result.start, 3);
        assert_eq!(result.end, 6);
    }

    #[test]
    fn test_match_start() {
        let result = match_start(r"\d+", "123abc", 0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().group, "123");

        let result = match_start(r"\d+", "abc123", 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_fullmatch() {
        let result = fullmatch(r"\d+", "123", 0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().group, "123");

        let result = fullmatch(r"\d+", "123abc", 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_findall() {
        let results = findall(r"\d+", "abc123def456", 0);
        assert_eq!(results, vec!["123", "456"]);
    }

    #[test]
    fn test_findall_with_groups() {
        let results = findall(r"(\d+)", "abc123def456", 0);
        assert_eq!(results, vec!["123", "456"]);
    }

    #[test]
    fn test_split() {
        let results = split(r"\s+", "a b  c", None, 0);
        assert_eq!(results, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_split_with_maxsplit() {
        let results = split(r"\s+", "a b c d", Some(2), 0);
        assert_eq!(results, vec!["a", "b", "c d"]);
    }

    // Expected values below are CPython 3.13's for the same call.
    #[test]
    fn test_fold_sub() {
        assert_eq!(fold_sub(r"\d+", "X", "a1b2c3", 0, 0).unwrap(), "aXbXcX");
        assert_eq!(fold_sub(r"\d+", "X", "a1b2c3", 2, 0).unwrap(), "aXbXc3");
        assert_eq!(fold_sub(r"\d", "", "a1b2c3", 0, 0).unwrap(), "abc");
        assert_eq!(
            fold_sub(r"(\w+)@(\w+)", r"\2 at \1", "me@host", 0, 0).unwrap(),
            "host at me"
        );
        assert_eq!(
            fold_sub(r"(?P<w>b+)", r"[\g<w>]", "abbc", 0, 0).unwrap(),
            "a[bb]c"
        );
        assert_eq!(fold_sub("a", "$x", "aa", 0, 0).unwrap(), "$x$x");
        assert_eq!(fold_sub("A", "-", "aA", 0, 2).unwrap(), "--");
        assert_eq!(fold_sub("x+", r"\n", "axb", 0, 0).unwrap(), "a\nb");
    }

    #[test]
    fn test_fold_sub_refuses_what_could_differ() {
        for (pattern, repl, text, flags) in [
            ("x*", "-", "abxd", 0),
            (r"(a)\1", "", "aa", 0),
            ("a(?=b)", "", "ab", 0),
            ("a$", "", "a\n", 0),
            ("[a&&b]", "", "a", 0),
            (r"\pL", "", "a", 0),
            ("a", r"\10", "a", 0),
            ("a", r"\q", "a", 0),
            ("a b", "", "ab", 64),
            ("é", "", "é", 0),
            ("a{,2}", "", "a", 0),
        ] {
            assert!(
                fold_sub(pattern, repl, text, 0, flags).is_err(),
                "{pattern:?} -> {repl:?} over {text:?} should not fold"
            );
        }
    }

    #[test]
    fn test_escape() {
        assert_eq!(escape("a.b*c?"), r"a\.b\*c\?");
        assert_eq!(escape("a b\t_c"), "a\\ b\\\t_c");
    }

    #[test]
    fn test_ignorecase_flag() {
        let result = search(r"abc", "ABC", 2);
        assert!(result.is_some());
        assert_eq!(result.unwrap().group, "ABC");
    }

    #[test]
    fn test_multiline_flag() {
        let result = search(r"^b", "a\nb", 8);
        assert!(result.is_some());
        assert_eq!(result.unwrap().group, "b");
    }

    #[test]
    fn test_dotall_flag() {
        let result = search(r"a.b", "a\nb", 16);
        assert!(result.is_some());
        assert_eq!(result.unwrap().group, "a\nb");
    }

    #[test]
    fn test_capture_groups() {
        let compiled = compile(r"(\d+)-(\d+)", 0).unwrap();
        let result = compiled.search("abc123-456def").unwrap();
        assert!(result.matched);
        assert_eq!(result.group, "123-456");
        assert_eq!(result.groups.len(), 2);
        assert_eq!(result.groups[0], Some("123".to_string()));
        assert_eq!(result.groups[1], Some("456".to_string()));
    }

    #[test]
    fn test_finditer() {
        let results = finditer(r"\d+", "a1b22c333", 0);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].group, "1");
        assert_eq!(results[1].group, "22");
        assert_eq!(results[2].group, "333");
    }

    #[test]
    fn test_match_span() {
        let result = search(r"\d+", "abc123def", 0).unwrap();
        assert_eq!(result.span(), (3, 6));
    }
}
