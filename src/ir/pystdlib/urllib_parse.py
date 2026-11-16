# urllib.parse, CPython 3.13's algorithms (Lib/urllib/parse.py) on str.

from typing import NamedTuple, Optional

_C0_OR_SPACE = "\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f "
_SCHEME_CHARS = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-."
_ALWAYS_SAFE = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-~"
_HEX = "0123456789ABCDEF"
_USES_PARAMS = ["", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu", "sip", "sips", "mms", "sftp", "tel"]
_USES_NETLOC = ["", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp", "snews", "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git", "git+ssh", "ws", "wss", "itms-services"]
_USES_RELATIVE = ["", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms", "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss"]


def _hostinfo(netloc: str) -> tuple[str, str]:
    hostinfo = netloc.rpartition("@")[2]
    before, have_open_br, bracketed = hostinfo.partition("[")
    if have_open_br:
        hostname, sep, port = bracketed.partition("]")
        port = port.partition(":")[2]
    else:
        hostname, sep, port = hostinfo.partition(":")
    return hostname, port


def _hostname(netloc: str) -> Optional[str]:
    hostname = _hostinfo(netloc)[0]
    if not hostname:
        return None
    host, percent, zone = hostname.partition("%")
    return host.lower() + percent + zone


def _port(netloc: str) -> Optional[int]:
    port = _hostinfo(netloc)[1]
    if not port:
        return None
    if port.isdigit() and port.isascii():
        value = int(port)
    else:
        raise ValueError("Port could not be cast to integer value as " + repr(port))
    if not (0 <= value <= 65535):
        raise ValueError("Port out of range 0-65535")
    return value


def _username(netloc: str) -> Optional[str]:
    userinfo, have_info, hostinfo = netloc.rpartition("@")
    if have_info:
        return userinfo.partition(":")[0]
    return None


def _password(netloc: str) -> Optional[str]:
    userinfo, have_info, hostinfo = netloc.rpartition("@")
    if have_info:
        username, have_password, password = userinfo.partition(":")
        if have_password:
            return password
    return None


class SplitResult(NamedTuple):
    scheme: str
    netloc: str
    path: str
    query: str
    fragment: str

    @property
    def hostname(self) -> Optional[str]:
        return _hostname(self.netloc)

    @property
    def port(self) -> Optional[int]:
        return _port(self.netloc)

    @property
    def username(self) -> Optional[str]:
        return _username(self.netloc)

    @property
    def password(self) -> Optional[str]:
        return _password(self.netloc)

    def geturl(self) -> str:
        return urlunsplit(self.scheme, self.netloc, self.path, self.query, self.fragment)

    def __repr__(self) -> str:
        return ("SplitResult(scheme=" + repr(self.scheme) + ", netloc=" + repr(self.netloc)
                + ", path=" + repr(self.path) + ", query=" + repr(self.query)
                + ", fragment=" + repr(self.fragment) + ")")


class ParseResult(NamedTuple):
    scheme: str
    netloc: str
    path: str
    params: str
    query: str
    fragment: str

    @property
    def hostname(self) -> Optional[str]:
        return _hostname(self.netloc)

    @property
    def port(self) -> Optional[int]:
        return _port(self.netloc)

    @property
    def username(self) -> Optional[str]:
        return _username(self.netloc)

    @property
    def password(self) -> Optional[str]:
        return _password(self.netloc)

    def geturl(self) -> str:
        return urlunparse(self.scheme, self.netloc, self.path, self.params, self.query, self.fragment)

    def __repr__(self) -> str:
        return ("ParseResult(scheme=" + repr(self.scheme) + ", netloc=" + repr(self.netloc)
                + ", path=" + repr(self.path) + ", params=" + repr(self.params)
                + ", query=" + repr(self.query) + ", fragment=" + repr(self.fragment) + ")")


def _splitnetloc(url: str, start: int) -> tuple[str, str]:
    delim = len(url)
    for c in "/?#":
        wdelim = url.find(c, start)
        if wdelim >= 0 and wdelim < delim:
            delim = wdelim
    return url[start:delim], url[delim:]


def _is_hex(s: str) -> bool:
    for c in s:
        if c not in "0123456789abcdefABCDEF":
            return False
    return True


def _is_ipv4(s: str) -> bool:
    # ipaddress.IPv4Address: four decimal octets, no leading zeros.
    octets = s.split(".")
    if len(octets) != 4:
        return False
    for o in octets:
        if not o or not (o.isdigit() and o.isascii()) or len(o) > 3:
            return False
        if len(o) > 1 and o[0] == "0":
            return False
        if int(o) > 255:
            return False
    return True


def _is_ipv6(s: str) -> bool:
    # ipaddress.IPv6Address._ip_int_from_string's checks.
    addr, percent, scope = s.partition("%")
    if percent and (not scope or "%" in scope):
        return False
    if not addr:
        return False
    parts = addr.split(":")
    if len(parts) < 3:
        return False
    if "." in parts[-1]:
        if not _is_ipv4(parts[-1]):
            return False
        parts.pop()
        parts.append("0")
        parts.append("0")
    if len(parts) > 9:
        return False
    skip = -1
    k = 1
    while k < len(parts) - 1:
        if not parts[k]:
            if skip >= 0:
                return False
            skip = k
        k += 1
    if skip >= 0:
        lo = len(parts) - skip - 1
        hi = skip
        if not parts[0]:
            hi -= 1
            if hi:
                return False
        if not parts[-1]:
            lo -= 1
            if lo:
                return False
        if hi + lo + 1 > 8:
            return False
        # the skipped run stands for at least one group
        if hi + lo >= 8:
            return False
    else:
        if len(parts) != 8:
            return False
        if not parts[0] or not parts[-1]:
            return False
    i = 0
    for part in parts:
        if i == skip or (skip >= 0 and not part and (i == 0 or i == len(parts) - 1)):
            i += 1
            continue
        if not part or len(part) > 4 or not _is_hex(part):
            return False
        i += 1
    return True


def _check_bracketed_host(hostname: str) -> None:
    if hostname.startswith("v"):
        dot = hostname.find(".")
        if dot < 2 or dot == len(hostname) - 1 or not _is_hex(hostname[1:dot]) or "\n" in hostname[dot:]:
            raise ValueError("IPvFuture address is invalid")
        return
    if _is_ipv4(hostname):
        raise ValueError("An IPv4 address cannot be in brackets")
    if not _is_ipv6(hostname):
        raise ValueError(repr(hostname) + " does not appear to be an IPv4 or IPv6 address")


def _check_bracketed_netloc(netloc: str) -> None:
    hostname_and_port = netloc.rpartition("@")[2]
    before_bracket, have_open_br, bracketed = hostname_and_port.partition("[")
    if have_open_br:
        if before_bracket:
            raise ValueError("Invalid IPv6 URL")
        hostname, sep, port = bracketed.partition("]")
        if port and not port.startswith(":"):
            raise ValueError("Invalid IPv6 URL")
    else:
        hostname = hostname_and_port.partition(":")[0]
    _check_bracketed_host(hostname)


def urlsplit(url: str, scheme: str = "", allow_fragments: bool = True) -> SplitResult:
    url = url.lstrip(_C0_OR_SPACE)
    scheme = scheme.strip(_C0_OR_SPACE)
    for b in "\t\r\n":
        url = url.replace(b, "")
        scheme = scheme.replace(b, "")
    netloc = ""
    query = ""
    fragment = ""
    i = url.find(":")
    if i > 0 and url[0].isascii() and url[0].isalpha():
        good = True
        for c in url[:i]:
            if c not in _SCHEME_CHARS:
                good = False
                break
        if good:
            scheme = url[:i].lower()
            url = url[i + 1:]
    if url[:2] == "//":
        netloc, url = _splitnetloc(url, 2)
        if ("[" in netloc and "]" not in netloc) or ("]" in netloc and "[" not in netloc):
            raise ValueError("Invalid IPv6 URL")
        if "[" in netloc and "]" in netloc:
            _check_bracketed_netloc(netloc)
    if allow_fragments and "#" in url:
        url, fragment = url.split("#", 1)
    if "?" in url:
        url, query = url.split("?", 1)
    return SplitResult(scheme, netloc, url, query, fragment)


def _splitparams(url: str) -> tuple[str, str]:
    if "/" in url:
        i = url.find(";", url.rfind("/"))
        if i < 0:
            return url, ""
    else:
        i = url.find(";")
    return url[:i], url[i + 1:]


def urlparse(url: str, scheme: str = "", allow_fragments: bool = True) -> ParseResult:
    parts = urlsplit(url, scheme, allow_fragments)
    path = parts.path
    params = ""
    if parts.scheme in _USES_PARAMS and ";" in path:
        path, params = _splitparams(path)
    return ParseResult(parts.scheme, parts.netloc, path, params, parts.query, parts.fragment)


def urlunsplit(scheme: str, netloc: str, url: str, query: str, fragment: str) -> str:
    if netloc:
        if url and url[:1] != "/":
            url = "/" + url
        url = "//" + netloc + url
    elif url[:2] == "//":
        url = "//" + url
    elif scheme and scheme in _USES_NETLOC and (not url or url[:1] == "/"):
        url = "//" + url
    if scheme:
        url = scheme + ":" + url
    if query:
        url = url + "?" + query
    if fragment:
        url = url + "#" + fragment
    return url


def urlunparse(scheme: str, netloc: str, url: str, params: str, query: str, fragment: str) -> str:
    if params:
        url = url + ";" + params
    return urlunsplit(scheme, netloc, url, query, fragment)


def urljoin(base: str, url: str, allow_fragments: bool = True) -> str:
    if not base:
        return url
    if not url:
        return base
    b = urlparse(base, "", allow_fragments)
    u = urlparse(url, b.scheme, allow_fragments)
    scheme = u.scheme
    netloc = u.netloc
    path = u.path
    params = u.params
    query = u.query
    fragment = u.fragment
    if scheme != b.scheme or scheme not in _USES_RELATIVE:
        return url
    if scheme in _USES_NETLOC:
        if netloc:
            return urlunparse(scheme, netloc, path, params, query, fragment)
        netloc = b.netloc
    if not path and not params:
        path = b.path
        params = b.params
        if not query:
            query = b.query
        return urlunparse(scheme, netloc, path, params, query, fragment)
    base_parts = b.path.split("/")
    if base_parts[-1] != "":
        base_parts.pop()
    if path[:1] == "/":
        segments = path.split("/")
    else:
        joined = base_parts + path.split("/")
        segments: list[str] = [joined[0]]
        for seg in joined[1:-1]:
            if seg:
                segments.append(seg)
        if len(joined) > 1:
            segments.append(joined[-1])
    resolved_path: list[str] = []
    for seg in segments:
        if seg == "..":
            if resolved_path:
                resolved_path.pop()
        elif seg == ".":
            continue
        else:
            resolved_path.append(seg)
    if segments[-1] == "." or segments[-1] == "..":
        resolved_path.append("")
    joined_path = "/".join(resolved_path)
    if not joined_path:
        joined_path = "/"
    return urlunparse(scheme, netloc, joined_path, params, query, fragment)


def quote(string: str, safe: str = "/") -> str:
    out: list[str] = []
    for b in string.encode("utf-8"):
        c = chr(b)
        if c in _ALWAYS_SAFE or (b < 128 and c in safe):
            out.append(c)
        else:
            out.append("%" + _HEX[b >> 4] + _HEX[b & 15])
    return "".join(out)


def quote_plus(string: str, safe: str = "") -> str:
    if " " in string:
        return quote(string, safe + " ").replace(" ", "+")
    return quote(string, safe)


def _hexval(c: int) -> int:
    if 48 <= c <= 57:
        return c - 48
    if 65 <= c <= 70:
        return c - 55
    if 97 <= c <= 102:
        return c - 87
    return -1


def _unquote_ascii(run: str) -> bytes:
    data = run.encode("utf-8")
    out: list[int] = []
    n = len(data)
    i = 0
    while i < n:
        c = data[i]
        if c == 37 and i + 2 < n:
            hi = _hexval(data[i + 1])
            lo = _hexval(data[i + 2])
            if hi >= 0 and lo >= 0:
                out.append(hi * 16 + lo)
                i += 3
                continue
        out.append(c)
        i += 1
    return bytes(out)


def _utf8_replace(data: bytes) -> str:
    # bytes.decode("utf-8", "replace"): each maximal invalid subsequence is
    # one U+FFFD, as CPython's decoder replaces it.
    out: list[str] = []
    n = len(data)
    i = 0
    while i < n:
        b = data[i]
        if b < 128:
            out.append(chr(b))
            i += 1
            continue
        if 194 <= b <= 223:
            need = 1
            lo = 128
            hi = 191
            cp = b & 31
        elif 224 <= b <= 239:
            need = 2
            lo = 160 if b == 224 else 128
            hi = 159 if b == 237 else 191
            cp = b & 15
        elif 240 <= b <= 244:
            need = 3
            lo = 144 if b == 240 else 128
            hi = 143 if b == 244 else 191
            cp = b & 7
        else:
            out.append("\ufffd")
            i += 1
            continue
        j = i + 1
        ok = True
        k = 0
        while k < need:
            if j >= n:
                ok = False
                break
            c = data[j]
            low = lo if k == 0 else 128
            high = hi if k == 0 else 191
            if c < low or c > high:
                ok = False
                break
            cp = (cp << 6) | (c & 63)
            j += 1
            k += 1
        if ok:
            out.append(chr(cp))
        else:
            out.append("\ufffd")
        i = j
    return "".join(out)


def unquote(string: str) -> str:
    if "%" not in string:
        return string
    out: list[str] = []
    n = len(string)
    i = 0
    while i < n:
        j = i
        while j < n and ord(string[j]) < 128:
            j += 1
        if j > i:
            out.append(_utf8_replace(_unquote_ascii(string[i:j])))
        k = j
        while k < n and ord(string[k]) >= 128:
            k += 1
        out.append(string[j:k])
        i = k
    return "".join(out)


def unquote_plus(string: str) -> str:
    return unquote(string.replace("+", " "))


def urlencode(query, doseq: bool = False, safe: str = "") -> str:
    parts: list[str] = []
    for k, v in query.items():
        parts.append(quote_plus(str(k), safe) + "=" + quote_plus(str(v), safe))
    return "&".join(parts)


def parse_qsl(qs: str, keep_blank_values: bool = False, strict_parsing: bool = False, separator: str = "&") -> list[tuple[str, str]]:
    r: list[tuple[str, str]] = []
    if not qs:
        return r
    for name_value in qs.split(separator):
        if not name_value and not strict_parsing:
            continue
        nv = name_value.split("=", 1)
        if len(nv) != 2:
            if strict_parsing:
                raise ValueError("bad query field: " + repr(name_value))
            if keep_blank_values:
                nv.append("")
            else:
                continue
        if len(nv[1]) or keep_blank_values:
            name = unquote(nv[0].replace("+", " "))
            value = unquote(nv[1].replace("+", " "))
            r.append((name, value))
    return r


def parse_qs(qs: str, keep_blank_values: bool = False, strict_parsing: bool = False, separator: str = "&") -> dict[str, list[str]]:
    parsed_result: dict[str, list[str]] = {}
    for name, value in parse_qsl(qs, keep_blank_values, strict_parsing, separator):
        if name in parsed_result:
            parsed_result[name].append(value)
        else:
            parsed_result[name] = [value]
    return parsed_result
