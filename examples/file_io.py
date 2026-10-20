# File I/O through WASI (#25).
#
# open()/read()/write()/close() go through WASI preview 1: path_open on the
# host's preopened directories, then fd_read/fd_write/fd_close. Run it with
# `wasmtime run --dir . file_io.wasm`, or under any WASI host; the playground's
# shim keeps files in memory. A file open() cannot open raises CPython's
# FileNotFoundError. Modules that never call open() import nothing.


def write_greeting() -> int:
    f = open("greeting.txt", "w")
    n = f.write("hello from waspy")
    f.close()
    return n


def read_greeting() -> int:
    f = open("greeting.txt", "r")
    text = f.read()
    f.close()
    return len(text)


def copy_with_context_managers() -> int:
    # `with open(...) as f:` closes the file at the end of the block.
    with open("copy.txt", "w") as out:
        with open("greeting.txt") as src:
            out.write(src.read())
    with open("copy.txt") as check:
        return len(check.read())


def append_line() -> int:
    with open("greeting.txt", "a") as f:
        f.write("\nsecond line")
    with open("greeting.txt") as f:
        return len(f.read())
