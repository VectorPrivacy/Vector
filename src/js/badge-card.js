// Interactive 3D holographic "achievement" card for profile badges. The card is a
// component (ui/BadgeCard) painting CSS vars; this owns the input: the pointer tilt
// (rAF-throttled) and the native gyro (setBadgeTilt), so it stays GPU-composited.

const _BADGE_TILT_DEG = 12;          // max tilt; refined, not extreme
const _badgeReduceMotion = !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
let _badgeRaf = 0;
let _badgePtr = null;
let _badgeKeyHandler = null;
let _badgeGyroActive = false;        // true only when a real rotation sensor is driving the tilt

// Native gyro bridge (Android MainActivity) pushes pitch/roll degrees here while the card is open, so
// the badge reacts to physically tilting the phone, sidestepping the WebView's deviceorientation prompt.
window.__vectorGyro = function (rx, ry) {
    if (VectorSvelte.badgeCardState().visible) setBadgeTilt(rx, ry);
};

VectorSvelte.setBadgeCardHandlers({
    close: () => hideBadgeCard(),
    // Pointer drives the tilt unless a real gyro is (then the sensor wins); skipped under reduced motion.
    pointerMove: (e) => {
        if (_badgeReduceMotion || _badgeGyroActive) return;
        _badgePtr = e;
        if (!_badgeRaf) _badgeRaf = requestAnimationFrame(_applyBadgePointerTilt);
    },
    pointerEnd: () => { if (!_badgeReduceMotion && !_badgeGyroActive) _resetBadgeTilt(); },
});

function _writeBadgeTilt(rx, ry, mxPct, myPct, holo) {
    VectorSvelte.setBadgeTiltVars({
        rx: rx.toFixed(2) + 'deg', ry: ry.toFixed(2) + 'deg',
        mx: mxPct.toFixed(1) + '%', my: myPct.toFixed(1) + '%',
        holo: (holo == null ? 1 : holo).toFixed(2), idle: false,
    });
}

function _applyBadgePointerTilt() {
    _badgeRaf = 0;
    const card = VectorSvelte.badgeCardEls().card;
    if (!_badgePtr || !card) return;
    const r = card.getBoundingClientRect();
    if (!r.width) return;
    const cx = Math.min(1, Math.max(0, (_badgePtr.clientX - r.left) / r.width));
    const cy = Math.min(1, Math.max(0, (_badgePtr.clientY - r.top) / r.height));
    _writeBadgeTilt((0.5 - cy) * 2 * _BADGE_TILT_DEG, (cx - 0.5) * 2 * _BADGE_TILT_DEG, cx * 100, cy * 100);
}

/** Native gyro (pitch/roll in degrees) drives the same tilt. */
function setBadgeTilt(rx, ry) {
    if (!VectorSvelte.badgeCardState().open) return;
    const clamp = (v) => Math.max(-_BADGE_TILT_DEG, Math.min(_BADGE_TILT_DEG, v));
    rx = clamp(rx); ry = clamp(ry);
    // Sheen blooms with how far it's tilted, so a still (gyro) card shows no sheen at rest.
    const holo = Math.min(1, (Math.abs(rx) + Math.abs(ry)) / _BADGE_TILT_DEG);
    _writeBadgeTilt(rx, ry, 50 + (ry / _BADGE_TILT_DEG) * 50, 50 - (rx / _BADGE_TILT_DEG) * 50, holo);
}

function _resetBadgeTilt() {
    // Idle springs the card back to flat through the stylesheet's transition.
    VectorSvelte.setBadgeTiltVars({ rx: '0deg', ry: '0deg', mx: '50%', my: '50%', holo: '0', idle: true });
}

/** @param {{title:string, html:string, svg:string, perks?:{text:string,sub?:string}[], subtitle?:string, tierProgress?:{current:number,total:number,icons:string[]}, access?:string}} badge */
function showBadgeCard({ title, html, svg, perks, subtitle, tierProgress, access }) {
    VectorSvelte.setBadgeCard({
        badge: {
            src: /:\/\/|^data:|^blob:/.test(svg) ? svg : './icons/' + svg,
            title: title || '', subtitle: subtitle || '', html: html || '',
            tiers: tierProgress || null, access: access || '', perks: perks || [],
        },
        open: true, visible: false,
    });
    _resetBadgeTilt();
    // Double rAF so the scaled-in start paints before is-visible flips it: the open transition
    // then plays reliably on first show.
    requestAnimationFrame(() => requestAnimationFrame(() => {
        if (VectorSvelte.badgeCardState().open) VectorSvelte.setBadgeCard({ visible: true });
    }));
    _badgeKeyHandler = (e) => { if (e.key === 'Escape') { e.preventDefault(); hideBadgeCard(); } };
    document.addEventListener('keydown', _badgeKeyHandler);
    // Gyro if the device has the sensor; otherwise the pointer tilt takes over.
    _badgeGyroActive = false;
    if (!_badgeReduceMotion && window.__vectorGyroBridge) {
        try { _badgeGyroActive = !!window.__vectorGyroBridge.start(); } catch (_) {}
    }
    pushBack('badge-card', hideBadgeCard);
}

function hideBadgeCard() {
    const st = VectorSvelte.badgeCardState();
    if (!st.open) return;
    VectorSvelte.setBadgeCard({ visible: false });
    if (_badgeKeyHandler) { document.removeEventListener('keydown', _badgeKeyHandler); _badgeKeyHandler = null; }
    try { if (window.__vectorGyroBridge) window.__vectorGyroBridge.stop(); } catch (_) {}
    _badgeGyroActive = false;
    popBack('badge-card');                    // no-op if a hardware back already popped us
    setTimeout(() => { if (!VectorSvelte.badgeCardState().visible) VectorSvelte.setBadgeCard({ open: false }); }, 260);
}
