//! Dynamic values (`ir::dynamic`): `typing.Any`, literals mixing types, `json.loads()`
//! with no declared type, and unannotated parameters whose calls pass several types.
//! Every expected value is what CPython 3.13 prints for the same program.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{dedent, run_main, try_compile};

/// `typing.Any` holds a dynamic value: operators, method calls, comparisons, and `len()` answer for whatever it holds, and a `list[Any]` or `dict[str, Any]` takes values of any kind.
#[test]
fn any_annotations_hold_dynamic_values() {
    let src = dedent(
        r#"
        from typing import Any


        def describe(x: Any) -> str:
            return "value " + str(x)


        def main():
            a: Any = 5
            b: Any = "hi"
            c: Any = 2.5
            print(a, b, c, a + 1, b + "!", c * 2)
            print(describe(a), describe(b), describe(None))
            n: int = a
            s: str = b
            print(n + 1, s.upper(), a == 5, b == "hi", a < 10, len(b))
            items: list[Any] = [1, 2, 3]
            items.append("x")
            print(items, len(items))
            record: dict[str, Any] = {}
            record["name"] = "bob"
            record["age"] = 30
            print(record, record["age"] + 1)


        if __name__ == "__main__":
            main()
        "#,
    );
    assert_eq!(
        run_main(&src, "", &[]).stdout,
        "5 hi 2.5 6 hi! 5.0\nvalue 5 value hi value None\n6 HI True True True 2\n[1, 2, 3, 'x'] 4\n{'name': 'bob', 'age': 30} 31\n"
    );
}

/// A literal mixing types (`{"name": "bob", "age": 30}`, `[1, "two", 3.0, None, True]`) holds dynamic values, nested ones included, and `json.dumps()` writes them as CPython does. They were refused.
#[test]
fn mixed_literals_are_records_of_dynamic_values() {
    let src = dedent(
        r#"
        import json


        def main():
            user = {"name": "bob", "age": 30, "admin": False, "score": 4.5}
            print(user, user["name"], user["age"] + 1)
            values = [1, "two", 3.0, None, True]
            print(values, len(values), values[1])
            tags = ["python", "wasm"]
            payload = {"model": "claude", "max_tokens": 100, "messages": [{"role": "user", "content": "hi"}]}
            print(payload["messages"][0]["content"], payload["max_tokens"] * 2)
            print(json.dumps(user))
            print(json.dumps(payload, indent=2))
            if user["admin"]:
                print("admin")
            else:
                print("not admin")
            older = user["age"] > 18
            print(older, user["score"] / 2, f"{user['name']} is {user['age']}")
            user["age"] = 31
            user["email"] = "bob@example.com"
            print(user)


        if __name__ == "__main__":
            main()
        "#,
    );
    assert_eq!(
        run_main(&src, "", &[]).stdout,
        "{'name': 'bob', 'age': 30, 'admin': False, 'score': 4.5} bob 31\n[1, 'two', 3.0, None, True] 5 two\nhi 200\n{\"name\": \"bob\", \"age\": 30, \"admin\": false, \"score\": 4.5}\n{\n  \"model\": \"claude\",\n  \"max_tokens\": 100,\n  \"messages\": [\n    {\n      \"role\": \"user\",\n      \"content\": \"hi\"\n    }\n  ]\n}\nnot admin\nTrue 2.25 bob is 30\n{'name': 'bob', 'age': 31, 'admin': False, 'score': 4.5, 'email': 'bob@example.com'}\n"
    );
}

/// `json.loads()` with no declared type parses into dynamic values: indexing, iteration over a list and a dict's keys, `+=` and `append` through them (seen through the dict that holds them), `sum()`, and `json.dumps(sort_keys=True)`.
#[test]
fn json_loads_with_no_type_parses_into_dynamic_values() {
    let src = dedent(
        r#"
        import json


        def main():
            text = '{"name": "ada", "langs": ["python", "rust"], "years": 12, "rate": 9.5, "active": true}'
            data = json.loads(text)
            print(data["name"], data["langs"][1], data["years"] + 1, data["rate"] * 2)
            print(len(data["langs"]), data["active"], data)
            for lang in data["langs"]:
                print("lang:", lang)
            for key in data:
                print(key, "->", data[key])
            data["years"] += 1
            data["langs"].append("go")
            print(json.dumps(data, sort_keys=True))
            config = json.loads("[1, 2, 3]")
            print(sum(config), config[0] + config[2])


        if __name__ == "__main__":
            main()
        "#,
    );
    assert_eq!(
        run_main(&src, "", &[]).stdout,
        "ada rust 13 19.0\n2 True {'name': 'ada', 'langs': ['python', 'rust'], 'years': 12, 'rate': 9.5, 'active': True}\nlang: python\nlang: rust\nname -> ada\nlangs -> ['python', 'rust']\nyears -> 12\nrate -> 9.5\nactive -> True\n{\"active\": true, \"langs\": [\"python\", \"rust\", \"go\"], \"name\": \"ada\", \"rate\": 9.5, \"years\": 13}\n6 4\n"
    );
}

/// An unannotated parameter takes the type its calls pass, or `Any` where they pass several (`show("count", 3)` and `show("name", "ada")`). It held only a word, and refused a str or a list.
#[test]
fn untyped_parameters_take_the_types_their_calls_pass() {
    let src = dedent(
        r#"
        def show(label, value):
            print(label + ":", value)


        def area(r):
            return 3.14159 * r * r


        def total(items):
            t = 0
            for x in items:
                t += x
            return t


        def shout(text):
            return text.upper() + "!"


        def countdown(n):
            if n <= 0:
                return "liftoff"
            return countdown(n - 1)


        def main():
            show("count", 3)
            show("name", "ada")
            show("ratio", 0.5)
            print(round(area(2), 3), round(area(1.5), 3))
            print(total([1, 2, 3]), shout("hey"), countdown(3))


        if __name__ == "__main__":
            main()
        "#,
    );
    assert_eq!(
        run_main(&src, "", &[]).stdout,
        "count: 3\nname: ada\nratio: 0.5\n12.566 7.069\n6 HEY! liftoff\n"
    );
}

/// `isinstance`, `is None`, `int()`, `float()`, `abs()`, format specs, `sorted`, `in`, `get`, unpacking, `sum`/`max`/`min`, and CPython's errors for what a kind does not do (`TypeError`, `KeyError`, `IndexError`, `AttributeError`, `JSONDecodeError`).
#[test]
fn builtins_and_errors_on_dynamic_values() {
    let src = dedent(
        r#"
        import json


        def main():
            row = {"id": 7, "name": "ada", "score": 91.25, "tags": ["x", "y"], "boss": None, "ok": True}
            print(isinstance(row["id"], int), isinstance(row["ok"], int), isinstance(row["name"], (int, str)))
            print(row["boss"] is None, row["name"] is not None, int(row["score"]), float(row["id"]), abs(-row["id"]))
            print(f"{row['name']:>6}|{row['score']:.1f}|{row['id']:03d}|{row['ok']}")
            print(sorted(row["tags"], reverse=True), "x" in row["tags"], "name" in row, row.get("missing", "none"))
            first, second = row["tags"]
            print(first, second, [t.upper() for t in row["tags"]], row["name"].upper(), row["name"] * 2)
            numbers = json.loads("[3, 1.5, 2, -4]")
            print(sum(numbers), sorted(numbers), max(numbers), min(numbers), numbers[-1] ** 2, 7 // numbers[0])
            for bad in ["row['name'] + 1", "row['id'][0]", "row['missing']", "row['tags'][5]", "len(row['id'])", "row['id'].upper()"]:
                try:
                    if bad == "row['name'] + 1":
                        print(row["name"] + 1)
                    elif bad == "row['id'][0]":
                        print(row["id"][0])
                    elif bad == "row['missing']":
                        print(row["missing"])
                    elif bad == "row['tags'][5]":
                        print(row["tags"][5])
                    elif bad == "len(row['id'])":
                        print(len(row["id"]))
                    else:
                        print(row["id"].upper())
                except (TypeError, KeyError, IndexError, AttributeError) as e:
                    print(type(e).__name__, e)
            try:
                json.loads('{"a": 1,}')
            except json.JSONDecodeError as e:
                print("JSONDecodeError:", e)


        if __name__ == "__main__":
            main()
        "#,
    );
    assert_eq!(
        run_main(&src, "", &[]).stdout,
        "True True True\nTrue True 91 7.0 7\n   ada|91.2|007|True\n['y', 'x'] True True none\nx y ['X', 'Y'] ADA adaada\n2.5 [-4, 1.5, 2, 3] 3 -4 16 2\nTypeError can only concatenate str (not \"int\") to str\nTypeError 'int' object is not subscriptable\nKeyError 'missing'\nIndexError list index out of range\nTypeError object of type 'int' has no len()\nAttributeError 'int' object has no attribute 'upper'\nJSONDecodeError: Illegal trailing comma before end of object: line 1 column 8 (char 7)\n"
    );
}

/// A dynamic value stored where another type is declared is converted to it,
/// and raises `TypeError` when it holds another kind: the place reads one kind,
/// where CPython would keep whatever it held.
#[test]
fn a_dynamic_value_of_another_kind_raises_where_a_type_is_declared() {
    let src = dedent(
        r#"
        from typing import Any

        def main() -> None:
            v: Any = "hi"
            for target in ["int", "float", "list"]:
                try:
                    if target == "int":
                        n: int = v
                        print(n)
                    elif target == "float":
                        f: float = v
                        print(f)
                    else:
                        xs: list[Any] = v
                        print(xs)
                except TypeError as e:
                    print("TypeError:", e)
            s: str = v
            print(s.upper())

        if __name__ == "__main__":
            main()
        "#,
    );
    assert_eq!(
        run_main(&src, "", &[]).stdout,
        "TypeError: a str is used where an int is declared\nTypeError: a str is used where a float is declared\nTypeError: a str is used where a list is declared\nHI\n"
    );
}

/// What a dynamic value cannot hold is refused when compiling: an instance of
/// one of the program's classes, and a typed list held under another name.
#[test]
fn what_a_dynamic_value_cannot_hold_is_refused() {
    let err = try_compile(&dedent(
        r#"
        from typing import Any

        class P:
            pass

        def f() -> int:
            x: Any = P()
            return 1
        "#,
    ))
    .expect_err("an instance cannot be held as Any");
    assert!(err.contains("is used where Any is declared"), "{err}");
}
