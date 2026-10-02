<script>
    // A Nostr post, article or video in full, over the app. Copy hands out the event's own
    // reference; Open Externally appears only when the message linked a website. What it
    // quotes shows as quote cards; opening one moves here, and Back returns.
    import { nostrEmbedModal, nostrEmbedCanGoBack } from '../../lib/nostrembed.svelte.js';
    import { profileVersion } from '../../lib/signals.svelte.js';
    import QuoteCard from './QuoteCard.svelte';
    import EmbedImage from './EmbedImage.svelte';
    import { imageViewerState } from '../../lib/imageviewer.svelte.js';
    import EmbedAuthor from './EmbedAuthor.svelte';
    import EmbedMedia from './EmbedMedia.svelte';
    import EmbedVideo from './EmbedVideo.svelte';
    let { h } = $props();   // h: NostrEmbedHelpers (js/nostr-embeds.js)

    const v = $derived(nostrEmbedModal());
    const e = $derived(v?.embed);
    const refKind = $derived(e?.bech32?.startsWith('naddr') ? 'naddr' : 'nevent');
    const quotes = $derived(e?.text ? h.refs(e.text) : []);
    const reposter = $derived.by(() => {
        if (!e?.reposted_by) return null;
        profileVersion(e.reposted_by);
        return h.author(e.reposted_by).name;
    });
    // A quote opened here can carry a warning the card that led to it never showed.
    let revealedFor = $state(null);
    const veiled = $derived(e?.content_warning != null && revealedFor !== e);

    function body(node, embed) {
        // The references a quote card shows leave the text, as they do in a message.
        const render = (em) => {
            const text = h.withoutRefs(em.text);
            node.replaceChildren(em.class === 'article' ? h.buildArticle(text, em.emoji) : h.buildText(text, em.emoji));
        };
        render(embed);
        return { update: render };
    }
</script>

<!-- Capture phase, so the viewer is still open when this looks: a picture opened from the
     modal takes the Escape first. -->
<svelte:document onkeydowncapture={(ev) => { if (ev.key === 'Escape' && v && !imageViewerState().active) h.close(); }} />

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="pack-details-overlay nostr-embed-overlay" hidden={!v} onclick={(ev) => { if (ev.target === ev.currentTarget) h.close(); }}>
    {#if e}
        <div class="pack-details-card nostr-embed-modal" class:is-article={e.class === 'article'}>
            {#if nostrEmbedCanGoBack()}
                <button type="button" class="pack-details-close nem-back" aria-label="Back" onclick={() => h.back()}>
                    <span class="icon icon-chevron-left"></span>
                </button>
            {/if}
            <button type="button" class="pack-details-close" aria-label="Close" onclick={() => h.close()}>
                <span class="icon icon-x"></span>
            </button>
            {#key v}
            <div class="nem-scroll" class:has-back={nostrEmbedCanGoBack()}>
                {#if reposter}<div class="ne-eyebrow">Reposted by {reposter}</div>{/if}
                {#if veiled}
                    <EmbedAuthor npub={e.author} at={e.published_at || e.created_at} {h} />
                    <button type="button" class="ne-veil" onclick={() => { revealedFor = e; }}>
                        <span class="ne-veil-title">Content warning{e.content_warning ? `: ${e.content_warning}` : ''}</span>
                        <span class="ne-veil-action">Show</span>
                    </button>
                {:else}
                    {#if e.class === 'article' && e.image}<EmbedImage url={e.image} cls="nem-cover" preview {h} />{/if}
                    <EmbedAuthor npub={e.author} at={e.published_at || e.created_at} {h} />
                    {#if e.title}<h2 class="nem-title">{e.title}</h2>{/if}
                    {#if e.class === 'article' && e.summary}<div class="nem-summary">{e.summary}</div>{/if}
                    {#if e.video}<EmbedVideo media={e.video} poster={e.image} short={e.class === 'short'} title={e.title} origin={v.origin} {h} />{/if}
                    {#if e.text}<div class="nem-body" use:body={e}></div>{/if}
                    {#if e.media.length}<EmbedMedia media={e.media} origin={v.origin} {h} />{/if}
                    {#each quotes as ref (ref.key)}
                        <QuoteCard {ref} {h} />
                    {/each}
                {/if}
            </div>
            {/key}
            <div class="ne-actions nem-actions">
                <button type="button" class="ne-btn" onclick={() => h.copy(e.bech32, `The ${refKind}`)}>Copy {refKind}</button>
                {#if v.url}<button type="button" class="ne-btn" onclick={() => h.openUrl(v.url)}>Open Externally</button>{/if}
            </div>
        </div>
    {/if}
</div>
