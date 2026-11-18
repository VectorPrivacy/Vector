<script>
    // A dropdown drawn in-app rather than by the OS, so each option can carry a lead
    // (a logo, a swatch) and the list matches the rest of the UI. Arrow keys move the
    // highlight while it is open, Enter or Space picks, Escape or a click outside closes.
    // options: [{ value, label, tag?, disabled? }]; lead(option) draws what sits before a
    // label, and a tag is a small pill after it that never truncates.
    // label: the accessible name, when no <label> points at the button.
    import { tick } from 'svelte';

    let { options, value, onchange, lead = null, id = undefined, label = undefined, class: cls = '', disabled = false } = $props();

    const CAP = 320;   // .vselect-list's max-height

    let open = $state(false);
    let up = $state(false);
    let hi = $state(0);
    let root = $state(null);
    let list = $state(null);
    let room = $state(null);    // px the open list may take
    const current = $derived(options.find((o) => o.value === value) || options[0]);

    // The band of the window the list can be seen in: inside every ancestor that clips it,
    // so a scrolling pane under a header doesn't hide its top.
    function visibleBand() {
        let top = 0, bottom = window.innerHeight;
        for (let el = root.parentElement; el; el = el.parentElement) {
            if (getComputedStyle(el).overflowY === 'visible') continue;
            const r = el.getBoundingClientRect();
            top = Math.max(top, r.top);
            bottom = Math.min(bottom, r.bottom);
        }
        return { top, bottom };
    }
    // Opens downward unless it doesn't fit there and there is more room above, and is never
    // taller than the side it opens on; past that it scrolls.
    function place() {
        const box = root.getBoundingClientRect();
        const band = visibleBand();
        // The gap to the button, the list's own padding and border, and a margin from the edge.
        const below = band.bottom - box.bottom - 24;
        const above = box.top - band.top - 24;
        up = below < Math.min(list.scrollHeight, CAP) && above > below;
        room = Math.max(96, Math.min(CAP, up ? above : below));
    }
    async function toggle() {
        if (disabled) return;
        open = !open;
        if (!open) return;
        hi = Math.max(0, options.findIndex((o) => o.value === value));
        place();
        await tick();
        reveal();
    }
    // Scrolls the list, never the page around it, to the highlighted option.
    function reveal() {
        const item = list?.children[hi];
        if (!item) return;
        const top = item.offsetTop, bottom = top + item.offsetHeight;
        if (top < list.scrollTop) list.scrollTop = top - 4;
        else if (bottom > list.scrollTop + list.clientHeight) list.scrollTop = bottom - list.clientHeight + 4;
    }
    function pick(o) {
        if (o.disabled) return;
        open = false;
        if (o.value !== value) onchange(o.value);
    }
    function step(dir) {
        for (let n = 0; n < options.length; n++) {
            hi = (hi + dir + options.length) % options.length;
            if (!options[hi].disabled) { reveal(); return; }
        }
    }
    function onkey(e) {
        if (!open) {
            if (['Enter', ' ', 'ArrowDown', 'ArrowUp'].includes(e.key)) { e.preventDefault(); toggle(); }
            return;
        }
        if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); open = false; }
        else if (e.key === 'ArrowDown') { e.preventDefault(); step(1); }
        else if (e.key === 'ArrowUp') { e.preventDefault(); step(-1); }
        else if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pick(options[hi]); }
        else if (e.key === 'Tab') open = false;
    }
</script>

<svelte:window onmousedown={(e) => { if (open && root && !root.contains(e.target)) open = false; }} />

<div class="vselect {cls}" class:open class:up class:disabled bind:this={root}>
    <button type="button" class="vselect-btn" {id} {disabled} aria-label={label && `${label}, ${current?.label ?? ''}`} aria-haspopup="listbox" aria-expanded={open}
            onclick={toggle} onkeydown={onkey}>
        {#if lead}<span class="vselect-lead">{@render lead(current)}</span>{/if}
        <span class="vselect-label cutoff">{current?.label ?? ''}</span>
        {#if current?.tag}<span class="vselect-tag">{current.tag}</span>{/if}
        <span class="vselect-caret"></span>
    </button>
    <div class="vselect-list" role="listbox" bind:this={list} style:max-height={room ? `${room}px` : null}>
        {#each options as o, i (o.value)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="vselect-item" class:hi={i === hi && !o.disabled} class:selected={o.value === value} class:disabled={o.disabled}
                 role="option" tabindex="-1" aria-selected={o.value === value} aria-disabled={o.disabled}
                 onmouseenter={() => { if (!o.disabled) hi = i; }} onclick={() => pick(o)}>
                {#if lead}<span class="vselect-lead">{@render lead(o)}</span>{/if}
                <span class="cutoff">{o.label}</span>
                {#if o.tag}<span class="vselect-tag">{o.tag}</span>{/if}
                {#if o.value === value}<span class="icon icon-check"></span>{/if}
            </div>
        {/each}
    </div>
</div>
