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
/// `async with` awaits `__aenter__` and `__aexit__`, and `async for` awaits
/// `__anext__` until it raises `StopAsyncIteration`; an unannotated
/// `__aiter__` or `__aenter__` that answers `self` is typed as its class.
#[test]
fn async_with_and_async_for_run_the_protocols() {
    prints(
        r#"
        import asyncio


        class Conn:
            def __init__(self, name: str) -> None:
                self.name = name

            async def __aenter__(self):
                print("open", self.name)
                await asyncio.sleep(0)
                return self

            async def __aexit__(self, exc_type, exc, tb) -> bool:
                print("close", self.name, exc is None)
                return False


        class Ticker:
            def __init__(self, n: int) -> None:
                self.i = 0
                self.n = n

            def __aiter__(self):
                return self

            async def __anext__(self) -> int:
                if self.i >= self.n:
                    raise StopAsyncIteration
                self.i += 1
                await asyncio.sleep(0)
                return self.i * 10


        async def main() -> None:
            async with Conn("db") as c:
                print("using", c.name)
            total = 0
            async for v in Ticker(3):
                print("tick", v)
                total += v
            else:
                print("done", total)
            try:
                async with Conn("x"):
                    raise ValueError("boom")
            except ValueError as e:
                print("caught", e)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "open db\nusing db\nclose db True\ntick 10\ntick 20\ntick 30\ndone 60\nopen x\nclose x False\ncaught boom\n",
    );
}

/// `async with` on every way out: suppression by `__aexit__`, a `return`, a
/// `break` and a `continue` through it, several managers in one statement, an
/// `__aexit__` answering None, and a task cancelled inside one (whose manager
/// suppresses the cancellation, so the task finishes). `async for` unpacks a
/// tuple target and runs its `else` only without a `break`.
#[test]
fn async_with_and_async_for_on_every_exit() {
    prints(
        r#"
        import asyncio


        class Quiet:
            def __init__(self, tag: str) -> None:
                self.tag = tag

            async def __aenter__(self) -> "Quiet":
                print("enter", self.tag)
                return self

            async def __aexit__(self, exc_type, exc, tb) -> bool:
                print("exit", self.tag, exc is not None)
                return exc is not None


        class Plain:
            async def __aenter__(self) -> int:
                return 7

            async def __aexit__(self, exc_type, exc, tb) -> None:
                print("plain exit", exc is None)


        class Pairs:
            def __init__(self) -> None:
                self.k = 0

            def __aiter__(self) -> "Pairs":
                return self

            async def __anext__(self) -> tuple[str, int]:
                self.k += 1
                if self.k > 4:
                    raise StopAsyncIteration
                await asyncio.sleep(0.001 * self.k)
                return ("p" + str(self.k), self.k)


        async def early(n: int) -> int:
            async with Quiet("r") as q:
                if n > 0:
                    return n * 2
                print("not returned")
            return -1


        async def slow() -> None:
            async with Quiet("slow"):
                async with Plain() as v:
                    print("v", v)
                    await asyncio.sleep(1)
                    print("never")


        async def main() -> None:
            async with Quiet("a"):
                raise KeyError("hidden")
            print("after suppressed")
            print("early", await early(5))
            for i in range(3):
                async with Quiet("loop" + str(i)):
                    if i == 1:
                        continue
                    if i == 2:
                        break
                    print("body", i)
            async for name, k in Pairs():
                if k == 3:
                    print("break at", name)
                    break
                print(name, k)
            else:
                print("no break")
            async with Quiet("m1") as a, Plain() as b:
                print("both", a.tag, b)
            t = asyncio.create_task(slow())
            await asyncio.sleep(0.01)
            t.cancel()
            try:
                await t
            except asyncio.CancelledError:
                print("cancelled", t.cancelled())


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "enter a\nexit a True\nafter suppressed\nenter r\nexit r False\nearly 10\nenter loop0\nbody 0\nexit loop0 False\nenter loop1\nexit loop1 False\nenter loop2\nexit loop2 False\np1 1\np2 2\nbreak at p3\nenter m1\nboth m1 7\nplain exit True\nexit m1 False\nenter slow\nv 7\nplain exit False\nexit slow True\n",
    );
}

/// `asyncio.Lock`, `Event`, `Semaphore`, `BoundedSemaphore`, `Queue`,
/// `LifoQueue`, and `PriorityQueue` (heapq's order for equal keys), and
/// `asyncio.timeout()` expiring, finishing in time, and with no deadline, as
/// CPython 3.13 schedules them.
#[test]
fn asyncio_locks_events_semaphores_queues_and_timeouts() {
    prints(
        r#"
        import asyncio

        lock = asyncio.Lock()


        async def guarded(name: str, delay: float) -> None:
            async with lock:
                print(name, "has the lock", lock.locked())
                await asyncio.sleep(delay)
                print(name, "lets go")


        async def waiter(ev: asyncio.Event, name: str) -> None:
            print(name, "waits")
            await ev.wait()
            print(name, "woke", ev.is_set())


        async def limited(sem: asyncio.Semaphore, n: int) -> int:
            async with sem:
                print("in", n)
                await asyncio.sleep(0.01)
                print("out", n)
            return n * n


        async def producer(q: asyncio.Queue[int], n: int) -> None:
            for i in range(n):
                await q.put(i)
                print("put", i, q.qsize())
            await q.put(-1)


        async def consumer(q: asyncio.Queue[int]) -> int:
            total = 0
            while True:
                item = await q.get()
                q.task_done()
                if item < 0:
                    break
                print("got", item)
                total += item
            return total


        async def main() -> None:
            await asyncio.gather(guarded("a", 0.02), guarded("b", 0.01), guarded("c", 0.0))
            ev = asyncio.Event()
            t1 = asyncio.create_task(waiter(ev, "w1"))
            t2 = asyncio.create_task(waiter(ev, "w2"))
            await asyncio.sleep(0)
            print("setting")
            ev.set()
            await t1
            await t2
            sem = asyncio.Semaphore(2)
            print(await asyncio.gather(*[limited(sem, i) for i in range(4)]))
            q: asyncio.Queue[int] = asyncio.Queue(maxsize=2)
            p = asyncio.create_task(producer(q, 5))
            total = await consumer(q)
            await p
            await q.join()
            print("total", total, q.empty(), q.maxsize)
            try:
                q.get_nowait()
            except asyncio.QueueEmpty:
                print("empty")
            try:
                async with asyncio.timeout(0.05):
                    await asyncio.sleep(1)
            except TimeoutError:
                print("timed out")
            async with asyncio.timeout(1) as cm:
                await asyncio.sleep(0.01)
            print("in time", cm.expired())
            async with asyncio.timeout(None):
                await asyncio.sleep(0)
            print("no deadline")
            try:
                lock.release()
            except RuntimeError as e:
                print("error:", e)
            b = asyncio.BoundedSemaphore(1)
            await b.acquire()
            b.release()
            try:
                b.release()
            except ValueError as e:
                print("error:", e)
            lq: asyncio.LifoQueue[str] = asyncio.LifoQueue()
            pq: asyncio.PriorityQueue[tuple[int, str]] = asyncio.PriorityQueue()
            for w in ["x", "y", "z"]:
                lq.put_nowait(w)
            for item in [(3, "c"), (1, "a"), (2, "b"), (1, "aa")]:
                pq.put_nowait(item)
            print([lq.get_nowait() for _ in range(3)])
            print([pq.get_nowait() for _ in range(4)])


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "a has the lock True\na lets go\nb has the lock True\nb lets go\nc has the lock True\nc lets go\nw1 waits\nw2 waits\nsetting\nw1 woke True\nw2 woke True\nin 0\nin 1\nout 0\nout 1\nin 2\nin 3\nout 2\nout 3\n[0, 1, 4, 9]\nput 0 1\nput 1 2\ngot 0\ngot 1\nput 2 1\nput 3 2\ngot 2\ngot 3\nput 4 1\ngot 4\ntotal 10 True 2\nempty\ntimed out\nin time False\nno deadline\nerror: Lock is not acquired.\nerror: BoundedSemaphore released too many times\n['z', 'y', 'x']\n[(1, 'a'), (1, 'aa'), (2, 'b'), (3, 'c')]\n",
    );
}

/// A lock in a field, a lock waiter cancelled while it waits (the next one
/// wakes), `wait_for()` over `Queue.get()` leaving no getter behind, a put
/// blocked on a full queue, `Queue.shutdown()`, and a semaphore made at zero.
#[test]
fn asyncio_primitives_under_cancellation() {
    prints(
        r#"
        import asyncio


        class Account:
            def __init__(self) -> None:
                self.balance = 0
                self.lock = asyncio.Lock()

            async def deposit(self, n: int) -> None:
                async with self.lock:
                    current = self.balance
                    await asyncio.sleep(0)
                    self.balance = current + n


        async def holder(lock: asyncio.Lock, name: str) -> None:
            await lock.acquire()
            try:
                print(name, "holds")
                await asyncio.sleep(0.05)
            finally:
                lock.release()
                print(name, "released")


        async def want(lock: asyncio.Lock, name: str) -> str:
            async with lock:
                print(name, "got it")
            return name


        async def take(q: asyncio.Queue[str]) -> str:
            return await q.get()


        async def main() -> None:
            acct = Account()
            await asyncio.gather(*[acct.deposit(i) for i in range(1, 6)])
            print("balance", acct.balance)

            lock = asyncio.Lock()
            h = asyncio.create_task(holder(lock, "h"))
            await asyncio.sleep(0)
            w1 = asyncio.create_task(want(lock, "w1"))
            w2 = asyncio.create_task(want(lock, "w2"))
            await asyncio.sleep(0.01)
            w1.cancel()
            print(await w2, w1.cancelled())
            await h

            q: asyncio.Queue[str] = asyncio.Queue()
            try:
                await asyncio.wait_for(q.get(), 0.02)
            except TimeoutError:
                print("get timed out", len(q._getters))
            t = asyncio.create_task(take(q))
            await asyncio.sleep(0)
            q.put_nowait("hello")
            print(await t)

            full: asyncio.Queue[int] = asyncio.Queue(1)
            full.put_nowait(1)
            try:
                full.put_nowait(2)
            except asyncio.QueueFull:
                print("full", full.full())
            putter = asyncio.create_task(full.put(3))
            await asyncio.sleep(0)
            print("putter waiting", putter.done())
            print(full.get_nowait())
            await asyncio.sleep(0)
            print("putter done", putter.done(), full.qsize())

            ev = asyncio.Event()
            ev.set()
            print(await ev.wait())
            ev.clear()
            print(ev.is_set())

            sd: asyncio.Queue[int] = asyncio.Queue()
            sd.put_nowait(5)
            sd.shutdown()
            print(await sd.get())
            try:
                await sd.get()
            except asyncio.QueueShutDown:
                print("shut down")
            try:
                sd.put_nowait(1)
            except asyncio.QueueShutDown:
                print("no put after shutdown")

            sem = asyncio.Semaphore(0)
            print(sem.locked())
            rel = asyncio.create_task(sem.acquire())
            await asyncio.sleep(0)
            sem.release()
            print(await rel, sem.locked())


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "balance 15\nh holds\nh released\nw2 got it\nw2 True\nget timed out 0\nhello\nfull True\nputter waiting False\n1\nputter done True 1\nTrue\nFalse\n5\nshut down\nno put after shutdown\nTrue\nTrue True\n",
    );
}

/// `asyncio.TaskGroup`: it waits for its tasks, a failing task cancels the
/// others and the group raises `ExceptionGroup` (its `str()`, `exceptions`,
/// and `message` as CPython words them), the body raising does the same, and
/// `create_task()` outside the group raises CPython's `RuntimeError`.
#[test]
fn asyncio_task_groups() {
    prints(
        r#"
        import asyncio


        async def work(n: int, delay: float) -> int:
            try:
                await asyncio.sleep(delay)
                print("done", n)
                return n * 10
            except asyncio.CancelledError:
                print("cancelled", n)
                raise


        async def fail(msg: str, delay: float) -> int:
            await asyncio.sleep(delay)
            raise ValueError(msg)


        async def main() -> None:
            async with asyncio.TaskGroup() as tg:
                t1 = tg.create_task(work(1, 0.02))
                t2 = tg.create_task(work(2, 0.01))
                print("spawned")
            print("results", t1.result(), t2.result())

            try:
                async with asyncio.TaskGroup() as tg:
                    tg.create_task(work(3, 0.05))
                    tg.create_task(fail("bad", 0.01))
                    tg.create_task(work(4, 0.005))
                    await asyncio.sleep(1)
                    print("never")
            except ExceptionGroup as eg:
                print("group:", eg)
                for e in eg.exceptions:
                    print(" ", type(e).__name__, e)

            try:
                async with asyncio.TaskGroup() as tg:
                    tg.create_task(fail("one", 0.01))
                    tg.create_task(fail("two", 0.01))
            except ExceptionGroup as eg:
                print("group:", eg, len(eg.exceptions), eg.message)

            try:
                async with asyncio.TaskGroup() as tg:
                    tg.create_task(work(5, 0.05))
                    raise KeyError("body")
            except ExceptionGroup as eg:
                print("group:", eg, [str(e) for e in eg.exceptions])

            tg2 = asyncio.TaskGroup()
            try:
                tg2.create_task(work(6, 0.0))
            except RuntimeError as e:
                print("error:", e)
            async with tg2:
                pass
            try:
                tg2.create_task(work(7, 0.0))
            except RuntimeError as e:
                print("error:", e)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "spawned\ndone 2\ndone 1\nresults 10 20\ndone 4\ncancelled 3\ngroup: unhandled errors in a TaskGroup (1 sub-exception)\n  ValueError bad\ngroup: unhandled errors in a TaskGroup (2 sub-exceptions) 2 unhandled errors in a TaskGroup\ngroup: unhandled errors in a TaskGroup (1 sub-exception) [\"'body'\"]\nerror: TaskGroup <TaskGroup> has not been entered\nerror: TaskGroup <TaskGroup entered> is finished\n",
    );
}

/// A task group whose parent is cancelled from outside cancels its tasks
/// (nested groups included), `asyncio.timeout()` around a group, and tasks
/// made in a comprehension inside one.
#[test]
fn asyncio_task_groups_cancelled_nested_and_timed_out() {
    prints(
        r#"
        import asyncio


        async def tick(name: str, n: int) -> str:
            try:
                for i in range(n):
                    await asyncio.sleep(0.01)
                    print(name, i)
                return name
            finally:
                print(name, "cleanup")


        async def outer() -> None:
            async with asyncio.TaskGroup() as tg:
                tg.create_task(tick("x", 10))
                async with asyncio.TaskGroup() as inner:
                    inner.create_task(tick("y", 2))
                print("inner done")


        async def main() -> None:
            t = asyncio.create_task(outer())
            await asyncio.sleep(0.045)
            t.cancel()
            try:
                await t
            except asyncio.CancelledError:
                print("outer cancelled", t.cancelled())
            try:
                async with asyncio.timeout(0.025):
                    async with asyncio.TaskGroup() as tg:
                        tg.create_task(tick("z", 5))
                        tg.create_task(tick("w", 1))
            except TimeoutError:
                print("timed out")
            results: list[str] = []
            async with asyncio.TaskGroup() as tg:
                tasks = [tg.create_task(tick(c, 1)) for c in "ab"]
            for task in tasks:
                results.append(task.result())
            print(results)


        if __name__ == "__main__":
            asyncio.run(main())
        "#,
        "x 0\ny 0\nx 1\ny 1\ny cleanup\ninner done\nx 2\nx 3\nx cleanup\nouter cancelled True\nz 0\nw 0\nw cleanup\nz 1\nz cleanup\ntimed out\na 0\na cleanup\nb 0\nb cleanup\n['a', 'b']\n",
    );
}

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
    refused(
        r#"
        class A:
            async def __aenter__(self) -> int:
                return 1

        async def main() -> None:
            async with A() as a:
                print(a)
        "#,
        "asynchronous context manager protocol (it has no '__aexit__' method)",
    );
    refused(
        r#"
        class A:
            async def __aenter__(self) -> int:
                return 1

            async def __aexit__(self, t, e, tb) -> None:
                pass

        def f() -> None:
            async with A():
                pass
        "#,
        "'async with' outside an async function",
    );
    refused(
        r#"
        def f(xs: list[int]) -> None:
            async for x in xs:
                print(x)
        "#,
        "'async for' outside an async function",
    );
    refused(
        r#"
        import asyncio

        async def main() -> None:
            q = asyncio.Queue()
            await q.put(1)
        "#,
        "needs the type of what it holds",
    );
}
