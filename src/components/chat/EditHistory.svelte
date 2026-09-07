<script>
    // A message's revisions, oldest to newest. The popup's placement is measured by the
    // opener, which passes whether it sits below the bubble so the stagger runs toward it.
    import { editHistoryState } from '../lib/edithistory.svelte.js';
    let { h } = $props();
    const st = editHistoryState();
    const total = $derived(st.entries.length);
    function when(ts) {
        return new Date(ts).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
    }
    // Shortcodes and twemoji are imperative passes over the text node.
    function emojify(node, [text, tags]) {
        node.textContent = text;
        h.renderEmoji(node, tags);
    }
</script>

{#each st.entries as entry, i (i)}
    {@const original = i === 0}
    {@const current = i === total - 1}
    <div class="edit-history-entry" class:original class:current
         style:animation-delay="{st.below ? i * 50 : (total - 1 - i) * 50}ms">
        <div class="edit-history-time">{when(entry.edited_at)}{#if original}<span class="edit-history-label">Original</span>{:else if current}<span class="edit-history-label">Current</span>{/if}</div>
        <div class="edit-history-text" use:emojify={[entry.content, st.tags]}></div>
    </div>
{/each}
