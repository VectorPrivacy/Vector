/**
 * Mini profile popup: the compact preview an avatar or name tap opens. The popup
 * itself is the MiniProfile island (src/components/people/MiniProfile.svelte),
 * driven from `openMiniProfile`; this side owns the open/close lifecycle, the nav
 * stack entry and the dismiss gestures.
 */

function _miniProfilePopup() {
    return VectorSvelte.miniProfileEls().popup;
}

VectorSvelte.setScreen('miniProfile', {
        h: {
            getProfile,
            getProfileAvatarSrc,
            getProfileBannerSrc,
            isMobile: () => !!platformFeatures?.is_mobile,
            twemojify,
            renderCustomEmojiShortcodes,
            renderMentions: (el) => renderMentions(el, false, { allowBare: true, queueSync: true }),
            showTooltip: showGlobalTooltip,
            hideTooltip: hideGlobalTooltip,
            onClose: hideMiniProfile,
            onMessage: (npub) => { hideMiniProfile(); openChat(npub); },
            onView: _miniProfileOpenFull,
        },
});

/** Open the mini profile for `npub`, anchored to the tapped element (null = centred). */
function showMiniProfile(npub, anchorEl) {
    if (!npub) return;
    // A mention chip INSIDE the open popup replaces it in place: keep the spot rather
    // than re-anchoring to a chip that is about to be torn down.
    let reuse = null;
    const current = _miniProfilePopup();
    if (current && anchorEl && current.contains(anchorEl)) {
        if (current.classList.contains('mini-profile-centered')) {
            reuse = { centered: true };
        } else {
            const r = current.getBoundingClientRect();
            reuse = { left: r.left, top: r.top };
        }
    }

    hideMiniProfile();
    VectorSvelte.openMiniProfile(npub, anchorEl, reuse);
    // The Android hardware back button closes the popup, not the tab beneath it.
    pushBack('mini-profile', hideMiniProfile);

    // Same priority queue renderMessage uses for missing author profiles.
    invoke('queue_profile_sync', { npub, priority: 'critical', forceRefresh: false }).catch(() => {});
}

/** Drill into the full profile screen. Shared by the avatar shortcut and "View Profile". */
function _miniProfileOpenFull(npub) {
    hideMiniProfile();
    previousChatBeforeProfile = strOpenChat || '';
    openProfile(getProfile(npub) || { id: npub });
}

function hideMiniProfile() {
    if (!VectorSvelte.miniProfile().npub) return;
    VectorSvelte.closeMiniProfile();
    popBack('mini-profile');   // no-op if a hardware-back already popped us
}

// Dismiss on outside-click (desktop), Escape, or a user scroll. The backdrop owns
// the mobile-centred dismiss path via its own click handler.
document.addEventListener('click', (e) => {
    const popup = _miniProfilePopup();
    // The path, not contains: a control the click re-renders is already unmounted here.
    if (!popup || popup.contains(e.target) || (e.composedPath?.() || []).includes(popup)) return;
    // Not on the avatar/name that opened it: the click delegate is about to re-open it
    // on the same chip, so let it own the lifecycle. The command line's bot chip too.
    if (e.target.closest('.dmsg-avatar, .dmsg-author, .dmsg-command-bot-avatar, .dmsg-command-bot')) return;
    hideMiniProfile();
});

document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hideMiniProfile();
});

// Only a USER scroll dismisses: a new message auto-scrolls the chat, which must not
// yank the popup closed. wheel/touch open a short intent window.
let _miniProfileScrollIntentUntil = 0;
const _markMiniProfileScrollIntent = () => { _miniProfileScrollIntentUntil = Date.now() + 200; };
document.addEventListener('wheel', _markMiniProfileScrollIntent, { capture: true, passive: true });
document.addEventListener('touchmove', _markMiniProfileScrollIntent, { capture: true, passive: true });

document.addEventListener('scroll', (e) => {
    const popup = _miniProfilePopup();
    if (!popup || popup.contains(e.target)) return;
    if (Date.now() < _miniProfileScrollIntentUntil) hideMiniProfile();
}, true);
