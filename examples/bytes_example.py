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
