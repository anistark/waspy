# uuid: UUID objects, uuid4 from the random module, and the name-based
# uuid3 and uuid5 over hashlib (RFC 4122 layout, as Lib/uuid.py).

import hashlib
import random
from typing import Optional

_HEX = "0123456789abcdef"


class UUID:
    def __init__(self, hex: str):
        digits = hex.replace("urn:", "").replace("uuid:", "").strip("{}").replace("-", "").lower()
        if len(digits) != 32:
            raise ValueError("badly formed hexadecimal UUID string")
        for c in digits:
            if c not in _HEX:
                raise ValueError("badly formed hexadecimal UUID string")
        self.hex: str = digits

    @property
    def bytes(self) -> bytes:
        out: list[int] = []
        i = 0
        while i < 32:
            out.append(_HEX.index(self.hex[i]) * 16 + _HEX.index(self.hex[i + 1]))
            i += 2
        return bytes(out)

    @property
    def version(self) -> Optional[int]:
        # Only an RFC 4122 UUID has a version.
        if (_HEX.index(self.hex[16]) & 12) != 8:
            return None
        return _HEX.index(self.hex[12])

    @property
    def urn(self) -> str:
        return "urn:uuid:" + str(self)

    def __str__(self) -> str:
        h = self.hex
        return h[0:8] + "-" + h[8:12] + "-" + h[12:16] + "-" + h[16:20] + "-" + h[20:32]

    def __repr__(self) -> str:
        return "UUID('" + str(self) + "')"

    def __eq__(self, other: "UUID") -> bool:
        return self.hex == other.hex

    def __lt__(self, other: "UUID") -> bool:
        return self.hex < other.hex

    def __hash__(self) -> int:
        return hash(self.hex)


def _from_bytes(raw: list[int], version: int) -> UUID:
    raw[6] = (raw[6] & 15) | (version << 4)
    raw[8] = (raw[8] & 63) | 128
    out: list[str] = []
    for b in raw:
        out.append(_HEX[b >> 4])
        out.append(_HEX[b & 15])
    return UUID("".join(out))


def uuid4() -> UUID:
    raw: list[int] = []
    for _ in range(16):
        raw.append(random.randint(0, 255))
    return _from_bytes(raw, 4)


def uuid3(namespace: UUID, name: str) -> UUID:
    digest = hashlib.md5(namespace.bytes + name.encode("utf-8")).digest()
    return _from_bytes(list(digest[:16]), 3)


def uuid5(namespace: UUID, name: str) -> UUID:
    digest = hashlib.sha1(namespace.bytes + name.encode("utf-8")).digest()
    return _from_bytes(list(digest[:16]), 5)


NAMESPACE_DNS = UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8")
NAMESPACE_URL = UUID("6ba7b811-9dad-11d1-80b4-00c04fd430c8")
NAMESPACE_OID = UUID("6ba7b812-9dad-11d1-80b4-00c04fd430c8")
NAMESPACE_X500 = UUID("6ba7b814-9dad-11d1-80b4-00c04fd430c8")
