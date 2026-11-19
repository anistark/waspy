// `just board-check`: the development board tells the truth.
//
// The board's data is docs/src/_data/modules.json, which the site renders at
// /modules/, a page per Python version in docs/src/_data/python.json. Cards
// are categories; a card's status is not stored, since it depends on the
// version shown. Every feature names the test in tests/integration/board.rs
// that backs it (`"test": "module::name"`; the module is the card the test
// was first written for, so a test id stays put when cards are regrouped).
// This runs that suite, ignored tests included, and fails when:
//
//   - a done feature names no test, or its test is missing, ignored, or fails;
//   - an open feature's test passes (the feature is done: move it on the board);
//   - a test in the suite backs no feature;
//   - a card has no section, or a feature's `python` is not a listed version;
//   - a feature from a version after the first has no `change` (new or
//     changed) or no `ref` (the PEP or What's New section it comes from).

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const moduleData = JSON.parse(readFileSync('docs/src/_data/modules.json', 'utf8'));
const versions = JSON.parse(readFileSync('docs/src/_data/python.json', 'utf8')).map((entry) => entry.version);
const SECTIONS = ['Language', 'Built-in types', 'Standard library', 'Platform'];

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
const later = new Map();
for (const module of Object.values(moduleData)) {
    if (!SECTIONS.includes(module.section)) {
        problems.push(`${module.title} has section ${JSON.stringify(module.section)}, not one of ${SECTIONS.join(', ')}`);
    }
    for (const feature of module.features) {
        const where = `${module.title}: ${feature.name}`;
        const test = feature.test;
        if (feature.status === 'done') done++;
        if (feature.python !== undefined) {
            if (!versions.includes(feature.python)) {
                problems.push(`python ${feature.python} is not in python.json: ${where}`);
            } else if (feature.python !== versions[0]) {
                later.set(feature.python, (later.get(feature.python) ?? 0) + 1);
                if (feature.change !== 'new' && feature.change !== 'changed') problems.push(`no change (new or changed): ${where}`);
                if (!feature.ref?.label || !feature.ref?.url) problems.push(`no ref (a label and a url): ${where}`);
            }
        }
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
const laterItems = versions
    .filter((version) => later.has(version))
    .map((version) => `${later.get(version)} for ${version}`)
    .join(', ');
console.log(`board-check: ${done} done features, each backed by a passing test; ${referenced.size - done} open ones with a test still failing${laterItems ? `; Python's later versions add ${laterItems}` : ''}`);
