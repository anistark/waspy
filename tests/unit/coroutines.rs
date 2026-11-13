//! Coroutines and `asyncio`: `async def` and `await` on the generator
//! machinery, and CPython 3.13's event loop, tasks, and futures written out in
//! IR.
//!
//! Every expected value is what CPython 3.13 prints for the same source. The
//! harness's monotonic clock stands still but for what the program waits, so
//! timers fall due in order without the tests waiting for them.

#[path = "../utils/harness.rs"]
mod harness;

use harness::{dedent, run_main, try_compile};

/// Run a program's `__main__` block and compare its standard output.
fn prints(source: &str, want: &str) {
    let run = run_main(&dedent(source), "", &[]);
    assert_eq!(run.stderr, "", "stderr");
    assert_eq!(run.stdout, want);
}

fn refused(source: &str, needle: &str) {
    let err = try_compile(&dedent(source)).expect_err("this construct must be refused");
    assert!(err.contains(needle), "expected `{needle}` in: {err}");
}

/// `asyncio.run()` runs a coroutine to completion, awaiting another inline.
#[test]
fn asyncio_run_awaits_a_coroutine() {
    prints(
        r#"
        import asyncio


        async def double(x: int) -> int:
            return x * 2


        async def main() -> None:
            r = await double(21)
            print("result", r)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "result 42\n",
    );
}

/// Tasks start in the order they are created and wake as their timers fall due.
#[test]
fn asyncio_tasks_run_in_timer_order() {
    prints(
        r#"
        import asyncio


        async def worker(name: str, delay: float) -> str:
            print("start", name)
            await asyncio.sleep(delay)
            print("end", name)
            return name.upper()


        async def main() -> None:
            t1 = asyncio.create_task(worker("a", 0.02))
            t2 = asyncio.create_task(worker("b", 0.01))
            print("created")
            r1 = await t1
            r2 = await t2
            print(r1, r2, t1.done(), t2.get_name())


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "created\nstart a\nstart b\nend b\nend a\nA B True Task-3\n",
    );
}

/// `gather()` answers in argument order, whatever order its children finish in; a starred list works, and results of different types unpack.
#[test]
fn asyncio_gather_keeps_argument_order() {
    prints(
        r#"
        import asyncio


        async def square(x: int) -> int:
            await asyncio.sleep(0.01 * (5 - x))
            print("square", x)
            return x * x


        async def label(x: int) -> str:
            await asyncio.sleep(0)
            return "n" + str(x)


        async def main() -> None:
            rs = await asyncio.gather(square(1), square(2), square(3))
            print(rs)
            n, s = await asyncio.gather(square(4), label(9))
            print(n, s)
            coros = [square(i) for i in range(3)]
            print(await asyncio.gather(*coros))


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "square 3\nsquare 2\nsquare 1\n[1, 4, 9]\nsquare 4\n16 n9\nsquare 2\nsquare 1\nsquare 0\n[0, 1, 4]\n",
    );
}

/// An exception raised in a coroutine is caught across an `await`, re-raised by awaiting the task it failed, and fails a `gather()`.
#[test]
fn asyncio_exceptions_cross_awaits_and_tasks() {
    prints(
        r#"
        import asyncio


        async def fail(msg: str) -> int:
            await asyncio.sleep(0)
            raise ValueError(msg)


        async def safe() -> int:
            try:
                return await fail("boom")
            except ValueError as e:
                print("caught", e)
                return -1
            finally:
                print("finally")


        async def main() -> None:
            print(await safe())
            t = asyncio.create_task(fail("later"))
            try:
                await t
            except ValueError as e:
                print("task failed:", e, t.done(), t.cancelled())
            try:
                await asyncio.gather(square_ok(), fail("in gather"))
            except ValueError as e:
                print("gather failed:", e)


        async def square_ok() -> int:
            await asyncio.sleep(0.01)
            print("square_ok done")
            return 4


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "caught boom\nfinally\n-1\ntask failed: later True False\ngather failed: in gather\n",
    );
}

/// `wait_for()` cancels what it waits on at the deadline, which runs its `finally`, and raises TimeoutError.
#[test]
fn asyncio_wait_for_times_out() {
    prints(
        r#"
        import asyncio


        async def slow(x: int) -> int:
            try:
                await asyncio.sleep(1)
                return x
            finally:
                print("slow cleanup", x)


        async def quick(x: int) -> int:
            await asyncio.sleep(0.01)
            return x + 1


        async def main() -> None:
            try:
                await asyncio.wait_for(slow(1), timeout=0.05)
            except TimeoutError:
                print("timed out")
            r = await asyncio.wait_for(quick(5), 1.0)
            print("quick", r)
            try:
                await asyncio.wait_for(asyncio.sleep(10), timeout=0.01)
            except asyncio.TimeoutError:
                print("sleep timed out")


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "slow cleanup 1\ntimed out\nquick 6\nsleep timed out\n",
    );
}

/// Cancelling a task throws CancelledError in where it waits; its handlers and `finally` run, and the task ends cancelled.
#[test]
fn asyncio_cancel_runs_cleanup() {
    prints(
        r#"
        import asyncio


        async def ticker(n: int) -> None:
            try:
                for i in range(n):
                    print("tick", i)
                    await asyncio.sleep(0.01)
            except asyncio.CancelledError:
                print("ticker cancelled")
                raise
            finally:
                print("ticker finally")


        async def main() -> None:
            t = asyncio.create_task(ticker(10))
            await asyncio.sleep(0.035)
            print("cancel ->", t.cancel())
            try:
                await t
            except asyncio.CancelledError:
                print("main saw cancel", t.cancelled(), t.done())


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "tick 0\ntick 1\ntick 2\ntick 3\ncancel -> True\nticker cancelled\nticker finally\nmain saw cancel True True\n",
    );
}

/// `sleep(0)` lets every other ready task run once, in the order they became ready.
#[test]
fn asyncio_sleep_zero_interleaves() {
    prints(
        r#"
        import asyncio


        async def chatter(name: str, n: int) -> None:
            for i in range(n):
                print(name, i)
                await asyncio.sleep(0)


        async def main() -> None:
            a = asyncio.create_task(chatter("a", 3))
            b = asyncio.create_task(chatter("b", 2))
            await chatter("main", 2)
            await a
            await b
            print("done")


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "main 0\na 0\nb 0\nmain 1\na 1\nb 1\na 2\ndone\n",
    );
}

/// An `async def` method makes a coroutine over its instance.
#[test]
fn asyncio_coroutine_methods() {
    prints(
        r#"
        import asyncio


        class Counter:
            def __init__(self, start: int) -> None:
                self.value = start

            async def bump(self, by: int) -> int:
                await asyncio.sleep(0)
                self.value += by
                return self.value


        async def main() -> int:
            c = Counter(10)
            print(await c.bump(1))
            print(await c.bump(5))
            total = 0
            for k in range(3):
                total += await c.bump(k)
            print("total", total, c.value)
            return c.value


        if __name__ == "__main__":
            print("run ->", asyncio.run(main()))
        "#,
        "11\n16\ntotal 52 19\nrun -> 19\n",
    );
}

/// `asyncio.run()` cancels the tasks left pending when its coroutine finishes, running their cleanup before it returns.
#[test]
fn asyncio_run_cancels_what_is_left() {
    prints(
        r#"
        import asyncio


        async def forever(name: str) -> None:
            try:
                while True:
                    await asyncio.sleep(0.01)
            finally:
                print("cleanup", name)


        async def main() -> str:
            asyncio.create_task(forever("x"))
            asyncio.create_task(forever("y"))
            await asyncio.sleep(0.025)
            print("main done")
            return "ok"


        if __name__ == "__main__":
            print(asyncio.run(main()))
            print("after")
        "#,
        "main done\ncleanup x\ncleanup y\nok\nafter\n",
    );
}

/// An `await` inside an expression is evaluated in CPython's order: what comes before it first (a global read before it keeps its value), in conditions, loops, and augmented assignments too.
#[test]
fn asyncio_awaits_in_expressions_keep_order() {
    prints(
        r#"
        import asyncio

        counter = 0


        async def bump(n: int) -> int:
            global counter
            await asyncio.sleep(0)
            counter += n
            return counter


        async def fetch(x: int) -> int:
            await asyncio.sleep(0)
            return x * 10


        async def main() -> None:
            a = [1, 2]
            a.append(await fetch(3))
            print(a)
            print("sum", await fetch(1) + await fetch(2))
            s = "n=" + str(await fetch(4))
            print(s)
            d = {"k": await fetch(5)}
            print(d)
            i = 0
            while await fetch(i) < 30:
                print("loop", i)
                i += 1
            for v in [await fetch(7), await fetch(8)]:
                print("v", v)
            if await fetch(1) > 5:
                print("big")
            total = 0
            total += await fetch(2)
            print("total", total)
            print("bump", counter + await bump(5), counter)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "[1, 2, 30]\nsum 30\nn=40\n{'k': 50}\nloop 0\nloop 1\nloop 2\nv 70\nv 80\nbig\ntotal 20\nbump 5 5\n",
    );
}

/// `wait_for()` with a timeout of zero cancels a task that is not done and raises TimeoutError; with None it just awaits.
#[test]
fn asyncio_wait_for_edge_timeouts() {
    prints(
        r#"
        import asyncio


        async def boom() -> None:
            await asyncio.sleep(0)
            raise RuntimeError("bad")


        async def main() -> None:
            try:
                r = await asyncio.wait_for(boom(), 1.0)
            except RuntimeError as e:
                print("got", e)
            t = asyncio.create_task(asyncio.sleep(0.5))
            try:
                await asyncio.wait_for(t, 0)
            except TimeoutError:
                print("zero timeout", t.cancelled())
            r2 = await asyncio.wait_for(asyncio.sleep(0), None)
            print("none timeout", r2)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "got bad\nzero timeout True\nnone timeout None\n",
    );
}

/// Cancelling a task waiting on `gather()` cancels the children still running.
#[test]
fn asyncio_cancelling_a_gather_cancels_its_children() {
    prints(
        r#"
        import asyncio
        from typing import List


        async def child(n: int) -> int:
            try:
                await asyncio.sleep(0.01 * n)
                print("child done", n)
                return n
            except asyncio.CancelledError:
                print("child cancelled", n)
                raise


        async def main() -> None:
            g = asyncio.create_task(gatherer())
            await asyncio.sleep(0.015)
            g.cancel()
            try:
                await g
            except asyncio.CancelledError:
                print("gatherer cancelled")


        async def gatherer() -> List[int]:
            return await asyncio.gather(child(1), child(2), child(3))


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "child done 1\nchild cancelled 2\nchild cancelled 3\ngatherer cancelled\n",
    );
}

/// `asyncio.run()` can run again once it has returned, and returns its coroutine's result.
#[test]
fn asyncio_run_twice() {
    prints(
        r#"
        import asyncio


        async def add(a: int, b: int) -> int:
            return a + b


        async def main() -> int:
            x = await add(1, 2)
            y = await add(x, 10)
            return y


        if __name__ == "__main__":
            print(asyncio.run(main()))
            print(asyncio.run(main()))
            t = asyncio.run(add(5, 6))
            print(t)
        "#,
        "13\n13\n11\n",
    );
}

/// Tasks are named `Task-<n>` from one count across runs, which each run's shutdown steps advance by two, as CPython's do.
#[test]
fn asyncio_task_names_count_on() {
    prints(
        r#"
        import asyncio


        async def named() -> str:
            await asyncio.sleep(0)
            return "x"


        async def main() -> None:
            t1 = asyncio.create_task(named())
            t2 = asyncio.create_task(named())
            print(t1.get_name(), t2.get_name())
            await t1
            await t2
            print(t1.result(), t2.done())


        if __name__ == "__main__":
            asyncio.run(main())
            asyncio.run(main())
        "#,
        "Task-2 Task-3\nx True\nTask-7 Task-8\nx True\n",
    );
}

/// A task's exception nothing retrieves is reported on standard error, with
/// the first and last lines of CPython's report (its `future:` line names the
/// script's path, and the traceback is not kept). `sys.exit()` in a task
/// ends the program with its status, after the loop closes.
#[test]
fn asyncio_reports_exceptions_never_retrieved() {
    let run = run_main(
        &dedent(
            r#"
            import asyncio
            import sys


            async def bad(n: int) -> None:
                await asyncio.sleep(0)
                raise ValueError("bad " + str(n))


            async def quitter() -> None:
                await asyncio.sleep(0)
                sys.exit(3)


            async def main() -> None:
                asyncio.create_task(bad(1))
                t = asyncio.create_task(bad(2))
                u = asyncio.create_task(bad(3))
                await asyncio.sleep(0.01)
                try:
                    await u
                except ValueError as e:
                    print("retrieved", e)
                print("end of main")
                await quitter()


            if __name__ == "__main__":
                asyncio.run(main())
                print("not reached")
            "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stdout, "retrieved bad 3\nend of main\n");
    assert_eq!(
        run.stderr,
        "Task exception was never retrieved\nValueError: bad 1\n\
         Task exception was never retrieved\nValueError: bad 2\n"
    );
    assert_eq!(run.exit, 3);
}

/// An exception `asyncio.run()`'s coroutine raises comes out of it, as an
/// uncaught one does: the last line of CPython's report, and status 1.
#[test]
fn asyncio_run_raises_what_its_coroutine_raises() {
    let run = run_main(
        &dedent(
            r#"
            import asyncio


            async def main() -> int:
                await asyncio.sleep(0)
                raise KeyError("missing")


            if __name__ == "__main__":
                try:
                    asyncio.run(main())
                except KeyError as e:
                    print("caught", e)
                asyncio.run(main())
            "#,
        ),
        "",
        &[],
    );
    assert_eq!(run.stdout, "caught 'missing'\n");
    assert_eq!(run.stderr, "KeyError: 'missing'\n");
    assert_eq!(run.exit, 1);
}

/// What CPython raises for misuse at run time: `asyncio.run()` inside a
/// running loop, a task made with no loop running, a result read too early.
#[test]
fn asyncio_misuse_raises_cpython_errors() {
    prints(
        r#"
        import asyncio


        async def inner() -> int:
            return 1


        async def main() -> None:
            c = inner()
            try:
                asyncio.run(c)
            except RuntimeError as e:
                print(e)
            await c
            t = asyncio.create_task(inner())
            try:
                t.result()
            except asyncio.InvalidStateError as e:
                print(type(e).__name__, e)
            print(await t, t.result(), t.exception())


        if __name__ == "__main__":
            try:
                asyncio.create_task(inner())
            except RuntimeError as e:
                print(e)
            asyncio.run(main())
        "#,
        "no running event loop\n\
         asyncio.run() cannot be called from a running event loop\n\
         InvalidStateError Result is not ready.\n\
         1 1 None\n",
    );
}

/// The constructs CPython rejects, or that would be answered wrong, are
/// refused at compile time.
#[test]
fn coroutine_misuse_is_refused() {
    refused(
        r#"
        import asyncio

        async def f() -> int:
            return 1

        async def main() -> None:
            f()
        "#,
        "is never awaited",
    );
    refused(
        r#"
        def f() -> int:
            await g()
            return 1
        "#,
        "'await' outside an async function",
    );
    refused(
        r#"
        async def ticks() -> None:
            yield 1
        "#,
        "asynchronous generator",
    );
    refused(
        r#"
        async def f(x: int) -> int:
            return x

        async def g(c: bool) -> int:
            return 1 if c else await f(2)
        "#,
        "conditional expression",
    );
    refused(
        r#"
        async def f(x: int) -> int:
            return x

        async def g(c: bool) -> bool:
            return c and await f(2) > 1
        "#,
        "right operand of 'and'/'or'",
    );
    refused(
        r#"
        async def f(x: int) -> int:
            return x

        async def g() -> int:
            return sum([await f(i) for i in range(3)])
        "#,
        "comprehension",
    );
    refused(
        r#"
        async def f():
            return 1
        "#,
        "declares no return type",
    );
    refused(
        r#"
        import asyncio

        async def main() -> None:
            await asyncio.wait([])
        "#,
        "asyncio.wait() is not supported",
    );
    refused(
        r#"
        import asyncio

        async def f() -> int:
            return 1

        async def main() -> None:
            g = asyncio.gather(f())
            await g
        "#,
        "awaited directly",
    );
    refused(
        r#"
        async def main() -> None:
            await 5
        "#,
        "can't be used in 'await' expression",
    );
}
