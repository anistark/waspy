def tuple_sum() -> int:
    """Index each element of a literal tuple: 1 + 2 + 3 = 6."""
    t = (1, 2, 3)
    return t[0] + t[1] + t[2]


def single_element() -> int:
    """A one-element tuple still needs its trailing comma."""
    single = (99,)
    return single[0]


def main():
    t = (1, 2, 3)
    print(t[0])
    print(t[1])
    print(t[2])

    mixed = (42, "hello", 3.14)
    print(mixed[0])
    print(mixed[1])
    print(mixed[2])

    empty: tuple[int] = ()

    single = (99,)
    print(single[0])

if __name__ == "__main__":
    main()
