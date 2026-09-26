<script>
    // A dropdown drawn in-app rather than by the OS, so each option can carry a lead
    // (a logo, a swatch) and the list matches the rest of the UI. Arrow keys move the
    // highlight while it is open, Enter or Space picks, Escape or a click outside closes.
    // options: [{ value, label, disabled? }]; lead(option) draws what sits before a label.
    let { options, value, onchange, lead = null, id = undefined, class: cls = '', disabled = false } = $props();

    let open = $state(false);
    let up = $state(false);
    let hi = $state(0);
    let root = $state(null);
    let list = $state(null);
    const current = $derived(options.find((o) => o.value === value) || options[0]);

    // Opens downward unless the list would run past the window and there is more room above.
    function place() {
        const box = root.getBoundingClientRect();
        const need = list.scrollHeight + 12;
        up = window.innerHeight - box.bottom < need && box.top > window.innerHeight - box.bottom;
    }
    function toggle() {
        if (disabled) return;
        open = !open;
        if (!open) return;
        hi = Math.max(0, options.findIndex((o) => o.value === value));
        place();
    }
    function pick(o) {
        if (o.disabled) return;
        open = false;
        if (o.value !== value) onchange(o.value);
    }
    function step(dir) {
        for (let n = 0; n < options.length; n++) {
            hi = (hi + dir + options.length) % options.length;
            if (!options[hi].disabled) return;
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
    <button type="button" class="vselect-btn" {id} {disabled} aria-haspopup="listbox" aria-expanded={open}
            onclick={toggle} onkeydown={onkey}>
        {#if lead}<span class="vselect-lead">{@render lead(current)}</span>{/if}
        <span class="vselect-label cutoff">{current?.label ?? ''}</span>
        <span class="vselect-caret"></span>
    </button>
    <div class="vselect-list" role="listbox" bind:this={list}>
        {#each options as o, i (o.value)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="vselect-item" class:hi={i === hi && !o.disabled} class:selected={o.value === value} class:disabled={o.disabled}
                 role="option" tabindex="-1" aria-selected={o.value === value} aria-disabled={o.disabled}
                 onmouseenter={() => { if (!o.disabled) hi = i; }} onclick={() => pick(o)}>
                {#if lead}<span class="vselect-lead">{@render lead(o)}</span>{/if}
                <span class="cutoff">{o.label}</span>
                {#if o.value === value}<span class="icon icon-check"></span>{/if}
            </div>
        {/each}
    </div>
</div>
