<script>
    // A post the expanded one quotes: who, a few lines and a thumbnail. Opening it moves the
    // modal to it; what it quotes in turn stays a chip, so nesting stops at one level.
    import EmbedAuthor from './EmbedAuthor.svelte';
    import EmbedImage from './EmbedImage.svelte';
    let { ref, h } = $props();   // ref: { key, entity, url }; h: NostrEmbedHelpers

    let res = $state(null);
    // svelte-ignore state_referenced_locally
    h.resolve(ref.entity).then((r) => { res = r; });
    const e = $derived(res?.state === 'ok' ? res.embed : null);
    const veiled = $derived(e?.content_warning != null);
    const label = $derived({ post: 'Quoted Post', article: 'Quoted Article', video: 'Quoted Video', short: 'Quoted Short' }[e?.class] || 'Quote');
    const thumb = $derived(!e || veiled ? null
        : e.image || e.media.find((m) => !m.is_video)?.url || e.media.find((m) => m.poster)?.poster || null);

    function textInto(node, [text, emoji]) {
        const render = ([t, em]) => node.replaceChildren(h.buildText(t, em));
        render([text, emoji]);
        return { update: render };
    }
    function open(ev) {
        if (!e || ev.target.closest('a,.mention,.ne-chip')) return;
        h.push({ embed: e, url: ref.url, entity: ref.entity });
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="ne-quote" class:btn={!!e} class:is-invalid={res && !e} onclick={open}>
    <div class="ne-eyebrow">{e?.reposted_by ? 'Quoted Repost' : label}</div>
    {#if !res}
        <div class="ne-author"><span class="pack-skel ne-skel-avatar"></span><span class="pack-skel ne-skel-name"></span></div>
        <span class="pack-skel ne-skel-line"></span>
    {:else if !e}
        <div class="ne-error">{res.error}</div>
    {:else}
        <EmbedAuthor npub={e.author} at={e.published_at || e.created_at} {h} />
        <div class="ne-quote-body">
            <div class="ne-quote-main">
                {#if e.title}<div class="ne-quote-title">{e.title}</div>{/if}
                {#if veiled}
                    <div class="ne-quote-text ne-quote-cw">Content warning{e.content_warning ? `: ${e.content_warning}` : ''}</div>
                {:else if e.text}
                    <div class="ne-quote-text" use:textInto={[e.text.slice(0, 600), e.emoji]}></div>
                {/if}
            </div>
            {#if thumb}<EmbedImage url={thumb} cls="ne-quote-thumb" {h} />{/if}
        </div>
    {/if}
</div>
