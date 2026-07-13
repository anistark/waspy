# Comprehensions: list/set/dict, filters, multiple generators, nesting.
# Each function returns an i32 checked by the integration tests.


def list_comp_basic() -> int:
    xs = [1, 2, 3, 4]
    ys = [x * 2 for x in xs]
    return ys[0] + ys[1] + ys[2] + ys[3]  # 20


def list_comp_filter() -> int:
    ys = [x for x in [1, 2, 3, 4, 5, 6] if x % 2 == 0]
    return len(ys) * 100 + ys[0] + ys[1] + ys[2]  # 312


def list_comp_range() -> int:
    ys = [x * x for x in range(5)]
    return ys[4] * 10 + len(ys)  # 165


def list_comp_descending_range() -> int:
    ys = [x for x in range(5, 0, -1)]
    return len(ys) * 100 + ys[0] * 10 + ys[4]  # 551


def comp_scope_does_not_leak() -> int:
    x = 100
    ys = [x * 2 for x in [1, 2, 3]]
    return x + ys[0]  # 102: the comprehension x must not clobber the local


def float_comp() -> int:
    ys = [x + 0.5 for x in [1.5, 2.5, 3.5]]
    return int(ys[0] + ys[1] + ys[2])  # 2 + 3 + 4 = 9


def dict_comp() -> int:
    d = {k: k * k for k in [1, 2, 3]}
    return d[3] * 10 + d[2]  # 94


def dict_comp_filter() -> int:
    d = {k: k + 10 for k in range(6) if k % 2 == 1}
    return len(d) * 100 + d[5]  # 315


def dict_comp_unpacks_pairs() -> int:
    items = [(1, 10), (2, 20), (3, 30)]
    d = {k: v for k, v in items}
    return d[2] + d[3]  # 50


def set_comp_dedups() -> int:
    s = {x % 3 for x in [0, 1, 2, 3, 4, 5]}
    return len(s)  # 3


def set_comp_filter_membership() -> int:
    s = {x for x in range(10) if x > 6}
    a = 0
    if 7 in s:
        a = a + 1
    if 3 in s:
        a = a + 10
    return len(s) * 100 + a  # 301


def multi_generator_flatten() -> int:
    m = [[1, 2], [3, 4], [5, 6]]
    flat = [x for row in m for x in row]
    return len(flat) * 100 + flat[0] + flat[5]  # 607


def multi_generator_with_filter() -> int:
    pairs = [x * 10 + y for x in range(3) if x > 0 for y in range(2)]
    return len(pairs) * 100 + pairs[0] + pairs[3]  # 431


def comp_inside_comp() -> int:
    rows = [[y * 3 for y in range(3)] for x in range(2)]
    return rows[1][2] + rows[0][1]  # 6 + 3 = 9


def comp_as_loop_iterable() -> int:
    total = 0
    for v in [x * x for x in range(4)]:
        total = total + v
    return total  # 14


def comp_rebuilt_per_iteration() -> int:
    total = 0
    for i in range(3):
        ys = [i * 10 + j for j in range(2)]
        total = total + ys[0] + ys[1]
    return total  # 1 + 21 + 41 = 63
