//! A parsed pattern compiled into the program the matcher runs: CPython's
//! `re/_compiler.py` choices (which literals and sets ignore case, and how,
//! the categories under ASCII or Unicode, `^`/`$` under MULTILINE, `.` under
//! DOTALL) over a backtracking machine whose repeats behave as `sre`'s
//! `MAX_UNTIL` and `MIN_UNTIL` do, the zero-width check included.

use super::parse::*;

/// The machine's operations; each is followed by its operands.
pub mod op {
    pub const MATCH: i32 = 1;
    /// `c`: the character is `c`.
    pub const CHAR: i32 = 2;
    /// `c`: its lowercase (Unicode) is `c`.
    pub const CHAR_I: i32 = 3;
    /// `c`: its ASCII lowercase is `c`.
    pub const CHAR_IA: i32 = 4;
    pub const NOTCHAR: i32 = 5;
    pub const NOTCHAR_I: i32 = 6;
    pub const NOTCHAR_IA: i32 = 7;
    pub const ANY: i32 = 8;
    pub const ANYALL: i32 = 9;
    /// `skip negate mode nranges (lo hi)* ncats cat* nuni (lo hi)*`: a set,
    /// tested on the character (mode 0), its Unicode lowercase (1, the `uni`
    /// ranges also on that lowercase's uppercase), or its ASCII lowercase
    /// (2). `skip` is the operation's length.
    pub const SET: i32 = 10;
    /// `a b`: go on at `a`, backtracking to `b`.
    pub const SPLIT: i32 = 11;
    pub const JMP: i32 = 12;
    /// `k`: mark `k` is the position.
    pub const SAVE: i32 = 13;
    /// `kind`: an assertion at the position.
    pub const AT: i32 = 14;
    pub const GROUPREF: i32 = 15;
    pub const GROUPREF_I: i32 = 16;
    pub const GROUPREF_IA: i32 = 17;
    /// `r`: repeat `r` starts, no iteration done.
    pub const REPEAT: i32 = 18;
    /// `r min max body exit`: after an iteration (or none) of a greedy
    /// repeat.
    pub const MAX_UNTIL: i32 = 19;
    /// `r min max more body exit`: of a lazy one; `more` is its LAZY_MORE.
    pub const MIN_UNTIL: i32 = 20;
    /// `r max body`: a lazy repeat's next iteration, after the rest failed.
    pub const LAZY_MORE: i32 = 21;
    /// An atomic group starts.
    pub const ATOMIC: i32 = 22;
    /// It matched: its choices are dropped.
    pub const ATOMIC_END: i32 = 23;
    /// `negate width end`: a lookahead (width -1) or lookbehind starts;
    /// `end` follows it.
    pub const LOOK: i32 = 24;
    pub const LOOK_END: i32 = 25;
    /// `g no`: on to `no` unless group `g` matched.
    pub const COND: i32 = 26;
    pub const FAIL: i32 = 27;
}

pub mod at {
    pub const BEGINNING: i32 = 0;
    pub const BEGINNING_LINE: i32 = 1;
    pub const BEGINNING_STRING: i32 = 2;
    pub const END: i32 = 3;
    pub const END_LINE: i32 = 4;
    pub const END_STRING: i32 = 5;
    pub const BOUNDARY: i32 = 6;
    pub const NON_BOUNDARY: i32 = 7;
    pub const ASCII_BOUNDARY: i32 = 8;
    pub const ASCII_NON_BOUNDARY: i32 = 9;
}

/// Categories as the set operation reads them; the ASCII ones add 6.
pub fn category_code(c: Category, ascii: bool) -> i32 {
    let base = match c {
        Category::Digit => 0,
        Category::NotDigit => 1,
        Category::Space => 2,
        Category::NotSpace => 3,
        Category::Word => 4,
        Category::NotWord => 5,
    };
    if ascii {
        base + 6
    } else {
        base
    }
}

/// `re._casefix._EXTRA_CASES` (CPython 3.13): the characters equal under
/// IGNORECASE to a lowercase one that lowercase alone does not reach.
const EXTRA_CASES: &[(u32, &[u32])] = &[
    (105, &[305]),
    (115, &[383]),
    (181, &[956]),
    (305, &[105]),
    (383, &[115]),
    (837, &[953, 8126]),
    (912, &[8147]),
    (944, &[8163]),
    (946, &[976]),
    (949, &[1013]),
    (952, &[977]),
    (953, &[837, 8126]),
    (954, &[1008]),
    (956, &[181]),
    (960, &[982]),
    (961, &[1009]),
    (962, &[963]),
    (963, &[962]),
    (966, &[981]),
    (976, &[946]),
    (977, &[952]),
    (981, &[966]),
    (982, &[960]),
    (1008, &[954]),
    (1009, &[961]),
    (1013, &[949]),
    (1074, &[7296]),
    (1076, &[7297]),
    (1086, &[7298]),
    (1089, &[7299]),
    (1090, &[7300, 7301]),
    (1098, &[7302]),
    (1123, &[7303]),
    (7296, &[1074]),
    (7297, &[1076]),
    (7298, &[1086]),
    (7299, &[1089]),
    (7300, &[1090, 7301]),
    (7301, &[1090, 7300]),
    (7302, &[1098]),
    (7303, &[1123]),
    (7304, &[42571]),
    (7777, &[7835]),
    (7835, &[7777]),
    (8126, &[837, 953]),
    (8147, &[912]),
    (8163, &[944]),
    (42571, &[7304]),
    (64261, &[64262]),
    (64262, &[64261]),
];

fn fixes(c: u32) -> &'static [u32] {
    EXTRA_CASES
        .binary_search_by_key(&c, |&(k, _)| k)
        .map(|i| EXTRA_CASES[i].1)
        .unwrap_or(&[])
}

/// `_sre.unicode_tolower`: the first character of the full lowercase.
pub fn lower(c: u32) -> u32 {
    let Some(ch) = char::from_u32(c) else {
        return c;
    };
    crate::core::unicode::lower(&ch.to_string())
        .chars()
        .next()
        .map_or(c, |l| l as u32)
}

fn upper(c: u32) -> u32 {
    let Some(ch) = char::from_u32(c) else {
        return c;
    };
    crate::core::unicode::upper(&ch.to_string())
        .chars()
        .next()
        .map_or(c, |u| u as u32)
}

fn iscased(c: u32) -> bool {
    lower(c) != c || upper(c) != c
}

fn ascii_lower(c: u32) -> u32 {
    if (65..=90).contains(&c) {
        c + 32
    } else {
        c
    }
}

fn ascii_iscased(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_alphabetic()
}

/// What a compiled pattern needs of the subject string.
#[derive(Default, Debug, Clone)]
pub struct Needs {
    /// The Unicode lowercase of each character.
    pub lower: bool,
    /// The uppercase of each lowercase.
    pub upper: bool,
}

pub struct Program {
    pub code: Vec<i32>,
    /// Repeat registers.
    pub repeats: usize,
    pub needs: Needs,
    /// The character every match starts with, when one does (an anchor for
    /// the search).
    pub first: Option<u32>,
}

struct Compiler {
    code: Vec<i32>,
    repeats: usize,
    needs: Needs,
}

impl Compiler {
    fn emit(&mut self, v: i32) {
        self.code.push(v);
    }
    fn here(&self) -> i32 {
        self.code.len() as i32
    }
    fn patch(&mut self, at: usize, v: i32) {
        self.code[at] = v;
    }

    fn literal(&mut self, c: u32, flags: u32, negate: bool) {
        let ignore = flags & FLAG_IGNORECASE != 0;
        let ascii = flags & FLAG_ASCII != 0;
        if !ignore {
            self.emit(if negate { op::NOTCHAR } else { op::CHAR });
            self.emit(c as i32);
        } else if ascii {
            if !ascii_iscased(c) {
                self.emit(if negate { op::NOTCHAR } else { op::CHAR });
                self.emit(c as i32);
            } else {
                self.emit(if negate { op::NOTCHAR_IA } else { op::CHAR_IA });
                self.emit(ascii_lower(c) as i32);
            }
        } else if !iscased(c) {
            self.emit(if negate { op::NOTCHAR } else { op::CHAR });
            self.emit(c as i32);
        } else {
            let lo = lower(c);
            self.needs.lower = true;
            let extra = fixes(lo);
            if extra.is_empty() {
                self.emit(if negate { op::NOTCHAR_I } else { op::CHAR_I });
                self.emit(lo as i32);
            } else {
                let mut members = vec![lo];
                members.extend_from_slice(extra);
                let ranges: Vec<(u32, u32)> = runs(members);
                self.set(negate, 1, &ranges, &[], &[]);
            }
        }
    }

    fn set(
        &mut self,
        negate: bool,
        mode: i32,
        ranges: &[(u32, u32)],
        cats: &[i32],
        uni: &[(u32, u32)],
    ) {
        let start = self.code.len();
        self.emit(op::SET);
        self.emit(0);
        self.emit(negate as i32);
        self.emit(mode);
        self.emit(ranges.len() as i32);
        for &(lo, hi) in ranges {
            self.emit(lo as i32);
            self.emit(hi as i32);
        }
        self.emit(cats.len() as i32);
        for &c in cats {
            self.emit(c);
        }
        self.emit(uni.len() as i32);
        for &(lo, hi) in uni {
            self.emit(lo as i32);
            self.emit(hi as i32);
        }
        let len = (self.code.len() - start) as i32;
        self.patch(start + 1, len);
    }

    /// `_optimize_charset` and the choice of IN, IN_IGNORE, or
    /// IN_UNI_IGNORE.
    fn charset(&mut self, items: &[SetItem], flags: u32) {
        let ignore = flags & FLAG_IGNORECASE != 0;
        let ascii = flags & FLAG_ASCII != 0;
        let mut negate = false;
        let mut members: Vec<u32> = Vec::new();
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut cats = Vec::new();
        let mut uni = Vec::new();
        let mut hascased = false;
        for item in items {
            match *item {
                SetItem::Negate => negate = true,
                SetItem::Category(c) => cats.push(category_code(c, ascii)),
                SetItem::Literal(c) if !ignore => members.push(c),
                SetItem::Range(lo, hi) if !ignore => ranges.push((lo, hi)),
                SetItem::Literal(c) if ascii => {
                    members.push(ascii_lower(c));
                    hascased |= ascii_iscased(ascii_lower(c));
                }
                SetItem::Range(lo, hi) if ascii => {
                    for c in lo..=hi.min(0x10ffff) {
                        members.push(ascii_lower(c));
                    }
                    hascased |= (lo..=hi).any(ascii_iscased);
                }
                SetItem::Literal(c) => {
                    let f = lower(c);
                    members.push(f);
                    members.extend_from_slice(fixes(f));
                    hascased |= iscased(f);
                }
                SetItem::Range(lo, hi) => {
                    for c in lo..=hi.min(0xffff) {
                        let f = lower(c);
                        members.push(f);
                        members.extend_from_slice(fixes(f));
                        hascased |= iscased(c);
                    }
                    if hi > 0xffff {
                        uni.push((lo.max(0x10000), hi));
                        hascased = true;
                    }
                }
            }
        }
        ranges.extend(runs(members));
        ranges.sort();
        let mode = match (ignore, ascii, hascased) {
            (false, _, _) | (true, _, false) => 0,
            (true, true, true) => 2,
            (true, false, true) => {
                self.needs.lower = true;
                if !uni.is_empty() {
                    self.needs.upper = true;
                }
                1
            }
        };
        if mode != 1 {
            // Without the lowercase test, a range beyond the BMP is a range.
            ranges.append(&mut uni);
        }
        self.set(negate, mode, &ranges, &cats, &uni);
    }

    fn sequence(&mut self, nodes: &[Node], flags: u32) {
        for node in nodes {
            self.node(node, flags);
        }
    }

    fn node(&mut self, node: &Node, flags: u32) {
        match node {
            Node::Literal(c) => self.literal(*c, flags, false),
            Node::NotLiteral(c) => self.literal(*c, flags, true),
            Node::Any => self.emit(if flags & FLAG_DOTALL != 0 {
                op::ANYALL
            } else {
                op::ANY
            }),
            Node::In(items) => self.charset(items, flags),
            Node::At(a) => {
                self.emit(op::AT);
                let multiline = flags & FLAG_MULTILINE != 0;
                let ascii = flags & FLAG_ASCII != 0;
                self.emit(match a {
                    At::Beginning if multiline => at::BEGINNING_LINE,
                    At::Beginning => at::BEGINNING,
                    At::BeginningString => at::BEGINNING_STRING,
                    At::End if multiline => at::END_LINE,
                    At::End => at::END,
                    At::EndString => at::END_STRING,
                    At::Boundary if ascii => at::ASCII_BOUNDARY,
                    At::Boundary => at::BOUNDARY,
                    At::NonBoundary if ascii => at::ASCII_NON_BOUNDARY,
                    At::NonBoundary => at::NON_BOUNDARY,
                });
            }
            Node::Branch(items) => {
                let mut ends = Vec::new();
                for (k, item) in items.iter().enumerate() {
                    if k + 1 < items.len() {
                        self.emit(op::SPLIT);
                        let at = self.code.len();
                        self.emit(self.here() + 2);
                        self.emit(0);
                        self.sequence(item, flags);
                        self.emit(op::JMP);
                        ends.push(self.code.len());
                        self.emit(0);
                        let next = self.here();
                        self.patch(at + 1, next);
                    } else {
                        self.sequence(item, flags);
                    }
                }
                let end = self.here();
                for e in ends {
                    self.patch(e, end);
                }
            }
            Node::Subpattern {
                group,
                add,
                del,
                body,
            } => {
                let inner = (flags | add) & !del;
                if let Some(g) = group {
                    self.emit(op::SAVE);
                    self.emit((2 * g) as i32);
                    self.sequence(body, inner);
                    self.emit(op::SAVE);
                    self.emit((2 * g + 1) as i32);
                } else {
                    self.sequence(body, inner);
                }
            }
            Node::Repeat {
                kind,
                min,
                max,
                body,
            } => {
                if *kind == Repeat::Possessive {
                    self.emit(op::ATOMIC);
                    self.repeat(Repeat::Max, *min, *max, body, flags);
                    self.emit(op::ATOMIC_END);
                } else {
                    self.repeat(*kind, *min, *max, body, flags);
                }
            }
            Node::Assert {
                behind,
                negate,
                body,
            } => {
                self.emit(op::LOOK);
                self.emit(*negate as i32);
                // Fixed, the parser checked.
                let width = if *behind { width(body) as i32 } else { -1 };
                self.emit(width);
                let at = self.code.len();
                self.emit(0);
                self.sequence(body, flags);
                self.emit(op::LOOK_END);
                let end = self.here();
                self.patch(at, end);
            }
            Node::Failure => self.emit(op::FAIL),
            Node::Atomic(body) => {
                self.emit(op::ATOMIC);
                self.sequence(body, flags);
                self.emit(op::ATOMIC_END);
            }
            Node::GroupRef(g) => {
                let ignore = flags & FLAG_IGNORECASE != 0;
                let code = match (ignore, flags & FLAG_ASCII != 0) {
                    (false, _) => op::GROUPREF,
                    (true, true) => op::GROUPREF_IA,
                    (true, false) => {
                        self.needs.lower = true;
                        op::GROUPREF_I
                    }
                };
                self.emit(code);
                self.emit(*g as i32);
            }
            Node::GroupRefExists { group, yes, no } => {
                self.emit(op::COND);
                self.emit(*group as i32);
                let at = self.code.len();
                self.emit(0);
                self.sequence(yes, flags);
                match no {
                    Some(no) => {
                        self.emit(op::JMP);
                        let jmp = self.code.len();
                        self.emit(0);
                        let start = self.here();
                        self.patch(at, start);
                        self.sequence(no, flags);
                        let end = self.here();
                        self.patch(jmp, end);
                    }
                    None => {
                        let end = self.here();
                        self.patch(at, end);
                    }
                }
            }
        }
    }

    fn repeat(&mut self, kind: Repeat, min: u32, max: u32, body: &[Node], flags: u32) {
        let r = self.repeats as i32;
        self.repeats += 1;
        let max = if max == MAXREPEAT { -1 } else { max as i32 };
        self.emit(op::REPEAT);
        self.emit(r);
        let until = self.code.len();
        if kind == Repeat::Max {
            // MAX_UNTIL r min max body exit
            self.emit(op::MAX_UNTIL);
            self.emit(r);
            self.emit(min as i32);
            self.emit(max);
            self.emit(0);
            self.emit(0);
            let body_at = self.here();
            self.patch(until + 4, body_at);
        } else {
            // MIN_UNTIL r min max more body exit, LAZY_MORE r max body
            self.emit(op::MIN_UNTIL);
            self.emit(r);
            self.emit(min as i32);
            self.emit(max);
            self.emit(0);
            self.emit(0);
            self.emit(0);
            let more = self.here();
            self.patch(until + 4, more);
            self.emit(op::LAZY_MORE);
            self.emit(r);
            self.emit(max);
            let more_body = self.code.len();
            self.emit(0);
            let body_at = self.here();
            self.patch(until + 5, body_at);
            self.patch(more_body, body_at);
        }
        self.sequence(body, flags);
        self.emit(op::JMP);
        self.emit(until as i32);
        let exit = self.here();
        let exit_slot = if kind == Repeat::Max {
            until + 5
        } else {
            until + 6
        };
        self.patch(exit_slot, exit);
    }
}

/// The width of a fixed-width subpattern.
fn width(nodes: &[Node]) -> u64 {
    nodes.iter().map(node_width).sum()
}

fn node_width(node: &Node) -> u64 {
    match node {
        Node::Literal(_) | Node::NotLiteral(_) | Node::Any | Node::In(_) => 1,
        Node::Branch(items) => items.first().map_or(0, |i| width(i)),
        Node::Subpattern { body, .. } | Node::Atomic(body) => width(body),
        Node::Repeat { min, body, .. } => width(body) * *min as u64,
        Node::GroupRefExists { yes, .. } => width(yes),
        // A group reference in a lookbehind has its group's fixed width,
        // which the parser checked; it cannot be measured here.
        Node::GroupRef(_) => 0,
        Node::At(_) | Node::Assert { .. } | Node::Failure => 0,
    }
}

/// Sorted, merged runs of the given characters.
fn runs(mut members: Vec<u32>) -> Vec<(u32, u32)> {
    members.sort_unstable();
    members.dedup();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for c in members {
        match out.last_mut() {
            Some((_, hi)) if *hi + 1 == c => *hi = c,
            _ => out.push((c, c)),
        }
    }
    out
}

/// Compile a parsed pattern: group 0's marks around it, then MATCH.
pub fn compile(parsed: &Parsed) -> Result<Program, String> {
    if contains_lookbehind_groupref(&parsed.nodes) {
        return Err(
            "a group reference inside a lookbehind is not supported by this compiler".to_string(),
        );
    }
    let mut c = Compiler {
        code: Vec::new(),
        repeats: 0,
        needs: Needs::default(),
    };
    c.sequence(&parsed.nodes, parsed.flags);
    c.emit(op::MATCH);
    let first = match parsed.nodes.first() {
        Some(Node::Literal(ch)) if parsed.flags & FLAG_IGNORECASE == 0 => Some(*ch),
        _ => None,
    };
    Ok(Program {
        code: c.code,
        repeats: c.repeats,
        needs: c.needs,
        first,
    })
}

fn contains_lookbehind_groupref(nodes: &[Node]) -> bool {
    fn refs(nodes: &[Node]) -> bool {
        nodes.iter().any(|n| match n {
            Node::GroupRef(_) => true,
            Node::Branch(items) => items.iter().any(|i| refs(i)),
            Node::Subpattern { body, .. } | Node::Atomic(body) | Node::Repeat { body, .. } => {
                refs(body)
            }
            Node::Assert { body, .. } => refs(body),
            Node::GroupRefExists { yes, no, .. } => {
                refs(yes) || no.as_ref().is_some_and(|n| refs(n))
            }
            _ => false,
        })
    }
    nodes.iter().any(|n| match n {
        Node::Assert {
            behind: true, body, ..
        } => refs(body),
        Node::Branch(items) => items.iter().any(|i| contains_lookbehind_groupref(i)),
        Node::Subpattern { body, .. } | Node::Atomic(body) | Node::Repeat { body, .. } => {
            contains_lookbehind_groupref(body)
        }
        Node::Assert { body, .. } => contains_lookbehind_groupref(body),
        Node::GroupRefExists { yes, no, .. } => {
            contains_lookbehind_groupref(yes)
                || no.as_ref().is_some_and(|n| contains_lookbehind_groupref(n))
        }
        _ => false,
    })
}
