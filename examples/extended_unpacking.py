# Extended (starred) unpacking: a, *b, c = xs binds b to the middle slice.
# Each function returns an i32 checked by the integration tests.


def star_in_middle() -> int:
    a, *b, c = [1, 2, 3, 4]
    return a * 100 + len(b) * 10 + c  # 124


def star_collects_values() -> int:
    a, *b, c = [1, 2, 3, 4]
    return b[0] * 10 + b[1]  # 23


def star_at_front() -> int:
    *xs, last = [5, 6, 7]
    return len(xs) * 100 + xs[0] * 10 + last  # 257


def star_at_back() -> int:
    first, *rest = [9, 8, 7, 6]
    return first * 100 + len(rest) * 10 + rest[2]  # 936


def star_binds_empty() -> int:
    a, *mid, b = [1, 2]
    return a * 100 + len(mid) * 10 + b  # 102


def star_from_tuple() -> int:
    a, *b, c = (10, 20, 30, 40)
    return a + b[0] + b[1] + c  # 100


def star_list_iterates() -> int:
    first, *rest = [1, 2, 3, 4, 5]
    total = 0
    for v in rest:
        total = total + v
    return first * 100 + total  # 114
