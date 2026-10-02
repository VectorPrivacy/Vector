<script>
    // One referenced Nostr event: a skeleton while relays answer, then the post, article or
    // video. A long post, a long description or any article opens whole in the modal.
    import EmbedAuthor from './EmbedAuthor.svelte';
    import EmbedMedia from './EmbedMedia.svelte';
    import EmbedVideo from './EmbedVideo.svelte';
    import EmbedImage from './EmbedImage.svelte';
    import { profileVersion } from '../../lib/signals.svelte.js';
    let { ref, origin = null, h } = $props();   // ref: { key, entity, url }; origin: { chatId, msgId }; h: NostrEmbedHelpers

    let res = $state(null);
    let revealed = $state(false);
    let clamped = $state(false);
    // One resolve per card: cards are keyed by reference. A settled one fills without the pop.
    // svelte-ignore state_referenced_locally
    const animate = !h.settled(ref.key);
    // svelte-ignore state_referenced_locally
    h.resolve(ref.entity).then((r) => { res = r; requestAnimationFrame(() => h.onResized()); });

    const e = $derived(res?.state === 'ok' ? res.embed : null);
    const label = $derived({ post: 'Nostr Post', article: 'Nostr Article', video: 'Nostr Video', short: 'Nostr Short' }[e?.class] || 'Nostr');
    // NIP-36 makes the reason optional: an empty one still veils.
    const reposter = $derived.by(() => {
        if (!e?.reposted_by) return null;
        profileVersion(e.reposted_by);
        return h.author(e.reposted_by).name;
    });
    const veiled = $derived(e?.content_warning != null && !revealed);
    const refKind = $derived(ref.entity.toLowerCase().startsWith('naddr') ? 'naddr' : ref.entity.toLowerCase().startsWith('note') ? 'note ID' : 'nevent');

    function textInto(node, [text, emoji]) {
        const render = ([t, em]) => {
            node.replaceChildren(h.buildText(t, em));
            requestAnimationFrame(() => { clamped = node.scrollHeight > node.clientHeight + 2; });
        };
        render([text, emoji]);
        return { update: render };
    }
    function open(ev) {
        ev?.stopPropagation();
        if (e) h.open({ embed: e, url: ref.url, entity: ref.entity, origin });
    }
    function hostOf(url) {
        try { return new URL(url).host.replace(/^www\./, ''); } catch { return 'Open'; }
    }
    function external(ev) {
        ev.stopPropagation();
        h.openUrl(ref.url);
    }
</script>

<div class="nostr-embed" class:is-loading={!res} class:is-invalid={res && !e} class:ne-ready={e && animate}>
    <div class="ne-eyebrow">{reposter ? `Reposted by ${reposter}` : label}</div>
    {#if !res}
        <div class="ne-author"><span class="pack-skel ne-skel-avatar"></span><span class="pack-skel ne-skel-name"></span></div>
        <span class="pack-skel ne-skel-line"></span>
        <span class="pack-skel ne-skel-line ne-skel-short"></span>
    {:else if !e}
        <div class="ne-unavailable">This post couldn't be loaded</div>
        <div class="ne-error">{res.error}</div>
        <div class="ne-actions">
            <button type="button" class="ne-btn" onclick={(ev) => { ev.stopPropagation(); h.copy(ref.entity, `The ${refKind}`); }}>Copy {refKind}</button>
            {#if ref.url}<button type="button" class="ne-btn" onclick={external}>Open Externally</button>{/if}
        </div>
    {:else}
        <EmbedAuthor npub={e.author} at={e.published_at || e.created_at} {h} />
        {#if veiled}
            <button type="button" class="ne-veil" onclick={(ev) => { ev.stopPropagation(); revealed = true; h.onResized(); }}>
                <span class="ne-veil-title">Content warning{e.content_warning ? `: ${e.content_warning}` : ''}</span>
                <span class="ne-veil-action">Show</span>
            </button>
        {:else if e.class === 'article'}
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="ne-article btn" onclick={open}>
                {#if e.image}<EmbedImage url={e.image} cls="ne-article-cover" {h} onload={() => h.onResized()} />{/if}
                {#if e.title}<div class="ne-title">{e.title}</div>{/if}
                {#if e.summary}<div class="ne-summary">{e.summary}</div>{/if}
            </div>
        {:else}
            {#if e.video}
                <EmbedVideo media={e.video} poster={e.image} short={e.class === 'short'} title={e.title} {origin} {h} />
            {:else if e.link}
                <!-- No file to fetch, only a page (a YouTube link): the poster opens it outside the app. -->
                <div class="ne-video" class:is-portrait={e.class === 'short'} style:aspect-ratio={e.class === 'short' ? '9 / 16' : '16 / 9'}>
                    <button type="button" class="ne-video-poster" aria-label="Open video" onclick={(ev) => { ev.stopPropagation(); h.openUrl(e.link); }}>
                        {#if e.image}<EmbedImage url={e.image} cls="ne-poster-img" {h} />{/if}
                        <span class="ne-video-play"><span class="icon icon-play"></span></span>
                        <span class="ne-video-badge">{hostOf(e.link)}</span>
                    </button>
                </div>
            {/if}
            {#if e.title && e.class !== 'post'}<div class="ne-title">{e.title}</div>{/if}
            {#if e.text}
                <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                <div class="ne-text" class:is-clamped={clamped} class:is-small={e.class !== 'post'}
                     onclick={(ev) => { if (clamped && !ev.target.closest('a,.mention,.ne-chip')) open(ev); }}
                     use:textInto={[e.text.slice(0, 4000), e.emoji]}></div>
            {/if}
            {#if e.media.length}<EmbedMedia media={e.media} {origin} {h} />{/if}
        {/if}
        {#if !veiled && (e.class === 'article' || clamped || ref.url)}
            <div class="ne-actions">
                {#if e.class === 'article'}
                    <button type="button" class="ne-btn ne-btn-primary" onclick={open}>Read Article</button>
                {:else if clamped}
                    <button type="button" class="ne-btn" onclick={open}>Show More</button>
                {/if}
                {#if ref.url}<button type="button" class="ne-btn" onclick={external}>Open Externally</button>{/if}
            </div>
        {/if}
    {/if}
</div>
