// The waspy website: Eleventy renders src/ into _site/, Tailwind CSS v4
// (through PostCSS) compiles src/assets/css/main.css, and esbuild bundles
// the browser scripts (the playground and its CodeMirror editor included).
import { readFile } from "node:fs/promises";
import path from "node:path";
import { HtmlBasePlugin } from "@11ty/eleventy";
import postcss from "postcss";
import tailwindcss from "@tailwindcss/postcss";
import * as esbuild from "esbuild";
import { createMarkdown } from "./lib/markdown.js";
import { highlight } from "./lib/highlight.js";

// Browser entry points, bundled by esbuild: output path (under _site/,
// without .js) to source. A page loads its own with `scripts:` front matter.
const SCRIPTS = {
    "assets/js/main": "src/assets/js/main.js",
    "assets/js/home": "src/assets/js/home.js",
    "playground/playground": "src/playground/playground.js",
    "modules/modules": "src/modules/modules.js",
    "paper/paper": "src/paper/paper.js",
};

export default function (eleventyConfig) {
    eleventyConfig.addPlugin(HtmlBasePlugin);

    // Markdown, shared with the changelog page.
    eleventyConfig.setLibrary("md", createMarkdown({ highlight }));
    eleventyConfig.addPairedShortcode("highlight", (code, lang) => highlight(code.replace(/^\n+|\s+$/g, ""), lang));

    // Static files, served as they are.
    eleventyConfig.addPassthroughCopy({ "src/assets/img": "assets/img" });
    eleventyConfig.addPassthroughCopy({ "src/paper/waspy.pdf": "paper/waspy.pdf" });
    eleventyConfig.addPassthroughCopy({ "src/playground/compiler": "playground/compiler" });
    eleventyConfig.addPassthroughCopy({
        "node_modules/@fontsource-variable/bricolage-grotesque/files/bricolage-grotesque-latin-opsz-normal.woff2": "assets/fonts/bricolage-grotesque.woff2",
        "node_modules/@fontsource-variable/instrument-sans/files/instrument-sans-latin-wght-normal.woff2": "assets/fonts/instrument-sans.woff2",
        "node_modules/@fontsource-variable/instrument-sans/files/instrument-sans-latin-wght-italic.woff2": "assets/fonts/instrument-sans-italic.woff2",
        "node_modules/@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-wght-normal.woff2": "assets/fonts/jetbrains-mono.woff2",
    });

    // CSS: only main.css is an entry point; the rest is imported into it.
    eleventyConfig.addTemplateFormats("css");
    eleventyConfig.addExtension("css", {
        outputFileExtension: "css",
        compile: async (input, inputPath) => {
            if (path.basename(inputPath) !== "main.css") return;
            return async () => {
                // Minified for a build; readable while serving.
                const minify = process.env.ELEVENTY_RUN_MODE === "build";
                const result = await postcss([tailwindcss({ optimize: { minify } })]).process(input, { from: inputPath });
                return result.css;
            };
        },
    });
    eleventyConfig.addWatchTarget("src/**/*.{js,css}");
    // Data read from outside src/: the changelog page and the crate version.
    eleventyConfig.addWatchTarget("../CHANGELOG.md");
    eleventyConfig.addWatchTarget("../Cargo.toml");

    // JavaScript: bundled by esbuild before each build. The playground
    // compiler (src/playground/compiler/) stays a separate file it fetches.
    eleventyConfig.on("eleventy.before", async ({ directories, runMode }) => {
        const entryPoints = {};
        for (const [out, src] of Object.entries(SCRIPTS)) {
            try {
                await readFile(src);
                entryPoints[out] = src;
            } catch {
                // A page whose script does not exist yet builds without it.
            }
        }
        await esbuild.build({
            entryPoints,
            outdir: directories.output,
            bundle: true,
            format: "esm",
            target: "es2022",
            splitting: false,
            minify: runMode === "build",
            sourcemap: runMode === "build" ? false : "inline",
            external: ["./compiler/*"],
            logLevel: "warning",
        });
    });

    // Filters.
    eleventyConfig.addFilter("toc", tocFromHtml);
    eleventyConfig.addFilter("startsWith", (s, prefix) => String(s || "").startsWith(prefix));
    eleventyConfig.addFilter("year", () => new Date().getFullYear());

    return {
        dir: { input: "src", output: "_site", includes: "_includes", data: "_data" },
        templateFormats: ["njk", "md", "html", "css"],
        markdownTemplateEngine: "njk",
        htmlTemplateEngine: "njk",
        pathPrefix: "/waspy/",
    };
}

// The h2 (and, at depth 3, h3) headings of rendered HTML that carry an id,
// for "On this page".
function tocFromHtml(html, depth = 3) {
    const items = [];
    const levels = depth >= 3 ? "23" : "2";
    const pattern = new RegExp(`<h([${levels}])[^>]*\\sid="([^"]+)"[^>]*>([\\s\\S]*?)<\\/h\\1>`, "g");
    for (const m of String(html || "").matchAll(pattern)) {
        const text = m[3]
            .replace(/<a[^>]*class="header-anchor"[^>]*>[\s\S]*?<\/a>/g, "")
            .replace(/<[^>]+>/g, "")
            .trim();
        items.push({ level: Number(m[1]), id: m[2], text });
    }
    return items;
}
