// `just board-check`: the development board tells the truth.
//
// Every feature on docs/modules/index.html names the test in
// tests/integration/board.rs that backs it (`test: "card::name"`). This runs
// that suite, ignored tests included, and fails when:
//
//   - a done feature names no test, or its test is missing, ignored, or fails;
//   - an open feature's test passes (the feature is done: move it on the board);
//   - a test in the suite backs no feature.
//
// This is the command the 1.0 gate runs: the board at 100% with this passing.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const html = readFileSync('docs/modules/index.html', 'utf8');
const marker = 'const moduleData = ';
const start = html.indexOf(marker) + marker.length;
let depth = 0;
let end = start;
for (; end < html.length; end++) {
    if (html[end] === '{') depth++;
    else if (html[end] === '}' && --depth === 0) break;
}
const moduleData = new Function(`return ${html.slice(start, end + 1)}`)();

const cargo = (args) =>
    execFileSync('cargo', ['test', '--all-features', '--test', 'integration_board', '--', ...args], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'pipe'],
        maxBuffer: 64 * 1024 * 1024,
    });

const listed = (output) =>
    new Set(
        output
            .split('\n')
            .filter((line) => line.endsWith(': test'))
            .map((line) => line.slice(0, -': test'.length)),
    );
const all = listed(cargo(['--list']));
const ignored = listed(cargo(['--list', '--ignored']));

// Run everything, ignored tests too, and keep going past failures.
let run;
try {
    run = cargo(['--include-ignored']);
} catch (err) {
    run = err.stdout ?? '';
}
const results = new Map();
for (const match of run.matchAll(/^test (\S+) \.\.\. (ok|FAILED)$/gm)) {
    results.set(match[1], match[2]);
}

const problems = [];
const referenced = new Set();
let done = 0;
for (const [card, module] of Object.entries(moduleData)) {
    for (const feature of module.features) {
        const where = `${module.title}: ${feature.name}`;
        const test = feature.test;
        if (feature.status === 'done') done++;
        if (!test) {
            if (feature.status === 'done') problems.push(`done, but names no test: ${where}`);
            continue;
        }
        referenced.add(test);
        if (!all.has(test)) {
            problems.push(`names ${test}, which does not exist: ${where}`);
            continue;
        }
        const result = results.get(test);
        if (feature.status === 'done') {
            if (ignored.has(test)) problems.push(`done, but ${test} is #[ignore]d: ${where}`);
            else if (result !== 'ok') problems.push(`done, but ${test} fails: ${where}`);
        } else if (result === 'ok') {
            problems.push(`${feature.status}, but ${test} passes (mark it done): ${where}`);
        }
    }
}
for (const test of all) {
    if (!referenced.has(test)) problems.push(`${test} backs no feature on the board`);
}

if (problems.length) {
    console.error(`board-check: ${problems.length} problem(s)`);
    for (const problem of problems) console.error(`  ${problem}`);
    process.exit(1);
}
console.log(`board-check: ${done} done features, each backed by a passing test; ${referenced.size - done} open ones with a test still failing`);
