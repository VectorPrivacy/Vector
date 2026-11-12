<script>
    // The edit-history panel: anchored above the bubble it was opened from (below when there
    // is no room), or a bottom sheet on a narrow screen. Closes on an outside click, the
    // scrim, the close button or Escape; the "edited" marker that opens it is exempt so a
    // click there never reopens.
    import { untrack } from 'svelte';
    import { cubicIn } from 'svelte/easing';
    import { editHistoryState } from '../lib/edithistory.svelte.js';
    import EditHistory from './EditHistory.svelte';
    import { tiling } from '../lib/tiling.js';
    let { h } = $props();   // h: renderEmoji(node, tags), hide()
    const st = editHistoryState();
    let popup = $state(null);
    let left = $state(0), top = $state(null), bottom = $state(null), maxH = $state(null), placed = $state(false), sheet = $state(false);

    // Placed by the room either side of the message, not by a measured height: the text
    // renders after the panel mounts, so it grows from its anchored edge and never past
    // the window.
    $effect(() => {
        const anchor = st.anchor;
        // A closing panel keeps its place while it leaves.
        if (!st.open) return;
        if (!anchor || !popup) { placed = false; return; }
        untrack(() => {
            sheet = window.matchMedia('(max-width: 640px)').matches;
            if (sheet) { top = bottom = maxH = null; placed = true; return; }
            const cap = Math.min(560, window.innerHeight * 0.7);
            // The window's title bar is above everything, so the panel stops short of it.
            const above = anchor.top - 44, below = window.innerHeight - anchor.bottom - 16;
            const up = above >= Math.min(cap, 320) || above >= below;
            st.below = !up;
            if (Math.max(above, below) < 260) {
                // No side has room: take the window's height, over the message.
                bottom = null; maxH = Math.min(cap, window.innerHeight - 60); top = Math.max(44, (window.innerHeight - maxH) / 2);
            } else if (up) { top = null; bottom = window.innerHeight - anchor.top + 6; maxH = Math.min(cap, above); }
            else { bottom = null; top = anchor.bottom + 6; maxH = Math.min(cap, below); }
            left = Math.max(10, Math.min(anchor.left, window.innerWidth - popup.offsetWidth - 10));
            placed = true;
        });
    });
    function onClick(e) {
        // A toggle inside re-renders, so its target can be gone from the page by now.
        if (!st.open || !e.target.isConnected || popup?.contains(e.target) || e.target.classList.contains('dmsg-edited')) return;
        h.hide();
    }
    function onKey(e) { if (st.open && e.key === 'Escape') h.hide(); }
    // The sheet slides back down; the panel fades.
    function leave() {
        return sheet
            ? { duration: 200, easing: cubicIn, css: (t) => `transform: translateY(${(1 - t) * 100}%)` }
            : { duration: 120, css: (t) => `opacity: ${t}` };
    }
    function scrimLeave() {
        return { duration: 200, css: (t) => `opacity: ${t}` };
    }
</script>

<svelte:window onclick={onClick} onkeydown={onKey} />

{#if st.open}
    {#if sheet}<div class="eh-scrim" out:scrimLeave|global></div>{/if}
    <div id="edit-history-popup" class="edit-history-popup" class:sheet bind:this={popup} out:leave role="dialog" aria-label="Edit history"
         style:left={sheet ? null : `${left}px`} style:top={top == null ? null : `${top}px`} style:bottom={bottom == null ? null : `${bottom}px`}
         style:max-height={maxH == null ? null : `${maxH}px`} style:visibility={placed ? 'visible' : 'hidden'}>
        <div class="eh-head">
            <span>Edit history</span>
            <button class="eh-close" aria-label="Close" onclick={(e) => { e.stopPropagation(); h.hide(); }}><span class="icon icon-x"></span></button>
        </div>
        <div class="edit-history-content" use:tiling>
            <EditHistory {h} />
        </div>
    </div>
{/if}
