# argparse, CPython 3.13's parsing, usage and help for the common parsers:
# positional and optional arguments, store, store_true, store_false, count,
# and append actions, nargs, choices, required, metavar, and help. The
# parsed values reach a Namespace whose fields, one per add_argument() the
# program makes, are typed from its type= and action= (ir::pystdlib writes
# that class, Namespace, from the program's calls; the add_argument calls
# themselves pass type= by name).

import sys
import textwrap
from typing import Optional


class ArgumentError(Exception):
    pass


class _Action:
    def __init__(self, option_strings: list[str], dest: str, action: str, nargs: str, type_name: str, choices: list[str], required: bool, help: str, metavar: str, default_text: str):
        self.option_strings: list[str] = option_strings
        self.dest: str = dest
        self.action: str = action
        self.nargs: str = nargs
        self.type_name: str = type_name
        self.choices: list[str] = choices
        self.required: bool = required
        self.help: str = help
        self.metavar: str = metavar
        self.default_text: str = default_text

    def takes_value(self) -> bool:
        return self.action == "store" or self.action == "append"

    def name(self) -> str:
        if self.option_strings:
            return "/".join(self.option_strings)
        if self.metavar != "":
            return self.metavar
        return self.dest


class _Found:
    def __init__(self, action: Optional[_Action], option: str, sep: bool, explicit: str, has_explicit: bool):
        self.action: Optional[_Action] = action
        self.option: str = option
        self.sep: bool = sep
        self.explicit: str = explicit
        self.has_explicit: bool = has_explicit


class _Raw:
    def __init__(self):
        self.values: dict[str, list[str]] = {}
        self.counts: dict[str, int] = {}
        self.given: dict[str, int] = {}


def _metavar(action: _Action, default: str) -> str:
    if action.metavar != "":
        return action.metavar
    if action.choices:
        return "{" + ",".join(action.choices) + "}"
    return default


def _format_args(action: _Action, default: str) -> str:
    m = _metavar(action, default)
    n = action.nargs
    if n == "":
        return m
    if n == "?":
        return "[" + m + "]"
    if n == "*":
        return "[" + m + " ...]"
    if n == "+":
        return m + " [" + m + " ...]"
    return " ".join([m] * int(n))


def _whitespace_collapsed(text: str) -> str:
    return " ".join(text.split())


class ArgumentParser:
    def __init__(self, prog: Optional[str] = None, description: Optional[str] = None, epilog: Optional[str] = None, add_help: bool = True):
        name = "python"
        if prog is not None:
            name = prog
        elif len(sys.argv) > 0:
            first: str = sys.argv[0]
            name = first.split("/")[-1]
        self.prog: str = name
        self.description: str = "" if description is None else description
        self.epilog: str = "" if epilog is None else epilog
        self._actions: list[_Action] = []
        if add_help:
            self._actions.append(_Action(["-h", "--help"], "help", "help", "", "str", [], False, "show this help message and exit", "", ""))

    def add_argument(self, names: list[str], action: str = "store", nargs: str = "", type: str = "str", choices: Optional[list[str]] = None, required: bool = False, help: str = "", metavar: str = "", dest: str = "", default_text: str = "") -> None:
        # Called with its names as one list (ir::pystdlib).
        allowed: list[str] = []
        if choices is not None:
            allowed = choices
        option_strings: list[str] = []
        positional = ""
        for n in names:
            if n.startswith("-"):
                option_strings.append(n)
            else:
                positional = n
        target = dest
        if target == "":
            if positional != "":
                target = positional
            else:
                long_names = [o for o in option_strings if o.startswith("--")]
                first = long_names[0] if long_names else option_strings[0]
                target = first.lstrip("-").replace("-", "_")
        kind = action
        if positional != "" and nargs == "" and kind == "store":
            required = True
        if positional != "" and nargs == "+":
            required = True
        self._actions.append(_Action(option_strings, target, kind, nargs, type, allowed, required, help, metavar, default_text))

    # -- usage and help --------------------------------------------------

    def _usage_parts(self, positionals: bool) -> list[str]:
        parts: list[str] = []
        for a in self._actions:
            if positionals != (len(a.option_strings) == 0):
                continue
            if a.option_strings:
                part = a.option_strings[0]
                if a.takes_value():
                    part = part + " " + _format_args(a, a.dest.upper())
                if not a.required:
                    part = "[" + part + "]"
            else:
                part = _format_args(a, a.dest)
            parts.append(part)
        return parts

    def format_usage(self) -> str:
        width = 78
        prefix = "usage: "
        opt_parts = self._usage_parts(False)
        pos_parts = self._usage_parts(True)
        usage = " ".join([self.prog] + opt_parts + pos_parts)
        if len(prefix) + len(usage) > width:
            indent = " " * (len(prefix) + len(self.prog) + 1)
            if opt_parts:
                lines = _get_lines([self.prog] + opt_parts, indent, width, True, len(prefix))
                lines.extend(_get_lines(pos_parts, indent, width, False, 0))
            elif pos_parts:
                lines = _get_lines([self.prog] + pos_parts, indent, width, True, len(prefix))
            else:
                lines = [self.prog]
            usage = "\n".join(lines)
        return prefix + usage + "\n"

    def _invocation(self, a: _Action) -> str:
        if not a.option_strings:
            return _metavar(a, a.dest)
        joined = ", ".join(a.option_strings)
        if not a.takes_value():
            return joined
        return joined + " " + _format_args(a, a.dest.upper())

    def format_help(self) -> str:
        width = 78
        out: list[str] = [self.format_usage(), "\n"]
        if self.description != "":
            out.append(textwrap.fill(_whitespace_collapsed(self.description), width) + "\n\n")
        longest = 0
        for a in self._actions:
            n = len(self._invocation(a)) + 2
            if n > longest:
                longest = n
        help_position = min(longest + 2, 24)
        help_width = max(width - help_position, 11)
        action_width = help_position - 2 - 2
        sections: list[tuple[str, bool]] = [("positional arguments:", True), ("options:", False)]
        first = True
        for title, positional in sections:
            chosen = [a for a in self._actions if (len(a.option_strings) == 0) == positional]
            if not chosen:
                continue
            if not first:
                out.append("\n")
            first = False
            out.append(title + "\n")
            for a in chosen:
                header = self._invocation(a)
                text = a.help.replace("%(default)s", a.default_text).replace("%(prog)s", self.prog)
                if text == "" or text.strip() == "":
                    out.append("  " + header + "\n")
                    continue
                lines = textwrap.wrap(_whitespace_collapsed(text), help_width)
                if len(header) <= action_width:
                    out.append("  " + header + " " * (action_width - len(header)) + "  " + lines[0] + "\n")
                else:
                    out.append("  " + header + "\n")
                    out.append(" " * help_position + lines[0] + "\n")
                for line in lines[1:]:
                    out.append(" " * help_position + line + "\n")
        if self.epilog != "":
            out.append("\n" + textwrap.fill(_whitespace_collapsed(self.epilog), width) + "\n")
        return "".join(out)

    def print_usage(self) -> None:
        sys.stdout.write(self.format_usage())

    def print_help(self) -> None:
        sys.stdout.write(self.format_help())

    def error(self, message: str) -> None:
        sys.stderr.write(self.format_usage())
        sys.stderr.write(self.prog + ": error: " + message + "\n")
        sys.exit(2)

    # -- parsing ---------------------------------------------------------

    def _option(self, text: str) -> Optional[_Action]:
        for a in self._actions:
            if text in a.option_strings:
                return a
        return None

    def _check(self, a: _Action, value: str) -> str:
        if a.type_name == "int":
            ok = True
            body = value.strip()
            if body.startswith("-") or body.startswith("+"):
                body = body[1:]
            if body == "" or not body.replace("_", "").isdigit():
                ok = False
            if not ok:
                self.error("argument " + a.name() + ": invalid int value: " + repr(value))
            value = str(int(value))
        elif a.type_name == "float":
            try:
                value = repr(float(value))
            except ValueError:
                self.error("argument " + a.name() + ": invalid float value: " + repr(value))
        if a.choices and value not in a.choices:
            self.error("argument " + a.name() + ": invalid choice: " + repr(value) + " (choose from " + ", ".join(a.choices) + ")")
        return value

    def _store(self, raw: _Raw, a: _Action, values: list[str]) -> None:
        checked = [self._check(a, v) for v in values]
        raw.given[a.dest] = raw.given.get(a.dest, 0) + 1
        if a.action == "append":
            if a.dest in raw.values:
                raw.values[a.dest].extend(checked)
            else:
                raw.values[a.dest] = checked
        else:
            raw.values[a.dest] = checked

    def _is_negative_number(self, text: str) -> bool:
        # ^-\d+$|^-\d*\.\d+$
        if len(text) < 2 or text[0] != "-":
            return False
        body = text[1:]
        if body.isdigit() and body.isascii():
            return True
        dot = body.find(".")
        if dot < 0:
            return False
        left = body[:dot]
        right = body[dot + 1:]
        return (left == "" or (left.isdigit() and left.isascii())) and right != "" and right.isdigit() and right.isascii()

    def _option_tuples(self, arg: str) -> list[_Found]:
        # _get_option_tuples: the options an abbreviation could mean.
        found: list[_Found] = []
        if arg.startswith("--"):
            prefix, sep, explicit = arg.partition("=")
            for a in self._actions:
                for o in a.option_strings:
                    if o.startswith(prefix):
                        found.append(_Found(a, o, sep != "", explicit if sep != "" else "", sep != ""))
        else:
            short = arg[:2]
            short_explicit = arg[2:]
            prefix, sep, explicit = arg.partition("=")
            for a in self._actions:
                for o in a.option_strings:
                    if o == short:
                        found.append(_Found(a, o, False, short_explicit, True))
                    elif o.startswith(prefix):
                        found.append(_Found(a, o, sep != "", explicit if sep != "" else "", sep != ""))
        return found

    def _parse_optional(self, arg: str) -> Optional[list[_Found]]:
        if not arg or arg[0] != "-":
            return None
        exact = self._option(arg)
        if exact is not None:
            return [_Found(exact, arg, False, "", False)]
        if len(arg) == 1:
            return None
        option_string, sep, explicit = arg.partition("=")
        if sep != "":
            named = self._option(option_string)
            if named is not None:
                return [_Found(named, option_string, True, explicit, True)]
        tuples = self._option_tuples(arg)
        if len(tuples) >= 1:
            return tuples
        if self._is_negative_number(arg):
            return None
        if " " in arg:
            return None
        return [_Found(None, arg, False, "", False)]

    def _match_argument(self, a: _Action, pattern: str) -> int:
        # The count of 'A's an optional's nargs takes at the start of pattern.
        k = 0
        while k < len(pattern) and pattern[k] == "A":
            k += 1
        n = a.nargs
        if not a.takes_value():
            return 0
        if n == "":
            if k >= 1:
                return 1
            raise ArgumentError("argument " + a.name() + ": expected one argument")
        if n == "?":
            return 1 if k >= 1 else 0
        if n == "*":
            return k
        if n == "+":
            if k >= 1:
                return k
            raise ArgumentError("argument " + a.name() + ": expected at least one argument")
        count = int(n)
        if k >= count:
            return count
        raise ArgumentError("argument " + a.name() + ": expected " + str(count) + " argument" + ("" if count == 1 else "s"))

    def _match_partial(self, actions: list[_Action], pattern: str) -> list[int]:
        # _match_arguments_partial: as many positionals as the 'A's (and a
        # '-' for '--') at the start of pattern satisfy, each taking as many
        # as it can while the later ones still get their least.
        lead = 0
        while lead < len(pattern) and pattern[lead] != "O":
            lead += 1
        i = len(actions)
        while i > 0:
            mins: list[int] = []
            maxs: list[int] = []
            for a in actions[:i]:
                n = a.nargs
                if n == "":
                    mins.append(1)
                    maxs.append(1)
                elif n == "?":
                    mins.append(0)
                    maxs.append(1)
                elif n == "*":
                    mins.append(0)
                    maxs.append(lead)
                elif n == "+":
                    mins.append(1)
                    maxs.append(lead)
                else:
                    mins.append(int(n))
                    maxs.append(int(n))
            dashes = pattern[:lead].count("-")
            available = lead - dashes
            if sum(mins) <= available:
                result: list[int] = []
                used = 0
                pos = 0
                for j in range(i):
                    later = 0
                    for m in mins[j + 1:]:
                        later += m
                    take = min(maxs[j], available - used - later)
                    # the span holds `take` 'A's and any '-' among them
                    span = 0
                    got = 0
                    while got < take:
                        if pattern[pos + span] == "A":
                            got += 1
                        span += 1
                    while pos + span < lead and pattern[pos + span] == "-" and (j == i - 1 or take > 0):
                        span += 1
                        break
                    result.append(span)
                    used += take
                    pos += span
                end = pos
                if end < len(pattern) and pattern[end] == "O":
                    while result and result[-1] == 0:
                        result.pop()
                return result
            i -= 1
        return []

    def _parse(self, args: list[str]) -> _Raw:
        raw = _Raw()
        pattern_parts: list[str] = []
        found: dict[int, list[_Found]] = {}
        after = False
        for i in range(len(args)):
            arg = args[i]
            if after:
                pattern_parts.append("A")
                continue
            if arg == "--":
                pattern_parts.append("-")
                after = True
                continue
            tuples = self._parse_optional(arg)
            if tuples is None:
                pattern_parts.append("A")
            else:
                found[i] = tuples
                pattern_parts.append("O")
        pattern = "".join(pattern_parts)
        positionals = [a for a in self._actions if not a.option_strings]
        extras: list[str] = []
        try:
            start = 0
            last_option = -1
            for k in found:
                if k > last_option:
                    last_option = k
            while start <= last_option:
                next_option = start
                while next_option <= last_option and next_option not in found:
                    next_option += 1
                if start != next_option:
                    end = self._consume_positionals(raw, positionals, args, pattern, start)
                    if end > start:
                        start = end
                        continue
                    start = end
                if start not in found:
                    extras.extend(args[start:next_option])
                    start = next_option
                start = self._consume_optional(raw, found, args, pattern, start, extras)
            stop = self._consume_positionals(raw, positionals, args, pattern, start)
            extras.extend(args[stop:])
        except ArgumentError as err:
            self.error(str(err))
        missing: list[str] = []
        for a in self._actions:
            if a.required and a.dest not in raw.given:
                if a.option_strings:
                    missing.append("/".join(a.option_strings))
                else:
                    missing.append(_metavar(a, a.dest))
        if missing:
            self.error("the following arguments are required: " + ", ".join(missing))
        if extras:
            self.error("unrecognized arguments: " + " ".join(extras))
        return raw

    def _consume_positionals(self, raw: _Raw, positionals: list[_Action], args: list[str], pattern: str, start: int) -> int:
        counts = self._match_partial(positionals, pattern[start:])
        for j in range(len(counts)):
            a = positionals[j]
            taken = args[start:start + counts[j]]
            if "--" in taken and "-" in pattern[start:start + counts[j]]:
                taken.remove("--")
            start += counts[j]
            self._take(raw, a, taken)
        del positionals[:len(counts)]
        return start

    def _consume_optional(self, raw: _Raw, found: dict[int, list[_Found]], args: list[str], pattern: str, start: int, extras: list[str]) -> int:
        tuples = found[start]
        if len(tuples) > 1:
            names = [f.option for f in tuples]
            self.error("ambiguous option: " + args[start].partition("=")[0] + " could match " + ", ".join(names))
        f = tuples[0]
        a = f.action
        option = f.option
        explicit = f.explicit
        has_explicit = f.has_explicit
        stop = start + 1
        while True:
            if a is None:
                extras.append(args[start])
                return start + 1
            act: _Action = a
            if has_explicit:
                count = 1 if act.takes_value() else 0
                if count == 0 and option[1] != "-" and explicit != "":
                    if f.sep or explicit[0] == "-":
                        raise ArgumentError("argument " + act.name() + ": ignored explicit argument " + repr(explicit))
                    self._take(raw, act, [])
                    option = "-" + explicit[0]
                    nxt = self._option(option)
                    if nxt is not None:
                        a = nxt
                        explicit = explicit[1:]
                        f = _Found(nxt, option, False, explicit, explicit != "")
                        if explicit == "":
                            has_explicit = False
                        elif explicit[0] == "=":
                            explicit = explicit[1:]
                            f = _Found(nxt, option, True, explicit, True)
                        continue
                    extras.append("-" + explicit)
                    stop = start + 1
                    break
                elif count == 1:
                    self._take(raw, act, [explicit])
                    stop = start + 1
                    break
                else:
                    raise ArgumentError("argument " + act.name() + ": ignored explicit argument " + repr(explicit))
            else:
                first = start + 1
                count = self._match_argument(act, pattern[first:])
                self._take(raw, act, args[first:first + count])
                stop = first + count
                break
        return stop

    def _take(self, raw: _Raw, a: _Action, values: list[str]) -> None:
        if a.action == "help":
            self.print_help()
            sys.exit(0)
        if not a.takes_value():
            raw.counts[a.dest] = raw.counts.get(a.dest, 0) + 1
            raw.given[a.dest] = raw.given.get(a.dest, 0) + 1
            return
        if a.nargs == "?" and not values and not a.option_strings:
            return
        self._store(raw, a, values)

    def parse_args(self, args: Optional[list[str]] = None) -> "Namespace":
        argv: list[str] = sys.argv[1:]
        if args is not None:
            argv = args
        return Namespace(self._parse(argv))


def _get_lines(parts: list[str], indent: str, width: int, has_prefix: bool, prefix_len: int) -> list[str]:
    lines: list[str] = []
    line: list[str] = []
    indent_length = len(indent)
    line_len = prefix_len - 1 if has_prefix else indent_length - 1
    for part in parts:
        if line_len + 1 + len(part) > width and line:
            lines.append(indent + " ".join(line))
            line = []
            line_len = indent_length - 1
        line.append(part)
        line_len += len(part) + 1
    if line:
        lines.append(indent + " ".join(line))
    if has_prefix:
        lines[0] = lines[0][len(indent):]
    return lines
