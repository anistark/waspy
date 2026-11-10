//! Memory reclamation: what is unreachable is freed and reused, and an
//! object with `__del__` is finalized when CPython would finalize it.
//!
//! The finalizer tests run a whole program and compare its output with what
//! CPython 3.13 prints for the same source, line for line, since the order of
//! the lines is the thing under test. The memory tests run a loop long enough
//! to allocate far more than the collector's threshold, and assert the
//! module's memory stays where a few rounds left it.
//!
//! The whole suite is also run with a collection forced at every statement
//! and freed memory overwritten (`just test-gc-stress`), which is what finds a
//! reference the collector cannot see. A test whose answer depends on when a
//! collection happens to run says so, and steps aside there.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{call_i32_1, dedent, instantiate, run_main, try_compile};

/// Whether the suite is being run with a collection at every statement.
fn stressed() -> bool {
    std::env::var_os("WASPY_GC_STRESS").is_some()
}

/// Run a program's `__main__` block and compare what it printed.
fn prints(source: &str, want: &str) {
    let run = run_main(&dedent(source), "", &[]);
    assert_eq!(run.stderr, "", "stderr");
    assert_eq!(run.stdout, want);
}

/// `source` is refused, and the refusal says `needle`.
fn refused(source: &str, needle: &str) {
    let err = try_compile(&dedent(source)).expect_err("this must be refused");
    assert!(err.contains(needle), "expected `{needle}` in: {err}");
}

/// Call `func(n)` and answer what it returned with the pages of memory the
/// module has afterwards.
fn pages_after(source: &str, func: &str, n: i32) -> (i32, usize) {
    let (instance, mut store) = instantiate(&dedent(source));
    let answer = instance
        .get_typed_func::<i32, i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported fn `{func}`"))
        .call(&mut store, n)
        .expect("call");
    let memory = instance
        .get_memory(&store, "memory")
        .expect("exported `memory`");
    (answer, memory.data(&store).len() / 65536)
}

const RESOURCE: &str = r#"
    class Res:
        def __init__(self, name: str):
            self.name = name
            print("open", name)

        def __del__(self):
            print("close", self.name)
"#;

/// A local's object is finalized as its function returns, a rebound name's
/// as it is rebound, a returned one when its last holder lets go, a
/// temporary at the end of its statement, and what is left as the program
/// ends.
#[test]
fn finalizer_runs_when_the_last_reference_goes() {
    prints(
        &format!(
            "{RESOURCE}{}",
            r#"
    def scoped() -> None:
        r = Res("local")
        print("in scoped")


    def rebinding() -> None:
        r = Res("first")
        r = Res("second")
        print("rebound")


    def returned() -> Res:
        r = Res("returned")
        return r


    if __name__ == "__main__":
        scoped()
        print("after scoped")
        rebinding()
        print("after rebinding")
        k = returned()
        print("holding", k.name)
        del k
        print("after del")
        Res("temp")
        print("after temp")
        g = Res("global")
        print("end")
    "#
        ),
        "open local\nin scoped\nclose local\nafter scoped\n\
         open first\nopen second\nclose first\nrebound\nclose second\nafter rebinding\n\
         open returned\nholding returned\nclose returned\nafter del\n\
         open temp\nclose temp\nafter temp\n\
         open global\nend\nclose global\n",
    );
}

const NODE: &str = r#"
    from typing import Dict, List, Optional, Tuple


    class Node:
        def __init__(self, n: int):
            self.n = n

        def __del__(self):
            print("del", self.n)
"#;

/// What a container holds is finalized when the container lets go of it,
/// in CPython's order: a list and a tuple last to first, a dict and an
/// instance's attributes first to last.
#[test]
fn containers_let_go_in_cpythons_order() {
    prints(
        &format!(
            "{NODE}{}",
            r#"
    class Box:
        def __init__(self) -> None:
            self.a = Node(1)
            self.b = Node(2)


    def fields() -> None:
        b = Box()
        print("box made")


    def dicts() -> None:
        d: Dict[str, Node] = {"x": Node(3), "y": Node(4)}
        d.clear()
        print("dict cleared")
        e: Dict[str, Node] = {"p": Node(7), "q": Node(8)}
        print("dict made")


    def tuples() -> None:
        t: Tuple[Node, Node] = (Node(5), Node(6))
        print("tuple made")


    def lists() -> None:
        xs: List[Node] = [Node(11), Node(12), Node(13)]
        del xs[0]
        print("deleted first")
        ys = xs
        xs = []
        print("still held", len(ys))


    if __name__ == "__main__":
        fields()
        print("--")
        dicts()
        print("--")
        tuples()
        print("--")
        lists()
        print("end")
    "#
        ),
        "box made\ndel 1\ndel 2\n--\n\
         del 3\ndel 4\ndict cleared\ndict made\ndel 7\ndel 8\n--\n\
         tuple made\ndel 6\ndel 5\n--\n\
         del 11\ndeleted first\nstill held 2\ndel 13\ndel 12\nend\n",
    );
}

/// `pop()` lets go of what it took out once the statement is done with it,
/// `clear()` of everything it held, last first, and an object passed to a
/// function is finalized as the function returns, before the caller goes on.
#[test]
fn removal_and_arguments() {
    prints(
        &format!(
            "{NODE}{}",
            r#"
    class Pair:
        def __init__(self, a: Node, b: Node):
            self.a = a
            self.b = b

        def __del__(self):
            print("del pair")


    def make_list() -> None:
        xs: List[Node] = [Node(1), Node(2), Node(3)]
        print("made", len(xs))
        xs.pop()
        print("popped")
        xs.clear()
        print("cleared")


    def nested() -> None:
        p = Pair(Node(10), Node(20))
        print("pair made")


    def value(n: Node) -> int:
        return n.n


    if __name__ == "__main__":
        make_list()
        print("--")
        nested()
        print("--")
        print(value(Node(7)))
        print("--")
        a = Node(100)
        b = Node(200)
        c = Node(300)
        print("end")
    "#
        ),
        "made 3\ndel 3\npopped\ndel 2\ndel 1\ncleared\n--\n\
         pair made\ndel pair\ndel 10\ndel 20\n--\n\
         del 7\n7\n--\n\
         end\ndel 100\ndel 200\ndel 300\n",
    );
}

/// Objects that refer to each other are never freed by reference counting,
/// so CPython leaves them to its collector, which in a program this small
/// runs as the interpreter exits. An object nothing else dead refers to is
/// finalized at once, as before.
#[test]
fn a_cycle_waits_for_the_collector() {
    // With a collection at every statement the cycle is found at once,
    // which is as right as CPython's collector running then would be.
    if stressed() {
        return;
    }
    prints(
        r#"
        from typing import Optional


        class Cyc:
            def __init__(self, n: int):
                self.n = n
                self.other: Optional["Cyc"] = None

            def __del__(self):
                print("del", self.n)


        class Plain:
            def __init__(self, n: int):
                self.n = n

            def __del__(self):
                print("plain", self.n)


        def cycle() -> None:
            a = Cyc(1)
            b = Cyc(2)
            a.other = b
            b.other = a
            print("cycle made")


        if __name__ == "__main__":
            cycle()
            print("after cycle")
            p = Plain(5)
            p = None
            print("end")
        "#,
        "cycle made\nafter cycle\nplain 5\nend\ndel 1\ndel 2\n",
    );
}

/// A generator left part way through a `try` is closed when the loop that
/// was walking it lets go of it, so its `finally` runs there.
#[test]
fn an_abandoned_generator_runs_its_finally() {
    prints(
        r#"
        def numbers():
            try:
                yield 1
                yield 2
                yield 3
            finally:
                print("cleanup")


        def first() -> int:
            found = 0
            for n in numbers():
                if n == 2:
                    found = n
                    break
            print("after loop")
            return found


        def whole() -> int:
            total = 0
            for n in numbers():
                total += n
            print("summed")
            return total


        if __name__ == "__main__":
            print(first())
            print(whole())
            print("end")
        "#,
        "cleanup\nafter loop\n2\ncleanup\nsummed\n6\nend\n",
    );
}

/// A `with` statement holds its manager until the block is done.
#[test]
fn a_context_manager_is_let_go_after_its_block() {
    prints(
        r#"
        class Ctx:
            def __enter__(self) -> "Ctx":
                print("enter")
                return self

            def __exit__(self, a, b, c) -> bool:
                print("exit")
                return False

            def __del__(self):
                print("del ctx")


        def managed() -> None:
            with Ctx():
                print("body")
            print("after with")


        if __name__ == "__main__":
            managed()
            print("end")
        "#,
        "enter\nbody\nexit\ndel ctx\nafter with\nend\n",
    );
}

/// A finalizer runs once, even when the object is reached again from it,
/// and one that raises does not stop the program.
#[test]
fn a_finalizer_that_raises_is_passed_over() {
    let run = run_main(
        &dedent(
            r#"
        class Loud:
            def __del__(self):
                print("going")
                raise ValueError("no")


        def f() -> int:
            x = Loud()
            return 1


        if __name__ == "__main__":
            print(f())
            print("end")
        "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stdout, "going\n1\nend\n");
    assert_eq!(run.exit, 0);
}

/// `del name` unbinds a local, a module variable, and a name the loop
/// rebinds each time round.
#[test]
fn del_of_a_name() {
    prints(
        r#"
        def f(n: int) -> int:
            total = 0
            for i in range(n):
                square = i * i
                total += square
                del square
            return total


        if __name__ == "__main__":
            words = ["a", "b"]
            count = len(words)
            del words
            print(count, f(4))
        "#,
        "2 14\n",
    );
}

/// Reading a name after its `del` is NameError in CPython, which a compiled
/// variable cannot raise, so it is refused: after the `del`, on another
/// path, on a loop's next turn, and from another function.
#[test]
fn a_name_read_after_its_del_is_refused() {
    refused(
        r#"
        def f() -> int:
            x = 1
            del x
            return x
        "#,
        "'x' is used after 'del x'",
    );
    refused(
        r#"
        def f(flag: bool) -> int:
            x = 1
            if flag:
                del x
            return x
        "#,
        "'x' is used after 'del x'",
    );
    refused(
        r#"
        def f(n: int) -> int:
            x = 1
            total = 0
            for i in range(n):
                total += x
                del x
            return total
        "#,
        "'x' is used after 'del x'",
    );
    refused(
        r#"
        limit = 10


        def over(n: int) -> bool:
            return n > limit


        def forget() -> None:
            global limit
            del limit
        "#,
        "deletes a module variable that 'over' also uses",
    );
    // Assigned again first, it is a name like any other.
    assert!(try_compile(&dedent(
        r#"
        def f() -> int:
            x = 1
            del x
            x = 2
            return x
        "#
    ))
    .is_ok());
}

const CHURN: &str = r#"
    from typing import Dict, List


    class Point:
        def __init__(self, x: int, y: int):
            self.x = x
            self.y = y

        def norm1(self) -> int:
            return abs(self.x) + abs(self.y)


    def build(n: int) -> List[Point]:
        out: List[Point] = []
        for i in range(n):
            out.append(Point(i, -i))
        return out


    def churn(rounds: int) -> int:
        total = 0
        keep: Dict[int, str] = {}
        for r in range(rounds):
            pts = build(50)
            names = [str(p.norm1()) + "-" + str(r) for p in pts]
            total += len(names) + len("".join(names))
            keep[r % 7] = names[r % 50]
            d: Dict[int, List[int]] = {i: [i, i + 1, i + 2] for i in range(20)}
            total += sum(len(v) for v in d.values())
        return total + len(keep)
"#;

/// A loop that builds and drops collections, instances, and strings gives
/// CPython's answer however long it runs, in the memory a few rounds took.
#[test]
fn a_long_loop_stays_in_bounded_memory() {
    let (few, _) = pages_after(CHURN, "churn", 3);
    assert_eq!(few, 918);
    // A collection at every statement makes the long runs take hours, and
    // says nothing about how memory grows between collections.
    if stressed() {
        return;
    }
    let (answer, after_some) = pages_after(CHURN, "churn", 300);
    assert_eq!(answer, 116007);
    let (answer, after_many) = pages_after(CHURN, "churn", 3000);
    assert_eq!(answer, 1309507);
    // Ten times the work, and it allocates megabytes: the same memory.
    assert_eq!(
        after_many, after_some,
        "memory grew with the number of rounds"
    );
    assert!(after_many <= 32, "{after_many} pages for a small live set");
}

/// What a comprehension's element throws away goes each time round, not when
/// the statement ends: a single statement that makes a great many temporary
/// strings stays in bounded memory too.
#[test]
fn a_comprehension_drops_its_temporaries_each_time_round() {
    let source = r#"
        def lengths(n: int) -> int:
            return sum([len(str(i) + "-" + str(i * 2)) for i in range(n)])
    "#;
    assert_eq!(call_i32_1(&dedent(source), "lengths", 10), 35);
    if stressed() {
        return;
    }
    let (answer, pages) = pages_after(source, "lengths", 200000);
    assert_eq!(answer, 2433335);
    // The list it builds is 1.6 MB (25 pages), and the collector lets the
    // heap reach about twice what is alive. The strings it throws away come
    // to some 14 MB, which kept would be more than 200 pages.
    assert!(pages < 100, "{pages} pages: the temporaries were kept");
}

/// Objects that only refer to each other are reclaimed like any others: a
/// loop that leaves a cycle behind each time round does not grow.
#[test]
fn cycles_are_reclaimed() {
    let source = r#"
        from typing import List, Optional


        class Link:
            def __init__(self, n: int):
                self.n = n
                self.next: Optional["Link"] = None
                self.pad: List[int] = [n] * 64


        def ring(size: int) -> int:
            first = Link(0)
            last = first
            for i in range(1, size):
                link = Link(i)
                last.next = link
                last = link
            last.next = first
            return last.n


        def rings(rounds: int) -> int:
            total = 0
            for r in range(rounds):
                total += ring(20)
            return total
    "#;
    let (answer, _) = pages_after(source, "rings", 2);
    assert_eq!(answer, 38);
    if stressed() {
        return;
    }
    let (answer, after_some) = pages_after(source, "rings", 200);
    assert_eq!(answer, 3800);
    let (answer, after_many) = pages_after(source, "rings", 4000);
    assert_eq!(answer, 76000);
    assert_eq!(after_many, after_some, "the cycles were kept");
}

/// A freed block is handed out again for a request of its size: a loop that
/// keeps one object of each of several sizes alive at a time runs in the
/// memory its first rounds took, with no growth at all.
#[test]
fn freed_blocks_are_reused_by_size() {
    let source = r#"
        from typing import List


        def sizes(rounds: int) -> int:
            total = 0
            for r in range(rounds):
                a = [r] * 3
                b = [r] * 17
                c = "x" * 40 + str(r)
                d = (r, r + 1)
                total += len(a) + len(b) + len(c) + d[1] - r
            return total
    "#;
    let (answer, _) = pages_after(source, "sizes", 5);
    assert_eq!(answer, 310);
    if stressed() {
        return;
    }
    let (answer, after_some) = pages_after(source, "sizes", 2000);
    assert_eq!(answer, 128890);
    let (answer, after_many) = pages_after(source, "sizes", 40000);
    assert_eq!(answer, 2628890);
    assert_eq!(after_many, after_some);
}

/// A value returned to the host stays valid across later calls into the
/// module, however much they allocate and collect.
#[test]
fn what_the_host_was_handed_is_kept() {
    let source = dedent(
        r#"
        from typing import List


        def make() -> List[int]:
            return [7, 8, 9]


        def churn(rounds: int) -> int:
            total = 0
            for r in range(rounds):
                junk = [r] * 200
                total += len(junk)
            return total


        def second(xs: List[int]) -> int:
            return xs[1]
        "#,
    );
    // Enough to collect many times over; with a collection at every
    // statement, a few rounds are.
    let rounds = if stressed() { 50 } else { 5000 };
    let (instance, mut store) = instantiate(&source);
    let list = instance
        .get_typed_func::<(), i32>(&store, "make")
        .expect("make")
        .call(&mut store, ())
        .expect("call");
    let churned = instance
        .get_typed_func::<i32, i32>(&store, "churn")
        .expect("churn")
        .call(&mut store, rounds)
        .expect("call");
    assert_eq!(churned, rounds * 200);
    let second = instance
        .get_typed_func::<i32, i32>(&store, "second")
        .expect("second")
        .call(&mut store, list)
        .expect("call");
    assert_eq!(second, 8);
}
