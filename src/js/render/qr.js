// QR rendering. Encoding comes from the vendored qrcode-generator library
// (global `qrcode` factory); this file owns how a QR becomes DOM.

/**
 * Render a QR code (SVG) into a container element. Reusable for Profile QR /
 * contact-share / Lightning URI flows.
 *
 * opts.ecc: error correction level ('L' | 'M' | 'Q' | 'H'), default 'M'.
 * The SVG has no background of its own — the container supplies the light
 * backing and quiet zone, and CSS fill overrides recolor the modules.
 */
function renderQrInto(containerEl, text, opts = {}) {
    if (!containerEl || !window.qrcode) return false;
    const ecc = opts.ecc || 'M';
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
    const dark = (row, col) => row >= 0 && col >= 0 && row < n && col < n && qr.isDark(row, col);

    let d = '';
    for (let row = 0; row < n; row++) {
        for (let col = 0; col < n; col++) {
            if (isFinder(row, col)) continue;
            const x = col * s, y = row * s;
            const top = dark(row - 1, col), bottom = dark(row + 1, col);
            const left = dark(row, col - 1), right = dark(row, col + 1);
            if (qr.isDark(row, col)) {
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

    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" preserveAspectRatio="xMinYMin meet">`
        + `<path d="${d}" fill="#000"/>` + eyes + '</svg>';
}
