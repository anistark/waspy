"""Grid algorithms: Conway's Game of Life and a breadth-first maze solver.

The fifth of the end-to-end programs. It was written as ordinary Python,
without reference to what waspy supports, and compiled unchanged. It found
more than any program before it, nearly all of it silent and most of it
foundational: a collection literal was one shared object per source line (so
a function called twice returned the same list), integer `//` and `%`
truncated instead of flooring, `op=` had its own wrong arithmetic, tuples and
lists compared by identity (so the maze search below never terminated: no
coordinate ever matched the goal or a visited cell), unpacked tuple members
were untyped, and mutating a module-level collection was lost. All of it is
fixed, and this program is asserted against CPython.

The shape: nested lists mutated in place, modular neighbour arithmetic with
negative offsets, tuples as coordinates in a set and in comparisons, a list
used as a queue, and module-level constant tables.
"""

from typing import List, Set, Tuple


def make_board(rows: List[str]) -> List[List[int]]:
    board: List[List[int]] = []
    for row in rows:
        cells: List[int] = []
        for ch in row:
            if ch == "#":
                cells.append(1)
            else:
                cells.append(0)
        board.append(cells)
    return board


def neighbours(board: List[List[int]], r: int, c: int) -> int:
    height = len(board)
    width = len(board[0])
    count = 0
    for dr in [-1, 0, 1]:
        for dc in [-1, 0, 1]:
            if dr == 0 and dc == 0:
                continue
            rr = (r + dr) % height
            cc = (c + dc) % width
            count += board[rr][cc]
    return count


def step(board: List[List[int]]) -> List[List[int]]:
    height = len(board)
    width = len(board[0])
    nxt: List[List[int]] = []
    for r in range(height):
        row: List[int] = []
        for c in range(width):
            n = neighbours(board, r, c)
            alive = board[r][c] == 1
            if alive and (n == 2 or n == 3):
                row.append(1)
            elif not alive and n == 3:
                row.append(1)
            else:
                row.append(0)
        nxt.append(row)
    return nxt


def population(board: List[List[int]]) -> int:
    total = 0
    for row in board:
        total += sum(row)
    return total


def render(board: List[List[int]]) -> str:
    lines: List[str] = []
    for row in board:
        line = ""
        for cell in row:
            if cell == 1:
                line += "#"
            else:
                line += "."
        lines.append(line)
    return "/".join(lines)


GLIDER = [
    ".#....",
    "..#...",
    "###...",
    "......",
    "......",
    "......",
]


def glider_population_after(generations: int) -> int:
    board = make_board(GLIDER)
    for _ in range(generations):
        board = step(board)
    return population(board)


def glider_after_four() -> str:
    board = make_board(GLIDER)
    for _ in range(4):
        board = step(board)
    return render(board)


def blinker_period() -> int:
    board = make_board([".....", "..#..", "..#..", "..#..", "....."])
    start = render(board)
    board = step(board)
    changed = render(board) != start
    board = step(board)
    back = render(board) == start
    if changed and back:
        return 2
    return 0


MAZE = [
    "S.#.....",
    ".##.###.",
    "....#...",
    "##.##.#.",
    "...#..#E",
]


def find(maze: List[str], target: str) -> Tuple[int, int]:
    for r in range(len(maze)):
        for c in range(len(maze[r])):
            if maze[r][c] == target:
                return (r, c)
    return (-1, -1)


def shortest_path(maze: List[str]) -> int:
    start = find(maze, "S")
    goal = find(maze, "E")
    seen: Set[Tuple[int, int]] = {start}
    queue: List[Tuple[int, int, int]] = [(start[0], start[1], 0)]
    while len(queue) > 0:
        r, c, dist = queue.pop(0)
        if (r, c) == goal:
            return dist
        for dr, dc in [(1, 0), (-1, 0), (0, 1), (0, -1)]:
            nr = r + dr
            nc = c + dc
            if nr < 0 or nr >= len(maze) or nc < 0 or nc >= len(maze[0]):
                continue
            if maze[nr][nc] == "#":
                continue
            if (nr, nc) in seen:
                continue
            seen.add((nr, nc))
            queue.append((nr, nc, dist + 1))
    return -1


def maze_distance() -> int:
    return shortest_path(MAZE)


def blocked_maze() -> int:
    return shortest_path(["S#E"])


if __name__ == "__main__":
    print(glider_population_after(0), glider_population_after(4), glider_after_four())
    print(blinker_period(), maze_distance(), blocked_maze())
