# pathlib.Path on POSIX: CPython 3.13's PurePosixPath parsing and the file
# operations WASI serves, over os and open().

import os


def _split(path: str) -> tuple[str, list[str]]:
    # posixpath.splitroot, then the tail without empty or '.' segments.
    if path[:1] != "/":
        root = ""
        rel = path
    elif path[1:2] != "/" or path[2:3] == "/":
        root = "/"
        rel = path[1:]
    else:
        root = path[:2]
        rel = path[2:]
    tail: list[str] = []
    for x in rel.split("/"):
        if x and x != ".":
            tail.append(x)
    return root, tail


def _join(a: str, b: str) -> str:
    # posixpath.join of two
    if b.startswith("/"):
        return b
    if not a or a.endswith("/"):
        return a + b
    return a + "/" + b


class Path:
    def __init__(self, *args: str):
        raw = ""
        for a in args:
            raw = _join(raw, a)
        root, tail = _split(raw)
        self._root: str = root
        self._tail: list[str] = tail

    def __str__(self) -> str:
        if self._root:
            return self._root + "/".join(self._tail)
        joined = "/".join(self._tail)
        return joined if joined else "."

    def __repr__(self) -> str:
        return "PosixPath(" + repr(str(self)) + ")"

    def __fspath__(self) -> str:
        return str(self)

    def __truediv__(self, key: str) -> "Path":
        return Path(str(self), key)

    def __eq__(self, other: "Path") -> bool:
        return str(self) == str(other)

    def __lt__(self, other: "Path") -> bool:
        return self.parts < other.parts

    def __hash__(self) -> int:
        return hash(str(self))

    def joinpath(self, a: str, b: str = "", c: str = "", d: str = "") -> "Path":
        return Path(str(self), a, b, c, d)

    @property
    def root(self) -> str:
        return self._root

    @property
    def anchor(self) -> str:
        return self._root

    @property
    def drive(self) -> str:
        return ""

    @property
    def parts(self) -> tuple[str, ...]:
        out: list[str] = []
        if self._root:
            out.append(self._root)
        for t in self._tail:
            out.append(t)
        return tuple(out)

    @property
    def name(self) -> str:
        if not self._tail:
            return ""
        return self._tail[-1]

    @property
    def suffix(self) -> str:
        name = self.name
        i = name.rfind(".")
        if 0 < i < len(name) - 1:
            return name[i:]
        return ""

    @property
    def suffixes(self) -> list[str]:
        name = self.name
        if name.endswith("."):
            return []
        name = name.lstrip(".")
        return ["." + s for s in name.split(".")[1:]]

    @property
    def stem(self) -> str:
        name = self.name
        i = name.rfind(".")
        if 0 < i < len(name) - 1:
            return name[:i]
        return name

    @property
    def parent(self) -> "Path":
        if not self._tail:
            return self
        p = Path()
        p._root = self._root
        p._tail = self._tail[:-1]
        return p

    def is_absolute(self) -> bool:
        return self._root != ""

    def with_name(self, name: str) -> "Path":
        if not name or "/" in name or name == ".":
            raise ValueError("Invalid name " + repr(name))
        if not self._tail:
            raise ValueError(repr(self) + " has an empty name")
        p = Path()
        p._root = self._root
        p._tail = self._tail[:-1] + [name]
        return p

    def with_stem(self, stem: str) -> "Path":
        suffix = self.suffix
        if not suffix:
            return self.with_name(stem)
        if not stem:
            raise ValueError(repr(self) + " has a non-empty suffix")
        return self.with_name(stem + suffix)

    def with_suffix(self, suffix: str) -> "Path":
        stem = self.stem
        if not stem:
            raise ValueError(repr(self) + " has an empty name")
        if suffix and not suffix.startswith("."):
            raise ValueError("Invalid suffix " + repr(suffix))
        if suffix == ".":
            raise ValueError("Invalid suffix " + repr(suffix))
        return self.with_name(stem + suffix)

    def relative_to(self, other: str) -> "Path":
        base = Path(other)
        n = len(base._tail)
        if base._root != self._root or base._tail != self._tail[:n]:
            raise ValueError(repr(str(self)) + " is not in the subpath of " + repr(str(base)))
        p = Path()
        p._tail = self._tail[n:]
        return p

    def is_relative_to(self, other: str) -> bool:
        base = Path(other)
        n = len(base._tail)
        return base._root == self._root and base._tail == self._tail[:n]

    def exists(self) -> bool:
        return os.path.exists(str(self))

    def is_file(self) -> bool:
        return os.path.isfile(str(self))

    def is_dir(self) -> bool:
        return os.path.isdir(str(self))

    def read_text(self) -> str:
        with open(str(self)) as f:
            return f.read()

    def write_text(self, data: str) -> int:
        with open(str(self), "w") as f:
            f.write(data)
        return len(data)

    def iterdir(self) -> list["Path"]:
        out: list[Path] = []
        for name in os.listdir(str(self)):
            out.append(self / name)
        return out

    def unlink(self, missing_ok: bool = False) -> None:
        if missing_ok and not os.path.exists(str(self)):
            return
        os.remove(str(self))

    def rename(self, target: str) -> "Path":
        os.rename(str(self), target)
        return Path(target)
