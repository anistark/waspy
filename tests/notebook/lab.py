"""Run one Python program twice, under CPython 3.13 and compiled by Waspy, and
show the two outputs side by side.

The notebook next to this file (`stdlib-lessons.ipynb`, opened with
`just notebook`) imports it. Nothing here is part of Waspy:
it shells out to the repository's `advanced_compiler` example (the same driver
`just compile` uses) and to the `wasmtime` CLI.

In the notebook:

    %%compare Optional title        run the cell's program under both
    %%compare --debug ...           compile with the debug build of Waspy
    %%compare --optimize ...        let Binaryen optimize the module
    %%compare --lines 12 ...        show at most 12 lines per stream
    %%cpython Optional title        run the cell under CPython 3.13 only

The last comparison is kept in `lab.last` as `(cpython_run, waspy_run)`.
"""

from __future__ import annotations

import difflib
import html
import os
import re
import shutil
import subprocess
import sysconfig
import tempfile
import textwrap
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path

from IPython.display import HTML, display

# --------------------------------------------------------------------------
# Where things are
# --------------------------------------------------------------------------


def _find_repo() -> Path:
    here = Path.cwd().resolve()
    for folder in [here, *here.parents]:
        cargo = folder / "Cargo.toml"
        if cargo.exists() and 'name = "waspy"' in cargo.read_text():
            return folder
    raise RuntimeError("run the notebook from inside the waspy repository")


REPO = _find_repo()
PYTHON = shutil.which("python3.13") or "/usr/local/bin/python3.13"
WASMTIME = shutil.which("wasmtime") or str(Path.home() / ".wasmtime/bin/wasmtime")
DRIVERS = {
    "release": REPO / "target/release/examples/advanced_compiler",
    "debug": REPO / "target/debug/examples/advanced_compiler",
}


def _stdlib_dir() -> str:
    try:
        out = subprocess.run(
            [PYTHON, "-c", "import sysconfig; print(sysconfig.get_paths()['stdlib'])"],
            capture_output=True, text=True,
        )
        return out.stdout.strip() or sysconfig.get_paths()["stdlib"]
    except FileNotFoundError:
        return sysconfig.get_paths()["stdlib"]


STDLIB = _stdlib_dir()
# CPython 3.13 colours tracebacks when the terminal (or a notebook) asks for it.
PLAIN_ENV = {**os.environ, "PYTHON_COLORS": "0", "NO_COLOR": "1", "TERM": "dumb"}
PLAIN_ENV.pop("FORCE_COLOR", None)
ANSI = re.compile(r"\x1b\[[0-9;]*m")
last: tuple | None = None

# --------------------------------------------------------------------------
# Running things
# --------------------------------------------------------------------------


@dataclass
class Run:
    """What one side produced."""

    stdout: str = ""
    stderr: str = ""
    code: int | None = 0
    ms: float = 0.0  # compile time for Waspy, run time for CPython
    ms_run: float = 0.0
    refused: str | None = None  # Waspy's compile-time error, if it refused
    crashed: str | None = None  # the compiler itself died (not the program)
    wasm_bytes: int | None = None
    files: dict[str, str] = field(default_factory=dict)


def _workdir() -> Path:
    return Path(tempfile.mkdtemp(prefix="waspy-nb-")).resolve()


def _clean(text: str, folder: Path) -> str:
    # Tracebacks name the temporary folder and the install; shorten both.
    for form in sorted({str(folder), str(folder).removeprefix("/private")}, key=len, reverse=True):
        text = text.replace(form + os.sep, "").replace(form, ".")
    return ANSI.sub("", text).replace(STDLIB + os.sep, "<stdlib>/")


def _snapshot(folder: Path) -> dict[str, str]:
    out = {}
    for path in sorted(folder.iterdir()):
        if path.suffix in (".wasm", ".py") or not path.is_file():
            continue
        try:
            out[path.name] = path.read_text()
        except UnicodeDecodeError:
            out[path.name] = "<binary>"
    return out


def _prepare(source: str) -> str:
    return textwrap.dedent(source).strip() + "\n"


def cpython(source: str, stdin: str = "") -> Run:
    """Run `source` as a script under CPython 3.13."""
    folder = _workdir()
    (folder / "main.py").write_text(_prepare(source))
    start = time.perf_counter()
    done = subprocess.run(
        [PYTHON, "main.py"], cwd=folder, input=stdin, capture_output=True, text=True,
        env=PLAIN_ENV,
    )
    return Run(
        stdout=_clean(done.stdout, folder),
        stderr=_clean(done.stderr, folder),
        code=done.returncode,
        ms=(time.perf_counter() - start) * 1000,
        files=_snapshot(folder),
    )


def compile_waspy(source: str, folder: Path, optimize: bool, build: str) -> tuple[Path | None, Run]:
    """Compile `source` with Waspy. Returns the .wasm path, or None and why."""
    stem = "main_" + uuid.uuid4().hex[:8]
    src = folder / f"{stem}.py"
    src.write_text(_prepare(source))
    args = [str(DRIVERS[build]), str(src)]
    if not optimize:
        args.append("--no-optimize")
    start = time.perf_counter()
    done = subprocess.run(args, cwd=REPO, capture_output=True, text=True)
    ms = (time.perf_counter() - start) * 1000
    src.unlink()
    built = REPO / "examples/output" / f"{stem}.wasm"
    if done.returncode != 0 or not built.exists():
        err = done.stderr
        if "overflowed its stack" in err or done.returncode < 0:
            lines = [l for l in err.splitlines() if l.strip()]
            return None, Run(crashed="\n".join(lines[-2:]) or f"signal {-done.returncode}", ms=ms, code=None)
        if "Caused by:" in err:
            err = err.split("Caused by:", 1)[1]
        err = "\n".join(l for l in textwrap.dedent(err).strip().splitlines()
                        if not l.startswith("Warning: Unresolved import"))
        return None, Run(refused=_clean(err.removeprefix("Error: "), folder), ms=ms, code=None)
    wasm = folder / f"{stem}.wasm"
    shutil.move(built, wasm)
    return wasm, Run(ms=ms, wasm_bytes=wasm.stat().st_size)


def waspy(source: str, stdin: str = "", optimize: bool = False, build: str = "release") -> Run:
    """Compile `source` with Waspy and run the module under wasmtime."""
    folder = _workdir()
    wasm, built = compile_waspy(source, folder, optimize, build)
    if wasm is None:
        return built
    start = time.perf_counter()
    done = subprocess.run(
        [WASMTIME, "run", "--dir=.::.", wasm.name],
        cwd=folder, input=stdin, capture_output=True, text=True,
    )
    built.ms_run = (time.perf_counter() - start) * 1000
    stderr = done.stderr
    if "wasm backtrace" in stderr:
        # A trap: keep wasmtime's one-line reason, drop its frame dump.
        lines = [l.strip() for l in stderr.splitlines() if l.strip()]
        reason = next((l for l in lines if "wasm trap" in l), lines[-1])
        stderr = f"[the WebAssembly module stopped: {reason}]\n"
    built.stdout = _clean(done.stdout, folder)
    built.stderr = _clean(stderr, folder)
    built.code = done.returncode
    built.files = _snapshot(folder)
    return built


# --------------------------------------------------------------------------
# Showing things
# --------------------------------------------------------------------------

CPY = "#3b7dc4"  # CPython blue
WSP = "#d98a1c"  # Waspy honey
OK = "#2f9e6a"
BAD = "#d1495b"
VIOLET = "#8b5cf6"

STYLE = f"""
<style>
.lab {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; margin: 4px 0 14px; }}
.lab pre {{ margin: 0; padding: 9px 11px; font: 12.5px/1.45 ui-monospace, 'SF Mono', Menlo, Consolas, monospace;
  white-space: pre-wrap; word-break: break-word; background: rgba(127,127,127,.09); border-radius: 6px; }}
.lab .title {{ font-weight: 650; font-size: 14.5px; margin-bottom: 8px; }}
.lab .cols {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 12px; }}
.lab .col {{ border: 1px solid rgba(127,127,127,.28); border-radius: 9px; overflow: hidden; }}
.lab .head {{ padding: 6px 11px; font-size: 12px; font-weight: 650; color: #fff;
  display: flex; justify-content: space-between; gap: 8px; }}
.lab .meta {{ font-weight: 500; opacity: .92; }}
.lab .body {{ padding: 8px; display: grid; gap: 6px; }}
.lab .tag {{ font-size: 10.5px; text-transform: uppercase; letter-spacing: .07em; opacity: .62; margin: 2px 2px -2px; }}
.lab .diffline {{ background: rgba(209,73,91,.22); border-radius: 3px; }}
.lab .more {{ opacity: .55; font-style: italic; }}
.lab .verdict {{ margin-top: 10px; padding: 8px 12px; border-radius: 8px; font-size: 13px; font-weight: 600; }}
.lab .refused {{ border-left: 4px solid {BAD}; background: rgba(209,73,91,.09); }}
.lab .crashed {{ border-left: 4px solid {VIOLET}; background: rgba(139,92,246,.11); }}
/* In a slideshow (RISE or nbconvert's slides), keep a demo's program small
   enough that its comparison fits on the slide below it. */
.reveal .jp-InputArea-editor, .reveal .jp-InputArea-editor pre,
.reveal .jp-InputArea-editor .cm-content, .reveal .jp-InputArea-editor .cm-line {{
  font-size: 14px !important; line-height: 1.35 !important; }}
.reveal .jp-InputPrompt {{ font-size: 12px !important; }}
</style>
"""


def _esc(text: str) -> str:
    return html.escape(text)


def _stream(label: str, text: str, marks: set[int], max_lines: int) -> str:
    if not text:
        return ""
    lines = text.rstrip("\n").split("\n")
    shown = lines[:max_lines]
    body = "\n".join(
        f'<span class="diffline">{_esc(l) or " "}</span>' if i in marks else _esc(l)
        for i, l in enumerate(shown)
    )
    if len(lines) > max_lines:
        hidden = len(lines) - max_lines
        differing = len([i for i in marks if i >= max_lines])
        extra = f", {differing:,} of them different" if differing else ""
        body += f'\n<span class="more">… {hidden:,} more lines{extra}</span>'
    return f'<div class="tag">{label}</div><pre>{body}</pre>'


def _differing(a: list[str], b: list[str]) -> tuple[set[int], set[int]]:
    left, right = set(), set()
    for op, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes():
        if op != "equal":
            left.update(range(i1, i2))
            right.update(range(j1, j2))
    return left, right


def _panel(name: str, color: str, run: Run, out_marks, err_marks, max_lines: int) -> str:
    meta = []
    if run.code is not None:
        meta.append(f"exit {run.code}")
    if run.wasm_bytes is not None:
        meta.append(f"{run.wasm_bytes:,} B of wasm")
    head = (f'<div class="head" style="background:{color}"><span>{name}</span>'
            f'<span class="meta">{" · ".join(meta)}</span></div>')
    if run.crashed is not None:
        body = f'<div class="tag">the compiler itself crashed</div><pre class="crashed">{_esc(run.crashed)}</pre>'
    elif run.refused is not None:
        body = f'<div class="tag">refused at compile time · nothing ran</div><pre class="refused">{_esc(run.refused)}</pre>'
    else:
        body = _stream("printed (stdout)", run.stdout, out_marks, max_lines)
        body += _stream("errors and logs (stderr)", run.stderr, err_marks, max_lines)
        for fname, text in run.files.items():
            body += _stream(f"file left behind: {fname}", repr(text) if "\x00" in text else text, set(), max_lines)
        body = body or '<div class="tag">no output</div>'
    return f'<div class="col">{head}<div class="body">{body}</div></div>'


def _lines(text: str) -> list[str]:
    return text.rstrip("\n").split("\n") if text else []


def compare(source: str, title: str = "", stdin: str = "", optimize: bool = False,
            build: str = "release", max_lines: int = 30) -> tuple[Run, Run]:
    """Run `source` under both and show them side by side, differences in red."""
    global last
    py = cpython(source, stdin)
    ws = waspy(source, stdin, optimize, build)
    last = (py, ws)

    po, wo = _differing(_lines(py.stdout), _lines(ws.stdout))
    pe, we = _differing(_lines(py.stderr), _lines(ws.stderr))
    if ws.refused is not None or ws.crashed is not None:
        po = pe = wo = we = set()

    if ws.crashed is not None:
        verdict = (VIOLET, "⚡ The compiler crashed before it produced a program.")
    elif ws.refused is not None:
        verdict = (WSP, "🛑 Waspy refused to build it."
                   + (" CPython only found the problem while running." if py.code else ""))
    elif (py.stdout, py.stderr, py.code, py.files) == (ws.stdout, ws.stderr, ws.code, ws.files):
        verdict = (OK, "✓ Identical: same output, same errors, same exit code, same files.")
    else:
        verdict = (BAD, "≠ The two answers differ (differing lines in red).")

    display(HTML(
        STYLE + '<div class="lab">'
        + (f'<div class="title">{_esc(title)}</div>' if title else "")
        + '<div class="cols">'
        + _panel("CPython 3.13", CPY, py, po, pe, max_lines)
        + _panel("Waspy → WebAssembly", WSP, ws, wo, we, max_lines)
        + "</div>"
        + f'<div class="verdict" style="background:{verdict[0]}24;border-left:4px solid {verdict[0]}">{verdict[1]}</div>'
        + "</div>"
    ))
    return py, ws


def solo(source: str, title: str = "", max_lines: int = 40) -> Run:
    """Run `source` under CPython 3.13 only and show it."""
    py = cpython(source)
    display(HTML(
        STYLE + '<div class="lab">'
        + (f'<div class="title">{_esc(title)}</div>' if title else "")
        + '<div class="cols">' + _panel("CPython 3.13", CPY, py, set(), set(), max_lines) + "</div></div>"
    ))
    return py


def callout(text: str, color: str = WSP) -> None:
    display(HTML(
        f'<div style="border-left:4px solid {color};background:{color}17;padding:10px 14px;'
        f'border-radius:6px;margin:6px 0;font-family:-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif">{text}</div>'
    ))


def table(rows: list[tuple], header: tuple) -> None:
    """A small HTML table for numbers the notebook measures."""
    cell = "padding:5px 14px;border-bottom:1px solid rgba(127,127,127,.2)"
    th = "".join(f"<th style='text-align:left;{cell}'>{_esc(str(h))}</th>" for h in header)
    trs = "".join(
        "<tr>" + "".join(f"<td style='{cell};font-family:ui-monospace,Menlo,monospace;font-size:12.5px'>{_esc(str(c))}</td>"
                         for c in row) + "</tr>"
        for row in rows
    )
    display(HTML(f"<table style='border-collapse:collapse;margin:6px 0 14px'><tr>{th}</tr>{trs}</table>"))


# --------------------------------------------------------------------------
# Notebook glue
# --------------------------------------------------------------------------


def _magic_compare(line: str, cell: str):
    words = line.split(" ")
    opts = {"optimize": False, "build": "release", "max_lines": 30}
    title = []
    i = 0
    while i < len(words):
        w = words[i]
        if w == "--debug":
            opts["build"] = "debug"
        elif w == "--optimize":
            opts["optimize"] = True
        elif w == "--lines":
            i += 1
            opts["max_lines"] = int(words[i])
        else:
            title.append(w)  # kept as typed, quotes included
        i += 1
    compare(cell, " ".join(title), **opts)


def _magic_cpython(line: str, cell: str):
    solo(cell, line.strip())


def setup(build_debug: bool = False) -> None:
    """Register the cell magics, check the tools, build the compiler driver."""
    try:
        ip = get_ipython()  # noqa: F821 (defined inside IPython)
        ip.register_magic_function(_magic_compare, "cell", "compare")
        ip.register_magic_function(_magic_cpython, "cell", "cpython")
    except NameError:
        pass
    rows = []
    for what, cmd in [("CPython", [PYTHON, "--version"]), ("wasmtime", [WASMTIME, "--version"])]:
        try:
            out = subprocess.run(cmd, capture_output=True, text=True).stdout.strip()
        except FileNotFoundError:
            out = "missing: install it first"
        rows.append((what, out))
    builds = [("release", ["cargo", "build", "--release", "--example", "advanced_compiler"])]
    if build_debug:
        builds.append(("debug", ["cargo", "build", "--example", "advanced_compiler"]))
    for name, cmd in builds:
        start = time.perf_counter()
        done = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True)
        status = "ready" if done.returncode == 0 else "build failed: " + done.stderr[-300:]
        rows.append((f"Waspy ({name} build)", f"{status} ({time.perf_counter() - start:.1f}s)"))
    rows.append(("repository", str(REPO)))
    table(rows, ("tool", "status"))
