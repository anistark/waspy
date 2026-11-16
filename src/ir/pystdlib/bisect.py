# bisect, CPython 3.13's pure-Python algorithms (Lib/bisect.py).

from typing import Optional


def bisect_right(a, x, lo=0, hi: Optional[int] = None):
    if lo < 0:
        raise ValueError("lo must be non-negative")
    if hi is None:
        hi = len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        if x < a[mid]:
            hi = mid
        else:
            lo = mid + 1
    return lo


def bisect_left(a, x, lo=0, hi: Optional[int] = None):
    if lo < 0:
        raise ValueError("lo must be non-negative")
    if hi is None:
        hi = len(a)
    while lo < hi:
        mid = (lo + hi) // 2
        if a[mid] < x:
            lo = mid + 1
        else:
            hi = mid
    return lo


def insort_right(a, x, lo=0, hi: Optional[int] = None):
    lo = bisect_right(a, x, lo, hi)
    a.insert(lo, x)


def insort_left(a, x, lo=0, hi: Optional[int] = None):
    lo = bisect_left(a, x, lo, hi)
    a.insert(lo, x)


def bisect(a, x, lo=0, hi: Optional[int] = None):
    return bisect_right(a, x, lo, hi)


def insort(a, x, lo=0, hi: Optional[int] = None):
    insort_right(a, x, lo, hi)
