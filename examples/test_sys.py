"""sys.maxsize, the largest int under waspy's 32-bit int (CPython's is
2**63 - 1). argv, platform, version, path, and the standard streams are
tested in tests/unit/stdlib.rs."""

import sys


def test_sys() -> int:
    return sys.maxsize
