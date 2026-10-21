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

A compiled program's side effects go through [WASI preview 1](https://github.com/WebAssembly/WASI/blob/main/legacy/preview1/docs.md), importing from `wasi_snapshot_preview1` exactly the functions it uses, so a program that prints nothing, reads nothing, and opens nothing imports nothing and instantiates with an empty import object, as above. The same module runs unchanged under `wasmtime run`, Node's `node:wasi`, or the small browser shim the playground ships ([`wasi.js`](https://github.com/anistark/waspy/blob/main/docs/playground/wasi.js)).

| What the program does | Imports |
|------|---------|
| `print()`, `sys.stdout.write()`, `sys.stderr.write()` | `fd_write` |
| `input()`, `sys.stdin.readline()` / `read()` / `readlines()`, `for line in sys.stdin` | `fd_read` (and `fd_write` for a prompt) |
| `sys.argv` | `args_sizes_get`, `args_get` |
| a `__main__` block that can raise an exception it does not catch | `fd_write`, `proc_exit` |
| `open()` and the file methods | `path_open`, `fd_prestat_get`, `fd_prestat_dir_name`, `fd_read`, `fd_write`, `fd_close` |

## Running a program

A program with an `if __name__ == "__main__":` block is a WASI command: it exports `_start`, which runs the block, so `wasmtime run program.wasm args...` runs it like `python3 program.py args...`. An exception the block does not catch makes it exit with status 1. Every other function is exported too and can be called directly; an exception escaping one traps.

Writes are unbuffered, one `fd_write` per `print` or `write`, so standard output and standard error come out in the order the program wrote them. Standard input is read in UTF-8, as CPython's is on POSIX: lines end at `\n` alone (a `\r` is kept, as CPython keeps it there), and bytes that are not UTF-8 raise `UnicodeDecodeError`. `input()` at the end of input raises `EOFError`. `sys.argv[0]` is whatever the host passes first (`wasmtime` passes the module's path). `sys.stdout` and the other streams are used through their methods; one passed around as a value is refused.

## File I/O

A path is resolved against the host's preopened directories as wasi-libc resolves it: an absolute path against the longest preopen naming a prefix of it, a relative one against the first relative preopen (`.`), else `/`. So `wasmtime run --dir . program.wasm` reads and writes the current directory, and a program run with no preopen can open nothing. A file `open()` cannot open raises CPython's exception, with CPython's message on a POSIX host: `FileNotFoundError: [Errno 2] No such file or directory: 'notes.txt'`, and `PermissionError`, `IsADirectoryError`, `NotADirectoryError`, and `FileExistsError` for the matching WASI errors. A size-less `f.read()` asks for at most one 64 KiB page per call.

**Confinement is the preopens'.** A program reaches only the directories its host preopened, which is WASI's sandbox, so a host running untrusted Python chooses them, and its own limits on descriptor count and transfer size. The playground's shim keeps files in a `Map` and touches no filesystem.

## The deprecated `waspy_host` interface

With `CompilerOptions::waspy_host_file_io` set, file I/O compiles instead to four imports from the `waspy_host` namespace, for embedders that implement them.

| Import | Signature | Result |
|--------|-----------|--------|
| `waspy_host.open` | `(path_ptr: i32, path_len: i32, flags: i32) -> i32` | file descriptor, or `-1` |
| `waspy_host.read` | `(fd: i32, buf_ptr: i32, len: i32) -> i32` | bytes read, `0` at EOF, negative on error |
| `waspy_host.write` | `(fd: i32, buf_ptr: i32, len: i32) -> i32` | bytes written, negative on error |
| `waspy_host.close` | `(fd: i32) -> i32` | `0` on success |

`path_ptr`/`path_len` and `buf_ptr`/`len` address the module's own linear memory, which the host reads and writes through the exported `memory`. `flags` is the Python mode string folded at compile time: `1` read, `2` write, `4` append, `8` binary, `16` update, or-ed together, so `"w+b"` is `26`. Under this interface the embedder owns confinement (the program chooses the path it passes), and a failed open answers -1 rather than raising.

## Preserved comments

Comments from the Python sources are kept in the generated binary as a `python.comments` custom section: UTF-8 text, one `file:line:text` entry per line. Custom sections carry no code, so this changes nothing about how the module runs, and the section survives optimization. Read it back with `waspy::core::comments::comments_from_wasm`, or with any tool that dumps WebAssembly custom sections.
