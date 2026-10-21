#!/usr/bin/env node
// Verify the 0.16.0 end-to-end programs under Node's WebAssembly engine (V8).
//
// Reads the manifest written by `cargo run --example verify_runtime` and calls
// every `runtime_check_*` export, requiring the answer the manifest names: 1
// for a check, 0 for a negative control. The expected values themselves live
// in the Python checker suffixes under tests/fixtures/runtime/, so this runner
// and the wasmtime one assert exactly the same thing.
//
// Run it through `just verify-runtime`.

import { readFile } from "node:fs/promises";
import { argv, exit } from "node:process";
import { createWasi, WasiExit } from "../docs/src/playground/wasi.js";

const manifestPath = argv[2] ?? "examples/output/runtime/manifest.json";

// A compiled waspy module imports what its program uses, all of it WASI
// preview 1 (printing, input, arguments, and `open()`), through the same shim
// the playground ships.
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const failures = [];
let passed = 0;

for (const mod of manifest.modules) {
  const label = `${mod.program} (${mod.optimized ? "optimized" : "unoptimized"})`;
  let instance;
  try {
    const bytes = await readFile(mod.wasm);
    const wasi = createWasi({ onStdout: () => {}, onStderr: () => {} });
    ({ instance } = await WebAssembly.instantiate(bytes, wasi.imports));
    wasi.bind(instance);
  } catch (error) {
    failures.push(`${label}: instantiate failed: ${error.message}`);
    continue;
  }

  for (const { function: name, expect } of mod.checks) {
    const fn = instance.exports[name];
    if (typeof fn !== "function") {
      failures.push(`${label}: ${name} is not exported`);
      continue;
    }
    let got;
    try {
      got = fn();
    } catch (error) {
      failures.push(`${label}: ${name} trapped: ${error.message}`);
      continue;
    }
    if (got === expect) {
      passed += 1;
    } else {
      failures.push(`${label}: ${name} answered ${got}, expected ${expect}`);
    }
  }
  console.log(`  ${label}: ${mod.checks.length} checks`);
}

// The WASI commands: `_start` with the fixture's input and arguments must
// write exactly what CPython wrote, and exit as it did.
for (const cmd of manifest.commands ?? []) {
  const label = `${cmd.program} (${cmd.optimized ? "optimized" : "unoptimized"}, command)`;
  let stdout = "";
  let stderr = "";
  const wasi = createWasi({
    args: ["main.py", ...cmd.args],
    stdin: await readFile(cmd.stdin),
    onStdout: (t) => (stdout += t),
    onStderr: (t) => (stderr += t),
  });
  let code = 0;
  try {
    const { instance } = await WebAssembly.instantiate(await readFile(cmd.wasm), wasi.imports);
    wasi.bind(instance);
    instance.exports._start();
  } catch (error) {
    if (error instanceof WasiExit) code = error.code;
    else {
      failures.push(`${label}: ${error.message}`);
      continue;
    }
  }
  const want = {
    stdout: await readFile(cmd.stdout, "utf8"),
    stderr: await readFile(cmd.stderr, "utf8"),
  };
  if (stdout !== want.stdout) failures.push(`${label}: stdout differs from CPython's`);
  else if (stderr !== want.stderr) failures.push(`${label}: stderr differs from CPython's`);
  else if (code !== cmd.exit) failures.push(`${label}: exited ${code}, CPython ${cmd.exit}`);
  else passed += 1;
  console.log(`  ${label}: output compared`);
}

if (failures.length > 0) {
  console.error(`\nnode: ${failures.length} failed, ${passed} passed`);
  for (const failure of failures) {
    console.error(`  FAIL ${failure}`);
  }
  exit(1);
}

console.log(`\nnode ${process.version}: ${passed} checks passed`);
