<script>
    // The quoted parent above a reply: avatar, name and a one-line snippet, from a view
    // the app resolves. Name and avatar open the author's mini profile; anything else
    // bubbles to the row's jump-to-message handler.
    import Avatar from '../ui/Avatar.svelte';

    let { view, h } = $props();   // view: { parentId, mine, npub, name, avatarSrc, html, emojiTags, attachment, hidden }; h: twemojify, renderCustomEmojiShortcodes, showMiniProfile

    function nameInto(node, text) {
        let cur;
        const render = (t) => { if (t === cur) return; cur = t; node.textContent = t; h.twemojify(node); };
        render(text);
        return { update: render };
    }
    // The snippet is preview HTML the app built from the parent's text, then twemoji
    // and the custom emoji the parent's own tags resolve.
    function snippetInto(node, [html, tags]) {
        let cur;
        const render = ([hh, tg]) => {
            const key = hh + '\0' + (tg || []).map(t => t.shortcode + '=' + t.url).join(',');
            if (key === cur) return;
            cur = key;
            node.innerHTML = hh;
            h.twemojify(node);
            if (tg?.length) h.renderCustomEmojiShortcodes(node, tg);
        };
        render([html, tags]);
        return { update: render };
    }
    function openProfile(e, anchor = e.currentTarget) {
        if (!view.npub) return;
        e.stopPropagation();
        h.showMiniProfile(view.npub, anchor);
    }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="dmsg-reply btn" class:dmsg-reply-them={!view.mine} id="r-{view.parentId}" onclick={() => h.jumpToMessage(view.parentId)}>
    <!-- display:contents has no box to anchor the card to: the avatar inside does. -->
    <span class="dmsg-reply-avatar-host" style="display:contents" onclick={(e) => openProfile(e, e.currentTarget.firstElementChild)}>
        <Avatar src={view.avatarSrc} size={16} class="dmsg-reply-avatar" />
    </span>
    <span class="dmsg-reply-name" style="color: rgba(255, 255, 255, 0.7);" data-npub={view.npub || undefined} use:nameInto={view.name} onclick={openProfile}></span>
    {#if view.hidden}
        <span class="dmsg-reply-text dmsg-reply-snippet" style="color: rgba(255, 255, 255, 0.45); font-style: italic;">{view.hidden}</span>
    {:else if view.html}
        <span class="dmsg-reply-text dmsg-reply-snippet" style="color: rgba(255, 255, 255, 0.45);" use:snippetInto={[view.html, view.emojiTags]}></span>
    {:else if view.attachment}
        <div class="dmsg-reply-snippet" style="display: flex; align-items: center;">
            <span class="icon icon-{view.attachment.icon}" style="position: relative; background-color: rgba(255, 255, 255, 0.45); width: 18px; height: 18px; margin: 0px;"></span>
            <span style="color: rgba(255, 255, 255, 0.45); margin-left: 5px;">{view.attachment.description}</span>
        </div>
    {/if}
</div>
