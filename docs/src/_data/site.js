// Site-wide data. The crate version is read from Cargo.toml at build time,
// so install snippets always name the published version.
import { readFileSync } from "node:fs";

const cargo = readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8");
const version = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1] ?? "0.0.0";

export default {
    name: "waspy",
    title: "Waspy",
    tagline: "Python to WebAssembly, compiled ahead of time",
    description:
        "Waspy compiles a typed subset of Python ahead of time into a standalone WebAssembly module, with no interpreter in the output. Written in Rust.",
    url: "https://anistark.github.io/waspy",
    version,
    repo: "https://github.com/anistark/waspy",
    crate: "https://crates.io/crates/waspy",
    apiDocs: "https://docs.rs/waspy",
    issues: "https://github.com/anistark/waspy/issues",
    // Community chat: every "join the discussion" link points here.
    discord: "https://discord.gg/AJMEeFXxXy",
    contributing: "https://github.com/anistark/waspy/blob/main/CONTRIBUTING.md",
    editBase: "https://github.com/anistark/waspy/edit/main/docs/",

    // Primary navigation; `match` is the URL prefix that marks it active.
    nav: [
        { label: "Docs", href: "/docs/", match: "/docs/" },
        { label: "Playground", href: "/playground/", match: "/playground/" },
        { label: "Modules", href: "/modules/", match: "/modules/" },
        { label: "Paper", href: "/paper/", match: "/paper/" },
    ],

    socials: [
        { label: "GitHub", href: "https://github.com/anistark/waspy", icon: "github" },
        { label: "X (Twitter)", href: "https://x.com/kranirudha", icon: "x" },
        { label: "Mastodon", href: "https://fosstodon.org/@ani", icon: "mastodon" },
    ],
};
