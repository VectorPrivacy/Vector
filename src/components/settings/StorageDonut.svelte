<script>
    // The Storage breakdown: summary line, donut, centre readout and legend, all
    // derived from the distribution the backend last reported. Hover previews a
    // slice; click sticky-selects it, which reveals Delete in the hole.
    import { storageState } from '../lib/settings.svelte.js';

    let { h } = $props();   // h: formatBytes, confirmDelete(cat, sizeText), deleteCategory(category, exts), refresh, onCacheCleared, toast, deleteFailed

    const CATEGORIES = [
        { name: 'Images', exts: ['jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'svg', 'avif', 'heic', 'heif', 'tif', 'tiff', 'ico'], title: 'Delete all Images?', noun: 'downloaded images' },
        { name: 'Video', exts: ['mp4', 'mov', 'avi', 'mkv', 'flv', 'wmv', '3gp', 'webm', 'm4v', 'mpeg', 'mpg'], title: 'Delete all Videos?', noun: 'downloaded videos' },
        { name: 'Audio', exts: ['mp3', 'wav', 'ogg', 'oga', 'opus', 'flac', 'm4a', 'aac', 'weba', 'wma', 'aiff'], title: 'Delete all Audio?', noun: 'downloaded audio and voice messages' },
        { name: 'Apps', exts: ['xdc', 'jsdos'], title: 'Delete all Mini Apps?', noun: 'downloaded Mini Apps' },
        { name: 'AI', key: '/ai_models', title: 'Delete AI Models?' },
        { name: 'Cache', key: '/cache', title: 'Clear the Cache?' },
        { name: 'Files', rest: true, title: 'Delete all Files?', noun: 'downloaded files' },
    ];
    // Slice colours rank by size, not category: the largest is always purple.
    const RAMP = ['#9D5DF9', '#5EC4F7', '#4AD99D', '#FBA35B', '#FBC85B', '#FC595C', '#B2B2B2'];
    const CX = 100, CY = 100, R_OUT = 96, R_IN = 57, GAP_PX = 5;
    // Slivers get a floor so they stay visible and tappable, paid for by the largest
    // slice. Must exceed GAP_PX / R_IN or the inner arc inverts.
    const MIN_SWEEP = 0.13;

    const storage = storageState();
    const extOwner = new Map();
    for (const c of CATEGORIES) if (c.exts) for (const e of c.exts) extOwner.set(e, c.name);
    const restName = CATEGORIES.find(c => c.rest).name;

    const segments = $derived.by(() => {
        const sizes = new Map(CATEGORIES.map(c => [c.name, 0]));
        for (const [key, bytes] of Object.entries(storage.distribution || {})) {
            const special = CATEGORIES.find(c => c.key === key);
            const owner = special ? special.name : (extOwner.get(key) || restName);
            sizes.set(owner, sizes.get(owner) + bytes);
        }
        const segs = CATEGORIES.map(c => ({ name: c.name, size: sizes.get(c.name) }))
            .filter(s => s.size > 0)
            .sort((a, b) => b.size - a.size)
            .map((s, i) => ({ ...s, color: RAMP[Math.min(i, RAMP.length - 1)] }));
        const total = segs.reduce((sum, s) => sum + s.size, 0);
        // Angles span gap centre-line to centre-line; the gap is carved in slicePath.
        let stolen = 0;
        const sweeps = segs.map(s => {
            const a = Math.PI * 2 * (s.size / total);
            if (a < MIN_SWEEP) { stolen += MIN_SWEEP - a; return MIN_SWEEP; }
            return a;
        });
        if (sweeps.length) sweeps[0] -= stolen;
        let angle = 0;
        return segs.map((s, i) => {
            const a0 = angle;
            angle += sweeps[i];
            return { ...s, a0, a1: angle, pct: (s.size / total) * 100 };
        });
    });
    const total = $derived(segments.reduce((sum, s) => sum + s.size, 0));

    let hovered = $state(-1);
    let selected = $state(-1);
    let busy = $state(false);
    // A new report resets the selection; the chart it pointed at is gone.
    $effect(() => { storage.seq; hovered = -1; selected = -1; });

    const view = $derived(hovered !== -1 ? hovered : selected);
    const showDelete = $derived(selected !== -1 && view === selected);
    const centre = $derived.by(() => {
        if (view === -1) return { value: h.formatBytes(total, 1), label: 'Total' };
        const s = segments[view];
        return { value: h.formatBytes(s.size, 1), label: `${s.name} · ${s.pct < 1 ? '<1' : Math.round(s.pct)}%` };
    });
    const summary = $derived(total === 0 ? "A breakdown of Vector's storage use." : `Total Storage Used: ${h.formatBytes(total, 1)}`);

    /** Annular sector between gap centre-lines a0..a1 (radians from 12 o'clock, clockwise).
     *  Each edge is inset by (gap/2)/r so the gap keeps a constant linear width. */
    function slicePath(a0, a1) {
        const pt = (r, a) => `${(CX + r * Math.sin(a)).toFixed(2)} ${(CY - r * Math.cos(a)).toFixed(2)}`;
        const gOut = (GAP_PX / 2) / R_OUT;
        const gIn = (GAP_PX / 2) / R_IN;
        const largeOut = (a1 - a0 - 2 * gOut) > Math.PI ? 1 : 0;
        const largeIn = (a1 - a0 - 2 * gIn) > Math.PI ? 1 : 0;
        return `M ${pt(R_OUT, a0 + gOut)} A ${R_OUT} ${R_OUT} 0 ${largeOut} 1 ${pt(R_OUT, a1 - gOut)} ` +
               `L ${pt(R_IN, a1 - gIn)} A ${R_IN} ${R_IN} 0 ${largeIn} 0 ${pt(R_IN, a0 + gIn)} Z`;
    }

    // Hit-test the whole ring by angle instead of per-path events: the gaps then
    // belong to their nearest slice, so crossing a gap never flashes the idle view.
    let svg;
    function sliceAt(e) {
        const rect = svg.getBoundingClientRect();
        if (!rect.width) return -1;
        const vx = (e.clientX - rect.left) * (200 / rect.width) - CX;
        const vy = (e.clientY - rect.top) * (200 / rect.height) - CY;
        const dist = Math.hypot(vx, vy);
        if (dist < R_IN - 2 || dist > R_OUT + 6) return -1;
        let a = Math.atan2(vx, -vy);
        if (a < 0) a += Math.PI * 2;
        let idx = 0;
        for (let i = 0; i < segments.length; i++) if (a >= segments[i].a0) idx = i;
        return idx;
    }
    function select(i) { selected = selected === i ? -1 : i; }

    async function del() {
        if (selected === -1 || busy) return;
        const seg = segments[selected];
        const cat = CATEGORIES.find(c => c.name === seg.name);
        if (!(await h.confirmDelete(cat, h.formatBytes(seg.size, 1)))) return;
        let category = 'files';
        let exts = [];
        if (cat.key === '/ai_models') category = 'ai';
        else if (cat.key === '/cache') category = 'cache';
        else if (cat.exts) exts = cat.exts;
        else {
            // Rest bucket: the exact extensions the slice counted, from the live report.
            const categorized = new Set();
            for (const c of CATEGORIES) {
                if (c.exts) c.exts.forEach(e => categorized.add(e));
                if (c.key) categorized.add(c.key);
            }
            exts = Object.keys(storage.distribution || {}).filter(k => !categorized.has(k));
        }
        busy = true;
        try {
            const res = await h.deleteCategory(category, exts);
            h.toast(`Freed ${res.freed_formatted}`);
            if (category === 'cache') h.onCacheCleared();
        } catch (e) {
            await h.deleteFailed(e);
        }
        busy = false;
        h.refresh();
    }
</script>

<div class="form-group">
    <p id="storage-summary">{summary}</p>
</div>
<div class="form-group">
    <div id="storage-donut-wrap">
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
        <svg id="storage-donut" viewBox="0 0 200 200" aria-hidden="true" bind:this={svg}
             style:cursor={hovered === -1 ? '' : 'pointer'}
             onmousemove={(e) => { hovered = sliceAt(e); }}
             onmouseleave={() => { hovered = -1; }}
             onclick={(e) => { const i = sliceAt(e); if (i !== -1) select(i); }}>
            {#key storage.seq}
                {#if total === 0}
                    <circle cx={CX} cy={CY} r={(R_OUT + R_IN) / 2} fill="none" stroke="rgba(255, 255, 255, 0.07)" stroke-width={R_OUT - R_IN} />
                {:else if segments.length === 1}
                    <!-- A lone category is a full ring; the arc path degenerates at 360 degrees -->
                    <circle class="storage-slice" class:pop={view === 0} cx={CX} cy={CY} r={(R_OUT + R_IN) / 2} fill="none" stroke={segments[0].color} stroke-width={R_OUT - R_IN} />
                {:else}
                    {#each segments as s, i (s.name)}
                        <path class="storage-slice" class:pop={view === i} class:dim={view !== -1 && view !== i}
                              d={slicePath(s.a0, s.a1)} fill={s.color} style:animation-delay="{i * 55}ms" />
                    {/each}
                {/if}
            {/key}
        </svg>
        <div id="storage-donut-center">
            <span id="storage-donut-value">{centre.value}</span>
            <!-- The button swaps in for the name line; three stacked rows don't fit the hole -->
            <span id="storage-donut-label" hidden={showDelete}>{centre.label}</span>
            <button id="storage-donut-delete" hidden={!showDelete} disabled={busy} onclick={del}>{busy ? 'Deleting...' : 'Delete'}</button>
        </div>
    </div>
    <div id="storage-legend">
        {#each segments as s, i (s.name)}
            <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
            <div class="storage-legend-item" class:dim={view !== -1 && view !== i}
                 onmouseenter={() => { hovered = i; }} onmouseleave={() => { hovered = -1; }} onclick={() => select(i)}>
                <span class="storage-legend-swatch" style:background-color={s.color}></span>
                <span>{s.name}</span>
            </div>
        {/each}
    </div>
</div>
