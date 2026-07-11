/**
 * Mini profile popup — Discord-style compact preview shown when an avatar/name
 * is clicked in a chat row. Banner strip on top, overlapping avatar, display
 * name + nickname + npub fingerprint, about/status text, and two CTAs:
 * "Send Message" (jumps to DM) and "View Profile" (drills into the existing
 * full profile screen).
 *
 *   Desktop: anchored to the clicked element, like the reaction details popup.
 *   Mobile : centered with backdrop, since anchor positioning is awkward at
 *            small viewport widths.
 *
 * Cross-file dependencies (resolved via classic-script global scope):
 *   main.js   — getProfile, getProfileAvatarSrc, getProfileBannerSrc,
 *               createPlaceholderAvatar, openProfile, openChat,
 *               platformFeatures, twemojify, invoke, convertFileSrc,
 *               previousChatBeforeProfile (assigned), strOpenChat
 */

let miniProfileEl = null;
let miniProfileBackdrop = null;
let miniProfileNpub = null;
let miniProfileAnonTimer = null;

// A found profile repaints instantly via the profile_update event, so this
// only fires for identities relays have nothing for — long enough that a slow
// (e.g. Tor) fetch of a real profile usually lands first.
const MINI_PROFILE_ANON_FALLBACK_MS = 6000;

/**
 * Open the mini profile for a given npub, anchored to the element the user
 * clicked. Pass null/undefined for `anchorEl` to fall back to centered.
 */
function showMiniProfile(npub, anchorEl) {
    if (!npub) return;

    // Tapping a mention chip INSIDE the open mini-profile replaces it in place:
    // reuse the current popup's screen position rather than re-anchoring to the
    // chip, which hideMiniProfile() is about to detach (its rect would collapse
    // to 0,0 → top-left). Captured before the teardown below.
    let reusePos = null;
    if (miniProfileEl && anchorEl && miniProfileEl.contains(anchorEl)) {
        if (miniProfileEl.classList.contains('mini-profile-centered')) {
            reusePos = { centered: true };
        } else {
            const r = miniProfileEl.getBoundingClientRect();
            reusePos = { left: r.left, top: r.top };
        }
    }

    hideMiniProfile();
    miniProfileNpub = npub;

    // Build a placeholder shell first so the popup feels instant. Profile data
    // (cached or freshly fetched) populates into the same element on resolve.
    const popup = document.createElement('div');
    popup.className = 'mini-profile-popup';
    popup.dataset.npub = npub;

    // Dim the rest of the UI so the popup reads as a focused modal on both
    // desktop and mobile. Click anywhere outside the popup to dismiss.
    const backdrop = document.createElement('div');
    backdrop.className = 'mini-profile-backdrop';
    backdrop.addEventListener('click', (e) => {
        if (e.target === backdrop) hideMiniProfile();
    });
    document.body.appendChild(backdrop);
    miniProfileBackdrop = backdrop;

    const profile = (typeof getProfile === 'function') ? getProfile(npub) : null;
    _populateMiniProfile(popup, npub, profile);

    document.body.appendChild(popup);
    miniProfileEl = popup;
    // Register with the nav stack so the Android hardware back button closes the mini profile
    // instead of falling through to the page/tab beneath it.
    pushBack('mini-profile', hideMiniProfile);

    if (reusePos?.centered) {
        popup.classList.add('mini-profile-centered');
    } else if (reusePos) {
        // Same spot as the popup we just replaced, clamped to the viewport in
        // case the new bio is taller.
        const popupRect = popup.getBoundingClientRect();
        const margin = 8;
        const left = Math.max(margin, Math.min(reusePos.left, window.innerWidth - popupRect.width - margin));
        const top = Math.max(margin, Math.min(reusePos.top, window.innerHeight - popupRect.height - margin));
        popup.style.left = `${left}px`;
        popup.style.top = `${top}px`;
    } else {
        _positionMiniProfile(popup, anchorEl);
    }

    // Kick off a fresh fetch in case data is stale or missing — same priority
    // queue renderMessage uses for missing author profiles.
    if (typeof invoke === 'function') {
        invoke('queue_profile_sync', {
            npub,
            priority: 'critical',
            forceRefresh: false,
        }).catch(() => { /* non-critical */ });
    }

    // With no cached profile we show "Loading…"; the backend stays silent when
    // relays have no metadata, so without this we'd hang forever. After a grace
    // period, treat an unresolved identity as anonymous.
    if (!profile) {
        miniProfileAnonTimer = setTimeout(() => {
            miniProfileAnonTimer = null;
            if (miniProfileEl === popup && miniProfileNpub === npub && !getProfile(npub)) {
                _populateMiniProfile(popup, npub, null, { fetchSettled: true });
            }
        }, MINI_PROFILE_ANON_FALLBACK_MS);
    }
}

/**
 * Drill into the full profile screen for `npub`. Shared by the avatar click
 * shortcut and the "View Profile" button.
 */
function _miniProfileOpenFull(npub) {
    hideMiniProfile();
    if (typeof openProfile !== 'function') return;
    previousChatBeforeProfile = (typeof strOpenChat !== 'undefined') ? strOpenChat : '';
    const prof = (typeof getProfile === 'function') ? getProfile(npub) : null;
    openProfile(prof || { id: npub });
}

function hideMiniProfile() {
    if (miniProfileAnonTimer) { clearTimeout(miniProfileAnonTimer); miniProfileAnonTimer = null; }
    if (miniProfileEl) { miniProfileEl.remove(); miniProfileEl = null; }
    if (miniProfileBackdrop) { miniProfileBackdrop.remove(); miniProfileBackdrop = null; }
    miniProfileNpub = null;
    popBack('mini-profile');   // no-op if a hardware-back already popped us
}

/**
 * Refresh the open mini profile if its npub matches `npub`. Called from
 * profile-update event handlers in main.js (wired via window export).
 */
function refreshMiniProfileIfMatches(npub) {
    if (!miniProfileEl || miniProfileNpub !== npub) return;
    const profile = (typeof getProfile === 'function') ? getProfile(npub) : null;
    if (!profile) return;
    // Real data arrived — the "Anon" fallback is moot.
    if (miniProfileAnonTimer) { clearTimeout(miniProfileAnonTimer); miniProfileAnonTimer = null; }
    _populateMiniProfile(miniProfileEl, npub, profile);
}
window.refreshMiniProfileIfMatches = refreshMiniProfileIfMatches;

/**
 * Build (or rebuild) the popup body for a profile. The popup element is
 * cleared first so this is safe to call multiple times against the same node
 * as data arrives.
 */
function _populateMiniProfile(popup, npub, profile, opts) {
    popup.innerHTML = '';

    // Banner strip
    const banner = document.createElement('div');
    banner.className = 'mini-profile-banner';
    const bannerSrc = (typeof getProfileBannerSrc === 'function') ? getProfileBannerSrc(profile) : null;
    if (bannerSrc) {
        const img = document.createElement('img');
        img.src = bannerSrc;
        img.alt = '';
        img.draggable = false;
        img.onerror = () => img.remove();
        banner.appendChild(img);
    }
    popup.appendChild(banner);

    // Avatar (overlapping the banner's bottom edge). Clicking it is a shortcut
    // to "View Profile" — same effect as the body button.
    const avatarWrap = document.createElement('div');
    avatarWrap.className = 'mini-profile-avatar';
    avatarWrap.title = 'View Profile';
    avatarWrap.onclick = (e) => {
        e.stopPropagation();
        _miniProfileOpenFull(npub);
    };
    const avatarSrc = (typeof getProfileAvatarSrc === 'function') ? getProfileAvatarSrc(profile) : null;
    if (avatarSrc) {
        const img = document.createElement('img');
        img.src = avatarSrc;
        img.alt = '';
        img.draggable = false;
        img.onerror = () => {
            img.replaceWith(createPlaceholderAvatar(false, 64));
        };
        avatarWrap.appendChild(img);
    } else {
        avatarWrap.appendChild(createPlaceholderAvatar(false, 64));
    }
    popup.appendChild(avatarWrap);

    // Status pill — Nostr kind 30315 user-status event. Positioned absolutely in
    // the banner area, to the right of the avatar (CSS handles placement). The
    // `.title` is the user-visible text — bare profile.status is the {title,
    // purpose, url} object and would stringify to "[object Object]".
    const statusText = (profile?.status?.title || '').toString().trim();
    if (statusText) {
        const status = document.createElement('div');
        status.className = 'mini-profile-status';
        const dot = document.createElement('span');
        dot.className = 'mini-profile-status-dot';
        status.appendChild(dot);
        const txt = document.createElement('span');
        txt.className = 'mini-profile-status-text';
        txt.textContent = statusText;
        twemojify(txt);
        status.appendChild(txt);
        popup.appendChild(status);
    }

    // Body
    const body = document.createElement('div');
    body.className = 'mini-profile-body';

    const name = document.createElement('div');
    name.className = 'mini-profile-name';
    const displayName = profile?.nickname || profile?.name || profile?.display_name;
    if (displayName) {
        name.textContent = displayName;
        twemojify(name);
    } else if (profile || (opts && opts.fetchSettled)) {
        // Either the profile loaded with no name set, or the fetch settled with
        // nothing on relays. Nostr identities are valid without metadata, so
        // call them what they are: anonymous.
        name.textContent = 'Anon';
    } else {
        // Profile not yet fetched — keep the placeholder so the user knows
        // something is still in flight.
        name.classList.add('mini-profile-name-loading');
        name.textContent = 'Loading…';
    }
    body.appendChild(name);

    // Sub-line: nickname OR truncated npub fingerprint
    const sub = document.createElement('div');
    sub.className = 'mini-profile-sub';
    const fingerprint = npub.length > 16 ? `${npub.slice(0, 12)}…${npub.slice(-4)}` : npub;
    sub.textContent = fingerprint;
    body.appendChild(sub);

    // About / bio (CSS line-clamped)
    const about = (profile?.about || '').trim();
    if (about) {
        const aboutEl = document.createElement('div');
        aboutEl.className = 'mini-profile-about';
        aboutEl.textContent = about;
        twemojify(aboutEl);
        // Render `npub1…` / `@npub` mentions as tappable @tags, same as the Profile tab.
        renderMentions(aboutEl, false, { allowBare: true, queueSync: true });
        body.appendChild(aboutEl);
    }

    // Actions
    const actions = document.createElement('div');
    actions.className = 'mini-profile-actions';

    const btnMessage = document.createElement('button');
    btnMessage.type = 'button';
    btnMessage.className = 'mini-profile-action mini-profile-action-primary';
    btnMessage.textContent = 'Send Message';
    btnMessage.onclick = (e) => {
        e.stopPropagation();
        hideMiniProfile();
        if (typeof openChat === 'function') openChat(npub);
    };
    actions.appendChild(btnMessage);

    const btnView = document.createElement('button');
    btnView.type = 'button';
    btnView.className = 'mini-profile-action';
    btnView.textContent = 'View Profile';
    btnView.onclick = (e) => {
        e.stopPropagation();
        _miniProfileOpenFull(npub);
    };
    actions.appendChild(btnView);

    body.appendChild(actions);
    popup.appendChild(body);
}

/**
 * Position the popup. Desktop: anchored next to the click target (right side
 * preferred, falling back to left/below if no room). Mobile: centered with
 * the backdrop providing the dismiss surface.
 */
function _positionMiniProfile(popup, anchorEl) {
    const isMobile = (typeof platformFeatures !== 'undefined') && platformFeatures?.is_mobile;

    if (isMobile || !anchorEl) {
        popup.classList.add('mini-profile-centered');
        return;
    }

    const rect = anchorEl.getBoundingClientRect();
    const popupRect = popup.getBoundingClientRect();
    const margin = 8;

    // Prefer to the right of the anchor. If no room, try left. Otherwise below.
    let left = rect.right + margin;
    if (left + popupRect.width + margin > window.innerWidth) {
        left = rect.left - popupRect.width - margin;
        if (left < margin) {
            // Fall back to below, centered horizontally on the anchor.
            left = Math.max(margin, Math.min(
                rect.left + (rect.width / 2) - (popupRect.width / 2),
                window.innerWidth - popupRect.width - margin
            ));
        }
    }

    // Vertical: try to align top with the anchor; clamp to viewport.
    let top = rect.top;
    if (top + popupRect.height + margin > window.innerHeight) {
        top = window.innerHeight - popupRect.height - margin;
    }
    if (top < margin) top = margin;

    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
}

// Dismiss on outside-click (desktop), Escape, or chat scroll. Backdrop owns
// the mobile-centered dismiss path via its own click handler.
document.addEventListener('click', (e) => {
    if (!miniProfileEl) return;
    if (miniProfileEl.contains(e.target)) return;
    // Don't dismiss on the avatar/name that opened it — the click delegate is
    // about to re-open it on the same chip; let it own the lifecycle. The
    // command line's bot avatar/name are openers too (same delegate).
    if (e.target.closest('.dmsg-avatar, .dmsg-author, .dmsg-command-bot-avatar, .dmsg-command-bot')) return;
    hideMiniProfile();
});

document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && miniProfileEl) hideMiniProfile();
});

// Only a *user* scroll gesture should dismiss — a new message auto-scrolls the
// chat programmatically, which must not yank the popup closed. wheel/touch set
// a short intent window; a scroll within it is the user's, anything else (the
// auto-scroll) is ignored.
let _miniProfileScrollIntentUntil = 0;
const _markMiniProfileScrollIntent = () => { _miniProfileScrollIntentUntil = Date.now() + 200; };
document.addEventListener('wheel', _markMiniProfileScrollIntent, { capture: true, passive: true });
document.addEventListener('touchmove', _markMiniProfileScrollIntent, { capture: true, passive: true });

document.addEventListener('scroll', (e) => {
    if (!miniProfileEl || miniProfileEl.contains(e.target)) return;
    if (Date.now() < _miniProfileScrollIntentUntil) hideMiniProfile();
}, true);
