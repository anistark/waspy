// The development board (/modules/): cards that open and close, the stats
// counting up, and the board exported as a PNG. The board itself is
// rendered at build time from src/_data/modules.json.

import { toPng } from "html-to-image";

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

// Cards: a button in each header opens its feature list. A closed list is
// inert, so its links are out of the tab order and hidden from screen readers.
function setOpen(card, open) {
    const toggle = card.querySelector("[data-card-toggle]");
    const body = card.querySelector("[data-card-body]");
    card.toggleAttribute("data-open", open);
    toggle?.setAttribute("aria-expanded", String(open));
    if (body) body.inert = !open;
}

// One listener on the document rather than one per button, so the toggles
// keep working when the page's HTML is replaced in place (the dev server's
// live reload patches the DOM without running this script again).
function initCards() {
    document.querySelectorAll("[data-card]").forEach((card) => setOpen(card, false));
    document.addEventListener("click", (event) => {
        const toggle = event.target.closest?.("[data-card-toggle]");
        const card = toggle?.closest("[data-card]");
        if (card) setOpen(card, !card.hasAttribute("data-open"));
    });
}

// Stats count up from zero once, as the page loads.
function initCounters() {
    if (reducedMotion()) return;
    const duration = 1000;
    document.querySelectorAll("[data-count]").forEach((element) => {
        const target = Number(element.dataset.count);
        const suffix = element.dataset.suffix ?? "";
        const start = performance.now();
        const step = (now) => {
            const t = Math.min((now - start) / duration, 1);
            const eased = 1 - Math.pow(1 - t, 3);
            element.textContent = `${Math.round(target * eased)}${suffix}`;
            if (t < 1) requestAnimationFrame(step);
        };
        element.textContent = `0${suffix}`;
        requestAnimationFrame(step);
    });
}

// The PNG export. The board is copied off screen, laid out wide with every
// card open (the .board-snapshot rules in the page), drawn in the current
// theme's background, and removed again, so the page itself never moves.
const SNAPSHOT_WIDTH = 2000;
// Safari refuses canvases over about 16.7 million pixels; stay under it.
const MAX_PIXELS = 16_000_000;

function snapshotCopy(root) {
    const copy = root.cloneNode(true);
    copy.classList.add("board-snapshot");
    copy.querySelectorAll("[data-export-exclude]").forEach((element) => element.remove());
    // Ids stay unique, except the icon sprite's, which the copy's icons use.
    copy.querySelectorAll("[id]:not(symbol)").forEach((element) => element.removeAttribute("id"));
    copy.querySelectorAll("[data-card]").forEach((card) => card.setAttribute("data-open", ""));
    copy.querySelectorAll("[data-card-body]").forEach((body) => (body.inert = false));
    copy.querySelectorAll(".reveal").forEach((element) => element.classList.add("in-view"));
    // The live counters may be mid-animation; the copy shows the final figures.
    copy.querySelectorAll("[data-count]").forEach((element) => {
        element.textContent = `${element.dataset.count}${element.dataset.suffix ?? ""}`;
    });
    const stage = document.createElement("div");
    stage.setAttribute("aria-hidden", "true");
    stage.style.cssText = `position:fixed;top:0;left:${-SNAPSHOT_WIDTH - 200}px;width:${SNAPSHOT_WIDTH}px;pointer-events:none;`;
    stage.append(copy);
    document.body.append(stage);
    return { copy, stage };
}

// Delegated like the cards, and the elements looked up on each click.
async function exportBoard(button) {
    const root = document.querySelector("[data-board-export]");
    if (!root) return;
    const label = button.querySelector("[data-export-label]");
    const status = document.querySelector("[data-export-status]");
    const original = label.dataset.original ?? (label.dataset.original = label.textContent);

    const finish = (text, announcement) => {
        label.textContent = text;
        if (status) status.textContent = announcement;
        setTimeout(() => {
            label.textContent = original;
            button.disabled = false;
        }, 1800);
    };

    button.disabled = true;
    label.textContent = "Preparing image";
    if (status) status.textContent = "Preparing the board image";
    const theme = document.documentElement.getAttribute("data-theme") === "light" ? "light" : "dark";
    const background = getComputedStyle(document.documentElement).getPropertyValue("--c-bg").trim();
    const { copy, stage } = snapshotCopy(root);
    try {
        await document.fonts?.ready;
        // Let the copy lay out (a timer, not a frame: frames stall in a background tab).
        await new Promise((resolve) => setTimeout(resolve, 50));
        const width = copy.offsetWidth;
        const height = copy.offsetHeight;
        const pixelRatio = Math.min(1.5, Math.sqrt(MAX_PIXELS / (width * height)));
        const url = await toPng(copy, { backgroundColor: background, width, height, pixelRatio, cacheBust: false });
        const link = document.createElement("a");
        link.download = `waspy-board-${new Date().toISOString().slice(0, 10)}-${theme}.png`;
        link.href = url;
        document.body.append(link);
        link.click();
        link.remove();
        finish("Exported", "The board was exported as a PNG image");
    } catch (error) {
        console.error("Board export failed:", error);
        finish("Export failed", "The board image could not be created");
    } finally {
        stage.remove();
    }
}

function initExport() {
    document.addEventListener("click", (event) => {
        const button = event.target.closest?.("[data-export-png]");
        if (button && !button.disabled) exportBoard(button);
    });
}

function init() {
    initCards();
    initCounters();
    initExport();
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
else init();
