"""Summarize a block of text: tokenize it, count the words, rank them, and
format a report.

A whole program rather than a feature demo: a word-frequency tool of the kind
anyone would write, exercising string methods on runtime strings, dict
accumulation, sorting, comprehensions, and fixed-point formatting all at once.
It was written without reference to what the compiler supported and then made
to run, which is how it earned its keep: composing those features turned up a
dozen defects that no single-feature test had caught.

Two things about the style are load-bearing rather than incidental:

- The container annotations are parameterised (`List[str]`, not a bare `list`).
  A bare `list` carries no element type, so the words in it would be compared as
  untyped words and the counts would not deduplicate.
- `top_words` sorts an explicit list of `(-count, word)` tuples rather than
  writing `sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))`. A lambda
  parameter carries no type yet, so indexing one inside a key reads the value
  wrongly; the compiler refuses such a key rather than miscompiling it. Sorting
  the tuples directly gives the same ordering.

`main()` returns exactly what CPython prints for the same source:

    23 words, 14 unique
    longest=quick avg=3.43
    fox: 4
    the: 4
    a: 2
"""

from typing import Dict, List, Tuple

SAMPLE = (
    "the quick brown fox jumps over the lazy dog. "
    "The dog barks, and the fox runs! "
    "A quick fox is a happy fox."
)


def normalize(word: str) -> str:
    """Strip surrounding punctuation and case-fold."""
    return word.strip(".,!?;:\"'()").lower()


def tokenize(text: str) -> List[str]:
    words = []
    for raw in text.split():
        w = normalize(raw)
        if w:
            words.append(w)
    return words


def count_words(words: List[str]) -> Dict[str, int]:
    counts = {}
    for w in words:
        counts[w] = counts.get(w, 0) + 1
    return counts


def top_words(counts: Dict[str, int], n: int) -> List[Tuple[int, str]]:
    """Most frequent first, ties broken alphabetically.

    Written with an explicit (-count, word) list rather than
    `sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))`, because a lambda
    parameter carries no type yet, so indexing one inside a key reads the value
    wrongly. Sorting tuples directly gives the same ordering: the negated count
    sorts descending and the word breaks ties alphabetically.
    """
    ranked: List[Tuple[int, str]] = []
    for word in counts:
        ranked.append((-counts[word], word))
    return sorted(ranked)[:n]


def longest_word(words: List[str]) -> str:
    best = ""
    for w in words:
        if len(w) > len(best):
            best = w
    return best


def average_length(words: List[str]) -> float:
    if not words:
        return 0.0
    total = sum(len(w) for w in words)
    return total / len(words)


def format_report(text: str, n: int) -> str:
    words = tokenize(text)
    counts = count_words(words)
    lines = [f"{word}: {-count}" for count, word in top_words(counts, n)]
    header = f"{len(words)} words, {len(counts)} unique"
    stats = f"longest={longest_word(words)} avg={average_length(words):.2f}"
    return header + "\n" + stats + "\n" + "\n".join(lines)


def unique_count(text: str) -> int:
    return len(count_words(tokenize(text)))


def word_count(text: str) -> int:
    return len(tokenize(text))


def main() -> str:
    return format_report(SAMPLE, 3)


if __name__ == "__main__":
    print(main())
