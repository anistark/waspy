# hashlib's MD5, SHA-1, SHA-224, and SHA-256 (RFC 1321, FIPS 180-4), on
# bytes. A word is an int holding 32 bits as two's complement, combined
# through the wrapping intrinsics `__waspy_u32_*`, since int arithmetic here
# raises OverflowError past 32 bits.

_K256 = [1116352408, 1899447441, -1245643825, -373957723, 961987163, 1508970993, -1841331548, -1424204075, -670586216, 310598401, 607225278, 1426881987, 1925078388, -2132889090, -1680079193, -1046744716, -459576895, -272742522, 264347078, 604807628, 770255983, 1249150122, 1555081692, 1996064986, -1740746414, -1473132947, -1341970488, -1084653625, -958395405, -710438585, 113926993, 338241895, 666307205, 773529912, 1294757372, 1396182291, 1695183700, 1986661051, -2117940946, -1838011259, -1564481375, -1474664885, -1035236496, -949202525, -778901479, -694614492, -200395387, 275423344, 430227734, 506948616, 659060556, 883997877, 958139571, 1322822218, 1537002063, 1747873779, 1955562222, 2024104815, -2067236844, -1933114872, -1866530822, -1538233109, -1090935817, -965641998]
_H256 = [1779033703, -1150833019, 1013904242, -1521486534, 1359893119, -1694144372, 528734635, 1541459225]
_H224 = [-1056596264, 914150663, 812702999, -150054599, -4191439, 1750603025, 1694076839, -1090891868]
_TMD5 = [-680876936, -389564586, 606105819, -1044525330, -176418897, 1200080426, -1473231341, -45705983, 1770035416, -1958414417, -42063, -1990404162, 1804603682, -40341101, -1502002290, 1236535329, -165796510, -1069501632, 643717713, -373897302, -701558691, 38016083, -660478335, -405537848, 568446438, -1019803690, -187363961, 1163531501, -1444681467, -51403784, 1735328473, -1926607734, -378558, -2022574463, 1839030562, -35309556, -1530992060, 1272893353, -155497632, -1094730640, 681279174, -358537222, -722521979, 76029189, -640364487, -421815835, 530742520, -995338651, -198630844, 1126891415, -1416354905, -57434055, 1700485571, -1894986606, -1051523, -2054922799, 1873313359, -30611744, -1560198380, 1309151649, -145523070, -1120210379, 718787259, -343485551]
_SMD5 = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21]
_HEX = "0123456789abcdef"


def _add(a: int, b: int) -> int:
    return __waspy_u32_add(a, b)


def _rotr(x: int, n: int) -> int:
    return __waspy_u32_rotr(x, n)


def _rotl(x: int, n: int) -> int:
    return __waspy_u32_rotl(x, n)


def _shr(x: int, n: int) -> int:
    return __waspy_u32_shr(x, n)


def _pad(data: bytes, big_endian: bool) -> bytes:
    n = len(data)
    bits_hi = n >> 29
    bits_lo = __waspy_u32_shl(n, 3)
    zeros = (55 - n) % 64
    out: list[int] = list(data)
    out.append(128)
    for _ in range(zeros):
        out.append(0)
    if big_endian:
        for w in [bits_hi, bits_lo]:
            out.append(_shr(w, 24) & 255)
            out.append(_shr(w, 16) & 255)
            out.append(_shr(w, 8) & 255)
            out.append(w & 255)
    else:
        for w in [bits_lo, bits_hi]:
            out.append(w & 255)
            out.append(_shr(w, 8) & 255)
            out.append(_shr(w, 16) & 255)
            out.append(_shr(w, 24) & 255)
    return bytes(out)


def _word_be(m: bytes, i: int) -> int:
    return __waspy_u32_shl(m[i], 24) | (m[i + 1] << 16) | (m[i + 2] << 8) | m[i + 3]


def _word_le(m: bytes, i: int) -> int:
    return __waspy_u32_shl(m[i + 3], 24) | (m[i + 2] << 16) | (m[i + 1] << 8) | m[i]


def _sha256(data: bytes, init: list[int]) -> list[int]:
    h = list(init)
    m = _pad(data, True)
    w = [0] * 64
    off = 0
    while off < len(m):
        for t in range(16):
            w[t] = _word_be(m, off + 4 * t)
        for t in range(16, 64):
            x = w[t - 15]
            s0 = _rotr(x, 7) ^ _rotr(x, 18) ^ _shr(x, 3)
            y = w[t - 2]
            s1 = _rotr(y, 17) ^ _rotr(y, 19) ^ _shr(y, 10)
            w[t] = _add(_add(w[t - 16], s0), _add(w[t - 7], s1))
        a = h[0]
        b = h[1]
        c = h[2]
        d = h[3]
        e = h[4]
        f = h[5]
        g = h[6]
        hh = h[7]
        for t in range(64):
            S1 = _rotr(e, 6) ^ _rotr(e, 11) ^ _rotr(e, 25)
            ch = (e & f) ^ (~e & g)
            temp1 = _add(_add(_add(hh, S1), _add(ch, _K256[t])), w[t])
            S0 = _rotr(a, 2) ^ _rotr(a, 13) ^ _rotr(a, 22)
            maj = (a & b) ^ (a & c) ^ (b & c)
            temp2 = _add(S0, maj)
            hh = g
            g = f
            f = e
            e = _add(d, temp1)
            d = c
            c = b
            b = a
            a = _add(temp1, temp2)
        h[0] = _add(h[0], a)
        h[1] = _add(h[1], b)
        h[2] = _add(h[2], c)
        h[3] = _add(h[3], d)
        h[4] = _add(h[4], e)
        h[5] = _add(h[5], f)
        h[6] = _add(h[6], g)
        h[7] = _add(h[7], hh)
        off += 64
    return h


def _sha1(data: bytes) -> list[int]:
    h = [1732584193, -271733879, -1732584194, 271733878, -1009589776]
    m = _pad(data, True)
    w = [0] * 80
    off = 0
    while off < len(m):
        for t in range(16):
            w[t] = _word_be(m, off + 4 * t)
        for t in range(16, 80):
            w[t] = _rotl(w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16], 1)
        a = h[0]
        b = h[1]
        c = h[2]
        d = h[3]
        e = h[4]
        for t in range(80):
            if t < 20:
                f = (b & c) | (~b & d)
                k = 1518500249
            elif t < 40:
                f = b ^ c ^ d
                k = 1859775393
            elif t < 60:
                f = (b & c) | (b & d) | (c & d)
                k = -1894007588
            else:
                f = b ^ c ^ d
                k = -899497514
            temp = _add(_add(_rotl(a, 5), f), _add(_add(e, k), w[t]))
            e = d
            d = c
            c = _rotl(b, 30)
            b = a
            a = temp
        h[0] = _add(h[0], a)
        h[1] = _add(h[1], b)
        h[2] = _add(h[2], c)
        h[3] = _add(h[3], d)
        h[4] = _add(h[4], e)
        off += 64
    return h


def _md5(data: bytes) -> list[int]:
    h = [1732584193, -271733879, -1732584194, 271733878]
    m = _pad(data, False)
    x = [0] * 16
    off = 0
    while off < len(m):
        for t in range(16):
            x[t] = _word_le(m, off + 4 * t)
        a = h[0]
        b = h[1]
        c = h[2]
        d = h[3]
        for i in range(64):
            if i < 16:
                f = (b & c) | (~b & d)
                g = i
            elif i < 32:
                f = (d & b) | (~d & c)
                g = (5 * i + 1) % 16
            elif i < 48:
                f = b ^ c ^ d
                g = (3 * i + 5) % 16
            else:
                f = c ^ (b | ~d)
                g = (7 * i) % 16
            f = _add(_add(f, a), _add(_TMD5[i], x[g]))
            a = d
            d = c
            c = b
            b = _add(b, _rotl(f, _SMD5[i]))
        h[0] = _add(h[0], a)
        h[1] = _add(h[1], b)
        h[2] = _add(h[2], c)
        h[3] = _add(h[3], d)
        off += 64
    return h


def _bytes_of(words: list[int], count: int, big_endian: bool) -> bytes:
    out: list[int] = []
    for w in words[:count]:
        if big_endian:
            out.append(_shr(w, 24) & 255)
            out.append(_shr(w, 16) & 255)
            out.append(_shr(w, 8) & 255)
            out.append(w & 255)
        else:
            out.append(w & 255)
            out.append(_shr(w, 8) & 255)
            out.append(_shr(w, 16) & 255)
            out.append(_shr(w, 24) & 255)
    return bytes(out)


class _Hash:
    def __init__(self, name: str, data: bytes):
        if name not in ["md5", "sha1", "sha224", "sha256"]:
            raise ValueError("unsupported hash type " + name)
        self.name: str = name
        self._data: bytes = data
        if name == "md5":
            self.digest_size: int = 16
        elif name == "sha1":
            self.digest_size = 20
        elif name == "sha224":
            self.digest_size = 28
        else:
            self.digest_size = 32
        self.block_size: int = 64

    def update(self, data: bytes) -> None:
        self._data = self._data + data

    def digest(self) -> bytes:
        if self.name == "md5":
            return _bytes_of(_md5(self._data), 4, False)
        if self.name == "sha1":
            return _bytes_of(_sha1(self._data), 5, True)
        if self.name == "sha224":
            return _bytes_of(_sha256(self._data, _H224), 7, True)
        return _bytes_of(_sha256(self._data, _H256), 8, True)

    def hexdigest(self) -> str:
        out: list[str] = []
        for b in self.digest():
            out.append(_HEX[b >> 4])
            out.append(_HEX[b & 15])
        return "".join(out)

    def copy(self) -> "_Hash":
        return _Hash(self.name, self._data)


def md5(data: bytes = b"") -> _Hash:
    return _Hash("md5", data)


def sha1(data: bytes = b"") -> _Hash:
    return _Hash("sha1", data)


def sha224(data: bytes = b"") -> _Hash:
    return _Hash("sha224", data)


def sha256(data: bytes = b"") -> _Hash:
    return _Hash("sha256", data)


def new(name: str, data: bytes = b"") -> _Hash:
    return _Hash(name.lower(), data)
