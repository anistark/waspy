//! Extraction of Python comments from source text.
//!
//! The parser discards comments, so they are recovered here by scanning the
//! raw source. Codegen emits them as the `python.comments` custom section.

/// A single `#` comment recovered from a Python source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceComment {
    /// Source file the comment came from. `<module>` for a source string
    /// compiled without a filename.
    pub file: String,
    /// 1-based line number of the comment.
    pub line: u32,
    /// Comment text, with the leading `#` and surrounding whitespace removed.
    pub text: String,
}

/// Scan `source` and return its comments in source order.
///
/// String literals are tracked so a `#` inside one (a docstring included) is
/// not mistaken for a comment. Escaped quotes are honored, which also covers
/// raw strings: `r"\""` does not terminate at the escaped quote in Python.
pub fn extract_comments(file: &str, source: &str) -> Vec<SourceComment> {
    let bytes = source.as_bytes();
    let mut comments = Vec::new();
    let mut line = 1u32;
    // Quote byte and whether the open literal is triple-quoted.
    let mut in_string: Option<(u8, bool)> = None;
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];

        match in_string {
            Some((quote, triple)) => {
                if c == b'\\' {
                    // A backslash escapes the next byte, newline included.
                    if bytes.get(i + 1) == Some(&b'\n') {
                        line += 1;
                    }
                    i += 2;
                    continue;
                }
                if c == b'\n' {
                    line += 1;
                    // Only a triple-quoted literal survives a newline.
                    if !triple {
                        in_string = None;
                    }
                    i += 1;
                    continue;
                }
                if c == quote {
                    if !triple {
                        in_string = None;
                        i += 1;
                    } else if bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote) {
                        in_string = None;
                        i += 3;
                    } else {
                        i += 1;
                    }
                    continue;
                }
                i += 1;
            }
            None => match c {
                b'\n' => {
                    line += 1;
                    i += 1;
                }
                b'#' => {
                    let start = i + 1;
                    let end = bytes[start..]
                        .iter()
                        .position(|b| *b == b'\n')
                        .map(|offset| start + offset)
                        .unwrap_or(bytes.len());
                    let text = source[start..end].trim();
                    if !text.is_empty() {
                        comments.push(SourceComment {
                            file: file.to_string(),
                            line,
                            text: text.to_string(),
                        });
                    }
                    i = end;
                }
                b'"' | b'\'' => {
                    let triple = bytes.get(i + 1) == Some(&c) && bytes.get(i + 2) == Some(&c);
                    in_string = Some((c, triple));
                    i += if triple { 3 } else { 1 };
                }
                _ => i += 1,
            },
        }
    }

    comments
}

/// Encode comments as the payload of the `python.comments` custom section:
/// UTF-8 text, one `file:line:text` entry per line.
///
/// A comment can never contain a newline, so the framing is unambiguous, and
/// text stays readable in any tool that dumps custom sections.
pub fn encode_comment_section(comments: &[SourceComment]) -> String {
    let mut payload = String::new();
    for comment in comments {
        payload.push_str(&comment.file);
        payload.push(':');
        payload.push_str(&comment.line.to_string());
        payload.push(':');
        payload.push_str(&comment.text);
        payload.push('\n');
    }
    payload
}

/// Read the comments back out of a compiled WebAssembly binary.
///
/// Returns an empty vector when the module carries no `python.comments`
/// section. Malformed input yields whatever was decodable rather than an
/// error: this is a debugging aid, not a validator.
///
/// ```
/// use waspy::compile_python_to_wasm;
/// use waspy::core::comments::comments_from_wasm;
///
/// let wasm = compile_python_to_wasm("# adds two numbers\ndef add(a: int, b: int) -> int:\n    return a + b\n")?;
/// let comments = comments_from_wasm(&wasm);
/// assert_eq!(comments[0].text, "adds two numbers");
/// # anyhow::Ok(())
/// ```
pub fn comments_from_wasm(wasm: &[u8]) -> Vec<SourceComment> {
    let Some(payload) = comment_section_payload(wasm) else {
        return Vec::new();
    };

    payload
        .lines()
        .filter_map(|entry| {
            let (file, rest) = entry.split_once(':')?;
            let (line, text) = rest.split_once(':')?;
            Some(SourceComment {
                file: file.to_string(),
                line: line.parse().ok()?,
                text: text.to_string(),
            })
        })
        .collect()
}

/// Locate the `python.comments` custom section and return its payload as text.
fn comment_section_payload(wasm: &[u8]) -> Option<&str> {
    // Past the 8-byte magic and version header, each section is a one-byte id,
    // a LEB128 size, and that many bytes; a custom section (id 0) opens with
    // its own name.
    if wasm.len() < 8 {
        return None;
    }
    let mut cursor = 8usize;

    while cursor < wasm.len() {
        let id = wasm[cursor];
        cursor += 1;
        let size = read_leb(wasm, &mut cursor)?;
        let end = cursor.checked_add(size)?;
        let contents = wasm.get(cursor..end)?;
        cursor = end;

        if id != 0 {
            continue;
        }
        let mut inner = 0usize;
        let name_len = read_leb(contents, &mut inner)?;
        let name = contents.get(inner..inner + name_len)?;
        if name == crate::compiler::COMMENTS_SECTION_NAME.as_bytes() {
            return std::str::from_utf8(contents.get(inner + name_len..)?).ok();
        }
    }

    None
}

/// Read an unsigned LEB128 integer, advancing `cursor` past it.
fn read_leb(bytes: &[u8], cursor: &mut usize) -> Option<usize> {
    let mut value = 0usize;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        value |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= usize::BITS as usize {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<String> {
        extract_comments("<module>", source)
            .into_iter()
            .map(|c| format!("{}:{}", c.line, c.text))
            .collect()
    }

    #[test]
    fn collects_full_line_and_trailing_comments() {
        let source = "# header\ndef f() -> int:\n    return 1  # answer\n";
        assert_eq!(texts(source), vec!["1:header", "3:answer"]);
    }

    #[test]
    fn ignores_hashes_inside_strings() {
        let source = "s = \"not # a comment\"\nt = 'also # not'  # but this is\n";
        assert_eq!(texts(source), vec!["2:but this is"]);
    }

    #[test]
    fn ignores_hashes_inside_docstrings_and_keeps_line_numbers() {
        let source = "def f() -> int:\n    \"\"\"Doc.\n\n    # not a comment\n    \"\"\"\n    return 1  # real\n";
        assert_eq!(texts(source), vec!["6:real"]);
    }

    #[test]
    fn handles_escaped_quotes() {
        let source = "s = \"a \\\" # still string\"  # comment\n";
        assert_eq!(texts(source), vec!["1:comment"]);
    }

    #[test]
    fn skips_empty_comments() {
        assert!(texts("#\n#   \nx = 1\n").is_empty());
    }

    #[test]
    fn encodes_one_line_per_comment() {
        let comments = extract_comments("main.py", "# a\nx = 1  # b\n");
        assert_eq!(
            encode_comment_section(&comments),
            "main.py:1:a\nmain.py:2:b\n"
        );
    }

    #[test]
    fn round_trips_through_a_compiled_module() {
        let source =
            "# what it does\ndef add(a: int, b: int) -> int:\n    return a + b  # the sum\n";

        // A custom section is worth little if the optimizer drops it.
        for optimize in [false, true] {
            let options = crate::CompilerOptions {
                optimize,
                ..Default::default()
            };
            let wasm = crate::compile_python_to_wasm_with_options(source, &options).unwrap();
            let comments = comments_from_wasm(&wasm);
            assert_eq!(
                comments,
                vec![
                    SourceComment {
                        file: "<module>".to_string(),
                        line: 1,
                        text: "what it does".to_string(),
                    },
                    SourceComment {
                        file: "<module>".to_string(),
                        line: 3,
                        text: "the sum".to_string(),
                    },
                ],
                "comments lost with optimize={optimize}"
            );
        }
    }

    #[test]
    fn a_module_without_comments_carries_no_section() {
        let wasm = crate::compile_python_to_wasm("def f() -> int:\n    return 1\n").unwrap();
        assert!(comments_from_wasm(&wasm).is_empty());
        assert!(comment_section_payload(&wasm).is_none());
    }

    #[test]
    fn merged_files_keep_their_own_filenames() {
        let sources = [
            ("a.py", "# from a\ndef f() -> int:\n    return 1\n"),
            ("b.py", "# from b\ndef g() -> int:\n    return 2\n"),
        ];
        let wasm = crate::compile_multiple_python_files(&sources, true).unwrap();
        let files: Vec<_> = comments_from_wasm(&wasm)
            .into_iter()
            .map(|c| format!("{}:{}", c.file, c.text))
            .collect();
        assert_eq!(files, vec!["a.py:from a", "b.py:from b"]);
    }
}
