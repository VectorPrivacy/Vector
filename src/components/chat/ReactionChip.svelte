<script>
    // One reaction chip: the glyph (a pack image through the cached-image binder, else the
    // twemojified character), the count in its clipped roller, and the pop-in for a chip
    // that lands after the row's first paint.
    //
    // h: customEmojiUrl(emoji, url), bindCachedImg(img, url, onUnavailable), twemojify(el),
    //    reducedMotion(), reactionClick(msgId, emoji), reactionChipRemoved()
    let { msgId, group, painted = false, h } = $props();

    // Anything that is not a resolvable custom emoji gets one uniform cap by code point,
    // surrogate-safe, on the DISPLAY only; data-emoji keeps the full value for the toggle.
    const GLYPH_CAP = 16;
    const customUrl = $derived(h.customEmojiUrl(group.emoji, group.url));
    const shown = $derived.by(() => {
        const cps = Array.from(group.emoji);
        return cps.length > GLYPH_CAP ? cps.slice(0, GLYPH_CAP).join('') + '…' : group.emoji;
    });

    function glyphInto(node, text) {
        node.textContent = text;
        h.twemojify(node);
    }
    // A deleted or 404 emoji becomes a twemojified question mark, so the chip stays a glyph.
    function customInto(img, url) {
        h.bindCachedImg(img, url, (el) => {
            const holder = el.parentElement;
            el.replaceWith(document.createTextNode('❓'));
            if (holder) h.twemojify(holder);
        });
    }

    // The count rolls on change: the old number slides out (up on an increment, down on a
    // decrement) while the new slides in from the opposite edge. Transform and opacity
    // only, so it composites on the GPU.
    let countEl = $state(null);
    let valEl = $state(null);
    // svelte-ignore state_referenced_locally
    let shownCount = $state(group.count);   // the first count; the effect below follows the rest
    $effect(() => {
        const to = group.count;
        const from = shownCount;
        if (to === from) return;
        if (!countEl || !valEl || h.reducedMotion() || !valEl.animate) { shownCount = to; return; }
        // Drop any still-animating leftover so rapid updates cannot pile up ghosts.
        for (const o of countEl.querySelectorAll('.rc-old')) o.remove();
        const up = to > from;
        const old = valEl.cloneNode(true);
        old.classList.add('rc-old');
        countEl.appendChild(old);
        shownCount = to;
        const duration = 340;
        const easing = 'cubic-bezier(0.22, 1, 0.36, 1)';
        const outAnim = old.animate(
            [{ transform: 'translateY(0)', opacity: 1 }, { transform: `translateY(${up ? '-100%' : '100%'})`, opacity: 0 }],
            { duration, easing });
        outAnim.onfinish = outAnim.oncancel = () => old.remove();
        valEl.animate(
            [{ transform: `translateY(${up ? '100%' : '-100%'})`, opacity: 0 }, { transform: 'translateY(0)', opacity: 1 }],
            { duration, easing });
    });

    // A hover tip anchored to a chip that just left would float forever.
    $effect(() => () => h.reactionChipRemoved());

    // svelte-ignore state_referenced_locally
    let entering = $state(painted);   // whether this chip arrived after its row: read once, on purpose
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<span
    class="reaction"
    class:reaction-enter={entering}
    data-emoji={group.emoji}
    data-msg-id={msgId}
    data-reacted={group.mine ? 'true' : undefined}
    title={group.mine ? 'Click to remove your reaction' : undefined}
    onanimationend={() => { entering = false; }}
    onclick={() => h.reactionClick(msgId, group.emoji)}
>
    {#if customUrl}
        <img alt={group.emoji} class="reaction-custom-emoji" use:customInto={customUrl}>
    {:else}
        <span class="reaction-glyph" use:glyphInto={shown}></span>
    {/if}
    <span class="reaction-count" bind:this={countEl}><span class="rc-value" bind:this={valEl}>{shownCount}</span></span>
</span>
