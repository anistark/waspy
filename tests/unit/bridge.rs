//! The JavaScript bridge: `js` and `pyodide.ffi` as Pyodide has them, run
//! here against the harness's small JavaScript (`harness::JsWorld`).
//!
//! Every expected value is what Pyodide 0.29.5 prints for the same source
//! (the browser gate, `just browser-verify`, checks the same programs
//! against Pyodide itself in Chromium), except where a test says the answer
//! is this compiler's own: a value stored where another kind is declared
//! raises `TypeError` here, where Pyodide keeps it.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{compile, dedent, link_host, run_main, try_compile, Host};
use wasmi::{Engine, Linker, Module, Store, Value};

fn prints(source: &str, want: &str) {
    let run = run_main(&dedent(source), "", &[]);
    assert_eq!(run.stderr, "", "stderr");
    assert_eq!(run.stdout, want);
}

fn refused(source: &str, needle: &str) {
    let err = try_compile(&dedent(source)).expect_err("this construct must be refused");
    assert!(err.contains(needle), "expected `{needle}` in: {err}");
}

/// Attributes, calls, `new`, items, iteration, `len`, conversions at a typed
/// store, `str()`, `repr()`, `int()`, `float()`, `bool()`, `==`, `is None`,
/// `.to_py()`, `to_js()`, `JsException`, and a function held in a variable.
#[test]
fn bridge_reads_writes_calls_and_converts() {
    prints(
        r#"
        import js
        from js import JSON, Math, Object
        from pyodide.ffi import JsException, JsProxy, to_js


        def total(arr: JsProxy) -> int:
            t = 0
            for v in arr:
                n: int = v
                t += n
            return t


        def main() -> None:
            print(Math.max(3, 7), Math.PI, Math.floor(2.7), Math.max(1.5, 2))
            n: int = Math.max(1, 2)
            f: float = Math.PI
            print(n + 1, round(f * 2, 3))
            o = JSON.parse('{"a": 1, "s": "hi", "xs": [10, 20, 30], "t": true, "z": null}')
            print(o.a, o.s, o.t, o.z, o.xs, repr(o.s))
            s: str = o.s
            print(s.upper(), len(o.xs), o.xs[1], o.xs[-1], total(o.xs))
            o.b = "new"
            o.c = 2.5
            o.d = None
            print(JSON.stringify(o))
            xs: list[int] = o.xs.to_py()
            print(sum(xs), xs)
            d: dict[str, int] = JSON.parse('{"x": 1, "y": 2}').to_py()
            print(d)
            print(JSON.stringify(to_js([1, 2, 3])), JSON.stringify(to_js({"k": [1, 2]})))
            e = Object.new()
            e.name = "ada"
            print(e.name, str(e))
            for k in Object.keys(o):
                print("key", k)
            try:
                JSON.parse("{")
            except JsException:
                print("JsException")
            try:
                print(o.missing)
            except AttributeError as err:
                print("AttributeError", err)
            try:
                print(o.xs[9])
            except IndexError as err:
                print("IndexError", err)
            print(o.a == 1, o.s == "hi", o.z is None, 1 == o.a, bool(o.xs), bool(JSON.parse("[]")))
            print(int(Math.PI), float(Math.max(1, 2)), str(Math.max(1, 2)), js.Number(2 ** 30))
            biggest = Math.max
            print(biggest(4, 9, 2))


        if __name__ == "__main__":
            main()
        "#,
        "7 3.141592653589793 2 2\n3 6.283\n1 hi True jsnull 10,20,30 'hi'\nHI 3 20 30 60\n{\"a\":1,\"s\":\"hi\",\"xs\":[10,20,30],\"t\":true,\"z\":null,\"b\":\"new\",\"c\":2.5}\n60 [10, 20, 30]\n{'x': 1, 'y': 2}\n[1,2,3] {\"k\":[1,2]}\nada [object Object]\nkey a\nkey s\nkey xs\nkey t\nkey z\nkey b\nkey c\nkey d\nJsException\nAttributeError missing\nIndexError 9\nTrue True False True True False\n3 2.0 2 1073741824\n9\n",
    );
}

/// A JS value stored where another kind is declared raises `TypeError`
/// (this compiler's answer: Pyodide keeps the value whatever the annotation
/// says, which a typed place cannot hold), and so does `int()` of a JS
/// string (Pyodide parses it), with a hint to convert it first.
#[test]
fn bridge_refuses_values_of_another_kind_at_run_time() {
    prints(
        r#"
        from js import JSON, Math


        def main() -> None:
            o = JSON.parse('{"s": "hi", "n": 2}')
            try:
                bad: int = o.s
            except TypeError as e:
                print("TypeError:", e)
            try:
                f: float = o.n
            except TypeError as e:
                print("TypeError:", e)
            try:
                print(int(o.s))
            except TypeError as e:
                print("TypeError:", e)
            print(int(str(o.n)) + 1)


        if __name__ == "__main__":
            main()
        "#,
        "TypeError: the JavaScript value is a string, where int is declared\n\
         TypeError: the JavaScript value is an int, where float is declared\n\
         TypeError: int() of a JavaScript string is not supported here. Hint: int(str(x))\n\
         3\n",
    );
}

/// What the bridge cannot give Pyodide's answer for is refused when the
/// program is compiled.
#[test]
fn bridge_refusals() {
    refused(
        r#"
        from js import JSON

        def f() -> None:
            JSON.stringify([1, 2])
        "#,
        "Hint: pass to_js(x)",
    );
    refused(
        r#"
        from js import JSON

        def f() -> list:
            xs: list[int] = JSON.parse("[1]")
            return xs
        "#,
        "convert it with .to_py()",
    );
    refused(
        r#"
        from js import JSON

        def f() -> int:
            x = JSON.parse("[1]").to_py()
            return 0
        "#,
        "to_py() needs the type it converts into",
    );
    refused(
        r#"
        from js import JSON

        def f() -> int:
            o = JSON.parse('{"n": 1}')
            o.n += 1
            return 0
        "#,
        "augmented assignment to the JavaScript property",
    );
}

/// The handles Python lets go of go back to JavaScript: a loop making a JS
/// value on each round leaves the table about as full as one round does.
#[test]
fn bridge_gives_handles_back() {
    let source = dedent(
        r#"
        from js import JSON


        def churn(n: int) -> int:
            total = 0
            for i in range(n):
                tmp = JSON.parse("[1, 2, 3]")
                total += len(tmp)
            return total
        "#,
    );
    let wasm = compile(&source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm[..]).expect("valid module");
    let mut store = Store::new(&engine, Host::default());
    let mut linker = Linker::<Host>::new(&engine);
    link_host(&mut linker);
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate")
        .start(&mut store)
        .expect("start");
    let churn = instance
        .get_typed_func::<i32, i32>(&store, "churn")
        .expect("churn");
    assert_eq!(churn.call(&mut store, 5000).expect("call"), 15000);
    let live = store.data().js.live();
    assert!(live < 1000, "{live} handles still held after 5000 rounds");
}

/// An export called from JavaScript through its wrapper (`__jsx_<name>`):
/// handles in, a handle out, a str and a list converted by the signature,
/// and a Python exception reported for the loader to throw.
#[test]
fn bridge_exports_take_and_answer_js_values() {
    use harness::Js;
    let source = dedent(
        r#"
        from pyodide.ffi import JsProxy


        def greet(name: str, times: int) -> str:
            return ("hi " + name + "! ") * times


        def mean(xs: list[int]) -> float:
            return sum(xs) / len(xs)


        def fails(n: int) -> int:
            if n < 0:
                raise ValueError("negative: " + str(n))
            return n * 2


        def inner(o: JsProxy) -> JsProxy:
            return o.inner
        "#,
    );
    let wasm = compile(&source);
    let engine = Engine::default();
    let module = Module::new(&engine, &wasm[..]).expect("valid module");
    let mut store = Store::new(&engine, Host::default());
    let mut linker = Linker::<Host>::new(&engine);
    link_host(&mut linker);
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate")
        .start(&mut store)
        .expect("start");
    let mut call = |name: &str, args: Vec<Js>| -> Result<Js, String> {
        let handles: Vec<Value> = args
            .into_iter()
            .map(|a| Value::I32(store.data_mut().js.add(a)))
            .collect();
        let f = instance
            .get_func(&store, &format!("__jsx_{name}"))
            .expect("a wrapper");
        let mut out = [Value::I32(0)];
        f.call(&mut store, &handles, &mut out).expect("call");
        let r = out[0].i32().unwrap();
        if r == -2 {
            return Err(store.data_mut().js.failure.take().unwrap_or_default());
        }
        let v = store.data().js.value(r);
        store.data_mut().js.drop_handle(r);
        Ok(v)
    };
    let text = |v: Result<Js, String>| v.map(|v| v.text()).unwrap_or_else(|e| e);
    assert_eq!(
        text(call("greet", vec![Js::Str("Ada".into()), Js::Num(2.0)])),
        "hi Ada! hi Ada! "
    );
    let xs = Js::Arr(std::rc::Rc::new(std::cell::RefCell::new(vec![
        Js::Num(1.0),
        Js::Num(2.0),
        Js::Num(6.0),
    ])));
    assert_eq!(text(call("mean", vec![xs])), "3");
    assert_eq!(text(call("fails", vec![Js::Num(21.0)])), "42");
    assert_eq!(
        text(call("fails", vec![Js::Num(-3.0)])),
        "ValueError: negative: -3"
    );
    assert_eq!(
        text(call("greet", vec![Js::Num(5.0), Js::Num(1.0)])),
        "TypeError: the JavaScript value is an int, where str is declared"
    );
    let o = Js::object(vec![("inner", Js::Str("deep".into()))]);
    assert_eq!(text(call("inner", vec![o])), "deep");
}
