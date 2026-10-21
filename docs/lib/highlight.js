// Build-time syntax highlighting with Prism, for markdown fences and the
// `highlight` paired shortcode in Nunjucks templates.
import Prism from "prismjs";
import loadLanguages from "prismjs/components/index.js";

loadLanguages(["python", "rust", "toml", "bash", "json", "wasm"]);

const ALIASES = { sh: "bash", shell: "bash", js: "javascript", py: "python", rs: "rust", wat: "wasm" };

export function highlight(code, lang) {
    const name = ALIASES[lang] || lang || "none";
    const grammar = Prism.languages[name];
    const body = grammar ? Prism.highlight(code, grammar, name) : escapeHtml(code);
    return `<pre class="language-${name}" tabindex="0"><code class="language-${name}">${body}</code></pre>`;
}

function escapeHtml(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
