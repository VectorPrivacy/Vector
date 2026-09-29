// Vector Web on phones: the edges the OS draws over the page.
(() => {
    'use strict';
    if (!matchMedia('(pointer: coarse)').matches) return;
    const root = document.documentElement;

    // ─── Floor ──────────────────────────────────────────────────────────────
    // A Home Screen web app runs to the bottom of the glass, under the home
    // indicator, and reports no safe-area inset. The floor lifts the tab bar and
    // composer to where the display's corners end, so a tap never lands on the
    // system gesture. Radii are the displays' own, in points, keyed by screen size.
    const RADII = {
        '375x812': 44, '414x896': 41.5, '390x844': 47.33, '428x926': 53.33,
        '393x852': 55, '430x932': 55, '402x874': 62, '440x956': 62, '420x912': 62,
    };
    function cornerRadius() {
        const w = Math.min(screen.width, screen.height);
        const h = Math.max(screen.width, screen.height);
        if (w === 414 && h === 896 && devicePixelRatio === 3) return 39;
        return RADII[`${w}x${h}`] ?? (h >= 812 ? 34 : 0);
    }
    const standalone = navigator.standalone === true || matchMedia('(display-mode: standalone)').matches;
    const radius = standalone ? cornerRadius() : 0;
    // ─── Keyboard ───────────────────────────────────────────────────────────
    // iOS shrinks the window for the keyboard but keeps laying the page out at
    // full height, then scrolls it up to reveal the input: the header leaves the
    // screen. The page is sized to what's visible instead and held at the top,
    // as Android resizes it. The document's own client height stays full-size.
    function fit() {
        const vv = window.visualViewport;
        const keyboard = !!vv && root.clientHeight - vv.height > 50;
        root.style.setProperty('--floor', `${keyboard ? 0 : radius}px`);
        root.style.height = keyboard ? `${vv.height}px` : '';
        if (keyboard) root.style.setProperty('--app-h', `${vv.height}px`); else root.style.removeProperty('--app-h');
        if (keyboard && (scrollY || vv.offsetTop)) scrollTo(0, 0);
    }
    fit();
    window.visualViewport?.addEventListener('resize', fit);
    window.visualViewport?.addEventListener('scroll', fit);
    addEventListener('resize', fit);
    addEventListener('scroll', fit, { passive: true });

    // ─── Top edge ───────────────────────────────────────────────────────────
    // iOS Safari tints its status bar from an opaque fixed box spanning the top
    // edge, and blurs or blackens it otherwise: a translucent header doesn't count.
    // The cap is that box on every screen, in the colour the top edge shows.
    const alphaOf = (c) => { const m = /rgba?\(([^)]+)\)/.exec(c); if (!m) return 0; const p = m[1].split(/[ ,/]+/).filter(Boolean); return p.length > 3 ? parseFloat(p[3]) : 1; };
    const channels = (c) => /rgba?\(([^)]+)\)/.exec(c)[1].split(/[ ,/]+/).filter(Boolean).map(parseFloat);

    function over(top, base) {
        const [r, g, b, a = 1] = channels(top);
        const [R, G, B] = channels(base);
        const mix = (x, y) => Math.round(x * a + y * (1 - a));
        return `rgb(${mix(r, R)}, ${mix(g, G)}, ${mix(b, B)})`;
    }

    function paint(cap) {
        const show = (on, bg) => {
            if (cap.hidden === on) cap.hidden = !on;
            if (on && cap.style.backgroundColor !== bg) cap.style.backgroundColor = bg;
        };
        if (!document.body.classList.contains('mobile')) { show(false); return; }
        // Topmost first: stack the translucent layers down to the first opaque one.
        const layers = [];
        for (const hit of document.elementsFromPoint(innerWidth / 2, 4)) {
            if (hit === cap || hit === root) continue;
            const bg = getComputedStyle(hit).backgroundColor;
            const a = alphaOf(bg);
            if (a <= 0) continue;
            layers.push(bg);
            if (a >= 1) break;
        }
        let colour = getComputedStyle(root).backgroundColor;
        for (const layer of layers.reverse()) colour = over(layer, colour);
        show(true, colour);
    }

    addEventListener('DOMContentLoaded', () => {
        const cap = document.querySelector('.edge-cap');
        if (!cap) return;
        // Coalesced, plus a late pass once a screen's entrance animation settles.
        let soon = 0;
        let late = 0;
        const schedule = () => {
            if (!soon) soon = requestAnimationFrame(() => { soon = 0; paint(cap); });
            clearTimeout(late);
            late = setTimeout(() => paint(cap), 450);
        };
        new MutationObserver(schedule).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ['style', 'class', 'hidden'] });
        addEventListener('resize', schedule);
        // The keyboard scrolls and shrinks the viewport without touching the DOM.
        addEventListener('scroll', schedule, { passive: true });
        window.visualViewport?.addEventListener('resize', schedule);
        window.visualViewport?.addEventListener('scroll', schedule);
        schedule();
    });
})();
