//! The parts of CPython 3.13's `configparser.ConfigParser` that
//! `logging.config.fileConfig()` reads its file with: sections and options
//! delimited by `=` or `:`, full-line `#` and `;` comments, values continued
//! on indented lines, the `DEFAULT` section, option names lowercased, a
//! repeated section or option an error, and `%(name)s` interpolation.
//!
//! A file CPython's parser rejects is rejected here, with its exception's
//! class and message.

/// An exception CPython raises: its class as a traceback names it, and its
/// message.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Raised {
    pub class: String,
    pub message: String,
}

impl Raised {
    pub(super) fn new(class: &str, message: String) -> Raised {
        Raised {
            class: class.to_string(),
            message,
        }
    }

    /// `KeyError(key)`, which prints the key's repr.
    pub(super) fn key(key: &str) -> Raised {
        Raised::new("KeyError", py_repr(key))
    }
}

type Options = Vec<(String, String)>;

/// A file as `ConfigParser.read()` leaves it.
#[derive(Debug, Default)]
pub(super) struct Ini {
    defaults: Options,
    sections: Vec<(String, Options)>,
}

const DEFAULT_SECTION: &str = "DEFAULT";
const MAX_INTERPOLATION_DEPTH: usize = 10;

/// `str.isspace()` of one character.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
pub(super) fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `str.rstrip()`.
fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `repr()` of a str.
pub(super) fn py_repr(s: &str) -> String {
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
            c if c == ' ' || crate::core::unicode::Predicate::Printable.test(&c.to_string()) => {
                out.push(c)
            }
            c => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// The lines of a file opened as text: a `\r\n` or a lone `\r` ends a line
/// as `\n` does, and each line keeps its end as `\n`.
fn lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line.push('\n');
                out.push(std::mem::take(&mut line));
            }
            '\n' => {
                line.push('\n');
                out.push(std::mem::take(&mut line));
            }
            c => line.push(c),
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn find<'o>(options: &'o Options, name: &str) -> Option<&'o String> {
    options.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

/// `BasicInterpolation.before_set()`, which a default's value passes.
fn check_default(value: &str) -> Result<(), Raised> {
    let mut rest = value.replace("%%", "");
    // The valid references, `%(name)s`, taken out.
    let mut out = String::new();
    while let Some(at) = rest.find("%(") {
        let after = &rest[at + 2..];
        match after.find(')') {
            Some(close) if close > 0 && after[close + 1..].starts_with('s') => {
                out.push_str(&rest[..at]);
                rest = after[close + 2..].to_string();
            }
            _ => {
                out.push_str(&rest[..at + 2]);
                rest = after.to_string();
            }
        }
    }
    out.push_str(&rest);
    match out.chars().position(|c| c == '%') {
        Some(at) => Err(Raised::new(
            "ValueError",
            format!(
                "invalid interpolation syntax in {} at position {at}",
                py_repr(value)
            ),
        )),
        None => Ok(()),
    }
}

impl Ini {
    /// `ConfigParser(defaults)` and `read(fname)` of a file holding `text`.
    pub(super) fn read(
        text: &str,
        fname: &str,
        defaults: &[(String, String)],
    ) -> Result<Ini, Raised> {
        let mut ini = Ini::default();
        for (key, value) in defaults {
            let key = crate::core::unicode::lower(key);
            if find(&ini.defaults, &key).is_some() {
                return Err(Raised::new(
                    "configparser.DuplicateOptionError",
                    format!(
                        "While reading from '<dict>': option {} in section 'DEFAULT' already \
                         exists",
                        py_repr(&key)
                    ),
                ));
            }
            check_default(value)?;
            ini.defaults.push((key, value.clone()));
        }
        ini.parse(text, fname)?;
        Ok(ini)
    }

    /// `RawConfigParser._read()`. The values are kept as their lines until
    /// the end, as CPython keeps them.
    fn parse(&mut self, text: &str, fname: &str) -> Result<(), Raised> {
        // The section being read: `None` before any, else its index in
        // `sections`, or the defaults.
        #[derive(Clone, Copy, PartialEq)]
        enum Cursor {
            Defaults,
            Section(usize),
        }
        let invalid =
            |message: String| Raised::new("RuntimeError", format!("{fname} is invalid: {message}"));
        let mut values: Vec<(Cursor, String, Vec<String>)> = Vec::new();
        let mut added_sections: Vec<String> = Vec::new();
        let mut added_options: Vec<(String, String)> = Vec::new();
        let mut cursor: Option<Cursor> = None;
        let mut sectname = String::new();
        let mut optname: Option<String> = None;
        let mut indent_level = 0usize;
        let mut errors: Vec<(usize, String)> = Vec::new();
        // The defaults given are read as values too, so a file's own replace
        // them.
        for (key, value) in std::mem::take(&mut self.defaults) {
            values.push((Cursor::Defaults, key, vec![value]));
        }
        for (k, line) in lines(text).into_iter().enumerate() {
            let lineno = k + 1;
            let stripped = strip(&line);
            let comment = stripped.starts_with('#') || stripped.starts_with(';');
            let clean = if comment { "" } else { stripped };
            if clean.is_empty() {
                // An empty line is part of the value before it.
                if let (false, Some(at), Some(name)) = (comment, cursor, optname.as_deref()) {
                    if !name.is_empty() {
                        if let Some((_, _, held)) = values
                            .iter_mut()
                            .rev()
                            .find(|(c, n, _)| *c == at && n == name)
                        {
                            held.push(String::new());
                        }
                    }
                }
                continue;
            }
            let cur_indent = line.chars().position(|c| !is_space(c)).unwrap_or(0);
            if let (Some(at), Some(name)) = (cursor, optname.as_deref()) {
                if !name.is_empty() && cur_indent > indent_level {
                    if let Some((_, _, held)) = values
                        .iter_mut()
                        .rev()
                        .find(|(c, n, _)| *c == at && n == name)
                    {
                        held.push(clean.to_string());
                    }
                    continue;
                }
            }
            indent_level = cur_indent;
            // `\[(?P<header>.+)\]`, matched at the start.
            let header = clean
                .strip_prefix('[')
                .and_then(|rest| rest.rfind(']').map(|end| &rest[..end]))
                .filter(|h| !h.is_empty());
            let Some(header) = header else {
                if cursor.is_none() {
                    return Err(invalid(format!(
                        "File contains no section headers.\nfile: {}, line: {lineno}\n{}",
                        py_repr(fname),
                        py_repr(&line)
                    )));
                }
                // An option: its name up to the first delimiter.
                let Some(at) = clean.find(['=', ':']) else {
                    errors.push((lineno, line.clone()));
                    continue;
                };
                let name = rstrip(&clean[..at]);
                if name.is_empty() {
                    errors.push((lineno, line.clone()));
                }
                let name = crate::core::unicode::lower(name);
                let key = (sectname.clone(), name.clone());
                if added_options.contains(&key) {
                    return Err(Raised::new(
                        "configparser.DuplicateOptionError",
                        format!(
                            "While reading from {} [line {lineno:2}]: option {} in section {} \
                             already exists",
                            py_repr(fname),
                            py_repr(&name),
                            py_repr(&sectname)
                        ),
                    ));
                }
                added_options.push(key);
                let value = strip(&clean[at + 1..]).to_string();
                let at = cursor.expect("a section");
                values.retain(|(c, n, _)| !(*c == at && *n == name));
                values.push((at, name.clone(), vec![value]));
                optname = Some(name);
                continue;
            };
            sectname = header.to_string();
            if let Some(at) = self.sections.iter().position(|(n, _)| *n == sectname) {
                if added_sections.contains(&sectname) {
                    return Err(Raised::new(
                        "configparser.DuplicateSectionError",
                        format!(
                            "While reading from {} [line {lineno:2}]: section {} already exists",
                            py_repr(fname),
                            py_repr(&sectname)
                        ),
                    ));
                }
                cursor = Some(Cursor::Section(at));
                added_sections.push(sectname.clone());
            } else if sectname == DEFAULT_SECTION {
                cursor = Some(Cursor::Defaults);
            } else {
                self.sections.push((sectname.clone(), Vec::new()));
                cursor = Some(Cursor::Section(self.sections.len() - 1));
                added_sections.push(sectname.clone());
            }
            optname = None;
        }
        if !errors.is_empty() {
            let mut message = format!("Source contains parsing errors: {}", py_repr(fname));
            for (lineno, line) in errors {
                message.push_str(&format!("\n\t[line {lineno:2}]: {}", py_repr(&line)));
            }
            return Err(invalid(message));
        }
        // `_join_multiline_values()`.
        for (at, name, held) in values {
            let value = rstrip(&held.join("\n")).to_string();
            match at {
                Cursor::Defaults => self.defaults.push((name, value)),
                Cursor::Section(k) => self.sections[k].1.push((name, value)),
            }
        }
        Ok(())
    }

    pub(super) fn has_section(&self, section: &str) -> bool {
        self.sections.iter().any(|(n, _)| n == section)
    }

    fn section(&self, section: &str) -> Option<&Options> {
        self.sections
            .iter()
            .find(|(n, _)| n == section)
            .map(|(_, o)| o)
    }

    /// `has_option()`: the section's own, or a default.
    pub(super) fn has_option(&self, section: &str, option: &str) -> bool {
        let option = crate::core::unicode::lower(option);
        match self.section(section) {
            Some(options) => {
                find(options, &option).is_some() || find(&self.defaults, &option).is_some()
            }
            None => false,
        }
    }

    /// The value before interpolation: the section's own, or a default.
    fn raw(&self, section: &str, option: &str) -> Option<&String> {
        let options = self.section(section)?;
        find(options, option).or_else(|| find(&self.defaults, option))
    }

    /// `cp.get(section, option, raw=True, fallback=None)`.
    pub(super) fn get_raw(&self, section: &str, option: &str) -> Option<String> {
        self.raw(section, &crate::core::unicode::lower(option))
            .cloned()
    }

    /// `cp[section][option]`: KeyError for a section or an option that is
    /// not there, else the value, interpolated.
    pub(super) fn item(&self, section: &str, option: &str) -> Result<String, Raised> {
        if !self.has_section(section) {
            return Err(Raised::key(section));
        }
        match self.get(section, option)? {
            Some(value) => Ok(value),
            None => Err(Raised::key(option)),
        }
    }

    /// `cp[section].get(option)`: the value, interpolated, or `None`.
    pub(super) fn get(&self, section: &str, option: &str) -> Result<Option<String>, Raised> {
        let option = crate::core::unicode::lower(option);
        let Some(value) = self.raw(section, &option) else {
            return Ok(None);
        };
        let mut out = String::new();
        self.interpolate(&option, &mut out, value, section, 1)?;
        Ok(Some(out))
    }

    /// `BasicInterpolation._interpolate_some()`.
    fn interpolate(
        &self,
        option: &str,
        out: &mut String,
        value: &str,
        section: &str,
        depth: usize,
    ) -> Result<(), Raised> {
        let rawval = self
            .raw(section, option)
            .cloned()
            .unwrap_or_else(|| value.to_string());
        if depth > MAX_INTERPOLATION_DEPTH {
            return Err(Raised::new(
                "configparser.InterpolationDepthError",
                format!(
                    "Recursion limit exceeded in value substitution: option {} in section {} \
                     contains an interpolation key which cannot be substituted in \
                     {MAX_INTERPOLATION_DEPTH} steps. Raw value: {}",
                    py_repr(option),
                    py_repr(section),
                    py_repr(&rawval)
                ),
            ));
        }
        let mut rest = value;
        while !rest.is_empty() {
            let Some(p) = rest.find('%') else {
                out.push_str(rest);
                return Ok(());
            };
            out.push_str(&rest[..p]);
            rest = &rest[p..];
            let after = &rest[1..];
            if let Some(escaped) = after.strip_prefix('%') {
                out.push('%');
                rest = escaped;
            } else if let Some(inside) = after.strip_prefix('(') {
                // `%\(([^)]+)\)s`.
                let name = match inside.find(')') {
                    Some(close) if close > 0 && inside[close + 1..].starts_with('s') => {
                        &inside[..close]
                    }
                    _ => {
                        return Err(Raised::new(
                            "configparser.InterpolationSyntaxError",
                            format!("bad interpolation variable reference {}", py_repr(rest)),
                        ))
                    }
                };
                rest = &inside[name.len() + 2..];
                let var = crate::core::unicode::lower(name);
                let Some(found) = self.raw(section, &var) else {
                    return Err(Raised::new(
                        "configparser.InterpolationMissingOptionError",
                        format!(
                            "Bad value substitution: option {} in section {} contains an \
                             interpolation key {} which is not a valid option name. Raw value: {}",
                            py_repr(option),
                            py_repr(section),
                            py_repr(&var),
                            py_repr(&rawval)
                        ),
                    ));
                };
                if found.contains('%') {
                    self.interpolate(option, out, found, section, depth + 1)?;
                } else {
                    out.push_str(found);
                }
            } else {
                return Err(Raised::new(
                    "configparser.InterpolationSyntaxError",
                    format!(
                        "'%' must be followed by '%' or '(', found: {}",
                        py_repr(rest)
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Ini, Raised> {
        Ini::read(text, "f.conf", &[])
    }

    #[test]
    fn sections_options_and_continuations() {
        let ini = read(
            "# comment\n[a]\nKey = one\n  two\n\n  three\n; other\nb: x = y\n[DEFAULT]\nd=%(key)s!\n",
        )
        .unwrap();
        assert_eq!(
            ini.get_raw("a", "key").as_deref(),
            Some("one\ntwo\n\nthree")
        );
        assert_eq!(ini.get("a", "B").unwrap().as_deref(), Some("x = y"));
        assert_eq!(
            ini.get("a", "d").unwrap().as_deref(),
            Some("one\ntwo\n\nthree!")
        );
        assert!(ini.has_option("a", "d"));
        assert!(!ini.has_option("b", "d"));
        assert_eq!(ini.item("b", "d"), Err(Raised::key("b")));
        assert_eq!(ini.item("a", "zz"), Err(Raised::key("zz")));
    }

    #[test]
    fn errors_are_cpythons() {
        let e = read("keys=root\n").unwrap_err();
        assert_eq!(e.class, "RuntimeError");
        assert_eq!(
            e.message,
            "f.conf is invalid: File contains no section headers.\nfile: 'f.conf', line: 1\n'keys=root\\n'"
        );
        let e = read("[a]\nbogus\n").unwrap_err();
        assert_eq!(
            e.message,
            "f.conf is invalid: Source contains parsing errors: 'f.conf'\n\t[line  2]: 'bogus\\n'"
        );
        let e = read("[a]\nk=1\nK=2\n").unwrap_err();
        assert_eq!(e.class, "configparser.DuplicateOptionError");
        assert_eq!(
            e.message,
            "While reading from 'f.conf' [line  3]: option 'k' in section 'a' already exists"
        );
        let e = read("[a]\n[a]\n").unwrap_err();
        assert_eq!(
            e.message,
            "While reading from 'f.conf' [line  2]: section 'a' already exists"
        );
        let ini = read("[a]\nk=%(nope)s\nj=50%\np=100%%\n").unwrap();
        assert_eq!(
            ini.get("a", "k").unwrap_err().message,
            "Bad value substitution: option 'k' in section 'a' contains an interpolation key \
             'nope' which is not a valid option name. Raw value: '%(nope)s'"
        );
        assert_eq!(
            ini.get("a", "j").unwrap_err().message,
            "'%' must be followed by '%' or '(', found: '%'"
        );
        assert_eq!(ini.get("a", "p").unwrap().as_deref(), Some("100%"));
    }

    /// What CPython 3.13's `ConfigParser` holds for each file: every
    /// section's options before interpolation, the defaults first.
    #[test]
    fn reads_as_configparser_reads() {
        type Sections = [(&'static str, &'static [(&'static str, &'static str)])];
        let cases: &[(&str, &Sections)] = &[
            (
                "[a]\nkey = one\n  two\n\n  three\n# a comment\n  four\n\n\nnext = x\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("key", "one\ntwo\n\nthree\nfour"), ("next", "x")]),
                ],
            ),
            (
                "  [a]\n    deep = 1\n      more\n    same = 2\n  less = 3\n      of less\n[b]\nk=v\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("deep", "1\nmore"), ("less", "3\nof less"), ("same", "2")]),
                    ("b", &[("k", "v")]),
                ],
            ),
            (
                "[a]\nurl = http://x/y?a=b\nkey with spaces : v = w\nnote = value # kept ; too\nk ; odd = 1\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("k ; odd", "1"), ("key with spaces", "v = w"), ("note", "value # kept ; too"), ("url", "http://x/y?a=b")]),
                ],
            ),
            (
                "[a]\nUPPER = 1\nStraße = 2\nİstanbul = 3\nΣΣ = 4\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("i̇stanbul", "3"), ("straße", "2"), ("upper", "1"), ("σς", "4")]),
                ],
            ),
            (
                "[a] trailing\nk = 1\n[b]c]\nk = 2\n[ spaced ]\nk = 3\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("k", "1")]),
                    ("b]c", &[("k", "2")]),
                    (" spaced ", &[("k", "3")]),
                ],
            ),
            (
                "[a]\nempty =\nblank =   \ncolon: v=1\nequals= v:1\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("blank", ""), ("colon", "v=1"), ("empty", ""), ("equals", "v:1")]),
                ],
            ),
            (
                "[DEFAULT]\nd = 1\ne = 2\n[a]\nd = mine\n[DEFAULT]\nf = 3\n[b]\nx = %(d)s\n",
                &[
                    ("DEFAULT", &[("d", "1"), ("e", "2"), ("f", "3")]),
                    ("a", &[("d", "mine")]),
                    ("b", &[("x", "%(d)s")]),
                ],
            ),
            (
                "[a]\r\nk = 1\r\n  more\r\n[b]\rj = 2\rlast = 3",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("k", "1\nmore")]),
                    ("b", &[("j", "2"), ("last", "3")]),
                ],
            ),
            (
                "[a]\n\tk = 1\n\t\t[not a header]\n\t\tx = y\n\tj = 2\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("j", "2"), ("k", "1\n[not a header]\nx = y")]),
                ],
            ),
            (
                "[a]\nk = 1\n    # indented comment\n    2\n\n    3\nz = end\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("k", "1\n2\n\n3"), ("z", "end")]),
                ],
            ),
            (
                "[a]\nk = 1\n\n\n\n[b]\n\nj = 2\n\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("k", "1")]),
                    ("b", &[("j", "2")]),
                ],
            ),
            (
                "[a]\nk\u{1f}=\u{1f}v\u{1f}\nj\u{a0}=\u{a0}w\u{a0}\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("j", "w"), ("k", "v")]),
                ],
            ),
            (
                "[a]\n  k = 1\n[b]\n    j = 2\n  i = 3\n",
                &[
                    ("DEFAULT", &[]),
                    ("a", &[("k", "1")]),
                    ("b", &[("i", "3"), ("j", "2")]),
                ],
            ),
        ];
        for (text, want) in cases {
            let ini = read(text).unwrap_or_else(|e| panic!("{text:?}: {e:?}"));
            let mut got: Vec<(String, Vec<(String, String)>)> =
                vec![("DEFAULT".to_string(), ini.defaults.clone())];
            got.extend(ini.sections.iter().cloned());
            for (_, options) in &mut got {
                options.sort();
            }
            let want: Vec<(String, Vec<(String, String)>)> = want
                .iter()
                .map(|(name, options)| {
                    (
                        name.to_string(),
                        options
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    )
                })
                .collect();
            assert_eq!(got, want, "{text:?}");
        }
    }
}
