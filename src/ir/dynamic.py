# Python's dynamic values, for the places static types run out: a `typing.Any`
# annotation, a collection literal whose elements differ in type
# (`{"name": "bob", "age": 30}`), and JSON parsed into `Any`. Written in the
# supported subset and grafted into a program that uses it (`ir::dynamic`),
# so it is compiled like the program's own code.
#
# A value is an instance of one class per kind, and every operation is a
# method each kind answers as CPython's built-in type does, with CPython's
# errors for what the type does not do. A list or dict holds `_Any` values,
# and is the very object the program holds, so a change through one name is
# seen through the other.

K_NONE = 0
K_BOOL = 1
K_INT = 2
K_FLOAT = 3
K_STR = 4
K_LIST = 5
K_DICT = 6
K_TUPLE = 7


class _Any:
    # None. The base of every kind, answering CPython's error for whatever a
    # kind does not do.
    def kind(self) -> int:
        return K_NONE

    def type_name(self) -> str:
        return "NoneType"

    def text(self) -> str:
        return "None"

    def show(self) -> str:
        return self.text()

    def __repr__(self) -> str:
        return self.text()

    def __str__(self) -> str:
        return self.show()

    def truth(self) -> bool:
        return False

    def __bool__(self) -> bool:
        return self.truth()

    def is_number(self) -> bool:
        return False

    def num(self) -> float:
        return 0.0

    def whole(self) -> int:
        return 0

    def text_or_empty(self) -> str:
        return ""

    def elements(self) -> list["_Any"]:
        empty: list[_Any] = []
        return empty

    def entries_or_empty(self) -> dict[str, "_Any"]:
        empty: dict[str, _Any] = {}
        return empty

    def declared(self, what: str) -> str:
        return "a " + self.type_name() + " is used where " + what + " is declared"

    def as_int(self) -> int:
        raise TypeError(self.declared("an int"))

    def as_bool(self) -> bool:
        raise TypeError(self.declared("a bool"))

    def as_float(self) -> float:
        raise TypeError(self.declared("a float"))

    def as_str(self) -> str:
        raise TypeError(self.declared("a str"))

    def as_list(self) -> list["_Any"]:
        raise TypeError(self.declared("a list"))

    def as_dict(self) -> dict[str, "_Any"]:
        raise TypeError(self.declared("a dict"))

    def as_none(self) -> int:
        raise TypeError(self.declared("None"))

    def isa(self, name: str) -> bool:
        return name == "NoneType"

    def no_attribute(self, name: str) -> str:
        return "'" + self.type_name() + "' object has no attribute '" + name + "'"

    def __len__(self) -> int:
        raise TypeError("object of type '" + self.type_name() + "' has no len()")

    def iter_items(self) -> list["_Any"]:
        raise TypeError("'" + self.type_name() + "' object is not iterable")

    def __getitem__(self, key: "_Any") -> "_Any":
        raise TypeError("'" + self.type_name() + "' object is not subscriptable")

    def __setitem__(self, key: "_Any", value: "_Any") -> None:
        raise TypeError("'" + self.type_name() + "' object does not support item assignment")

    def __contains__(self, item: "_Any") -> bool:
        raise TypeError("argument of type '" + self.type_name() + "' is not iterable")

    def __eq__(self, other: "_Any") -> bool:
        return other.kind() == K_NONE

    def __ne__(self, other: "_Any") -> bool:
        return not self.__eq__(other)

    def order(self, other: "_Any", op: str) -> int:
        raise TypeError("'" + op + "' not supported between instances of '" + self.type_name() + "' and '" + other.type_name() + "'")

    def __lt__(self, other: "_Any") -> bool:
        return self.order(other, "<") < 0

    def __le__(self, other: "_Any") -> bool:
        return self.order(other, "<=") <= 0

    def __gt__(self, other: "_Any") -> bool:
        return self.order(other, ">") > 0

    def __ge__(self, other: "_Any") -> bool:
        return self.order(other, ">=") >= 0

    def unsupported(self, op: str, other: "_Any") -> str:
        return "unsupported operand type(s) for " + op + ": '" + self.type_name() + "' and '" + other.type_name() + "'"

    def add(self, other: "_Any") -> "_Any":
        raise TypeError(self.unsupported("+", other))

    def __add__(self, other: "_Any") -> "_Any":
        return self.add(other)

    def __radd__(self, other: "_Any") -> "_Any":
        return other.add(self)

    def sub(self, other: "_Any") -> "_Any":
        if self.is_number() and other.is_number():
            return _any_arith(self, other, "-")
        raise TypeError(self.unsupported("-", other))

    def __sub__(self, other: "_Any") -> "_Any":
        return self.sub(other)

    def __rsub__(self, other: "_Any") -> "_Any":
        return other.sub(self)

    def mul(self, other: "_Any") -> "_Any":
        if self.is_number() and other.is_number():
            return _any_arith(self, other, "*")
        if other.kind() in (K_STR, K_LIST, K_TUPLE) and self.kind() in (K_INT, K_BOOL):
            return other.repeat(self.whole())
        raise TypeError(self.unsupported("*", other))

    def repeat(self, times: int) -> "_Any":
        raise TypeError("can't multiply sequence by non-int of type '" + self.type_name() + "'")

    def __mul__(self, other: "_Any") -> "_Any":
        return self.mul(other)

    def __rmul__(self, other: "_Any") -> "_Any":
        return other.mul(self)

    def truediv(self, other: "_Any") -> "_Any":
        if self.is_number() and other.is_number():
            return _any_arith(self, other, "/")
        raise TypeError(self.unsupported("/", other))

    def __truediv__(self, other: "_Any") -> "_Any":
        return self.truediv(other)

    def __rtruediv__(self, other: "_Any") -> "_Any":
        return other.truediv(self)

    def floordiv(self, other: "_Any") -> "_Any":
        if self.is_number() and other.is_number():
            return _any_arith(self, other, "//")
        raise TypeError(self.unsupported("//", other))

    def __floordiv__(self, other: "_Any") -> "_Any":
        return self.floordiv(other)

    def __rfloordiv__(self, other: "_Any") -> "_Any":
        return other.floordiv(self)

    def mod(self, other: "_Any") -> "_Any":
        if self.is_number() and other.is_number():
            return _any_arith(self, other, "%")
        raise TypeError(self.unsupported("%", other))

    def __mod__(self, other: "_Any") -> "_Any":
        return self.mod(other)

    def __rmod__(self, other: "_Any") -> "_Any":
        return other.mod(self)

    def power(self, other: "_Any") -> "_Any":
        if self.is_number() and other.is_number():
            return _any_arith(self, other, "**")
        raise TypeError(self.unsupported("** or pow()", other))

    def __pow__(self, other: "_Any") -> "_Any":
        return self.power(other)

    def __rpow__(self, other: "_Any") -> "_Any":
        return other.power(self)

    def __neg__(self) -> "_Any":
        raise TypeError("bad operand type for unary -: '" + self.type_name() + "'")

    def __abs__(self) -> "_Any":
        raise TypeError("bad operand type for abs(): '" + self.type_name() + "'")

    def get(self, key: "_Any", default: "_Any" = None) -> "_Any":
        raise AttributeError(self.no_attribute("get"))

    def keys(self) -> list["_Any"]:
        raise AttributeError(self.no_attribute("keys"))

    def values(self) -> list["_Any"]:
        raise AttributeError(self.no_attribute("values"))

    def items(self) -> list[tuple["_Any", "_Any"]]:
        raise AttributeError(self.no_attribute("items"))

    def setdefault(self, key: "_Any", default: "_Any" = None) -> "_Any":
        raise AttributeError(self.no_attribute("setdefault"))

    def append(self, value: "_Any") -> None:
        raise AttributeError(self.no_attribute("append"))

    def extend(self, values: "_Any") -> None:
        raise AttributeError(self.no_attribute("extend"))

    def pop(self) -> "_Any":
        raise AttributeError(self.no_attribute("pop"))

    def upper(self) -> "_Any":
        raise AttributeError(self.no_attribute("upper"))

    def lower(self) -> "_Any":
        raise AttributeError(self.no_attribute("lower"))

    def strip(self) -> "_Any":
        raise AttributeError(self.no_attribute("strip"))

    def split(self, sep: "_Any" = None) -> "_Any":
        raise AttributeError(self.no_attribute("split"))

    def startswith(self, prefix: "_Any") -> bool:
        raise AttributeError(self.no_attribute("startswith"))

    def endswith(self, suffix: "_Any") -> bool:
        raise AttributeError(self.no_attribute("endswith"))

    def replace(self, old: "_Any", new: "_Any") -> "_Any":
        raise AttributeError(self.no_attribute("replace"))

    def join(self, items: "_Any") -> "_Any":
        raise AttributeError(self.no_attribute("join"))

    def to_int(self) -> int:
        raise TypeError("int() argument must be a string, a bytes-like object or a real number, not '" + self.type_name() + "'")

    def to_float(self) -> float:
        raise TypeError("float() argument must be a string or a real number, not '" + self.type_name() + "'")

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        return "null"

    def format_with(self, spec: str) -> str:
        if spec == "":
            return self.show()
        raise TypeError("unsupported format string passed to " + self.type_name() + ".__format__")


class _AnyBool(_Any):
    def __init__(self, value: bool):
        self.value = value

    def kind(self) -> int:
        return K_BOOL

    def type_name(self) -> str:
        return "bool"

    def text(self) -> str:
        return "True" if self.value else "False"

    def truth(self) -> bool:
        return self.value

    def is_number(self) -> bool:
        return True

    def num(self) -> float:
        return 1.0 if self.value else 0.0

    def whole(self) -> int:
        return 1 if self.value else 0

    def as_bool(self) -> bool:
        return self.value

    def isa(self, name: str) -> bool:
        return name == "bool" or name == "int"

    def __eq__(self, other: _Any) -> bool:
        return other.is_number() and self.num() == other.num()

    def order(self, other: _Any, op: str) -> int:
        return _any_number_order(self, other, op)

    def add(self, other: _Any) -> _Any:
        if other.is_number():
            return _any_arith(self, other, "+")
        raise TypeError(self.unsupported("+", other))

    def __neg__(self) -> _Any:
        return _AnyInt(-self.whole())

    def __abs__(self) -> _Any:
        return _AnyInt(self.whole())

    def to_int(self) -> int:
        return self.whole()

    def to_float(self) -> float:
        return self.num()

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        return "true" if self.value else "false"

    def format_with(self, spec: str) -> str:
        if spec == "":
            return self.text()
        return format(self.whole(), spec)


class _AnyInt(_Any):
    def __init__(self, value: int):
        self.value = value

    def kind(self) -> int:
        return K_INT

    def type_name(self) -> str:
        return "int"

    def text(self) -> str:
        return repr(self.value)

    def truth(self) -> bool:
        return self.value != 0

    def is_number(self) -> bool:
        return True

    def num(self) -> float:
        return float(self.value)

    def whole(self) -> int:
        return self.value

    def as_int(self) -> int:
        return self.value

    def isa(self, name: str) -> bool:
        return name == "int"

    def __eq__(self, other: _Any) -> bool:
        if other.kind() == K_FLOAT:
            return float(self.value) == other.num()
        return other.is_number() and self.value == other.whole()

    def order(self, other: _Any, op: str) -> int:
        return _any_number_order(self, other, op)

    def add(self, other: _Any) -> _Any:
        if other.is_number():
            return _any_arith(self, other, "+")
        raise TypeError(self.unsupported("+", other))

    def __neg__(self) -> _Any:
        return _AnyInt(-self.value)

    def __abs__(self) -> _Any:
        return _AnyInt(abs(self.value))

    def to_int(self) -> int:
        return self.value

    def to_float(self) -> float:
        return float(self.value)

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        return repr(self.value)

    def format_with(self, spec: str) -> str:
        return format(self.value, spec)


class _AnyFloat(_Any):
    def __init__(self, value: float):
        self.value = value

    def kind(self) -> int:
        return K_FLOAT

    def type_name(self) -> str:
        return "float"

    def text(self) -> str:
        return repr(self.value)

    def truth(self) -> bool:
        return self.value != 0.0

    def is_number(self) -> bool:
        return True

    def num(self) -> float:
        return self.value

    def as_float(self) -> float:
        return self.value

    def isa(self, name: str) -> bool:
        return name == "float"

    def __eq__(self, other: _Any) -> bool:
        return other.is_number() and self.value == other.num()

    def order(self, other: _Any, op: str) -> int:
        return _any_number_order(self, other, op)

    def add(self, other: _Any) -> _Any:
        if other.is_number():
            return _any_arith(self, other, "+")
        raise TypeError(self.unsupported("+", other))

    def __neg__(self) -> _Any:
        return _AnyFloat(-self.value)

    def __abs__(self) -> _Any:
        return _AnyFloat(abs(self.value))

    def to_int(self) -> int:
        return int(self.value)

    def to_float(self) -> float:
        return self.value

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        if self.value != self.value:
            return "NaN"
        if self.value == float("inf"):
            return "Infinity"
        if self.value == -float("inf"):
            return "-Infinity"
        return repr(self.value)

    def format_with(self, spec: str) -> str:
        return format(self.value, spec)


class _AnyStr(_Any):
    def __init__(self, value: str):
        self.value = value

    def kind(self) -> int:
        return K_STR

    def type_name(self) -> str:
        return "str"

    def text(self) -> str:
        return repr(self.value)

    def show(self) -> str:
        return self.value

    def truth(self) -> bool:
        return len(self.value) > 0

    def text_or_empty(self) -> str:
        return self.value

    def as_str(self) -> str:
        return self.value

    def isa(self, name: str) -> bool:
        return name == "str"

    def __len__(self) -> int:
        return len(self.value)

    def iter_items(self) -> list[_Any]:
        return [_AnyStr(c) for c in self.value]

    def __getitem__(self, key: _Any) -> _Any:
        if key.kind() in (K_INT, K_BOOL):
            i = key.whole()
            n = len(self.value)
            if i < 0:
                i += n
            if i < 0 or i >= n:
                raise IndexError("string index out of range")
            return _AnyStr(self.value[i])
        raise TypeError("string indices must be integers, not '" + key.type_name() + "'")

    def __contains__(self, item: _Any) -> bool:
        if item.kind() != K_STR:
            raise TypeError("'in <string>' requires string as left operand, not " + item.type_name())
        return item.text_or_empty() in self.value

    def __eq__(self, other: _Any) -> bool:
        return other.kind() == K_STR and self.value == other.text_or_empty()

    def order(self, other: _Any, op: str) -> int:
        if other.kind() != K_STR:
            raise TypeError("'" + op + "' not supported between instances of 'str' and '" + other.type_name() + "'")
        theirs = other.text_or_empty()
        if self.value < theirs:
            return -1
        if self.value > theirs:
            return 1
        return 0

    def add(self, other: _Any) -> _Any:
        if other.kind() != K_STR:
            raise TypeError("can only concatenate str (not \"" + other.type_name() + "\") to str")
        return _AnyStr(self.value + other.text_or_empty())

    def repeat(self, times: int) -> _Any:
        return _AnyStr(self.value * times)

    def mul(self, other: _Any) -> _Any:
        if other.kind() in (K_INT, K_BOOL):
            return _AnyStr(self.value * other.whole())
        raise TypeError("can't multiply sequence by non-int of type '" + other.type_name() + "'")

    def upper(self) -> _Any:
        return _AnyStr(self.value.upper())

    def lower(self) -> _Any:
        return _AnyStr(self.value.lower())

    def strip(self) -> _Any:
        return _AnyStr(self.value.strip())

    def split(self, sep: _Any = None) -> _Any:
        parts: list[str] = []
        if sep.kind() == K_NONE:
            parts = self.value.split()
        else:
            parts = self.value.split(sep.as_str())
        return _AnyList([_AnyStr(p) for p in parts])

    def startswith(self, prefix: _Any) -> bool:
        return self.value.startswith(prefix.as_str())

    def endswith(self, suffix: _Any) -> bool:
        return self.value.endswith(suffix.as_str())

    def replace(self, old: _Any, new: _Any) -> _Any:
        return _AnyStr(self.value.replace(old.as_str(), new.as_str()))

    def join(self, items: _Any) -> _Any:
        parts: list[str] = []
        for item in items.iter_items():
            if item.kind() != K_STR:
                raise TypeError("sequence item " + str(len(parts)) + ": expected str instance, " + item.type_name() + " found")
            parts.append(item.text_or_empty())
        return _AnyStr(self.value.join(parts))

    def to_int(self) -> int:
        return int(self.value)

    def to_float(self) -> float:
        return float(self.value)

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        return _any_json_quote(self.value)

    def format_with(self, spec: str) -> str:
        return format(self.value, spec)


class _AnyList(_Any):
    def __init__(self, items: list[_Any]):
        self.items = items

    def kind(self) -> int:
        return K_LIST

    def type_name(self) -> str:
        return "list"

    def text(self) -> str:
        return "[" + ", ".join([x.text() for x in self.items]) + "]"

    def truth(self) -> bool:
        return len(self.items) > 0

    def elements(self) -> list[_Any]:
        return self.items

    def as_list(self) -> list[_Any]:
        return self.items

    def isa(self, name: str) -> bool:
        return name == "list"

    def __len__(self) -> int:
        return len(self.items)

    def iter_items(self) -> list[_Any]:
        return self.items

    def __getitem__(self, key: _Any) -> _Any:
        if key.kind() in (K_INT, K_BOOL):
            i = key.whole()
            n = len(self.items)
            if i < 0:
                i += n
            if i < 0 or i >= n:
                raise IndexError("list index out of range")
            return self.items[i]
        raise TypeError("list indices must be integers or slices, not " + key.type_name())

    def __setitem__(self, key: _Any, value: _Any) -> None:
        if key.kind() in (K_INT, K_BOOL):
            i = key.whole()
            n = len(self.items)
            if i < 0:
                i += n
            if i < 0 or i >= n:
                raise IndexError("list assignment index out of range")
            self.items[i] = value
            return
        raise TypeError("list indices must be integers or slices, not " + key.type_name())

    def __contains__(self, item: _Any) -> bool:
        for x in self.items:
            if x.__eq__(item):
                return True
        return False

    def __eq__(self, other: _Any) -> bool:
        if other.kind() != K_LIST:
            return False
        theirs = other.elements()
        if len(theirs) != len(self.items):
            return False
        for i in range(len(self.items)):
            if not self.items[i].__eq__(theirs[i]):
                return False
        return True

    def order(self, other: _Any, op: str) -> int:
        if other.kind() != K_LIST:
            raise TypeError("'" + op + "' not supported between instances of 'list' and '" + other.type_name() + "'")
        return _any_sequence_order(self.items, other.elements(), op)

    def add(self, other: _Any) -> _Any:
        if other.kind() != K_LIST:
            raise TypeError("can only concatenate list (not \"" + other.type_name() + "\") to list")
        joined: list[_Any] = []
        for x in self.items:
            joined.append(x)
        for x in other.elements():
            joined.append(x)
        return _AnyList(joined)

    def repeat(self, times: int) -> _Any:
        out: list[_Any] = []
        for _ in range(times):
            for x in self.items:
                out.append(x)
        return _AnyList(out)

    def mul(self, other: _Any) -> _Any:
        if other.kind() in (K_INT, K_BOOL):
            return self.repeat(other.whole())
        raise TypeError("can't multiply sequence by non-int of type '" + other.type_name() + "'")

    def append(self, value: _Any) -> None:
        self.items.append(value)

    def extend(self, values: _Any) -> None:
        for x in values.iter_items():
            self.items.append(x)

    def pop(self) -> _Any:
        if len(self.items) == 0:
            raise IndexError("pop from empty list")
        return self.items.pop()

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        if len(self.items) == 0:
            return "[]"
        parts: list[str] = []
        for x in self.items:
            parts.append(x.json(indent, level + 1, sort_keys))
        return _any_json_join("[", "]", parts, indent, level)


class _AnyTuple(_Any):
    def __init__(self, items: list[_Any]):
        self.items = items

    def kind(self) -> int:
        return K_TUPLE

    def type_name(self) -> str:
        return "tuple"

    def text(self) -> str:
        if len(self.items) == 1:
            return "(" + self.items[0].text() + ",)"
        return "(" + ", ".join([x.text() for x in self.items]) + ")"

    def truth(self) -> bool:
        return len(self.items) > 0

    def elements(self) -> list[_Any]:
        return self.items

    def isa(self, name: str) -> bool:
        return name == "tuple"

    def __len__(self) -> int:
        return len(self.items)

    def iter_items(self) -> list[_Any]:
        return self.items

    def __getitem__(self, key: _Any) -> _Any:
        if key.kind() in (K_INT, K_BOOL):
            i = key.whole()
            n = len(self.items)
            if i < 0:
                i += n
            if i < 0 or i >= n:
                raise IndexError("tuple index out of range")
            return self.items[i]
        raise TypeError("tuple indices must be integers or slices, not " + key.type_name())

    def __contains__(self, item: _Any) -> bool:
        for x in self.items:
            if x.__eq__(item):
                return True
        return False

    def __eq__(self, other: _Any) -> bool:
        if other.kind() != K_TUPLE:
            return False
        theirs = other.elements()
        if len(theirs) != len(self.items):
            return False
        for i in range(len(self.items)):
            if not self.items[i].__eq__(theirs[i]):
                return False
        return True

    def order(self, other: _Any, op: str) -> int:
        if other.kind() != K_TUPLE:
            raise TypeError("'" + op + "' not supported between instances of 'tuple' and '" + other.type_name() + "'")
        return _any_sequence_order(self.items, other.elements(), op)

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        if len(self.items) == 0:
            return "[]"
        parts: list[str] = []
        for x in self.items:
            parts.append(x.json(indent, level + 1, sort_keys))
        return _any_json_join("[", "]", parts, indent, level)


class _AnyDict(_Any):
    def __init__(self, entries: dict[str, _Any]):
        self.entries = entries

    def kind(self) -> int:
        return K_DICT

    def type_name(self) -> str:
        return "dict"

    def text(self) -> str:
        return "{" + ", ".join([repr(k) + ": " + v.text() for k, v in self.entries.items()]) + "}"

    def truth(self) -> bool:
        return len(self.entries) > 0

    def entries_or_empty(self) -> dict[str, _Any]:
        return self.entries

    def as_dict(self) -> dict[str, _Any]:
        return self.entries

    def isa(self, name: str) -> bool:
        return name == "dict"

    def __len__(self) -> int:
        return len(self.entries)

    def iter_items(self) -> list[_Any]:
        return [_AnyStr(k) for k in self.entries]

    def key_of(self, key: _Any) -> str:
        if key.kind() != K_STR:
            raise KeyError(key.text())
        return key.text_or_empty()

    def __getitem__(self, key: _Any) -> _Any:
        k = self.key_of(key)
        if k not in self.entries:
            raise KeyError(key.text())
        return self.entries[k]

    def __setitem__(self, key: _Any, value: _Any) -> None:
        if key.kind() != K_STR:
            raise TypeError("a dict holding Any values takes str keys here, not " + key.type_name())
        self.entries[key.text_or_empty()] = value

    def __contains__(self, item: _Any) -> bool:
        if item.kind() != K_STR:
            return False
        return item.text_or_empty() in self.entries

    def __eq__(self, other: _Any) -> bool:
        if other.kind() != K_DICT:
            return False
        theirs = other.entries_or_empty()
        if len(theirs) != len(self.entries):
            return False
        for k, v in self.entries.items():
            if k not in theirs:
                return False
            if not v.__eq__(theirs[k]):
                return False
        return True

    def get(self, key: _Any, default: _Any = None) -> _Any:
        if key.kind() == K_STR and key.text_or_empty() in self.entries:
            return self.entries[key.text_or_empty()]
        return default

    def keys(self) -> list[_Any]:
        return [_AnyStr(k) for k in self.entries]

    def values(self) -> list[_Any]:
        return [v for v in self.entries.values()]

    def items(self) -> list[tuple[_Any, _Any]]:
        out: list[tuple[_Any, _Any]] = []
        for k, v in self.entries.items():
            out.append((_AnyStr(k), v))
        return out

    def setdefault(self, key: _Any, default: _Any = None) -> _Any:
        k = self.key_of(key)
        if k not in self.entries:
            self.entries[k] = default
        return self.entries[k]

    def pop(self) -> _Any:
        raise TypeError("pop expected at least 1 argument, got 0")

    def json(self, indent: int, level: int, sort_keys: bool) -> str:
        if len(self.entries) == 0:
            return "{}"
        keys: list[str] = [k for k in self.entries]
        if sort_keys:
            keys = sorted(keys)
        separator = _AnyJsonStyle.key_sep
        parts: list[str] = []
        for k in keys:
            parts.append(_any_json_quote(k) + separator + self.entries[k].json(indent, level + 1, sort_keys))
        return _any_json_join("{", "}", parts, indent, level)


def _any_number_order(a: _Any, b: _Any, op: str) -> int:
    if not b.is_number():
        raise TypeError("'" + op + "' not supported between instances of '" + a.type_name() + "' and '" + b.type_name() + "'")
    if a.kind() == K_FLOAT or b.kind() == K_FLOAT:
        x = a.num()
        y = b.num()
        if x < y:
            return -1
        if x > y:
            return 1
        if x == y:
            return 0
        return 2
    i = a.whole()
    j = b.whole()
    if i < j:
        return -1
    if i > j:
        return 1
    return 0


def _any_sequence_order(mine: list[_Any], theirs: list[_Any], op: str) -> int:
    n = min(len(mine), len(theirs))
    for i in range(n):
        if not mine[i].__eq__(theirs[i]):
            return mine[i].order(theirs[i], op)
    if len(mine) < len(theirs):
        return -1
    if len(mine) > len(theirs):
        return 1
    return 0


def _any_arith(a: _Any, b: _Any, op: str) -> _Any:
    # Two numbers: ints (and bools) stay ints where CPython's do.
    if a.kind() != K_FLOAT and b.kind() != K_FLOAT and op != "/":
        i = a.whole()
        j = b.whole()
        if op == "+":
            return _AnyInt(i + j)
        if op == "-":
            return _AnyInt(i - j)
        if op == "*":
            return _AnyInt(i * j)
        if op == "//":
            if j == 0:
                raise ZeroDivisionError("integer division or modulo by zero")
            return _AnyInt(i // j)
        if op == "%":
            if j == 0:
                raise ZeroDivisionError("integer modulo by zero")
            return _AnyInt(i % j)
        if j < 0:
            if i == 0:
                raise ZeroDivisionError("0.0 cannot be raised to a negative power")
            return _AnyFloat(float(i) ** float(j))
        return _AnyInt(i ** j)
    x = a.num()
    y = b.num()
    if op == "+":
        return _AnyFloat(x + y)
    if op == "-":
        return _AnyFloat(x - y)
    if op == "*":
        return _AnyFloat(x * y)
    if op == "/":
        if y == 0.0:
            if a.kind() == K_FLOAT or b.kind() == K_FLOAT:
                raise ZeroDivisionError("float division by zero")
            raise ZeroDivisionError("division by zero")
        return _AnyFloat(x / y)
    if op == "//":
        if y == 0.0:
            raise ZeroDivisionError("float floor division by zero")
        return _AnyFloat(x // y)
    if op == "%":
        if y == 0.0:
            raise ZeroDivisionError("float modulo by zero")
        return _AnyFloat(x % y)
    return _AnyFloat(x ** y)


def _any_json_quote(text: str) -> str:
    out = "\""
    for c in text:
        code = ord(c)
        if c == "\"":
            out += "\\\""
        elif c == "\\":
            out += "\\\\"
        elif c == "\n":
            out += "\\n"
        elif c == "\r":
            out += "\\r"
        elif c == "\t":
            out += "\\t"
        elif c == "\b":
            out += "\\b"
        elif c == "\f":
            out += "\\f"
        elif code < 32 or (code > 126 and _AnyJsonStyle.ascii):
            if code > 0xFFFF:
                code -= 0x10000
                out += "\\u" + _any_hex4(0xD800 + (code >> 10)) + "\\u" + _any_hex4(0xDC00 + (code & 0x3FF))
            else:
                out += "\\u" + _any_hex4(code)
        else:
            out += c
    return out + "\""


def _any_hex4(n: int) -> str:
    digits = "0123456789abcdef"
    return digits[(n >> 12) & 15] + digits[(n >> 8) & 15] + digits[(n >> 4) & 15] + digits[n & 15]


class _AnyJsonStyle:
    # The separators and escaping of the `json.dumps` call being written.
    item_sep = ", "
    key_sep = ": "
    ascii = True
    # CPython's item separator before a new line, which code generation
    # passes when indenting (and so needs this literal in memory).
    line_sep = ","


def _any_json_join(open_: str, close: str, parts: list[str], indent: int, level: int) -> str:
    if indent < 0:
        return open_ + _AnyJsonStyle.item_sep.join(parts) + close
    inner = "\n" + " " * (indent * (level + 1))
    outer = "\n" + " " * (indent * level)
    return open_ + inner + (_AnyJsonStyle.item_sep + inner).join(parts) + outer + close


def __any_none() -> _Any:
    return _Any()


def __any_of_bool(value: bool) -> _Any:
    return _AnyBool(value)


def __any_of_int(value: int) -> _Any:
    return _AnyInt(value)


def __any_of_float(value: float) -> _Any:
    return _AnyFloat(value)


def __any_of_str(value: str) -> _Any:
    return _AnyStr(value)


def __any_of_opt_str(value: Optional[str]) -> _Any:
    if value is None:
        return _Any()
    return _AnyStr(str(value))


def __any_of_opt_int(value: Optional[int]) -> _Any:
    if value is None:
        return _Any()
    whole: int = value
    return _AnyInt(whole)


def __any_of_opt_float(value: Optional[float]) -> _Any:
    if value is None:
        return _Any()
    real: float = value
    return _AnyFloat(real)


def __any_of_opt_bool(value: Optional[bool]) -> _Any:
    if value is None:
        return _Any()
    return _AnyBool(value == True)


def __any_of_list(items: list[_Any]) -> _Any:
    return _AnyList(items)


def __any_of_dict(entries: dict[str, _Any]) -> _Any:
    return _AnyDict(entries)


def __any_of_tuple(items: list[_Any]) -> _Any:
    return _AnyTuple(items)


def __any_as_bool(value: _Any) -> bool:
    return value.as_bool()


def __any_as_int(value: _Any) -> int:
    return value.as_int()


def __any_as_float(value: _Any) -> float:
    return value.as_float()


def __any_as_str(value: _Any) -> str:
    return value.as_str()


def __any_as_list(value: _Any) -> list[_Any]:
    return value.as_list()


def __any_as_dict(value: _Any) -> dict[str, _Any]:
    return value.as_dict()


def __any_int(value: _Any) -> int:
    return value.to_int()


def __any_float(value: _Any) -> float:
    return value.to_float()


def __any_is_none(value: _Any) -> bool:
    return value.kind() == K_NONE


def __any_isa(value: _Any, name: str) -> bool:
    return value.isa(name)


def __any_dumps(value: _Any, indent: int, sort_keys: bool, item_sep: str, key_sep: str, ensure_ascii: bool) -> str:
    _AnyJsonStyle.item_sep = item_sep
    _AnyJsonStyle.key_sep = key_sep
    _AnyJsonStyle.ascii = ensure_ascii
    try:
        return value.json(indent, 0, sort_keys)
    finally:
        _AnyJsonStyle.item_sep = ", "
        _AnyJsonStyle.key_sep = ": "
        _AnyJsonStyle.ascii = True


def __any_sum(items: list[_Any], start: _Any) -> _Any:
    total = start
    for x in items:
        total = total.add(x)
    return total


class _AnyParser:
    # JSON text parsed into dynamic values, as CPython's `json.loads` parses
    # it: the same values, and `json.JSONDecodeError` with its message and
    # position for a document it rejects.
    def __init__(self, text: str):
        self.text = text
        self.chars: list[str] = list(text)
        self.n = len(self.chars)
        self.end = 0

    def fail(self, message: str, pos: int) -> None:
        line = 1
        last = -1
        for i in range(min(pos, self.n)):
            if self.chars[i] == "\n":
                line += 1
                last = i
        column = pos - last
        raise JSONDecodeError(message + ": line " + str(line) + " column " + str(column) + " (char " + str(pos) + ")")

    def skip(self, pos: int) -> int:
        while pos < self.n and self.chars[pos] in " \t\n\r":
            pos += 1
        return pos

    def document(self) -> _Any:
        pos = self.skip(0)
        result = self.value(pos)
        end = self.skip(self.end)
        if end != self.n:
            self.fail("Extra data", end)
        return result

    def value(self, pos: int) -> _Any:
        if pos >= self.n:
            self.fail("Expecting value", pos)
        c = self.chars[pos]
        if c == "\"":
            text = self.string(pos + 1)
            return _AnyStr(text)
        if c == "{":
            return self.obj(pos + 1)
        if c == "[":
            return self.array(pos + 1)
        if self.matches(pos, "null"):
            self.end = pos + 4
            return _Any()
        if self.matches(pos, "true"):
            self.end = pos + 4
            return _AnyBool(True)
        if self.matches(pos, "false"):
            self.end = pos + 5
            return _AnyBool(False)
        if self.matches(pos, "NaN"):
            self.end = pos + 3
            return _AnyFloat(float("nan"))
        if self.matches(pos, "Infinity"):
            self.end = pos + 8
            return _AnyFloat(float("inf"))
        if self.matches(pos, "-Infinity"):
            self.end = pos + 9
            return _AnyFloat(-float("inf"))
        return self.number(pos)

    def matches(self, pos: int, word: str) -> bool:
        if pos + len(word) > self.n:
            return False
        for i in range(len(word)):
            if self.chars[pos + i] != word[i]:
                return False
        return True

    def number(self, pos: int) -> _Any:
        start = pos
        if pos < self.n and self.chars[pos] == "-":
            pos += 1
        if pos >= self.n or self.chars[pos] not in "0123456789":
            self.fail("Expecting value", start)
        if self.chars[pos] == "0":
            pos += 1
        else:
            while pos < self.n and self.chars[pos] in "0123456789":
                pos += 1
        is_float = False
        if pos < self.n and self.chars[pos] == "." and pos + 1 < self.n and self.chars[pos + 1] in "0123456789":
            is_float = True
            pos += 1
            while pos < self.n and self.chars[pos] in "0123456789":
                pos += 1
        if pos < self.n and self.chars[pos] in "eE":
            mark = pos
            pos += 1
            if pos < self.n and self.chars[pos] in "+-":
                pos += 1
            if pos < self.n and self.chars[pos] in "0123456789":
                is_float = True
                while pos < self.n and self.chars[pos] in "0123456789":
                    pos += 1
            else:
                pos = mark
        self.end = pos
        literal = "".join(self.chars[start:pos])
        if is_float:
            return _AnyFloat(float(literal))
        return _AnyInt(int(literal))

    def string(self, pos: int) -> str:
        start = pos - 1
        parts: list[str] = []
        while True:
            if pos >= self.n:
                self.fail("Unterminated string starting at", start)
            c = self.chars[pos]
            if c == "\"":
                self.end = pos + 1
                return "".join(parts)
            if c == "\\":
                pos += 1
                if pos >= self.n:
                    self.fail("Unterminated string starting at", start)
                e = self.chars[pos]
                if e == "u":
                    code = self.hex_at(pos + 1)
                    pos += 5
                    if 0xD800 <= code <= 0xDBFF and pos + 1 < self.n and self.chars[pos] == "\\" and self.chars[pos + 1] == "u":
                        low = self.hex_at(pos + 2)
                        if 0xDC00 <= low <= 0xDFFF:
                            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                            pos += 6
                    parts.append(chr(code))
                    continue
                if e == "\"":
                    parts.append("\"")
                elif e == "\\":
                    parts.append("\\")
                elif e == "/":
                    parts.append("/")
                elif e == "b":
                    parts.append("\b")
                elif e == "f":
                    parts.append("\f")
                elif e == "n":
                    parts.append("\n")
                elif e == "r":
                    parts.append("\r")
                elif e == "t":
                    parts.append("\t")
                else:
                    self.fail("Invalid \\escape", pos - 1)
                pos += 1
                continue
            if ord(c) < 32:
                self.fail("Invalid control character at", pos)
            parts.append(c)
            pos += 1

    def hex_at(self, pos: int) -> int:
        if pos + 4 > self.n:
            self.fail("Invalid \\uXXXX escape", pos - 1)
        code = 0
        for i in range(4):
            d = self.chars[pos + i]
            code *= 16
            if "0" <= d <= "9":
                code += ord(d) - 48
            elif "a" <= d <= "f":
                code += ord(d) - 87
            elif "A" <= d <= "F":
                code += ord(d) - 55
            else:
                self.fail("Invalid \\uXXXX escape", pos - 1)
        return code

    def array(self, pos: int) -> _Any:
        items: list[_Any] = []
        pos = self.skip(pos)
        if pos < self.n and self.chars[pos] == "]":
            self.end = pos + 1
            return _AnyList(items)
        while True:
            pos = self.skip(pos)
            items.append(self.value(pos))
            pos = self.skip(self.end)
            if pos < self.n and self.chars[pos] == "]":
                self.end = pos + 1
                return _AnyList(items)
            if pos >= self.n or self.chars[pos] != ",":
                self.fail("Expecting ',' delimiter", pos)
            comma = pos
            pos = self.skip(pos + 1)
            if pos < self.n and self.chars[pos] == "]":
                self.fail("Illegal trailing comma before end of array", comma)

    def obj(self, pos: int) -> _Any:
        entries: dict[str, _Any] = {}
        pos = self.skip(pos)
        if pos < self.n and self.chars[pos] == "}":
            self.end = pos + 1
            return _AnyDict(entries)
        while True:
            pos = self.skip(pos)
            if pos >= self.n or self.chars[pos] != "\"":
                self.fail("Expecting property name enclosed in double quotes", pos)
            key = self.string(pos + 1)
            pos = self.skip(self.end)
            if pos >= self.n or self.chars[pos] != ":":
                self.fail("Expecting ':' delimiter", pos)
            pos = self.skip(pos + 1)
            entries[key] = self.value(pos)
            pos = self.skip(self.end)
            if pos < self.n and self.chars[pos] == "}":
                self.end = pos + 1
                return _AnyDict(entries)
            if pos >= self.n or self.chars[pos] != ",":
                self.fail("Expecting ',' delimiter", pos)
            comma = pos
            pos = self.skip(pos + 1)
            if pos < self.n and self.chars[pos] == "}":
                self.fail("Illegal trailing comma before end of object", comma)


def __any_loads(text: str) -> _Any:
    parser = _AnyParser(text)
    return parser.document()


def __any_format(value: _Any, spec: str) -> str:
    return value.format_with(spec)


def __any_max(items: list[_Any]) -> _Any:
    if len(items) == 0:
        raise ValueError("max() iterable argument is empty")
    best = items[0]
    for i in range(1, len(items)):
        if items[i].order(best, ">") > 0:
            best = items[i]
    return best


def __any_min(items: list[_Any]) -> _Any:
    if len(items) == 0:
        raise ValueError("min() iterable argument is empty")
    best = items[0]
    for i in range(1, len(items)):
        if items[i].order(best, "<") < 0:
            best = items[i]
    return best
