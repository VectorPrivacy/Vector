<script>
    // Community invite links in a message body, as cards (the first few, distinct).
    import InviteCard from './InviteCard.svelte';
    let { text, h } = $props();
    const keys = $derived(h.inviteKeys(text));
</script>

{#each keys as inviteKey (inviteKey)}
    <InviteCard {inviteKey} {h} />
{/each}
