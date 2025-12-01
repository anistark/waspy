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
