# time: the clocks and sleep, over WASI's clock_time_get and poll_oneoff.
# There is no time zone on WASI, so local time is UTC, as CPython's is there.

import datetime
from typing import NamedTuple, Optional


class struct_time(NamedTuple):
    tm_year: int
    tm_mon: int
    tm_mday: int
    tm_hour: int
    tm_min: int
    tm_sec: int
    tm_wday: int
    tm_yday: int
    tm_isdst: int

    def __repr__(self) -> str:
        return ("time.struct_time(tm_year=" + str(self.tm_year) + ", tm_mon=" + str(self.tm_mon)
                + ", tm_mday=" + str(self.tm_mday) + ", tm_hour=" + str(self.tm_hour)
                + ", tm_min=" + str(self.tm_min) + ", tm_sec=" + str(self.tm_sec)
                + ", tm_wday=" + str(self.tm_wday) + ", tm_yday=" + str(self.tm_yday)
                + ", tm_isdst=" + str(self.tm_isdst) + ")")


def time() -> float:
    return __datetime_clock_us() / 1000000.0


def monotonic() -> float:
    return __asyncio_clock() / 1000000000.0


def perf_counter() -> float:
    return __asyncio_clock() / 1000000000.0


def process_time() -> float:
    return __asyncio_clock() / 1000000000.0


def sleep(secs: float) -> None:
    if secs < 0:
        raise ValueError("sleep length must be non-negative")
    __asyncio_wait(__asyncio_ns(secs))


def _struct(d: datetime.datetime) -> struct_time:
    yday = d.toordinal() - datetime.date(d.year, 1, 1).toordinal() + 1
    return struct_time(d.year, d.month, d.day, d.hour, d.minute, d.second, d.weekday(), yday, 0)


def gmtime(secs: Optional[float] = None) -> struct_time:
    t = time()
    if secs is not None:
        t = secs
    return _struct(datetime.datetime.fromtimestamp(float(int(t)), datetime.timezone.utc))


def localtime(secs: Optional[float] = None) -> struct_time:
    return gmtime(secs)


def strftime(format: str, t: Optional[struct_time] = None) -> str:
    st: struct_time = localtime()
    if t is not None:
        st = t
    d = datetime.datetime(st.tm_year, st.tm_mon, st.tm_mday, st.tm_hour, st.tm_min, st.tm_sec)
    return d.strftime(format)
