<script>
    // What a comment answers (NIP-22): a chip opening the event, the page it's about, or its scope.
    let { reply, h } = $props();   // reply: the embed's reply_to; h: NostrEmbedHelpers

    const NOUNS = {
        1: 'a post', 1111: 'a comment', 30023: 'an article', 21: 'a video', 34235: 'a video',
        22: 'a short', 34236: 'a short', 1063: 'a file', 30311: 'a live stream', 34550: 'a community',
    };
    const noun = $derived(NOUNS[reply.kind] || 'a Nostr event');
    const web = $derived(/^https?:\/\//i.test(reply.external || '') ? reply.external : null);

    function host(url) {
        try { return new URL(url).host.replace(/^www\./, ''); } catch { return url; }
    }
</script>

<div class="ne-reply">
    Replying to
    {#if reply.entity}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span class="ne-chip btn" onclick={(ev) => { ev.stopPropagation(); h.openRef(reply.entity); }}>{noun}</span>
    {:else if web}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span class="ne-chip btn" onclick={(ev) => { ev.stopPropagation(); h.openUrl(web); }}>{host(web)}</span>
    {:else}
        <span class="ne-reply-scope">{reply.external || noun}</span>
    {/if}
</div>
