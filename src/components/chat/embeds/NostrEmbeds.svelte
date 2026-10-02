<script>
    // The Nostr posts, articles and videos a message references, as cards (the first few, distinct).
    import NostrEmbedCard from './NostrEmbedCard.svelte';
    let { text, origin, h } = $props();   // origin: { chatId, msgId }; h: NostrEmbedHelpers (js/nostr-embeds.js)
    const refs = $derived(h.refs(text));
</script>

{#each refs as ref (ref.key)}
    <NostrEmbedCard {ref} {origin} {h} />
{/each}
