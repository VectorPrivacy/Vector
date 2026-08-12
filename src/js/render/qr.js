// QR rendering. Encoding comes from the vendored qrcode-generator library
// (global `qrcode` factory); this file owns how a QR becomes DOM.

/**
 * Render a QR code (SVG) into a container element. Reusable for Profile QR /
 * contact-share / Lightning URI flows.
 *
 * opts.ecc: error correction level ('L' | 'M' | 'Q' | 'H'), default 'Q' —
 * the center logo occludes ~5% of the symbol, so the default funds that
 * damage with headroom instead of spending the whole M budget on it.
 * The SVG has no background of its own — the container supplies the light
 * backing and quiet zone, and CSS fill overrides recolor the modules.
 */
function renderQrInto(containerEl, text, opts = {}) {
    if (!containerEl || !window.qrcode) return false;
    const ecc = opts.ecc || 'Q';
    // Re-rendering an identical QR is pure waste (SVG re-parse + re-raster):
    // profile refreshes re-enter with the same npub over and over.
    const key = `${ecc}|${text}`;
    if (containerEl.dataset.qrKey === key && containerEl.firstChild) return true;
    const qr = window.qrcode(0, ecc);
    qr.addData(text);
    qr.make();
    containerEl.innerHTML = buildCurvyQrSvg(qr);
    containerEl.dataset.qrKey = key;
    return true;
}

/**
 * Fullscreen QR overlay, shared by the Profile QR and the bunker login QR.
 * Closes via the button, a backdrop tap, Escape, or Android hardware back.
 */
function openQrOverlay(text) {
    if (!text) return;
    if (!renderQrInto(document.getElementById('qr-overlay-full'), text)) return;
    const overlay = document.getElementById('qr-overlay');
    overlay.classList.add('active');
    overlay.onclick = (e) => { if (e.target === overlay) closeQrOverlay(); };
    document.getElementById('qr-overlay-close').onclick = closeQrOverlay;
    document.addEventListener('keydown', handleQrOverlayEscape);
    pushBack('qr-overlay', closeQrOverlay);
}

function closeQrOverlay() {
    document.getElementById('qr-overlay').classList.remove('active');
    document.removeEventListener('keydown', handleQrOverlayEscape);
    popBack('qr-overlay');
}

function handleQrOverlayEscape(e) {
    if (e.key === 'Escape') closeQrOverlay();
}

/**
 * Paint a QR as a smoothed "liquid" SVG: a dark module's corner rounds off
 * wherever no dark neighbour continues the run (isolated modules become soft
 * squares, runs fuse into capped bars), light inner corners get concave
 * fillets so meeting runs flow together, and the three finder eyes become
 * rounded rings with matching pupils. Emits <path> elements only, so CSS
 * fill overrides (e.g. the mini banner icon) recolor it wholesale.
 */
function buildCurvyQrSvg(qr) {
    const n = qr.getModuleCount();
    const s = 4;             // units per module
    const r = s * 0.375;     // corner radius — half a module reads as fully melted
    const size = n * s;
    const isFinder = (row, col) =>
        (row < 7 && col < 7) || (row < 7 && col >= n - 7) || (row >= n - 7 && col < 7);

    // Center-logo knockout: modules the sticker touches are simply not drawn
    // (no cut-off fragments peeking out behind it), and the hole reads as
    // light to the neighbour logic so surviving modules round toward it.
    // To a decoder, absent and white-covered are the same pixels — the ECC
    // budget already funds this damage, plus the small margin ring.
    const logoH = size * 0.28;
    const k = logoH / QR_LOGO_H;
    const tx = (size - QR_LOGO_W * k) / 2;
    const ty = (size - logoH) / 2;
    // Conservative shield silhouette in logo units: rounded-shoulder rect
    // down to the taper line, then a linear taper to the rounded bottom tip.
    const shieldHit = (lx, ly, m) => {
        if (lx < -m || lx > QR_LOGO_W + m || ly < -m || ly > QR_LOGO_H + m) return false;
        const shoulder = 9;
        if (ly < shoulder) {
            if (lx < shoulder) return Math.hypot(lx - shoulder, ly - shoulder) <= shoulder + m;
            if (lx > QR_LOGO_W - shoulder) return Math.hypot(lx - (QR_LOGO_W - shoulder), ly - shoulder) <= shoulder + m;
            return true;
        }
        const taperY = 44;
        if (ly <= taperY) return true;
        const t = (QR_LOGO_H - ly) / (QR_LOGO_H - taperY);
        return Math.abs(lx - QR_LOGO_W / 2) <= (QR_LOGO_W / 2) * Math.max(t, 0) + m;
    };
    const mLogo = (0.75 * s) / k; // margin: ~3/4 module of clean white around the sticker
    const holed = (row, col) => {
        const x = col * s, y = row * s;
        for (const [px, py] of [[x, y], [x + s, y], [x, y + s], [x + s, y + s], [x + s / 2, y + s / 2]]) {
            if (shieldHit((px - tx) / k, (py - ty) / k, mLogo)) return true;
        }
        return false;
    };
    const dark = (row, col) => row >= 0 && col >= 0 && row < n && col < n
        && qr.isDark(row, col) && !holed(row, col);

    let d = '';
    for (let row = 0; row < n; row++) {
        for (let col = 0; col < n; col++) {
            if (isFinder(row, col)) continue;
            const x = col * s, y = row * s;
            const top = dark(row - 1, col), bottom = dark(row + 1, col);
            const left = dark(row, col - 1), right = dark(row, col + 1);
            if (dark(row, col)) {
                const tl = !top && !left ? r : 0, tr = !top && !right ? r : 0;
                const br = !bottom && !right ? r : 0, bl = !bottom && !left ? r : 0;
                d += `M${x + tl},${y}h${s - tl - tr}` + (tr ? `a${tr},${tr} 0 0 1 ${tr},${tr}` : '')
                    + `v${s - tr - br}` + (br ? `a${br},${br} 0 0 1 ${-br},${br}` : '')
                    + `h${-(s - br - bl)}` + (bl ? `a${bl},${bl} 0 0 1 ${-bl},${-bl}` : '')
                    + `v${-(s - bl - tl)}` + (tl ? `a${tl},${tl} 0 0 1 ${tl},${-tl}` : '') + 'z';
            } else {
                // Fillet only true inner corners (diagonal dark too) — welding
                // diagonal-only neighbours turns checkerboards into lattice noise.
                if (top && left && dark(row - 1, col - 1)) d += `M${x},${y}L${x + r},${y}A${r},${r} 0 0 0 ${x},${y + r}z`;
                if (top && right && dark(row - 1, col + 1)) d += `M${x + s},${y}L${x + s},${y + r}A${r},${r} 0 0 0 ${x + s - r},${y}z`;
                if (bottom && right && dark(row + 1, col + 1)) d += `M${x + s},${y + s}L${x + s - r},${y + s}A${r},${r} 0 0 0 ${x + s},${y + s - r}z`;
                if (bottom && left && dark(row + 1, col - 1)) d += `M${x},${y + s}L${x},${y + s - r}A${r},${r} 0 0 0 ${x + r},${y + s}z`;
            }
        }
    }

    const roundedSquare = (x, y, w, radius) =>
        `M${x + radius},${y}h${w - 2 * radius}a${radius},${radius} 0 0 1 ${radius},${radius}`
        + `v${w - 2 * radius}a${radius},${radius} 0 0 1 ${-radius},${radius}`
        + `h${-(w - 2 * radius)}a${radius},${radius} 0 0 1 ${-radius},${-radius}`
        + `v${-(w - 2 * radius)}a${radius},${radius} 0 0 1 ${radius},${-radius}z`;
    let eyes = '';
    for (const [fx, fy] of [[0, 0], [(n - 7) * s, 0], [0, (n - 7) * s]]) {
        // Ring: 7-module rounded square with a concentric 5-module hole (evenodd)
        eyes += `<path d="${roundedSquare(fx, fy, 7 * s, 2.1 * s)}${roundedSquare(fx + s, fy + s, 5 * s, 1.1 * s)}" fill="#000" fill-rule="evenodd"/>`;
        // Pupil: the 3-module centre, rounded to match the ring
        eyes += `<path d="${roundedSquare(fx + 2 * s, fy + 2 * s, 3 * s, 1.1 * s)}" fill="#000"/>`;
    }

    // Center logo, placed in the knockout carved above. It lives in a <g> —
    // recolor rules target the svg's direct-child <path>es only, so the
    // sticker keeps its own white/black identity on any backing.
    const logo = `<g class="qr-logo" transform="translate(${tx.toFixed(2)},${ty.toFixed(2)}) scale(${k.toFixed(4)})">${QR_LOGO_PATHS}</g>`;

    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" preserveAspectRatio="xMinYMin meet">`
        + `<path d="${d}" fill="#000"/>` + eyes + logo + '</svg>';
}

// Vector logo with sticker padding baked in (the #fff path IS the carve-out).
// Inlined: render time must never fetch, and ids are stripped so multiple
// QRs in one DOM can't collide.
const QR_LOGO_W = 40.81, QR_LOGO_H = 63.38;
const QR_LOGO_PATHS =
    '<path fill="#fff" d="M40.81 23.2a5 5 0 0 0-.55-2.29q.5-1.5.5-3.07V8.69A8.7 8.7 0 0 0 32.07 0h-.23a8.7 8.7 0 0 0-6.83 3.65 9.9 9.9 0 0 0-9.2 0A8.7 8.7 0 0 0 8.98 0h-.23A8.7 8.7 0 0 0 .06 8.69v9.15q0 1.57.5 3.07a5 5 0 0 0-.55 2.29v7.74q0 1.54.48 3.02a5 5 0 0 0-.48 2.15v7.74a9.8 9.8 0 0 0 3.18 7.22L13.8 60.8a9.77 9.77 0 0 0 13.24 0l10.61-9.73a9.8 9.8 0 0 0 3.18-7.22v-7.74q0-1.15-.48-2.15.48-1.47.48-3.02V23.2Z"/>'
    + '<path fill="#000" d="m35.78 36.12-11.99 10.9a5 5 0 0 1-6.77 0L5.03 36.12l-.01-.01v7.74c0 1.34.56 2.62 1.55 3.52l10.61 9.73a4.76 4.76 0 0 0 6.46 0l10.61-9.73a4.8 4.8 0 0 0 1.55-3.52v-7.74z"/>'
    + '<path fill="#000" d="m29.79 38.55 4.45-4.08a4.8 4.8 0 0 0 1.55-3.52v-7.74l-.01.01-11.99 10.9a5 5 0 0 1-6.77 0L5.03 23.22l-.01-.01v7.74c0 1.34.56 2.62 1.55 3.52l10.61 9.73a4.76 4.76 0 0 0 6.46 0l6.15-5.64"/>'
    + '<path fill="#000" d="M31.97 5.02a3.73 3.73 0 0 0-3.58 3.76v6.28c0 1.05-.44 2.06-1.22 2.77l-3.42 3.13a5 5 0 0 1-3.35 1.32 5 5 0 0 1-3.35-1.32l-3.42-3.13a3.8 3.8 0 0 1-1.22-2.77V8.78c0-2.01-1.57-3.7-3.58-3.76a3.7 3.7 0 0 0-3.77 3.67v9.15c0 1.34.56 2.62 1.55 3.52l10.61 9.73c.87.8 2 1.24 3.17 1.25a4.7 4.7 0 0 0 3.17-1.25l10.61-9.73a4.8 4.8 0 0 0 1.55-3.52V8.69c0-2.06-1.7-3.73-3.77-3.67Z"/>'
    + '<circle fill="#000" cx="20.41" cy="12.37" r="4.85"/>';
