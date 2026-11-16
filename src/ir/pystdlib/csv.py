# csv, the reader's state machine and the writer of CPython 3.13's
# Modules/_csv.c, for the excel dialect and its common options.

from typing import Optional

QUOTE_MINIMAL = 0
QUOTE_ALL = 1
QUOTE_NONNUMERIC = 2
QUOTE_NONE = 3

_START_RECORD = 0
_START_FIELD = 1
_ESCAPED_CHAR = 2
_IN_FIELD = 3
_IN_QUOTED_FIELD = 4
_ESCAPE_IN_QUOTED_FIELD = 5
_QUOTE_IN_QUOTED_FIELD = 6
_EAT_CRNL = 7


class Error(Exception):
    pass


class _Reader:
    def __init__(self, lines: list[str], delimiter: str, quotechar: str, skipinitialspace: bool, strict: bool, escapechar: str, quoting: int):
        if quoting == QUOTE_NONNUMERIC:
            raise NotImplementedError("csv.reader with quoting=QUOTE_NONNUMERIC is not supported")
        # The source's lines, read as the reader is made (its readlines()).
        self._lines: list[str] = lines
        self._next_line: int = 0
        self._delimiter: str = delimiter
        self._quotechar: str = quotechar
        self._skipinitialspace: bool = skipinitialspace
        self._strict: bool = strict
        self._escapechar: str = escapechar
        self._quoting: int = quoting
        self._state: int = _START_RECORD
        self._fields: list[str] = []
        self._field: list[str] = []
        self.line_num: int = 0

    def _save_field(self) -> None:
        self._fields.append("".join(self._field))
        self._field = []

    def _end_line(self) -> None:
        # The character after a line: EOL in _csv.c.
        state = self._state
        if state == _START_RECORD:
            return
        if state == _START_FIELD or state == _IN_FIELD or state == _QUOTE_IN_QUOTED_FIELD:
            self._save_field()
            self._state = _START_RECORD
        elif state == _ESCAPED_CHAR:
            self._field.append("\n")
            self._state = _IN_FIELD
        elif state == _ESCAPE_IN_QUOTED_FIELD:
            self._field.append("\n")
            self._state = _IN_QUOTED_FIELD
        elif state == _EAT_CRNL:
            self._state = _START_RECORD

    def _process(self, c: str) -> None:
        state = self._state
        if state == _START_RECORD:
            if c == "\n" or c == "\r":
                self._state = _EAT_CRNL
                return
            self._state = _START_FIELD
            state = _START_FIELD
        if state == _START_FIELD:
            if c == "\n" or c == "\r":
                self._save_field()
                self._state = _EAT_CRNL
            elif c == self._quotechar and self._quoting != QUOTE_NONE:
                self._state = _IN_QUOTED_FIELD
            elif c == self._escapechar:
                self._state = _ESCAPED_CHAR
            elif c == " " and self._skipinitialspace:
                pass
            elif c == self._delimiter:
                self._save_field()
            else:
                self._field.append(c)
                self._state = _IN_FIELD
        elif state == _ESCAPED_CHAR:
            self._field.append(c)
            self._state = _IN_FIELD
        elif state == _IN_FIELD:
            if c == "\n" or c == "\r":
                self._save_field()
                self._state = _EAT_CRNL
            elif c == self._escapechar:
                self._state = _ESCAPED_CHAR
            elif c == self._delimiter:
                self._save_field()
                self._state = _START_FIELD
            else:
                self._field.append(c)
        elif state == _IN_QUOTED_FIELD:
            if c == self._escapechar:
                self._state = _ESCAPE_IN_QUOTED_FIELD
            elif c == self._quotechar and self._quoting != QUOTE_NONE:
                self._state = _QUOTE_IN_QUOTED_FIELD
            else:
                self._field.append(c)
        elif state == _ESCAPE_IN_QUOTED_FIELD:
            self._field.append(c)
            self._state = _IN_QUOTED_FIELD
        elif state == _QUOTE_IN_QUOTED_FIELD:
            if self._quoting != QUOTE_NONE and c == self._quotechar:
                self._field.append(c)
                self._state = _IN_QUOTED_FIELD
            elif c == self._delimiter:
                self._save_field()
                self._state = _START_FIELD
            elif c == "\n" or c == "\r":
                self._save_field()
                self._state = _EAT_CRNL
            elif not self._strict:
                self._field.append(c)
                self._state = _IN_FIELD
            else:
                raise Error("'" + self._delimiter + "' expected after '" + self._quotechar + "'")
        elif state == _EAT_CRNL:
            if c == "\n" or c == "\r":
                pass
            else:
                raise Error("new-line character seen in unquoted field - do you need to open the file with newline=''?")

    def __iter__(self) -> "_Reader":
        return self

    def __next__(self) -> list[str]:
        row = self._read_row()
        if row is None:
            raise StopIteration
        return row

    def _read_row(self) -> Optional[list[str]]:
        # The next record, or None at the end of the data.
        self._fields = []
        self._field = []
        self._state = _START_RECORD
        while True:
            line = ""
            got = False
            if self._next_line < len(self._lines):
                line = self._lines[self._next_line]
                self._next_line += 1
                got = True
            if not got:
                if len(self._field) != 0 or self._state == _IN_QUOTED_FIELD:
                    if self._strict:
                        raise Error("unexpected end of data")
                    self._save_field()
                    break
                return None
            self.line_num += 1
            for c in line:
                if c == "\0":
                    raise Error("line contains NUL")
                self._process(c)
            self._end_line()
            if self._state == _START_RECORD:
                break
        fields = self._fields
        self._fields = []
        return fields


def reader(csvfile: list[str], delimiter: str = ",", quotechar: str = "\"", skipinitialspace: bool = False, strict: bool = False, escapechar: str = "", quoting: int = QUOTE_MINIMAL) -> _Reader:
    # Called with its source's readlines() (ir::pystdlib).
    return _Reader(csvfile, delimiter, quotechar, skipinitialspace, strict, escapechar, quoting)


class _Format:
    def __init__(self, delimiter: str, quotechar: str, lineterminator: str, quoting: int, escapechar: str):
        self._delimiter: str = delimiter
        self._quotechar: str = quotechar
        self._lineterminator: str = lineterminator
        self._quoting: int = quoting
        self._escapechar: str = escapechar

    def _field(self, text: str, quote_empty: bool) -> str:
        needs = self._quoting == QUOTE_ALL
        for c in text:
            if c == self._delimiter or c == self._quotechar or c in self._lineterminator or (self._escapechar != "" and c == self._escapechar):
                if self._quoting == QUOTE_NONE:
                    if self._escapechar == "":
                        raise Error("need to escape, but no escapechar set")
                else:
                    needs = True
        if text == "" and quote_empty:
            if self._quoting == QUOTE_NONE:
                raise Error("single empty field record must be quoted")
            needs = True
        out: list[str] = []
        for c in text:
            if c == self._quotechar and self._quoting != QUOTE_NONE:
                if self._escapechar != "":
                    out.append(self._escapechar)
                else:
                    out.append(self._quotechar)
            elif self._quoting == QUOTE_NONE and (c == self._delimiter or c in self._lineterminator or c == self._escapechar):
                out.append(self._escapechar)
            out.append(c)
        body = "".join(out)
        if needs:
            return self._quotechar + body + self._quotechar
        return body

    def line(self, texts: list[str]) -> str:
        parts: list[str] = []
        single = len(texts) == 1
        for text in texts:
            parts.append(self._field(text, single))
        return self._delimiter.join(parts) + self._lineterminator


class writer:
    def __init__(self, f, delimiter: str = ",", quotechar: str = "\"", lineterminator: str = "\r\n", quoting: int = QUOTE_MINIMAL, escapechar: str = ""):
        self._f = f
        self._format: _Format = _Format(delimiter, quotechar, lineterminator, quoting, escapechar)

    def writerow(self, row):
        texts: list[str] = []
        for value in row:
            texts.append(str(value))
        return self._f.write(self._format.line(texts))

    def writerows(self, rows) -> None:
        for row in rows:
            self.writerow(row)


def DictReader(f: list[str], fieldnames: Optional[list[str]] = None, restkey: Optional[str] = None, restval: str = "", delimiter: str = ",") -> "_DictReader":
    # Called with its source's readlines() (ir::pystdlib).
    return _DictReader(f, fieldnames, restkey, restval, delimiter)


class _DictReader:
    def __init__(self, lines: list[str], fieldnames: Optional[list[str]], restkey: Optional[str], restval: str, delimiter: str):
        self._reader: _Reader = _Reader(lines, delimiter, "\"", False, False, "", QUOTE_MINIMAL)
        self._fieldnames: Optional[list[str]] = fieldnames
        self.restval: str = restval
        if restkey is not None:
            raise NotImplementedError("csv.DictReader's restkey is not supported")

    @property
    def fieldnames(self) -> list[str]:
        names = self._fieldnames
        if names is None:
            first = self._reader._read_row()
            got: list[str] = []
            if first is not None:
                got = first
            self._fieldnames = got
            return got
        return names

    def __iter__(self) -> "_DictReader":
        return self

    def __next__(self) -> dict[str, str]:
        names: list[str] = self.fieldnames
        found = self._reader._read_row()
        while found is not None and found == []:
            found = self._reader._read_row()
        if found is None:
            raise StopIteration
        row: list[str] = found
        d: dict[str, str] = {}
        i = 0
        for name in names:
            if i < len(row):
                d[name] = row[i]
            else:
                d[name] = self.restval
            i += 1
        if len(row) > len(names):
            raise NotImplementedError("a csv.DictReader row longer than its fieldnames needs restkey, which is not supported")
        return d


class DictWriter:
    def __init__(self, f, fieldnames: list[str], restval: str = "", extrasaction: str = "raise", delimiter: str = ",", lineterminator: str = "\r\n"):
        if extrasaction != "raise" and extrasaction != "ignore":
            raise ValueError("extrasaction (" + extrasaction + ") must be 'raise' or 'ignore'")
        self.fieldnames: list[str] = fieldnames
        self.restval: str = restval
        self.extrasaction: str = extrasaction
        self._f = f
        self._format: _Format = _Format(delimiter, "\"", lineterminator, QUOTE_MINIMAL, "")

    def writeheader(self):
        return self._f.write(self._format.line(self.fieldnames))

    def writerow(self, rowdict):
        if self.extrasaction == "raise":
            wrong: list[str] = []
            for k in rowdict:
                if k not in self.fieldnames:
                    wrong.append(k)
            if wrong:
                raise ValueError("dict contains fields not in fieldnames: " + ", ".join([repr(w) for w in wrong]))
        values: list[str] = []
        for key in self.fieldnames:
            if key in rowdict:
                values.append(str(rowdict[key]))
            else:
                values.append(self.restval)
        return self._f.write(self._format.line(values))

    def writerows(self, rowdicts) -> None:
        for row in rowdicts:
            self.writerow(row)
