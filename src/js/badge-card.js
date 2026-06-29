// Interactive 3D holographic "achievement" card for profile badges. Replaces the flat
// popupConfirm badge popup: the card tilts toward the pointer (Phase 2: native gyro via
// setBadgeTilt) while a holographic foil sheen sweeps across with the motion.
//
// Perf: pointer input is rAF-throttled and only writes CSS custom properties (--rx/--ry/
// --mx/--my/--holo); the transform + gradients read them, so it stays GPU-composited (60fps
// on budget Android). The badge/title/desc sit at different translateZ for parallax depth.

const _BADGE_TILT_DEG = 12;          // max tilt; refined, not extreme
const _badgeReduceMotion = !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
let _badgeEls = null;
let _badgeRaf = 0;
let _badgePtr = null;
let _badgeKeyHandler = null;
let _badgeGyroActive = false;        // true only when a real rotation sensor is driving the tilt
let _badgeOpen = false;              // logical open state (guards the open/close transitions)

// Native gyro bridge (Android MainActivity) pushes pitch/roll degrees here while the card is open, so
// the badge reacts to physically tilting the phone — sidestepping the WebView's deviceorientation prompt.
window.__vectorGyro = function (rx, ry) {
    if (_badgeEls && _badgeEls.overlay.classList.contains('is-visible')) setBadgeTilt(rx, ry);
};

function _buildBadgeCardDom() {
    const overlay = document.createElement('div');
    overlay.className = 'badge-card-overlay';
    overlay.id = 'badge-card-overlay';
    overlay.innerHTML =
        '<div class="badge-card-scene">' +
          '<div class="badge-card">' +
            '<div class="badge-card-holo"></div>' +
            '<div class="badge-card-glare"></div>' +
            '<img class="badge-card-badge" alt="" draggable="false">' +
            '<div class="badge-card-title"></div>' +
            '<div class="badge-card-subtitle"></div>' +
            '<div class="badge-card-desc"></div>' +
            '<div class="badge-card-tiers"></div>' +
            '<div class="badge-card-access">' +
              // Award-ribbon glyph; inherits the accent green via currentColor.
              '<svg class="badge-card-access-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="8" r="6"></circle><path d="M15.477 12.89 17 22l-5-3-5 3 1.523-9.11"></path></svg>' +
              '<span class="badge-card-access-text"></span>' +
            '</div>' +
            '<div class="badge-card-perks"></div>' +
          '</div>' +
        '</div>';
    document.body.appendChild(overlay);

    const scene = overlay.querySelector('.badge-card-scene');
    const card = overlay.querySelector('.badge-card');

    // Backdrop (outside the card) closes; clicks on the card itself don't.
    overlay.addEventListener('click', (e) => { if (e.target === overlay) hideBadgeCard(); });

    // Pointer (mouse on desktop, finger on touch) drives the tilt. When a real gyro is driving
    // (_badgeGyroActive), it's suppressed so the sensor wins; on a gyro-less phone it's the fallback,
    // so a finger "presses the badge in". Skipped under reduced-motion.
    if (!_badgeReduceMotion) {
        scene.addEventListener('pointermove', (e) => {
            if (_badgeGyroActive) return;
            _badgePtr = e;
            if (!_badgeRaf) _badgeRaf = requestAnimationFrame(_applyBadgePointerTilt);
        });
        const onPointerEnd = () => { if (!_badgeGyroActive) _resetBadgeTilt(); };
        scene.addEventListener('pointerleave', onPointerEnd);
        scene.addEventListener('pointercancel', onPointerEnd);
    }

    _badgeEls = {
        overlay, scene, card,
        badge: overlay.querySelector('.badge-card-badge'),
        title: overlay.querySelector('.badge-card-title'),
        subtitle: overlay.querySelector('.badge-card-subtitle'),
        desc: overlay.querySelector('.badge-card-desc'),
        tiers: overlay.querySelector('.badge-card-tiers'),
        access: overlay.querySelector('.badge-card-access'),
        accessText: overlay.querySelector('.badge-card-access-text'),
        perks: overlay.querySelector('.badge-card-perks'),
    };
    return _badgeEls;
}

function _writeBadgeTilt(rx, ry, mxPct, myPct, holo) {
    const c = _badgeEls.card;
    c.classList.remove('badge-card--idle');
    c.style.setProperty('--rx', rx.toFixed(2) + 'deg');
    c.style.setProperty('--ry', ry.toFixed(2) + 'deg');
    c.style.setProperty('--mx', mxPct.toFixed(1) + '%');
    c.style.setProperty('--my', myPct.toFixed(1) + '%');
    c.style.setProperty('--holo', (holo == null ? 1 : holo).toFixed(2));
}

function _applyBadgePointerTilt() {
    _badgeRaf = 0;
    if (!_badgePtr || !_badgeEls) return;
    const r = _badgeEls.card.getBoundingClientRect();
    if (!r.width) return;
    const cx = Math.min(1, Math.max(0, (_badgePtr.clientX - r.left) / r.width));
    const cy = Math.min(1, Math.max(0, (_badgePtr.clientY - r.top) / r.height));
    _writeBadgeTilt((0.5 - cy) * 2 * _BADGE_TILT_DEG, (cx - 0.5) * 2 * _BADGE_TILT_DEG, cx * 100, cy * 100);
}

/** Phase 2 hook: native gyro (pitch/roll in degrees) drives the same tilt. */
function setBadgeTilt(rx, ry) {
    if (!_badgeEls) return;
    const clamp = (v) => Math.max(-_BADGE_TILT_DEG, Math.min(_BADGE_TILT_DEG, v));
    rx = clamp(rx); ry = clamp(ry);
    // Sheen blooms with how far it's tilted, so a still (gyro) card shows no sheen at rest.
    const holo = Math.min(1, (Math.abs(rx) + Math.abs(ry)) / _BADGE_TILT_DEG);
    _writeBadgeTilt(rx, ry, 50 + (ry / _BADGE_TILT_DEG) * 50, 50 - (rx / _BADGE_TILT_DEG) * 50, holo);
}

function _resetBadgeTilt() {
    if (!_badgeEls) return;
    const c = _badgeEls.card;
    c.classList.add('badge-card--idle');     // spring transition back to flat
    c.style.setProperty('--rx', '0deg');
    c.style.setProperty('--ry', '0deg');
    c.style.setProperty('--mx', '50%');
    c.style.setProperty('--my', '50%');
    c.style.setProperty('--holo', '0');
}

/** Render a badge's bonuses (perks) under the description; hidden when a badge grants nothing. */
function _renderBadgePerks(els, perks) {
    els.perks.innerHTML = '';
    if (!perks || !perks.length) { els.perks.style.display = 'none'; return; }
    els.perks.style.display = '';
    const label = document.createElement('div');
    label.className = 'badge-card-perks-label';
    label.textContent = perks.length === 1 ? 'Perk' : 'Perks';
    els.perks.appendChild(label);
    for (const p of perks) {
        const row = document.createElement('div');
        row.className = 'badge-card-perk';
        const text = document.createElement('span');
        text.className = 'badge-card-perk-text';
        text.textContent = p.text;
        row.appendChild(text);
        if (p.sub) {
            const sub = document.createElement('span');
            sub.className = 'badge-card-perk-sub';
            sub.textContent = p.sub;
            row.appendChild(sub);
        }
        els.perks.appendChild(row);
    }
}

/** Render the tiered-badge progress rail: `total` circular nodes (icons[i-1]), filled through `current`,
 *  joined by connectors that go accent-green only between two already-held tiers. Hidden when absent. */
function _renderBadgeTiers(els, tierProgress) {
    els.tiers.innerHTML = '';
    if (!tierProgress || !tierProgress.total) { els.tiers.style.display = 'none'; return; }
    els.tiers.style.display = '';
    const { current, total, icons } = tierProgress;
    for (let i = 1; i <= total; i++) {
        const node = document.createElement('div');
        node.className = 'badge-card-tier-node' + (i <= current ? ' is-filled' : '');
        const img = document.createElement('img');
        img.alt = ''; img.draggable = false;
        const ic = icons && icons[i - 1];
        if (ic) img.src = /:\/\/|^data:|^blob:/.test(ic) ? ic : './icons/' + ic;
        node.appendChild(img);
        els.tiers.appendChild(node);
        // Connector to the next node is green only when both ends (i and i+1) are held.
        if (i < total) {
            const conn = document.createElement('div');
            conn.className = 'badge-card-tier-conn' + (current >= i + 1 ? ' is-filled' : '');
            els.tiers.appendChild(conn);
        }
    }
}

/** @param {{title:string, html:string, svg:string, perks?:{text:string,sub?:string}[], subtitle?:string, tierProgress?:{current:number,total:number,icons:string[]}, access?:string}} badge — `html` is trusted (hardcoded copy). */
function showBadgeCard({ title, html, svg, perks, subtitle, tierProgress, access }) {
    const els = _badgeEls || _buildBadgeCardDom();
    els.badge.src = /:\/\/|^data:|^blob:/.test(svg) ? svg : './icons/' + svg;
    els.title.textContent = title || '';
    if (subtitle) { els.subtitle.textContent = subtitle; els.subtitle.style.display = ''; }
    else { els.subtitle.style.display = 'none'; }
    els.desc.innerHTML = html || '';
    _renderBadgeTiers(els, tierProgress);
    if (access) { els.accessText.textContent = access; els.access.style.display = ''; }
    else { els.access.style.display = 'none'; }
    _renderBadgePerks(els, perks);
    _resetBadgeTilt();
    _badgeOpen = true;
    els.overlay.style.display = 'flex';
    // Double-rAF so the opacity:0 / scaled-in start paints before is-visible flips it — reliably plays
    // the open transition even on first show (a reflow alone doesn't always trigger it from display:none).
    requestAnimationFrame(() => requestAnimationFrame(() => {
        if (_badgeOpen) els.overlay.classList.add('is-visible');
    }));
    _badgeKeyHandler = (e) => { if (e.key === 'Escape') { e.preventDefault(); hideBadgeCard(); } };
    document.addEventListener('keydown', _badgeKeyHandler);
    // Gyro if the device has the sensor; otherwise _badgeGyroActive stays false and the pointer/touch
    // tilt takes over (a finger presses the badge in).
    _badgeGyroActive = false;
    if (!_badgeReduceMotion && window.__vectorGyroBridge) {
        try { _badgeGyroActive = !!window.__vectorGyroBridge.start(); } catch (_) {}
    }
    pushBack('badge-card', hideBadgeCard);
}

function hideBadgeCard() {
    if (!_badgeEls || !_badgeOpen) return;
    _badgeOpen = false;
    const { overlay } = _badgeEls;
    overlay.classList.remove('is-visible');
    if (_badgeKeyHandler) { document.removeEventListener('keydown', _badgeKeyHandler); _badgeKeyHandler = null; }
    try { if (window.__vectorGyroBridge) window.__vectorGyroBridge.stop(); } catch (_) {}
    _badgeGyroActive = false;
    popBack('badge-card');                    // no-op if a hardware-back already popped us
    setTimeout(() => { if (_badgeEls && !_badgeOpen) _badgeEls.overlay.style.display = 'none'; }, 260);
}
