<script>
    // Who wrote an embedded event and when. The avatar and name open their mini profile.
    import Avatar from '../../ui/Avatar.svelte';
    import { profileVersion } from '../../lib/signals.svelte.js';
    let { npub, at, h } = $props();   // h: NostrEmbedHelpers
    const who = $derived.by(() => { profileVersion(npub); return h.author(npub); });

    function nameInto(node, text) {
        const render = (t) => { node.textContent = t; h.twemojify(node); };
        render(text);
        return { update: render };
    }
    function openProfile(e) {
        e.stopPropagation();
        h.showMiniProfile(npub, e.currentTarget);
    }
</script>

<div class="ne-author">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <span class="ne-author-who btn" onclick={openProfile}>
        <Avatar src={who.avatarSrc} size={22} />
        <span class="ne-author-name" use:nameInto={who.name}></span>
    </span>
    {#if at}<span class="ne-when">{h.when(at)}</span>{/if}
</div>
