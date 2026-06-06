# Float collections round-trip through the one-word-per-element layout
# (floats are stored as f32). See `just compile examples/list_float.py`.


def midpoint() -> float:
    pts = [1.5, 2.5, 3.5]
    return pts[1]


def total() -> float:
    xs = [10.5, 20.25, 30.125]
    return xs[0] + xs[1] + xs[2]


def pair_second() -> float:
    p = (1.25, 2.75)
    return p[1]


def int_list_index() -> int:
    xs = [10, 20, 30]
    return xs[1]
