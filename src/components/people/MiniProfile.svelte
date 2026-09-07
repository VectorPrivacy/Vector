<script>
    // Discord-style compact preview for an avatar or name tap: banner, overlapping
    // avatar, status pill, name, fingerprint, bio and two actions. Derives from the
    // profile's signal, so a fetch landing repaints it in place. Desktop anchors it
    // beside the tap; mobile centres it over a backdrop.
    import { flushSync } from 'svelte';
    import { profileVersion } from '../lib/signals.svelte.js';
    import { miniProfile, settleMiniProfile } from '../lib/miniprofile.svelte.js';

    let { h } = $props();   // h: getProfile, getProfileAvatarSrc, getProfileBannerSrc, isMobile, twemojify, renderCustomEmojiShortcodes, renderMentions, createPlaceholderAvatar, showTooltip, hideTooltip, onClose, onMessage, onView

    // Relays stay silent for an identity with no metadata; after this long, call it Anon.
    const ANON_FALLBACK_MS = 6000;

    const view = $derived.by(() => {
        const m = miniProfile();
        if (!m.npub) return null;
        profileVersion(m.npub);
        const p = h.getProfile(m.npub);
        const npub = m.npub;
        const displayName = p?.nickname || p?.name || p?.display_name || '';
        return {
            npub, p,
            bannerSrc: h.getProfileBannerSrc(p),
            avatarSrc: h.getProfileAvatarSrc(p),
            status: (p?.status?.title || '').toString().trim(),
            emojiTags: p?.status?.emoji_tags || [],
            displayName,
            // A profile with no name, or a fetch that settled empty: Nostr identities are
            // valid without metadata, so call them what they are.
            nameText: displayName || ((p || m.settled) ? 'Anon' : 'Loading…'),
            loading: !displayName && !p && !m.settled,
            bot: !!p?.bot,
            fingerprint: npub.length > 16 ? `${npub.slice(0, 12)}…${npub.slice(-4)}` : npub,
            about: (p?.about || '').trim(),
        };
    });

    // Give up on a silent fetch after the grace period.
    $effect(() => {
        const m = miniProfile();
        if (!m.npub || h.getProfile(m.npub)) return;
        const t = setTimeout(() => settleMiniProfile(m.npub), ANON_FALLBACK_MS);
        return () => clearTimeout(t);
    });

    function textInto(node, [text, tags, mentions]) {
        let cur;
        const render = ([t, tg, mn]) => {
            const key = t + '\0' + (tg || []).map(x => x.shortcode + '=' + x.url).join(',');
            if (key === cur) return;
            cur = key;
            node.textContent = t;
            h.twemojify(node);
            if (tg?.length) h.renderCustomEmojiShortcodes(node, tg);
            if (mn) h.renderMentions(node);
        };
        render([text, tags, mentions]);
        return { update: render };
    }
    let avatarBroken = $state(false);
    let bannerBroken = $state(false);
    $effect(() => { view?.avatarSrc; avatarBroken = false; });
    $effect(() => { view?.bannerSrc; bannerBroken = false; });
    function placeholderInto(node) { node.replaceChildren(h.createPlaceholderAvatar(false, 64)); }

    // ── placement ──
    // Anchored: right of the tap, else left, else below; clamped to the viewport.
    // A popup replacing one it was opened from keeps that one's spot.
    let popup = $state(null);
    let centered = $state(false);
    let pos = $state({ left: 0, top: 0 });
    $effect(() => {
        const m = miniProfile();
        if (!m.npub || !popup) return;
        const margin = 8;
        if (m.reuse?.centered || (!m.reuse && (h.isMobile() || !m.anchor))) { centered = true; return; }
        centered = false;
        flushSync();
        const popupRect = popup.getBoundingClientRect();
        if (m.reuse) {
            pos = {
                left: Math.max(margin, Math.min(m.reuse.left, window.innerWidth - popupRect.width - margin)),
                top: Math.max(margin, Math.min(m.reuse.top, window.innerHeight - popupRect.height - margin)),
            };
            return;
        }
        const rect = m.anchor.getBoundingClientRect();
        let left = rect.right + margin;
        if (left + popupRect.width + margin > window.innerWidth) {
            left = rect.left - popupRect.width - margin;
            if (left < margin) {
                left = Math.max(margin, Math.min(rect.left + rect.width / 2 - popupRect.width / 2, window.innerWidth - popupRect.width - margin));
            }
        }
        let top = rect.top;
        if (top + popupRect.height + margin > window.innerHeight) top = window.innerHeight - popupRect.height - margin;
        if (top < margin) top = margin;
        pos = { left, top };
    });
</script>

{#if view}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="mini-profile-backdrop" onclick={(e) => { if (e.target === e.currentTarget) h.onClose(); }}></div>
    <div class="mini-profile-popup" class:mini-profile-centered={centered} data-npub={view.npub} bind:this={popup}
         style:left={centered ? null : `${pos.left}px`} style:top={centered ? null : `${pos.top}px`}>
        <div class="mini-profile-banner">
            {#if view.bannerSrc && !bannerBroken}
                <img src={view.bannerSrc} alt="" draggable="false" onerror={() => { bannerBroken = true; }}>
            {/if}
        </div>
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="mini-profile-avatar" title="View Profile" onclick={(e) => { e.stopPropagation(); h.onView(view.npub); }}>
            {#if view.avatarSrc && !avatarBroken}
                <img src={view.avatarSrc} alt="" draggable="false" onerror={() => { avatarBroken = true; }}>
            {:else}
                <div style="display:contents" use:placeholderInto></div>
            {/if}
        </div>
        {#if view.status}
            <div class="mini-profile-status">
                <span class="mini-profile-status-dot"></span>
                <span class="mini-profile-status-text" use:textInto={[view.status, view.emojiTags, false]}></span>
            </div>
        {/if}
        <div class="mini-profile-body">
            <div class="mini-profile-name" class:mini-profile-name-loading={view.loading}>
                <span use:textInto={[view.nameText, null, false]}></span>
                {#if view.bot}
                    <span class="icon icon-bot mini-profile-bot-icon" role="img" aria-label="Bot"
                          onmouseenter={(e) => h.showTooltip('Bot', e.currentTarget)} onmouseleave={() => h.hideTooltip()}></span>
                {/if}
            </div>
            <div class="mini-profile-sub">{view.fingerprint}</div>
            {#if view.about}
                <div class="mini-profile-about" use:textInto={[view.about, null, true]}></div>
            {/if}
            <div class="mini-profile-actions">
                <button type="button" class="mini-profile-action mini-profile-action-primary" onclick={(e) => { e.stopPropagation(); h.onMessage(view.npub); }}>Send Message</button>
                <button type="button" class="mini-profile-action" onclick={(e) => { e.stopPropagation(); h.onView(view.npub); }}>View Profile</button>
            </div>
        </div>
    </div>
{/if}
