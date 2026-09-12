#!/usr/bin/env python3
"""Verify the 0.16.0 end-to-end programs under the `wasmtime` CLI.

Reads the manifest written by `cargo run --example verify_runtime` and invokes
every `runtime_check_*` export, requiring the answer the manifest names: 1 for
a check, 0 for a negative control. The expected values themselves live in the
Python checker suffixes under tests/fixtures/runtime/, so this runner and the
Node one assert exactly the same thing.

The CLI can call an export but cannot read the module's memory, which is why
the comparisons happen inside the module rather than here.

Run it through `just verify-runtime`.
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path

MANIFEST = Path(sys.argv[1] if len(sys.argv) > 1 else "examples/output/runtime/manifest.json")


def invoke(wasm: str, function: str) -> str:
    """Call one export and return its printed result."""
    result = subprocess.run(
        ["wasmtime", "run", "--invoke", function, wasm],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        # The stderr tail carries the trap or the missing-export message; the
        # leading `--invoke is experimental` warning is not worth reprinting.
        detail = result.stderr.strip().splitlines()
        raise RuntimeError(detail[-1] if detail else f"exit {result.returncode}")
    return result.stdout.strip()


def main() -> int:
    if shutil.which("wasmtime") is None:
        print("wasmtime not found on PATH; install it from https://wasmtime.dev", file=sys.stderr)
        return 1
    if not MANIFEST.exists():
        print(f"{MANIFEST} not found; run `cargo run --example verify_runtime`", file=sys.stderr)
        return 1

    manifest = json.loads(MANIFEST.read_text())
    failures = []
    passed = 0

    for module in manifest["modules"]:
        kind = "optimized" if module["optimized"] else "unoptimized"
        label = f"{module['program']} ({kind})"
        for check in module["checks"]:
            name, expect = check["function"], check["expect"]
            try:
                got = invoke(module["wasm"], name)
            except RuntimeError as error:
                failures.append(f"{label}: {name} failed: {error}")
                continue
            if got == str(expect):
                passed += 1
            else:
                failures.append(f"{label}: {name} answered {got!r}, expected {expect}")
        print(f"  {label}: {len(module['checks'])} checks")

    if failures:
        print(f"\nwasmtime: {len(failures)} failed, {passed} passed", file=sys.stderr)
        for failure in failures:
            print(f"  FAIL {failure}", file=sys.stderr)
        return 1

    version = subprocess.run(
        ["wasmtime", "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()
    print(f"\n{version}: {passed} checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
