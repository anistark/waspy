// The playground's editors, on CodeMirror 6: the Python source editor and
// the read-only WebAssembly text viewer. Colors come from the site's CSS
// custom properties, so both follow the light/dark switch on their own; the
// `themechange` event only flips CodeMirror's own dark flag (which picks
// the base styling of tooltips and panels).

import { EditorState, Compartment, StateEffect, StateField, RangeSetBuilder, EditorSelection } from '@codemirror/state';
import {
    EditorView,
    Decoration,
    ViewPlugin,
    keymap,
    lineNumbers,
    highlightActiveLine,
    highlightActiveLineGutter,
    highlightSpecialChars,
    drawSelection,
    dropCursor,
    rectangularSelection,
    crosshairCursor,
} from '@codemirror/view';
import {
    HighlightStyle,
    StreamLanguage,
    syntaxHighlighting,
    syntaxTree,
    bracketMatching,
    indentOnInput,
    indentUnit,
    foldGutter,
    foldKeymap,
} from '@codemirror/language';
import { defaultKeymap, history, historyKeymap, indentLess, indentMore } from '@codemirror/commands';
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete';
import { highlightSelectionMatches, searchKeymap } from '@codemirror/search';
import { lintGutter, setDiagnostics, setDiagnosticsEffect } from '@codemirror/lint';
import { python } from '@codemirror/lang-python';
import { wast } from '@codemirror/legacy-modes/mode/wast';
import { tags as t } from '@lezer/highlight';

const v = (name) => `var(--c-${name})`;
const isDark = () => document.documentElement.getAttribute('data-theme') !== 'light';

/* ---------- Theme ---------- */

const baseTheme = EditorView.theme({
    '&': {
        height: '100%',
        color: v('ink'),
        backgroundColor: 'transparent',
        fontSize: '0.85rem',
    },
    '&.cm-focused': { outline: 'none' },
    '.cm-scroller': {
        fontFamily: 'var(--font-mono)',
        lineHeight: '1.7',
        fontVariantLigatures: 'none',
        fontFeatureSettings: '"liga" 0, "calt" 0',
        scrollbarWidth: 'thin',
        scrollbarColor: `${v('line-strong')} transparent`,
    },
    '.cm-content': { padding: '0.9rem 0', caretColor: v('amber') },
    '.cm-line': { padding: '0 1.1rem 0 0.75rem' },
    '.cm-gutters': {
        backgroundColor: 'transparent',
        color: v('ink-2'),
        border: 'none',
        borderRight: `1px solid ${v('line')}`,
    },
    '.cm-lineNumbers .cm-gutterElement': { padding: '0 0.5rem 0 0.9rem', minWidth: '2.6rem', opacity: '0.75' },
    '.cm-activeLine': { backgroundColor: `color-mix(in srgb, ${v('amber')} 6%, transparent)` },
    '.cm-activeLineGutter': { backgroundColor: 'transparent', color: v('honey') },
    '.cm-activeLineGutter.cm-gutterElement': { opacity: '1' },
    '.cm-cursor, .cm-dropCursor': { borderLeft: `2px solid ${v('amber')}` },
    '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': {
        backgroundColor: `color-mix(in srgb, ${v('amber')} 24%, transparent) !important`,
    },
    '.cm-selectionMatch': { backgroundColor: `color-mix(in srgb, ${v('teal')} 18%, transparent)` },
    '&.cm-focused .cm-matchingBracket': {
        color: v('honey'),
        backgroundColor: `color-mix(in srgb, ${v('amber')} 16%, transparent)`,
        outline: `1px solid color-mix(in srgb, ${v('amber')} 45%, transparent)`,
        borderRadius: '2px',
    },
    '&.cm-focused .cm-nonmatchingBracket': { color: v('red') },
    '.cm-foldGutter .cm-gutterElement': { padding: '0 0.3rem', cursor: 'pointer', color: v('ink-2') },
    '.cm-foldPlaceholder': {
        backgroundColor: `color-mix(in srgb, ${v('amber')} 14%, transparent)`,
        border: `1px solid ${v('line-strong')}`,
        color: v('honey'),
        padding: '0 0.4rem',
        borderRadius: '4px',
    },
    '.cm-tooltip': {
        backgroundColor: v('bg-1'),
        color: v('ink'),
        border: `1px solid ${v('line-strong')}`,
        borderRadius: '10px',
        boxShadow: 'var(--shadow-soft)',
        overflow: 'hidden',
    },
    '.cm-tooltip-autocomplete > ul': { fontFamily: 'var(--font-mono)', fontSize: '0.8rem', maxHeight: '14rem' },
    '.cm-tooltip-autocomplete > ul > li': { padding: '0.15rem 0.6rem !important' },
    '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
        backgroundColor: `color-mix(in srgb, ${v('amber')} 22%, transparent)`,
        color: v('ink'),
    },
    '.cm-completionIcon': { color: v('ink-2') },
    '.cm-completionMatchedText': { textDecoration: 'none', color: v('honey'), fontWeight: '600' },
    '.cm-tooltip-lint': { padding: '0' },
    '.cm-diagnostic': { fontFamily: 'var(--font-mono)', fontSize: '0.78rem', padding: '0.45rem 0.7rem', maxWidth: '32rem' },
    '.cm-diagnostic-error': { borderLeft: `3px solid ${v('red')}` },
    '.cm-lintRange-error': {
        backgroundImage: 'none',
        textDecoration: `underline wavy ${v('red')}`,
        textDecorationSkipInk: 'none',
        textUnderlineOffset: '4px',
    },
    '.cm-gutter-lint': { width: '1.1rem' },
    '.cm-gutter-lint .cm-gutterElement': { padding: '0 0 0 0.35rem', display: 'flex', alignItems: 'center' },
    '.cm-lint-marker': { width: '0.6rem', height: '0.6rem', content: 'none' },
    '.cm-lint-marker-error': {
        content: 'none',
        backgroundColor: v('red'),
        borderRadius: '50%',
        boxShadow: `0 0 6px ${v('red')}`,
    },
    '.cm-errorLine': {
        backgroundColor: `color-mix(in srgb, ${v('red')} 12%, transparent)`,
        boxShadow: `inset 2px 0 0 ${v('red')}`,
    },
    '.cm-panels': { backgroundColor: v('bg-1'), color: v('ink'), borderColor: v('line') },
    '.cm-panels.cm-panels-top': { borderBottom: `1px solid ${v('line')}` },
    '.cm-panels.cm-panels-bottom': { borderTop: `1px solid ${v('line')}` },
    '.cm-panel.cm-search': { fontFamily: 'var(--font-sans)', fontSize: '0.8rem', padding: '0.4rem 0.6rem' },
    '.cm-panel.cm-search input, .cm-panel.cm-search button': {
        borderRadius: '6px',
        border: `1px solid ${v('line-strong')}`,
        backgroundColor: v('bg'),
        backgroundImage: 'none',
        color: v('ink'),
        fontSize: '0.78rem',
    },
    '.cm-searchMatch': { backgroundColor: `color-mix(in srgb, ${v('honey')} 22%, transparent)`, outline: 'none' },
    '.cm-searchMatch-selected': { backgroundColor: `color-mix(in srgb, ${v('ember')} 32%, transparent)` },
    '.cm-pyBuiltin': { color: v('teal') },
    '.cm-pySelf': { color: v('amber'), fontStyle: 'italic' },
});

const pythonStyle = HighlightStyle.define([
    { tag: [t.keyword, t.controlKeyword, t.operatorKeyword, t.definitionKeyword, t.moduleKeyword], color: v('ember') },
    { tag: t.modifier, color: v('ember') },
    { tag: [t.function(t.definition(t.variableName)), t.definition(t.className)], color: v('honey'), fontWeight: '600' },
    { tag: [t.function(t.variableName), t.function(t.propertyName)], color: v('honey') },
    { tag: [t.string, t.special(t.string)], color: v('green') },
    { tag: t.escape, color: v('teal') },
    { tag: [t.number, t.bool, t.null], color: v('teal') },
    { tag: [t.lineComment, t.comment], color: v('ink-2'), fontStyle: 'italic' },
    { tag: t.meta, color: v('amber') },
    { tag: [t.arithmeticOperator, t.bitwiseOperator, t.compareOperator, t.updateOperator, t.definitionOperator], color: v('amber') },
    { tag: [t.paren, t.squareBracket, t.brace, t.separator, t.derefOperator, t.punctuation], color: v('ink-1') },
    { tag: t.propertyName, color: v('ink') },
    { tag: t.variableName, color: v('ink') },
    { tag: t.invalid, color: v('red') },
]);

const watStyle = HighlightStyle.define([
    { tag: t.keyword, color: v('ember') },
    { tag: t.atom, color: v('teal') },
    { tag: t.variableName, color: v('honey') },
    { tag: t.string, color: v('green') },
    { tag: t.number, color: v('teal') },
    { tag: t.comment, color: v('ink-2'), fontStyle: 'italic' },
]);

// CodeMirror's own dark flag, flipped live with the site theme.
function darkFlag() {
    const compartment = new Compartment();
    const extension = compartment.of(EditorView.darkTheme.of(isDark()));
    const follow = (view) => {
        const sync = () => view.dispatch({ effects: compartment.reconfigure(EditorView.darkTheme.of(isDark())) });
        document.addEventListener('themechange', sync);
    };
    return { extension, follow };
}

/* ---------- Python extras ---------- */

// Builtins and `self`, which the Python grammar tags as plain names.
const BUILTINS = new Set([
    'abs', 'all', 'any', 'bool', 'bytes', 'chr', 'dict', 'divmod', 'enumerate', 'filter', 'float', 'format',
    'frozenset', 'getattr', 'hasattr', 'hash', 'hex', 'id', 'input', 'int', 'isinstance', 'issubclass', 'iter',
    'len', 'list', 'map', 'max', 'min', 'next', 'object', 'oct', 'open', 'ord', 'pow', 'print', 'range', 'repr',
    'reversed', 'round', 'set', 'setattr', 'sorted', 'str', 'sum', 'super', 'tuple', 'type', 'zip',
    'Exception', 'ValueError', 'TypeError', 'KeyError', 'IndexError', 'ZeroDivisionError', 'RuntimeError',
    'AssertionError', 'StopIteration', 'NotImplementedError', 'AttributeError', 'OSError', 'ArithmeticError',
    'LookupError', 'OverflowError', 'FileNotFoundError',
]);
const builtinMark = Decoration.mark({ class: 'cm-pyBuiltin' });
const selfMark = Decoration.mark({ class: 'cm-pySelf' });

function builtinDecorations(view) {
    const builder = new RangeSetBuilder();
    const tree = syntaxTree(view.state);
    for (const { from, to } of view.visibleRanges) {
        tree.iterate({
            from,
            to,
            enter(node) {
                if (node.name !== 'VariableName') return;
                const parent = node.node.parent?.name;
                if (parent === 'FunctionDefinition' || parent === 'ClassDefinition') return;
                const text = view.state.doc.sliceString(node.from, node.to);
                if (text === 'self' || text === 'cls') builder.add(node.from, node.to, selfMark);
                else if (BUILTINS.has(text)) builder.add(node.from, node.to, builtinMark);
            },
        });
    }
    return builder.finish();
}

const pythonNames = ViewPlugin.fromClass(
    class {
        constructor(view) {
            this.decorations = builtinDecorations(view);
        }
        update(update) {
            if (update.docChanged || update.viewportChanged || syntaxTree(update.startState) !== syntaxTree(update.state)) {
                this.decorations = builtinDecorations(update.view);
            }
        }
    },
    { decorations: (p) => p.decorations },
);

// A red background on each line a compile error points at, alongside the
// lint underline and gutter dot.
const setErrorLines = StateEffect.define();
const errorLineMark = Decoration.line({ class: 'cm-errorLine' });
const errorLines = StateField.define({
    create: () => Decoration.none,
    update(deco, tr) {
        deco = tr.docChanged ? Decoration.none : deco;
        for (const e of tr.effects) {
            if (e.is(setErrorLines)) {
                const builder = new RangeSetBuilder();
                for (const pos of [...new Set(e.value)].sort((a, b) => a - b)) builder.add(pos, pos, errorLineMark);
                deco = builder.finish();
            }
        }
        return deco;
    },
    provide: (f) => EditorView.decorations.from(f),
});

// Errors point at the program as it was compiled, so an edit retires them
// (the diagnostics in the same transaction as the edit).
const clearErrorsOnEdit = EditorState.transactionExtender.of((tr) => {
    if (!tr.docChanged || tr.effects.some((e) => e.is(setDiagnosticsEffect))) return null;
    return { effects: setDiagnosticsEffect.of([]) };
});

// Tab indents a selection, or inserts spaces to the next indent stop.
function insertIndent(view) {
    const { state } = view;
    if (state.selection.ranges.some((r) => !r.empty)) return indentMore(view);
    const unit = state.facet(indentUnit).length;
    view.dispatch(
        state.changeByRange((range) => {
            const line = state.doc.lineAt(range.head);
            const spaces = ' '.repeat(unit - ((range.head - line.from) % unit));
            return { changes: { from: range.head, insert: spaces }, range: EditorSelection.cursor(range.head + spaces.length) };
        }),
        { scrollIntoView: true, userEvent: 'input' },
    );
    return true;
}

/* ---------- Editors ---------- */

/**
 * The Python source editor.
 *
 * @param {HTMLElement} parent
 * @param {{ doc: string, onChange: () => void, onCursor: (line: number, col: number) => void, onRun: () => void }} hooks
 */
export function createPythonEditor(parent, { doc, onChange, onCursor, onRun }) {
    const dark = darkFlag();
    const extensions = [
        lineNumbers(),
        lintGutter(),
        foldGutter({
            markerDOM(open) {
                const el = document.createElement('span');
                el.textContent = open ? '▾' : '▸';
                el.style.opacity = open ? '0.55' : '1';
                return el;
            },
        }),
        highlightActiveLineGutter(),
        highlightSpecialChars(),
        history(),
        drawSelection(),
        dropCursor(),
        EditorState.allowMultipleSelections.of(true),
        indentOnInput(),
        indentUnit.of('    '),
        EditorState.tabSize.of(4),
        python(),
        syntaxHighlighting(pythonStyle),
        pythonNames,
        bracketMatching(),
        closeBrackets(),
        autocompletion({ icons: false }),
        rectangularSelection(),
        crosshairCursor(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        errorLines,
        clearErrorsOnEdit,
        keymap.of([
            { key: 'Mod-Enter', run: () => (onRun(), true), preventDefault: true },
            { key: 'Tab', run: insertIndent, shift: indentLess },
            ...closeBracketsKeymap,
            ...defaultKeymap,
            ...searchKeymap,
            ...historyKeymap,
            ...foldKeymap,
            ...completionKeymap,
        ]),
        baseTheme,
        dark.extension,
        EditorView.contentAttributes.of({ 'aria-label': 'Python source', spellcheck: 'false', autocorrect: 'off', autocapitalize: 'off' }),
        EditorView.updateListener.of((update) => {
            if (update.docChanged) onChange();
            if (update.selectionSet || update.docChanged) {
                const head = update.state.selection.main.head;
                const line = update.state.doc.lineAt(head);
                onCursor(line.number, head - line.from + 1);
            }
        }),
    ];

    const makeState = (text) => EditorState.create({ doc: text, extensions });
    const view = new EditorView({ state: makeState(doc), parent });
    dark.follow(view);

    let hasErrors = false;

    return {
        view,
        getValue: () => view.state.doc.toString(),
        lineCount: () => view.state.doc.lines,
        /** Replace the whole program; `fresh` also starts a new undo history. */
        setValue(text, { fresh = false } = {}) {
            if (fresh) {
                view.setState(makeState(text));
                hasErrors = false;
                onChange();
                onCursor(1, 1);
            } else {
                view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
            }
        },
        /** Move the cursor to a 0-based line and column, scrolled to the middle. */
        jumpTo(line, ch = 0) {
            const l = view.state.doc.line(Math.min(line + 1, view.state.doc.lines));
            const pos = Math.min(l.from + ch, l.to);
            view.focus();
            view.dispatch({ selection: { anchor: pos }, effects: EditorView.scrollIntoView(pos, { y: 'center' }) });
        },
        /** Show compile errors: `{ line, ch, message }` with 0-based line and column. */
        setErrors(errors) {
            const doc = view.state.doc;
            const diagnostics = [];
            const lines = [];
            for (const e of errors) {
                if (e.line < 0 || e.line >= doc.lines) continue;
                const l = doc.line(e.line + 1);
                const indent = l.text.length - l.text.trimStart().length;
                const from = l.from + Math.min(Math.max(e.ch, indent), l.length);
                diagnostics.push({ from, to: Math.max(from, l.to), severity: 'error', source: 'waspy', message: e.message });
                lines.push(l.from);
            }
            hasErrors = diagnostics.length > 0;
            view.dispatch(setDiagnostics(view.state, diagnostics), { effects: setErrorLines.of(lines) });
        },
        clearErrors() {
            if (!hasErrors) return;
            hasErrors = false;
            view.dispatch(setDiagnostics(view.state, []), { effects: setErrorLines.of([]) });
        },
        focus: () => view.focus(),
    };
}

/**
 * The read-only WebAssembly text viewer.
 *
 * @param {HTMLElement} parent
 */
export function createWatViewer(parent) {
    const dark = darkFlag();
    const view = new EditorView({
        parent,
        state: EditorState.create({
            doc: '',
            extensions: [
                lineNumbers(),
                foldGutter({
                    markerDOM(open) {
                        const el = document.createElement('span');
                        el.textContent = open ? '▾' : '▸';
                        el.style.opacity = open ? '0.55' : '1';
                        return el;
                    },
                }),
                highlightSpecialChars(),
                drawSelection(),
                highlightActiveLineGutter(),
                StreamLanguage.define(wast),
                syntaxHighlighting(watStyle),
                highlightSelectionMatches(),
                EditorState.readOnly.of(true),
                keymap.of([...defaultKeymap, ...searchKeymap, ...foldKeymap]),
                baseTheme,
                EditorView.theme({ '.cm-cursor': { display: 'none !important' } }),
                dark.extension,
                EditorView.contentAttributes.of({ 'aria-label': 'WebAssembly text (read only)' }),
            ],
        }),
    });
    dark.follow(view);

    return {
        view,
        /** Replace the text, keeping the scroll position where it was. */
        setValue(text) {
            const { scrollTop, scrollLeft } = view.scrollDOM;
            view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
            view.requestMeasure({
                read: () => null,
                write: () => {
                    view.scrollDOM.scrollTop = scrollTop;
                    view.scrollDOM.scrollLeft = scrollLeft;
                },
            });
        },
        refresh: () => view.requestMeasure(),
    };
}
