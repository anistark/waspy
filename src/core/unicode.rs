//! CPython's character classes and case mappings, over the tables in
//! `unicode_tables` (generated from CPython by
//! `scripts/gen_unicode_tables.py`).
//!
//! The string functions here are ports of CPython's `unicodeobject.c`
//! (`do_upper`, `do_lower` with its final-sigma rule, `do_capitalize`,
//! `do_title`, and the `is*` predicates), so a method folded on a literal
//! answers what the same method answers at run time, and both answer what
//! CPython does. Code generation lays the same tables out in memory for the
//! runtime helpers (`compiler::unicode`).

use crate::core::unicode_tables as t;

/// A character class, one table each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Alpha,
    Decimal,
    Digit,
    Numeric,
    Space,
    Upper,
    Lower,
    Title,
    Cased,
    CaseIgnorable,
}

impl Class {
    pub const ALL: [Class; 10] = [
        Class::Alpha,
        Class::Decimal,
        Class::Digit,
        Class::Numeric,
        Class::Space,
        Class::Upper,
        Class::Lower,
        Class::Title,
        Class::Cased,
        Class::CaseIgnorable,
    ];

    pub fn ranges(self) -> &'static [(u32, u32)] {
        match self {
            Class::Alpha => t::ALPHA,
            Class::Decimal => t::DECIMAL,
            Class::Digit => t::DIGIT,
            Class::Numeric => t::NUMERIC,
            Class::Space => t::SPACE,
            Class::Upper => t::UPPER,
            Class::Lower => t::LOWER,
            Class::Title => t::TITLE,
            Class::Cased => t::CASED,
            Class::CaseIgnorable => t::CASE_IGNORABLE,
        }
    }

    pub fn has(self, c: char) -> bool {
        let cp = c as u32;
        let ranges = self.ranges();
        let i = ranges.partition_point(|&(_, end)| end < cp);
        i < ranges.len() && ranges[i].0 <= cp
    }
}

/// A case mapping, one pair of tables each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaseMap {
    Upper,
    Lower,
    Title,
}

impl CaseMap {
    pub const ALL: [CaseMap; 3] = [CaseMap::Upper, CaseMap::Lower, CaseMap::Title];

    pub fn runs(self) -> &'static [(u32, u32, u32, i32)] {
        match self {
            CaseMap::Upper => t::TO_UPPER_RUNS,
            CaseMap::Lower => t::TO_LOWER_RUNS,
            CaseMap::Title => t::TO_TITLE_RUNS,
        }
    }

    pub fn multi(self) -> &'static [(u32, [u32; 3], u32)] {
        match self {
            CaseMap::Upper => t::TO_UPPER_MULTI,
            CaseMap::Lower => t::TO_LOWER_MULTI,
            CaseMap::Title => t::TO_TITLE_MULTI,
        }
    }

    /// Append the mapping of `c` to `out`.
    pub fn push(self, c: char, out: &mut String) {
        let cp = c as u32;
        let multi = self.multi();
        if let Ok(i) = multi.binary_search_by_key(&cp, |&(from, _, _)| from) {
            let (_, to, n) = multi[i];
            for &m in &to[..n as usize] {
                out.push(char::from_u32(m).expect("a mapping is a character"));
            }
            return;
        }
        let runs = self.runs();
        let i = runs.partition_point(|&(_, end, _, _)| end < cp);
        if let Some(&(start, _, step, delta)) = runs.get(i) {
            if start <= cp && (cp - start) % step == 0 {
                let m = (cp as i64 + delta as i64) as u32;
                out.push(char::from_u32(m).expect("a mapping is a character"));
                return;
            }
        }
        out.push(c);
    }
}

const CAPITAL_SIGMA: char = '\u{3a3}';

/// CPython's `handle_capital_sigma`: the final form when a cased character
/// comes before (past case-ignorable ones) and none comes after.
fn sigma(chars: &[char], i: usize) -> char {
    let before = chars[..i]
        .iter()
        .rev()
        .find(|&&c| !Class::CaseIgnorable.has(c));
    let mut is_final = before.is_some_and(|&c| Class::Cased.has(c));
    if is_final {
        let after = chars[i + 1..]
            .iter()
            .find(|&&c| !Class::CaseIgnorable.has(c));
        is_final = after.map_or(true, |&c| !Class::Cased.has(c));
    }
    if is_final {
        '\u{3c2}'
    } else {
        '\u{3c3}'
    }
}

fn push_lower(chars: &[char], i: usize, out: &mut String) {
    if chars[i] == CAPITAL_SIGMA {
        out.push(sigma(chars, i));
    } else {
        CaseMap::Lower.push(chars[i], out);
    }
}

pub fn upper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        CaseMap::Upper.push(c, &mut out);
    }
    out
}

pub fn lower(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..chars.len() {
        push_lower(&chars, i, &mut out);
    }
    out
}

/// The first character titlecased, the rest lowercased.
pub fn capitalize(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for i in 0..chars.len() {
        if i == 0 {
            CaseMap::Title.push(chars[0], &mut out);
        } else {
            push_lower(&chars, i, &mut out);
        }
    }
    out
}

/// Each character after a cased one lowercased, every other titlecased.
pub fn title(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut previous_is_cased = false;
    for i in 0..chars.len() {
        if previous_is_cased {
            push_lower(&chars, i, &mut out);
        } else {
            CaseMap::Title.push(chars[i], &mut out);
        }
        previous_is_cased = Class::Cased.has(chars[i]);
    }
    out
}

/// A predicate over a string, as CPython evaluates it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Predicate {
    Alpha,
    Alnum,
    Decimal,
    Digit,
    Numeric,
    Space,
    Upper,
    Lower,
}

impl Predicate {
    pub fn test(self, s: &str) -> bool {
        let every = |f: &dyn Fn(char) -> bool| !s.is_empty() && s.chars().all(f);
        match self {
            Predicate::Alpha => every(&|c| Class::Alpha.has(c)),
            Predicate::Alnum => every(&|c| is_alnum(c)),
            Predicate::Decimal => every(&|c| Class::Decimal.has(c)),
            Predicate::Digit => every(&|c| Class::Digit.has(c)),
            Predicate::Numeric => every(&|c| Class::Numeric.has(c)),
            Predicate::Space => every(&|c| Class::Space.has(c)),
            Predicate::Upper => cased_test(s, Class::Upper, Class::Lower),
            Predicate::Lower => cased_test(s, Class::Lower, Class::Upper),
        }
    }
}

/// `isalnum` of one character: alphabetic or numeric of any kind.
pub fn is_alnum(c: char) -> bool {
    Class::Alpha.has(c) || Class::Decimal.has(c) || Class::Digit.has(c) || Class::Numeric.has(c)
}

/// `isupper` (or `islower`): no character of the other case or titlecase,
/// and at least one of this case. A single character is its class alone.
fn cased_test(s: &str, this: Class, other: Class) -> bool {
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return this.has(c);
    }
    let mut cased = false;
    for c in s.chars() {
        if other.has(c) || Class::Title.has(c) {
            return false;
        }
        if this.has(c) {
            cased = true;
        }
    }
    cased
}

/// `str.strip()` and its one-sided forms: whitespace, or the characters of
/// `chars`, removed from the chosen ends.
pub fn strip<'a>(s: &'a str, chars: Option<&str>, left: bool, right: bool) -> &'a str {
    let cut = |c: char| match chars {
        Some(set) => set.contains(c),
        None => Class::Space.has(c),
    };
    let mut out = s;
    if left {
        out = out.trim_start_matches(cut);
    }
    if right {
        out = out.trim_end_matches(cut);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each expected value is CPython 3.13's answer.
    #[test]
    fn case_mappings_match_cpython() {
        assert_eq!(upper("straße ﬁ ŉ"), "STRASSE FI ʼN");
        assert_eq!(lower("ΑΣ ΑΣΑ Σ İ"), "ας ασα σ i\u{307}");
        assert_eq!(
            title("hello-world 3rd ǆemal they're"),
            "Hello-World 3Rd ǅemal They'Re"
        );
        assert_eq!(capitalize("ǆEMAL"), "ǅemal");
        assert_eq!(capitalize("ßa"), "Ssa");
    }

    #[test]
    fn predicates_match_cpython() {
        assert!(Predicate::Alpha.test("é"));
        assert!(!Predicate::Upper.test("123"));
        assert!(Predicate::Upper.test("A1"));
        assert!(!Predicate::Lower.test("aǅ"));
        assert!(Predicate::Digit.test("²"));
        assert!(!Predicate::Decimal.test("²"));
        assert!(Predicate::Numeric.test("½"));
        assert!(Predicate::Space.test("\u{a0}\u{1c}\u{3000}"));
        assert_eq!(strip("\u{1c} hi\u{a0}", None, true, true), "hi");
    }
}
