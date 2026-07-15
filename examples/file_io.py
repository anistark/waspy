# File I/O through the waspy_host interface (#25).
#
# open()/read()/write()/close() compile to calls into four imported host
# functions (module "waspy_host": open, read, write, close). The embedder
# provides them — WASM has no filesystem of its own — so the same binary can
# be backed by real files (a runtime shim), an in-memory store (browser), or
# anything else. Modules that never call open() import nothing.


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
