"""datetime's constants. Constructing and reading dates and times is tested
in tests/unit/stdlib.rs."""

import datetime


def test_datetime_constants() -> int:
    return datetime.MAXYEAR


def year_span() -> int:
    return datetime.MAXYEAR - datetime.MINYEAR
