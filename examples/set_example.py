def dedup_size() -> int:
    """Duplicate members collapse at construction: {1, 2, 2, 3, 1} has 3."""
    s = {1, 2, 2, 3, 1}
    return len(s)


def membership() -> int:
    """`in` and `not in` probe the set's hash table."""
    s = {4, 5, 6}
    if 5 in s and 7 not in s:
        return 1
    return 0


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
