// The home page: the architecture flowchart's tooltips and its one-shot
// pulse, and the contributors list fetched from the GitHub API.

const REPO_API = "https://api.github.com/repos/anistark/waspy/contributors?per_page=100";

// --------------------------------------------------------------------------
// Architecture flowchart
//
// Every listener is on the document and finds the chart's elements when an
// event arrives, so the tooltips keep working when the page's HTML is
// replaced in place (the dev server's live reload patches the DOM without
// running this script again).
// --------------------------------------------------------------------------

let activeNode = null;

function chart() {
    const wrapper = document.querySelector("[data-flowchart]");
    const tooltip = wrapper?.querySelector("#fc-tooltip");
    return wrapper && tooltip ? { wrapper, tooltip } : null;
}

const nodeFrom = (target) => target?.closest?.("[data-flowchart] .fc-node") ?? null;

function placeAtPointer({ wrapper, tooltip }, event) {
    // Relative to the wrapper; keep the tooltip inside it.
    const box = wrapper.getBoundingClientRect();
    const x = event.clientX - box.left;
    const y = event.clientY - box.top;
    const w = tooltip.offsetWidth;
    const h = tooltip.offsetHeight;
    let left = x + 16;
    let top = y + 16;
    if (left + w > box.width - 8) left = Math.max(8, x - w - 16);
    if (top + h > box.height - 8) top = Math.max(8, y - h - 16);
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${top}px`;
}

function placeAtNode({ wrapper, tooltip }, node) {
    const box = wrapper.getBoundingClientRect();
    const r = node.getBoundingClientRect();
    const w = tooltip.offsetWidth;
    let left = r.left - box.left + r.width / 2 - w / 2;
    left = Math.min(Math.max(8, left), box.width - w - 8);
    let top = r.bottom - box.top + 10;
    if (top + tooltip.offsetHeight > box.height - 8) top = r.top - box.top - tooltip.offsetHeight - 10;
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${Math.max(8, top)}px`;
}

function show(parts, node) {
    if (activeNode && activeNode !== node) activeNode.classList.remove("is-active");
    activeNode = node;
    node.classList.add("is-active");
    node.setAttribute("aria-describedby", "fc-tooltip");
    parts.tooltip.querySelector("[data-tooltip-label]").textContent = `Step ${node.dataset.step}: ${node.dataset.name}`;
    parts.tooltip.querySelector("[data-tooltip-text]").textContent = node.dataset.info;
    parts.tooltip.classList.remove("hidden");
    parts.tooltip.setAttribute("data-show", "");
}

function hide() {
    if (activeNode) {
        activeNode.classList.remove("is-active");
        activeNode.removeAttribute("aria-describedby");
    }
    activeNode = null;
    const parts = chart();
    if (!parts) return;
    parts.tooltip.classList.add("hidden");
    parts.tooltip.removeAttribute("data-show");
}

const isShowing = (node) => activeNode === node && !chart()?.tooltip.classList.contains("hidden");

function initFlowchart() {
    // Hover: pointerover/pointerout bubble, unlike pointerenter/leave.
    document.addEventListener("pointerover", (event) => {
        const node = nodeFrom(event.target);
        const parts = chart();
        if (!node || !parts || event.pointerType === "touch" || node.contains(event.relatedTarget)) return;
        show(parts, node);
        placeAtPointer(parts, event);
    });
    document.addEventListener("pointermove", (event) => {
        const node = nodeFrom(event.target);
        const parts = chart();
        if (node && parts && event.pointerType !== "touch" && activeNode === node) placeAtPointer(parts, event);
    });
    document.addEventListener("pointerout", (event) => {
        const node = nodeFrom(event.target);
        if (!node || event.pointerType === "touch" || node.contains(event.relatedTarget)) return;
        if (document.activeElement !== node) hide();
    });

    // Keyboard focus anchors the tooltip under the node; a tap that focuses
    // it is handled by the click below.
    document.addEventListener("focusin", (event) => {
        const node = nodeFrom(event.target);
        const parts = chart();
        if (!node || !parts || !node.matches(":focus-visible")) return;
        show(parts, node);
        placeAtNode(parts, node);
    });
    document.addEventListener("focusout", (event) => {
        const node = nodeFrom(event.target);
        if (node && activeNode === node) hide();
    });

    // A tap toggles the tooltip; a mouse click keeps it open; a click
    // anywhere else closes it.
    document.addEventListener("click", (event) => {
        const node = nodeFrom(event.target);
        const parts = chart();
        if (!node || !parts) {
            hide();
            return;
        }
        if (event.pointerType === "touch" && isShowing(node)) {
            hide();
            return;
        }
        show(parts, node);
        placeAtPointer(parts, event);
    });

    document.addEventListener("keydown", (event) => {
        if (event.key === "Escape") {
            hide();
            return;
        }
        const node = nodeFrom(event.target);
        const parts = chart();
        if (!node || !parts || (event.key !== "Enter" && event.key !== " ")) return;
        event.preventDefault();
        if (isShowing(node)) hide();
        else {
            show(parts, node);
            placeAtNode(parts, node);
        }
    });

    // Run the pulse once, the first time the chart is on screen. It is
    // decorative, so a chart patched in by a live reload just goes without.
    const svg = document.querySelector("[data-flowchart] svg");
    const motion = svg?.querySelector("#fc-motion");
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (!motion || reduced || typeof motion.beginElement !== "function" || !("IntersectionObserver" in window)) return;
    const observer = new IntersectionObserver(
        (entries) => {
            if (entries.some((e) => e.isIntersecting)) {
                observer.disconnect();
                motion.beginElement();
            }
        },
        { threshold: 0.35 },
    );
    observer.observe(svg);
}

// --------------------------------------------------------------------------
// Contributors
// --------------------------------------------------------------------------

function isBot(person) {
    return person.type === "Bot" || /\[bot\]$/.test(person.login || "");
}

function contributorCard(person) {
    const a = document.createElement("a");
    a.href = person.html_url;
    a.target = "_blank";
    a.rel = "noopener";
    a.title = `${person.login}: ${person.contributions} commits`;
    a.className =
        "group flex w-24 flex-col items-center gap-2 rounded-2xl p-3 text-center transition hover:-translate-y-0.5 hover:bg-surface sm:w-32";

    const img = document.createElement("img");
    img.src = `${person.avatar_url}${person.avatar_url.includes("?") ? "&" : "?"}s=140`;
    img.alt = "";
    img.loading = "lazy";
    img.width = 64;
    img.height = 64;
    img.className = "size-14 rounded-full bg-bg-2 ring-2 ring-line transition group-hover:ring-amber/70 sm:size-16";

    const name = document.createElement("span");
    name.className = "w-full truncate text-sm font-semibold text-ink group-hover:text-honey";
    name.textContent = person.login;

    const count = document.createElement("span");
    count.className = "font-mono text-[0.68rem] text-ink-2";
    count.textContent = `${person.contributions} ${person.contributions === 1 ? "commit" : "commits"}`;

    a.append(img, name, count);
    return a;
}

function showContributorsFallback(grid) {
    const p = document.createElement("p");
    p.className = "self-center text-sm text-ink-2";
    p.append("Could not load the list right now. ");
    const link = document.createElement("a");
    link.href = grid.dataset.fallback;
    link.className = "link";
    link.textContent = "See contributors on GitHub";
    p.append(link, ".");
    grid.replaceChildren(p);
}

// The list is fetched once. A grid still marked aria-busy is filled from
// it, which also covers a grid that a live reload put back in its
// "Loading" state.
let contributors = null;

function fetchContributors() {
    contributors ??= fetch(REPO_API, { headers: { Accept: "application/vnd.github+json" } })
        .then((res) => {
            if (!res.ok) throw new Error(String(res.status));
            return res.json();
        })
        .then((people) => (Array.isArray(people) ? people.filter((p) => !isBot(p)) : []));
    return contributors;
}

async function fillContributors() {
    const grid = document.querySelector('[data-contributors][aria-busy="true"]');
    if (!grid || grid.dataset.filling) return;
    grid.dataset.filling = "";
    try {
        const humans = await fetchContributors();
        if (!humans.length) throw new Error("no contributors");
        grid.replaceChildren(...humans.map(contributorCard));
    } catch {
        contributors = null; // try again next time
        showContributorsFallback(grid);
    } finally {
        delete grid.dataset.filling;
        grid.setAttribute("aria-busy", "false");
    }
}

function initContributors() {
    fillContributors();
    new MutationObserver(() => fillContributors()).observe(document.body, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ["aria-busy"],
    });
}

// Module scripts run after the document is parsed.
initFlowchart();
initContributors();
