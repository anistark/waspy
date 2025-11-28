def test_sets():
    # Basic set creation
    s = {1, 2, 3}
    print(s)

    # Empty set
    empty: set[int] = set()
    print(empty)

    # Set with strings
    words = {"hello", "world"}
    print(words)

if __name__ == "__main__":
    test_sets()
