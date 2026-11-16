# pydantic's BaseModel, the subset a compiled program can have: a model's
# fields are typed by its annotations, and `ir::shims` writes each model's
# constructor, repr, comparison, dump, and validation out of them. What is
# here is what every model shares: the errors and the lax conversions
# pydantic 2.14 makes from JSON-shaped values (its messages word for word).

import json
from typing import Any, Optional


class BaseModel:
    pass


class _Err:
    def __init__(self, type: str, loc: list[str], msg: str, input: Any, ctx: str = "") -> None:
        self.type: str = type
        self.loc: list[str] = loc
        self.msg: str = msg
        self.input: Any = input
        self.ctx: str = ctx

    def __repr__(self) -> str:
        return "_Err(" + self.type + ")"


def _kind(v: Any) -> str:
    if v is None:
        return "NoneType"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, int):
        return "int"
    if isinstance(v, float):
        return "float"
    if isinstance(v, str):
        return "str"
    if isinstance(v, list):
        return "list"
    return "dict"


def _short(text: str) -> str:
    if len(text) > 50:
        return text[:25] + "..." + text[len(text) - 24:]
    return text


def _loc_text(loc: list[str]) -> str:
    parts: list[str] = []
    for p in loc:
        if p.startswith("#"):
            parts.append(p[1:])
        else:
            parts.append(p)
    return ".".join(parts)


def _json_str(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)


def _json(v: Any) -> str:
    if v is None:
        return "null"
    if isinstance(v, bool):
        b: bool = v
        return "true" if b else "false"
    if isinstance(v, int):
        i: int = v
        return str(i)
    if isinstance(v, float):
        f: float = v
        return json.dumps(f)
    if isinstance(v, str):
        s: str = v
        return _json_str(s)
    if isinstance(v, list):
        items: list[Any] = v
        parts: list[str] = []
        for x in items:
            parts.append(_json(x))
        return "[" + ",".join(parts) + "]"
    entries: dict[str, Any] = v
    fields: list[str] = []
    for k in entries:
        fields.append(_json_str(k) + ":" + _json(entries[k]))
    return "{" + ",".join(fields) + "}"


def _loc_json(loc: list[str]) -> str:
    parts: list[str] = []
    for p in loc:
        if p.startswith("#"):
            parts.append(p[1:])
        else:
            parts.append(_json_str(p))
    return "[" + ",".join(parts) + "]"


def _err_json(e: _Err) -> str:
    out = "{\"type\":" + _json_str(e.type) + ",\"loc\":" + _loc_json(e.loc) + ",\"msg\":" + _json_str(e.msg) + ",\"input\":" + _json(e.input)
    if e.ctx != "":
        out += ",\"ctx\":{\"error\":" + _json_str(e.ctx) + "}"
    return out + "}"


class ValidationError(ValueError):
    def __init__(self, title: str) -> None:
        super().__init__(title)
        self.title: str = title
        self.errs: list[_Err] = []

    def error_count(self) -> int:
        return len(self.errs)

    def __str__(self) -> str:
        n = len(self.errs)
        out = str(n) + " validation error" + ("" if n == 1 else "s") + " for " + self.title
        for e in self.errs:
            out += "\n"
            if len(e.loc) > 0:
                out += _loc_text(e.loc) + "\n"
            out += "  " + e.msg + " [type=" + e.type + ", input_value=" + _short(repr(e.input)) + ", input_type=" + _kind(e.input) + "]"
            out += "\n    For further information visit https://errors.pydantic.dev/2.14/v/" + e.type
        return out


def _fail(title: str, errs: list[_Err]) -> ValidationError:
    e = ValidationError(title)
    e.errs = errs
    return e


def _empty() -> Any:
    d: dict[str, Any] = {}
    return d


def _is_dict(v: Any) -> bool:
    return isinstance(v, dict)


def _missing(loc: list[str], whole: Any) -> _Err:
    return _Err("missing", loc, "Field required", whole)


def _digits(s: str) -> bool:
    if s == "" or s.startswith("_") or s.endswith("_") or "__" in s:
        return False
    for c in s:
        if not (c.isdigit() or c == "_"):
            return False
    return True


def _v_str(v: Any, loc: list[str], errs: list[_Err]) -> str:
    if isinstance(v, str):
        s: str = v
        return s
    errs.append(_Err("string_type", loc, "Input should be a valid string", v))
    return ""


def _v_int(v: Any, loc: list[str], errs: list[_Err]) -> int:
    if isinstance(v, bool):
        b: bool = v
        return 1 if b else 0
    if isinstance(v, int):
        i: int = v
        return i
    if isinstance(v, float):
        f: float = v
        if f != f or f == float("inf") or f == float("-inf"):
            errs.append(_Err("finite_number", loc, "Input should be a finite number", v))
            return 0
        if f != float(int(f)):
            errs.append(_Err("int_from_float", loc, "Input should be a valid integer, got a number with a fractional part", v))
            return 0
        return int(f)
    if isinstance(v, str):
        s: str = v
        t = s.strip()
        sign = ""
        if t.startswith("+") or t.startswith("-"):
            sign = t[0]
            t = t[1:]
        if t.endswith(".0"):
            t = t[: len(t) - 2]
        if _digits(t):
            n = int(t.replace("_", ""))
            return -n if sign == "-" else n
        errs.append(_Err("int_parsing", loc, "Input should be a valid integer, unable to parse string as an integer", v))
        return 0
    errs.append(_Err("int_type", loc, "Input should be a valid integer", v))
    return 0


def _v_float(v: Any, loc: list[str], errs: list[_Err]) -> float:
    if isinstance(v, bool):
        b: bool = v
        return 1.0 if b else 0.0
    if isinstance(v, int):
        i: int = v
        return float(i)
    if isinstance(v, float):
        f: float = v
        return f
    if isinstance(v, str):
        s: str = v
        try:
            return float(s.strip())
        except ValueError:
            errs.append(_Err("float_parsing", loc, "Input should be a valid number, unable to parse string as a number", v))
            return 0.0
    errs.append(_Err("float_type", loc, "Input should be a valid number", v))
    return 0.0


def _v_bool(v: Any, loc: list[str], errs: list[_Err]) -> bool:
    if isinstance(v, bool):
        b: bool = v
        return b
    if isinstance(v, int):
        i: int = v
        if i == 0 or i == 1:
            return i == 1
        errs.append(_Err("bool_parsing", loc, "Input should be a valid boolean, unable to interpret input", v))
        return False
    if isinstance(v, float):
        f: float = v
        if f == 0.0 or f == 1.0:
            return f == 1.0
        errs.append(_Err("bool_type", loc, "Input should be a valid boolean", v))
        return False
    if isinstance(v, str):
        s: str = v
        t = s.strip().lower()
        if t in ["1", "on", "t", "true", "y", "yes"]:
            return True
        if t in ["0", "off", "f", "false", "n", "no"]:
            return False
        errs.append(_Err("bool_parsing", loc, "Input should be a valid boolean, unable to interpret input", v))
        return False
    errs.append(_Err("bool_type", loc, "Input should be a valid boolean", v))
    return False


def _v_list(v: Any, loc: list[str], errs: list[_Err]) -> list[Any]:
    if isinstance(v, list):
        items: list[Any] = v
        return items
    errs.append(_Err("list_type", loc, "Input should be a valid array" if _Mode.json else "Input should be a valid list", v))
    return []


def _v_dict(v: Any, loc: list[str], errs: list[_Err]) -> dict[str, Any]:
    if isinstance(v, dict):
        entries: dict[str, Any] = v
        return entries
    errs.append(_Err("dict_type", loc, "Input should be an object" if _Mode.json else "Input should be a valid dictionary", v))
    return {}


class _Mode:
    # Set while FastAPI validates a request body, which pydantic reads in its
    # from-attributes mode, and while model_validate_json validates, whose
    # messages name JSON's types.
    attributes = False
    json = False


def _v_model(v: Any, title: str, loc: list[str], errs: list[_Err]) -> dict[str, Any]:
    if isinstance(v, dict):
        entries: dict[str, Any] = v
        return entries
    if _Mode.attributes:
        errs.append(_Err("model_attributes_type", loc, "Input should be a valid dictionary or object to extract fields from", v))
    elif _Mode.json:
        errs.append(_Err("model_type", loc, "Input should be an object", v))
    else:
        errs.append(_Err("model_type", loc, "Input should be a valid dictionary or instance of " + title, v))
    return {}


def _parse_json(text: str, title: str) -> Any:
    try:
        return json.loads(text)
    except json.JSONDecodeError as e:
        errs: list[_Err] = []
        errs.append(_Err("json_invalid", [], "Invalid JSON: " + str(e), text))
        raise _fail(title, errs)
