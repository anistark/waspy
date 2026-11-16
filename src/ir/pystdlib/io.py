# io.StringIO, an in-memory text stream (CPython 3.13's semantics: a write at
# the position overwrites, reading advances the position).

from typing import Optional


class StringIO:
    def __init__(self, initial_value: str = "", newline: str = "\n"):
        self._buf: str = initial_value
        self._pos: int = 0
        self.closed: bool = False

    def _check(self) -> None:
        if self.closed:
            raise ValueError("I/O operation on closed file.")

    def write(self, s: str) -> int:
        self._check()
        if s == "":
            return 0
        end = self._pos + len(s)
        if self._pos > len(self._buf):
            self._buf = self._buf + "\0" * (self._pos - len(self._buf))
        self._buf = self._buf[: self._pos] + s + self._buf[end:]
        self._pos = end
        return len(s)

    def writelines(self, lines: list[str]) -> None:
        for line in lines:
            self.write(line)

    def getvalue(self) -> str:
        self._check()
        return self._buf

    def read(self, size: int = -1) -> str:
        self._check()
        if size < 0:
            out = self._buf[self._pos:]
        else:
            out = self._buf[self._pos : self._pos + size]
        self._pos += len(out)
        return out

    def readline(self, size: int = -1) -> str:
        self._check()
        if self._pos >= len(self._buf):
            return ""
        end = self._buf.find("\n", self._pos)
        if end < 0:
            end = len(self._buf)
        else:
            end += 1
        if size >= 0 and self._pos + size < end:
            end = self._pos + size
        out = self._buf[self._pos : end]
        self._pos = end
        return out

    def readlines(self) -> list[str]:
        lines: list[str] = []
        while True:
            line = self.readline()
            if line == "":
                break
            lines.append(line)
        return lines

    def seek(self, pos: int, whence: int = 0) -> int:
        self._check()
        if whence == 0:
            if pos < 0:
                raise ValueError("Negative seek position " + str(pos))
            self._pos = pos
        elif whence == 2:
            self._pos = len(self._buf)
        return self._pos

    def tell(self) -> int:
        self._check()
        return self._pos

    def truncate(self, size: Optional[int] = None) -> int:
        self._check()
        n = self._pos if size is None else size
        self._buf = self._buf[:n]
        return n

    def close(self) -> None:
        self.closed = True

    def __iter__(self) -> "StringIO":
        return self

    def __next__(self) -> str:
        line = self.readline()
        if line == "":
            raise StopIteration
        return line

    def __enter__(self) -> "StringIO":
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        self.close()
        return False
