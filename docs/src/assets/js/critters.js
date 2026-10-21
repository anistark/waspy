// Ambient background critters. Now and then a python (for Python) or a crab
// (Rust) walks across the page through the honeycomb background, cell to
// cell, and each hexagon it passes lights up in honey, then fades. One at a time, faint,
// behind the content, never in the way of a click. Off when the visitor
// prefers reduced motion, while the tab is hidden, and on pages whose body
// has data-critters="off".
//
// The honeycomb is body::after in main.css: a 56 by 100 tile, placed at
// -120px from the top left of the page (it scrolls with the content) and
// drifting with the honey-drift animation. Its hexagons are
// pointy-topped, 56 wide and 66 tall, in rows 50 apart, every other row
// shifted by half a cell. The critters live in the tile's coordinates, and
// their layer moves with the same drift, so they stay on the cells.

const SVG = "http://www.w3.org/2000/svg";
const LAYER_INSET = -120;
const DRIFT = { x: 56, y: 200, ms: 90_000 };
const FIRST_DELAY = [3_000, 7_000];
const NEXT_DELAY = [8_000, 20_000];
// A walk that wanders off and never reaches the far side ends anyway.
const MAX_WALK_MS = 150_000;

const rand = (min, max) => min + Math.random() * (max - min);

function svgEl(name, attrs = {}) {
    const el = document.createElementNS(SVG, name);
    for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value);
    return el;
}

// --------------------------------------------------------------------------
// The honeycomb's cells
// --------------------------------------------------------------------------

const HEX = { w: 56, row: 50, top: 33 };
// Corners of a cell around its centre (the background's own hexagon).
const CORNERS = [[0, -33], [28, -17], [28, 17], [0, 33], [-28, 17], [-28, -17]];
const NEIGHBOURS = [[56, 0], [-56, 0], [28, 50], [-28, 50], [28, -50], [-28, -50]];

const mod = (a, n) => ((a % n) + n) % n;
// Even rows have centres at x = 28 + 56i; odd rows are shifted half a cell.
const rowX = (r) => (mod(r, 2) === 1 ? 0 : 28);

function nearestCell(x, y) {
    const r0 = Math.round((y - HEX.top) / HEX.row);
    let best = null;
    let bestDistance = Infinity;
    for (let r = r0 - 1; r <= r0 + 1; r++) {
        const cy = HEX.top + r * HEX.row;
        const cx = rowX(r) + Math.round((x - rowX(r)) / HEX.w) * HEX.w;
        const d = Math.hypot(cx - x, cy - y);
        if (d < bestDistance) {
            bestDistance = d;
            best = { x: cx, y: cy };
        }
    }
    return best;
}

// The next cell on a walk heading `dir`: mostly forward, sometimes
// wandering, never straight back.
function nextCell(c, prev, dir) {
    const scored = NEIGHBOURS.map(([dx, dy]) => ({ x: c.x + dx, y: c.y + dy, dx, dy }))
        .filter((o) => !prev || o.x !== prev.x || o.y !== prev.y)
        .map((o) => {
            const along = (o.dx * dir.x + o.dy * dir.y) / Math.hypot(o.dx, o.dy);
            return { o, weight: Math.max(0.05, along + 0.55) ** 2 * rand(0.6, 1.4) };
        });
    const total = scored.reduce((sum, item) => sum + item.weight, 0);
    let r = Math.random() * total;
    for (const item of scored) {
        r -= item.weight;
        if (r <= 0) return item.o;
    }
    return scored[scored.length - 1].o;
}

// --------------------------------------------------------------------------
// The layer and the drift
// --------------------------------------------------------------------------

function driftAnimation() {
    return document
        .getAnimations?.()
        .find((a) => a.animationName === "honey-drift" && a.effect?.pseudoElement === "::after");
}

// Where the tile's origin is on the page now. Not wrapped at the end of each
// 90 s cycle: the lattice is periodic, so the unwrapped offset lines up too,
// and a critter never jumps.
function origin() {
    const t = Number(driftAnimation()?.currentTime ?? 0) / DRIFT.ms;
    return { x: LAYER_INSET + t * DRIFT.x, y: LAYER_INSET + t * DRIFT.y };
}

function createLayer() {
    const layer = document.createElement("div");
    layer.className = "critters";
    layer.setAttribute("aria-hidden", "true");
    const svg = svgEl("svg", { width: "100%", height: "100%" });
    const world = svgEl("g");
    const trail = svgEl("g", { class: "critter-trail" });
    const actors = svgEl("g");
    world.append(trail, actors);
    svg.append(world);
    layer.append(svg);
    document.body.prepend(layer);
    return { layer, world, trail, actors };
}

// A cell that lights up and fades away on its own.
function light(trail, c) {
    const points = CORNERS.map(([dx, dy]) => `${c.x + dx},${c.y + dy}`).join(" ");
    const cell = svgEl("polygon", { points });
    trail.append(cell);
    cell.addEventListener("animationend", () => cell.remove(), { once: true });
}

// A walk from just outside one side of the page to past the other, at a
// height within the part of the page on screen when it starts, in lattice
// coordinates. The critter then stays at that place on the page.
function plan(yRange) {
    const o = origin();
    const w = document.documentElement.clientWidth;
    const h = window.innerHeight;
    const fromLeft = Math.random() < 0.5;
    const start = nearestCell((fromLeft ? -70 : w + 70) - o.x, window.scrollY + h * rand(...yRange) - o.y);
    const dir = { x: fromLeft ? 1 : -1, y: rand(-0.35, 0.35) };
    const length = Math.hypot(dir.x, dir.y);
    dir.x /= length;
    dir.y /= length;
    // Done once the critter is this far past the far edge of the page.
    const finished = (pageX) => (fromLeft ? pageX > w + 90 : pageX < -90);
    return { start, dir, finished };
}

// --------------------------------------------------------------------------
// Snake: a python. A thick body that follows the head from cell to cell,
// swelling behind the neck and tapering to the tail, with honey blotches
// down its back, a broad head, and a forked tongue that flicks now and then.
// --------------------------------------------------------------------------

const SNAKE = {
    width: 15, // at its thickest
    step: 4, // spacing of the body's sample points
    blotchEvery: 24,
};

// Width along the body, k from 0 (neck) to 1 (tail tip).
function snakeWidth(k) {
    if (k < 0.28) return SNAKE.width * (0.78 + 0.22 * (k / 0.28));
    return SNAKE.width * Math.max(0.1, 1 - 0.9 * Math.pow((k - 0.28) / 0.72, 1.35));
}

function snakeShape(length) {
    const g = svgEl("g", { class: "critter critter-snake" });
    const body = svgEl("path", { class: "snake-body" });
    const blotches = svgEl("g", { class: "snake-blotches" });
    const count = Math.floor((length * 0.82 - 26) / SNAKE.blotchEvery);
    for (let i = 0; i < count; i++) blotches.append(svgEl("ellipse", { rx: 6.5, ry: 3.6 }));
    // The head points along +x from where it joins the neck.
    const head = svgEl("g", { class: "snake-head" });
    // Broader than the body behind it, as a python's head is.
    const skull = svgEl("g", { transform: "scale(1.45 1.55)" });
    head.append(skull);
    skull.append(
        svgEl("path", { class: "snake-tongue", d: "M17.5 0H23M23 0L25.5 -1.8M23 0L25.5 1.8" }),
        svgEl("path", { class: "snake-skull", d: "M-4 -4.6C2 -6.4 9 -6.6 14.5 -3.4Q18 -1.2 18 0Q18 1.2 14.5 3.4C9 6.6 2 6.4 -4 4.6Z" }),
        svgEl("circle", { class: "snake-eye", cx: 10.5, cy: -3.4, r: 1.15 }),
        svgEl("circle", { class: "snake-eye", cx: 10.5, cy: 3.4, r: 1.15 }),
    );
    g.append(body, blotches, head);
    return { g, body, blotches: [...blotches.children], head };
}

function snake(world, done) {
    const { start, dir, finished } = plan([0.15, 0.85]);
    const speed = rand(22, 30); // px per second: an unhurried crawl
    const length = rand(260, 320);
    const { g, body, blotches, head: headEl } = snakeShape(length);
    world.actors.append(g);

    // The cells walked so far, then the head between the last two.
    const path = [start];
    let prev = null;
    let from = start;
    let to = nextCell(start, null, dir);
    let progress = 0;
    let last = null;
    let frame = 0;
    const began = performance.now();

    // Points every SNAKE.step px back from the head along the walked path,
    // smoothed so the turns between cells become curves.
    const sample = (head) => {
        let points = [head];
        let need = SNAKE.step;
        let cursor = head;
        for (let i = path.length - 1; i >= 0 && points.length * SNAKE.step < length; i--) {
            const v = path[i];
            let seg = Math.hypot(cursor.x - v.x, cursor.y - v.y);
            while (seg >= need && points.length * SNAKE.step < length) {
                const t = need / seg;
                cursor = { x: cursor.x + (v.x - cursor.x) * t, y: cursor.y + (v.y - cursor.y) * t };
                points.push(cursor);
                seg -= need;
                need = SNAKE.step;
            }
            need -= seg;
            cursor = v;
        }
        for (let pass = 0; pass < 14; pass++) {
            points = points.map((p, i) => {
                if (i === 0 || i === points.length - 1) return p;
                const a = points[i - 1];
                const b = points[i + 1];
                return { x: (a.x + 2 * p.x + b.x) / 4, y: (a.y + 2 * p.y + b.y) / 4 };
            });
        }
        return points;
    };

    // Unit tangent (pointing towards the head) at sample i.
    const tangent = (points, i) => {
        const a = points[Math.max(0, i - 1)];
        const b = points[Math.min(points.length - 1, i + 1)];
        const l = Math.hypot(a.x - b.x, a.y - b.y) || 1;
        return { x: (a.x - b.x) / l, y: (a.y - b.y) / l };
    };

    const tick = (now) => {
        const dt = last === null ? 0 : Math.min(0.05, (now - last) / 1000);
        last = now;
        const o = origin();
        world.world.setAttribute("transform", `translate(${o.x.toFixed(2)} ${o.y.toFixed(2)})`);

        progress += speed * dt;
        let edge = Math.hypot(to.x - from.x, to.y - from.y);
        while (progress >= edge) {
            progress -= edge;
            light(world.trail, to);
            path.push(to);
            prev = from;
            from = to;
            to = nextCell(from, prev, dir);
            edge = Math.hypot(to.x - from.x, to.y - from.y);
        }
        if (path.length > 60) path.splice(0, path.length - 60);
        const t = progress / edge;
        const head = { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t };

        const points = sample(head);
        const lastIndex = points.length - 1;
        // Width by distance from the head, so a snake still sliding in from
        // the edge keeps its proportions.
        const widthAt = (i) => snakeWidth(Math.min(1, (i * SNAKE.step) / length));
        const left = [];
        const right = [];
        points.forEach((p, i) => {
            const tg = tangent(points, i);
            const half = widthAt(i) / 2;
            left.push(`${(p.x - tg.y * half).toFixed(1)},${(p.y + tg.x * half).toFixed(1)}`);
            right.push(`${(p.x + tg.y * half).toFixed(1)},${(p.y - tg.x * half).toFixed(1)}`);
        });
        body.setAttribute("d", `M${left.join("L")}L${right.reverse().join("L")}Z`);

        // Blotches down the back, each lying along the body and sized to it.
        // A blotch past the end of the body drawn so far stays hidden.
        blotches.forEach((el, j) => {
            const i = Math.round((26 + j * SNAKE.blotchEvery) / SNAKE.step);
            if (i > lastIndex) {
                el.setAttribute("visibility", "hidden");
                return;
            }
            el.removeAttribute("visibility");
            const p = points[i];
            const tg = tangent(points, i);
            const w = widthAt(i);
            el.setAttribute("ry", (w * 0.26).toFixed(1));
            el.setAttribute("rx", (Math.min(6.5, w * 0.55)).toFixed(1));
            el.setAttribute(
                "transform",
                `translate(${p.x.toFixed(1)} ${p.y.toFixed(1)}) rotate(${((Math.atan2(tg.y, tg.x) * 180) / Math.PI).toFixed(1)})`,
            );
        });

        const hd = tangent(points, Math.min(1, lastIndex));
        headEl.setAttribute(
            "transform",
            `translate(${head.x.toFixed(1)} ${head.y.toFixed(1)}) rotate(${((Math.atan2(hd.y, hd.x) * 180) / Math.PI).toFixed(1)})`,
        );

        const tail = points[points.length - 1];
        if ((finished(tail.x + o.x) && finished(head.x + o.x)) || now - began > MAX_WALK_MS) {
            g.remove();
            done();
            return;
        }
        frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => {
        cancelAnimationFrame(frame);
        g.remove();
    };
}

// --------------------------------------------------------------------------
// Crab: scuttles a few cells at a time, pauses, sometimes snaps its claws.
// --------------------------------------------------------------------------

function crabShape() {
    const g = svgEl("g", { class: "critter critter-crab" });
    const legs = svgEl("g", { fill: "none", stroke: "currentColor", "stroke-width": "2.2", "stroke-linecap": "round" });
    for (const side of [-1, 1]) {
        for (let i = 0; i < 3; i++) {
            const x = side * (8 + i * 4);
            legs.append(
                svgEl("path", {
                    d: `M${x} 4 q${side * 6} ${4 + i} ${side * 9} ${10 - i}`,
                    class: `crab-leg crab-leg-${(i + (side > 0 ? 1 : 0)) % 2 ? "a" : "b"}`,
                }),
            );
        }
    }
    const claws = svgEl("g", { fill: "currentColor" });
    for (const side of [-1, 1]) {
        claws.append(
            svgEl("path", {
                d: `M${side * 10} -2 q${side * 6} -6 ${side * 12} -9`,
                fill: "none",
                stroke: "currentColor",
                "stroke-width": "2.4",
                "stroke-linecap": "round",
            }),
            svgEl("path", {
                class: "crab-claw",
                d: `M${side * 22} -11 a5 5 0 1 0 ${side * -0.1} 0.1 l${side * 4} 3 z`,
                style: `transform-origin: ${side * 22}px -11px`,
            }),
        );
    }
    const eyes = svgEl("g", { stroke: "currentColor", "stroke-width": "1.8", "stroke-linecap": "round" });
    for (const side of [-1, 1]) {
        eyes.append(
            svgEl("line", { x1: side * 4, y1: -4, x2: side * 5, y2: -10 }),
            svgEl("circle", { cx: side * 5, cy: -11, r: 2, fill: "currentColor", stroke: "none" }),
        );
    }
    const shell = svgEl("ellipse", { cx: 0, cy: 2, rx: 13, ry: 8.5, fill: "currentColor" });
    // About a cell wide, so it reads clearly next to the python.
    const sprite = svgEl("g", { transform: "scale(1)" });
    sprite.append(legs, claws, eyes, shell);
    g.append(sprite);
    return g;
}

function crab(world, done) {
    const { start, dir, finished } = plan([0.4, 0.9]);
    const g = crabShape();
    world.actors.append(g);

    let pos = { ...start };
    let prev = null;
    let timer = 0;
    let frame = 0;
    let cancelled = false;
    const place = () => g.setAttribute("transform", `translate(${pos.x.toFixed(1)} ${pos.y.toFixed(1)})`);
    place();

    // Keep the layer on the drifting lattice while the crab rests, too.
    const began = performance.now();
    const follow = (now) => {
        const o = origin();
        world.world.setAttribute("transform", `translate(${o.x.toFixed(2)} ${o.y.toFixed(2)})`);
        if (finished(pos.x + o.x) || now - began > MAX_WALK_MS) {
            cancelled = true;
            clearTimeout(timer);
            g.remove();
            done();
            return;
        }
        frame = requestAnimationFrame(follow);
    };
    frame = requestAnimationFrame(follow);

    const walkEdge = (remaining) => {
        if (cancelled) return;
        if (remaining === 0) {
            g.classList.remove("walking");
            if (Math.random() < 0.35) {
                g.classList.add("snapping");
                setTimeout(() => g.classList.remove("snapping"), 900);
            }
            timer = setTimeout(() => walkEdge(Math.round(rand(1, 4))), rand(700, 2400));
            return;
        }
        g.classList.add("walking");
        const from = pos;
        const to = nextCell(from, prev, dir);
        const distance = Math.hypot(to.x - from.x, to.y - from.y);
        const duration = (distance / rand(55, 75)) * 1000;
        const begin = performance.now();
        const step = (now) => {
            if (cancelled) return;
            const t = Math.min(1, (now - begin) / duration);
            pos = { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t };
            place();
            if (t < 1) {
                requestAnimationFrame(step);
                return;
            }
            light(world.trail, to);
            prev = from;
            pos = to;
            walkEdge(remaining - 1);
        };
        requestAnimationFrame(step);
    };
    timer = setTimeout(() => walkEdge(Math.round(rand(2, 4))), 300);

    return () => {
        cancelled = true;
        clearTimeout(timer);
        cancelAnimationFrame(frame);
        g.remove();
    };
}

// --------------------------------------------------------------------------
// Scheduling
// --------------------------------------------------------------------------

export function initCritters() {
    if (document.body?.dataset.critters === "off") return;
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    if (motion.matches) return;

    let world = null;
    let timer = 0;
    let stop = null;
    let lastKind = null;

    const schedule = (range) => {
        clearTimeout(timer);
        timer = setTimeout(spawn, rand(...range));
    };

    function spawn() {
        // One at a time: the walk under way schedules the next when it ends.
        if (stop) return;
        if (document.hidden || motion.matches) {
            schedule(NEXT_DELAY);
            return;
        }
        world ??= createLayer();
        // Mostly alternate, so the same critter rarely comes twice in a row.
        const kind = lastKind && Math.random() < 0.75 ? (lastKind === snake ? crab : snake) : Math.random() < 0.5 ? snake : crab;
        lastKind = kind;
        stop = kind(world, () => {
            stop = null;
            schedule(NEXT_DELAY);
        });
    }

    // A hidden tab stops: the critter leaves, and a new one comes later.
    document.addEventListener("visibilitychange", () => {
        if (!document.hidden) return;
        stop?.();
        stop = null;
        schedule(NEXT_DELAY);
    });
    motion.addEventListener("change", () => {
        if (!motion.matches) return;
        stop?.();
        stop = null;
        clearTimeout(timer);
    });

    schedule(FIRST_DELAY);

    // For trying it out: `waspyCritter()` in the console sends one now.
    window.waspyCritter = () => {
        clearTimeout(timer);
        stop?.();
        stop = null;
        spawn();
    };
}
