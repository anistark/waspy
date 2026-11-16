# base64's RFC 4648 encodings (Lib/base64.py over binascii), on bytes.

_STD = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
_URL = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"


class Error(ValueError):
    pass


def _encode(s: bytes, alphabet: bytes) -> bytes:
    out: list[int] = []
    n = len(s)
    i = 0
    while i + 2 < n:
        v = (s[i] << 16) | (s[i + 1] << 8) | s[i + 2]
        out.append(alphabet[(v >> 18) & 63])
        out.append(alphabet[(v >> 12) & 63])
        out.append(alphabet[(v >> 6) & 63])
        out.append(alphabet[v & 63])
        i += 3
    rest = n - i
    if rest == 1:
        v = s[i] << 16
        out.append(alphabet[(v >> 18) & 63])
        out.append(alphabet[(v >> 12) & 63])
        out.append(61)
        out.append(61)
    elif rest == 2:
        v = (s[i] << 16) | (s[i + 1] << 8)
        out.append(alphabet[(v >> 18) & 63])
        out.append(alphabet[(v >> 12) & 63])
        out.append(alphabet[(v >> 6) & 63])
        out.append(61)
    return bytes(out)


def _value(c: int, alphabet: bytes) -> int:
    i = 0
    while i < 64:
        if alphabet[i] == c:
            return i
        i += 1
    return -1


def _decode(s: bytes, alphabet: bytes, strict: bool) -> bytes:
    # binascii.a2b_base64, step for step (Modules/binascii.c).
    out: list[int] = []
    quad_pos = 0
    leftchar = 0
    pads = 0
    padding_started = False
    n = len(s)
    i = 0
    while i < n:
        c = s[i]
        if c == 61:
            padding_started = True
            if strict and quad_pos == 0:
                if i == 0:
                    raise Error("Leading padding not allowed")
                raise Error("Excess padding not allowed")
            if quad_pos >= 2:
                pads += 1
                if quad_pos + pads >= 4:
                    if strict and i + 1 < n:
                        raise Error("Excess data after padding")
                    return bytes(out)
            i += 1
            continue
        v = _value(c, alphabet)
        if v < 0:
            if strict:
                raise Error("Only base64 data is allowed")
            i += 1
            continue
        if strict and padding_started:
            raise Error("Discontinuous padding not allowed")
        pads = 0
        if quad_pos == 0:
            quad_pos = 1
            leftchar = v
        elif quad_pos == 1:
            quad_pos = 2
            out.append(((leftchar << 2) | (v >> 4)) & 255)
            leftchar = v & 15
        elif quad_pos == 2:
            quad_pos = 3
            out.append(((leftchar << 4) | (v >> 2)) & 255)
            leftchar = v & 3
        else:
            quad_pos = 0
            out.append(((leftchar << 6) | v) & 255)
            leftchar = 0
        i += 1
    if quad_pos == 1:
        raise Error(
            "Invalid base64-encoded string: number of data characters ("
            + str(len(out) // 3 * 4 + 1)
            + ") cannot be 1 more than a multiple of 4"
        )
    if quad_pos != 0:
        raise Error("Incorrect padding")
    return bytes(out)


def _urlsafe(s: bytes) -> bytes:
    out: list[int] = []
    for c in s:
        if c == 45:
            out.append(43)
        elif c == 95:
            out.append(47)
        else:
            out.append(c)
    return bytes(out)


def b64encode(s: bytes) -> bytes:
    return _encode(s, _STD)


def b64decode(s: bytes, validate: bool = False) -> bytes:
    return _decode(s, _STD, validate)


def standard_b64encode(s: bytes) -> bytes:
    return _encode(s, _STD)


def standard_b64decode(s: bytes) -> bytes:
    return _decode(s, _STD, False)


def urlsafe_b64encode(s: bytes) -> bytes:
    return _encode(s, _URL)


def urlsafe_b64decode(s: bytes) -> bytes:
    return _decode(_urlsafe(s), _STD, False)


def b16encode(s: bytes) -> bytes:
    digits = b"0123456789ABCDEF"
    out: list[int] = []
    for c in s:
        out.append(digits[c >> 4])
        out.append(digits[c & 15])
    return bytes(out)


def _hex_value(c: int) -> int:
    if 48 <= c <= 57:
        return c - 48
    if 65 <= c <= 70:
        return c - 55
    return -1


def b16decode(s: bytes, casefold: bool = False) -> bytes:
    digits = b"0123456789ABCDEF"
    if casefold:
        upper: list[int] = []
        for c in s:
            upper.append(c - 32 if 97 <= c <= 122 else c)
        s = bytes(upper)
    if len(s) % 2:
        raise Error("Odd-length string")
    out: list[int] = []
    i = 0
    while i < len(s):
        hi = _hex_value(s[i])
        lo = _hex_value(s[i + 1])
        if hi < 0 or lo < 0:
            raise Error("Non-base16 digit found")
        out.append(hi * 16 + lo)
        i += 2
    return bytes(out)
