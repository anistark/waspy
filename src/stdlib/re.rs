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

    pub fn sub(&self, repl: &str, text: &str, count: Option<usize>) -> String {
        match count {
            Some(0) => text.to_string(),
            Some(n) => {
                let mut result = text.to_string();
                for _ in 0..n {
                    if let Some(m) = self.regex.find(&result) {
                        let expanded = expand_replacement(repl, &result, &self.regex);
                        result =
                            format!("{}{}{}", &result[..m.start()], expanded, &result[m.end()..]);
                    } else {
                        break;
                    }
                }
                result
            }
            None => self.regex.replace_all(text, repl).to_string(),
        }
    }

    pub fn subn(&self, repl: &str, text: &str, count: Option<usize>) -> (String, usize) {
        let mut result = text.to_string();
        let mut num_subs = 0;
        let max_count = count.unwrap_or(usize::MAX);

        while num_subs < max_count {
            if let Some(m) = self.regex.find(&result) {
                let expanded = expand_replacement(repl, &result, &self.regex);
                result = format!("{}{}{}", &result[..m.start()], expanded, &result[m.end()..]);
                num_subs += 1;
            } else {
                break;
            }
        }
        (result, num_subs)
    }
}

fn expand_replacement(repl: &str, _text: &str, _regex: &Regex) -> String {
    repl.to_string()
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

pub fn sub(pattern: &str, repl: &str, text: &str, count: Option<usize>, flags: i32) -> String {
    match compile(pattern, flags) {
        Ok(compiled) => compiled.sub(repl, text, count),
        Err(_) => text.to_string(),
    }
}

pub fn subn(
    pattern: &str,
    repl: &str,
    text: &str,
    count: Option<usize>,
    flags: i32,
) -> (String, usize) {
    match compile(pattern, flags) {
        Ok(compiled) => compiled.subn(repl, text, count),
        Err(_) => (text.to_string(), 0),
    }
}

pub fn escape(pattern: &str) -> String {
    regex::escape(pattern)
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

    #[test]
    fn test_sub() {
        let result = sub(r"\d+", "X", "a1b2c3", None, 0);
        assert_eq!(result, "aXbXcX");
    }

    #[test]
    fn test_sub_with_count() {
        let result = sub(r"\d+", "X", "a1b2c3", Some(2), 0);
        assert_eq!(result, "aXbXc3");
    }

    #[test]
    fn test_subn() {
        let (result, count) = subn(r"\d+", "X", "a1b2c3", None, 0);
        assert_eq!(result, "aXbXcX");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_escape() {
        let result = escape("a.b*c?");
        assert_eq!(result, r"a\.b\*c\?");
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
