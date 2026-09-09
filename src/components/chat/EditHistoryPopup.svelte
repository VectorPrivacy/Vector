<script>
    // The edit-history popup: a message's revisions, placed above the bubble it was opened
    // from, or below when there is no room above. The popup measures itself once shown, so
    // the stagger of the entries knows which way it faces. Closes on an outside click or
    // Escape; the "edited" marker that opens it is exempt so a click there never reopens.
    import { untrack } from 'svelte';
    import { editHistoryState } from '../lib/edithistory.svelte.js';
    import EditHistory from './EditHistory.svelte';
    let { h } = $props();   // h: renderEmoji(node, tags), hide()
    const st = editHistoryState();
    let popup = $state(null);
    let content = $state(null);
    let left = $state(0), top = $state(0), placed = $state(false);

    $effect(() => {
        const anchor = st.anchor;
        if (!st.open || !anchor || !popup) { placed = false; return; }
        untrack(() => {
            placed = false;
            const { height, width } = popup.getBoundingClientRect();
            let y = anchor.top - height - 4;
            const below = y < 10;
            if (below) y = anchor.bottom + 4;
            st.below = below;
            left = Math.max(10, Math.min(anchor.left, window.innerWidth - width - 10));
            top = y;
            placed = true;
            // The latest revision sits at the bottom.
            content.scrollTop = content.scrollHeight;
        });
    });
    function onClick(e) {
        if (!st.open || popup?.contains(e.target) || e.target.classList.contains('dmsg-edited')) return;
        h.hide();
    }
    function onKey(e) { if (st.open && e.key === 'Escape') h.hide(); }
</script>

<svelte:window onclick={onClick} onkeydown={onKey} />

{#if st.open}
    <div id="edit-history-popup" class="edit-history-popup" bind:this={popup}
         style:left="{left}px" style:top="{top}px" style:visibility={placed ? 'visible' : 'hidden'}>
        <div class="edit-history-content" bind:this={content}>
            <EditHistory {h} />
        </div>
    </div>
{/if}
