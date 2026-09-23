

# ---------------------------------------------------------------------------
# Runtime checks, appended to examples/grid_algorithms.py by
# examples/verify_runtime.rs
# ---------------------------------------------------------------------------
#
# Every value below is what CPython answers for the same source. GLIDER and
# MAZE are module-level lists, so this is the program that runs the start
# function (module definitions evaluated once into globals) under both
# engines, optimized and unoptimized. A `runtime_check_negative_` function
# must answer 0.


def runtime_check_glider_population() -> int:
    if glider_population_after(0) != 5:
        return 0
    if glider_population_after(4) != 5:
        return 0
    return 1


def runtime_check_glider_after_four() -> int:
    if glider_after_four() == "....../..#.../...#../.###../....../......":
        return 1
    return 0


def runtime_check_blinker_period() -> int:
    if blinker_period() == 2:
        return 1
    return 0


def runtime_check_maze_distance() -> int:
    if maze_distance() == 15:
        return 1
    return 0


def runtime_check_blocked_maze() -> int:
    if blocked_maze() == -1:
        return 1
    return 0


def runtime_check_negative_maze_distance() -> int:
    if maze_distance() == 14:
        return 1
    return 0
