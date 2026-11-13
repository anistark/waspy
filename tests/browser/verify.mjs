#!/usr/bin/env node
// The browser runtime gate: the end-to-end programs, run in headless Chromium
// through the playground's WASI shim, each checked against Pyodide running
// the same Python in the same page.
//
// The Node and wasmtime runners (scripts/verify_runtime_*) check these
// programs against CPython's answers. This one runs them where a web program
// will run, and takes its reference from Pyodide, the Python a browser runs:
// the reference the web cards on the development board are checked against,
// since CPython has no browser. For each module built by
// `cargo run --example verify_runtime`, every `runtime_check_*` export must
// answer what the manifest names in the browser and under Pyodide alike; for
// each WASI command, the compiled program's standard output, standard error,
// and exit status must be Pyodide's for the same input and arguments.
//
// Run it through `just browser-verify`. It needs this directory's
// dependencies (`npm ci`) and Playwright's Chromium
// (`npx playwright install chromium`).

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { argv, exit } from "node:process";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const root = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const manifestPath = resolve(root, argv[2] ?? "examples/output/runtime/manifest.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));

const TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".json": "application/json",
    ".map": "application/json",
    ".wasm": "application/wasm",
    ".zip": "application/zip",
    ".py": "text/plain; charset=utf-8",
};

// The repository, read-only and on loopback: the page fetches the WASI shim,
// Pyodide, the binaries, and the Python they were compiled from.
const server = createServer(async (request, response) => {
    const path = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    const file = resolve(root, `.${path}`);
    if (request.method !== "GET" || !file.startsWith(root + sep)) {
        response.writeHead(403).end();
        return;
    }
    try {
        const body = await readFile(file);
        response.writeHead(200, { "content-type": TYPES[extname(file)] ?? "application/octet-stream" });
        response.end(body);
    } catch {
        response.writeHead(404).end();
    }
});
await new Promise((ready) => server.listen(0, "127.0.0.1", ready));
const origin = `http://127.0.0.1:${server.address().port}`;

const failures = [];
let passed = 0;
const browser = await chromium.launch();
try {
    const page = await browser.newPage();
    // The gate is hermetic: a request off loopback is a failure, not a fetch.
    await page.route("**/*", (route) => {
        const url = route.request().url();
        if (url.startsWith(`${origin}/`)) return route.continue();
        failures.push(`the page tried to fetch ${url}`);
        return route.abort();
    });
    page.on("pageerror", (error) => failures.push(`page error: ${error.message}`));
    await page.goto(`${origin}/tests/browser/index.html`);
    await page.waitForFunction(() => window.gateReady !== undefined);
    const versions = await page.evaluate(() => window.gateReady);
    console.log(`Chromium ${browser.version()}, Pyodide ${versions.pyodide} (Python ${versions.python})\n`);

    // Pyodide's answers, once per program (both optimization levels share them).
    const references = new Map();

    for (const mod of manifest.modules) {
        const label = `${mod.program} (${mod.optimized ? "optimized" : "unoptimized"})`;
        const names = mod.checks.map((check) => check.function);
        if (!mod.python) {
            failures.push(`${label}: the manifest names no Python source; rebuild it`);
            continue;
        }
        let got;
        try {
            got = await page.evaluate(([wasm, names]) => window.gate.waspyChecks(wasm, names), [mod.wasm, names]);
            if (!references.has(mod.program)) {
                const { dir, files } = mod.python;
                references.set(
                    mod.program,
                    await page.evaluate(
                        ([dir, files, names]) => window.gate.pyodideChecks(dir, files, names),
                        [dir, files, names],
                    ),
                );
            }
        } catch (error) {
            failures.push(`${label}: ${error.message}`);
            continue;
        }
        const reference = references.get(mod.program);
        for (const { function: name, expect } of mod.checks) {
            if (reference[name] !== expect) {
                failures.push(`${label}: ${name}: Pyodide answers ${reference[name]}, the manifest ${expect}`);
            } else if (got[name] !== expect) {
                failures.push(`${label}: ${name} answered ${got[name]} in the browser, Pyodide ${reference[name]}`);
            } else {
                passed += 1;
            }
        }
        console.log(`  ${label}: ${mod.checks.length} checks`);
    }

    for (const cmd of manifest.commands ?? []) {
        const label = `${cmd.program} (${cmd.optimized ? "optimized" : "unoptimized"}, command)`;
        const stdin = await readFile(resolve(root, cmd.stdin), "utf8");
        // Pyodide's run, once per program, input, and arguments.
        const key = `command:${cmd.program}:${cmd.stdin}:${JSON.stringify(cmd.args)}`;
        let got;
        try {
            got = await page.evaluate(
                ([wasm, stdin, args]) => window.gate.waspyCommand(wasm, stdin, args),
                [cmd.wasm, stdin, cmd.args],
            );
            if (!references.has(key)) {
                references.set(
                    key,
                    await page.evaluate(
                        ([source, stdin, args]) => window.gate.pyodideCommand(source, stdin, args),
                        [cmd.source, stdin, cmd.args],
                    ),
                );
            }
        } catch (error) {
            failures.push(`${label}: ${error.message}`);
            continue;
        }
        const reference = references.get(key);
        if (got.error) failures.push(`${label}: ${got.error}`);
        else if (got.stdout !== reference.stdout) failures.push(`${label}: stdout differs from Pyodide's`);
        else if (got.stderr !== reference.stderr) failures.push(`${label}: stderr differs from Pyodide's`);
        else if (got.exit !== reference.exit) failures.push(`${label}: exited ${got.exit}, Pyodide ${reference.exit}`);
        else passed += 1;
        console.log(`  ${label}: output compared`);
    }
} finally {
    await browser.close();
    server.close();
}

if (failures.length > 0) {
    console.error(`\nbrowser: ${failures.length} failed, ${passed} passed`);
    for (const failure of failures) console.error(`  FAIL ${failure}`);
    exit(1);
}

console.log(`\nbrowser: ${passed} checks passed, each against Pyodide`);
