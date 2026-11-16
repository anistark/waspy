# textwrap, CPython 3.13's TextWrapper (Lib/textwrap.py). The regular
# expression that splits text into chunks (wordsep_re, break_on_hyphens) is
# written out as the scan it performs, alternative by alternative.

from typing import Optional

_WS = "\t\n\x0b\x0c\r "


def _is_ws(c: str) -> bool:
    return c in _WS


def _is_word(c: str) -> bool:
    return c.isalnum() or c == "_"


def _is_letter(c: str) -> bool:
    return _is_word(c) and not c.isdecimal()


def _is_punct(c: str) -> bool:
    return _is_word(c) or c in "!\"'&.,?"


def _dashes_then_word(text: str, j: int) -> bool:
    # (?=-{2,}\w) at j
    k = j
    n = len(text)
    while k < n and text[k] == "-":
        k += 1
    if k - j < 2:
        return False
    # backtracking may end the dashes earlier, but the char after any shorter
    # run is a '-', which is not \w
    return k < n and _is_word(text[k])


def _split_hyphens(text: str) -> list[str]:
    chunks: list[str] = []
    n = len(text)
    i = 0
    while i < n:
        c = text[i]
        if _is_ws(c):
            j = i
            while j < n and _is_ws(text[j]):
                j += 1
            chunks.append(text[i:j])
            i = j
            continue
        if c == "-" and i > 0 and _is_punct(text[i - 1]):
            k = i
            while k < n and text[k] == "-":
                k += 1
            if k - i >= 2 and k < n and _is_word(text[k]):
                chunks.append(text[i:k])
                i = k
                continue
        j = i + 1
        end = -1
        while True:
            # nws+? has consumed text[i:j]
            if j < n and text[j] == "-":
                behind = (j >= 2 and _is_letter(text[j - 2]) and _is_letter(text[j - 1])) or (
                    j >= 3 and _is_letter(text[j - 3]) and text[j - 2] == "-" and _is_letter(text[j - 1])
                )
                ahead = j + 1 < n and _is_letter(text[j + 1]) and (
                    (j + 2 < n and _is_letter(text[j + 2]))
                    or (j + 3 < n and text[j + 2] == "-" and _is_letter(text[j + 3]))
                )
                if behind and ahead:
                    end = j + 1
                    break
            if j == n or _is_ws(text[j]):
                end = j
                break
            if _is_punct(text[j - 1]) and _dashes_then_word(text, j):
                end = j
                break
            j += 1
        chunks.append(text[i:end])
        i = end
    return chunks


def _split_simple(text: str) -> list[str]:
    chunks: list[str] = []
    n = len(text)
    i = 0
    while i < n:
        j = i
        if _is_ws(text[i]):
            while j < n and _is_ws(text[j]):
                j += 1
        else:
            while j < n and not _is_ws(text[j]):
                j += 1
        chunks.append(text[i:j])
        i = j
    return chunks


class TextWrapper:
    def __init__(
        self,
        width: int = 70,
        initial_indent: str = "",
        subsequent_indent: str = "",
        expand_tabs: bool = True,
        replace_whitespace: bool = True,
        fix_sentence_endings: bool = False,
        break_long_words: bool = True,
        drop_whitespace: bool = True,
        break_on_hyphens: bool = True,
        tabsize: int = 8,
        max_lines: Optional[int] = None,
        placeholder: str = " [...]",
    ):
        self.width: int = width
        self.initial_indent: str = initial_indent
        self.subsequent_indent: str = subsequent_indent
        self.expand_tabs: bool = expand_tabs
        self.replace_whitespace: bool = replace_whitespace
        self.fix_sentence_endings: bool = fix_sentence_endings
        self.break_long_words: bool = break_long_words
        self.drop_whitespace: bool = drop_whitespace
        self.break_on_hyphens: bool = break_on_hyphens
        self.tabsize: int = tabsize
        self.max_lines: Optional[int] = max_lines
        self.placeholder: str = placeholder
        if fix_sentence_endings:
            raise NotImplementedError("textwrap's fix_sentence_endings is not supported")

    def _munge_whitespace(self, text: str) -> str:
        if self.expand_tabs:
            text = text.expandtabs(self.tabsize)
        if self.replace_whitespace:
            out: list[str] = []
            for c in text:
                if c in _WS:
                    out.append(" ")
                else:
                    out.append(c)
            text = "".join(out)
        return text

    def _split_chunks(self, text: str) -> list[str]:
        text = self._munge_whitespace(text)
        if self.break_on_hyphens:
            return _split_hyphens(text)
        return _split_simple(text)

    def _handle_long_word(self, reversed_chunks: list[str], cur_line: list[str], cur_len: int, width: int) -> None:
        if width < 1:
            space_left = 1
        else:
            space_left = width - cur_len
        if self.break_long_words:
            end = space_left
            chunk = reversed_chunks[-1]
            if self.break_on_hyphens and len(chunk) > space_left:
                hyphen = chunk.rfind("-", 0, space_left)
                if hyphen > 0:
                    before = chunk[:hyphen]
                    if before.strip("-") != "":
                        end = hyphen + 1
            cur_line.append(chunk[:end])
            reversed_chunks[-1] = chunk[end:]
        elif not cur_line:
            cur_line.append(reversed_chunks.pop())

    def _wrap_chunks(self, chunks: list[str]) -> list[str]:
        lines: list[str] = []
        if self.width <= 0:
            raise ValueError("invalid width " + repr(self.width) + " (must be > 0)")
        max_lines = self.max_lines
        if max_lines is not None:
            if max_lines > 1:
                indent = self.subsequent_indent
            else:
                indent = self.initial_indent
            if len(indent) + len(self.placeholder.lstrip()) > self.width:
                raise ValueError("placeholder too large for max width")
        chunks.reverse()
        while chunks:
            cur_line: list[str] = []
            cur_len = 0
            if lines:
                indent = self.subsequent_indent
            else:
                indent = self.initial_indent
            width = self.width - len(indent)
            if self.drop_whitespace and chunks[-1].strip() == "" and len(lines) > 0:
                chunks.pop()
            while chunks:
                size = len(chunks[-1])
                if cur_len + size <= width:
                    cur_line.append(chunks.pop())
                    cur_len += size
                else:
                    break
            if len(chunks) > 0 and len(chunks[-1]) > width:
                self._handle_long_word(chunks, cur_line, cur_len, width)
                cur_len = 0
                for part in cur_line:
                    cur_len += len(part)
            if self.drop_whitespace and len(cur_line) > 0 and cur_line[-1].strip() == "":
                cur_len -= len(cur_line[-1])
                cur_line.pop()
            if cur_line:
                fits = max_lines is None or len(lines) + 1 < max_lines
                if not fits:
                    last = len(chunks) == 0 or (
                        self.drop_whitespace and len(chunks) == 1 and chunks[0].strip() == ""
                    )
                    fits = last and cur_len <= width
                if fits:
                    lines.append(indent + "".join(cur_line))
                else:
                    placed = False
                    while cur_line:
                        if cur_line[-1].strip() != "" and cur_len + len(self.placeholder) <= width:
                            cur_line.append(self.placeholder)
                            lines.append(indent + "".join(cur_line))
                            placed = True
                            break
                        cur_len -= len(cur_line[-1])
                        cur_line.pop()
                    if not placed:
                        done = False
                        if lines:
                            prev_line = lines[-1].rstrip()
                            if len(prev_line) + len(self.placeholder) <= self.width:
                                lines[-1] = prev_line + self.placeholder
                                done = True
                        if not done:
                            lines.append(indent + self.placeholder.lstrip())
                    break
        return lines

    def wrap(self, text: str) -> list[str]:
        return self._wrap_chunks(self._split_chunks(text))

    def fill(self, text: str) -> str:
        return "\n".join(self.wrap(text))


def wrap(
    text: str,
    width: int = 70,
    initial_indent: str = "",
    subsequent_indent: str = "",
    expand_tabs: bool = True,
    replace_whitespace: bool = True,
    fix_sentence_endings: bool = False,
    break_long_words: bool = True,
    drop_whitespace: bool = True,
    break_on_hyphens: bool = True,
    tabsize: int = 8,
    max_lines: Optional[int] = None,
    placeholder: str = " [...]",
) -> list[str]:
    w = TextWrapper(width, initial_indent, subsequent_indent, expand_tabs, replace_whitespace,
                    fix_sentence_endings, break_long_words, drop_whitespace, break_on_hyphens,
                    tabsize, max_lines, placeholder)
    return w.wrap(text)


def fill(
    text: str,
    width: int = 70,
    initial_indent: str = "",
    subsequent_indent: str = "",
    expand_tabs: bool = True,
    replace_whitespace: bool = True,
    fix_sentence_endings: bool = False,
    break_long_words: bool = True,
    drop_whitespace: bool = True,
    break_on_hyphens: bool = True,
    tabsize: int = 8,
    max_lines: Optional[int] = None,
    placeholder: str = " [...]",
) -> str:
    w = TextWrapper(width, initial_indent, subsequent_indent, expand_tabs, replace_whitespace,
                    fix_sentence_endings, break_long_words, drop_whitespace, break_on_hyphens,
                    tabsize, max_lines, placeholder)
    return w.fill(text)


def shorten(text: str, width: int, placeholder: str = " [...]") -> str:
    w = TextWrapper(width, "", "", True, True, False, True, True, True, 8, 1, placeholder)
    return w.fill(" ".join(text.strip().split()))


def dedent(text: str) -> str:
    lines = text.split("\n")
    margin = ""
    found = False
    for line in lines:
        stripped = line.lstrip(" \t")
        if stripped == "":
            continue
        indent = line[: len(line) - len(stripped)]
        if not found:
            margin = indent
            found = True
        elif indent.startswith(margin):
            pass
        elif margin.startswith(indent):
            margin = indent
        else:
            k = 0
            while k < len(margin) and k < len(indent) and margin[k] == indent[k]:
                k += 1
            margin = margin[:k]
    out: list[str] = []
    for line in lines:
        if line.strip(" \t") == "":
            out.append("")
        else:
            out.append(line[len(margin):])
    return "\n".join(out)


def indent(text: str, prefix: str) -> str:
    out: list[str] = []
    for line in text.splitlines(True):
        if line.strip() != "":
            out.append(prefix + line)
        else:
            out.append(line)
    return "".join(out)
