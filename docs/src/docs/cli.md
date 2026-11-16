---
title: The command line
eyebrow: Start here
description: "Compile, check, and run Python with the waspy command: build, check, and run."
---

The `waspy` command compiles a Python file or project to WebAssembly, checks one without writing anything, or compiles and runs it.

## Installing

```sh
cargo install waspy
```

or, from Python's package index, which builds the same binary:

```sh
pip install waspy
```

`waspy run` runs the compiled module with a WebAssembly runtime, [wasmtime](https://wasmtime.dev) by default, which it expects on the `PATH`.

## Commands

```text
waspy build <path> [-o <out.wasm>] [--no-optimize] [--js] [-v]
waspy check <path>
waspy run <path> [--runtime <command>] [-- <args>...]
waspy --version
waspy help
```

`<path>` is a `.py` file, whose imports of the program's own modules are found beside it and compiled with it, or a project directory.

### build

Compiles to `<path>.wasm`, or to the file `-o` names, and runs Binaryen over the output unless `--no-optimize` is given. `--js` wraps every export for the [JavaScript loader](/docs/javascript/) and writes `waspy-loader.js` beside the module. `-v` prints what the compiler does.

```sh
waspy build app.py            # app.wasm
waspy build app.py -o out.wasm --no-optimize
waspy build myproject/        # myproject/myproject.wasm
```

### check

Compiles without writing anything and reports what the compiler refuses, one error per line in the form editors read, `file:line:column: error: message`, then exits with status 1. A file that compiles prints `file: ok` and exits with status 0, so `waspy check` fits a pre-commit hook or a CI step.

```text
$ waspy check app.py
app.py:5:1: error: the function 'g' is defined inside a module-level 'if', loop, 'try', or 'with', which is not supported: a module's functions and classes are made when it is compiled. Hint: define it at module level
```

An error the compiler finds while it lowers a statement carries that statement's line and column. One it finds while it generates code for a function carries the line of that function's `def`.

### run

Compiles (without optimizing, to start quickly) and runs the module, with the current directory preopened for file I/O and the arguments after the path (or after `--`) as `sys.argv[1:]`. The exit status is the program's.

```sh
waspy run app.py
waspy run greet.py bob --count 2
waspy run app.py -- --help
waspy run app.py --runtime wasmtime
```

`--runtime` names another runtime command, as does the `WASPY_RUNTIME` environment variable; one other than wasmtime is given the module's path and the arguments.

## What a program looks like

A script runs top to bottom as it does under `python3`: statements at the top level run where they stand, and an `if __name__ == "__main__":` block runs after them. A program that uses the JavaScript bridge (`import js`) needs the loader in a browser or Node rather than `waspy run`. See [Running compiled modules](/docs/running/) for the WASI functions a module imports.
