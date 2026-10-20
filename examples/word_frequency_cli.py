"""A command-line word counter: reads text on standard input and prints the
most frequent words as a table, the count of them given as its argument.

    python3 word_frequency_cli.py 5 < essay.txt

Compiled with waspy it is a WASI command: `wasmtime run` runs the
`__main__` block, `print` writes to standard output, the summary goes to
standard error, and `sys.argv` carries the argument.
"""

import sys
from typing import Dict, List, Tuple


def words_of(line: str) -> List[str]:
    found: List[str] = []
    for raw in line.split():
        word = raw.strip(".,;:!?\"'()").lower()
        if word:
            found.append(word)
    return found


def count(lines: List[str]) -> Dict[str, int]:
    counts: Dict[str, int] = {}
    for line in lines:
        for word in words_of(line):
            counts[word] = counts.get(word, 0) + 1
    return counts


def top(counts: Dict[str, int], n: int) -> List[Tuple[str, int]]:
    pairs: List[Tuple[str, int]] = [(word, counts[word]) for word in counts]
    pairs = sorted(pairs, key=lambda p: p[0])
    pairs = sorted(pairs, key=lambda p: p[1], reverse=True)
    return pairs[:n]


def main() -> None:
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    lines = sys.stdin.readlines()
    counts = count(lines)
    total = sum(counts.values())
    print(f"{'word':<12}{'count':>6}{'share':>9}")
    print("-" * 27)
    for word, c in top(counts, n):
        print(f"{word:<12}{c:>6}{c / total:>9.1%}")
    print("-" * 27)
    print(f"{'total':<12}{total:>6,}")
    print(f"{len(lines)} lines, {len(counts)} distinct words", file=sys.stderr)


if __name__ == "__main__":
    main()
