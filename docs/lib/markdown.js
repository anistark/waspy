// The one markdown-it instance the site renders with: Eleventy's .md
// templates and the changelog page (which renders CHANGELOG.md at build
// time) both use it, so they share anchors and code highlighting.
import markdownIt from "markdown-it";
import markdownItAnchor from "markdown-it-anchor";
import markdownItAttrs from "markdown-it-attrs";

export function slugify(text) {
    return String(text)
        .toLowerCase()
        .replace(/<[^>]+>/g, "")
        .replace(/[`'"]/g, "")
        .replace(/[^\p{L}\p{N}]+/gu, "-")
        .replace(/^-+|-+$/g, "");
}

export function createMarkdown({ highlight } = {}) {
    const md = markdownIt({ html: true, linkify: false, typographer: false, highlight });
    // Attributes first, so a heading's `{#id}` is kept by the anchor plugin.
    md.use(markdownItAttrs, { allowedAttributes: ["id", "class"] });
    md.use(markdownItAnchor, {
        slugify,
        level: [2, 3],
        permalink: markdownItAnchor.permalink.ariaHidden({
            placement: "after",
            class: "header-anchor",
            symbol: "#",
        }),
    });
    return md;
}
