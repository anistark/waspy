// CHANGELOG.md, rendered at build time for /docs/changelog/. The site is
// rebuilt on every push to main, so the page always matches the file.
import { readFileSync } from "node:fs";
import { createMarkdown } from "../../lib/markdown.js";
import { highlight } from "../../lib/highlight.js";

const REPO = "https://github.com/anistark/waspy";

export default function () {
    const source = readFileSync(new URL("../../../CHANGELOG.md", import.meta.url), "utf8");

    // Drop the "# Changelog" title and preamble; the page has its own.
    const body = source.slice(source.indexOf("\n## ") + 1);

    const releases = [];
    const markdown = body
        .replace(/^## \[Unreleased\][ \t]*$/m, () => {
            releases.push({ id: "upcoming", version: "Upcoming", date: null });
            return '## Upcoming {#upcoming}\n\n<p class="release-meta">Merged on main, not yet released</p>';
        })
        .replace(/^## \[([^\]]+)\]\(([^)]+)\)[ \t]*-[ \t]*(\S+)[ \t]*$/gm, (_, version, url, date) => {
            const id = "v" + version.replace(/\./g, "-");
            releases.push({ id, version, date, url });
            return `## ${version} {#${id}}\n\n<p class="release-meta">Released ${date} · <a href="${url}">Release on GitHub</a></p>`;
        })
        // Relative links in the file point into the repository.
        .replace(/\]\((?!https?:|#|mailto:)\.?\/?([^)]+)\)/g, `](${REPO}/blob/main/$1)`);

    return {
        html: createMarkdown({ highlight }).render(markdown),
        releases,
    };
}
