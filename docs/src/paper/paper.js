// The paper page: the contents list marks the section in view, a thin bar
// shows reading progress, and the print button opens the print dialog
// (the page's print styles lay it out as a paper).

function initToc() {
    const links = [...document.querySelectorAll("[data-paper-toc] a")];
    const targets = links
        .map((a) => document.getElementById(decodeURIComponent(a.hash.slice(1))))
        .filter(Boolean);
    if (!targets.length || !("IntersectionObserver" in window)) return;

    const visible = new Set();
    const mark = () => {
        // The first section on screen, or failing that the last one passed.
        let current = targets.find((t) => visible.has(t));
        if (!current) {
            const passed = targets.filter((t) => t.getBoundingClientRect().top < 120);
            current = passed[passed.length - 1];
        }
        links.forEach((a) => a.setAttribute("aria-current", String(!!current && a.hash === "#" + current.id)));
    };
    const observer = new IntersectionObserver(
        (entries) => {
            entries.forEach((e) => (e.isIntersecting ? visible.add(e.target) : visible.delete(e.target)));
            mark();
        },
        { rootMargin: "-90px 0px -60% 0px" },
    );
    targets.forEach((t) => observer.observe(t));
}

function initProgress() {
    const bar = document.querySelector("[data-paper-progress]");
    const article = document.querySelector(".paper");
    if (!bar || !article) return;
    let queued = false;
    const update = () => {
        queued = false;
        const rect = article.getBoundingClientRect();
        const total = rect.height - window.innerHeight;
        const read = total > 0 ? Math.min(1, Math.max(0, -rect.top / total)) : 1;
        bar.style.setProperty("--progress", read.toFixed(4));
    };
    const schedule = () => {
        if (!queued) {
            queued = true;
            requestAnimationFrame(update);
        }
    };
    update();
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
}

function initPrint() {
    document.querySelectorAll("[data-paper-print]").forEach((button) => {
        if (typeof window.print !== "function") return;
        button.hidden = false;
        button.addEventListener("click", () => window.print());
    });
}

initToc();
initProgress();
initPrint();
