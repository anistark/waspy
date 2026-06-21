def sum_until_five() -> int:
    # `break` exits the loop early.
    total = 0
    for i in range(100):
        if i == 5:
            break
        total = total + i
    return total


def sum_odds_below_ten() -> int:
    # `continue` skips to the next iteration.
    total = 0
    for i in range(10):
        if i % 2 == 0:
            continue
        total = total + i
    return total


def first_multiple_over(limit: int, step: int) -> int:
    # `break`/`continue` inside a `while` loop.
    value = 0
    while True:
        value = value + step
        if value <= limit:
            continue
        break
    return value


def count_inner_breaks() -> int:
    # `break` only exits the innermost loop.
    count = 0
    for i in range(3):
        for j in range(3):
            if j == 1:
                break
            count = count + 1
    return count


def main():
    print(sum_until_five())
    print(sum_odds_below_ten())
    print(first_multiple_over(10, 3))
    print(count_inner_breaks())


if __name__ == "__main__":
    main()
