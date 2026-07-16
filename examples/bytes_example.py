def first_byte() -> int:
    """Indexing a bytes value yields the byte's integer value: ord('h') = 104."""
    b = b"hello"
    return b[0]


def slice_length() -> int:
    """b"hello"[1:4] is b"ell", three bytes long."""
    b = b"hello"
    s = b[1:4]
    return len(s)


def concat_length() -> int:
    """Concatenation allocates a fresh 10-byte blob."""
    b1 = b"hello"
    b2 = b"world"
    return len(b1 + b2)


def test_bytes():
    b = b"hello"
    print(b[0])
    print(b[1])

    # Test slicing
    slice1 = b[1:4]
    slice2 = b[:3]
    slice3 = b[2:]

    # Test concatenation
    b1 = b"hello"
    b2 = b"world"
    combined = b1 + b2

if __name__ == "__main__":
    test_bytes()
