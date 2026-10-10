<script>
    // The one image cropper: any aspect, above every overlay. The box holds the aspect through
    // every gesture (move it, drag a corner, or draw a fresh one on the image), and the
    // preview shows the crop where it will live.
    import { cropOverlay, closeCropper } from '../lib/cropper.svelte.js';
    import { popIn } from '../lib/popin.js';
    import BannerArt from '../community/BannerArt.svelte';
    import Avatar from './Avatar.svelte';

    const ov = cropOverlay.state();

    // Display px: below this the corner dots crowd out the middle a move grabs.
    const MIN_EDGE = 36;
    // The dots sit 7px outside the box; the image is fitted inside this margin so they stay in.
    const INSET = 10;
    // The banner template's top 50 of 146 px sit under the channel list's fade.
    const BANNER_FADE = 50 / 146;
    // The guide's note needs this much band to sit inside it; otherwise it goes outside the box.
    const NOTE_H = 22;
    const NOTE_W = 160;

    let stage = $state(null);
    let stageW = $state(0);
    let noteW = $state(0);
    let natural = $state({ w: 0, h: 0 });
    let img = $state({ left: 0, top: 0, width: 0, height: 0 });
    let crop = $state({ x: 0, y: 0, w: 0, h: 0 });
    let gesture = null;

    const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));
    const maxW = () => Math.min(img.width, img.height * ov.aspect);
    const minW = () => Math.min(maxW(), MIN_EDGE * Math.max(1, ov.aspect));

    // The crop in the shown image's pixels, with that image's size (`sw` x `sh`): a preview copy
    // may be scaled down, and the backend maps the rectangle onto the original.
    const rect = $derived.by(() => {
        if (!img.width || !natural.w) return null;
        const k = natural.w / img.width;
        const w = Math.max(1, Math.min(Math.round(crop.w * k), natural.w, Math.floor(natural.h * ov.aspect)));
        const h = Math.max(1, Math.round(w / ov.aspect));
        return {
            x: clamp(Math.round((crop.x - img.left) * k), 0, natural.w - w),
            y: clamp(Math.round((crop.y - img.top) * k), 0, natural.h - h),
            w,
            h,
            sw: natural.w,
            sh: natural.h,
        };
    });

    function onLoad(e) {
        const el = e.currentTarget;
        natural = { w: el.naturalWidth, h: el.naturalHeight };
        const sw = stage.clientWidth, sh = stage.clientHeight;
        const scale = Math.min((sw - INSET * 2) / natural.w, (sh - INSET * 2) / natural.h);
        const width = Math.round(natural.w * scale), height = Math.round(natural.h * scale);
        img = { left: Math.round((sw - width) / 2), top: Math.round((sh - height) / 2), width, height };
        const w = maxW();
        const h = w / ov.aspect;
        crop = { x: img.left + (width - w) / 2, y: img.top + (height - h) / 2, w, h };
    }

    function fit() {
        const w = clamp(crop.w, minW(), maxW());
        const h = w / ov.aspect;
        crop = {
            x: clamp(crop.x, img.left, img.left + img.width - w),
            y: clamp(crop.y, img.top, img.top + img.height - h),
            w,
            h,
        };
    }

    function begin(e, kind, extra) {
        if (gesture || !img.width) return;
        const r = stage.getBoundingClientRect();
        gesture = { id: e.pointerId, kind, ox: r.left, oy: r.top, ...extra };
        try { stage.setPointerCapture(e.pointerId); } catch {}
        e.preventDefault();
        e.stopPropagation();
    }

    function onStageDown(e) {
        if (e.target !== stage && !e.target.classList.contains('crop-img')) return;
        const r = stage.getBoundingClientRect();
        const px = e.clientX - r.left, py = e.clientY - r.top;
        if (px < img.left || px > img.left + img.width || py < img.top || py > img.top + img.height) return;
        begin(e, 'resize', { ax: px, ay: py });
    }

    function onCorner(e, corner) {
        begin(e, 'resize', {
            ax: corner[1] === 'l' ? crop.x + crop.w : crop.x,
            ay: corner[0] === 't' ? crop.y + crop.h : crop.y,
        });
    }

    function onMove(e) {
        if (!gesture || e.pointerId !== gesture.id) return;
        if (gesture.kind === 'move') {
            crop.x = clamp(gesture.cx + e.clientX - gesture.sx, img.left, img.left + img.width - crop.w);
            crop.y = clamp(gesture.cy + e.clientY - gesture.sy, img.top, img.top + img.height - crop.h);
            return;
        }
        const px = e.clientX - gesture.ox, py = e.clientY - gesture.oy;
        const { ax, ay } = gesture;
        const right = px >= ax, down = py >= ay;
        // The anchor stays put; the box grows toward the pointer only as far as the image allows.
        const roomW = right ? img.left + img.width - ax : ax - img.left;
        const roomH = down ? img.top + img.height - ay : ay - img.top;
        const w = Math.max(minW(), Math.min(Math.max(Math.abs(px - ax), Math.abs(py - ay) * ov.aspect), roomW, roomH * ov.aspect));
        const h = w / ov.aspect;
        crop = { x: right ? ax : ax - w, y: down ? ay : ay - h, w, h };
        fit();
    }

    function onUp(e) {
        if (!gesture || e.pointerId !== gesture.id) return;
        try { stage.releasePointerCapture(e.pointerId); } catch {}
        gesture = null;
    }

    const confirm = () => rect && closeCropper(rect);

    const noteInside = $derived(crop.h * BANNER_FADE >= NOTE_H && crop.w >= NOTE_W);
    const noteAbove = $derived(crop.y - NOTE_H >= 0);
    // Centred on the box, but never past the stage's sides.
    const noteLeft = $derived(clamp(crop.x + crop.w / 2 - noteW / 2, 4, Math.max(4, stageW - noteW - 4)));

    // The pane a member sees: the header, the channels, and the banner over their foot.
    const pane = $derived(ov.context || { width: 240, sections: [] });
    const paneW = $derived(Math.round(pane.width || 240));
    const paneBannerH = $derived(Math.round(paneW * 146 / 238));
    const sections = $derived(pane.sections?.length
        ? pane.sections
        : [{ label: 'Public', rows: [{ name: 'general', tier: 'has-unread' }, { name: 'announcements', tier: 'is-read' }] }]);

    // Rows shown per section: only those that end above the banner, and a section with none
    // drops its header too. Measured with everything rendered, then cut, once per pane.
    let listEl = $state(null);
    let rowsShown = $state(null);
    $effect(() => {
        void sections;
        void paneW;
        rowsShown = null;
    });
    $effect(() => {
        if (rowsShown || !listEl || ov.preview !== 'banner' || !listEl.clientHeight) return;
        // Layout offsets, not screen rects: the card's pop-in scales the rects mid-measure.
        const top = (el) => {
            let y = 0;
            for (let n = el; n && n !== listEl; n = n.offsetParent) y += n.offsetTop;
            return y;
        };
        const limit = listEl.clientHeight - paneBannerH - 4;
        const out = [];
        let room = true;
        for (const section of listEl.querySelectorAll('.chatlist-channel-section')) {
            let shown = 0;
            for (const row of room ? section.querySelectorAll('.chatlist-channel') : []) {
                if (top(row) + row.offsetHeight > limit) {
                    room = false;
                    break;
                }
                shown++;
            }
            out.push(shown);
        }
        rowsShown = out;
    });

    // Capture phase, stopped: the picker and Community Settings each close on their own Escape.
    $effect(() => {
        if (!ov.active || ov.closing) return;
        const onKey = (e) => {
            if (e.key === 'Escape') closeCropper(null);
            else if (e.key === 'Enter') confirm();
            else return;
            e.preventDefault();
            e.stopPropagation();
        };
        document.addEventListener('keydown', onKey, true);
        return () => document.removeEventListener('keydown', onKey, true);
    });

    // A fresh image re-measures from scratch.
    $effect(() => {
        void ov.src;
        natural = { w: 0, h: 0 };
        img = { left: 0, top: 0, width: 0, height: 0 };
        gesture = null;
    });

    const chipStyle = (size) => {
        if (!crop.w || !img.width) return '';
        const k = size / crop.w;
        return `background-image: url("${ov.src}"); background-size: ${img.width * k}px ${img.height * k}px; background-position: ${(img.left - crop.x) * k}px ${(img.top - crop.y) * k}px;`;
    };
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="crop-overlay" class:active={ov.active} class:closing={ov.closing}
     onclick={(e) => { if (e.target === e.currentTarget) closeCropper(null); }}>
    <div class="crop-card" class:banner={ov.preview === 'banner'} use:popIn={ov.tick}>
        <header class="crop-head">
            <h3 class="crop-title">{ov.title}</h3>
            {#if ov.hint}<p class="crop-hint">{ov.hint}</p>{/if}
        </header>

        <div class="crop-body">
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div class="crop-stage" bind:this={stage} bind:clientWidth={stageW}
                 onpointerdown={onStageDown} onpointermove={onMove} onpointerup={onUp} onpointercancel={onUp}>
                {#if ov.src}
                    <img class="crop-img" src={ov.src} alt="" draggable="false" onload={onLoad}
                         style="left: {img.left}px; top: {img.top}px; width: {img.width}px; height: {img.height}px;"
                         class:ready={img.width > 0}>
                {/if}
                {#if img.width}
                    <div class="crop-box" class:round={!!ov.shape} style="left: {crop.x}px; top: {crop.y}px; width: {crop.w}px; height: {crop.h}px;"
                         onpointerdown={(e) => begin(e, 'move', { sx: e.clientX, sy: e.clientY, cx: crop.x, cy: crop.y })}>
                        {#if ov.preview === 'banner'}
                            <div class="crop-guide" style="height: {BANNER_FADE * 100}%">
                                {#if noteInside}<span>Fades under the channel list</span>{/if}
                            </div>
                        {/if}
                        {#each ['tl', 'tr', 'bl', 'br'] as corner (corner)}
                            <div class="crop-handle crop-handle-{corner}" onpointerdown={(e) => onCorner(e, corner)}></div>
                        {/each}
                    </div>
                {/if}
                {#if img.width && ov.preview === 'banner' && !noteInside}
                    <div class="crop-note" class:above={noteAbove} bind:clientWidth={noteW}
                         style="left: {noteLeft}px; top: {noteAbove ? crop.y - 4 : crop.y + crop.h + 4}px;">
                        The top band fades under the channel list
                    </div>
                {/if}
            </div>

            <aside class="crop-preview" aria-hidden="true">
                <span class="crop-preview-label">Preview</span>
                {#if ov.preview === 'banner'}
                    <!-- The live pane's own classes, so the preview IS how it looks. -->
                    <div class="crop-pane" style="width: {paneW}px; height: {paneBannerH + 210}px; background: {pane.background || '#000'};">
                        <div class="crop-pane-surface">
                            <div class="chatlist-community-head">
                                <Avatar src={pane.iconSrc} size={36} group={true} class="chatlist-community-head-avatar" />
                                <div class="chatlist-community-head-meta">
                                    <span class="chatlist-community-head-name cutoff">{pane.name || 'Community'}</span>
                                    {#if pane.members}
                                        <span class="chatlist-community-head-members">
                                            <span class="icon icon-users-multi chatlist-community-head-members-icon"></span>
                                            <span>{pane.members}</span>
                                        </span>
                                    {/if}
                                </div>
                                <div class="chatlist-community-head-caret"><span class="icon icon-chevron-down"></span></div>
                            </div>
                            <div class="crop-pane-list" bind:this={listEl}>
                                <div class="chatlist-channels chatlist-channels-pane">
                                    {#each sections as section, s (section.label)}
                                        {#if !rowsShown || rowsShown[s] > 0}
                                        <div class="chatlist-channel-section">
                                            <div class="chatlist-channel-section-head">
                                                <div class="chatlist-channel-section-toggle">
                                                    <span class="chatlist-channel-section-label">{section.label}</span>
                                                    <span class="chatlist-channel-section-caret"><span class="icon icon-chevron-down"></span></span>
                                                </div>
                                            </div>
                                            <div class="chatlist-channel-section-body">
                                                {#each rowsShown ? section.rows.slice(0, rowsShown[s]) : section.rows as row, i (i)}
                                                    <div class="chatlist-channel {row.tier}">
                                                        <span class="chatlist-channel-hash"><span class="icon icon-channel-hash"></span></span>
                                                        <span class="chatlist-channel-name cutoff">{row.name}</span>
                                                    </div>
                                                {/each}
                                            </div>
                                        </div>
                                        {/if}
                                    {/each}
                                </div>
                            </div>
                            <BannerArt src={ov.src} {rect} {natural} class="crop-pane-banner" />
                        </div>
                    </div>
                {:else}
                    <div class="crop-chips">
                        <div class="crop-chip" class:round={!!ov.shape} style="width: 96px; height: 96px; {chipStyle(96)}"></div>
                        <div class="crop-chip" class:round={ov.shape === 'round'} class:rail={ov.shape === 'community'} style="width: 48px; height: 48px; {chipStyle(48)}"></div>
                        <div class="crop-chip" class:round={!!ov.shape} style="width: 24px; height: 24px; {chipStyle(24)}"></div>
                    </div>
                {/if}
            </aside>
        </div>

        <footer class="crop-actions">
            <button type="button" class="crop-btn" onclick={() => closeCropper(null)}>Cancel</button>
            <button type="button" class="crop-btn primary" disabled={!rect} onclick={confirm}>Crop</button>
        </footer>
    </div>
</div>
