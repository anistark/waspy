// The JavaScript side of a waspy module (`waspy::js_loader()`).
//
// A Python program that imports `js` or `pyodide` reaches JavaScript through
// the `waspy_js` imports this file provides: it keeps the JavaScript values
// Python holds in a table, by handle, answers each operation Python makes on
// one (an attribute, a call, `new`, an item, iteration, a conversion), and
// takes a handle back when Python's collector lets go of the value. It also
// wraps the module's exports by the signatures the module lists (its
// `waspy.exports` custom section), so they take and answer JavaScript
// values: numbers, strings, booleans, undefined for None, arrays and plain
// objects for lists, tuples, and dicts, and any value for a `JsProxy`. A
// Python exception escaping one is thrown as a `PythonError` whose message
// is the last line of CPython's report. An exported `async def` answers a
// Promise: its coroutine runs as a task on the module's event loop, which
// this file runs between JavaScript's turns (a timer for `asyncio.sleep`,
// a promise's settling for `await promise`), and the Promise resolves with
// what the coroutine returns, or rejects with the `PythonError` it raised.
//
//   import { instantiate } from './waspy-loader.js';
//   const { exports } = await instantiate(bytes, { wasi: myWasi.imports.wasi_snapshot_preview1 });
//   exports.greet('Ada');
//
// `wasi` is the program's WASI preview 1 host (it needs one only if it
// prints, reads, or opens files), `globals` what `js` names (`globalThis` by
// default), and `modules` the separately compiled modules a program may load
// by a name computed at run time (`importlib.import_module(name)`), each the
// `exports` of another `instantiate`.
//
// A program with a `__main__` block runs through `start()`, which answers a
// Promise. Where the browser has JavaScript Promise Integration
// (`WebAssembly.Suspending` and `WebAssembly.promising`), a promise awaited
// inside `asyncio.run()` suspends the program until it settles, as Pyodide's
// does; without it, such an `await` raises `RuntimeError`.
//
//   const { start } = await instantiate(bytes, { wasi });
//   await start();

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8');

export class PythonError extends Error {
    constructor(message) {
        super(message);
        this.name = 'PythonError';
    }
}

/// A value JSON would not carry over as it is.
class Unsupported extends Error {}

export class Bridge {
    constructor({ globals = globalThis, modules = {} } = {}) {
        this.globals = globals;
        this.modules = modules;
        this.values = [];
        this.free = [];
        this.lastError = undefined;
        this.bytes = new Uint8Array(0);
        this.failure = null;
        this.instance = null;
        // The Promises exported coroutines answered, by token, and what
        // settled before the Promise was made.
        this.promises = new Map();
        this.settled = new Map();
        this.timer = null;
        // While `start()` runs the program under JSPI: the promises that
        // settled for it to collect, what wakes it, and how many calls from
        // JavaScript into the module are under way (none may suspend).
        this.inMain = false;
        this.queue = [];
        this.wake = null;
        this.taken = null;
        this.depth = 0;
    }

    /// A promise Python awaits settled: queued for the program `start()`
    /// runs (waking it if it waits), or handed to the event loop the loader
    /// runs between JavaScript's turns.
    deliver(token, failed, value) {
        if (this.inMain) {
            this.queue.push({ token, failed, value });
            if (this.wake) this.wake();
        } else {
            this.settle(token, failed, value);
        }
    }

    /// Run the module's event loop again in `delay` milliseconds (none when
    /// it is negative: nothing is waiting on a timer).
    schedule(delay) {
        if (this.timer !== null) clearTimeout(this.timer);
        this.timer = null;
        if (delay >= 0) {
            this.timer = setTimeout(() => {
                this.timer = null;
                this.schedule(this.instance.exports.__waspy_drive());
            }, delay);
        }
    }

    /// A promise Python awaits settled.
    settle(token, failed, value) {
        this.schedule(this.instance.exports.__waspy_settle(token, failed, this.add(value)));
    }

    /// An exported coroutine's call, as a Promise.
    promise(token) {
        return new Promise((resolve, reject) => {
            const early = this.settled.get(token);
            if (early) {
                this.settled.delete(token);
                (early.failed ? reject : resolve)(early.value);
            } else {
                this.promises.set(token, { resolve, reject });
            }
        });
    }

    add(value) {
        const h = this.free.length ? this.free.pop() : this.values.length;
        this.values[h] = value;
        return h;
    }

    get(h) {
        return this.values[h];
    }

    drop(h) {
        this.values[h] = undefined;
        this.free.push(h);
    }

    /// How many handles are held: what Python can still reach.
    live() {
        return this.values.length - this.free.length;
    }

    memory() {
        return new Uint8Array(this.instance.exports.memory.buffer);
    }

    text(ptr, len) {
        return decoder.decode(this.memory().subarray(ptr, ptr + len));
    }

    /// Run an operation, answering -2 (with the error kept) if it throws.
    guard(operation) {
        try {
            return operation();
        } catch (error) {
            this.lastError = error;
            return -2;
        }
    }

    imports() {
        const has = (obj, name) => obj !== null && obj !== undefined && name in Object(obj);
        return {
            global: (p, l) => {
                const name = this.text(p, l);
                return has(this.globals, name) ? this.add(this.globals[name]) : -1;
            },
            global_this: () => this.add(this.globals),
            get: (h, p, l) =>
                this.guard(() => {
                    const obj = this.get(h);
                    const name = this.text(p, l);
                    if (obj === null || obj === undefined) {
                        throw new TypeError(`Cannot read properties of ${obj} (reading '${name}')`);
                    }
                    return has(obj, name) ? this.add(obj[name]) : -1;
                }),
            set: (h, p, l, v) =>
                this.guard(() => {
                    this.get(h)[this.text(p, l)] = this.get(v);
                    return 0;
                }),
            call: (f, args) =>
                this.guard(() => {
                    const fn = this.get(f);
                    if (typeof fn !== 'function') return -3;
                    return this.add(Reflect.apply(fn, undefined, this.get(args)));
                }),
            call_method: (h, p, l, args) =>
                this.guard(() => {
                    const obj = this.get(h);
                    const name = this.text(p, l);
                    if (!has(obj, name)) return -1;
                    const fn = obj[name];
                    if (typeof fn !== 'function') return -3;
                    return this.add(Reflect.apply(fn, obj, this.get(args)));
                }),
            construct: (f, args) =>
                this.guard(() => {
                    const fn = this.get(f);
                    if (typeof fn !== 'function') return -3;
                    return this.add(Reflect.construct(fn, this.get(args)));
                }),
            array: () => this.add([]),
            push: (a, v) => {
                this.get(a).push(this.get(v));
            },
            number: (x) => this.add(x),
            boolean: (b) => this.add(b !== 0),
            string: (p, l) => this.add(this.text(p, l)),
            undefined: () => this.add(undefined),
            kind: (h) => {
                const v = this.get(h);
                if (v === undefined) return 0;
                if (v === null) return 1;
                if (v === false) return 2;
                if (v === true) return 3;
                if (typeof v === 'number') return Number.isSafeInteger(v) ? 4 : 5;
                if (typeof v === 'string') return 6;
                if (typeof v === 'function') return 7;
                if (Array.isArray(v)) return 8;
                if (typeof v === 'object') return 9;
                return 10;
            },
            to_number: (h) => Number(this.get(h)),
            text: (h) => this.add(String(this.get(h))),
            utf8: (h) => {
                this.bytes = encoder.encode(this.get(h));
                return this.bytes.length;
            },
            utf8_copy: (at) => {
                this.memory().set(this.bytes, at);
            },
            equal: (a, b) => (this.get(a) === this.get(b) ? 1 : 0),
            length: (h) => {
                const v = this.get(h);
                if (typeof v === 'string' || Array.isArray(v)) return v.length;
                if (v !== null && typeof v === 'object') {
                    if (typeof v.length === 'number') return v.length;
                    if (typeof v.size === 'number') return v.size;
                }
                return -1;
            },
            item: (h, i) => {
                const v = this.get(h);
                const arrayLike =
                    Array.isArray(v) ||
                    ArrayBuffer.isView(v) ||
                    (v !== null && typeof v === 'object' && typeof v.length === 'number');
                if (!arrayLike) return -3;
                const at = i < 0 ? i + v.length : i;
                return at < 0 || at >= v.length ? -1 : this.add(v[at]);
            },
            iterate: (h) =>
                this.guard(() => {
                    const v = this.get(h);
                    if (v === null || v === undefined || typeof v[Symbol.iterator] !== 'function') {
                        return -3;
                    }
                    return this.add(v[Symbol.iterator]());
                }),
            next: (it) =>
                this.guard(() => {
                    const step = this.get(it).next();
                    return step.done ? -1 : this.add(step.value);
                }),
            error: () => this.add(this.lastError),
            release: (h) => this.drop(h),
            keep: (h) => this.add(this.get(h)),
            stringify: (h) => {
                try {
                    const text = JSON.stringify(this.get(h), (key, value) => {
                        // A Map with string keys is the dict Pyodide makes of it.
                        if (value instanceof Map) {
                            if ([...value.keys()].every((k) => typeof k === 'string')) {
                                return Object.fromEntries(value);
                            }
                            throw new Unsupported();
                        }
                        if (
                            value instanceof Set ||
                            typeof value === 'function' ||
                            typeof value === 'symbol' ||
                            typeof value === 'bigint'
                        ) {
                            throw new Unsupported();
                        }
                        return value;
                    });
                    return this.add(text === undefined ? 'null' : text);
                } catch (error) {
                    // A cycle is a TypeError, and what JSON would not carry
                    // over as it is, unsupported: neither converts.
                    if (error instanceof Unsupported || error instanceof TypeError) return -3;
                    this.lastError = error;
                    return -2;
                }
            },
            parse: (p, l) => this.guard(() => this.add(JSON.parse(this.text(p, l)))),
            import_module: (p, l) => {
                const name = this.text(p, l);
                return Object.hasOwn(this.modules, name) ? this.add(this.modules[name]) : -1;
            },
            fail: (p, l) => {
                this.failure = this.text(p, l);
            },
            then: (p, token) => {
                Promise.resolve(this.get(p)).then(
                    (value) => this.deliver(token, 0, value),
                    (error) => this.deliver(token, 1, error),
                );
            },
            // JSPI: whether the program may suspend here (inside `start()`,
            // not inside a call JavaScript made into it), and the suspension
            // itself: until a promise it awaits settles, or `ms` milliseconds
            // (none when negative) for its next timer.
            can_suspend: () => (this.inMain && this.depth === 0 && SUSPENDING ? 1 : 0),
            suspend: (ms) => {
                if (this.queue.length) return 0;
                return new Promise((resolve) => {
                    let timer = null;
                    this.wake = () => {
                        this.wake = null;
                        if (timer !== null) clearTimeout(timer);
                        resolve(0);
                    };
                    if (ms >= 0) timer = setTimeout(this.wake, ms);
                });
            },
            // The next settled promise's token (-1 when there is none), then
            // whether it failed and its value.
            next_settled: () => {
                this.taken = this.queue.shift() ?? null;
                return this.taken ? this.taken.token : -1;
            },
            settled_failed: () => (this.taken.failed ? 1 : 0),
            settled_value: () => this.add(this.taken.value),
            resolve: (token, failed, h) => {
                let value = this.get(h);
                this.drop(h);
                if (failed) value = new PythonError(value);
                const waiting = this.promises.get(token);
                if (waiting) {
                    this.promises.delete(token);
                    (failed ? waiting.reject : waiting.resolve)(value);
                } else {
                    this.settled.set(token, { failed, value });
                }
            },
        };
    }

    /// A Python function an export is, as a JavaScript one.
    wrap(signature) {
        const exports = this.instance.exports;
        const wrapper = exports[`__jsx_${signature.name}`];
        if (!wrapper) return exports[signature.name];
        return (...args) => {
            const handles = signature.params.map((_, k) => this.add(args[k]));
            this.depth += 1;
            let r;
            try {
                r = wrapper(...handles);
            } finally {
                this.depth -= 1;
            }
            if (r === -2) {
                const message = this.failure;
                this.failure = null;
                throw new PythonError(message);
            }
            if (signature.async) {
                const answer = this.promise(r);
                this.schedule(exports.__waspy_drive());
                return answer;
            }
            const value = this.get(r);
            this.drop(r);
            return value;
        };
    }
}

/// Whether the host has JavaScript Promise Integration.
const SUSPENDING =
    typeof WebAssembly.Suspending === 'function' && typeof WebAssembly.promising === 'function';

/// The module's list of exports and their signatures.
export function signatures(module) {
    const [section] = WebAssembly.Module.customSections(module, 'waspy.exports');
    return section ? JSON.parse(decoder.decode(section)) : [];
}

/// Instantiate a compiled waspy module (bytes or a `WebAssembly.Module`),
/// answering its instance, its exports wrapped by their signatures, and the
/// bridge.
export async function instantiate(source, { wasi = undefined, globals, modules } = {}) {
    const module = source instanceof WebAssembly.Module ? source : await WebAssembly.compile(source);
    const bridge = new Bridge({ globals, modules });
    const imports = {};
    const wanted = new Set(WebAssembly.Module.imports(module).map((i) => i.module));
    if (wanted.has('waspy_js')) {
        imports.waspy_js = bridge.imports();
        if (SUSPENDING) imports.waspy_js.suspend = new WebAssembly.Suspending(imports.waspy_js.suspend);
    }
    if (wanted.has('wasi_snapshot_preview1')) {
        if (!wasi) throw new Error('this module needs a WASI host: pass { wasi }');
        imports.wasi_snapshot_preview1 = wasi;
    }
    const instance = await WebAssembly.instantiate(module, imports);
    bridge.instance = instance;
    const exports = {};
    for (const signature of signatures(module)) {
        const wrapped = bridge.wrap(signature);
        if (wrapped) exports[signature.name] = wrapped;
    }
    // Run the program's `__main__` block (`_start`), suspending where it
    // awaits a promise when the host has JSPI.
    const start = async () => {
        const main = instance.exports._start;
        if (typeof main !== 'function') throw new Error('this module has no __main__ block (_start)');
        if (!SUSPENDING || !wanted.has('waspy_js')) {
            main();
            return;
        }
        bridge.inMain = true;
        try {
            await WebAssembly.promising(main)();
        } finally {
            bridge.inMain = false;
        }
    };
    return { instance, exports, bridge, start };
}
