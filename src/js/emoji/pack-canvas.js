// The picker's pack sections: one canvas per pack drawn from a shared frame cache, and the shared emoji tooltip.
// One global scope: loads before picker.js and shares its globals.

// ============================================================================
// Canvas-batched pack rendering
// ============================================================================
//
// Each pack section gets one `<canvas>` that draws every emoji from a
// shared decoded-frame cache. One compositor layer per section instead
// of one per emoji, and animation is driven by a single requestAnimationFrame
// — not by the browser's native animated-image pipeline. This is what
// Discord (on Electron/Chromium) gets for free; on WKWebView we build it.
//
// Decoding happens in Rust: each emoji becomes a presized PNG strip of 56×56 frames
// on disk, and a section's strips fold into one atlas file, so the webview loads a
// pack in one asset request and slices it with drawImage. Descriptors are cached
// globally by URL so reopening the panel reuses everything.
//
// Activation: IntersectionObserver toggles each section's slot in the
// global active set; the rAF loop self-terminates when the set is empty.
// Panel hide manually drains the set so we don't keep ticking under
// opacity:0. Single shared rAF for ALL pack sections in the picker.

// WKWebView doesn't ship the WebCodecs `ImageDecoder` API, so frame decoding
// runs in Rust; nothing but small descriptors crosses IPC.
const PACK_CANVAS_THUMB_PX = 28;
/** Row stride. Slightly taller than the thumb's 28+pad to give rows a
 *  bit of breathing room between them, matching the stock grid's 4px
 *  gap visually without introducing real gaps inside the canvas. */
const PACK_CANVAS_CELL_PX = 38;
/** Visible hover/active highlight box — always square, sized to the
 *  row height minus margin so vertical gaps between rows stay clean. */
const PACK_CANVAS_BOX_PX = 34;
/** Horizontal gap between cells. Mirrors stock `.emoji-grid { gap: 4px }`
 *  so the canvas column positions land at the same x-coordinates as the
 *  native grid's columns — otherwise canvas cells are slightly wider
 *  and the first-column thumb visibly drifts right of stock's first emoji. */
const PACK_CANVAS_GAP_PX = 4;
/** Hover scale + animation duration — mirrors stock CSS
 *  `transition: all 0.2s ease; .emoji-grid span:hover { transform: scale(1.125) }`.
 *  Each cell tweens independently so quick mouse-overs don't snap. */
const PACK_CANVAS_HOVER_SCALE = 1.125;
const PACK_CANVAS_HOVER_MS = 200;
const PACK_CANVAS_TOOLTIP_DELAY_MS = 350;

// Whole-canvas redraw cap. Staggered per-cell frame flips would otherwise
// repaint the canvas on nearly every refresh tick (up to 120Hz on ProMotion),
// so paint cost tracks display refresh. Capping decouples it; banked time still
// advances frames at real speed, so we drop frames rather than slow animation.
// Imperceptible at thumb size. The slack absorbs rAF dt jitter so a 60Hz desktop
// isn't accidentally pushed down to 30fps by a frame landing just under budget.
const PACK_CANVAS_FPS_DESKTOP = 60;
const PACK_CANVAS_FPS_MOBILE = 30;
const PACK_CANVAS_FRAME_SLACK_MS = 4;

// ============================================================================
// Shared Vector tooltip (used by canvas cells, preview thumbs, inline emoji)
// ============================================================================
//
// One global tooltip element + show/hide/schedule helpers. Two ways in:
//   1. `data-emoji-tooltip="..."` on any element → handled automatically
//      via document-level mouseover/mouseout delegation
//   2. Direct API (`scheduleEmojiTooltipAt`) → used by the canvas, which
//      has no per-cell DOM, just coordinates
//
// All callers share the same tooltip node so simultaneously-hovered
// elements can't double up, and the styling stays consistent.

let _emojiTooltipTimer = null;
let _emojiTooltipCurrentAnchor = null;
let _emojiTooltipWatchdog = null;

// Ground truth for "still hovering" — mouseout never fires for an anchor that
// re-renders under the cursor or scrolls away beneath a stationary pointer
// (WKWebView synthesises no boundary events on scroll), which left the tooltip
// stuck on screen. Only anchored tooltips are watched; the pack canvas has no
// per-cell anchor and manages its own dismissal.
let _emojiTipPointerX = -1;
let _emojiTipPointerY = -1;
document.addEventListener('mousemove', (e) => {
    _emojiTipPointerX = e.clientX;
    _emojiTipPointerY = e.clientY;
}, { passive: true });

function _startEmojiTooltipWatchdog(anchor) {
    if (_emojiTooltipWatchdog) clearInterval(_emojiTooltipWatchdog);
    _emojiTooltipWatchdog = setInterval(() => {
        // Anchor changed or cleared since arming: this watchdog is stale.
        if (_emojiTooltipCurrentAnchor !== anchor) return;
        let alive = anchor.isConnected;
        if (alive && _emojiTipPointerX >= 0) {
            const r = anchor.getBoundingClientRect();
            alive = _emojiTipPointerX >= r.left - 2 && _emojiTipPointerX <= r.right + 2
                 && _emojiTipPointerY >= r.top - 2 && _emojiTipPointerY <= r.bottom + 2;
        }
        if (!alive) hideEmojiTooltip();
    }, 250);
}

/** Touch-only mobiles synthesise mouseover/mouseout on tap, which would
 *  flash the tooltip on every emoji selection — suppress on mobile.
 *  `platformFeatures.is_mobile` is Vector's canonical desktop/mobile split
 *  (set up in main.js at boot) and is used by every other hover-only path. */
function _supportsHoverTooltip() {
    return !!platformFeatures && !platformFeatures.is_mobile;
}



function showEmojiTooltipAt(text, x, y) {
    VectorSvelte.showPickerTip(text, x, y);
}

function hideEmojiTooltip() {
    if (_emojiTooltipWatchdog) {
        clearInterval(_emojiTooltipWatchdog);
        _emojiTooltipWatchdog = null;
    }
    if (_emojiTooltipTimer) {
        clearTimeout(_emojiTooltipTimer);
        _emojiTooltipTimer = null;
    }
    _emojiTooltipCurrentAnchor = null;
    VectorSvelte.hidePickerTip();
}

function scheduleEmojiTooltipAt(text, x, y, delay = PACK_CANVAS_TOOLTIP_DELAY_MS) {
    if (!_supportsHoverTooltip()) return;
    if (_emojiTooltipTimer) clearTimeout(_emojiTooltipTimer);
    _emojiTooltipTimer = setTimeout(() => {
        _emojiTooltipTimer = null;
        showEmojiTooltipAt(text, x, y);
    }, delay);
}

// Event-delegation path for any element carrying a `data-emoji-tooltip`
// attribute (preview thumbs, inline custom emoji <img>s, anything else
// that wants the same look). Skipped at handler time when mobile.
document.addEventListener('mouseover', (e) => {
    if (!_supportsHoverTooltip()) return;
    const el = e.target.closest && e.target.closest('[data-emoji-tooltip]');
    if (!el || el === _emojiTooltipCurrentAnchor) return;
    // Reaction chips own their own hover tip ("Right-click for details") — the
    // per-emoji shortcode tooltip would double up, so suppress it inside them.
    if (el.closest('.reaction')) return;
    _emojiTooltipCurrentAnchor = el;
    const text = el.dataset.emojiTooltip;
    if (!text) return;
    const rect = el.getBoundingClientRect();
    scheduleEmojiTooltipAt(text, rect.left + rect.width / 2, rect.top);
    _startEmojiTooltipWatchdog(el);
}, true);

document.addEventListener('mouseout', (e) => {
    if (!_supportsHoverTooltip()) return;
    const el = e.target.closest && e.target.closest('[data-emoji-tooltip]');
    if (!el) return;
    // mouseout fires when crossing into children too — only react when we
    // actually leave the tooltipped element.
    const related = e.relatedTarget;
    if (related && el.contains(related)) return;
    if (related && related.closest && related.closest('[data-emoji-tooltip]') === el) return;
    hideEmojiTooltip();
}, true);

// Mobile tap-tooltip — desktop uses hover, mobile users tap to see the
// shortcode. Any other interaction (tap elsewhere, scroll, swipe, key)
// dismisses. Bypasses the hover suppression check since mobile WebViews
// synthesize mouseover-on-tap, but the document-level click listener
// runs *after* those so a stale tooltip from a synthesized hover would
// just get re-shown here anyway.
function _isMobileTouchEnv() {
    return platformFeatures?.is_mobile;
}

document.addEventListener('click', (e) => {
    if (!_isMobileTouchEnv()) return;
    const el = e.target.closest && e.target.closest('[data-emoji-tooltip]');
    if (!el || el.closest('.reaction')) {
        // Tap landed elsewhere (or on a reaction, which owns its own tip) —
        // dismiss any active tooltip.
        if (_emojiTooltipCurrentAnchor) hideEmojiTooltip();
        return;
    }
    // Toggle: tapping the same emoji again hides; tapping a different
    // tooltipped emoji moves the tooltip to it.
    if (_emojiTooltipCurrentAnchor === el) {
        hideEmojiTooltip();
        return;
    }
    const text = el.dataset.emojiTooltip;
    if (!text) return;
    _emojiTooltipCurrentAnchor = el;
    const rect = el.getBoundingClientRect();
    showEmojiTooltipAt(text, rect.left + rect.width / 2, rect.top);
}, true);

// Any of these dismiss a mobile-tap tooltip — anything that "isn't
// looking at this emoji anymore" should drop it. Desktop hover takes
// care of its own dismissal via mouseout above.
document.addEventListener('touchmove', () => {
    if (_isMobileTouchEnv() && _emojiTooltipCurrentAnchor) hideEmojiTooltip();
}, { passive: true });
document.addEventListener('scroll', () => {
    if (_isMobileTouchEnv() && _emojiTooltipCurrentAnchor) hideEmojiTooltip();
}, true);
document.addEventListener('keydown', () => {
    if (_isMobileTouchEnv() && _emojiTooltipCurrentAnchor) hideEmojiTooltip();
});

/** url → Promise<{img: HTMLImageElement, frameCount, frameSize, durations: number[]} | null> */
const _packEmojiSheetCache = new Map();

/** path → Promise<HTMLImageElement|null>: a pack atlas is one file shared by every emoji
 *  in it, so it is fetched and decoded once however many strips point into it. Bounded
 *  by insertion order; a session that browses every pack still holds a few dozen. */
const _sheetImageCache = new Map();
const SHEET_IMAGE_CACHE_MAX = 256;

function _sheetImage(path) {
    if (_sheetImageCache.has(path)) return _sheetImageCache.get(path);
    const promise = (async () => {
        const img = new Image();
        img.src = convertFileSrc(path);
        // decode() blocks until the PNG is fully ready to draw — avoids
        // a flash of placeholder when the canvas calls drawImage().
        try {
            if (typeof img.decode === 'function') await img.decode();
            else await new Promise((res, rej) => { img.onload = res; img.onerror = rej; });
        } catch (_e) {}
        // A broken image throws inside drawImage and would take the shared loop down
        // with it: a file that failed to load is not cached, it is retried next time.
        if (!img.complete || !img.naturalWidth) {
            _sheetImageCache.delete(path);
            return null;
        }
        return img;
    })();
    if (_sheetImageCache.size >= SHEET_IMAGE_CACHE_MAX) {
        _sheetImageCache.delete(_sheetImageCache.keys().next().value);
    }
    _sheetImageCache.set(path, promise);
    return promise;
}

/** A sheet descriptor from the backend → the frames the canvas draws, or null when its
 *  file did not load (the grid compacts the cell away). The PNG is a file in the app
 *  cache (its own sheet, or a pack atlas the strip sits inside), so it loads through
 *  the asset route: no pixels in IPC, and the decode happens off the main thread. */
async function _sheetFrames(sheet) {
    if (!sheet || !sheet.path) return null;
    const img = await _sheetImage(sheet.path);
    if (!img) return null;
    return {
        img,
        x: sheet.x || 0,
        y: sheet.y || 0,
        frameCount: sheet.frame_count,
        frameSize: sheet.frame_size,
        durations: sheet.frame_durations_ms || [],
    };
}

/** Everything the on-disk sheet cache already holds for these urls, in one crossing.
 *  Misses stay unknown, so the per-emoji path fetches them one by one as they land. */
async function primePackEmojiSheets(urls) {
    const unknown = [...new Set(urls.filter(u => !_packEmojiSheetCache.has(u) && !_emojiFailReason.has(u)))];
    if (!unknown.length) return;
    let hits;
    try { hits = await invoke('cached_emoji_sheets', { urls: unknown }); }
    catch (e) { console.warn('[emoji-packs] cached sheets lookup failed:', e); return; }
    unknown.forEach((url, i) => {
        const sheet = Array.isArray(hits) ? hits[i] : null;
        if (sheet) _packEmojiSheetCache.set(url, _sheetFrames(sheet));
    });
}

async function decodePackEmojiFrames(url) {
    if (_packEmojiSheetCache.has(url)) return _packEmojiSheetCache.get(url);
    const promise = (async () => {
        try {
            return await _sheetFrames(await invoke('decode_animated_emoji', { url }));
        } catch (e) {
            console.warn('[emoji-packs] frame decode failed:', url, e);
            _emojiFailReason.set(url, String(e && e.message ? e.message : e));
            return null;
        }
    })();
    _packEmojiSheetCache.set(url, promise);
    return promise;
}

/** Forget every decoded sheet and atlas and make each pack grid ask again.
 *  After "Delete Cache" the files these point at are gone; without this the
 *  panel keeps drawing from memory and looks cached when nothing is. */
function resetPackEmojiSheets() {
    _packEmojiSheetCache.clear();
    _sheetImageCache.clear();
    for (const grid of _packCanvasGrids.values()) {
        grid.frames = new Array(grid.emojis.length).fill(undefined);
        grid.dirty.clear();
        for (let i = 0; i < grid.emojis.length; i++) grid.dirty.add(i);
        grid._framesRequested = false;
        grid._requestFrames();
    }
}

const _activeCanvasSections = new Set();
let _packCanvasRafHandle = null;
let _packCanvasLastTick = 0;

function _packCanvasTick(now) {
    const dt = _packCanvasLastTick ? Math.min(now - _packCanvasLastTick, 100) : 0;
    _packCanvasLastTick = now;
    let anyActive = false;
    for (const section of _activeCanvasSections) {
        // A preview card can be ripped from the DOM (message re-render) while
        // still in the active set, before its IO reports the removal — reap it
        // here so we don't tick a detached canvas or leak its observers.
        if (!section.canvas.isConnected) { section.destroy(); continue; }
        // One section's failure must not stop every other canvas in the app.
        try {
            if (section._advance(dt)) anyActive = true;
        } catch (e) {
            console.warn('[emoji-packs] section tick failed, dropping it:', e);
            section.destroy();
        }
    }
    // Pause when nothing needs animating (all visible packs static + idle).
    // Sections stay in the active set; hover / frame-load / re-intersect restart
    // the loop. Avoids a 60fps wakeup while the panel sits open on static packs.
    if (_activeCanvasSections.size === 0 || !anyActive) {
        _packCanvasRafHandle = null;
        _packCanvasLastTick = 0;
        return;
    }
    _packCanvasRafHandle = requestAnimationFrame(_packCanvasTick);
}

function _startPackCanvasLoop() {
    if (_packCanvasRafHandle) return;
    _packCanvasLastTick = 0;
    _packCanvasRafHandle = requestAnimationFrame(_packCanvasTick);
}

/** Drain the panel's sections from the loop — used when the picker closes so
 *  we don't keep ticking it under opacity:0. In-chat preview grids are left
 *  active (they animate independently of the panel); the loop only stops once
 *  nothing at all remains. */
function _stopPackCanvasLoop() {
    for (const s of _activeCanvasSections) {
        if (!s.isPreview) _activeCanvasSections.delete(s);
    }
    if (_activeCanvasSections.size === 0 && _packCanvasRafHandle) {
        cancelAnimationFrame(_packCanvasRafHandle);
        _packCanvasRafHandle = null;
    }
}

/** Re-arm the pack canvases currently on-screen in `.emoji-main` after a panel
 *  reopen: decode their frames (lazily — off-screen packs are left untouched)
 *  and resume the loop. Uses a direct geometry check instead of leaning on the
 *  IntersectionObserver, which doesn't reliably re-fire across the panel's
 *  hide/show (the close drains the active set, but the observed intersection
 *  state never changed). The IO still handles subsequent scrolling. */
function _rearmVisiblePackCanvases() {
    const main = _pickerEls.main;
    if (!main || _packCanvasGrids.size === 0) return;
    // A hidden panel's grids stay asleep: nothing decodes and nothing ticks until an open.
    if (!VectorSvelte.pickerVisible()) return;
    // main + each canvas share the panel's transform, so this viewport-space
    // overlap test stays correct even mid open-transition.
    const mainRect = main.getBoundingClientRect();
    let any = false;
    for (const grid of _packCanvasGrids.values()) {
        const r = grid.canvas.getBoundingClientRect();
        if (r.height > 0 && r.bottom > mainRect.top && r.top < mainRect.bottom) {
            grid._requestFrames();
            _activeCanvasSections.add(grid);
            any = true;
        }
    }
    if (any) _startPackCanvasLoop();
}

function _drawRoundedRect(ctx, x, y, w, h, r) {
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.lineTo(x + w - r, y);
    ctx.quadraticCurveTo(x + w, y, x + w, y + r);
    ctx.lineTo(x + w, y + h - r);
    ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
    ctx.lineTo(x + r, y + h);
    ctx.quadraticCurveTo(x, y + h, x, y + h - r);
    ctx.lineTo(x, y + r);
    ctx.quadraticCurveTo(x, y, x + r, y);
    ctx.closePath();
}

class PackCanvasGrid {
    // `opts` lets the same engine back two surfaces: the full emoji panel
    // (interactive, click-to-insert, hover-scale) and the in-chat pack
    // preview (decorative, capped thumb count, tooltip-only). Defaults
    // reproduce the panel's exact behaviour so existing call sites are
    // unchanged.
    constructor(pack, opts = {}) {
        this.pack = pack;
        // Exclude emoji already known to be unavailable (oversized / 404 / etc.) so they never take a
        // blank slot. First-time failures are dropped post-decode by _compact().
        this.emojis = (opts.emojis || pack.emojis).filter(e => !_emojiFailReason.has(e.url));
        // Fires after a compaction changed the row count: the section's height follows it.
        this.onRowsChange = opts.onRowsChange || null;
        this.cols = opts.cols || 6;
        this.rows = Math.ceil(this.emojis.length / this.cols) || 1;
        this.dpr = Math.min(window.devicePixelRatio || 1, 2);
        // Row stride + drawn-thumb size are configurable so the preview can
        // run a tighter grid (28px thumb in a 32px row) than the panel
        // (28px thumb in a 38px row).
        this.cellStride = opts.cellPx || PACK_CANVAS_CELL_PX;
        this.thumbPx = opts.thumbPx || PACK_CANVAS_THUMB_PX;
        this.gapPx = opts.gapPx != null ? opts.gapPx : PACK_CANVAS_GAP_PX;
        this.boxPx = opts.boxPx != null ? opts.boxPx : PACK_CANVAS_BOX_PX;
        // Behaviour flags. Panel: all on. Preview: tooltip only.
        this.hoverScale = opts.hoverScale !== false;
        this.hoverTooltip = opts.hoverTooltip !== false;
        this.selectable = opts.selectable !== false;
        this.isPreview = !!opts.isPreview;
        // IntersectionObserver config: panel roots on `.emoji-main`; preview
        // roots on the viewport with a margin so frames decode just before
        // a card scrolls into view.
        this._ioRoot = opts.ioRoot != null ? opts.ioRoot : null;
        this._ioMargin = opts.ioRootMargin || '0px';
        // Cell dimensions are computed from the parent's width at attach
        // time so the canvas matches the native grid's `repeat(N, 1fr)`
        // layout instead of bunching to a fixed width in the centre.
        this.cellW = this.cellStride;
        this.cellH = this.cellStride;

        const canvas = document.createElement('canvas');
        canvas.className = 'emoji-pack-canvas';
        canvas.dataset.packId = pack.id;
        canvas.style.display = 'block';
        canvas.style.width = '100%';
        // Unsized until the first measurement: the browser default (300x150) would pass
        // the no-op guard in _resize whenever the measured cell equals the initial guess.
        canvas.width = 0;
        canvas.height = 0;
        this.canvas = canvas;
        this.ctx = canvas.getContext('2d');

        this.frames = new Array(this.emojis.length);
        this.cellState = this.emojis.map(() => ({
            frame: 0,
            elapsed: 0,
            // Hover scale state — per-cell so concurrent enter/leave
            // animations on different cells coexist without snapping.
            scale: 1,
            scaleTarget: 1,
            scaleFrom: 1,
            scaleStart: 0,
        }));
        this.hoveredIndex = -1;
        this.dirty = new Set();
        this._io = null;
        this._ro = null;

        // Animation scheduler. Skip the per-cell scan on ticks where no frame is
        // due and no hover tween is running; `_nextDue` is ms until the soonest
        // frame flip across loaded animated cells (Infinity = nothing animated).
        // `_accumDt` banks elapsed time across skipped ticks so the eventual scan
        // advances by the real elapsed time. Starts at 0 so the first ticks scan
        // until frames load + settle.
        this._nextDue = 0;
        this._accumDt = 0;
        this._hasTween = false;
        // Min ms between whole-canvas redraws (60fps desktop, 30fps mobile).
        const isMobile = platformFeatures?.is_mobile;
        this._frameBudget = 1000 / (isMobile ? PACK_CANVAS_FPS_MOBILE : PACK_CANVAS_FPS_DESKTOP);

        // Tooltip drives the shared singleton (defined at module scope so
        // canvas cells, preview thumbs and inline custom emoji all share
        // one node + one show/hide timer).
        this._tooltipPendingIdx = -1;

        for (let i = 0; i < this.emojis.length; i++) this.dirty.add(i);

        this._installEvents();
        // Frames are decoded lazily on first visibility (see
        // attachVisibilityObserver) — an off-screen pack decodes nothing.
        this._framesRequested = false;
    }

    _resize() {
        const parent = this.canvas.parentElement;
        if (!parent) return;
        const cssWidth = parent.clientWidth;
        if (cssWidth <= 0) return;
        // Match the stock grid's column gap so canvas columns land at the
        // same x-coordinates as the native `repeat(N, 1fr)` layout.
        const cellW = (cssWidth - this.gapPx * (this.cols - 1)) / this.cols;
        // Row stride stays compact (matches the stock grid's vertical
        // rhythm) even when cells get wider — keeps the grid scannable.
        const cellH = this.cellStride;
        const cssHeight = cellH * this.rows;
        // `canvas.width > 0` guard: skip only genuine no-op resizes. Without it
        // a first measurement that happens to match the initial cellW/cellH
        // guess would return before the canvas is ever sized (stays 300×150).
        if (this.canvas.width > 0 && Math.abs(cellW - this.cellW) < 0.5 && cellH === this.cellH) return;
        this.canvas.style.height = cssHeight + 'px';
        this.canvas.width = Math.round(cssWidth * this.dpr);
        this.canvas.height = Math.round(cssHeight * this.dpr);
        this.cellW = cellW;
        this.cellH = cellH;
        // setTransform resets any prior scale (canvas resize clears the
        // transform too, but be explicit to avoid surprises).
        this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
        this.ctx.imageSmoothingEnabled = true;
        // 'low' is visually identical at a 28px thumb (≈1:1 at dpr 2) but far
        // cheaper than 'high' on the downscale path, which runs every drawn frame.
        this.ctx.imageSmoothingQuality = 'low';
        for (let i = 0; i < this.emojis.length; i++) this.dirty.add(i);
        this._render();
    }

    _installEvents() {
        // Hover tracking is needed for either the scale tween or the
        // tooltip; the preview wants only the latter, the panel both.
        if (this.hoverScale || this.hoverTooltip) {
            this.canvas.addEventListener('mousemove', (e) => {
                const idx = this._cellAtEvent(e);
                this._setHoverCell(idx);
                this.canvas.style.cursor = idx >= 0 && this.selectable ? 'pointer' : '';
            });
            this.canvas.addEventListener('mouseleave', () => {
                this._setHoverCell(-1);
                this.canvas.style.cursor = '';
            });
        }
        if (this.selectable) {
            this.canvas.addEventListener('click', (e) => {
                const idx = this._cellAtEvent(e);
                if (idx < 0) return;
                e.stopPropagation();
                _handlePackEmojiSelect(this.pack, this.emojis[idx], e.shiftKey);
            });
        }
    }

    _setHoverCell(idx) {
        if (idx === this.hoveredIndex) return;
        const now = performance.now();
        if (this.hoverScale) {
            if (this.hoveredIndex >= 0) {
                const prev = this.cellState[this.hoveredIndex];
                prev.scaleFrom = prev.scale;
                prev.scaleTarget = 1;
                prev.scaleStart = now;
                this.dirty.add(this.hoveredIndex);
            }
            if (idx >= 0) {
                const cur = this.cellState[idx];
                cur.scaleFrom = cur.scale;
                cur.scaleTarget = PACK_CANVAS_HOVER_SCALE;
                cur.scaleStart = now;
                this.dirty.add(idx);
            }
        }
        this.hoveredIndex = idx;
        this._scheduleTooltip(idx);
        // Only the scale tween needs per-frame ticks; a tooltip-only grid
        // (preview) must not wake the rAF loop just to track the cursor.
        if (this.hoverScale) {
            this._hasTween = true;
            _activeCanvasSections.add(this);
            _startPackCanvasLoop();
        }
    }

    _scheduleTooltip(idx) {
        if (!this.hoverTooltip) return;
        this._tooltipPendingIdx = idx;
        if (idx < 0) {
            hideEmojiTooltip();
            return;
        }
        const emoji = this.emojis[idx];
        if (!emoji) return;
        const rect = this.canvas.getBoundingClientRect();
        const col = idx % this.cols;
        const row = (idx / this.cols) | 0;
        const cx = rect.left + col * (this.cellW + this.gapPx) + this.cellW / 2;
        const cy = rect.top + row * this.cellH;
        scheduleEmojiTooltipAt(`:${emoji.dispCode || emoji.shortcode}:`, cx, cy);
    }

    _cellAtEvent(e) {
        const rect = this.canvas.getBoundingClientRect();
        const x = e.clientX - rect.left;
        const y = e.clientY - rect.top;
        const stride = this.cellW + this.gapPx;
        const col = Math.floor(x / stride);
        const row = Math.floor(y / this.cellH);
        if (col < 0 || col >= this.cols || row < 0 || row >= this.rows) return -1;
        // Reject clicks that landed in the gap itself rather than on a cell.
        if ((x - col * stride) > this.cellW) return -1;
        const idx = row * this.cols + col;
        return idx < this.emojis.length ? idx : -1;
    }

    // Decode this section's frames once, the first time it becomes visible. The panel's
    // grids are in the DOM from login on, so their observer fires while the panel is
    // still hidden; the open re-arms what is on screen, so nothing decodes before then.
    _requestFrames() {
        if (this._framesRequested) return;
        if (!this.isPreview && !VectorSvelte.pickerVisible()) return;
        this._framesRequested = true;
        this._loadFrames();
    }

    async _loadFrames() {
        // One call for what is already local, one call each for what is not.
        await primePackEmojiSheets(this.emojis.map(e => e.url));
        let pending = this.emojis.length;
        for (let i = 0; i < this.emojis.length; i++) {
            const url = this.emojis[i].url;
            const idx = i;
            decodePackEmojiFrames(url).then((sheet) => {
                this.frames[idx] = sheet || null;
                if (sheet) {
                    this.dirty.add(idx);
                    this._render();
                    // A newly-loaded animated emoji needs the scheduler to re-evaluate
                    // and the loop to resume if it had gone idle while static.
                    this._nextDue = 0;
                    if (_activeCanvasSections.has(this)) _startPackCanvasLoop();
                }
                // Once every frame has resolved, drop any that failed (oversized / 404 / etc.) so
                // they leave no blank slot — the grid reflows around them.
                if (--pending <= 0) this._compact();
            });
        }
    }

    /** Remove emoji whose frame failed to decode and compact the grid (no blank slots). */
    _compact() {
        const e = [], f = [], s = [];
        let failed = false;
        for (let i = 0; i < this.emojis.length; i++) {
            if (this.frames[i] === null) { failed = true; continue; }
            e.push(this.emojis[i]); f.push(this.frames[i]); s.push(this.cellState[i]);
        }
        if (!failed) return;
        this.emojis = e; this.frames = f; this.cellState = s;
        this.rows = Math.ceil(this.emojis.length / this.cols) || 1;
        if (this.onRowsChange) this.onRowsChange();
        this.hoveredIndex = -1;
        this.dirty.clear();
        for (let i = 0; i < this.emojis.length; i++) this.dirty.add(i);
        this.cellW = 0; // force _resize to recompute the canvas height for the new row count
        this._resize();
    }

    attachVisibilityObserver(root) {
        // Size against parent now that we're in the DOM, then keep
        // tracking width changes (window resize, picker layout shifts).
        this._resize();
        if (typeof ResizeObserver !== 'undefined' && this.canvas.parentElement) {
            this._ro = new ResizeObserver(() => this._resize());
            this._ro.observe(this.canvas.parentElement);
        }
        // Explicit arg wins (panel passes `.emoji-main`); otherwise fall back
        // to the configured root (preview: viewport).
        const ioRoot = root != null ? root : this._ioRoot;
        if (typeof IntersectionObserver === 'undefined') {
            this._requestFrames();   // no IO to gate on — decode now
            _activeCanvasSections.add(this);
            _startPackCanvasLoop();
            return;
        }
        this._io = new IntersectionObserver(entries => {
            for (const entry of entries) {
                if (entry.isIntersecting) {
                    this._requestFrames();   // lazy decode: only once the pack is actually on-screen
                    _activeCanvasSections.add(this);
                    _startPackCanvasLoop();
                } else {
                    _activeCanvasSections.delete(this);
                    // Preview cards live in the chat log and get torn out on
                    // message re-render; removal fires a non-intersecting entry,
                    // so reap the grid (disconnect observers) when its canvas
                    // has left the DOM.
                    if (this.isPreview && !entry.target.isConnected) this.destroy();
                }
            }
        }, { root: ioRoot || null, rootMargin: this._ioMargin, threshold: 0 });
        this._io.observe(this.canvas);
    }

    // Returns whether this section still needs ticking (animated frames pending,
    // a hover tween in flight, or frames still loading). The shared loop pauses
    // itself when every active section returns false.
    _advance(dt) {
        // Frames never asked for (the panel has not been opened yet) → nothing to tick.
        if (!this._framesRequested) return false;
        // Settled + nothing animated → no work until a hover / frame-load wakes us.
        if (!this._hasTween && this._nextDue === Infinity) return false;
        this._accumDt += Math.max(dt, 0);
        // Bank the time and wait until a redraw is actually warranted (loop stays
        // alive but this is O(1), not a full per-cell scan). The wait is the
        // greater of the next frame flip and the FPS budget, so the canvas never
        // repaints faster than the cap; a live hover tween redraws every budget
        // for smooth scaling. Slack absorbs rAF dt jitter at the budget boundary.
        const minWait = this._hasTween ? this._frameBudget : Math.max(this._nextDue, this._frameBudget);
        if (this._accumDt < minWait - PACK_CANVAS_FRAME_SLACK_MS) return true;

        const effDt = this._accumDt;   // banked elapsed time (per-tick dt already clamped upstream)
        this._accumDt = 0;
        const now = performance.now();
        const n = this.emojis.length;
        let nextDue = Infinity;
        let hasTween = false;

        for (let i = 0; i < n; i++) {
            const cell = this.cellState[i];
            const sheet = this.frames[i];

            if (sheet === undefined) {
                // Still loading — re-scan promptly so we start it the moment it lands.
                nextDue = 0;
            } else if (sheet && sheet.frameCount >= 2) {
                // Advance frames only when real time elapsed, but ALWAYS report
                // this animated cell's next-due time. A zero-dt tick (the first
                // tick after re-activation resets the clock) must not be read as
                // "nothing animated" — that stops the loop and freezes the grid,
                // which has no hover path to wake it back up.
                if (effDt > 0) {
                    cell.elapsed += effDt;
                    let dur = sheet.durations[cell.frame] || 100;
                    while (cell.elapsed >= dur) {
                        cell.elapsed -= dur;
                        cell.frame = (cell.frame + 1) % sheet.frameCount;
                        dur = sheet.durations[cell.frame] || 100;
                        this.dirty.add(i);
                    }
                }
                const dur = sheet.durations[cell.frame] || 100;
                const rem = dur - cell.elapsed;
                if (rem < nextDue) nextDue = rem;
            }

            // Hover scale tween. ease-out cubic gives the same "quick
            // start, soft settle" feel as CSS `ease` for short anims.
            if (cell.scale !== cell.scaleTarget) {
                const t = Math.min((now - cell.scaleStart) / PACK_CANVAS_HOVER_MS, 1);
                const eased = 1 - Math.pow(1 - t, 3);
                cell.scale = t >= 1
                    ? cell.scaleTarget
                    : cell.scaleFrom + (cell.scaleTarget - cell.scaleFrom) * eased;
                this.dirty.add(i);
                if (cell.scale !== cell.scaleTarget) hasTween = true;
            }
        }

        this._nextDue = nextDue;
        this._hasTween = hasTween;
        this._render();
        return hasTween || nextDue !== Infinity;
    }

    _render() {
        if (this.dirty.size === 0) return;
        const ctx = this.ctx;
        // Clamp the drawn thumb to the cell so a narrow column (small card)
        // can't overflow into its neighbour.
        const thumb = Math.min(this.thumbPx, this.cellW, this.cellH);
        const insetX = (this.cellW - thumb) / 2;
        const insetY = (this.cellH - thumb) / 2;
        // The hover box, sized so that even fully scaled it stays inside the row.
        const box = Math.min(this.boxPx, Math.floor(Math.min(this.cellW, this.cellH) / PACK_CANVAS_HOVER_SCALE));
        const stride = this.cellW + this.gapPx;
        for (const i of this.dirty) {
            const col = i % this.cols;
            const row = (i / this.cols) | 0;
            const x = col * stride;
            const y = row * this.cellH;
            const cell = this.cellState[i];
            const cx = x + this.cellW / 2;
            const cy = y + this.cellH / 2;

            // A cell owns exactly its rect: it clears nothing of its neighbours and the
            // clip means nothing it paints, scaled or not, can land on them either.
            ctx.save();
            ctx.beginPath();
            ctx.rect(x, y, stride, this.cellH);
            ctx.clip();
            ctx.clearRect(x, y, stride, this.cellH);

            // Scale around the cell centre so the thumb grows in place
            // instead of drifting toward a corner.
            if (cell.scale !== 1) {
                ctx.translate(cx, cy);
                ctx.scale(cell.scale, cell.scale);
                ctx.translate(-cx, -cy);
            }

            // Hover highlight — alpha eases in/out with the scale tween
            // (the bg-color side of stock's `transition: all`).
            const bgProgress = Math.max(0, Math.min(1,
                (cell.scale - 1) / (PACK_CANVAS_HOVER_SCALE - 1),
            ));
            if (bgProgress > 0.01 && box > 0) {
                const boxX = x + (this.cellW - box) / 2;
                const boxY = y + (this.cellH - box) / 2;
                ctx.fillStyle = `rgba(255, 255, 255, ${0.10 * bgProgress})`;
                _drawRoundedRect(ctx, boxX, boxY, box, box, 6);
                ctx.fill();
            }

            const sheet = this.frames[i];
            if (sheet && sheet.img) {
                // Source rect: vertical strip of frames, each `frameSize`
                // pixels tall, indexed by the current frame.
                const sy = sheet.y + cell.frame * sheet.frameSize;
                ctx.drawImage(
                    sheet.img,
                    sheet.x, sy, sheet.frameSize, sheet.frameSize,
                    x + insetX, y + insetY, thumb, thumb,
                );
            } else if (sheet === undefined) {
                // Loading placeholder — same visual weight as a filled cell
                // so the layout doesn't shift when frames resolve.
                ctx.fillStyle = 'rgba(255, 255, 255, 0.03)';
                _drawRoundedRect(ctx, x + insetX, y + insetY, thumb, thumb, 4);
                ctx.fill();
            }

            ctx.restore();
        }
        this.dirty.clear();
    }

    destroy() {
        _activeCanvasSections.delete(this);
        if (this._io) this._io.disconnect();
        if (this._ro) this._ro.disconnect();
        // The shared tooltip belongs to the module — only hide it if it
        // happens to be showing this section's cell. Don't remove the node.
        hideEmojiTooltip();
    }
}

/** Tracks every active canvas grid by pack address so re-renders can
 *  destroy obsolete observers. */
const _packCanvasGrids = new Map();
