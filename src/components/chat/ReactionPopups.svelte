<script>
    // The reaction hover tip ("reacted by Alice and Bob") and the details popup (who
    // reacted with this emoji), both placed against their chip after measuring. The
    // details rows derive from the message's version and each reactor's profile.
    import { flushSync } from 'svelte';
    import { profileVersion } from '../lib/signals.svelte.js';
    import { messageVersion } from '../lib/chatview.svelte.js';
    import { reactionTip, reactionDetails, bindReactionEl } from '../lib/reactionpopups.svelte.js';
    import Avatar from '../ui/Avatar.svelte';

    let { h } = $props();   // h: findMessage(msgId), getProfile, getName, getProfileAvatarSrc, twemojify, bindCachedImg, emojiLabel(emoji)

    const tip = $derived(reactionTip());
    const details = $derived(reactionDetails());

    // 1 → "Alice"; 2 → "Alice and Bob"; 3 → all three; 4+ → three and "N others".
    const NAMES_VISIBLE = 3;
    function formatNames(names) {
        if (names.length === 1) return names[0];
        if (names.length === 2) return `${names[0]} and ${names[1]}`;
        if (names.length === 3) return `${names[0]}, ${names[1]}, and ${names[2]}`;
        const others = names.length - NAMES_VISIBLE;
        return `${names.slice(0, NAMES_VISIBLE).join(', ')}, and ${others} other${others === 1 ? '' : 's'}`;
    }

    const reactors = $derived.by(() => {
        if (!details) return [];
        messageVersion(details.msgId);
        const msg = h.findMessage(details.msgId);
        return (msg?.reactions || []).filter(r => r.emoji === details.emoji).map(r => {
            profileVersion(r.author_id);
            const p = h.getProfile(r.author_id);
            return { id: r.author_id, src: h.getProfileAvatarSrc(p), name: p?.name || p?.display_name || r.author_id.slice(0, 12) + '...' };
        });
    });
    // A custom emoji is named by its shortcode, which the chip and the tip no longer show.
    const label = $derived.by(() => {
        if (!details) return '';
        if (details.url) return details.emoji;
        const name = h.emojiLabel(details.emoji) || '';
        return name ? name.charAt(0).toUpperCase() + name.slice(1) : '';
    });

    function emojiInto(node, emoji) {
        let cur;
        const render = (e) => { if (e === cur) return; cur = e; node.textContent = e; h.twemojify(node); };
        render(emoji);
        return { update: render };
    }
    // A custom emoji that cannot load falls back to its shortcode, as the chip does to a glyph.
    function customInto(img, [url, emoji]) {
        h.bindCachedImg(img, url, (el) => {
            const holder = el.parentElement;
            el.replaceWith(document.createTextNode(emoji));
            if (holder) h.twemojify(holder);
        });
    }

    // Above the chip, below when there is no room; clamped to the viewport.
    function place(node, [anchor, gap, centred]) {
        const run = ([a, g, c]) => {
            if (!a) return;
            flushSync();
            const rect = a.getBoundingClientRect();
            const mine = node.getBoundingClientRect();
            let top = rect.top - mine.height - g;
            if (top < 10) top = rect.bottom + g;
            let left = c ? rect.left + rect.width / 2 - mine.width / 2 : rect.left;
            left = Math.max(10, Math.min(left, window.innerWidth - mine.width - 10));
            node.style.left = `${left}px`;
            node.style.top = `${top}px`;
        };
        run([anchor, gap, centred]);
        return { update: run };
    }
    const bindTip = bindReactionEl('tip');
    const bindDetails = bindReactionEl('details');
</script>

{#if tip}
    <div class="reaction-hover-tip" use:bindTip use:place={[tip.anchor, 6, true]}>
        {#if tip.url}
            <span class="reaction-hover-tip-emoji"><img alt={tip.emoji} class="reaction-custom-emoji" use:customInto={[tip.url, tip.emoji]}></span>
        {:else}
            <span class="reaction-hover-tip-emoji" use:emojiInto={tip.emoji}></span>
        {/if}
        <span class="reaction-hover-tip-text">reacted by {formatNames(tip.names)}</span>
        <span class="reaction-hover-tip-hint">Right-click for details</span>
    </div>
{/if}

{#if details && reactors.length}
    <div class="reaction-details-popup" use:bindDetails use:place={[details.anchor, 4, false]}>
        <div class="reaction-details-header">
            {#if details.url}
                <span class="reaction-details-emoji"><img alt={details.emoji} class="reaction-custom-emoji" use:customInto={[details.url, details.emoji]}></span>
            {:else}
                <span class="reaction-details-emoji" use:emojiInto={details.emoji}></span>
            {/if}
            {#if label}<span class="reaction-details-label">{label}</span>{/if}
        </div>
        <div class="reaction-details-body">
            {#each reactors as r (r.id)}
                <div class="reaction-detail-row">
                    <Avatar src={r.src} size={25} class="reaction-avatar" />
                    <span class="reaction-detail-name">{r.name}</span>
                </div>
            {/each}
        </div>
    </div>
{/if}
