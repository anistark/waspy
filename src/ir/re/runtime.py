# `re`'s parser and compiler for a pattern that arrives when the program runs.
#
# A pattern written in the program is parsed and compiled by the compiler
# (`parse.rs`, `compile.rs`). One computed at run time needs the same two
# steps inside the compiled module, so they are written again here in the
# Python subset, function for function, and lowered like the program's own
# code (`ir::embedded`). Both give the same program for the same pattern,
# which the tests check pattern by pattern.
#
# The names in capitals below are constants the loader folds into the code.
# A token is an int: a character's code, or ESC plus the code of the
# character a backslash came before, or NONE at the end of the pattern. The
# file also runs under CPython with `PatternError = re.error`, which is how it
# is compared with the compiler's own parser.

F_IGNORECASE = 2
F_LOCALE = 4
F_MULTILINE = 8
F_DOTALL = 16
F_UNICODE = 32
F_VERBOSE = 64
F_DEBUG = 128
F_ASCII = 256
F_TYPE = 292

ESC = 2097152
NONE = -1
MAXREPEAT = 4294967295.0
MAXWIDTH = 9223372036854775808.0
MAXINT = 2147483647
MAXGROUPS = 1073741823.0

K_LITERAL = 1
K_NOT_LITERAL = 2
K_ANY = 3
K_IN = 4
K_AT = 5
K_BRANCH = 6
K_SUBPATTERN = 7
K_REPEAT = 8
K_ASSERT = 9
K_FAILURE = 10
K_ATOMIC = 11
K_GROUPREF = 12
K_GROUPREF_EXISTS = 13
K_SEQ = 14

S_NEGATE = 0
S_LITERAL = 1
S_RANGE = 2
S_CATEGORY = 3

P_BEGINNING = 0
P_BEGINNING_STRING = 1
P_END = 2
P_END_STRING = 3
P_BOUNDARY = 4
P_NON_BOUNDARY = 5

CAT_DIGIT = 0
CAT_NOT_DIGIT = 1
CAT_SPACE = 2
CAT_NOT_SPACE = 3
CAT_WORD = 4
CAT_NOT_WORD = 5

R_MAX = 0
R_MIN = 1
R_POSSESSIVE = 2

OP_MATCH = 1
OP_CHAR = 2
OP_CHAR_I = 3
OP_CHAR_IA = 4
OP_NOTCHAR = 5
OP_NOTCHAR_I = 6
OP_NOTCHAR_IA = 7
OP_ANY = 8
OP_ANYALL = 9
OP_SET = 10
OP_SPLIT = 11
OP_JMP = 12
OP_SAVE = 13
OP_AT = 14
OP_GROUPREF = 15
OP_GROUPREF_I = 16
OP_GROUPREF_IA = 17
OP_REPEAT = 18
OP_MAX_UNTIL = 19
OP_MIN_UNTIL = 20
OP_LAZY_MORE = 21
OP_ATOMIC = 22
OP_ATOMIC_END = 23
OP_LOOK = 24
OP_LOOK_END = 25
OP_COND = 26
OP_FAIL = 27

AT_BEGINNING = 0
AT_BEGINNING_LINE = 1
AT_BEGINNING_STRING = 2
AT_END = 3
AT_END_LINE = 4
AT_END_STRING = 5
AT_BOUNDARY = 6
AT_NON_BOUNDARY = 7
AT_ASCII_BOUNDARY = 8
AT_ASCII_NON_BOUNDARY = 9


# A node of the parsed pattern. `a`, `b`, and `c` are what its kind reads:
# a literal's character; a subpattern's group (or NONE), the flags it turns
# on, and the ones it turns off; a repeat's kind and its least and most
# counts as the program holds them (`lo` and `hi` are the counts as written);
# an assertion's direction, whether it is negated, and a lookbehind's width.
class __re_rt_Node:
    def __init__(self, kind: int, a: int, b: int, c: int) -> None:
        self.kind = kind
        self.a = a
        self.b = b
        self.c = c
        self.lo = 0.0
        self.hi = 0.0
        self.items: list[int] = []
        self.body: list[__re_rt_Node] = []
        self.alts: list[__re_rt_Node] = []


class __re_rt_Source:
    def __init__(self, chars: list[int], istext: bool) -> None:
        self.chars = chars
        self.istext = istext
        self.index = 0
        self.next = NONE


class __re_rt_State:
    def __init__(self, flags: int) -> None:
        self.flags = flags
        self.names: list[str] = []
        self.gids: list[int] = []
        self.width_lo: list[float] = [-1.0]
        self.width_hi: list[float] = [-1.0]
        self.lookbehind = NONE
        self.ref_groups: list[int] = []
        self.ref_pos: list[int] = []
        self.variable_lookbehind = False


# What the pattern's object is made of.
class __re_rt_Compiled:
    def __init__(self) -> None:
        self.code: list[int] = []
        self.flags = 0
        self.groups = 1
        self.repeats = 0
        self.first = NONE
        self.lower = False
        self.upper = False
        self.names: list[str] = []
        self.indices: list[int] = []


# `re.error`'s message: the text, the position, and its line and column when
# the pattern has several lines.
def __re_rt_position(chars: list[int], msg: str, pos: int) -> str:
    out = msg + " at position " + str(pos)
    several = False
    for c in chars:
        if c == 10:
            several = True
    if several:
        line = 1
        last = -1
        i = 0
        while i < pos and i < len(chars):
            if chars[i] == 10:
                line = line + 1
                last = i
            i = i + 1
        col = pos + 1
        if last >= 0:
            col = pos - last
        out = out + " (line " + str(line) + ", column " + str(col) + ")"
    return out


def __re_rt_tell(src: __re_rt_Source) -> int:
    if src.next == NONE:
        return src.index
    if src.next >= ESC:
        return src.index - 2
    return src.index - 1


def __re_rt_error(src: __re_rt_Source, msg: str, offset: int) -> str:
    shown = msg
    if not src.istext:
        # A bytes pattern's message is encoded with backslashreplace.
        digits = "0123456789abcdef"
        shown = ""
        for ch in msg:
            n = ord(ch)
            if n < 128:
                shown = shown + ch
            else:
                shown = shown + "\\x" + digits[(n // 16) % 16] + digits[n % 16]
    pos = __re_rt_tell(src) - offset
    if pos < 0:
        pos = 0
    return __re_rt_position(src.chars, shown, pos)


def __re_rt_advance(src: __re_rt_Source) -> None:
    index = src.index
    if index >= len(src.chars):
        src.next = NONE
        return
    c = src.chars[index]
    if c == 92:
        if index + 1 >= len(src.chars):
            raise PatternError(
                __re_rt_position(src.chars, "bad escape (end of pattern)", len(src.chars) - 1)
            )
        src.next = ESC + src.chars[index + 1]
        src.index = index + 2
        return
    src.next = c
    src.index = index + 1


def __re_rt_get(src: __re_rt_Source) -> int:
    this = src.next
    __re_rt_advance(src)
    return this


def __re_rt_match(src: __re_rt_Source, c: int) -> bool:
    if src.next == c:
        __re_rt_advance(src)
        return True
    return False


def __re_rt_is_digit(t: int) -> bool:
    return t >= 48 and t <= 57


def __re_rt_is_oct(t: int) -> bool:
    return t >= 48 and t <= 55


def __re_rt_is_hex(t: int) -> bool:
    return (t >= 48 and t <= 57) or (t >= 97 and t <= 102) or (t >= 65 and t <= 70)


def __re_rt_is_letter(t: int) -> bool:
    return (t >= 65 and t <= 90) or (t >= 97 and t <= 122)


# A token as the pattern writes it.
def __re_rt_token(t: int) -> str:
    if t >= ESC:
        return "\\" + chr(t - ESC)
    return chr(t)


def __re_rt_token_isalpha(t: int) -> bool:
    if t < 0 or t >= ESC:
        return False
    return chr(t).isalpha()


def __re_rt_getuntil(src: __re_rt_Source, terminator: int, name: str) -> str:
    out = ""
    while True:
        c = src.next
        __re_rt_advance(src)
        if c == NONE:
            if out == "":
                raise PatternError(__re_rt_error(src, "missing " + name, 0))
            raise PatternError(
                __re_rt_error(
                    src, "missing " + chr(terminator) + ", unterminated name", len(out)
                )
            )
        if c == terminator:
            if out == "":
                raise PatternError(__re_rt_error(src, "missing " + name, 1))
            break
        out = out + __re_rt_token(c)
    return out


def __re_rt_checkgroupname(src: __re_rt_Source, name: str, offset: int) -> None:
    if not name.isidentifier() or not (src.istext or name.isascii()):
        raise PatternError(
            __re_rt_error(src, "bad character in group name " + repr(name), len(name) + offset)
        )


def __re_rt_group_id(state: __re_rt_State, name: str) -> int:
    i = 0
    while i < len(state.names):
        if state.names[i] == name:
            return state.gids[i]
        i = i + 1
    return NONE


# Whether a group is closed, so its width is known.
def __re_rt_checkgroup(state: __re_rt_State, gid: int) -> bool:
    return gid < len(state.width_lo) and state.width_lo[gid] >= 0.0


def __re_rt_checklookbehindgroup(src: __re_rt_Source, state: __re_rt_State, gid: int) -> None:
    if state.lookbehind != NONE:
        if not __re_rt_checkgroup(state, gid):
            raise PatternError(__re_rt_error(src, "cannot refer to an open group", 0))
        if gid >= state.lookbehind:
            raise PatternError(
                __re_rt_error(
                    src, "cannot refer to group defined in the same lookbehind subpattern", 0
                )
            )


def __re_rt_opengroup(src: __re_rt_Source, state: __re_rt_State, name: str, named: bool) -> int:
    gid = len(state.width_lo)
    state.width_lo.append(-1.0)
    state.width_hi.append(-1.0)
    if named:
        ogid = __re_rt_group_id(state, name)
        if ogid != NONE:
            raise PatternError(
                __re_rt_error(
                    src,
                    "redefinition of group name "
                    + repr(name)
                    + " as group "
                    + str(gid)
                    + "; was group "
                    + str(ogid),
                    len(name) + 1,
                )
            )
        state.names.append(name)
        state.gids.append(gid)
    return gid


# The category `\c` names, or NONE.
def __re_rt_category(c: int) -> int:
    if c == 100:
        return CAT_DIGIT
    if c == 68:
        return CAT_NOT_DIGIT
    if c == 115:
        return CAT_SPACE
    if c == 83:
        return CAT_NOT_SPACE
    if c == 119:
        return CAT_WORD
    if c == 87:
        return CAT_NOT_WORD
    return NONE


# The character `\c` is, for a, b, f, n, r, t, v, and a backslash, or NONE.
def __re_rt_escape_literal(c: int) -> int:
    if c == 97:
        return 7
    if c == 98:
        return 8
    if c == 102:
        return 12
    if c == 110:
        return 10
    if c == 114:
        return 13
    if c == 116:
        return 9
    if c == 118:
        return 11
    if c == 92:
        return 92
    return NONE


# `\x`, `\u`, `\U`: exactly `n` hex digits, as a code point.
def __re_rt_hex_escape(src: __re_rt_Source, escape: str, n: int) -> int:
    full = escape
    value = 0
    k = 0
    while k < n and __re_rt_is_hex(src.next):
        d = src.next
        full = full + chr(d)
        if d >= 97:
            d = d - 87
        elif d >= 65:
            d = d - 55
        else:
            d = d - 48
        if value < 1114112:
            value = value * 16 + d
        __re_rt_advance(src)
        k = k + 1
    if k != n:
        raise PatternError(__re_rt_error(src, "incomplete escape " + full, len(full)))
    if n == 8 and value > 1114111:
        raise PatternError(__re_rt_error(src, "bad escape " + full, len(full)))
    return value


def __re_rt_named_escape() -> int:
    raise NotImplementedError(
        "\\N{...} in a pattern computed at run time is not supported: the character names"
        " are not in the compiled program. Hint: write the character itself, or \\uXXXX"
    )


# An escape inside a set: an item, as its three numbers.
def __re_rt_class_escape(src: __re_rt_Source, this: int) -> list[int]:
    c = this - ESC
    escape = "\\" + chr(c)
    lit = __re_rt_escape_literal(c)
    if lit != NONE:
        return [S_LITERAL, lit, 0]
    cat = __re_rt_category(c)
    if cat != NONE:
        return [S_CATEGORY, cat, 0]
    if c == 120:
        return [S_LITERAL, __re_rt_hex_escape(src, escape, 2), 0]
    if src.istext:
        if c == 117:
            return [S_LITERAL, __re_rt_hex_escape(src, escape, 4), 0]
        if c == 85:
            return [S_LITERAL, __re_rt_hex_escape(src, escape, 8), 0]
        if c == 78:
            return [S_LITERAL, __re_rt_named_escape(), 0]
    if __re_rt_is_oct(c):
        full = escape
        value = c - 48
        k = 0
        while k < 2 and __re_rt_is_oct(src.next):
            full = full + chr(src.next)
            value = value * 8 + src.next - 48
            __re_rt_advance(src)
            k = k + 1
        if value > 255:
            raise PatternError(
                __re_rt_error(
                    src, "octal escape value " + full + " outside of range 0-0o377", len(full)
                )
            )
        return [S_LITERAL, value, 0]
    if __re_rt_is_digit(c) or __re_rt_is_letter(c):
        raise PatternError(__re_rt_error(src, "bad escape " + escape, 2))
    return [S_LITERAL, c, 0]


# An escape outside a set.
def __re_rt_escape(src: __re_rt_Source, this: int, state: __re_rt_State) -> __re_rt_Node:
    c = this - ESC
    escape = "\\" + chr(c)
    if c == 65:
        return __re_rt_Node(K_AT, P_BEGINNING_STRING, 0, 0)
    if c == 98:
        return __re_rt_Node(K_AT, P_BOUNDARY, 0, 0)
    if c == 66:
        return __re_rt_Node(K_AT, P_NON_BOUNDARY, 0, 0)
    if c == 90:
        return __re_rt_Node(K_AT, P_END_STRING, 0, 0)
    cat = __re_rt_category(c)
    if cat != NONE:
        node = __re_rt_Node(K_IN, 0, 0, 0)
        node.items = [S_CATEGORY, cat, 0]
        return node
    lit = __re_rt_escape_literal(c)
    if lit != NONE:
        return __re_rt_Node(K_LITERAL, lit, 0, 0)
    if c == 120:
        return __re_rt_Node(K_LITERAL, __re_rt_hex_escape(src, escape, 2), 0, 0)
    if src.istext:
        if c == 117:
            return __re_rt_Node(K_LITERAL, __re_rt_hex_escape(src, escape, 4), 0, 0)
        if c == 85:
            return __re_rt_Node(K_LITERAL, __re_rt_hex_escape(src, escape, 8), 0, 0)
        if c == 78:
            return __re_rt_Node(K_LITERAL, __re_rt_named_escape(), 0, 0)
    if c == 48:
        value = 0
        k = 0
        while k < 2 and __re_rt_is_oct(src.next):
            value = value * 8 + src.next - 48
            __re_rt_advance(src)
            k = k + 1
        return __re_rt_Node(K_LITERAL, value, 0, 0)
    if __re_rt_is_digit(c):
        full = escape
        group = c - 48
        if __re_rt_is_digit(src.next):
            second = src.next
            full = full + chr(second)
            __re_rt_advance(src)
            if __re_rt_is_oct(c) and __re_rt_is_oct(second) and __re_rt_is_oct(src.next):
                third = src.next
                full = full + chr(third)
                __re_rt_advance(src)
                value = (c - 48) * 64 + (second - 48) * 8 + third - 48
                if value > 255:
                    raise PatternError(
                        __re_rt_error(
                            src,
                            "octal escape value " + full + " outside of range 0-0o377",
                            len(full),
                        )
                    )
                return __re_rt_Node(K_LITERAL, value, 0, 0)
            group = group * 10 + second - 48
        if group < len(state.width_lo):
            if not __re_rt_checkgroup(state, group):
                raise PatternError(
                    __re_rt_error(src, "cannot refer to an open group", len(full))
                )
            __re_rt_checklookbehindgroup(src, state, group)
            return __re_rt_Node(K_GROUPREF, group, 0, 0)
        raise PatternError(
            __re_rt_error(src, "invalid group reference " + str(group), len(full) - 1)
        )
    if __re_rt_is_letter(c):
        raise PatternError(__re_rt_error(src, "bad escape " + escape, 2))
    return __re_rt_Node(K_LITERAL, c, 0, 0)


# The least and most characters a sequence matches.
def __re_rt_getwidth(nodes: list[__re_rt_Node], state: __re_rt_State) -> list[float]:
    lo = 0.0
    hi = 0.0
    for node in nodes:
        i = 0.0
        j = 0.0
        kind = node.kind
        if kind == K_BRANCH:
            i = MAXWIDTH
            for alt in node.alts:
                w = __re_rt_getwidth(alt.body, state)
                if w[0] < i:
                    i = w[0]
                if w[1] > j:
                    j = w[1]
        elif kind == K_ATOMIC or kind == K_SUBPATTERN:
            w = __re_rt_getwidth(node.body, state)
            i = w[0]
            j = w[1]
        elif kind == K_REPEAT:
            w = __re_rt_getwidth(node.body, state)
            i = w[0] * node.lo
            if node.hi == MAXREPEAT:
                if w[1] > 0.0:
                    j = MAXWIDTH
            else:
                j = w[1] * node.hi
        elif kind == K_LITERAL or kind == K_NOT_LITERAL or kind == K_ANY or kind == K_IN:
            i = 1.0
            j = 1.0
        elif kind == K_GROUPREF:
            if state.width_lo[node.a] >= 0.0:
                i = state.width_lo[node.a]
                j = state.width_hi[node.a]
        elif kind == K_GROUPREF_EXISTS:
            w = __re_rt_getwidth(node.body, state)
            j = w[1]
            if node.b == 1:
                i = w[0]
                other = __re_rt_getwidth(node.alts[0].body, state)
                if other[0] < i:
                    i = other[0]
                if other[1] > j:
                    j = other[1]
        lo = lo + i
        if lo > MAXWIDTH:
            lo = MAXWIDTH
        hi = hi + j
        if hi > MAXWIDTH:
            hi = MAXWIDTH
    return [lo, hi]


def __re_rt_seq_eq(xs: list[__re_rt_Node], ys: list[__re_rt_Node]) -> bool:
    if len(xs) != len(ys):
        return False
    i = 0
    while i < len(xs):
        if not __re_rt_node_eq(xs[i], ys[i]):
            return False
        i = i + 1
    return True


def __re_rt_node_eq(x: __re_rt_Node, y: __re_rt_Node) -> bool:
    if x.kind != y.kind or x.a != y.a or x.b != y.b or x.c != y.c:
        return False
    if x.lo != y.lo or x.hi != y.hi:
        return False
    if x.items != y.items:
        return False
    if not __re_rt_seq_eq(x.body, y.body):
        return False
    return __re_rt_seq_eq(x.alts, y.alts)


# A set's items, each once, in the order they first appear.
def __re_rt_uniq(items: list[int]) -> list[int]:
    out: list[int] = []
    i = 0
    while i < len(items):
        seen = False
        j = 0
        while j < len(out):
            if out[j] == items[i] and out[j + 1] == items[i + 1] and out[j + 2] == items[i + 2]:
                seen = True
                break
            j = j + 3
        if not seen:
            out.append(items[i])
            out.append(items[i + 1])
            out.append(items[i + 2])
        i = i + 3
    return out


def __re_rt_parse_sub(
    src: __re_rt_Source, state: __re_rt_State, verbose: bool, nested: int
) -> list[__re_rt_Node]:
    items: list[__re_rt_Node] = []
    while True:
        first = nested == 0 and len(items) == 0
        seq = __re_rt_Node(K_SEQ, 0, 0, 0)
        seq.body = __re_rt_parse(src, state, verbose, nested + 1, first)
        items.append(seq)
        if not __re_rt_match(src, 124):
            break
        if nested == 0:
            verbose = (state.flags & F_VERBOSE) != 0
    if len(items) == 1:
        return items[0].body
    out: list[__re_rt_Node] = []
    # A prefix every branch shares moves out of the branch.
    while True:
        if len(items[0].body) == 0:
            break
        prefix = items[0].body[0]
        shared = True
        for item in items:
            if len(item.body) == 0 or not __re_rt_node_eq(item.body[0], prefix):
                shared = False
                break
        if not shared:
            break
        for item in items:
            item.body.pop(0)
        out.append(prefix)
    # Branches of one character each are a set.
    members: list[int] = []
    single = True
    for item in items:
        if len(item.body) != 1:
            single = False
            break
        only = item.body[0]
        if only.kind == K_LITERAL:
            members.append(S_LITERAL)
            members.append(only.a)
            members.append(0)
        elif only.kind == K_IN and only.items[0] != S_NEGATE:
            members.extend(only.items)
        else:
            single = False
            break
    if single:
        charset = __re_rt_Node(K_IN, 0, 0, 0)
        charset.items = __re_rt_uniq(members)
        out.append(charset)
        return out
    branch = __re_rt_Node(K_BRANCH, 0, 0, 0)
    branch.alts = items
    out.append(branch)
    return out


def __re_rt_flag(c: int) -> int:
    if c == 105:
        return F_IGNORECASE
    if c == 76:
        return F_LOCALE
    if c == 109:
        return F_MULTILINE
    if c == 115:
        return F_DOTALL
    if c == 120:
        return F_VERBOSE
    if c == 97:
        return F_ASCII
    if c == 117:
        return F_UNICODE
    return 0


# Inline flags after `(?`: nothing for global flags, which are set on the
# state, or the flags a group turns on and off.
def __re_rt_parse_flags(src: __re_rt_Source, state: __re_rt_State, char: int) -> list[int]:
    add = 0
    off = 0
    c = char
    if c != 45:
        while True:
            flag = __re_rt_flag(c)
            if src.istext and c == 76:
                raise PatternError(
                    __re_rt_error(
                        src, "bad inline flags: cannot use 'L' flag with a str pattern", 0
                    )
                )
            if not src.istext and c == 117:
                raise PatternError(
                    __re_rt_error(
                        src, "bad inline flags: cannot use 'u' flag with a bytes pattern", 0
                    )
                )
            add = add | flag
            if (flag & F_TYPE) != 0 and (add & F_TYPE) != flag:
                raise PatternError(
                    __re_rt_error(
                        src, "bad inline flags: flags 'a', 'u' and 'L' are incompatible", 0
                    )
                )
            c = __re_rt_get(src)
            if c == NONE:
                raise PatternError(__re_rt_error(src, "missing -, : or )", 0))
            if c == 41 or c == 45 or c == 58:
                break
            if __re_rt_flag(c) == 0:
                if __re_rt_token_isalpha(c):
                    raise PatternError(__re_rt_error(src, "unknown flag", 1))
                raise PatternError(
                    __re_rt_error(src, "missing -, : or )", len(__re_rt_token(c)))
                )
    if c == 41:
        state.flags = state.flags | add
        return []
    if c == 45:
        c = __re_rt_get(src)
        if c == NONE:
            raise PatternError(__re_rt_error(src, "missing flag", 0))
        if __re_rt_flag(c) == 0:
            if __re_rt_token_isalpha(c):
                raise PatternError(__re_rt_error(src, "unknown flag", 1))
            raise PatternError(__re_rt_error(src, "missing flag", len(__re_rt_token(c))))
        while True:
            flag = __re_rt_flag(c)
            if (flag & F_TYPE) != 0:
                raise PatternError(
                    __re_rt_error(
                        src, "bad inline flags: cannot turn off flags 'a', 'u' and 'L'", 0
                    )
                )
            off = off | flag
            c = __re_rt_get(src)
            if c == NONE:
                raise PatternError(__re_rt_error(src, "missing :", 0))
            if c == 58:
                break
            if __re_rt_flag(c) == 0:
                if __re_rt_token_isalpha(c):
                    raise PatternError(__re_rt_error(src, "unknown flag", 1))
                raise PatternError(__re_rt_error(src, "missing :", len(__re_rt_token(c))))
    if (add & off) != 0:
        raise PatternError(__re_rt_error(src, "bad inline flags: flag turned on and off", 1))
    return [add, off]


def __re_rt_unsupported_warning(what: str) -> str:
    return (
        what
        + " in a pattern computed at run time is not supported: CPython prints a"
        + " FutureWarning for it"
    )


# `[...]`, after the bracket.
def __re_rt_parse_set(src: __re_rt_Source, out: list[__re_rt_Node]) -> None:
    here = __re_rt_tell(src) - 1
    if src.next == 91:
        raise NotImplementedError(__re_rt_unsupported_warning("a possibly nested set ('[[')"))
    negate = __re_rt_match(src, 94)
    items: list[int] = []
    while True:
        this = __re_rt_get(src)
        if this == NONE:
            raise PatternError(
                __re_rt_error(src, "unterminated character set", __re_rt_tell(src) - here)
            )
        if this == 93 and len(items) > 0:
            break
        code1 = [S_LITERAL, this, 0]
        if this >= ESC:
            code1 = __re_rt_class_escape(src, this)
        elif len(items) > 0 and src.next == this:
            if this == 45 or this == 38 or this == 126 or this == 124:
                raise NotImplementedError(
                    __re_rt_unsupported_warning(
                        "a possible set operation ('" + chr(this) + chr(this) + "')"
                    )
                )
        if __re_rt_match(src, 45):
            that = __re_rt_get(src)
            if that == NONE:
                raise PatternError(
                    __re_rt_error(src, "unterminated character set", __re_rt_tell(src) - here)
                )
            if that == 93:
                items.extend(code1)
                items.append(S_LITERAL)
                items.append(45)
                items.append(0)
                break
            code2 = [S_LITERAL, that, 0]
            if that >= ESC:
                code2 = __re_rt_class_escape(src, that)
            elif that == 45:
                raise NotImplementedError(
                    __re_rt_unsupported_warning("a possible set difference ('--')")
                )
            if code1[0] != S_LITERAL or code2[0] != S_LITERAL or code2[1] < code1[1]:
                a = __re_rt_token(this)
                b = __re_rt_token(that)
                raise PatternError(
                    __re_rt_error(
                        src, "bad character range " + a + "-" + b, len(a) + 1 + len(b)
                    )
                )
            items.append(S_RANGE)
            items.append(code1[1])
            items.append(code2[1])
        else:
            items.extend(code1)
    items = __re_rt_uniq(items)
    if len(items) == 3 and items[0] == S_LITERAL:
        if negate:
            out.append(__re_rt_Node(K_NOT_LITERAL, items[1], 0, 0))
        else:
            out.append(__re_rt_Node(K_LITERAL, items[1], 0, 0))
        return
    node = __re_rt_Node(K_IN, 0, 0, 0)
    if negate:
        node.items = [S_NEGATE, 0, 0]
        node.items.extend(items)
    else:
        node.items = items
    out.append(node)


def __re_rt_number(digits: str) -> float:
    value = 0.0
    for ch in digits:
        value = value * 10.0 + float(ord(ch) - 48)
    return value


# A count as the program holds it.
def __re_rt_clamp(x: float) -> int:
    if x >= 2147483647.0:
        return MAXINT
    return int(x)


# `*`, `+`, `?`, or `{`, after it: the item before it repeated.
def __re_rt_parse_repeat(src: __re_rt_Source, out: list[__re_rt_Node], this: int) -> None:
    here = __re_rt_tell(src)
    lo = 0.0
    hi = MAXREPEAT
    if this == 63:
        hi = 1.0
    elif this == 43:
        lo = 1.0
    elif this == 123:
        if src.next == 125:
            out.append(__re_rt_Node(K_LITERAL, 123, 0, 0))
            return
        least = ""
        most = ""
        while __re_rt_is_digit(src.next):
            least = least + chr(__re_rt_get(src))
        if __re_rt_match(src, 44):
            while __re_rt_is_digit(src.next):
                most = most + chr(__re_rt_get(src))
        else:
            most = least
        if not __re_rt_match(src, 125):
            out.append(__re_rt_Node(K_LITERAL, 123, 0, 0))
            src.index = here
            __re_rt_advance(src)
            return
        if least != "":
            lo = __re_rt_number(least)
            if lo >= MAXREPEAT:
                raise OverflowError("the repetition number is too large")
        if most != "":
            hi = __re_rt_number(most)
            if hi >= MAXREPEAT:
                raise OverflowError("the repetition number is too large")
            if hi < lo:
                raise PatternError(
                    __re_rt_error(
                        src, "min repeat greater than max repeat", __re_rt_tell(src) - here
                    )
                )
    if len(out) == 0 or out[len(out) - 1].kind == K_AT:
        raise PatternError(
            __re_rt_error(src, "nothing to repeat", __re_rt_tell(src) - here + 1)
        )
    item = out[len(out) - 1]
    if item.kind == K_REPEAT:
        raise PatternError(__re_rt_error(src, "multiple repeat", __re_rt_tell(src) - here + 1))
    out.pop()
    kind = R_MAX
    if __re_rt_match(src, 63):
        kind = R_MIN
    elif __re_rt_match(src, 43):
        kind = R_POSSESSIVE
    most_held = NONE
    if hi != MAXREPEAT:
        most_held = __re_rt_clamp(hi)
    node = __re_rt_Node(K_REPEAT, kind, __re_rt_clamp(lo), most_held)
    node.lo = lo
    node.hi = hi
    if item.kind == K_SUBPATTERN and item.a == NONE and item.b == 0 and item.c == 0:
        node.body = item.body
    else:
        node.body = [item]
    out.append(node)


# `(?=`, `(?!`, `(?<=`, `(?<!`, after the character that chose it.
def __re_rt_parse_assert(
    src: __re_rt_Source,
    state: __re_rt_State,
    out: list[__re_rt_Node],
    verbose: bool,
    nested: int,
    char: int,
    start: int,
) -> None:
    behind = 0
    kind = char
    opened = False
    if kind == 60:
        c = __re_rt_get(src)
        if c == NONE:
            raise PatternError(__re_rt_error(src, "unexpected end of pattern", 0))
        if c != 61 and c != 33:
            shown = __re_rt_token(c)
            raise PatternError(
                __re_rt_error(src, "unknown extension ?<" + shown, len(shown) + 2)
            )
        kind = c
        behind = 1
        if state.lookbehind == NONE:
            state.lookbehind = len(state.width_lo)
            opened = True
    p = __re_rt_parse_sub(src, state, verbose, nested + 1)
    if opened:
        state.lookbehind = NONE
    if not __re_rt_match(src, 41):
        raise PatternError(
            __re_rt_error(src, "missing ), unterminated subpattern", __re_rt_tell(src) - start)
        )
    width = -1
    if behind == 1:
        w = __re_rt_getwidth(p, state)
        if w[0] != w[1]:
            # CPython's compiler raises this, once the pattern is parsed.
            state.variable_lookbehind = True
        width = __re_rt_clamp(w[0])
    if kind == 61:
        node = __re_rt_Node(K_ASSERT, behind, 0, width)
        node.body = p
        out.append(node)
    elif len(p) > 0:
        node = __re_rt_Node(K_ASSERT, behind, 1, width)
        node.body = p
        out.append(node)
    else:
        out.append(__re_rt_Node(K_FAILURE, 0, 0, 0))


# `(?(group)yes|no)`, after its second parenthesis.
def __re_rt_parse_conditional(
    src: __re_rt_Source,
    state: __re_rt_State,
    out: list[__re_rt_Node],
    verbose: bool,
    nested: int,
    start: int,
) -> None:
    condname = __re_rt_getuntil(src, 41, "group name")
    decimal = True
    for ch in condname:
        if ch < "0" or ch > "9":
            decimal = False
    condgroup = 0
    if decimal:
        value = __re_rt_number(condname)
        if value == 0.0:
            raise PatternError(__re_rt_error(src, "bad group number", len(condname) + 1))
        if value >= MAXGROUPS:
            raise PatternError(
                __re_rt_error(
                    src, "invalid group reference " + condname.lstrip("0"), len(condname) + 1
                )
            )
        condgroup = int(value)
        if condgroup not in state.ref_groups:
            state.ref_groups.append(condgroup)
            state.ref_pos.append(__re_rt_tell(src) - len(condname) - 1)
    else:
        __re_rt_checkgroupname(src, condname, 1)
        condgroup = __re_rt_group_id(state, condname)
        if condgroup == NONE:
            raise PatternError(
                __re_rt_error(
                    src, "unknown group name " + repr(condname), len(condname) + 1
                )
            )
    __re_rt_checklookbehindgroup(src, state, condgroup)
    node = __re_rt_Node(K_GROUPREF_EXISTS, condgroup, 0, 0)
    node.body = __re_rt_parse(src, state, verbose, nested + 1, False)
    if __re_rt_match(src, 124):
        other = __re_rt_Node(K_SEQ, 0, 0, 0)
        other.body = __re_rt_parse(src, state, verbose, nested + 1, False)
        if src.next == 124:
            raise PatternError(
                __re_rt_error(src, "conditional backref with more than two branches", 0)
            )
        node.b = 1
        node.alts = [other]
    if not __re_rt_match(src, 41):
        raise PatternError(
            __re_rt_error(src, "missing ), unterminated subpattern", __re_rt_tell(src) - start)
        )
    out.append(node)


# `(`, after it. Answers whether it set global flags, which the caller's
# verbose mode then follows.
def __re_rt_parse_group(
    src: __re_rt_Source,
    state: __re_rt_State,
    out: list[__re_rt_Node],
    verbose: bool,
    nested: int,
    first: bool,
) -> bool:
    start = __re_rt_tell(src) - 1
    capture = True
    atomic = False
    name = ""
    named = False
    add = 0
    off = 0
    if __re_rt_match(src, 63):
        char = __re_rt_get(src)
        if char == NONE:
            raise PatternError(__re_rt_error(src, "unexpected end of pattern", 0))
        if char == 80:
            if __re_rt_match(src, 60):
                name = __re_rt_getuntil(src, 62, "group name")
                __re_rt_checkgroupname(src, name, 1)
                named = True
            elif __re_rt_match(src, 61):
                name = __re_rt_getuntil(src, 41, "group name")
                __re_rt_checkgroupname(src, name, 1)
                gid = __re_rt_group_id(state, name)
                if gid == NONE:
                    raise PatternError(
                        __re_rt_error(src, "unknown group name " + repr(name), len(name) + 1)
                    )
                if not __re_rt_checkgroup(state, gid):
                    raise PatternError(
                        __re_rt_error(src, "cannot refer to an open group", len(name) + 1)
                    )
                __re_rt_checklookbehindgroup(src, state, gid)
                out.append(__re_rt_Node(K_GROUPREF, gid, 0, 0))
                return False
            else:
                c = __re_rt_get(src)
                if c == NONE:
                    raise PatternError(__re_rt_error(src, "unexpected end of pattern", 0))
                shown = __re_rt_token(c)
                raise PatternError(
                    __re_rt_error(src, "unknown extension ?P" + shown, len(shown) + 2)
                )
        elif char == 58:
            capture = False
        elif char == 35:
            while True:
                if src.next == NONE:
                    raise PatternError(
                        __re_rt_error(
                            src, "missing ), unterminated comment", __re_rt_tell(src) - start
                        )
                    )
                if __re_rt_get(src) == 41:
                    break
            return False
        elif char == 61 or char == 33 or char == 60:
            __re_rt_parse_assert(src, state, out, verbose, nested, char, start)
            return False
        elif char == 40:
            __re_rt_parse_conditional(src, state, out, verbose, nested, start)
            return False
        elif char == 62:
            capture = False
            atomic = True
        elif char == 45 or __re_rt_flag(char) != 0:
            parsed = __re_rt_parse_flags(src, state, char)
            if len(parsed) == 0:
                if not first or len(out) > 0:
                    raise PatternError(
                        __re_rt_error(
                            src,
                            "global flags not at the start of the expression",
                            __re_rt_tell(src) - start,
                        )
                    )
                return True
            add = parsed[0]
            off = parsed[1]
            capture = False
        else:
            shown = __re_rt_token(char)
            raise PatternError(
                __re_rt_error(src, "unknown extension ?" + shown, len(shown) + 1)
            )
    group = NONE
    if capture:
        group = __re_rt_opengroup(src, state, name, named)
    inner = (verbose or (add & F_VERBOSE) != 0) and (off & F_VERBOSE) == 0
    p = __re_rt_parse_sub(src, state, inner, nested + 1)
    if not __re_rt_match(src, 41):
        raise PatternError(
            __re_rt_error(src, "missing ), unterminated subpattern", __re_rt_tell(src) - start)
        )
    if group != NONE:
        w = __re_rt_getwidth(p, state)
        state.width_lo[group] = w[0]
        state.width_hi[group] = w[1]
    if atomic:
        node = __re_rt_Node(K_ATOMIC, 0, 0, 0)
        node.body = p
        out.append(node)
    else:
        node = __re_rt_Node(K_SUBPATTERN, group, add, off)
        node.body = p
        out.append(node)
    return False


def __re_rt_is_special(c: int) -> bool:
    if c == 46 or c == 92 or c == 91 or c == 123 or c == 40 or c == 41:
        return True
    return c == 42 or c == 43 or c == 63 or c == 94 or c == 36 or c == 124


def __re_rt_parse(
    src: __re_rt_Source, state: __re_rt_State, verbose: bool, nested: int, first: bool
) -> list[__re_rt_Node]:
    out: list[__re_rt_Node] = []
    while True:
        this = src.next
        if this == NONE or this == 124 or this == 41:
            break
        __re_rt_advance(src)
        if verbose:
            if this == 32 or this == 9 or this == 10 or this == 13 or this == 11 or this == 12:
                continue
            if this == 35:
                while True:
                    c = __re_rt_get(src)
                    if c == NONE or c == 10:
                        break
                continue
        if this >= ESC:
            out.append(__re_rt_escape(src, this, state))
        elif not __re_rt_is_special(this):
            out.append(__re_rt_Node(K_LITERAL, this, 0, 0))
        elif this == 91:
            __re_rt_parse_set(src, out)
        elif this == 42 or this == 43 or this == 63 or this == 123:
            __re_rt_parse_repeat(src, out, this)
        elif this == 46:
            out.append(__re_rt_Node(K_ANY, 0, 0, 0))
        elif this == 40:
            if __re_rt_parse_group(src, state, out, verbose, nested, first):
                verbose = (state.flags & F_VERBOSE) != 0
        elif this == 94:
            out.append(__re_rt_Node(K_AT, P_BEGINNING, 0, 0))
        elif this == 36:
            out.append(__re_rt_Node(K_AT, P_END, 0, 0))
    # Non-capturing groups without flags are unpacked.
    unpacked: list[__re_rt_Node] = []
    for node in out:
        if node.kind == K_SUBPATTERN and node.a == NONE and node.b == 0 and node.c == 0:
            for inner in node.body:
                unpacked.append(inner)
        else:
            unpacked.append(node)
    return unpacked


# `_sre.unicode_tolower`, as the compiler reads it: the first character of
# the full lowercase.
def __re_rt_lower(c: int) -> int:
    if c > 1114111 or (c >= 55296 and c <= 57343):
        return c
    return ord(chr(c).lower()[0])


def __re_rt_upper(c: int) -> int:
    if c > 1114111 or (c >= 55296 and c <= 57343):
        return c
    return ord(chr(c).upper()[0])


def __re_rt_iscased(c: int) -> bool:
    return __re_rt_lower(c) != c or __re_rt_upper(c) != c


def __re_rt_ascii_lower(c: int) -> int:
    if c >= 65 and c <= 90:
        return c + 32
    return c


# `re._casefix._EXTRA_CASES` (CPython 3.13): the characters equal under
# IGNORECASE to a lowercase one that lowercase alone does not reach.
def __re_rt_fixes(c: int) -> list[int]:
    if c < 105 or c > 64262 or (c > 1123 and c < 7296) or (c > 8163 and c < 42571):
        return []
    if c == 105:
        return [305]
    if c == 115:
        return [383]
    if c == 181:
        return [956]
    if c == 305:
        return [105]
    if c == 383:
        return [115]
    if c == 837:
        return [953, 8126]
    if c == 912:
        return [8147]
    if c == 944:
        return [8163]
    if c == 946:
        return [976]
    if c == 949:
        return [1013]
    if c == 952:
        return [977]
    if c == 953:
        return [837, 8126]
    if c == 954:
        return [1008]
    if c == 956:
        return [181]
    if c == 960:
        return [982]
    if c == 961:
        return [1009]
    if c == 962:
        return [963]
    if c == 963:
        return [962]
    if c == 966:
        return [981]
    if c == 976:
        return [946]
    if c == 977:
        return [952]
    if c == 981:
        return [966]
    if c == 982:
        return [960]
    if c == 1008:
        return [954]
    if c == 1009:
        return [961]
    if c == 1013:
        return [949]
    if c == 1074:
        return [7296]
    if c == 1076:
        return [7297]
    if c == 1086:
        return [7298]
    if c == 1089:
        return [7299]
    if c == 1090:
        return [7300, 7301]
    if c == 1098:
        return [7302]
    if c == 1123:
        return [7303]
    if c == 7296:
        return [1074]
    if c == 7297:
        return [1076]
    if c == 7298:
        return [1086]
    if c == 7299:
        return [1089]
    if c == 7300:
        return [1090, 7301]
    if c == 7301:
        return [1090, 7300]
    if c == 7302:
        return [1098]
    if c == 7303:
        return [1123]
    if c == 7304:
        return [42571]
    if c == 7777:
        return [7835]
    if c == 7835:
        return [7777]
    if c == 8126:
        return [837, 953]
    if c == 8147:
        return [912]
    if c == 8163:
        return [944]
    if c == 42571:
        return [7304]
    if c == 64261:
        return [64262]
    if c == 64262:
        return [64261]
    return []


# Sorted, merged runs of the given characters, as pairs.
def __re_rt_runs(members: list[int]) -> list[int]:
    members.sort()
    out: list[int] = []
    for c in members:
        n = len(out)
        if n > 0 and out[n - 1] == c:
            continue
        if n > 0 and out[n - 1] + 1 == c:
            out[n - 1] = c
        else:
            out.append(c)
            out.append(c)
    return out


def __re_rt_sort_pairs(p: list[int]) -> None:
    i = 2
    while i < len(p):
        lo = p[i]
        hi = p[i + 1]
        j = i - 2
        while j >= 0 and (p[j] > lo or (p[j] == lo and p[j + 1] > hi)):
            p[j + 2] = p[j]
            p[j + 3] = p[j + 1]
            j = j - 2
        p[j + 2] = lo
        p[j + 3] = hi
        i = i + 2


def __re_rt_emit_set(
    out: __re_rt_Compiled,
    negate: bool,
    mode: int,
    ranges: list[int],
    cats: list[int],
    uni: list[int],
) -> None:
    start = len(out.code)
    out.code.append(OP_SET)
    out.code.append(0)
    if negate:
        out.code.append(1)
    else:
        out.code.append(0)
    out.code.append(mode)
    out.code.append(len(ranges) // 2)
    out.code.extend(ranges)
    out.code.append(len(cats))
    out.code.extend(cats)
    out.code.append(len(uni) // 2)
    out.code.extend(uni)
    out.code[start + 1] = len(out.code) - start


def __re_rt_emit_plain(out: __re_rt_Compiled, c: int, negate: bool) -> None:
    if negate:
        out.code.append(OP_NOTCHAR)
    else:
        out.code.append(OP_CHAR)
    out.code.append(c)


def __re_rt_emit_literal(out: __re_rt_Compiled, c: int, flags: int, negate: bool) -> None:
    if (flags & F_IGNORECASE) == 0:
        __re_rt_emit_plain(out, c, negate)
        return
    if (flags & F_UNICODE) == 0:
        if c < 128 and __re_rt_is_letter(c):
            if negate:
                out.code.append(OP_NOTCHAR_IA)
            else:
                out.code.append(OP_CHAR_IA)
            out.code.append(__re_rt_ascii_lower(c))
        else:
            __re_rt_emit_plain(out, c, negate)
        return
    if not __re_rt_iscased(c):
        __re_rt_emit_plain(out, c, negate)
        return
    low = __re_rt_lower(c)
    out.lower = True
    extra = __re_rt_fixes(low)
    if len(extra) == 0:
        if negate:
            out.code.append(OP_NOTCHAR_I)
        else:
            out.code.append(OP_CHAR_I)
        out.code.append(low)
        return
    members = [low]
    members.extend(extra)
    empty: list[int] = []
    __re_rt_emit_set(out, negate, 1, __re_rt_runs(members), empty, empty)


# `_optimize_charset` and the choice of IN, IN_IGNORE, or IN_UNI_IGNORE.
def __re_rt_emit_charset(out: __re_rt_Compiled, items: list[int], flags: int) -> None:
    ignore = (flags & F_IGNORECASE) != 0
    narrow = (flags & F_UNICODE) == 0
    negate = False
    members: list[int] = []
    ranges: list[int] = []
    cats: list[int] = []
    uni: list[int] = []
    hascased = False
    k = 0
    while k < len(items):
        tag = items[k]
        x = items[k + 1]
        y = items[k + 2]
        k = k + 3
        if tag == S_NEGATE:
            negate = True
        elif tag == S_CATEGORY:
            if narrow:
                cats.append(x + 6)
            else:
                cats.append(x)
        elif not ignore:
            if tag == S_LITERAL:
                members.append(x)
            else:
                ranges.append(x)
                ranges.append(y)
        elif narrow:
            if tag == S_LITERAL:
                low = __re_rt_ascii_lower(x)
                members.append(low)
                if low < 128 and __re_rt_is_letter(low):
                    hascased = True
            else:
                c = x
                top = y
                if top > 1114111:
                    top = 1114111
                while c <= top:
                    members.append(__re_rt_ascii_lower(c))
                    c = c + 1
                if (x <= 90 and y >= 65) or (x <= 122 and y >= 97):
                    hascased = True
        elif tag == S_LITERAL:
            f = __re_rt_lower(x)
            members.append(f)
            members.extend(__re_rt_fixes(f))
            if __re_rt_iscased(f):
                hascased = True
        else:
            c = x
            top = y
            if top > 65535:
                top = 65535
            while c <= top:
                f = __re_rt_lower(c)
                members.append(f)
                members.extend(__re_rt_fixes(f))
                if __re_rt_iscased(c):
                    hascased = True
                c = c + 1
            if y > 65535:
                if x > 65536:
                    uni.append(x)
                else:
                    uni.append(65536)
                uni.append(y)
                hascased = True
    ranges.extend(__re_rt_runs(members))
    __re_rt_sort_pairs(ranges)
    mode = 0
    if ignore and hascased:
        if narrow:
            mode = 2
        else:
            mode = 1
            out.lower = True
            if len(uni) > 0:
                out.upper = True
    if mode != 1:
        # Without the lowercase test, a range beyond the BMP is a range.
        ranges.extend(uni)
        uni = []
    __re_rt_emit_set(out, negate, mode, ranges, cats, uni)


def __re_rt_emit_sequence(out: __re_rt_Compiled, nodes: list[__re_rt_Node], flags: int) -> None:
    for node in nodes:
        __re_rt_emit_node(out, node, flags)


def __re_rt_emit_repeat(
    out: __re_rt_Compiled, kind: int, lo: int, hi: int, body: list[__re_rt_Node], flags: int
) -> None:
    r = out.repeats
    out.repeats = r + 1
    out.code.append(OP_REPEAT)
    out.code.append(r)
    until = len(out.code)
    if kind == R_MAX:
        out.code.append(OP_MAX_UNTIL)
        out.code.append(r)
        out.code.append(lo)
        out.code.append(hi)
        out.code.append(0)
        out.code.append(0)
        out.code[until + 4] = len(out.code)
    else:
        out.code.append(OP_MIN_UNTIL)
        out.code.append(r)
        out.code.append(lo)
        out.code.append(hi)
        out.code.append(0)
        out.code.append(0)
        out.code.append(0)
        out.code[until + 4] = len(out.code)
        out.code.append(OP_LAZY_MORE)
        out.code.append(r)
        out.code.append(hi)
        more = len(out.code)
        out.code.append(0)
        out.code[until + 5] = len(out.code)
        out.code[more] = len(out.code)
    __re_rt_emit_sequence(out, body, flags)
    out.code.append(OP_JMP)
    out.code.append(until)
    if kind == R_MAX:
        out.code[until + 5] = len(out.code)
    else:
        out.code[until + 6] = len(out.code)


def __re_rt_emit_node(out: __re_rt_Compiled, node: __re_rt_Node, flags: int) -> None:
    kind = node.kind
    ignore = (flags & F_IGNORECASE) != 0
    narrow = (flags & F_UNICODE) == 0
    if kind == K_LITERAL:
        __re_rt_emit_literal(out, node.a, flags, False)
    elif kind == K_NOT_LITERAL:
        __re_rt_emit_literal(out, node.a, flags, True)
    elif kind == K_ANY:
        if (flags & F_DOTALL) != 0:
            out.code.append(OP_ANYALL)
        else:
            out.code.append(OP_ANY)
    elif kind == K_IN:
        __re_rt_emit_charset(out, node.items, flags)
    elif kind == K_AT:
        multiline = (flags & F_MULTILINE) != 0
        where = AT_BEGINNING
        if node.a == P_BEGINNING:
            if multiline:
                where = AT_BEGINNING_LINE
        elif node.a == P_BEGINNING_STRING:
            where = AT_BEGINNING_STRING
        elif node.a == P_END:
            where = AT_END
            if multiline:
                where = AT_END_LINE
        elif node.a == P_END_STRING:
            where = AT_END_STRING
        elif node.a == P_BOUNDARY:
            where = AT_BOUNDARY
            if narrow:
                where = AT_ASCII_BOUNDARY
        else:
            where = AT_NON_BOUNDARY
            if narrow:
                where = AT_ASCII_NON_BOUNDARY
        out.code.append(OP_AT)
        out.code.append(where)
    elif kind == K_BRANCH:
        ends: list[int] = []
        n = len(node.alts)
        k = 0
        while k < n:
            if k + 1 < n:
                out.code.append(OP_SPLIT)
                at = len(out.code)
                out.code.append(at + 2)
                out.code.append(0)
                __re_rt_emit_sequence(out, node.alts[k].body, flags)
                out.code.append(OP_JMP)
                ends.append(len(out.code))
                out.code.append(0)
                out.code[at + 1] = len(out.code)
            else:
                __re_rt_emit_sequence(out, node.alts[k].body, flags)
            k = k + 1
        for e in ends:
            out.code[e] = len(out.code)
    elif kind == K_SUBPATTERN:
        inner = __re_rt_combine_flags(flags, node.b, node.c)
        if node.a != NONE:
            out.code.append(OP_SAVE)
            out.code.append(2 * node.a)
            __re_rt_emit_sequence(out, node.body, inner)
            out.code.append(OP_SAVE)
            out.code.append(2 * node.a + 1)
        else:
            __re_rt_emit_sequence(out, node.body, inner)
    elif kind == K_REPEAT:
        if node.a == R_POSSESSIVE:
            out.code.append(OP_ATOMIC)
            __re_rt_emit_repeat(out, R_MAX, node.b, node.c, node.body, flags)
            out.code.append(OP_ATOMIC_END)
        else:
            __re_rt_emit_repeat(out, node.a, node.b, node.c, node.body, flags)
    elif kind == K_ASSERT:
        out.code.append(OP_LOOK)
        out.code.append(node.b)
        out.code.append(node.c)
        at = len(out.code)
        out.code.append(0)
        __re_rt_emit_sequence(out, node.body, flags)
        out.code.append(OP_LOOK_END)
        out.code[at] = len(out.code)
    elif kind == K_FAILURE:
        out.code.append(OP_FAIL)
    elif kind == K_ATOMIC:
        out.code.append(OP_ATOMIC)
        __re_rt_emit_sequence(out, node.body, flags)
        out.code.append(OP_ATOMIC_END)
    elif kind == K_GROUPREF:
        if not ignore:
            out.code.append(OP_GROUPREF)
        elif narrow:
            out.code.append(OP_GROUPREF_IA)
        else:
            out.lower = True
            out.code.append(OP_GROUPREF_I)
        out.code.append(node.a)
    elif kind == K_GROUPREF_EXISTS:
        out.code.append(OP_COND)
        out.code.append(node.a)
        at = len(out.code)
        out.code.append(0)
        __re_rt_emit_sequence(out, node.body, flags)
        if node.b == 1:
            out.code.append(OP_JMP)
            jump = len(out.code)
            out.code.append(0)
            out.code[at] = len(out.code)
            __re_rt_emit_sequence(out, node.alts[0].body, flags)
            out.code[jump] = len(out.code)
        else:
            out.code[at] = len(out.code)


def __re_rt_has_groupref(nodes: list[__re_rt_Node]) -> bool:
    for node in nodes:
        if node.kind == K_GROUPREF:
            return True
        if __re_rt_has_groupref(node.body):
            return True
        for alt in node.alts:
            if __re_rt_has_groupref(alt.body):
                return True
    return False


def __re_rt_lookbehind_groupref(nodes: list[__re_rt_Node]) -> bool:
    for node in nodes:
        if node.kind == K_ASSERT and node.a == 1:
            if __re_rt_has_groupref(node.body):
                return True
        elif __re_rt_lookbehind_groupref(node.body):
            return True
        for alt in node.alts:
            if __re_rt_lookbehind_groupref(alt.body):
                return True
    return False


def __re_rt_combine_flags(flags: int, add: int, off: int) -> int:
    inner = flags
    if (add & F_TYPE) != 0:
        inner = inner - (inner & F_TYPE)
    inner = inner | add
    return inner - (inner & off)


# Whether CPython's search would pass over positions the pattern matches at:
# it skips to the characters a pattern can start with, a set it compiles under
# the pattern's own flags, so a class inside a leading group that changes
# ASCII to UNICODE or back reads differently there than in the group. Where
# the group's reading is the wider one, the search never tries positions a
# match would match at.
def __re_rt_search_skips_matches(nodes: list[__re_rt_Node], flags: int) -> bool:
    inner = flags
    seq = nodes
    while len(seq) > 0 and seq[0].kind == K_SUBPATTERN:
        inner = __re_rt_combine_flags(inner, seq[0].b, seq[0].c)
        seq = seq[0].body
    if len(seq) == 0 or seq[0].kind != K_IN:
        return False
    if (flags & F_UNICODE) == (inner & F_UNICODE):
        return False
    items = seq[0].items
    # `\w`, `\d`, and `\s` match more under UNICODE, their negations more
    # under ASCII; a negated set turns that round.
    unicode = (inner & F_UNICODE) != 0
    negated = items[0] == S_NEGATE
    wider = False
    k = 0
    while k < len(items):
        if items[k] == S_CATEGORY:
            positive = items[k + 1] % 2 == 0
            if (positive == unicode) != negated:
                wider = True
        k = k + 3
    if not wider:
        return False
    if (inner & F_IGNORECASE) != 0:
        wide = (inner & F_UNICODE) != 0
        k = 0
        while k < len(items):
            tag = items[k]
            x = items[k + 1]
            y = items[k + 2]
            k = k + 3
            if tag == S_LITERAL:
                if wide and __re_rt_iscased(x):
                    return False
                if not wide and x < 128 and __re_rt_is_letter(x):
                    return False
            elif tag == S_RANGE:
                if y > 65535:
                    return False
                c = x
                while c <= y:
                    if wide and __re_rt_iscased(c):
                        return False
                    if not wide and c < 128 and __re_rt_is_letter(c):
                        return False
                    c = c + 1
    return True


# Parse and compile a pattern under `flags`, as `re.compile` does: a str
# pattern, or a bytes one (`istext` false) given as its latin-1 decoding.
def __re_rt_compile(pattern: str, flags: int, istext: bool) -> __re_rt_Compiled:
    chars: list[int] = []
    for ch in pattern:
        chars.append(ord(ch))
    src = __re_rt_Source(chars, istext)
    __re_rt_advance(src)
    state = __re_rt_State(flags)
    nodes = __re_rt_parse_sub(src, state, (flags & F_VERBOSE) != 0, 0)
    fixed = state.flags
    if istext:
        if (fixed & F_LOCALE) != 0:
            raise ValueError("cannot use LOCALE flag with a str pattern")
        if (fixed & F_ASCII) == 0:
            fixed = fixed | F_UNICODE
        elif (fixed & F_UNICODE) != 0:
            raise ValueError("ASCII and UNICODE flags are incompatible")
    else:
        if (fixed & F_UNICODE) != 0:
            raise ValueError("cannot use UNICODE flag with a bytes pattern")
        if (fixed & F_LOCALE) != 0 and (fixed & F_ASCII) != 0:
            raise ValueError("ASCII and LOCALE flags are incompatible")
        if (fixed & F_LOCALE) != 0:
            raise NotImplementedError(
                "re.LOCALE matches a bytes pattern by the host's locale, which is not"
                " supported. Hint: leave it out"
            )
    if src.next != NONE:
        raise PatternError(__re_rt_error(src, "unbalanced parenthesis", 0))
    k = 0
    while k < len(state.ref_groups):
        if state.ref_groups[k] >= len(state.width_lo):
            raise PatternError(
                __re_rt_position(
                    chars,
                    "invalid group reference " + str(state.ref_groups[k]),
                    state.ref_pos[k],
                )
            )
        k = k + 1
    if (fixed & F_DEBUG) != 0:
        raise NotImplementedError(
            "re.DEBUG prints the parsed pattern, which is not supported"
        )
    if state.variable_lookbehind:
        raise PatternError("look-behind requires fixed-width pattern")
    if __re_rt_lookbehind_groupref(nodes):
        raise NotImplementedError(
            "a group reference inside a lookbehind is not supported in a pattern"
        )
    if __re_rt_search_skips_matches(nodes, fixed):
        raise NotImplementedError(
            "a pattern that starts with a class inside (?a:...) or (?u:...) that matches"
            " more there than under the pattern's own flags is not supported: CPython's"
            " search reads \\w, \\d, and \\s by the pattern's flags and its match by the"
            " group's. Hint: put the flag on the whole pattern"
        )
    out = __re_rt_Compiled()
    out.flags = fixed
    out.groups = len(state.width_lo)
    out.names = state.names
    out.indices = state.gids
    __re_rt_emit_sequence(out, nodes, fixed)
    out.code.append(OP_MATCH)
    if len(nodes) > 0 and nodes[0].kind == K_LITERAL and (fixed & F_IGNORECASE) == 0:
        out.first = nodes[0].a
    return out
