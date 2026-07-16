def sum_range() -> int:
    """Sum range(5): 0 + 1 + 2 + 3 + 4 = 10."""
    total = 0
    for i in range(5):
        total = total + i
    return total


def sum_with_step() -> int:
    """Sum range(0, 10, 2): 0 + 2 + 4 + 6 + 8 = 20."""
    total = 0
    for i in range(0, 10, 2):
        total = total + i
    return total


def sum_descending() -> int:
    """Sum range(10, 0, -1): 10 + 9 + ... + 1 = 55."""
    total = 0
    for i in range(10, 0, -1):
        total = total + i
    return total


def main():
    for i in range(5):
        print(i)

    print("---")

    for i in range(2, 8):
        print(i)

    print("---")

    for i in range(0, 10, 2):
        print(i)

    print("---")

    for i in range(10, 0, -1):
        print(i)

if __name__ == "__main__":
    main()
