"""A gradebook: reads `name, subject, score` lines from standard input and
reports each student's average and letter grade, the leader of each subject,
and a JSON summary, listing the lines it could not read.

A whole program rather than a feature demo, written the way the tool would be
written in Python without reference to what the compiler supported, then made
to run. It found four gaps on the way: chained comparisons (`0 <= x <= 100`)
were refused, an int compared with a float on its left failed validation, a
`defaultdict` field annotated `Dict[...]` was refused, and a local computed
from a variable a lambda captures (`sorted(d, key=lambda k: d[k])`) was not
typed.

Run it as a WASI command:

    just compile examples/gradebook.py
    wasmtime run examples/output/gradebook.wasm < grades.txt
"""

import json
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass
from typing import Dict, List, Optional


class BadRecord(ValueError):
    pass


@dataclass
class Score:
    student: str
    subject: str
    points: float


def parse_line(line: str, lineno: int) -> Score:
    parts = [p.strip() for p in line.split(",")]
    if len(parts) != 3:
        raise BadRecord(f"line {lineno}: expected 3 fields, got {len(parts)}")
    name, subject, raw = parts
    try:
        points = float(raw)
    except ValueError:
        raise BadRecord(f"line {lineno}: bad score {raw!r}")
    if not 0 <= points <= 100:
        raise BadRecord(f"line {lineno}: score {points} out of range")
    return Score(name.title(), subject.lower(), points)


def letter(avg: float) -> str:
    for cutoff, grade in [(90, "A"), (80, "B"), (70, "C"), (60, "D")]:
        if avg >= cutoff:
            return grade
    return "F"


class Gradebook:
    def __init__(self) -> None:
        self.by_student: Dict[str, List[Score]] = defaultdict(list)
        self.errors: List[str] = []

    def add(self, score: Score) -> None:
        self.by_student[score.student].append(score)

    def average(self, student: str) -> float:
        scores = self.by_student[student]
        return sum(s.points for s in scores) / len(scores)

    def leader(self, subject: str) -> Optional[Score]:
        best: Optional[Score] = None
        for scores in self.by_student.values():
            for s in scores:
                if s.subject == subject and (best is None or s.points > best.points):
                    best = s
        return best

    def subjects(self) -> List[str]:
        seen: List[str] = []
        for scores in self.by_student.values():
            for s in scores:
                if s.subject not in seen:
                    seen.append(s.subject)
        return sorted(seen)


def report(book: Gradebook) -> str:
    lines: List[str] = []
    ranked = sorted(book.by_student, key=lambda n: (-book.average(n), n))
    for name in ranked:
        avg = book.average(name)
        lines.append(f"{name:<8} {avg:6.2f}  {letter(avg)}")
    grades = Counter(letter(book.average(n)) for n in ranked)
    lines.append("grades: " + ", ".join(f"{g}={grades[g]}" for g in sorted(grades)))
    for subject in book.subjects():
        best = book.leader(subject)
        if best is not None:
            lines.append(f"top {subject}: {best.student} ({best.points:g})")
    summary = {name: round(book.average(name), 1) for name in ranked}
    lines.append(json.dumps(summary, sort_keys=True))
    if book.errors:
        lines.append(f"{len(book.errors)} bad line(s):")
        lines.extend("  " + e for e in book.errors)
    return "\n".join(lines)


def main() -> None:
    book = Gradebook()
    for lineno, line in enumerate(sys.stdin, start=1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        try:
            book.add(parse_line(line, lineno))
        except BadRecord as e:
            book.errors.append(str(e))
    print(report(book))


if __name__ == "__main__":
    main()
