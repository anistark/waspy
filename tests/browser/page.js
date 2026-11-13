// The page the browser gate drives (tests/browser/verify.mjs). It runs a
// compiled waspy module through the playground's WASI shim, the code path a
// program takes in a browser, and runs the Python it was compiled from under
// Pyodide in the same page, so each answer has its reference beside it.
//
// Everything is fetched from the repository over loopback; nothing here
// reaches the network (the runner aborts any request that tries).

import { createWasi, WasiExit } from "/docs/src/playground/wasi.js";
import { loadPyodide } from "/tests/browser/node_modules/pyodide/pyodide.mjs";

async function fetchOk(url) {
    const response = await fetch(url);
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    return response;
}

const text = async (path) => (await fetchOk(`/${path}`)).text();
const bytes = async (path) => (await fetchOk(`/${path}`)).arrayBuffer();

// Python run under Pyodide. A program's checks import its main.py as a module
// (so a __main__ block does not run, as the compiled module's exports do not
// run it either) and call each check. A command runs as __main__ with the
// given standard input and arguments; an exception it does not catch writes
// the last line of CPython's report, as a compiled program does (a traceback
// is not kept; see the Limitations page).
const HELPERS = `
import importlib, io, json, sys, traceback

def _gate_checks(where, files, names):
    stems = [f[:-3] for f in json.loads(files)]
    def forget():
        for stem in stems:
            sys.modules.pop(stem, None)
    forget()
    importlib.invalidate_caches()
    sys.path.insert(0, where)
    saved = sys.stdout
    sys.stdout = io.StringIO()
    try:
        main = importlib.import_module("main")
        return json.dumps({n: int(getattr(main, n)()) for n in json.loads(names)})
    finally:
        sys.stdout = saved
        sys.path.remove(where)
        forget()

def _gate_command(code, filename, stdin, args):
    out, err = io.StringIO(), io.StringIO()
    saved = sys.stdin, sys.stdout, sys.stderr, sys.argv
    sys.stdin = io.TextIOWrapper(io.BytesIO(stdin.encode("utf-8")), encoding="utf-8")
    sys.stdout, sys.stderr = out, err
    sys.argv = ["main.py", *json.loads(args)]
    status = 0
    try:
        exec(compile(code, filename, "exec"), {"__name__": "__main__"})
    except SystemExit as e:
        if isinstance(e.code, int):
            status = e.code
        elif e.code is not None:
            err.write(f"{e.code}\\n")
            status = 1
    except BaseException as e:
        err.write("".join(traceback.format_exception_only(e)))
        status = 1
    finally:
        sys.stdin, sys.stdout, sys.stderr, sys.argv = saved
    return json.dumps({"stdout": out.getvalue(), "stderr": err.getvalue(), "exit": status})
`;

let pyodide = null;

// Call each named export of a compiled module, answering what each returned
// (or why it could not).
async function waspyChecks(wasm, names) {
    const wasi = createWasi({});
    const { instance } = await WebAssembly.instantiate(await bytes(wasm), wasi.imports);
    wasi.bind(instance);
    const answers = {};
    for (const name of names) {
        const fn = instance.exports[name];
        if (typeof fn !== "function") {
            answers[name] = "not exported";
            continue;
        }
        try {
            answers[name] = fn();
        } catch (error) {
            answers[name] = `trapped: ${error.message}`;
        }
    }
    return answers;
}

// Run a compiled module's `_start` as a WASI command.
async function waspyCommand(wasm, stdin, args) {
    let stdout = "";
    let stderr = "";
    const wasi = createWasi({
        args: ["main.py", ...args],
        stdin,
        onStdout: (t) => (stdout += t),
        onStderr: (t) => (stderr += t),
    });
    const { instance } = await WebAssembly.instantiate(await bytes(wasm), wasi.imports);
    wasi.bind(instance);
    let exit = 0;
    try {
        instance.exports._start();
    } catch (error) {
        if (error instanceof WasiExit) exit = error.code;
        else return { error: error.message };
    }
    return { stdout, stderr, exit };
}

// The same program's checks under Pyodide: its files are copied into
// Pyodide's file system, each program into a directory of its own.
async function pyodideChecks(dir, files, names) {
    const where = `/gate/${dir.replaceAll("/", "_")}`;
    pyodide.FS.mkdirTree(where);
    for (const file of files) {
        pyodide.FS.writeFile(`${where}/${file}`, await text(`${dir}/${file}`));
    }
    const run = pyodide.globals.get("_gate_checks");
    try {
        return JSON.parse(run(where, JSON.stringify(files), JSON.stringify(names)));
    } finally {
        run.destroy();
    }
}

async function pyodideCommand(source, stdin, args) {
    const run = pyodide.globals.get("_gate_command");
    try {
        return JSON.parse(run(await text(source), source, stdin, JSON.stringify(args)));
    } finally {
        run.destroy();
    }
}

window.gate = { waspyChecks, waspyCommand, pyodideChecks, pyodideCommand };

// Resolves once Pyodide is loaded, with the versions in use.
window.gateReady = (async () => {
    pyodide = await loadPyodide({ indexURL: "/tests/browser/node_modules/pyodide/" });
    pyodide.runPython(HELPERS);
    return {
        pyodide: pyodide.version,
        python: pyodide.runPython("import sys; sys.version.split()[0]"),
    };
})();
