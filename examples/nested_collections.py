# Nested collections, per-iteration memory correctness, and float dict/set
# values. Exercises the v0.12.0 collection-robustness work (Issue #14).


def nested_grid() -> int:
    # A statically nested list-of-lists: each inner literal gets its own
    # region, so indexing reaches the right element. grid[0][1] + grid[1][0].
    grid = [[1, 2], [3, 4]]
    return grid[0][1] + grid[1][0]


def loop_escape() -> int:
    # A list literal built inside a loop that escapes the loop body. Each
    # iteration must get its own region, otherwise every stored row aliases the
    # last one written. With distinct regions grid[0][0]==0 and grid[2][0]==2,
    # so this is 0*100 + 2 == 2 (aliasing would give 202).
    grid = [[0, 0], [0, 0], [0, 0]]
    for i in range(3):
        grid[i] = [i, i + 10]
    return grid[0][0] * 100 + grid[2][0]


def float_dict_sum() -> int:
    # Float dict values round-trip through the one-word slot (stored as f32),
    # so d[1] + d[2] == 11.0 and int() truncates to 11.
    d = {1: 3.5, 2: 7.5}
    return int(d[1] + d[2])


def float_set_size() -> int:
    # Float set members de-duplicate by value: {1.5, 1.5, 2.5} has two members.
    s = {1.5, 1.5, 2.5}
    return len(s)


def float_list_roundtrip() -> float:
    # Pi to full f64 precision survives storage in and retrieval from a list.
    # An f32 slot would round it to ~3.1415927 and fail an exact compare.
    xs = [3.141592653589793, 2.718281828459045]
    return xs[0]


def float_list_sum() -> float:
    # 0.1 + 0.2 == 0.30000000000000004 in f64; the famous low bits only survive
    # if both operands keep their full f64 representation in the list slots.
    xs = [0.1, 0.2]
    return xs[0] + xs[1]


def float_dict_precise() -> float:
    # Dict values keep full f64 precision through the linear-search lookup.
    d = {1: 3.141592653589793}
    return d[1]


def float_tuple_roundtrip() -> float:
    # A float tuple member round-trips losslessly. (Tuple indexing types every
    # element as the first element's type, so the tuple is kept homogeneous.)
    t = (1.0, 3.141592653589793)
    return t[1]


def float_membership() -> int:
    # `in` over a float list compares by value at full width.
    xs = [1.5, 2.5, 3.5]
    if 2.5 in xs:
        return 1
    return 0


def float_loop_sum() -> float:
    # Iterating a float list binds each element as an f64 loop variable.
    total = 0.0
    for x in [0.1, 0.2, 0.3]:
        total = total + x
    return total


def int_set_dedup() -> int:
    # The set hash table drops duplicates on insert: {1, 2, 2, 3, 1} -> 3.
    s = {1, 2, 2, 3, 1}
    return len(s)


def set_membership() -> int:
    # `in` / `not in` probe the hash table.
    s = {1, 3, 5, 7}
    found = 0
    if 5 in s:
        found = found + 1
    if 4 not in s:
        found = found + 1
    return found


def set_collision_probe() -> int:
    # With capacity 8, the values 0, 8 and 16 all hash to bucket 0, so insertion
    # and lookup must walk the linear probe chain. All three stay distinct and
    # are found; 24 (also bucket 0) is absent. Result: 3*10 + 1 + 1 == 32.
    s = {0, 8, 16}
    result = len(s) * 10
    if 8 in s:
        result = result + 1
    if 24 not in s:
        result = result + 1
    return result


def float_set_membership() -> int:
    # Float set members are hashed and compared at full f64 width.
    s = {1.5, 2.5, 3.5}
    if 2.5 in s:
        return 1
    return 0


def set_loop_fresh() -> int:
    # A set rebuilt each iteration must start empty (stale bucket state from the
    # previous iteration is cleared), so each pass counts exactly two distinct
    # members: 2 * 3 == 6.
    total = 0
    for i in range(3):
        s = {i, i + 100}
        total = total + len(s)
    return total


def main():
    print(nested_grid())
    print(loop_escape())
    print(float_dict_sum())
    print(float_set_size())


if __name__ == "__main__":
    main()
