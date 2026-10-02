import { EXAMPLES } from './examples.js';

const COMPILER_JS = new URL('./compiler/waspy.js', import.meta.url).href;
const COMPILER_WASM = new URL('./compiler/waspy_bg.wasm', import.meta.url).href;
const WABT_URL = 'https://cdn.jsdelivr.net/npm/wabt@1.0.39/index.js';
const BINARYEN_URL = 'https://cdn.jsdelivr.net/npm/binaryen@132.0.0/index.js';
const STORAGE_KEY = 'waspy-playground';

const SECTION_NAMES = ['custom', 'type', 'import', 'function', 'table', 'memory', 'global', 'export', 'start', 'element', 'code', 'data', 'datacount', 'tag'];
const SECTION_COLORS = {
    header: 'var(--text-2)',
    type: 'var(--teal)',
    import: 'var(--red)',
    function: 'var(--honey)',
    table: 'var(--green)',
    memory: 'var(--green)',
    global: 'var(--teal)',
    export: 'var(--ember)',
    start: 'var(--red)',
    element: 'var(--honey)',
    code: 'var(--amber)',
    data: 'var(--green)',
    datacount: 'var(--text-1)',
    custom: 'var(--text-2)',
    tag: 'var(--red)',
};

const HOST_IMPORTS = {
    waspy_host: { open: () => -1, read: () => 0, write: (_fd, _ptr, len) => len, close: () => 0 },
};

const $ = (id) => document.getElementById(id);
const isMac = /Mac|iPhone|iPad/.test(navigator.platform);

const state = {
    compiler: null,
    compilerWasm: null,
    compilerLoads: 0,
    wabt: null,
    binaryen: null,
    bytes: null,
    wat: '',
    sigs: [],
    instance: null,
    view: 'wat',
    timer: null,
    errorMarks: [],
};

let editor;
let watViewer;

function loadPrefs() {
    try {
        return JSON.parse(localStorage.getItem(STORAGE_KEY)) || {};
    } catch {
        return {};
    }
}

function savePrefs(patch) {
    try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...loadPrefs(), ...patch }));
    } catch {}
}

function setStatus(kind, text) {
    $('status').dataset.state = kind;
    $('statusText').textContent = text;
}

function formatBytes(n) {
    if (n < 1024) return `${n} B`;
    return `${(n / 1024).toFixed(n < 10240 ? 2 : 1)} KB`;
}

function escapeHtml(s) {
    return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
}

/* ---------- Loading ---------- */

async function fetchWithProgress(url, onProgress) {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`${url.split('/').pop()} returned HTTP ${res.status}`);
    const total = Number(res.headers.get('content-length')) || 0;
    if (!res.body || !total) {
        onProgress(-1);
        return new Uint8Array(await res.arrayBuffer());
    }
    const reader = res.body.getReader();
    const chunks = [];
    let received = 0;
    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        received += value.length;
        onProgress(Math.min(received / total, 1));
    }
    const out = new Uint8Array(received);
    let offset = 0;
    for (const c of chunks) {
        out.set(c, offset);
        offset += c.length;
    }
    return out;
}

async function instantiateCompiler(wasm) {
    const n = state.compilerLoads++;
    const mod = await import(n ? `${COMPILER_JS}?instance=${n}` : COMPILER_JS);
    await mod.default({ module_or_path: wasm });
    return mod;
}

async function loadCompiler() {
    $('stateUnavailable').hidden = true;
    $('stateLoading').hidden = false;
    setStatus('loading', 'Loading compiler');
    const bar = $('loadProgress');
    bar.classList.remove('indeterminate');
    bar.style.width = '0';

    try {
        state.compilerWasm = await fetchWithProgress(COMPILER_WASM, (p) => {
            if (p < 0) bar.classList.add('indeterminate');
            else bar.style.width = `${Math.round(p * 100)}%`;
        });
        state.compiler = await instantiateCompiler(state.compilerWasm);
    } catch (err) {
        console.error(err);
        $('stateLoading').hidden = true;
        $('stateUnavailable').hidden = false;
        $('unavailableMsg').textContent = `The in-browser build of Waspy could not be fetched (${err.message}).`;
        setStatus('error', 'Compiler unavailable');
        return;
    }

    $('stateLoading').hidden = true;
    $('compileBtn').disabled = false;
    compile();
}

function loadScript(src) {
    return new Promise((resolve, reject) => {
        const s = document.createElement('script');
        s.src = src;
        s.onload = resolve;
        s.onerror = () => reject(new Error(`could not load ${src}`));
        document.head.appendChild(s);
    });
}

async function getWabt() {
    if (!state.wabt) {
        state.wabt = loadScript(WABT_URL).then(() => window.WabtModule());
        state.wabt.catch(() => { state.wabt = null; });
    }
    return state.wabt;
}

async function getBinaryen() {
    if (!state.binaryen) {
        state.binaryen = import(BINARYEN_URL).then((m) => m.default);
        state.binaryen.catch(() => { state.binaryen = null; });
    }
    return state.binaryen;
}

/* ---------- Compile ---------- */

function scheduleCompile() {
    clearTimeout(state.timer);
    if ($('autoToggle').checked) state.timer = setTimeout(compile, 450);
}

// Mirrors `optimize_wasm()`: Binaryen's default pipeline at the levels of
// binaryen-rs's `CodegenConfig::default()`, which are both 0.
function optimize(binaryen, bytes) {
    const mod = binaryen.readBinary(bytes);
    try {
        const F = binaryen.Features;
        mod.setFeatures(mod.getFeatures() | F.BulkMemory | (F.BulkMemoryOpt || 0));
        binaryen.setOptimizeLevel(0);
        binaryen.setShrinkLevel(0);
        mod.optimize();
        return mod.emitBinary();
    } finally {
        mod.dispose();
    }
}

async function compile() {
    if (!state.compiler) return;
    clearTimeout(state.timer);
    const source = editor.getValue();
    savePrefs({ source });

    const optimized = $('optimizeToggle').checked;
    let binaryen;
    if (optimized) {
        setStatus('busy', 'Loading Binaryen');
        try {
            binaryen = await getBinaryen();
        } catch (err) {
            showError(`Binaryen could not be loaded: ${err.message || err}`, source);
            return;
        }
    }

    setStatus('busy', 'Compiling');
    const started = performance.now();
    let bytes;
    try {
        bytes = state.compiler.compile(source);
        state.sigs = JSON.parse(state.compiler.signatures(source));
    } catch (err) {
        if (err instanceof WebAssembly.RuntimeError) {
            await recoverFromCrash(err, source);
        } else {
            showError(String(err.message || err), source);
        }
        return;
    }

    if (optimized) {
        try {
            bytes = optimize(binaryen, bytes);
        } catch (err) {
            showError(`Binaryen could not optimize this module: ${err.message || err}`, source);
            return;
        }
    }
    const elapsed = performance.now() - started;

    clearError();
    state.bytes = bytes;
    $('output').classList.add('ready');
    try {
        state.instance = (await WebAssembly.instantiate(bytes, HOST_IMPORTS)).instance;
    } catch (err) {
        state.instance = null;
        console.error(err);
    }

    const sections = parseSections(bytes);
    const code = sections.find((s) => s.name === 'code');
    const exportsList = WebAssembly.Module.exports(new WebAssembly.Module(bytes));
    $('statSize').textContent = formatBytes(bytes.length);
    $('statFuncs').textContent = `${code ? code.count : 0} functions`;
    $('statExports').textContent = `${exportsList.filter((e) => e.kind === 'function').length} exports`;
    setStatus('ok', `${optimized ? 'Optimized' : 'Compiled'} in ${elapsed < 10 ? elapsed.toFixed(1) : Math.round(elapsed)} ms`);

    $('copyBtn').disabled = false;
    $('downloadBtn').disabled = false;
    renderBinary(bytes, sections);
    renderRun(exportsList);
    renderWat(bytes);
}

// A panic inside the compiler traps, and the instance can't be trusted after
// that, so report it and swap in a fresh one built from the same bytes.
async function recoverFromCrash(err, source) {
    let panic = null;
    try {
        panic = state.compiler.take_panic();
    } catch {}
    showError(panic || err.message, source, { internal: true });
    try {
        state.compiler = await instantiateCompiler(state.compilerWasm);
    } catch (reloadErr) {
        console.error(reloadErr);
        state.compiler = null;
        $('compileBtn').disabled = true;
        setStatus('error', 'Compiler crashed, reload the page');
    }
}

async function renderWat(bytes) {
    let wabt;
    try {
        wabt = await getWabt();
    } catch {
        watViewer.setValue(';; The WebAssembly text view needs wabt.js, which could not be loaded.\n;; The Binary and Run tabs still work.');
        return;
    }
    if (bytes !== state.bytes) return;
    let mod;
    try {
        mod = wabt.readWasm(bytes, { readDebugNames: true, bulk_memory: true, mutable_globals: true, sign_extension: true });
        mod.generateNames();
        mod.applyNames();
        state.wat = mod.toText({ foldExprs: false, inlineExport: false });
    } catch (err) {
        state.wat = `;; wabt could not disassemble this module: ${err.message || err}`;
    } finally {
        if (mod) mod.destroy();
    }
    const scroll = watViewer.getScrollInfo();
    watViewer.setValue(state.wat);
    watViewer.scrollTo(scroll.left, scroll.top);
}

/* ---------- Errors ---------- */

function locate(message, source) {
    const lineMatch = message.match(/line (\d+)(?:,? column (\d+))?/i);
    if (lineMatch) return { line: Number(lineMatch[1]) - 1, ch: lineMatch[2] ? Number(lineMatch[2]) - 1 : 0 };
    const fnMatch = message.match(/in function '([\w.:]+)'/);
    if (fnMatch) {
        const name = fnMatch[1].split(/::|\./).pop();
        const lines = source.split('\n');
        const idx = lines.findIndex((l) => new RegExp(`^\\s*def\\s+${name}\\s*\\(`).test(l));
        if (idx >= 0) return { line: idx, ch: lines[idx].indexOf('def') };
    }
    return null;
}

function showError(raw, source, { internal = false } = {}) {
    clearMarks();
    const messages = raw.split('\n').map((m) => m.replace(/^\s*also:\s*/, '').trim()).filter(Boolean);
    const list = $('errorList');
    list.innerHTML = '';
    for (const msg of messages) {
        const li = document.createElement('li');
        const text = document.createElement('span');
        text.textContent = msg;
        li.appendChild(text);
        const loc = locate(msg, source);
        if (loc && loc.line < editor.lineCount()) {
            const jump = document.createElement('button');
            jump.className = 'pg-jump';
            jump.type = 'button';
            jump.textContent = `Ln ${loc.line + 1}`;
            jump.addEventListener('click', () => {
                editor.focus();
                editor.setCursor(loc);
                editor.scrollIntoView(loc, 80);
            });
            li.appendChild(jump);
            markLine(loc.line, msg);
        }
        list.appendChild(li);
    }
    $('errorBadge').textContent = internal ? 'Internal compiler error' : 'Compile error';
    const note = $('errorNote');
    note.textContent = state.bytes ? 'Showing the output of the last successful compile.' : '';
    if (internal) {
        note.textContent = 'This is a bug in Waspy, not in your program. ';
        const link = document.createElement('a');
        link.href = issueUrl(raw, source);
        link.target = '_blank';
        link.rel = 'noopener';
        link.textContent = 'Report it with this program';
        note.appendChild(link);
    }
    $('errorPanel').hidden = false;
    $('output').classList.toggle('stale', !!state.bytes);
    setStatus('error', messages.length > 1 ? `${messages.length} errors` : 'Error');
}

function issueUrl(message, source) {
    const fence = '```';
    const program = source.length > 4000 ? '(too long for a link, paste it here)' : `${fence}python\n${source}\n${fence}`;
    const body = `The playground's compiler crashed on this program.\n\n${fence}\n${message}\n${fence}\n\n${program}\n`;
    const params = new URLSearchParams({ title: 'Internal compiler error in the playground', body });
    return `https://github.com/anistark/waspy/issues/new?${params}`;
}

function markLine(line, msg) {
    editor.addLineClass(line, 'background', 'pg-error-line');
    const marker = document.createElement('span');
    marker.className = 'pg-error-marker';
    marker.textContent = '●';
    marker.title = msg;
    editor.setGutterMarker(line, 'pg-errors', marker);
    state.errorMarks.push(line);
}

function clearMarks() {
    for (const line of state.errorMarks) {
        if (line < editor.lineCount()) editor.removeLineClass(line, 'background', 'pg-error-line');
    }
    editor.clearGutter('pg-errors');
    state.errorMarks = [];
}

function clearError() {
    clearMarks();
    $('errorPanel').hidden = true;
    $('output').classList.remove('stale');
}

/* ---------- Binary view ---------- */

function readLeb(bytes, pos) {
    let result = 0;
    let shift = 0;
    let len = 0;
    let byte;
    do {
        byte = bytes[pos + len++];
        result += (byte & 0x7f) * 2 ** shift;
        shift += 7;
    } while (byte & 0x80);
    return [result, len];
}

function parseSections(bytes) {
    const out = [{ name: 'header', start: 0, end: 8, count: 0 }];
    let pos = 8;
    while (pos < bytes.length) {
        const id = bytes[pos];
        const [size, n] = readLeb(bytes, pos + 1);
        const bodyStart = pos + 1 + n;
        const name = SECTION_NAMES[id] || `section ${id}`;
        const hasCount = id >= 1 && id <= 13 && id !== 8;
        out.push({ name, start: pos, end: bodyStart + size, count: hasCount ? readLeb(bytes, bodyStart)[0] : 0 });
        pos = bodyStart + size;
    }
    return out;
}

function renderBinary(bytes, sections) {
    const total = bytes.length;
    const bar = $('sectionBar');
    const legend = $('sectionLegend');
    bar.innerHTML = '';
    legend.innerHTML = '';

    for (const s of sections) {
        const color = SECTION_COLORS[s.name] || 'var(--text-2)';
        const size = s.end - s.start;
        const seg = document.createElement('span');
        seg.style.flex = `${size} 0 0`;
        seg.style.setProperty('--sec', color);
        seg.title = `${s.name}: ${formatBytes(size)}`;
        seg.addEventListener('click', () => jumpToOffset(s.start));
        bar.appendChild(seg);

        const li = document.createElement('li');
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.style.setProperty('--sec', color);
        btn.innerHTML = `<i></i>${s.name} <em>${formatBytes(size)} · ${Math.max(0.1, (size / total) * 100).toFixed(1)}%</em>`;
        btn.addEventListener('click', () => jumpToOffset(s.start));
        li.appendChild(btn);
        legend.appendChild(li);
    }

    const owner = new Array(total);
    for (const s of sections) owner.fill(s.name, s.start, s.end);

    const rows = [];
    for (let off = 0; off < total; off += 16) {
        let hex = '';
        let ascii = '';
        let run = null;
        let runText = '';
        const flush = () => {
            if (runText) hex += `<span class="b" style="--sec:${SECTION_COLORS[run] || 'var(--text-0)'}">${runText}</span>`;
            runText = '';
        };
        for (let i = 0; i < 16; i++) {
            const idx = off + i;
            if (idx >= total) {
                flush();
                hex += '   ';
                continue;
            }
            if (owner[idx] !== run) {
                flush();
                run = owner[idx];
            }
            runText += bytes[idx].toString(16).padStart(2, '0') + (i === 7 ? '  ' : ' ');
            const b = bytes[idx];
            ascii += b >= 0x20 && b < 0x7f ? escapeHtml(String.fromCharCode(b)) : '·';
        }
        flush();
        rows.push(`<span class="off" id="hx-${off}">${off.toString(16).padStart(6, '0')}</span>  ${hex} <span class="asc">${ascii}</span>`);
    }
    $('hexDump').innerHTML = rows.join('\n');
}

function jumpToOffset(offset) {
    const row = $(`hx-${offset - (offset % 16)}`);
    if (!row) return;
    const view = $('viewBinary');
    const top = row.offsetTop - view.querySelector('.pg-sections').offsetHeight - 8;
    view.scrollTo({ top, behavior: 'smooth' });
    row.classList.add('flash');
    setTimeout(() => row.classList.remove('flash'), 900);
}

/* ---------- Run view ---------- */

const RUNNABLE = new Set(['int', 'float', 'bool']);

function parseParam(p) {
    const i = p.indexOf(':');
    return i < 0 ? { name: p.trim(), type: 'unknown' } : { name: p.slice(0, i).trim(), type: p.slice(i + 1).trim() };
}

function renderRun(exportsList) {
    const list = $('runList');
    list.innerHTML = '';
    const exported = new Set(exportsList.filter((e) => e.kind === 'function').map((e) => e.name));
    const fns = state.sigs.filter((s) => exported.has(s.name));

    if (!fns.length) {
        list.innerHTML = '<p class="pg-run-empty">This module exports no top-level functions to call.</p>';
        return;
    }

    for (const sig of fns) {
        const params = sig.params.map(parseParam);
        const blocked = params.find((p) => !RUNNABLE.has(p.type));
        const card = document.createElement('div');
        card.className = 'pg-fn';

        const sigEl = document.createElement('div');
        sigEl.className = 'pg-fn-sig';
        const paramHtml = params.map((p) => `${escapeHtml(p.name)}: <span class="ty">${escapeHtml(p.type)}</span>`).join(', ');
        sigEl.innerHTML = `<span class="kw">def</span> <span class="fn">${escapeHtml(sig.name)}</span>(${paramHtml}) → <span class="ty">${escapeHtml(sig.returns)}</span>`;
        card.appendChild(sigEl);

        const args = document.createElement('div');
        args.className = 'pg-fn-args';
        const inputs = params.map((p) => {
            const label = document.createElement('label');
            label.className = 'pg-arg';
            label.textContent = p.name;
            let input;
            if (p.type === 'bool') {
                input = document.createElement('select');
                input.innerHTML = '<option value="1">True</option><option value="0">False</option>';
            } else {
                input = document.createElement('input');
                input.type = 'text';
                input.inputMode = p.type === 'float' ? 'decimal' : 'numeric';
                input.placeholder = p.type === 'float' ? '0.0' : '0';
                input.spellcheck = false;
                input.addEventListener('input', () => input.classList.remove('invalid'));
            }
            input.disabled = !!blocked;
            input.setAttribute('aria-label', `${sig.name} ${p.name}`);
            input.addEventListener('keydown', (e) => {
                if (e.key === 'Enter') call(sig, params, inputs);
            });
            label.appendChild(input);
            args.appendChild(label);
            return input;
        });

        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'pg-run-btn';
        btn.innerHTML = '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M6 4.5v15a1 1 0 0 0 1.5.86l12.5-7.5a1 1 0 0 0 0-1.72L7.5 3.64A1 1 0 0 0 6 4.5z"/></svg>Run';
        btn.disabled = !!blocked || !state.instance;
        btn.addEventListener('click', () => call(sig, params, inputs));
        args.appendChild(btn);
        card.appendChild(args);

        if (blocked) {
            const note = document.createElement('p');
            note.className = 'pg-fn-note';
            note.textContent = `Takes a ${blocked.type} argument. The playground can pass int, float and bool values.`;
            card.appendChild(note);
        }
        list.appendChild(card);
    }
}

function pyFloat(x) {
    if (Number.isNaN(x)) return 'nan';
    if (!Number.isFinite(x)) return x > 0 ? 'inf' : '-inf';
    if (Number.isInteger(x) && Math.abs(x) < 1e16) return x.toFixed(1);
    return String(x).replace(/e([+-])(\d)$/, 'e$10$2');
}

function pyStr(s) {
    const quote = s.includes("'") && !s.includes('"') ? '"' : "'";
    const body = s.replace(/[\\\n\r\t]/g, (c) => ({ '\\': '\\\\', '\n': '\\n', '\r': '\\r', '\t': '\\t' }[c]));
    return quote + (quote === "'" ? body.replace(/'/g, "\\'") : body) + quote;
}

function readStr(ptr) {
    const mem = state.instance.exports.memory;
    if (!mem || ptr < 4 || ptr > mem.buffer.byteLength) return null;
    const len = new DataView(mem.buffer).getUint32(ptr - 4, true);
    if (ptr + len > mem.buffer.byteLength) return null;
    return new TextDecoder().decode(new Uint8Array(mem.buffer, ptr, len));
}

function formatResult(value, type) {
    if (value === undefined || type === 'None') return 'None';
    switch (type) {
        case 'int': return String(value | 0);
        case 'float': return pyFloat(value);
        case 'bool': return value ? 'True' : 'False';
        case 'str': {
            const s = readStr(value);
            return s === null ? `<str at ${value}>` : pyStr(s);
        }
        default: return `${value}  (a ${type}; the playground shows int, float, bool and str values)`;
    }
}

function call(sig, params, inputs) {
    if (!state.instance) return;
    const args = [];
    let ok = true;
    params.forEach((p, i) => {
        const input = inputs[i];
        const raw = input.value.trim();
        if (p.type === 'bool') return args.push(Number(raw));
        const valid = p.type === 'int' ? /^[+-]?\d+$/.test(raw) : raw !== '' && Number.isFinite(Number(raw));
        if (!valid) {
            input.classList.add('invalid');
            ok = false;
        }
        args.push(p.type === 'int' ? Number(raw) | 0 : Number(raw));
    });
    if (!ok) return;

    const shown = params.map((p, i) => (p.type === 'bool' ? (args[i] ? 'True' : 'False') : p.type === 'float' ? pyFloat(args[i]) : String(args[i])));
    const started = performance.now();
    let result;
    let isError = false;
    try {
        result = formatResult(state.instance.exports[sig.name](...args), sig.returns);
    } catch (err) {
        isError = true;
        result = err instanceof WebAssembly.RuntimeError ? `Trapped: ${err.message}. An exception nothing caught unwinds out of the module.` : String(err);
    }
    log(`${sig.name}(${shown.join(', ')})`, result, isError, performance.now() - started);
}

function log(callText, result, isError, ms) {
    const body = $('console');
    const empty = body.querySelector('.pg-console-empty');
    if (empty) empty.remove();
    const row = document.createElement('div');
    row.className = 'pg-log';
    row.innerHTML = `<span class="pg-log-call"></span><span class="pg-log-result${isError ? ' err' : ''}"></span><span class="pg-log-time">${ms < 1 ? '<1' : ms.toFixed(1)} ms</span>`;
    row.querySelector('.pg-log-call').textContent = callText;
    row.querySelector('.pg-log-result').textContent = result;
    body.appendChild(row);
    body.scrollTop = body.scrollHeight;
}

/* ---------- Views, share, resize ---------- */

function setView(view) {
    state.view = view;
    document.querySelectorAll('.pg-tab[data-view]').forEach((t) => {
        const active = t.dataset.view === view;
        t.classList.toggle('active', active);
        t.setAttribute('aria-selected', String(active));
    });
    document.querySelectorAll('.pg-view').forEach((v) => { v.hidden = v.dataset.view !== view; });
    $('copyBtn').title = view === 'binary' ? 'Copy the bytes as hex' : 'Copy the WebAssembly text';
    $('copyBtn').setAttribute('aria-label', $('copyBtn').title);
    if (view === 'wat') watViewer.refresh();
    savePrefs({ view });
}

function flashCopied(btn) {
    btn.classList.add('copied');
    setTimeout(() => btn.classList.remove('copied'), 1500);
}

async function copyOutput() {
    if (!state.bytes) return;
    const text = state.view === 'binary'
        ? Array.from(state.bytes, (b) => b.toString(16).padStart(2, '0')).join(' ')
        : state.wat;
    await navigator.clipboard.writeText(text);
    flashCopied($('copyBtn'));
}

function download() {
    if (!state.bytes) return;
    const url = URL.createObjectURL(new Blob([state.bytes], { type: 'application/wasm' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = 'main.wasm';
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function toBase64Url(bytes) {
    let bin = '';
    bytes.forEach((b) => { bin += String.fromCharCode(b); });
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function fromBase64Url(s) {
    const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/'));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

async function pipe(bytes, stream) {
    return new Uint8Array(await new Response(new Blob([bytes]).stream().pipeThrough(stream)).arrayBuffer());
}

async function share() {
    const raw = new TextEncoder().encode(editor.getValue());
    const hash = 'CompressionStream' in window
        ? `z=${toBase64Url(await pipe(raw, new CompressionStream('deflate-raw')))}`
        : `code=${toBase64Url(raw)}`;
    const url = `${location.origin}${location.pathname}#${hash}`;
    history.replaceState(null, '', `#${hash}`);
    const btn = $('shareBtn');
    const label = btn.querySelector('span');
    try {
        await navigator.clipboard.writeText(url);
        label.textContent = 'Link copied';
    } catch {
        label.textContent = 'Link in address bar';
    }
    flashCopied(btn);
    setTimeout(() => { label.textContent = 'Share'; }, 1800);
}

async function sourceFromHash() {
    const params = new URLSearchParams(location.hash.slice(1));
    try {
        if (params.has('z')) return new TextDecoder().decode(await pipe(fromBase64Url(params.get('z')), new DecompressionStream('deflate-raw')));
        if (params.has('code')) return new TextDecoder().decode(fromBase64Url(params.get('code')));
    } catch (err) {
        console.warn('Could not read the shared program', err);
    }
    return null;
}

function setSplit(pct) {
    const clamped = Math.min(80, Math.max(20, pct));
    $('workspace').style.setProperty('--split', `calc(${clamped}% - 7px)`);
    $('gutter').setAttribute('aria-valuenow', String(Math.round(clamped)));
    return clamped;
}

function initResize(initial) {
    const gutter = $('gutter');
    const workspace = $('workspace');
    let current = setSplit(initial || 50);

    const refresh = () => {
        editor.refresh();
        watViewer.refresh();
    };

    gutter.addEventListener('pointerdown', (e) => {
        gutter.setPointerCapture(e.pointerId);
        gutter.classList.add('dragging');
        document.body.classList.add('pg-resizing');
        const move = (ev) => {
            const rect = workspace.getBoundingClientRect();
            current = setSplit(((ev.clientX - rect.left) / rect.width) * 100);
        };
        const up = () => {
            gutter.removeEventListener('pointermove', move);
            gutter.classList.remove('dragging');
            document.body.classList.remove('pg-resizing');
            savePrefs({ split: current });
            refresh();
        };
        gutter.addEventListener('pointermove', move);
        gutter.addEventListener('pointerup', up, { once: true });
    });

    gutter.addEventListener('dblclick', () => {
        current = setSplit(50);
        savePrefs({ split: current });
        refresh();
    });

    gutter.addEventListener('keydown', (e) => {
        const step = e.shiftKey ? 10 : 2;
        if (e.key === 'ArrowLeft') current = setSplit(current - step);
        else if (e.key === 'ArrowRight') current = setSplit(current + step);
        else return;
        e.preventDefault();
        savePrefs({ split: current });
        refresh();
    });
}

/* ---------- Boot ---------- */

async function init() {
    const prefs = loadPrefs();
    const shared = await sourceFromHash();
    const select = $('exampleSelect');
    for (const ex of EXAMPLES) select.add(new Option(ex.name, ex.id));

    const example = EXAMPLES.find((e) => e.id === prefs.example) || EXAMPLES[0];
    select.value = example.id;

    editor = CodeMirror($('editor'), {
        value: shared ?? prefs.source ?? example.code,
        mode: 'python',
        lineNumbers: true,
        indentUnit: 4,
        tabSize: 4,
        indentWithTabs: false,
        matchBrackets: true,
        autoCloseBrackets: true,
        styleActiveLine: true,
        gutters: ['pg-errors', 'CodeMirror-linenumbers'],
        extraKeys: {
            Tab: (cm) => (cm.somethingSelected() ? cm.indentSelection('add') : cm.replaceSelection('    ', 'end')),
            'Shift-Tab': (cm) => cm.indentSelection('subtract'),
            'Cmd-Enter': () => compile(),
            'Ctrl-Enter': () => compile(),
        },
    });

    watViewer = CodeMirror($('viewWat'), {
        value: '',
        mode: 'wast',
        readOnly: true,
        lineNumbers: true,
        cursorBlinkRate: -1,
    });

    editor.on('change', () => {
        if (state.errorMarks.length) clearMarks();
        if (location.hash) history.replaceState(null, '', location.pathname);
        scheduleCompile();
    });
    editor.on('cursorActivity', () => {
        const { line, ch } = editor.getCursor();
        $('cursorPos').textContent = `Ln ${line + 1}, Col ${ch + 1}`;
    });

    $('compileKbd').textContent = isMac ? '⌘↵' : 'Ctrl ↵';
    $('compileBtn').title = `Compile (${isMac ? '⌘' : 'Ctrl'}+Enter)`;
    $('compileBtn').addEventListener('click', compile);
    $('retryBtn').addEventListener('click', loadCompiler);
    $('shareBtn').addEventListener('click', share);
    $('copyBtn').addEventListener('click', copyOutput);
    $('downloadBtn').addEventListener('click', download);
    $('clearConsole').addEventListener('click', () => {
        $('console').innerHTML = '<p class="pg-console-empty">Call an exported function to see what it returns.</p>';
    });

    select.addEventListener('change', () => {
        const ex = EXAMPLES.find((e) => e.id === select.value);
        savePrefs({ example: ex.id });
        editor.setValue(ex.code);
        editor.clearHistory();
        compile();
    });

    $('resetBtn').addEventListener('click', () => {
        const ex = EXAMPLES.find((e) => e.id === select.value);
        editor.setValue(ex.code);
        compile();
    });

    const optimizeToggle = $('optimizeToggle');
    const autoToggle = $('autoToggle');
    optimizeToggle.checked = !!prefs.optimize;
    autoToggle.checked = prefs.auto !== false;
    optimizeToggle.addEventListener('change', () => {
        savePrefs({ optimize: optimizeToggle.checked });
        compile();
    });
    autoToggle.addEventListener('change', () => {
        savePrefs({ auto: autoToggle.checked });
        if (autoToggle.checked) compile();
    });

    document.querySelectorAll('.pg-tab[data-view]').forEach((t) => t.addEventListener('click', () => setView(t.dataset.view)));
    document.addEventListener('keydown', (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === 'Enter' && !e.defaultPrevented) {
            e.preventDefault();
            compile();
        }
    });

    initResize(prefs.split);
    setView(prefs.view || 'wat');
    loadCompiler();
    getWabt().catch(() => {});
}

init();
