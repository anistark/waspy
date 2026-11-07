---
title: Running compiled modules
eyebrow: Running
description: "Calling exports, the WASI host interface, file I/O, and what a host has to provide."
---

## Calling exports

Every module-level function is exported under its own name, and can be called directly from any WebAssembly host:

```js
// Browser or Node.js
const { instance } = await WebAssembly.instantiate(wasmBuffer);
console.log(instance.exports.factorial(5)); // 120
```

## The host interface

A compiled program's side effects go through [WASI preview 1](https://github.com/WebAssembly/WASI/blob/main/legacy/preview1/docs.md), importing from `wasi_snapshot_preview1` exactly the functions it uses, so a program that prints nothing, reads nothing, and opens nothing imports nothing and instantiates with an empty import object, as above. The same module runs unchanged under `wasmtime run`, Node's `node:wasi`, or the small browser shim the playground ships ([`wasi.js`](https://github.com/anistark/waspy/blob/main/docs/src/playground/wasi.js)).

| What the program does | Imports |
|------|---------|
| `print()`, `sys.stdout.write()`, `sys.stderr.write()` | `fd_write` |
| `input()`, `sys.stdin.readline()` / `read()` / `readlines()`, `for line in sys.stdin` | `fd_read` (and `fd_write` for a prompt) |
| `sys.argv` | `args_sizes_get`, `args_get` |
| a `__main__` block that can raise an exception it does not catch | `fd_write`, `proc_exit` |
| `open()` and the file methods | `path_open`, `fd_prestat_get`, `fd_prestat_dir_name`, `fd_read`, `fd_write`, `fd_close` |
| `os.path.exists()`, `isfile()`, `isdir()`, `getsize()` | `path_filestat_get`, `fd_prestat_get`, `fd_prestat_dir_name` |
| `os.remove()`, `os.unlink()` | `path_unlink_file`, `fd_prestat_get`, `fd_prestat_dir_name` |
| `os.rename()`, `os.replace()` | `path_rename`, `fd_prestat_get`, `fd_prestat_dir_name` |
| `random`, `os.urandom()` | `random_get` |
| `os.environ`, `os.getenv()` | `environ_sizes_get`, `environ_get` |
| reading the clock (`datetime`'s `now()` and `today()`, a log record's time) | `clock_time_get` |

## Running a program

A program with an `if __name__ == "__main__":` block is a WASI command: it exports `_start`, which runs the block, so `wasmtime run program.wasm args...` runs it like `python3 program.py args...`. The block runs as module code once every module definition is made: a module variable it assigns is rebound for every function, and it may not reach a definition that comes after it in the file, which CPython would not have made yet. An exception the block does not catch makes it exit with status 1. Only the program's own block runs, never an imported module's. Every other function is exported too and can be called directly; an exception escaping one traps.

Writes are unbuffered, one `fd_write` per `print` or `write`, so standard output and standard error come out in the order the program wrote them. Standard input is read in UTF-8, as CPython's is on POSIX: lines end at `\n` alone (a `\r` is kept, as CPython keeps it there), and bytes that are not UTF-8 raise `UnicodeDecodeError`. `input()` at the end of input raises `EOFError`. `sys.argv[0]` is whatever the host passes first (`wasmtime` passes the module's path). `sys.stdout` and the other streams are used through their methods; one passed around as a value is refused.

## File I/O

A path is resolved against the host's preopened directories as wasi-libc resolves it: an absolute path against the longest preopen naming a prefix of it, a relative one against the first relative preopen (`.`), else `/`. So `wasmtime run --dir . program.wasm` reads and writes the current directory, and a program run with no preopen can open nothing. A file `open()` cannot open raises CPython's exception, with CPython's message on a POSIX host: `FileNotFoundError: [Errno 2] No such file or directory: 'notes.txt'`, and `PermissionError`, `IsADirectoryError`, `NotADirectoryError`, and `FileExistsError` for the matching WASI errors. A size-less `f.read()` asks for at most one 64 KiB page per call.

**Confinement is the preopens'.** A program reaches only the directories its host preopened, which is WASI's sandbox, so a host running untrusted Python chooses them, and its own limits on descriptor count and transfer size. The playground's shim keeps files in a `Map` and touches no filesystem.

## Preserved comments

Comments from the Python sources are kept in the generated binary as a `python.comments` custom section: UTF-8 text, one `file:line:text` entry per line. Custom sections carry no code, so this changes nothing about how the module runs, and the section survives optimization. Read it back with `waspy::core::comments::comments_from_wasm`, or with any tool that dumps WebAssembly custom sections.
