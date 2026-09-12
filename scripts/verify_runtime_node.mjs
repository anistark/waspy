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

const manifestPath = argv[2] ?? "examples/output/runtime/manifest.json";

// A compiled waspy module has no host imports unless the program uses file
// I/O, and none of these do. The stubs are defined anyway so that a program
// added later still instantiates; an unused import object is ignored.
const imports = {
  waspy_host: {
    open: () => -1,
    read: () => 0,
    write: (_fd, _buf, len) => len,
    close: () => 0,
  },
};

const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const failures = [];
let passed = 0;

for (const mod of manifest.modules) {
  const label = `${mod.program} (${mod.optimized ? "optimized" : "unoptimized"})`;
  let instance;
  try {
    const bytes = await readFile(mod.wasm);
    ({ instance } = await WebAssembly.instantiate(bytes, imports));
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

if (failures.length > 0) {
  console.error(`\nnode: ${failures.length} failed, ${passed} passed`);
  for (const failure of failures) {
    console.error(`  FAIL ${failure}`);
  }
  exit(1);
}

console.log(`\nnode ${process.version}: ${passed} checks passed`);
