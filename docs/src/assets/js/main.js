// Shared behaviour for every page: theme toggle, mobile menu, scroll
// reveals, copy buttons, the header's scrolled state, and the background
// critters.

import { initCritters } from "./critters.js";

const THEME_KEY = "theme";

function setTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    try {
        localStorage.setItem(THEME_KEY, theme);
    } catch {
        // Storage can be unavailable (private mode); the toggle still works.
    }
    document.dispatchEvent(new CustomEvent("themechange", { detail: { theme } }));
}

function initThemeToggle() {
    document.querySelectorAll("[data-theme-toggle]").forEach((button) => {
        button.addEventListener("click", () => {
            const current = document.documentElement.getAttribute("data-theme");
            setTheme(current === "light" ? "dark" : "light");
        });
    });
}

function initMobileMenu() {
    const button = document.querySelector("[data-menu-toggle]");
    const menu = document.getElementById("mobile-menu");
    if (!button || !menu) return;
    const close = () => {
        menu.hidden = true;
        button.setAttribute("aria-expanded", "false");
    };
    button.addEventListener("click", () => {
        const open = menu.hidden;
        menu.hidden = !open;
        button.setAttribute("aria-expanded", String(open));
    });
    document.addEventListener("keydown", (event) => {
        if (event.key === "Escape") close();
    });
    window.matchMedia("(min-width: 768px)").addEventListener("change", (event) => {
        if (event.matches) close();
    });
}

function initHeaderState() {
    const header = document.querySelector("[data-site-header]");
    if (!header) return;
    const update = () => header.toggleAttribute("data-scrolled", window.scrollY > 8);
    update();
    window.addEventListener("scroll", update, { passive: true });
}

function initReveal() {
    const elements = document.querySelectorAll(".reveal");
    if (!elements.length) return;
    if (!("IntersectionObserver" in window)) {
        elements.forEach((el) => el.classList.add("in-view"));
        return;
    }
    const observer = new IntersectionObserver(
        (entries) => {
            for (const entry of entries) {
                if (entry.isIntersecting) {
                    entry.target.classList.add("in-view");
                    observer.unobserve(entry.target);
                }
            }
        },
        { threshold: 0.1, rootMargin: "0px 0px -40px 0px" },
    );
    elements.forEach((el) => observer.observe(el));
}

// <button data-copy="text"> copies its text; a <pre> inside [data-copy-code]
// gets a copy button of its own.
function initCopyButtons() {
    // A button with a [data-copy-label] child (an icon beside a label)
    // changes only that label, so the icon stays.
    const flash = (button, label) => {
        const target = button.querySelector("[data-copy-label]") || button;
        const original = target.dataset.label || target.textContent;
        target.dataset.label = original;
        target.textContent = label;
        button.toggleAttribute("data-copied", true);
        setTimeout(() => {
            target.textContent = original;
            button.removeAttribute("data-copied");
        }, 1800);
    };
    const copy = (button, text) =>
        navigator.clipboard.writeText(text).then(
            () => flash(button, "Copied"),
            () => flash(button, "Press Ctrl+C"),
        );

    document.querySelectorAll("[data-copy]").forEach((button) => {
        button.addEventListener("click", () => copy(button, button.dataset.copy));
    });

    document.querySelectorAll("[data-copy-code] pre").forEach((pre) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "code-copy";
        button.textContent = "Copy";
        button.addEventListener("click", () => copy(button, pre.innerText));
        const wrapper = document.createElement("div");
        wrapper.className = "code-block";
        pre.replaceWith(wrapper);
        wrapper.append(pre, button);
    });
}

document.addEventListener("DOMContentLoaded", () => {
    initThemeToggle();
    initMobileMenu();
    initHeaderState();
    initReveal();
    initCopyButtons();
    initCritters();
});
