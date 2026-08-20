def tuple_sum() -> int:
    """Index each element of a literal tuple: 1 + 2 + 3 = 6."""
    t = (1, 2, 3)
    return t[0] + t[1] + t[2]


def single_element() -> int:
    """A one-element tuple still needs its trailing comma."""
    single = (99,)
    return single[0]


def float_tuple() -> float:
    """A tuple of floats reads back at full width. Mixing floats with ints in
    one literal is a compile error: they share a slot read at one width."""
    t = (1.5, 2.5, 3.5)
    return t[0] + t[2]


def main():
    t = (1, 2, 3)
    print(t[0])
    print(t[1])
    print(t[2])

    # Every element occupies one 8-byte slot and the tuple reads them all at
    # one width, so a tuple mixing floats with ints is rejected at compile time
    # rather than reading one of the two back as garbage. Keep them one type.
    floats = (1.5, 2.5, 3.5)
    print(floats[0])
    print(floats[2])

    empty: tuple[int] = ()

    single = (99,)
    print(single[0])

if __name__ == "__main__":
    main()
