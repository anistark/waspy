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


def mutate_size() -> int:
    """`add` inserts, and re-adding an existing member changes nothing."""
    s = {1, 2}
    s.add(3)
    s.add(2)
    return len(s)


def grow_and_find() -> int:
    """Adding past the literal's capacity rehashes into a larger table, and
    every member (old and new) is still found afterwards."""
    s = {1}
    i = 0
    while i < 50:
        s.add(i)
        i = i + 1
    found = 0
    if 37 in s:
        found = 1
    if 77 in s:
        found = found + 10
    return len(s) * 100 + found


def remove_and_discard() -> int:
    """`remove` drops a member; `discard` does too, but ignores a miss."""
    s = {1, 2, 3}
    s.remove(2)
    s.discard(9)
    s.discard(3)
    still_there = 0
    if 2 in s:
        still_there = 1
    return len(s) * 10 + still_there


def readd_after_remove() -> int:
    """A removed member leaves a tombstone; adding it back makes it a member
    again rather than probing past its old bucket forever."""
    s = {1, 2}
    s.remove(1)
    s.add(1)
    back = 0
    if 1 in s:
        back = 1
    return len(s) * 10 + back


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
