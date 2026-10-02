#!/usr/bin/env node
// Verify the docs-site playground build (`just playground`) end to end.
//
// Loads the wasm-bindgen bundle from docs/playground/compiler/ under Node,
// compiles every example the page ships (docs/playground/examples.js), calls
// the functions listed below, and requires the answer CPython gives for the
// same source. The refused example must fail to compile.
//
// Run it through `just playground-verify`.

import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { exit } from "node:process";

const root = new URL("../docs/playground/", import.meta.url);
const compiler = await import(new URL("compiler/waspy.js", root));
await compiler.default({ module_or_path: await readFile(new URL("compiler/waspy_bg.wasm", root)) });
const { EXAMPLES } = await import(new URL("examples.js", root));

const CALLS = {
  fibonacci: [["fib", 20], ["fib_iter", 30], ["is_prime", 97], ["is_prime", 91]],
  strings: [["slug"], ["count_vowels"], ["badge", 1], ["badge", 3]],
  classes: [["describe_circle", 2.0], ["describe_square", 1.5]],
  collections: [["squares", 10], ["word_count"], ["median", 9, 2, 5], ["unique_count"]],
  exceptions: [["try_withdraw", 30], ["try_withdraw", 300], ["parse_or_default", 7]],
  generators: [["total_countdown", 10], ["sum_evens", 10]],
};
const REFUSED = { refused: "eval" };

function cpython(source, name, args) {
  const program = `${source}\nimport json, sys\nprint(json.dumps(${name}(*json.loads(sys.argv[1]))))\n`;
  return JSON.parse(execFileSync("python3", ["-c", program, JSON.stringify(args)], { encoding: "utf8" }));
}

function decode(instance, value, type) {
  switch (type) {
    case "bool":
      return value !== 0;
    case "str": {
      const { buffer } = instance.exports.memory;
      const len = new DataView(buffer).getUint32(value - 4, true);
      return new TextDecoder().decode(new Uint8Array(buffer, value, len));
    }
    default:
      return value;
  }
}

const failures = [];
let passed = 0;

for (const example of EXAMPLES) {
  if (example.id in REFUSED) {
    try {
      compiler.compile(example.code);
      failures.push(`${example.id}: compiled, but it must be refused`);
    } catch (error) {
      if (error.message.includes(REFUSED[example.id])) passed += 1;
      else failures.push(`${example.id}: refused with an unexpected message: ${error.message}`);
    }
    continue;
  }

  const calls = CALLS[example.id];
  if (!calls) {
    failures.push(`${example.id}: no checks listed in scripts/verify_playground.mjs`);
    continue;
  }

  let instance;
  let sigs;
  try {
    const bytes = compiler.compile(example.code);
    sigs = JSON.parse(compiler.signatures(example.code));
    ({ instance } = await WebAssembly.instantiate(bytes, {}));
  } catch (error) {
    failures.push(`${example.id}: ${error.message}`);
    continue;
  }

  for (const [name, ...args] of calls) {
    const label = `${example.id}: ${name}(${args.join(", ")})`;
    const sig = sigs.find((s) => s.name === name);
    if (!sig || typeof instance.exports[name] !== "function") {
      failures.push(`${label}: not exported with a signature`);
      continue;
    }
    const expected = cpython(example.code, name, args);
    let got;
    try {
      got = decode(instance, instance.exports[name](...args), sig.returns);
    } catch (error) {
      failures.push(`${label}: trapped: ${error.message}`);
      continue;
    }
    if (got === expected) passed += 1;
    else failures.push(`${label}: answered ${JSON.stringify(got)}, CPython answers ${JSON.stringify(expected)}`);
  }
  console.log(`  ${example.id}: ${calls.length} checks`);
}

if (failures.length > 0) {
  console.error(`\nplayground: ${failures.length} failed, ${passed} passed`);
  for (const failure of failures) console.error(`  FAIL ${failure}`);
  exit(1);
}

console.log(`\nplayground: ${passed} checks passed`);
