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

use harness::{call_i32, compile, dedent, link_host, run_main, run_main_on, try_compile, Host};
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
            o.n += [1]
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
    harness::start_bridge(&mut store, &instance);
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
    harness::start_bridge(&mut store, &instance);
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

/// A promise awaited inside `asyncio.run()` suspends the program until it
/// settles, as Pyodide's does through JavaScript Promise Integration (which
/// the loader's `start()` gives a program, and the harness models): values,
/// a rejection as `JsException`, and promises alongside timers in tasks.
#[test]
fn promises_awaited_inside_asyncio_run() {
    prints(
        r#"
        import asyncio
        import js
        from pyodide.ffi import JsException


        async def plus_one(x: int) -> int:
            v: int = await js.Promise.resolve(x)
            await asyncio.sleep(0.01)
            return v + 1


        async def main() -> None:
            a, b = await asyncio.gather(plus_one(1), plus_one(10))
            s: str = await js.Promise.resolve("two")
            try:
                await js.Promise.reject(js.Error.new("nope"))
            except JsException as e:
                print("rejected", e)
            print(a, b, s)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "rejected Error: nope\n2 11 two\n",
    );
}

/// Without JavaScript Promise Integration nothing can settle such a promise
/// while the loop runs, so the `await` raises `RuntimeError` (this answer is
/// this compiler's own: Pyodide without JSPI refuses `asyncio.run()` itself).
#[test]
fn promise_inside_asyncio_run_without_jspi_raises() {
    let mut host = Host::default();
    host.js.jspi = false;
    let (run, _) = run_main_on(
        &dedent(
            r#"
            import asyncio
            import js


            async def main() -> None:
                v: int = await js.Promise.resolve(1)
                print(v)


            if __name__ == "__main__":
                asyncio.run(main())
            "#,
        ),
        host,
    );
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with(
            "RuntimeError: a JavaScript promise is awaited inside asyncio.run(), which cannot \
             hand control back to JavaScript to settle it here"
        ),
        "{}",
        run.stderr
    );
    assert_eq!(run.exit, 1);
}

/// Keyword arguments in a call to JavaScript are one object after the
/// others, as Pyodide passes them, and an augmented assignment to a property
/// reads it as the right-hand side's kind and sets the result.
#[test]
fn keyword_arguments_and_property_augmented_assignment() {
    prints(
        r#"
        from js import Array, JSON


        def main() -> None:
            print(JSON.stringify(Array(1, once=True, name="x")))
            o = JSON.parse('{"n": 1, "s": "a", "f": 1.5}')
            o.n += 2
            o.s += "b"
            o.f += 0.5
            o.n *= 10
            print(JSON.stringify(o))


        if __name__ == "__main__":
            main()
        "#,
        "[1,{\"once\":true,\"name\":\"x\"}]\n{\"n\":30,\"s\":\"ab\",\"f\":2}\n",
    );
}

/// A JavaScript string is the str Pyodide makes of it: its length and items
/// count code points, and an item past its end is `string index out of
/// range`. An array's missing item is the index, and an object that only has
/// a `length` is not subscriptable.
#[test]
fn js_strings_are_indexed_by_code_points() {
    prints(
        r#"
        from js import JSON


        def main() -> None:
            s = JSON.parse('"hé\U0001F600lo"')
            print(len(s), s[2] == "\U0001F600", s[3], s[-1], s[-4])
            for k in [10, -10]:
                try:
                    print(s[k])
                except IndexError as e:
                    print("str", k, e)
            a = JSON.parse("[1, 2]")
            try:
                print(a[5])
            except IndexError as e:
                print("arr", e)
            o = JSON.parse('{"length": 2, "0": "x"}')
            try:
                print(o[0])
            except TypeError as e:
                print(e)


        if __name__ == "__main__":
            main()
        "#,
        "5 True l o \u{e9}\nstr 10 string index out of range\nstr -10 string index out of range\n\
         arr 5\n'pyodide.ffi.JsProxy' object is not subscriptable\n",
    );
}

/// A module using the bridge runs its top level on first use, so a module
/// definition may reach JavaScript: the `__main__` block runs it first (and
/// reports an exception it raises as any other), and an export called from
/// the host finds it run.
#[test]
fn a_bridge_module_starts_on_first_use() {
    let source = r#"
        from js import JSON, document

        heading = document.createElement("h1")
        items = JSON.parse("[3, 4]")
        count = len(items)


        def total() -> int:
            t = 0
            for x in items:
                n: int = x
                t += n
            return t


        if __name__ == "__main__":
            print(count, total(), heading.tagName)
        "#;
    prints(source, "2 7 H1\n");
    assert_eq!(call_i32(&dedent(source), "total"), 7);
    let run = run_main(
        &dedent(
            r#"
            from js import JSON

            config = JSON.parse("[1, 2")


            def main() -> None:
                print("never")


            if __name__ == "__main__":
                main()
            "#,
        ),
        "",
        &[],
    );
    assert_eq!((run.stdout.as_str(), run.exit), ("", 1));
    assert!(
        run.stderr
            .starts_with("pyodide.ffi.JsException: SyntaxError"),
        "{}",
        run.stderr
    );
}

/// `requestAnimationFrame` with a Python callback: each frame's callbacks
/// run with the frame's time, those they ask for at the next frame, one
/// cancelled never runs, and `asyncio.run()` waits for them (the loop
/// suspends until JavaScript calls back), as Pyodide's does.
#[test]
fn animation_frames_wake_asyncio_run() {
    prints(
        r#"
        import asyncio

        from js import cancelAnimationFrame, document, performance, requestAnimationFrame
        from pyodide.ffi import create_once_callable, create_proxy

        box = document.createElement("div")
        seen: list[float] = []
        done = asyncio.Event()


        def step(t) -> None:
            seen.append(float(t))
            box.textContent = str(len(seen))
            if len(seen) == 3:
                done.set()
            else:
                requestAnimationFrame(stepper)


        stepper = create_proxy(step)


        def skipped(t) -> None:
            print("skipped frame ran")


        async def main() -> None:
            begin = float(performance.now())
            requestAnimationFrame(stepper)
            cancelAnimationFrame(requestAnimationFrame(create_once_callable(skipped)))
            await done.wait()
            stepper.destroy()
            print(len(seen), box.textContent, seen[0] < seen[1] and seen[1] < seen[2], seen[0] > begin - 1000)
            await asyncio.sleep(0.01)
            print("after a sleep", len(seen))


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "3 3 True True\nafter a sleep 3\n",
    );
}

/// `x[i] = v` of a JavaScript array or typed array, as Pyodide stores it:
/// past the end an array grows and a typed array ignores it, an index before
/// the start sets nothing an item reads, and a string or a plain object
/// refuses it with `TypeError`. A typed array's bytes come out with
/// `to_bytes()`; bytes go in with `to_js()`.
#[test]
fn item_assignment_and_buffers() {
    prints(
        r#"
        from js import JSON, Uint8Array
        from pyodide.ffi import to_js


        def main() -> None:
            a = JSON.parse("[1, 2, 3]")
            a[0] = "x"
            a[-1] = 9
            a[4] = 5
            a[-9] = 0
            print(JSON.stringify(a), len(a))
            o = JSON.parse('{"k": 1}')
            try:
                o[0] = 1
            except TypeError as e:
                print("TypeError", e)
            s = JSON.parse('"ab"')
            try:
                s[0] = "x"
            except TypeError as e:
                print("TypeError", e)
            t = Uint8Array.new(2)
            t[5] = 1
            t[-1] = 258
            t[0] += 7
            print(list(t.to_bytes()), to_js(b"hi").length)


        if __name__ == "__main__":
            main()
        "#,
        "[\"x\",2,9,null,5] 5\nTypeError 'pyodide.ffi.JsProxy' object does not support item assignment\n\
         TypeError 'str' object does not support item assignment\n[7, 2] 2\n",
    );
    refused(
        r#"
        from js import Uint8Array


        def main() -> None:
            out = bytearray(2)
            Uint8Array.new(2).assign_to(out)
        "#,
        "assign_to() hands over a writable buffer",
    );
}

/// A JavaScript value tested for truth is as true as the value Pyodide hands
/// over, in an `if`, a `while`, a conditional expression, `not`, `and`, `or`,
/// and a comprehension's filter: `null` and an empty array are false. Each
/// raised `TypeError` (the value converted to a bool, as a store into a
/// `bool` place converts it).
#[test]
fn js_values_tested_for_truth() {
    prints(
        r#"
        from js import JSON, document


        def main() -> None:
            host = document.getElementById("nothing")
            if host:
                print("statement: found")
            else:
                print("statement: none")
            print("expression:", "found" if host else "none")
            xs = JSON.parse("[]")
            ys = JSON.parse("[0, 1]")
            print("empty array:", "yes" if xs else "no", not xs, bool(ys))
            n = 0
            while not host and n < 2:
                n += 1
            print("loops", n, host is None, "both" if xs or ys else "neither")
            print([k for k in range(3) if ys and not xs])


        if __name__ == "__main__":
            main()
        "#,
        "statement: none\nexpression: none\nempty array: no True True\nloops 2 False both\n[0, 1, 2]\n",
    );
}

/// `and` / `or` over JavaScript values answer an operand, tested as Pyodide
/// tests it: `host or fallback` is the element, and a JavaScript value beside
/// a str or an int is converted to it where it is the answer (`obj.n or 5`
/// is 5 for a JavaScript 0). The answer was a bool, which raised for an
/// operand that was not one.
#[test]
fn js_values_answered_by_and_or() {
    prints(
        r#"
        from js import JSON, document


        def main() -> None:
            xs = JSON.parse("[]")
            ys = JSON.parse("[0, 1]")
            pick = xs or ys
            print(pick.length, (ys and xs).length)
            host = document.getElementById("nothing")
            fallback = document.createElement("div")
            el = host or fallback
            print(el.tagName)
            obj = JSON.parse('{"name": "", "n": 0, "label": "box"}')
            name: str = obj.name or "anon"
            print(name, obj.n or 5, obj.label or "none", "x" and obj.label)


        if __name__ == "__main__":
            main()
        "#,
        "2 0\nDIV\nanon 5 box box\n",
    );
}

/// A Python function JavaScript calls through `create_proxy`: its
/// unannotated parameters take JavaScript values, what it answers goes back
/// converted, a lambda works the same, a function called for its effect
/// answers undefined, and a destroyed proxy can no longer be called.
#[test]
fn proxies_of_python_functions() {
    prints(
        r#"
        import js
        from pyodide.ffi import JsException, create_proxy, to_js

        calls: list[str] = []


        def add(a, b) -> int:
            x: int = a
            y: int = b
            return x + y


        def note(v):
            s: str = v
            calls.append(s)


        def main() -> None:
            p = create_proxy(add)
            print(js.Reflect.apply(p, None, to_js([2, 3])))
            shout = create_proxy(lambda s: str(s).upper() + "!")
            print(js.Reflect.apply(shout, None, to_js(["hi"])))
            n = create_proxy(note)
            print(js.Reflect.apply(n, None, to_js(["seen"])), calls)
            p.destroy()
            try:
                js.Reflect.apply(p, None, to_js([1, 1]))
            except JsException as e:
                print("destroyed:", str(e).splitlines()[0])
            for i in range(200):
                q = create_proxy(lambda v: v)
                q.destroy()
            print(js.Reflect.apply(shout, None, to_js(["again"])))


        if __name__ == "__main__":
            main()
        "#,
        "5\nHI!\nNone ['seen']\ndestroyed: Error: Object has already been destroyed\nAGAIN!\n",
    );
}

/// A Python function handed to JavaScript without a proxy, and a proxy of
/// something other than a function or a lambda, are refused.
#[test]
fn proxy_refusals() {
    refused(
        r#"
        from js import document

        def on_click(e) -> None:
            pass

        def main() -> None:
            document.body.addEventListener("click", on_click)
        "#,
        "pass create_proxy(f)",
    );
    refused(
        r#"
        from pyodide.ffi import create_proxy

        def main() -> None:
            handlers = [print]
            p = create_proxy(handlers[0])
        "#,
        "take a function defined with 'def' at module level, or a lambda",
    );
    refused(
        r#"
        from pyodide.ffi import create_proxy

        def twice(x):
            return x

        def main() -> None:
            p = create_proxy(twice)
        "#,
        "returns a value but declares no return type",
    );
}

/// `pyfetch()`'s `json()` stored where no type is declared, and its
/// `signal`, are refused.
#[test]
fn fetch_refusals() {
    refused(
        r#"
        from pyodide.http import pyfetch

        async def main() -> None:
            resp = await pyfetch("https://api.test/scores")
            data = await resp.json()
            print(data)
        "#,
        "to_py() needs the type it converts into",
    );
    refused(
        r#"
        from pyodide.http import pyfetch

        async def main(s) -> None:
            resp = await pyfetch("https://api.test/scores", signal=s)
        "#,
        "pyfetch()'s 'signal' is not supported",
    );
}
