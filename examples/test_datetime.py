"""datetime's constants. Constructing and reading dates and times is not
implemented yet (planned for 0.20.0), so those calls are compile errors."""

import datetime


def test_datetime_constants() -> int:
    return datetime.MAXYEAR


def year_span() -> int:
    return datetime.MAXYEAR - datetime.MINYEAR
