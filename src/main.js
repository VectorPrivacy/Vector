const { invoke, convertFileSrc } = window.__TAURI__.core;
const { getCurrentWebview } = window.__TAURI__.webview;
const { getCurrentWindow } = window.__TAURI__.window;
const { getCurrentWebviewWindow } = window.__TAURI__.webviewWindow;
const { listen } = window.__TAURI__.event;
const { openUrl, revealItemInDir } = window.__TAURI__.opener;

// System event types (matches Rust SystemEventType enum)
const SystemEventType = {
    MemberLeft: 0,
    MemberJoined: 1,
    MemberRemoved: 2,
    WallpaperChanged: 3,
    WallpaperRemoved: 4,
    PinsModified: 5,
};

/** The one true display-name resolver. Accepts a profile object or an npub/id string.
 *  Order: local nickname → Nostr name → Nostr display_name → shortened npub. */
function getName(profileOrId) {
    const p = typeof profileOrId === 'string' ? getProfile(profileOrId) : profileOrId;
    const id = typeof profileOrId === 'string' ? profileOrId : p?.id;
    return p?.nickname || p?.name || p?.display_name || (id ? id.substring(0, 12) + '…' : 'Someone');
}

/** Resolve a system-event actor's display name from cached profiles; npub-prefix fallback. */
function systemEventName(npub) {
    return getName(npub);
}

/** The non-name part of a system-event line (" has joined", etc.). Split from the name so the
 * in-chat render can make the name a clickable profile affordance while keeping the rest plain. */
function systemEventSuffix(eventType) {
    switch (eventType) {
        case SystemEventType.MemberLeft: return ' has left';
        case SystemEventType.MemberJoined: return ' has joined';
        case SystemEventType.MemberRemoved: return ' was removed';
        case SystemEventType.WallpaperChanged: return ' changed the wallpaper';
        case SystemEventType.WallpaperRemoved: return ' removed the wallpaper';
        case SystemEventType.PinsModified: return ' modified the Pins';
        default: return '';
    }
}

/** Build a system-event line, resolving the actor's CURRENT cached name (the stored content
 * was baked with the raw npub at receive time, before the profile was known). Plain-string form
 * for notifications / chatlist previews; the in-chat DOM uses `insertSystemEvent` for the
 * clickable-name variant. */
function systemEventContent(eventType, npub) {
    return systemEventName(npub) + systemEventSuffix(eventType);
}

/**
 * Multi-account API surface. Wraps the Tauri commands that the in-app My
 * Profile dropdown and the pre-login picker both consume. Keeping it in one
 * place so the two callers can't drift on validation or error handling.
 */
const domTheme = document.getElementById('theme');


const domProfile = document.getElementById('profile');
const domProfileBackBtn = document.getElementById('profile-back-btn');
const domProfileHeaderAvatarContainer = document.getElementById('profile-header-avatar-container');
const domProfileName = document.getElementById('profile-name');
const domProfileStatus = document.getElementById('profile-status');
let fProfileEditMode = false;
const domProfileEditBtn = document.getElementById('profile-edit-btn');
const domProfileEditBar = document.getElementById('profile-edit-bar');
const domProfileEditCancelBtn = document.getElementById('profile-edit-cancel-btn');
const domProfileEditSaveBtn = document.getElementById('profile-edit-save-btn');
const domProfileBanner = document.getElementById('profile-banner');
const domProfileAvatar = document.getElementById('profile-avatar');
const domProfileNameSecondary = document.getElementById('profile-secondary-name');
const domProfileStatusSecondary = document.getElementById('profile-secondary-status');
const domProfileBadgeInvite = document.getElementById('profile-badge-invites');
const domProfileBadgeFawkes = document.getElementById('profile-badge-fawkes');
const domProfileBadgeBugHunter = document.getElementById('profile-badge-bughunter');
const domProfileDescription = document.getElementById('profile-description');
const domProfileDescriptionEditor = document.getElementById('profile-description-editor');
const domProfileOptions = document.getElementById('profile-option-list');
const domProfileOptionMessage = document.getElementById('profile-option-message');
const domProfileOptionMute = document.getElementById('profile-option-mute');
const domProfileOptionShare = document.getElementById('profile-option-share');
const domProfileOptionMore = document.getElementById('profile-option-more');
const domProfileMoreDropdown = document.getElementById('profile-more-dropdown');
const domProfileOptionNickname = document.getElementById('profile-option-nickname');
const domProfileOptionBlock = document.getElementById('profile-option-block');
const domProfileId = document.getElementById('profile-id');

// Our own cached badge flags (from get_my_badges / badges_updated). Used so
// the own-profile badge display reads the reliable persisted flag instead of
// re-querying the (often flaky) holding relay on every open.
let _myBadges = null;
// Session cache of other users' Fawkes-badge results, keyed by npub, so
// re-opening a profile doesn't re-fetch from the relay each time. Badges are
// permanent, so a session-lifetime cache is safe; next launch re-resolves.
const _fawkesBadgeCache = new Map();

// One-shot guard: we only do the live own-badge fallback check once per session
// (so non-holders don't re-hit the relay on every own-profile open).
let _ownBadgeLiveChecked = false;

/** Resolve whether `npub` holds the V for Vector badge, with caching.
 *  Own profile → the persisted `badge_vector` flag (fast path). Others →
 *  fetched once per session via check_fawkes_badge, then cached. */
async function resolveFawkesBadge(npub, isMine) {
    if (isMine) {
        if (_myBadges?.vector) return true;                 // sticky flag set — no network
        if (_myBadges === null) {
            try { _myBadges = await invoke('get_my_badges'); } catch {}
            if (_myBadges?.vector) return true;
        }
        // Flag not set yet — the post-sync refresh may have missed the claim (the
        // holding relay is often flaky during the saturated sync window → it backs
        // off for hours). A live check at this quiet moment confirms it; on success
        // the backend self-persists + emits badges_updated, which lifts the
        // emoji-pack perks. One attempt per session.
        if (_ownBadgeLiveChecked) return !!_myBadges?.vector;
        _ownBadgeLiveChecked = true;
        try {
            const has = await invoke('check_fawkes_badge', { npub });
            if (has) _myBadges = { vector: true, tier: 3 };
            return has;
        } catch { return !!_myBadges?.vector; }
    }
    if (_fawkesBadgeCache.has(npub)) return _fawkesBadgeCache.get(npub);
    try {
        const has = await invoke('check_fawkes_badge', { npub });
        _fawkesBadgeCache.set(npub, has);
        return has;
    } catch { return false; }
}

/** V for Vector (Guy Fawkes) badge card. Grants Full Premium (effective tier 3). */
function showFawkesCard() {
    showBadgeCard({
        title: 'V for Vector Badge',
        html: `Acquired by logging in on Guy Fawkes Day&nbsp;(November 5, 2025).<br><br><i style="opacity: 0.5; font-size: 13px;">Remember, remember the 5th of November...</i>`,
        svg: 'fawkes_mask.svg',
        perks: [{ text: 'Unlimited emoji packs', sub: 'up from 3' }, { text: 'Up to 90 emoji per pack', sub: 'up from 30' }, { text: 'Unlimited accounts', sub: 'up from 3' }],
    });
}

/** Resolve a user's Bug Hunter tier (0-3). Own → the cached value (filled by the
 *  post-sync refresh); others → a live fetch, session-cached. 0 = no badge. */
const _bugHunterTierCache = new Map();
async function resolveBugHunterTier(npub, isMine) {
    if (isMine) {
        if (_myBadges === null) { try { _myBadges = await invoke('get_my_badges'); } catch {} }
        return _myBadges?.bug_hunter | 0;
    }
    if (_bugHunterTierCache.has(npub)) return _bugHunterTierCache.get(npub);
    try {
        const tier = await invoke('get_bug_hunter_tier', { npub });
        _bugHunterTierCache.set(npub, tier);
        return tier;
    } catch { return 0; }
}

/** Open the Bug Hunter card for a held tier (1-3): highest-tier art, the tier
 *  rail, and the Partial/Full Premium access label. */
function showBugHunterCard(tier) {
    showBadgeCard({
        title: 'Bug Hunter',
        subtitle: 'Tier ' + tier,
        html: 'Bug Hunter badges are one of the most prestigious awards to true contributors of Vector who have identified and reported bugs or issues.',
        svg: 'bughunter_' + tier + '.svg',
        tierProgress: { current: tier, total: 3, icons: ['bughunter_1.svg', 'bughunter_2.svg', 'bughunter_3.svg'] },
        access: tier >= 3 ? 'Full Premium Access' : 'Partial Premium Access',
    });
}

// Close profile "More" dropdown when clicking outside
document.addEventListener('click', () => {
    if (domProfileMoreDropdown) {
        domProfileMoreDropdown.style.display = 'none';
        domProfileOptionMore.classList.remove('active');
    }
});

const domGroupOverview = document.getElementById('group-overview');
const domGroupOverviewBackBtn = document.getElementById('group-overview-back-btn');
// The overview body is an island (mounted below); the roster host and search live inside it.
const groupMembersEl = () => document.getElementById('group-overview-members');
const groupSearchEl = () => document.getElementById('group-member-search-input');

const domChats = document.getElementById('chats');
const domChatBookmarksBtn = document.getElementById('chat-bookmarks-btn');
const domAccount = document.getElementById('account');
const domAccountAvatarContainer = document.getElementById('account-avatar-container');
const domAccountName = document.getElementById('account-name');
const domAccountStatus = document.getElementById('account-status');
const domSyncLine = document.getElementById('sync-line');
const domChatList = document.getElementById('chat-list');
const domChatNewDM = document.getElementById('new-chat-btn');
const domChatNewGroup = document.getElementById('create-group-btn');
const domNavbar = document.getElementById('navbar');
const domInvites = document.getElementById('invites');
const domInvitesBtn = document.getElementById('invites-btn');
const domProfileBtn = document.getElementById('profile-btn');
const domChatlistBtn = document.getElementById('chat-btn');
const domSettingsBtn = document.getElementById('settings-btn');

const domChat = document.getElementById('chat');
const domChatBackBtn = document.getElementById('chat-back-btn');
const domChatBackNotificationDot = document.getElementById('chat-back-notification-dot');
const domChatHeaderAvatarContainer = document.getElementById('chat-header-avatar-container');
const domChatContact = document.getElementById('chat-contact');
const domChatContactStatus = document.getElementById('chat-contact-status');
const domChatMessages = document.getElementById('chat-messages');
const domChatMessageBox = document.getElementById('chat-box');
const domChatMessagesScrollReturnBtn = document.getElementById('chat-scroll-return');
// Late-bound because the composer is constructed here, thousands of lines before
// the mention selector that owns the tracked list. `var` so the binding exists no
// matter which of the two runs first.
var composerMentionLookup = () => [];

/** The plain `<textarea>` the composer replaced. Kept as an escape hatch for a
 *  WebView that can't drive a contenteditable, and as the automatic landing spot
 *  if the composer fails to construct. Shares the same id, so every rule and call
 *  site that isn't rich-composer-specific behaves as it always did. */
function createLegacyComposer(host) {
    const ta = document.createElement('textarea');
    ta.id = 'chat-input';
    ta.placeholder = 'Enter message...';
    host.appendChild(ta);
    return ta;
}

/** Off only when explicitly disabled, so a fresh install gets the rich one. */
function richComposerEnabled() {
    try { return localStorage.getItem('rich_composer') !== 'false'; } catch (_) { return true; }
}

// Rich composer: renders markdown, mention pills and custom emoji inline while
// `value` stays the exact string that gets sent. It exposes the textarea's face
// (value/selection/focus/listeners) and proxies anything else to its element, so
// every existing call site here, in picker.js and in mentions.js is unchanged.
//
// Built inside a try/catch on purpose. This runs at module scope, so a throw here
// would take the rest of main.js with it — no chat, no settings, no way to turn
// the composer off. Falling back to the textarea keeps the app usable on a WebView
// we haven't met yet.
const domChatMessageInput = (() => {
    const host = document.getElementById('chat-input-host');
    if (!richComposerEnabled()) return createLegacyComposer(host);
    try {
        return buildRichComposer(host);
    } catch (err) {
        console.error('[composer] rich composer failed to construct, falling back', err);
        host.textContent = '';
        return createLegacyComposer(host);
    }
})();

// Only a shortcode the user actually has renders as an image; anything else
// stays literal so it can still be typed and sent verbatim. Shared by every
// composer instance (chat + the Status mini-composer).
function cmpResolvePackEmoji(code) {
    for (const pack of arrEmojiPacks) {
        const hit = pack.emojis && pack.emojis.find(e => (e.dispCode || e.shortcode) === code);
        if (hit) return hit.url;
    }
    return null;
}

// Pack art lives on a remote host the WebView refuses to load — Android fails
// it outright. Bind through the same disk cache the message renderer uses, so
// composers and sent messages resolve identically, and degrade a missing one
// to its literal `:shortcode:` rather than a broken image.
function cmpBindEmojiImg(img, url, onFail) {
    if (window.bindCachedEmojiImg) window.bindCachedEmojiImg(img, url, 'emoji', onFail);
    else onFail();
}

function buildRichComposer(host) {
    const composer = createRichComposer(host, {
    placeholder: 'Enter message...',
    resolveEmoji: cmpResolvePackEmoji,
    // The draft carries `@display-name` and resolves to an npub at send, so a pill
    // is styled editable text rather than an atomic widget.
    bindEmojiImg: cmpBindEmojiImg,
    // A pasted mention carries the raw key. `getName` is the app's one display-name
    // resolver and already shortens an npub it doesn't know, so this never renders
    // a wall of bech32.
    resolveNpub: (npub) => getName(npub),
    // Which tracked name, if any, sits at `at` in `src`. The LONGEST wins, so
    // "@Walter White and co" pills only the name. Names are arbitrary text, so
    // each one measures itself rather than being matched against a shape.
    resolveMention: (src, at) => {
        let best = null;
        for (const m of composerMentionLookup()) {
            if (!m.name) continue;
            if (best && m.name.length <= best.length) continue;
            if (cmpFold(src.slice(at, at + m.name.length)).toLowerCase() !== cmpFold(m.name).toLowerCase()) continue;
            best = m.name;
        }
        return best;
        },
    });
    composer.el.id = 'chat-input';
    return composer;
}
const domChatMessageInputFile = document.getElementById('chat-input-file');
const domChatMessageInputCancel = document.getElementById('chat-input-cancel');
const domChatReplyBarName = document.getElementById('chat-reply-bar-name');
const domChatReplyBarSnippet = document.getElementById('chat-reply-bar-snippet');
const domChatReplyBarCancel = document.getElementById('chat-reply-bar-cancel');
const domChatMessageInputEmoji = document.getElementById('chat-input-emoji');
const domAttachmentPanel = document.getElementById('attachment-panel');
const domAttachmentPanelMain = document.getElementById('attachment-panel-main');
const domAttachmentPanelFile = document.getElementById('attachment-panel-file');
const domAttachmentPanelFolder = document.getElementById('attachment-panel-folder');
const domAttachmentPanelMiniApps = document.getElementById('attachment-panel-miniapps');
const domAttachmentPanelCommands = document.getElementById('attachment-panel-commands');
const domAttachmentPanelMiniAppsView = document.getElementById('attachment-panel-miniapps-view');
const domMiniAppsGrid = document.getElementById('miniapps-grid');
const domMiniAppsSearch = document.getElementById('miniapps-search');
const domAttachmentPanelBack = document.getElementById('attachment-panel-back');
const domMarketplacePanel = document.getElementById('marketplace-panel');
const domMarketplaceBackBtn = document.getElementById('marketplace-back-btn');
const domMiniAppLaunchOverlay = document.getElementById('miniapp-launch-overlay');
const domMiniAppLaunchIconContainer = document.getElementById('miniapp-launch-icon-container');
const domMiniAppLaunchName = document.getElementById('miniapp-launch-name');
const domMiniAppLaunchCancel = document.getElementById('miniapp-launch-cancel');
const domMiniAppLaunchSolo = document.getElementById('miniapp-launch-solo');
const domMiniAppLaunchInvite = document.getElementById('miniapp-launch-invite');
const domChatMessageInputVoice = document.getElementById('chat-input-voice');
const domChatMessageInputSend = document.getElementById('chat-input-send');
const domChatInputContainer = document.querySelector('.chat-input-container');

const domChatNew = document.getElementById('chat-new');
const domChatNewBackBtn = document.getElementById('chat-new-back-text-btn');
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

// Create Group UI refs
const domCreateGroup = document.getElementById('create-group');
const domSettings = document.getElementById('settings');
const domSettingsThemeSelect = document.getElementById('theme-select');
const domSettingsPrivacyWebPreviewsInfo = document.getElementById('privacy-web-previews-info');
const domSettingsPrivacyStripTrackingInfo = document.getElementById('privacy-strip-tracking-info');
const domSettingsPrivacySendTypingInfo = document.getElementById('privacy-send-typing-info');
const domSettingsPrivacyTorInfo = document.getElementById('privacy-tor-info');
const domSettingsStorageGalleryInfo = document.getElementById('storage-gallery-info');
const domSettingsExportAccountInfo = document.getElementById('export-account-info');
const domSettingsChangePinInfo = document.getElementById('change-pin-info');
const domSettingsLogoutInfo = document.getElementById('logout-info');
const domSettingsLogout = document.getElementById('logout-btn');
const domSettingsExport = document.getElementById('export-account-btn');
const domRemoteSignerReauthBtn = document.getElementById('remote-signer-reauth-btn');

const domApp = document.getElementById('popup-container');
const domPopup = document.getElementById('popup');
const domPopupIcon = document.getElementById('popupIcon');
const domPopupTitle = document.getElementById('popupTitle');
const domPopupSubtext = document.getElementById('popupSubtext');
const domPopupConfirmBtn = document.getElementById('popupConfirm');
const domPopupCancelBtn = document.getElementById('popupCancel');
const domPopupInput = document.getElementById('popupInput');

/**
 * Opens or closes the Attachment Panel
 *
 * The panel slides up from behind the chat box, similar to the emoji panel.
 */
/**
 * Run an async function (typically `invoke('login_from_stored_key', ...)`)
 * while polling Tor's bootstrap state. If Tor is mid-bootstrap during the
 * call, the lockscreen title is overridden with "Bootstrapping Tor… NN%"
 * so the user isn't told the app is "decrypting" while it's actually
 * waiting on Arti's consensus fetch. Title is restored on completion.
 */
async function runWithTorBootstrapStatus(fn) {
    const titleEl = domLoginEncryptTitle;
    const original = titleEl ? titleEl.textContent : '';
    let pollHandle = null;
    let didOverride = false;

    if (titleEl) {
        const tick = async () => {
            try {
                const state = await invoke('tor_get_state');
                if (!state || !state.enabled) return;
                const status = state.status || '';
                if (state.running) {
                    // Bootstrap finished mid-call; restore the original title
                    // unless we're about to be replaced by the next phase anyway.
                    if (didOverride) {
                        titleEl.textContent = original;
                        didOverride = false;
                    }
                } else if (status.startsWith('bootstrapping')) {
                    const pct = Number.isFinite(state.bootstrap_progress)
                        ? state.bootstrap_progress
                        : null;
                    titleEl.textContent = pct != null
                        ? `Bootstrapping Tor… ${pct}%`
                        : 'Bootstrapping Tor…';
                    didOverride = true;
                }
            } catch (_) { /* swallow — failsafe */ }
        };
        // First sample now so the title flips immediately when bootstrap is
        // already in flight, then keep up at 1Hz which matches Arti's event
        // cadence well enough.
        tick();
        pollHandle = setInterval(tick, 1000);
    }

    try {
        return await fn();
    } finally {
        if (pollHandle) clearInterval(pollHandle);
        if (didOverride && titleEl) titleEl.textContent = original;
    }
}

// Mirror the attachment panel's `.visible` class into the Android back stack
// so the hardware back press dismisses it from any open site (toggle button,
// outside click, send finish, miniapp launch).
if (domAttachmentPanel) {
    new MutationObserver(() => {
        if (domAttachmentPanel.classList.contains('visible')) {
            pushBack('attachment-panel', closeAttachmentPanel);
        } else {
            popBack('attachment-panel');
        }
    }).observe(domAttachmentPanel, { attributes: true, attributeFilter: ['class'] });
}

function toggleAttachmentPanel() {
    if (!domAttachmentPanel.classList.contains('visible')) {
        // Close emoji panel if open
        if (picker.classList.contains('visible')) {
            picker.classList.remove('visible');
            picker.style.bottom = '';
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
        }

        // Display the attachment panel
        domAttachmentPanel.classList.add('visible');
        domChatMessageInputFile.classList.add('open');

        // Position attachment panel dynamically above the chat-box
        const chatBox = document.getElementById('chat-box');
        if (chatBox) {
            const chatBoxHeight = chatBox.getBoundingClientRect().height;
            domAttachmentPanel.style.bottom = (chatBoxHeight + 10) + 'px';
        }
        
        // Commands: only in chats with known bots; grayed while a draft exists.
        if (domAttachmentPanelCommands) {
            const showCmds = !!(commandCtrl && commandCtrl.hasBots && commandCtrl.hasBots());
            domAttachmentPanelCommands.style.display = showCmds ? '' : 'none';
            if (showCmds) {
                domAttachmentPanelCommands.classList.toggle('disabled', domChatMessageInput.value.trim().length > 0);
            }
        }

        // Animate items when panel opens
        animateAttachmentPanelItems(domAttachmentPanelMain);
    } else {
        // Hide the attachment panel
        closeAttachmentPanel();
    }
}

/**
 * Closes the Attachment Panel
 */
function closeAttachmentPanel() {
    domAttachmentPanel.classList.remove('visible');
    domAttachmentPanel.style.bottom = '';
    domChatMessageInputFile.classList.remove('open');
    // Deactivate edit mode if active
    deactivateMiniAppsEditMode();
    // Reset to main view when closing
    showAttachmentPanelMain();
}

/**
 * Shows a global tooltip above the target element
 * @param {string} text - The tooltip text
 * @param {HTMLElement} targetElement - The element to position the tooltip above
 */
function showGlobalTooltip(text, targetElement) {
    const tooltip = document.getElementById('global-tooltip');
    if (!tooltip) return;
    
    tooltip.textContent = text;

    // Get the target element's position
    const rect = targetElement.getBoundingClientRect();

    // Position tooltip above the element, centered horizontally, but clamped
    // inside the viewport: a wide tooltip (e.g. a long URL) centered over an
    // edge-hugging target otherwise bleeds off-screen.
    const pad = 8;
    const half = tooltip.offsetWidth / 2;
    const centerX = rect.left + rect.width / 2;
    const clampedX = Math.max(pad + half, Math.min(window.innerWidth - pad - half, centerX));
    tooltip.style.left = `${clampedX}px`;
    tooltip.style.top = `${rect.top - 8}px`;
    tooltip.style.transform = 'translate(-50%, -100%)';

    // Show the tooltip
    tooltip.classList.add('visible');
}

/**
 * Hides the global tooltip
 */
function hideGlobalTooltip() {
    const tooltip = document.getElementById('global-tooltip');
    if (!tooltip) return;
    tooltip.classList.remove('visible');
}

// Dismiss stuck tooltips on any click/tap or window blur
document.addEventListener('click', hideGlobalTooltip);
document.addEventListener('touchstart', hideGlobalTooltip);
window.addEventListener('blur', hideGlobalTooltip);

/**
 * Helper function to escape HTML.
 * Escapes quotes too: callers interpolate into quoted attributes
 * (alt="...", data-*="..."), where an unescaped quote is an attribute
 * breakout → injected event handler. Safe for text contexts as well.
 */
function escapeHtml(text) {
    return String(text ?? '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

/**
 * Strip HTML tags and block-level markdown from message content to produce clean preview plaintext.
 * Idempotent — safe to call on already-cleaned text.
 *
 * Counterpart: strip_content_for_preview() in notification_service.rs (Rust, for OS notifications)
 *
 * @param {string} content - Raw message content
 * @returns {string} Plain text suitable for previews (inline markdown like ** and || preserved)
 */
function contentToPreviewText(content) {
    if (!content) return '';
    let text = content;
    // Replace <br> / <br/> with space
    text = text.replace(/<br\s*\/?>/gi, ' ');
    // Strip known HTML tags only (preserve unknown angle bracket content like "<insert text here>")
    text = text.replace(/<\/?(a|abbr|b|blockquote|br|code|del|details|div|em|h[1-6]|hr|i|li|ol|p|pre|s|span|strong|sub|summary|sup|table|tbody|td|th|thead|tr|u|ul)(?:\s[^>]*)?\/?>/gi, '');
    // Strip block-level markdown: headers, blockquotes, code fences, horizontal rules
    text = text.replace(/^#{1,6}\s+/gm, '');
    text = text.replace(/^>\s?/gm, '');
    text = text.replace(/^```[\s\S]*?^```/gm, '');
    text = text.replace(/^---+$/gm, '');
    text = text.replace(/^\*\*\*+$/gm, '');
    // Strip inline code backticks (keep inner text)
    text = text.replace(/`([^`]*)`/g, '$1');
    // Collapse whitespace and trim
    text = text.replace(/\s+/g, ' ').trim();
    return text;
}

/**
 * Convert message content to safe HTML for inline preview rendering.
 * Strips HTML/block markdown via contentToPreviewText(), HTML-escapes, then renders
 * inline markdown (bold, italic, strikethrough, spoiler) as safe HTML tags.
 *
 * Security: escapeHtml() runs BEFORE markdown→HTML conversion, so only our own tags
 * (<b>, <i>, <s>, <span>) appear in the output — no user-controlled HTML is possible.
 *
 * IMPORTANT: Truncate BEFORE calling this (not after), since truncating the output
 * can break HTML tags. Use: contentToPreviewHtml(truncateGraphemes(contentToPreviewText(text), n))
 *
 * Used by: reply context (renderMessage), reply-on-edit listener, chat list (generateChatPreviewText)
 * Counterpart: strip_content_for_preview() in notification_service.rs (plaintext only, for OS notifications)
 *
 * @param {string} content - Raw message content (or pre-cleaned plaintext)
 * @returns {string} Safe HTML string — assign to .innerHTML
 */
function contentToPreviewHtml(content) {
    // Headers demote to BOLD in one-line previews (chat list + pins): the
    // structure can't survive the flatten, but the emphasis can. Rewritten to
    // markdown bold BEFORE the flatten so it rides the existing `**` pathway
    // (and fenced code is stripped later, taking any rewritten lines with it).
    const demoted = String(content ?? '').replace(/^#{1,6}\s+(.+)$/gm, '**$1**');
    let text = contentToPreviewText(demoted);
    // HTML-escape to prevent injection — must happen before markdown conversion
    text = escapeHtml(text);
    // Convert inline markdown to HTML (order matters: bold before italic to avoid **x** matching **)
    text = text.replace(/\*\*(.+?)\*\*/g, '<b>$1</b>');
    text = text.replace(/\*(.+?)\*/g, '<i>$1</i>');
    text = text.replace(/~~(.+?)~~/g, '<s>$1</s>');
    // Spoilers → non-interactive blur effect (spoiler-preview class prevents click-to-reveal,
    // unlike .spoiler in full messages which is revealable — see markdown.js click handler)
    text = text.replace(/\|\|(.+?)\|\|/g, '<span class="spoiler-wrapper"><span class="spoiler spoiler-preview">$1</span></span>');
    return text;
}

/**
 * Truncates a string to a maximum number of grapheme clusters (visual characters).
 * Unlike substring(), this properly handles emojis and other multi-byte characters.
 */
function truncateGraphemes(text, maxLength) {
    const segmenter = new Intl.Segmenter('en', { granularity: 'grapheme' });
    const segments = [...segmenter.segment(text)];
    if (segments.length <= maxLength) return text;
    return segments.slice(0, maxLength).map(s => s.segment).join('') + '…';
}

/**
 * Grapheme truncation that treats a whole `:shortcode:` as ONE atomic token
 * priced like an emoji (2): cutting inside one reveals the raw code in the
 * preview, and pricing it by its letters lets three emojis eat the budget.
 */
function truncateEmojiAware(text, maxLength) {
    const parts = text.split(/(:[a-zA-Z0-9_~-]+:)/g);
    const segmenter = new Intl.Segmenter('en', { granularity: 'grapheme' });
    let out = '';
    let used = 0;
    for (let i = 0; i < parts.length; i++) {
        const part = parts[i];
        if (!part) continue;
        if (i % 2 === 1) {
            if (used + 2 > maxLength) return out + '…';
            out += part;
            used += 2;
        } else {
            const segments = [...segmenter.segment(part)];
            const remaining = maxLength - used;
            if (segments.length > remaining) {
                return out + segments.slice(0, remaining).map(s => s.segment).join('') + '…';
            }
            out += part;
            used += segments.length;
        }
    }
    return out;
}

/**
 * Close inline-markdown delimiters that lost their pair after truncation, so
 * the renderer applies the original styling to the truncated tail and a half-
 * spoiler stays blurred instead of leaking its content. The closing delimiter
 * is appended after the ellipsis so the `…` lives inside the wrapped span.
 */
function balanceInlineMarkdown(text) {
    if (((text.match(/\*\*/g) || []).length) % 2 === 1) text += '**';
    const singleStars = [...text.matchAll(/(?<!\*)\*(?!\*)/g)];
    if (singleStars.length % 2 === 1) text += '*';
    if (((text.match(/~~/g) || []).length) % 2 === 1) text += '~~';
    if (((text.match(/\|\|/g) || []).length) % 2 === 1) text += '||';
    return text;
}

/**
 * Build the small HTML preview used inside reply-context bubbles. Resolves
 * @npub mentions to display names, strips/normalises the content, truncates
 * by graphemes, auto-closes any orphaned inline-markdown delimiters, then
 * renders inline markdown to safe HTML.
 */
function buildReplyPreviewHtml(content, maxLength = 50) {
    const resolved = resolveMentionText(content);
    const plain = contentToPreviewText(resolved);
    const truncated = truncateEmojiAware(plain, maxLength);
    const balanced = balanceInlineMarkdown(truncated);
    return contentToPreviewHtml(balanced);
}

/**
 * Represents a user profile.
 * @typedef {Object} Profile
 * @property {string} id - Unique identifier for the profile.
 * @property {string} name - The name of the user.
 * @property {string} avatar - URL to the user's avatar image.
 * @property {string} last_read - ID of the last message that was read.
 * @property {Status} status - The current status of the user.
 * @property {number} last_updated - Timestamp indicating when the profile was last updated.
 * @property {number} typing_until - Timestamp until which the user is considered typing.
 * @property {boolean} mine - Indicates if this profile belongs to the current user.
 */

/**
 * Represents a message in the system.
 * @typedef {Object} Message
 * @property {string} id - Unique identifier for the message.
 * @property {string} content - The content of the message.
 * @property {string} replied_to - ID of the message this is replying to, if any.
 * @property {Object} preview_metadata - Metadata for link previews, if any.
 * @property {Attachment[]} attachments - Array of file attachments.
 * @property {Reaction[]} reactions - An array of reactions to this message.
 * @property {number} at - Timestamp when the message was sent.
 * @property {boolean} pending - Whether the message is still being sent.
 * @property {boolean} failed - Whether the message failed to send.
 * @property {boolean} mine - Indicates if this message was sent by the current user.
 */

/**
 * Represents a file attachment in a message.
 * @typedef {Object} Attachment
 * @property {string} id - The unique file ID (encryption nonce).
 * @property {string} key - The encryption key.
 * @property {string} nonce - The encryption nonce.
 * @property {string} extension - The file extension.
 * @property {string} url - The host URL, typically a NIP-96 server.
 * @property {string} path - The storage directory path.
 * @property {number} size - The download size of the encrypted file.
 * @property {boolean} downloading - Whether the file is currently being downloaded.
 * @property {boolean} downloaded - Whether the file has been downloaded.
 */

/**
 * Represents metadata for a website preview.
 * @typedef {Object} SiteMetadata
 * @property {string} domain - The domain of the website.
 * @property {string} [og_title] - Open Graph title.
 * @property {string} [og_description] - Open Graph description.
 * @property {string} [og_image] - Open Graph image URL.
 * @property {string} [og_url] - Open Graph URL.
 * @property {string} [og_type] - Open Graph content type.
 * @property {string} [title] - Website title.
 * @property {string} [description] - Website description.
 * @property {string} [favicon] - Website favicon URL.
 */

/**
 * Represents the status of a user.
 * @typedef {Object} Status
 * @property {string} title - The title of the status.
 * @property {string} purpose - Description or purpose of the status.
 * @property {string} url - URL associated with the status, if any.
 */

/**
 * Represents a reaction to a message.
 * @typedef {Object} Reaction
 * @property {string} id - Unique identifier for the reaction.
 * @property {string} reference_id - The HEX Event ID of the message being reacted to.
 * @property {string} author_id - The npub of the author who reacted.
 * @property {string} emoji - The emoji used for the reaction.
 */

/**
 * Represents a chat between users.
 * @typedef {Object} Chat
 * @property {string} id - Chat ID (npub for DMs).
 * @property {string} chat_type - Type of chat (DirectMessage, Group, etc).
 * @property {string[]} participants - Array of participant npubs.
 * @property {Message[]} messages - Array of messages in this chat.
 * @property {string} last_read - ID of the last read message.
 * @property {number} created_at - Timestamp when chat was created.
 * @property {Object} metadata - Additional chat metadata.
 * @property {boolean} muted - Whether the chat is muted.
 */

/**
 * Represents an MLS group invite.
 * @typedef {Object} MLSWelcome
 * @property {string} id - Unique identifier for the welcome/invite.
 * @property {string} group_id - The MLS group ID.
 * @property {string} group_name - Name of the group.
 * @property {string} welcomer_pubkey - Pubkey of the person who invited.
 * @property {number} member_count - Number of members in the group.
 * @property {string} [image] - Optional group avatar image.
 * @property {string} [description] - Optional group description.
 */

/**
 * Represents an MLS message record.
 * @typedef {Object} MLSMessageRecord
 * @property {string} inner_event_id - The inner event ID for deduplication.
 * @property {string} wrapper_event_id - The wrapper event ID.
 * @property {string} author_pubkey - The sender's pubkey.
 * @property {string} content - The message content.
 * @property {number} created_at - Timestamp in seconds.
 * @property {Array<Array<string>>} tags - Nostr tags.
 * @property {boolean} mine - Whether this message was sent by the current user.
 */

/**
 * A cache of all profiles (without messages)
 * @type {Profile[]}
 */
let arrProfiles = [];

/**
 * A cache of all chats (with messages)
 * @type {Chat[]}
 */
let arrChats = [];


/**
 * Pending Community invites (npub gift-wraps the user hasn't accepted yet). Rendered as
 * pinned slots at the top of the chat list, like MLS welcomes.
 * @type {Array<{community_id: string, name: string, inviter_npub: string}>}
 */
let arrCommunityInvites = [];

/**
 * The current open chat (by npub)
 */
let strOpenChat = "";
/** Blocked message IDs the user has clicked to reveal (survives re-renders) */
const revealedBlockedMessages = new Set();

/**
 * The chat ID we came from when opening a profile (to return to on back)
 */
let previousChatBeforeProfile = "";

/**
 * Interval ID for periodic profile refresh while viewing profile tab
 */
let profileRefreshInterval = null;

/**
 * Get a DM chat for a user
 * @param {string} npub - The user's npub
 * @returns {Chat|undefined} - The chat if it exists
 */
function getDMChat(npub) {
    return arrChats.find(c => c.chat_type === 'DirectMessage' && c.id === npub);
}

/**
 * Get a chat by ID (works for DMs and Community channels)
 * @param {string} id - The chat ID (npub for DM, channel id for Community)
 * @returns {Chat|undefined} - The chat if it exists
 */
function getChat(id) {
    return arrChats.find(c => c.id === id);
}

/**
 * Get or create a chat (DM or Community channel)
 * @param {string} id - The chat ID (npub for DM, channel id for Community)
 * @param {string} chatType - 'DirectMessage' or 'Community'
 * @returns {Chat} - The chat (existing or newly created)
 */
function getOrCreateChat(id, chatType = 'DirectMessage') {
    const isGroupType = chatType === 'Community';
    let chat = isGroupType
        ? arrChats.find(c => c.chat_type === 'Community' && c.id === id)
        : getDMChat(id);
    if (!chat) {
        chat = {
            id: id,
            chat_type: chatType,
            participants: isGroupType ? [] : [id],
            messages: [],
            last_read: '',
            created_at: Math.floor(Date.now() / 1000),
            metadata: {},
            muted: false
        };
        arrChats.push(chat);
    }
    return chat;
}

/**
 * Whether a chat is "group-like": a many-person room rendered with avatar + per-message
 * author headers (an MLS group OR a Community channel), as opposed to a 1:1 DM. Single
 * source of truth for the render-layer group/DM fork. (MLS is being retired in favor of
 * Communities; this keeps both rendering during the transition.)
 */
function chatIsGroup(chat) {
    // MLS is being torn out; a "group-like" chat is now exclusively a Community channel.
    return !!chat && chat.chat_type === 'Community';
}

/**
 * Whether a chat is a dissolved Community channel (owner tombstone, §6.1). The backend seals
 * a dissolved community: new sends/reactions/edits silently go nowhere. The UI mirrors that
 * by disabling the composer and stripping all message actions except own-message delete.
 */
function chatIsDissolved(chat) {
    return !!chat && chat.chat_type === 'Community' && chat.metadata?.custom_fields?.dissolved === 'true';
}

/** Lookup a message row's chat (by the open chat) and report if it's dissolved. */
function rowIsInDissolvedCommunity() {
    return chatIsDissolved(arrChats.find(c => c.id === strOpenChat));
}

/**
 * Apply the dissolved-community composer lockdown + end-of-community divider to the currently open chat.
 * Shared by `openChat` (on open) and the `community_refreshed` listener (when a community seals while it's
 * the open view) so a realtime dissolution updates the live UI, not just the cached flag.
 */
function applyDissolvedChatUI(chat) {
    VectorSvelte.setLock('dissolved', 'This community has been dissolved.');
    VectorSvelte.flushSync();
    if (!document.getElementById('dissolved-notice')) {
        const communityName = chat?.metadata?.custom_fields?.name || 'This community';
        const dissolvedNotice = insertSystemEvent(`${communityName} was dissolved by the owner.`);
        dissolvedNotice.id = 'dissolved-notice';
        dissolvedNotice.style.marginBottom = '20px';
        domChatMessages.appendChild(dissolvedNotice);
    }
}

/**
 * Resolve Community channel logos to local cached paths. Unlike MLS, a Community chat's
 * name/description/identity already arrive IN the chat payload (`custom_fields`, persisted
 * in the chats table) and load uniformly with DMs — no metadata hydrate needed. Only the
 * encrypted logo needs an async cache step, exactly like DM profile avatars: read the
 * `icon` flag + `community_id` from the chat's own metadata and cache lazily.
 */
const _communityAvatarAttempted = new Set();
function resolveCommunityAvatars() {
    for (const chat of arrChats) {
        if (chat.chat_type !== 'Community') continue;
        const cf = chat.metadata?.custom_fields || {};
        // Ask the backend directly instead of trusting an `icon` flag on the chat
        // row — the flag can lag the community's own row (set at register time,
        // while the icon arrives via the live control fold). The call is a cheap
        // local read when there's no icon and disk-cached once there is.
        if (cf.community_id && !chat.metadata.avatar_cached && !_communityAvatarAttempted.has(chat.id)) {
            _communityAvatarAttempted.add(chat.id);
            invoke('cache_community_image', { communityId: cf.community_id, isBanner: false })
                .then(path => {
                    if (path) {
                        chat.metadata.avatar_cached = path;
                        communityChanged(cf.community_id);
                    }
                })
                .catch(() => {})
                .finally(() => {
                    // A miss stays retryable on the NEXT resolver pass (a fresh icon
                    // can land via the live fold at any time); memoizing only the
                    // in-flight window stops same-pass duplicate invokes.
                    _communityAvatarAttempted.delete(chat.id);
                });
        }
    }
}

/**
 * Get or create a DM chat for a user
 * @param {string} npub - The user's npub
 * @returns {Chat} - The chat (existing or newly created)
 */
function getOrCreateDMChat(npub) {
    return getOrCreateChat(npub, 'DirectMessage');
}

/**
 * Finalize a pending message after successful send.
 * Updates the message ID and clears the pending state.
 * @param {string} chatId - The chat ID
 * @param {string} pendingId - The temporary pending ID
 * @param {string} eventId - The real event ID from the backend
 */
function finalizePendingMessage(chatId, pendingId, eventId) {
    const chat = getChat(chatId);
    if (!chat) return;

    const msgIdx = chat.messages.findIndex(m => m.id === pendingId);
    if (msgIdx === -1) return;

    const msg = chat.messages[msgIdx];
    const oldId = msg.id;
    msg.id = eventId;
    msg.pending = false;

    // Own message just landed: we know its delete-meta without a fetch (retained
    // keys on send, never admin-hideable). Seed the real id, drop the pending one.
    dmsgInvalidateDeleteMeta(oldId);
    dmsgSetOwnDeleteMeta(eventId, true);

    // Update event cache
    if (eventCache.has(chatId)) {
        const cachedEvents = eventCache.getEvents(chatId);
        if (cachedEvents) {
            const cacheIdx = cachedEvents.findIndex(m => m.id === oldId);
            if (cacheIdx !== -1) {
                cachedEvents[cacheIdx] = msg;
            }
        }
    }

    // Re-render if this chat is open
    if (strOpenChat === chatId) {
        const domMsg = document.getElementById(oldId);
        if (domMsg) {
            const profile = getProfile(chatId);
            updateMessageRow(domMsg, msg, profile, oldId);
        }
        strLastMsgID = eventId;
        softChatScroll();
    }
}

/**
 * Pinned chat ids, in pin order — the account's favourites, synced across its
 * own devices. Ids are opaque: a DM's npub, or a Community's id.
 * @type {string[]}
 */
let arrPinnedChats = [];

/**
 * The id a chat is pinned BY. A Community is pinned as the community, not as
 * the channel row that represents it, so its `general` row is what gets hoisted.
 * @param {Chat} chat
 * @returns {string}
 */
function chatPinKey(chat) {
    if (chat.chat_type === 'Community') {
        return chat.metadata?.custom_fields?.community_id || chat.id;
    }
    return chat.id;
}

/**
 * Pin rank, or -1 when unpinned. Looked up per render rather than stamped onto
 * chats when they sync, so a chat that arrives AFTER its pin is pinned on its
 * first paint with no restart and no retro-pass.
 * @param {Chat} chat
 * @returns {number}
 */
function chatPinRank(chat) {
    return arrPinnedChats.indexOf(chatPinKey(chat));
}

/**
 * True while a sync phase is running. The synced preference lists (pins,
 * blocks, mutes, nicknames) are whole-list newest-wins, so a change made before
 * this device has reconciled would publish its emptier view over another
 * device's. The backend refuses such a publish outright; this stops the user
 * making one and wondering why nothing happened.
 */
let fSyncing = false;

/**
 * Refuse a synced-preference action mid-sync, with a reason. Returns true when
 * the caller should stop.
 */
/**
 * Repaint every rendered surface showing `npub`'s name: chat list, open chat
 * header, message authors, command invokers, bot names and system-event lines.
 *
 * A single helper on purpose. The chat is DOM-windowed, so re-rendering reuses
 * existing rows and the name baked in at build time survives it — every surface
 * has to be patched in place, and doing that per call site missed one four
 * times running. Any new element rendering a person's name should tag itself
 * `data-npub` and be added to the selector here, not handled somewhere else.
 */
function refreshRenderedName(npub) {
    if (!npub) return;
    profileChanged(npub);

    const cProfile = getProfile(npub);
    const strName = getName(cProfile || npub);
    const sel = (cls) => `${cls}[data-npub="${npub}"]`;
    for (const el of document.querySelectorAll(
        [sel('.dmsg-author'), sel('.dmsg-command-author'), sel('.dmsg-command-bot')].join(', ')
    )) {
        el.textContent = strName;
        twemojify(el);
    }
    // System-event lines phrase the name their own way ("X has joined"), so
    // they take the same treatment through their own formatter.
    for (const el of document.querySelectorAll(sel('.system-event-name'))) {
        el.textContent = systemEventName(npub);
        twemojify(el);
    }

    // The open chat's header names the DM's peer; a group's header is the
    // group's own name and must not be relabelled.
    if (strOpenChat) {
        const cOpen = arrChats.find(c => c.id === strOpenChat);
        if (cOpen && !chatIsGroup(cOpen) && cOpen.id === npub) {
            setChatHeader(cOpen);
        }
    }
}

function blockedBySync() {
    if (!fSyncing) return false;
    showToast('Syncing your account — try again in a moment');
    return true;
}

/** Has the pinned list been pulled from the backend this session? */
let _pinnedLoaded = false;

/**
 * Pull the pinned list once, from whichever path paints the chat list first.
 *
 * There are three boot paths — login (`init_finished`), `init()`, and dev
 * hot-reload, which hydrates and renders without either — so hooking them
 * individually leaves whichever one nobody remembered painting unpinned.
 */
async function ensurePinnedLoaded() {
    if (_pinnedLoaded) return;
    _pinnedLoaded = true;
    try {
        arrPinnedChats = await invoke('get_pinned_chats') || [];
        listChanged();
    } catch (e) {
        _pinnedLoaded = false; // a failed load must be retryable, not latched
        console.error('Failed to load pinned chats:', e);
    }
}

/**
 * The one chat-list ordering: pinned first in pin order, then newest activity.
 */
function sortChats() {
    ensurePinnedLoaded();
    arrChats.sort((a, b) => {
        const ra = chatPinRank(a), rb = chatPinRank(b);
        if (ra !== rb) {
            if (ra === -1) return 1;
            if (rb === -1) return -1;
            return ra - rb;
        }
        return getChatSortTimestamp(b) - getChatSortTimestamp(a);
    });
}

/**
 * One chat's own activity time: its newest conversation message, or its join/creation
 * time when it has none.
 * @param {Chat} chat
 * @returns {number}
 */
function getChatOwnSortTimestamp(chat) {
    // Find the latest actual conversation message — skip system events so a
    // wallpaper change doesn't bubble the chat to the top of the chatlist.
    let lastMessage = null;
    if (chat.messages?.length) {
        for (let i = chat.messages.length - 1; i >= 0; i--) {
            const m = chat.messages[i];
            if (m.system_event) continue;
            lastMessage = m;
            break;
        }
    }
    if (lastMessage?.at) return lastMessage.at;

    // No real messages yet — fall back to join/creation time so a fresh community
    // sorts by when we joined, not to the bottom. custom_fields values are strings.
    let t = Number(
        chat.metadata?.created_at ||
        chat.metadata?.custom_fields?.created_at ||
        chat.created_at ||
        0
    );
    // Mixed clocks: custom_fields.created_at is ms, the chat row's created_at is
    // SECONDS — unnormalized seconds sort 1000× older than every ms timestamp.
    if (t > 0 && t < 1e12) t *= 1000;
    return t || 0;
}

/**
 * Compute a timestamp for sorting chats, falling back to metadata for empty groups.
 * A community's row stands in for the whole community, so it sorts on the newest
 * activity across ALL its channels — otherwise a busy secondary channel leaves the
 * community stranded at the bottom of the list behind its own quiet primary row.
 * @param {Chat} chat
 * @returns {number}
 */
function getChatSortTimestamp(chat) {
    let lastActivity = getChatOwnSortTimestamp(chat);
    const communityId = chatIsGroup(chat) && isPrimaryChannelChat(chat)
        ? chat.metadata?.custom_fields?.community_id
        : null;
    if (communityId) {
        for (const sibling of arrChats) {
            if (sibling === chat || sibling.chat_type !== 'Community') continue;
            if (sibling.metadata?.custom_fields?.community_id !== communityId) continue;
            // Own-timestamp only: two rows of one community both claiming to be primary
            // (legacy rows with no stamp) would otherwise recurse into each other.
            lastActivity = Math.max(lastActivity, getChatOwnSortTimestamp(sibling));
        }
    }

    // Monotonic ratchet: the key never regresses for the lifetime of the chat
    // object. A control-state purge empties `chat.messages`, and without this
    // the key fell back to the community's CREATION time — the chat sank down
    // the list mid-session, unread badge and all, until its cache repopulated.
    const ratchet = Math.max(lastActivity || 0, chat._sortStamp || 0);
    chat._sortStamp = ratchet;
    return ratchet;
}

/**
 * Lazy-load a message-less community's latest membership event into `chat.lastSystemEvent` so the
 * chat-list preview shows "X has joined" instead of "No messages yet". One-shot per chat (guarded);
 * a community with a real message needs nothing. On resolve, requests the actor's profile so the
 * npub-stub upgrades to a name (via profile_update), then patches just this row.
 */
function ensureCommunityPreviewActivity(chat) {
    if (!chat || chat._sysEvRequested) return;
    if ((chat.messages || []).some(m => !m.system_event)) return; // a real message already drives the preview
    chat._sysEvRequested = true;
    invoke('get_system_events', { conversationId: chat.id }).then(events => {
        if (!events || !events.length) return;
        const latest = events.reduce((a, b) => (b.at > a.at ? b : a));
        chat.lastSystemEvent = { event_type: latest.event_type, member_npub: latest.member_npub, at: latest.at };
        const np = latest.member_npub;
        if (np && !arrProfiles.some(p => p.id === np) && !strangerProfileRequested.has(np)) {
            strangerProfileRequested.add(np);
            invoke('load_profile', { npub: np }).catch(() => {});
        }
        updateChatlistPreview(chat.id);
    }).catch(() => {});
}


/**
 * Resolve a just-picked image path to a webview-displayable <img> src. On Android the file picker
 * returns a content:// URI that convertFileSrc can't render (broken preview), so cache_android_file
 * reads it and hands back a base64 preview; desktop uses the asset path directly. Returns null when
 * no preview is available, so the caller can keep its placeholder instead of showing a broken image.
 */
async function pickedImagePreviewSrc(path) {
    if (!path) return null;
    if (platformFeatures.os !== 'android') return convertFileSrc(path);
    try {
        const info = await invoke('cache_android_file', { filePath: path });
        if (info?.preview) return info.preview;
    } catch (e) {
        console.error('[preview] cache_android_file failed:', e);
    }
    return null;
}

/** Extract a valid npub from a bare npub string or a vectorapp.io profile URL */
function extractNpub(input) {
    const trimmed = (input || '').trim();
    if (/^npub1[a-z0-9]{58}$/i.test(trimmed)) return trimmed.toLowerCase();
    const m = trimmed.match(/https?:\/\/vectorapp\.io\/profile\/(npub1[a-z0-9]{58})/i);
    if (m) return m[1].toLowerCase();
    return null;
}

/** Track npubs we've already fired load_profile for (to avoid duplicate relay lookups) */
const strangerProfileRequested = new Set();

/** Active invite-modal re-render callback (set while the invite modal is open) */
let activeInviteModalRerender = null;

/**
 * Get a profile by npub
 * @param {string} npub - The user's npub
 * @returns {Profile|undefined} - The profile if it exists
 */
// Profile lookup index. Every row, badge and rail derivation resolves profiles by id,
// and a linear scan over thousands of profiles was the dominant cost of a list update.
// Rebuilt when the array is swapped, extended in place when it grows; the one site
// that replaces an entry (profile_update) re-points its key.
let profileIndex = new Map();
let profileIndexOf = null;
let profileIndexLen = 0;
function getProfile(npub) {
    if (profileIndexOf !== arrProfiles) {
        profileIndex = new Map();
        profileIndexOf = arrProfiles;
        profileIndexLen = 0;
    }
    if (profileIndexLen !== arrProfiles.length) {
        if (profileIndexLen > arrProfiles.length) { profileIndex = new Map(); profileIndexLen = 0; }
        for (let i = profileIndexLen; i < arrProfiles.length; i++) {
            const p = arrProfiles[i];
            if (p?.id) profileIndex.set(p.id, p);
        }
        profileIndexLen = arrProfiles.length;
    }
    return profileIndex.get(npub);
}

/**
 * Get the avatar src for a profile: the backend-cached local file, or null
 * (placeholder) while the cache is empty.
 *
 * NEVER fall back to the remote `profile.avatar` URL: it's attacker-chosen
 * kind-0 data and a WebView fetch of it bypasses Tor — worst exactly when
 * the backend cache download is still pending/blackholed during bootstrap.
 * @param {Profile} profile - The profile object
 * @returns {string|null} - The avatar src to use, or null if none available
 */
function getProfileAvatarSrc(profile) {
    if (!profile) return null;
    if (profile.avatar_cached) {
        return convertFileSrc(profile.avatar_cached);
    }
    return null;
}

/**
 * Get the banner src for a profile: backend-cached local file or null.
 * Same rule as getProfileAvatarSrc — never emit the remote URL.
 * @param {Profile} profile - The profile object
 * @returns {string|null} - The banner src to use, or null if none available
 */
function getProfileBannerSrc(profile) {
    if (!profile) return null;
    if (profile.banner_cached) {
        return convertFileSrc(profile.banner_cached);
    }
    return null;
}

/**
 * Create an avatar image element with automatic fallback to placeholder on error
 * @param {string} src - The image source URL
 * @param {number} size - The size of the avatar in pixels
 * @param {boolean} isGroup - Whether this is a group avatar (affects placeholder)
 * @returns {HTMLElement} - Either an img element or a placeholder div
 */
function createAvatarImg(src, size, isGroup = false) {
    if (!src) {
        return createPlaceholderAvatar(isGroup, size);
    }

    const img = document.createElement('img');
    img.src = src;
    img.style.width = size + 'px';
    img.style.height = size + 'px';
    img.style.objectFit = 'cover';
    img.style.borderRadius = '50%';

    // On error, replace with placeholder
    img.onerror = function() {
        const placeholder = createPlaceholderAvatar(isGroup, size);
        // Copy over any classes from the failed img
        placeholder.className = img.className;
        img.replaceWith(placeholder);
    };

    return img;
}

/* ── Member sections ───────────────────────────────────────────────────────
 * The same shape the channel pane uses: `{ id, label }` plus its rows, a head
 * that collapses it, and the closed set kept across restarts. Grouping is
 * decided by the caller, so the day roles arrive nothing here changes.
 */

const MEMBER_SECTION_KEY = 'ws_member_sections_closed';

function loadClosedMemberSections() {
    try {
        return new Set(JSON.parse(localStorage.getItem(MEMBER_SECTION_KEY) || '[]'));
    } catch {
        return new Set();
    }
}

let closedMemberSections = loadClosedMemberSections();

function memberSectionClosed(communityId, sectionId) {
    return closedMemberSections.has(`${communityId}:${sectionId}`);
}

/** Remember a roster section's collapsed state per community (the island flips the DOM). */
function setMemberSectionClosed(communityId, sectionId, closing) {
    const key = `${communityId}:${sectionId}`;
    if (closing) closedMemberSections.add(key);
    else closedMemberSections.delete(key);
    try {
        localStorage.setItem(MEMBER_SECTION_KEY, JSON.stringify([...closedMemberSections]));
    } catch { /* a full quota must not break the roster */ }
}


/**
 * Tracks if we're in the initial chat open period for auto-scrolling
 */
let chatOpenAutoScrollTimer = null;

/**
 * Tracks the timestamp when a chat was opened for media load auto-scrolling
 */
let chatOpenTimestamp = 0;

let maintenanceLoopStarted = false;
function startMaintenanceLoop() {
    if (maintenanceLoopStarted) return;
    maintenanceLoopStarted = true;
    let maintenanceTick = 0;
    setInterval(() => {
        maintenanceTick++;

        // Widescreen keeps the list pane on screen via CSS (`body.ws #chats { display:
        // flex !important; }`) whatever the tab's inline display says — gate on the
        // live layout, not the inline style, or the list never ticks in wide mode.
        const listOnScreen = domChats.style.display !== 'none'
            || (typeof wsActive === 'function' && wsActive());

        // Clear expired typing indicators (every tick)
        const now = Date.now() / 1000;
        arrChats.forEach(chat => {
            if (chat.active_typers && chat.active_typers.length > 0) {
                // Clear the array if we haven't received an update in 30 seconds
                if (!chat.last_typing_update || now - chat.last_typing_update > 30) {
                    chat.active_typers = [];

                    // If this is the open chat, refresh the display
                    if (strOpenChat === chat.id) {
                        updateChatHeaderSubtext(chat);
                    }

                    // Refresh chat list (in-place; typing doesn't affect sort order)
                    if (listOnScreen) {
                        updateChatlistPreview(chat.id);
                    }
                }
            }
        });

        // Update chatlist timestamps every 6th tick (~30 seconds)
        if (maintenanceTick % 6 === 0 && listOnScreen) {
            updateChatlistTimestamps();
        }
    }, 5000);
}

/**
 * Synchronise all messages from the backend
 */
async function init(skipAccountCheck = false) {
    // Check if account is selected (skip during boot — we just logged in)
    if (!skipAccountCheck) {
        try {
            await invoke("get_current_account");
        } catch (e) {
            console.log('[Init] No account selected, triggering fetch_messages');
            await invoke("fetch_messages", { init: true });
            return;
        }
    }

    // UI maintenance: typing-indicator expiry + chatlist timestamp refresh.
    // Extracted so the dev-mode hot-reload path can also start it — that path
    // hydrates state and renders the UI without going through init(), which
    // would otherwise leave typing indicators stuck on hot-reloads.
    startMaintenanceLoop();

    // Proceed to load and decrypt the database, and begin iterative Nostr synchronisation
    await invoke("fetch_messages", { init: true });

    // Begin an asynchronous loop to refresh profile data
    fetchProfiles().finally(async () => {
        setAsyncInterval(fetchProfiles, 45000);
    });

    // Display pending Community invites.
    await loadCommunityInvites();

    // Preload each community's admin roster so admin tags + @everyone render from the first paint,
    // not only after the Group Info panel has been opened (which used to be the sole roster loader).
    const seenCommunities = new Set();
    for (const c of arrChats) {
        const cid = c.chat_type === 'Community' ? c.metadata?.custom_fields?.community_id : null;
        if (cid && !seenCommunities.has(cid)) {
            seenCommunities.add(cid);
            loadCommunityRoles(cid);
        }
    }
}


// ── Community invites (pending npub gift-wraps) ──────────────────────────────

/**
 * A "thread" function dedicated to refreshing Profile data in the background
 * Also runs periodic maintenance tasks (cache cleanup, etc.)
 */
async function fetchProfiles() {
    // Use the new profile sync system
    await invoke("sync_all_profiles");

    // Run periodic maintenance (cache cleanup, memory optimization)
    invoke("run_maintenance").catch(() => {});
}

/**
 * Replace @npub1... mentions in text with display names for previews/notifications
 * @param {string} text
 * @returns {string}
 */
function resolveMentionText(text) {
    if (!text) return text;
    // Same shapes renderMentions pills: @-, nostr:-prefixed, or bare npubs.
    return text.replace(/(?<![\w/=?&#%.-])(?:@|nostr:)?(npub1[a-z0-9]{58})\b/g, (full, npub) => {
        const profile = getProfile(npub);
        if (profile) {
            return '@' + getName(npub);
        }
        return full;
    });
}

/** Whether the chat header's back dot lights: another visible chat has unread, or an
 *  invite waits. The header derives it from the list, invite and chat signals. */
function chatBackDotWanted() {
    if (!strOpenChat) return false;
    if (arrCommunityInvites.length > 0) return true;
    // Only chats the user can actually SEE and open count, with the SAME badge count as
    // the chatlist rows, so the dot can't light for something with no row to visit.
    return arrChats.some(chat => chat.id !== strOpenChat && chatIsVisibleInList(chat) && computeRowBadgeCount(chat) > 0);
}

/**
 * Sets a specific message as the last read message
 * @param {Chat} chat - The Chat to update
 * @param {Message|string} message - The Message to set as last read
 */
/** Walk a messages array backward and return the latest "contact" message —
 *  non-mine AND not a system event. System events are status notifications,
 *  not conversation, so they must not be picked as the markAsRead anchor —
 *  otherwise `last_read` lands on the system event itself and prior contact
 *  messages re-surface as unread on the next walk. */
function findLatestContactMessage(messages, maxAt = Infinity) {
    if (!messages?.length) return null;
    for (let i = messages.length - 1; i >= 0; i--) {
        const m = messages[i];
        if (m.system_event) continue;
        // An own message proves we read up to ITS time, not past it: a boot/catch-up
        // sweep replays our OLD sends, and marking newer contact messages read off a
        // stale own-send would silently swallow genuinely-unread arrivals.
        if (m.at > maxAt) continue;
        if (!m.mine) return m;
    }
    return null;
}

function markAsRead(chat, message, explicit = false) {
    // A chat the user just marked unread stays unread until they open it or
    // explicitly mark it read — otherwise the ambient sweeps (focus, repaint,
    // scroll) undo the action instantly, which reads as a flicker.
    if (chat && isChatUnreadLatched(chat.id)) {
        if (explicit || chat.id === strOpenChat) {
            clearChatUnreadLatch(chat.id);
        } else {
            return;
        }
    }
    // If we have a chat, and we haven't already marked as read, update its last_read and notify backend
    if (chat && message.id !== chat.last_read) {
        chat.last_read = message.id;
        // Optimistic clear so the badge drops instantly on read; the debounced DB refresh below
        // is authoritative (corrects the rare case where a newer non-mine message remains unread).
        chat.unread = 0;

        // Persist via backend using chat-based API
        invoke("mark_as_read", { chatId: chat.id, messageId: message.id });

        // Widescreen keeps the list, its channels and the rail mounted beside the
        // open chat, so the optimistic clear has to repaint NOW: the refresh below
        // compares against the value we just set, finds no change, and repaints
        // nothing.
        chatChanged(chat);

        // The read advanced — re-derive this chat's unread from the DB (authoritative).
        scheduleUnreadRefresh();
    }
}

/** Mark a chat fully caught-up: advance last_read to the newest CONTACT message, never the raw
 *  window tail. The tail can be a system event (kind 30078), and pinning last_read there gives the
 *  unread query a row it can't anchor on, wedging the badge at a permanent 99+. No-op when the
 *  window holds no contact message (nothing non-mine can be unread). Used by the jump/reveal paths. */
function markChatCaughtUp(chat, explicit = false) {
    const caughtUp = findLatestContactMessage(chat?.messages);
    if (caughtUp) markAsRead(chat, caughtUp, explicit);
}

/** True when the chat's newest conversational message is from the other side (not us, not a
 *  system event) — i.e. there's something to flag as unread. The precondition for the action. */
function chatCanMarkUnread(chat) {
    const msgs = chat?.messages;
    if (!msgs?.length) return false;
    for (let i = msgs.length - 1; i >= 0; i--) {
        if (msgs[i].system_event) continue;
        return !msgs[i].mine;
    }
    return false;
}

/** Mark a chat unread: the backend retreats last_read to just before the newest contact message,
 *  computed from the full DB history (a community row may hold only a preview message in RAM, so we
 *  can't pick the anchor here). Repaints only when the backend actually marked it, then re-derives
 *  the authoritative count — so a no-op (we spoke last) never flashes a badge that snaps back. */
async function markChatUnread(chat) {
    let lastRead = null;
    try { lastRead = await invoke('mark_as_unread', { chatId: chat.id }); } catch (_e) {}
    if (lastRead === null || lastRead === undefined) return; // no-op (we spoke last / nothing to surface)
    // Keep the cached marker in lock-step with the DB (empty string = never-read) so a follow-up
    // Mark as Read isn't skipped by markAsRead's "already at last_read" guard.
    chat.last_read = lastRead;
    chat.unread = Math.max(1, chat.unread || 0);
    // Latch the deliberate retreat: the closed chat still gets auto-marked by
    // list repaints / window-focus sweeps, which would instantly undo it. The
    // latch clears the moment the user actually opens the chat.
    setChatUnreadLatch(chat.id);
    chatChanged(chat);
    refreshUnreadCounts();
}

/** Chats the user explicitly marked unread. While latched, the ambient
 *  auto-mark-read paths (focus regain, scroll-to-bottom, list repaint) leave
 *  the chat alone — only OPENING it counts as reading. */
const setChatsMarkedUnread = new Set();
function setChatUnreadLatch(chatId) { if (chatId) setChatsMarkedUnread.add(chatId); }
function clearChatUnreadLatch(chatId) { if (chatId) setChatsMarkedUnread.delete(chatId); }
function isChatUnreadLatched(chatId) { return setChatsMarkedUnread.has(chatId); }

/**
 * Per-chat unread badges are sourced from the DB (`chat.unread`), not by walking in-memory
 * messages — so they're correct even after a restart, when only the last message per chat is in
 * RAM. This fetches the authoritative counts and updates every chat. Awaited at boot; elsewhere use
 * the debounced `scheduleUnreadRefresh` so a burst of arrivals coalesces into one query.
 */
async function refreshUnreadCounts() {
    let counts;
    try {
        counts = await invoke('get_unread_counts');
    } catch (e) {
        return; // keep prior chat.unread on failure
    }
    // Only the rows whose count moved re-derive.
    let changed = false;
    for (const chat of arrChats) {
        const n = counts[chat.id] || 0;
        if (chat.unread !== n) { chat.unread = n; touchChatRow(chat); changed = true; }
    }
    if (changed) reorderChatlist();
}

let _unreadRefreshTimer = null;
function scheduleUnreadRefresh() {
    if (_unreadRefreshTimer) clearTimeout(_unreadRefreshTimer);
    // Trailing debounce: run AFTER the burst settles so the DB reflects every just-persisted
    // message/read, avoiding a stale snapshot mid-flight.
    _unreadRefreshTimer = setTimeout(() => { _unreadRefreshTimer = null; refreshUnreadCounts(); }, 200);
}

/**
 * Send a NIP-17 message to a Nostr user
 * @param {string} pubkey - The user's pubkey
 * @param {string} content - The content of the message
 * @param {string?} replied_to - The reference of the message, if any
 * @param {string?} bot - Command routing: the chosen bot's npub (community sends only;
 *                        a DM's recipient IS the bot, so no tag is needed)
 */
async function message(pubkey, content, replied_to, bot) {
    // Community channels send through their own envelope path (the DM/MLS `message`
    // command can't address a channel id). The backend drives the pending→sent/failed
    // lifecycle (optimistic bubble + finalize), so there's no pending id to finalize here.
    const chat = arrChats.find(c => c.id === pubkey);
    if (chat && chat.chat_type === 'Community') {
        await invoke('send_community_message', { channelId: pubkey, content, repliedTo: replied_to || '', bot: bot || null });
        return;
    }
    const result = await invoke("message", { receiver: pubkey, content: content, repliedTo: replied_to });
    if (result && result.event_id) {
        finalizePendingMessage(pubkey, result.pending_id, result.event_id);
    }
}

/**
 * Send an emoji reaction, routing Community channels to their own command (the DM/MLS
 * `react_to_message` can't address a channel id). Custom-emoji images aren't carried in
 * the Community envelope yet, so a community reaction sends the emoji/shortcode content.
 */
function reactToMessageRouted(referenceId, chatId, emoji, emojiUrl) {
    const chat = arrChats.find(c => c.id === chatId);

    // Group ceiling + per-tier fresh-reaction allowance (joins always pass) —
    // gate BEFORE any optimistic bookkeeping so nothing strands on refusal.
    const gateReason = reactionTierGate(chat?.messages.find(m => m.id === referenceId), emoji);
    if (gateReason) {
        showToast(gateReason);
        return Promise.resolve(null);
    }

    // Reactions are a real "use" of the emoji — record it (single chokepoint for
    // stock + custom reactions). Custom reactions arrive as `:shortcode:` + a url.
    if (emojiUrl) {
        bumpEmojiUsage('custom', emoji.replace(/^:|:$/g, ''), emojiUrl);
    } else {
        bumpEmojiUsage('unicode', emoji);
    }
    if (chat && chat.chat_type === 'Community') {
        return invoke('react_to_community_message', { channelId: chatId, messageId: referenceId, emoji, emojiUrl: emojiUrl || null });
    }
    const args = { referenceId, chatId, emoji };
    if (emojiUrl) args.emojiUrl = emojiUrl;
    return invoke('react_to_message', args);
}

/**
 * Send a file via NIP-96 server to a Nostr user or group
 * @param {string} pubkey - The user's pubkey or group_id
 * @param {string?} replied_to - The reference of the message, if any
 * @param {string} filepath - The absolute file path
 */
async function sendFile(pubkey, replied_to, filepath) {
    try {
        // Community channels send through their own envelope path (multi-attachment
        // capable). The backend drives the pending → sent/failed lifecycle, so there's
        // no pending id to finalize here (mirrors send_community_message).
        const chat = arrChats.find(c => c.id === pubkey);
        if (chat && chat.chat_type === 'Community') {
            await invoke('send_community_files', { channelId: pubkey, content: '', filePaths: [filepath], nameOverrides: [''], useCompression: false, keepMetadata: false, repliedTo: replied_to || '' });
        } else {
            // DMs use the protocol-agnostic file_message command.
            const result = await invoke("file_message", { receiver: pubkey, repliedTo: replied_to, filePath: filepath, keepMetadata: false, nameOverride: '' });
            if (result && result.event_id) {
                finalizePendingMessage(pubkey, result.pending_id, result.event_id);
            }
        }
    } catch (e) {
        // User-initiated cancel — the pending bubble is already gone; no error toast.
        if (e && e.toString().includes('Upload cancelled')) { nLastTypingIndicator = 0; return; }
        const { title, body } = humanizeUploadError(String(e));
        popupConfirm(title, body, true, '', 'vector_warning.svg');
    }
    nLastTypingIndicator = 0;
}

/** Raw upload error → user-friendly { title, body }. Technical detail
 *  is appended in small text for users who want to dig in. */
function humanizeUploadError(raw) {
    const lower = raw.toLowerCase();
    const technical = `<br><br><span style="opacity: 0.5; font-size: 12px;">${escapeHtml(raw)}</span>`;

    if (/status\s+413/.test(lower) || /payload too large/.test(lower)) {
        return {
            title: 'File too large',
            body: 'None of your media servers will accept a file this big. Try a smaller file, or add a server that supports larger uploads in Settings → Network.' + technical,
        };
    }
    if (/status\s+415/.test(lower)
        || /file could not be processed/.test(lower)
        || /file type not detected/.test(lower)
        || /not allowed/.test(lower)
        || /unsupported/.test(lower)) {
        return {
            title: 'File type not supported',
            body: 'Your media servers don\'t accept this kind of file. Try a different file format, or add a server with broader file type support in Settings → Network.' + technical,
        };
    }
    if (/status\s+401/.test(lower) || /unauthorized/.test(lower)) {
        return {
            title: 'Media server rejected your account',
            body: 'This server refused Vector\'s upload signature. It may require allowlisting or paid access. Open Settings → Network to swap in a server that accepts your account.' + technical,
        };
    }
    if (/all blossom servers failed/.test(lower)) {
        return {
            title: 'No media server could take this file',
            body: 'Every media server you have configured rejected the upload. Open Settings → Network to see which servers you have enabled, or add one that supports your file.' + technical,
        };
    }
    return {
        title: 'File send failed',
        body: 'Vector could not send this file. Check your connection and try again, or pick a different file.' + technical,
    };
}


/**
 * A flag that indicates when Vector is still in it's initiation sequence
 */
let fInit = true;

/**
 * Execute a deep link action (profile, etc.)
 * @param {Object} payload - The action payload with action_type and target
 */
async function executeDeepLinkAction(payload) {
    const { action_type, target } = payload;
    if (action_type === 'profile') {
        // Open the profile for the given npub
        // First, try to find an existing profile in our cache
        let profile = arrProfiles.find(p => p.id === target);

        if (!profile) {
            // Profile not in cache - create a minimal profile object
            // The openProfile function will trigger a refresh from the network
            profile = { id: target };
        }

        // Store the current chat so we can return to it
        previousChatBeforeProfile = strOpenChat;

        // Open the profile view
        await openProfile(profile);
    } else if (action_type === 'chat') {
        // Open a specific chat (triggered by tapping a notification)
        await openChat(target);
    } else if (action_type === 'emoji_pack') {
        // Open the Pack Details modal for the given naddr. The modal
        // owns the fetch, render, and subscribe/unsubscribe flow; we
        // just hand it the address.
        if (typeof openPackDetailsModal === 'function') {
            await openPackDetailsModal(target);
        }
    } else if (action_type === 'community_invite') {
        // Invite link (vector://invite#… or vectorapp.io/invite#…) — `target` is the full URL;
        // the join flow re-parses its fragment, previews, and accepts on confirm.
        await previewAndJoinCommunityLink(target);
    }
}

/**
 * A flag that indicates when the initial sync is complete
 * This is separate from fInit because sync continues after UI init
 */
let fSyncComplete = false;

/**
 * Our Bech32 Nostr Public Key
 */
let strPubkey;

window.addEventListener("DOMContentLoaded", async () => {
    // Once login fade-in animation ends, remove it
    domLogin.addEventListener('animationend', () => domLogin.classList.remove('fadein-anim'), { once: true });

    // Fetch platform features to determine OS-specific behavior
    await fetchPlatformFeatures();

    // Downgrade gate, before any other boot step: this build must not touch a
    // database a newer Vector wrote. Terminal by design — nothing below runs.
    try {
        const downgrade = await invoke('check_account_downgrade');
        if (downgrade) {
            await showDowngradeBlock(downgrade);
            return;
        }
    } catch (e) {
        // A failure here must not strand a healthy account on a blank screen;
        // init_database still refuses the open if this really was a downgrade.
        console.error('Downgrade check failed:', e);
    }

    // Initialize relay dialog event listeners
    initRelayDialogs();

    // Wire the multi-account UI — both the in-app dropdown and the pre-login
    // picker register their event listeners here. Safe to call before login
    // because both surfaces lazily fetch their data when first opened.
    profileSwitcher.init();
    loginPicker.init();

    // Set up early deep link listener BEFORE login flow
    // This handles deep link events that arrive while the app is running
    // Note: Deep links received before JS loads are stored in Rust and retrieved after login
    await listen('deep_link_action', async (evt) => {
        // If user is not logged in yet (fInit is true), ignore - Rust already stored it
        if (fInit) {
            console.log('Deep link received before login, Rust backend has stored it');
            return;
        }

        // Consume through the pending slot (returns the stored action and CLEARS it):
        // executing evt.payload directly would leave the pending copy behind to
        // replay as a stale action on the next boot, and a double-delivered URL
        // (event + cold-start catch) would run twice.
        const action = await invoke('get_pending_deep_link').catch(() => null);
        if (action) await executeDeepLinkAction(action);
    });

    // Inbound share from another app (Android share sheet). If not logged in yet,
    // Rust has stored it pending and the post-login poll will pick it up. Route through the
    // atomic consume so a live event + a resume poll can't double-handle the same share.
    await listen('share_received', async () => {
        if (fInit) return;
        await consumePendingShare();
    });

    // Listen for critical loading errors from the backend (database, migrations, etc.)
    // Registered early so it catches errors from login_from_stored_key and login
    await listen('loading_error', (evt) => {
        console.error('[Boot] Loading error:', evt.payload);
        popupConfirm('Loading Error', evt.payload, true, '', 'vector_warning.svg');
    });

    // Bunker session events — must be registered EARLY (alongside
    // loading_error / session_reload), not inside setupRustListeners, because
    // the re-auth flow fires these while the user is still on the login
    // screen, before any successful login has booted the main listener set.
    await listen('bunker_state', (evt) => {
        const state = evt?.payload?.state;
        // Keep the Security panel's status dot in sync with live signer
        // health — cheap DOM update, no-op when the card is hidden.
        if (typeof applyRemoteSignerDot === 'function') applyRemoteSignerDot(state);
        // Toast is for steady-state signer health changes only. When the
        // bunker pairing form is up the form owns its own status display,
        // and the backend's Connecting/Online events during pre-commit pairing
        // would otherwise leak as misleading "signer online" toasts in the UI.
        const bunkerFormVisible = VectorSvelte.loginState().bunker;
        if (state === 'offline') {
            if (!bunkerFormVisible && !window.__bunkerOfflineToastShown) {
                if (typeof showToast === 'function') {
                    showToast('Remote signer offline. Please check your signer app.');
                }
                window.__bunkerOfflineToastShown = true;
            }
        } else if (state === 'online') {
            if (!bunkerFormVisible && window.__bunkerOfflineToastShown) {
                if (typeof showToast === 'function') {
                    showToast('Remote signer back online.');
                }
                window.__bunkerOfflineToastShown = false;
            }
        } else {
            window.__bunkerOfflineToastShown = false;
        }
    });

    // NIP-55 offline-signer health. Fires when a background op comes back
    // rejected (needs_auth) or the signer app is uninstalled (missing). Maps
    // onto the same Security-panel dot as bunker; a needs_auth in steady state
    // means Amber revoked a permission, so nudge the user to re-authorize.
    await listen('nip55_state', (evt) => {
        const state = evt?.payload?.state;
        if (typeof applyRemoteSignerDot === 'function') {
            // Dot tracks install health, not the noisy per-op state: only a
            // genuinely-gone signer goes red. A needs_auth blip (a kind Amber
            // didn't auto-approve) leaves the dot green and is surfaced by the
            // toast + Re-authorize button instead of painting the card broken.
            if (state === 'missing') applyRemoteSignerDot('offline');
            else if (state === 'ready') applyRemoteSignerDot('online');
        }
        if (state === 'needs_auth') {
            if (!window.__nip55NeedsAuthToastShown && typeof showToast === 'function') {
                showToast('Your signer needs re-authorization. Open Settings to reconnect.');
                window.__nip55NeedsAuthToastShown = true;
            }
        } else if (state === 'missing') {
            if (!window.__nip55MissingToastShown && typeof showToast === 'function') {
                showToast('Your signer app is not installed. Reinstall it to keep signing.');
                window.__nip55MissingToastShown = true;
            }
        } else if (state === 'ready') {
            window.__nip55NeedsAuthToastShown = false;
            window.__nip55MissingToastShown = false;
        }
    });

    await listen('bunker_awaiting_approval', () => {
        // Countdown is reroll-bound; once we're waiting on user approval
        // in the signer app, auto-reroll would be hostile.
        stopBunkerSessionTimer();
        const status = document.getElementById('bunker-status-text');
        if (status) {
            status.textContent = 'Check your signer app to approve…';
            status.className = 'login-bunker-status connecting';
        }
    });

    // Two terminal-success events for the bunker form; the choice depends on
    // whether the account already exists locally:
    //   `bunker_session_staged`         — first-time pairing. Account row not
    //       yet committed; UI hands off to the encryption-choice flow which
    //       writes the rolled-back settings via setup_encryption/skip.
    //   `bunker_reauthorize_succeeded`  — existing account regaining a live
    //       signer. Settings are already on disk; UI goes straight to login.
    await listen('bunker_session_staged', async (evt) => {
        stopBunkerSessionTimer();
        strPubkey = evt?.payload?.npub || strPubkey;
        const status = document.getElementById('bunker-status-text');
        if (status) {
            status.textContent = 'Connected. Choosing security…';
            status.className = 'login-bunker-status online';
        }
        if (typeof window.hideBunkerForm === 'function') window.hideBunkerForm();
        openEncryptionFlow(false);
        invoke('connect').catch((err) => {
            console.warn('[bunker_session_staged] connect() failed:', err);
        });
    });

    await listen('bunker_session_failed', (evt) => {
        // Failure during the pairing window almost always means the timeout
        // fired — auto-reroll a fresh QR so the user isn't stranded with a
        // dead code. Genuine relay-down errors will surface again on the
        // next attempt (and the countdown will resume from there).
        stopBunkerSessionTimer();
        const err = evt?.payload?.error || 'Signer connection failed';
        const status = document.getElementById('bunker-status-text');
        if (status) {
            status.textContent = String(err);
            status.className = 'login-bunker-status error';
        }
        // Only auto-reroll if the bunker form is actually visible — don't
        // start a fresh session if the user has navigated away.
        if (VectorSvelte.loginState().bunker) {
            setTimeout(() => {
                if (VectorSvelte.loginState().bunker) {
                    startBunkerSession();
                }
            }, 1500);
        }
    });

    await listen('bunker_reauthorize_succeeded', async (evt) => {
        stopBunkerSessionTimer();
        // Drain the one-shot recovery slot so a later reauth attempt in the
        // same session doesn't pick up this completed pairing's npub and
        // mistake it for a missed-event recovery.
        invoke('get_pending_reauth_result').catch(() => {});
        try {
            strPubkey = evt?.payload?.npub || strPubkey;
            // Form hidden = user backed out; the backend already swapped the
            // signer (identity matched, no harm done), but rebuilding the UI
            // mid-Settings would yank them out of where they are. Skip the
            // boot sequence — the live session is already healthy.
            const formVisible = VectorSvelte.loginState().bunker;
            if (!formVisible) return;
            const origin = bunkerReauthOrigin;
            if (typeof window.hideBunkerForm === 'function') window.hideBunkerForm();
            if (origin) {
                // Reauth from inside the app: the underlying session never
                // went down (only the signer handles were swapped), so
                // `login(true)`'s full boot would just dump us on the login
                // form. Mirror the Back-button restore — tear down the
                // bunker form and put the user back on the panel they came
                // from.
                VectorSvelte.loginScreen('none', false);
                VectorSvelte.loginShowForm(false);
                bunkerReauthOrigin = null;
                if (origin === 'settings' && typeof openSettings === 'function') {
                    openSettings();
                } else if (typeof closeChat === 'function') {
                    closeChat();
                }
            } else {
                // Reauth fired from the boot-time "Signer unreachable" popup
                // on the login screen — no session is up yet. Full boot.
                invoke('connect').catch((err) => {
                    console.warn('[bunker_reauthorize_succeeded] connect() failed:', err);
                });
                login(true);
            }
        } catch (e) {
            console.error('[bunker_reauthorize_succeeded] transition failed:', e);
        }
    });

    await listen('bunker_reauthorize_failed', (evt) => {
        stopBunkerSessionTimer();
        // Form hidden = user backed out; the in-flight bg task may still
        // eventually emit failure (timeout) — silently drop it since the
        // user already moved on and the live session is unchanged.
        const formVisible = VectorSvelte.loginState().bunker;
        if (!formVisible) return;
        const err = evt?.payload?.error || 'Re-authorization failed';
        const status = document.getElementById('bunker-status-text');
        if (status) {
            status.textContent = String(err);
            status.className = 'login-bunker-status error';
        }
        // Auto-reroll on reauth timeout too (same rationale as pairing).
        if (VectorSvelte.loginState().bunker) {
            setTimeout(() => {
                if (VectorSvelte.loginState().bunker) {
                    startBunkerSession();
                }
            }, 1500);
        }
    });

    await listen('bunker_auth_url', async (evt) => {
        const url = evt?.payload?.url;
        if (!url) return;
        // Restrict to http(s). The URL originates from the signer over a
        // relay; an attacker between us and the bunker could otherwise
        // push javascript:, file://, or platform-protocol URLs.
        let parsed = null;
        try { parsed = new URL(url); } catch (_) {}
        if (!parsed || (parsed.protocol !== 'http:' && parsed.protocol !== 'https:')) {
            console.warn('[bunker_auth_url] rejected non-http(s) URL:', url);
            return;
        }
        try {
            await openUrl(parsed.toString());
        } catch (err) {
            popupConfirm(
                'Approve in your signer',
                `Open this URL to approve the request:<br><br>${escapeHtml(parsed.toString())}`,
                true,
            );
        }
    });

    // Module-scope: callable from the boot-time login_from_stored_key catch.
    window.handleBunkerLoginError = async function handleBunkerLoginError(e) {
        const msg = String(e || '');
        const looksLikeBunkerOffline = msg.includes('Remote signer unreachable')
            || msg.toLowerCase().includes('bunker');
        if (!looksLikeBunkerOffline) return false;
        const wantsReauth = await popupConfirm(
            'Signer unreachable',
            'Your remote signer didn\'t respond. If you\'ve reset or revoked Vector\'s permissions in your signer app, re-pair below without losing your account data.<br><br>'
                + escapeHtml(msg),
            false,
            '',
            'vector_warning.svg',
            '',
            'Re-authorize Signer'
        );
        if (wantsReauth && typeof window.showBunkerForm === 'function') {
            window.showBunkerForm('reauth');
            return true;
        }
        return false;
    };

    // Multi-account: listen for `session_reload` from `swap_session`. Must be
    // registered HERE (DOMContentLoaded) — not inside `setupRustListeners`,
    // which only fires after a successful login. The pre-login picker emits
    // `swap_session` from the unlock screen, well before any login completes.
    // Everything from here to the show() below runs BEFORE the window is visible,
    // so anything that throws or never settles strands a running app with no GUI.
    // Each step guards itself rather than trusting the backend to answer.
    try {
        await listen('session_reload', () => {
            window.location.reload();
        });
    } catch (e) {
        console.warn('session_reload listener failed to register:', e);
    }

    // Immediately load and apply theme settings (visual only, don't save)
    try {
        const strTheme = await invoke('get_theme');
        if (strTheme) {
            applyTheme(strTheme);
        }
    } catch (e) {
        console.warn('Theme preload failed; showing with the default:', e);
    }

    // Show the main window now that content is ready (prevents white flash on startup)
    // The window starts hidden via tauri.conf.json and Rust setup hides it explicitly
    // The WKWebView background is set to dark natively in lib.rs so no delay is needed
    // Only needed on desktop - mobile doesn't have this issue
    // Optional-chained on purpose: platformFeatures is a global filled by another
    // bootstrap, and reading `.is_mobile` off an undefined one threw one line
    // short of the only call that reveals the window. Desktop is the safe
    // default — a stray show() on mobile is a no-op.
    if (!platformFeatures?.is_mobile) {
        try {
            await getCurrentWebviewWindow().show();
        } catch (e) {
            console.warn('Failed to show main window:', e);
        }
    }

    // [DEBUG MODE] Check if backend already has state from a previous session (hot-reload scenario)
    // This allows skipping the entire login/decrypt flow during development hot-reloads
    let fDebugHotReloaded = false;
    if (platformFeatures.debug_mode) {
        try {
            const hotReloadState = await invoke('debug_hot_reload_sync');
            if (hotReloadState && hotReloadState.success) {
                console.log('[Debug Hot-Reload] Backend state recovered, skipping login flow');
                
                // Hydrate frontend state from backend
                strPubkey = hotReloadState.npub;
                arrProfiles = hotReloadState.profiles || [];
                arrChats = hotReloadState.chats || [];
                // Seeded from the payload, so the first paint is already in pin order
                // rather than correcting itself an IPC hop later.
                arrPinnedChats = hotReloadState.pinned || [];
                _pinnedLoaded = true;

                // Setup Rust listeners
                await setupRustListeners();

                // Resolve Community logos (metadata already rides the chat payload).
                resolveCommunityAvatars();

                // Warm the emoji set + frecency. The login flow does this in its `init_finished`
                // handler, which hot-reload skips — so without it `arrEmojiPacks`/frecency stay empty
                // after a dev refresh until the picker is first opened (defaults-only `:` autocomplete
                // and a stale first panel open).
                loadEmojiPacks();
                loadEmojiUsage();

                // Hide login UI and show main UI
                VectorSvelte.loginHide();
                domNavbar.style.display = '';
                domChatBookmarksBtn.style.display = 'flex';
                
                // Render our profile
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = '';
                
                // Mark init as complete so renderChatlist works
                fInit = false;
                wsUpdate();
                // Catch a share that landed between the cold-start poll and now (the live listener
                // skips events while fInit was still true).
                consumePendingShare();
                // Same window exists for deep links: drain any action stored while
                // fInit gated the live listener.
                invoke('get_pending_deep_link').then(a => { if (a) executeDeepLinkAction(a); }).catch(() => {});

                mountChatlist();

                // Show the New Chat buttons (same as normal login flow)
                if (domChatNewDM) {
                    domChatNewDM.style.display = '';
                    domChatNewDM.onclick = openNewChat;
                }
                if (domChatNewGroup) {
                    domChatNewGroup.style.display = '';
                    domChatNewGroup.onclick = openCreateGroup;
                }
                
                // Adjust sizes
                adjustSize();
                
                // Update unread counter
                await invoke('update_unread_counter');

                // Re-apply badge-gated perks (raised emoji-pack limits). Hot-reload skips the login
                // flow where this normally runs, so without it the Vector badge benefits silently
                // revert to the default limits across a dev refresh. The flag is cached, so no network.
                invoke('get_my_badges').then(b => {
                    _myBadges = b;
                    applyTierLimits(b?.tier | 0);
                }).catch(() => {});

                // Monitor relay connections and render relay list
                invoke("monitor_relay_connections");
                renderRelayList();
                
                // Initialize the updater (version info, update button)
                initializeUpdater();
                
                console.log(`[Debug Hot-Reload] Restored ${arrProfiles.length} profiles, ${arrChats.length} chats`);

                // Hot-reload skips init(), which is where the typing-indicator
                // expiration sweep normally registers. Start it explicitly so
                // dev sessions don't accumulate stuck typing indicators.
                startMaintenanceLoop();

                // Mark as hot-reloaded so we skip the login flow but continue with button setup
                fDebugHotReloaded = true;
            }
        } catch (e) {
            // Backend not initialized - continue normal flow
            console.log('[Debug Hot-Reload] Backend not initialized, proceeding with normal login');
        }
    }

    // Single IPC call: account existence + encryption status
    // (boot_select_account already ran at Tauri startup, so this is just a static read + 1 DB query)
    if (!fDebugHotReloaded) {
        console.time('[Boot] getBootStatus');
        const { account_exists, enabled, security_type, signer_type } = await invoke('get_encryption_and_key');
        // Stash so the boot-time "Connecting…" title can be reworded for
        // bunker accounts (the 15s wait is dominated by the signer round-trip).
        window.__activeSignerType = signer_type || 'local';
        console.timeEnd('[Boot] getBootStatus');

        // Show the pre-login picker pill whenever ≥2 accounts exist on disk
        // — both for the normal multi-account boot AND for the "marker
        // missing, accounts on disk" recovery case where the backend
        // intentionally returns account_exists=false to defer the choice
        // to the user. Without this branch, marker-missing users would land
        // on the bare Create / Login screen with no visible path to their
        // existing accounts. `loginPicker.show()` is self-gating: it hides
        // the pill again if the account list ends up <2 long.
        if (account_exists) {
            try {
                const activeNpub = await invoke('get_current_account');
                await loginPicker.show(activeNpub);
            } catch (_) {
                // No current account or list failed — leave picker hidden.
            }
        } else {
            try {
                await loginPicker.show(null);
            } catch (_) { /* hide-on-fail handled inside show() */ }

            // Single-account recovery: if exactly ONE account is on disk
            // but the marker is missing, the user has no visible path to
            // their existing account (the picker pill hides for accounts
            // <2). Promote the lone account to active automatically —
            // `setActiveAndSwap` writes the marker and triggers a reload
            // which re-enters this boot flow with `account_exists: true`.
            // Without this, a user who lost their marker (Add Profile
            // abort, file corruption, manual delete) sees the bare
            // Create / Login screen with no indication their account
            // still exists on disk.
            if (loginPicker.accounts && loginPicker.accounts.length === 1) {
                const onlyNpub = loginPicker.accounts[0].npub;
                try {
                    await multiAccount.setActiveAndSwap(onlyNpub);
                    // The above triggers `session_reload` which reloads
                    // the page; control never returns here in practice.
                    return;
                } catch (e) {
                    // Could not promote (migration in flight, etc.) —
                    // fall through to Create / Login as a last resort.
                    console.error('[Boot] Single-account auto-promote failed:', e);
                }
            }
        }

        // PIVX default flip: a FRESH install (no account has ever existed on this
        // device) gets the wallet hidden until it's summoned via the Mini Apps
        // search. A device that has run Vector before keeps its state — visible
        // unless the user explicitly hid it. Stamped once, so deleting every
        // account later never re-hides a wallet the user has been using.
        if (!localStorage.getItem('pivx_default_applied')) {
            localStorage.setItem('pivx_default_applied', 'true');
            const everRan = account_exists || (loginPicker.accounts && loginPicker.accounts.length > 0);
            if (!everRan && localStorage.getItem('pivx_hidden') === null) {
                localStorage.setItem('pivx_hidden', 'true');
            }
        }

        if (account_exists) {
            if (enabled) {
                // Encryption enabled - show PIN or password screen for decryption
                openEncryptionFlow(true, security_type || 'pin');
            } else {
                // Encryption disabled - login directly from stored key (key never crosses IPC).
                //
                // Show the lockscreen with a neutral status title so the
                // user gets feedback during the multi-second boot —
                // particularly important on first-launch with Tor enabled,
                // where consensus fetch can take 5-15s. Without this, the
                // user sees a frozen Create/Login screen with no progress
                // indication. We hide the type-select / PIN / password
                // input UI inside `#login-encrypt` since we're not
                // soliciting anything; the title is the whole UX.
                VectorSvelte.loginScreen('encrypt');
                const typeSelect = document.getElementById('login-encrypt-type-select');
                const pinRow = document.getElementById('login-encrypt-pins');
                const passwordBox = document.getElementById('login-encrypt-password');
                if (typeSelect) typeSelect.style.display = 'none';
                if (pinRow) pinRow.style.display = 'none';
                if (passwordBox) passwordBox.style.display = 'none';
                // Set a neutral baseline title; `runWithTorBootstrapStatus`
                // overrides it with "Bootstrapping Tor… NN%" when Arti is
                // mid-consensus, and `init()` later overrides it again
                // with "Decrypting Database…" / sync progress. For bunker
                // accounts the 15s wait is dominated by the signer RPC, so
                // surface that to the user.
                domLoginEncryptTitle.textContent = window.__activeSignerType === 'bunker'
                    ? 'Connecting to Signer…'
                    : 'Connecting…';
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                // Past the point of no return — login_from_stored_key is
                // about to install this account's keys into the live
                // session. A mid-flight picker swap would race the bind.
                loginPicker.hide();

                try {
                    console.time('[Boot] login_from_stored_key');
                    const npub = await runWithTorBootstrapStatus(() =>
                        invoke("login_from_stored_key", { password: null })
                    );
                    console.timeEnd('[Boot] login_from_stored_key');
                    domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                    // domLogin (the whole lockscreen) is hidden later by
                    // login() once the chat surface is ready.

                    strPubkey = npub;
                    console.time('[Boot] login() total');
                    login(true); // skipAnimations = true
                } catch (e) {
                    console.error('Direct login failed:', e);
                    domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                    // Bunker-unreachable case: offer re-authorization
                    // instead of bouncing the user to the start screen.
                    // The account stays intact, only the pairing needs
                    // refreshing in the signer app.
                    const handled = typeof window.handleBunkerLoginError === 'function'
                        ? await window.handleBunkerLoginError(e)
                        : false;
                    if (handled) return; // reauth UI is now driving
                    // Generic failure path — the unencrypted account couldn't
                    // be loaded. Surface the error and bounce back to the
                    // Create / Login screen so they can re-import or create
                    // fresh.
                    await popupConfirm(
                        'Could not load your account',
                        String(e),
                        true
                    );
                    VectorSvelte.loginScreen('start');
                    // Re-show picker if any other accounts exist; the
                    // user can switch to a working one.
                    if (typeof loginPicker !== 'undefined'
                        && loginPicker.accounts
                        && loginPicker.accounts.length >= 2) {
                        loginPicker.show(loginPicker.activeNpub);
                    }
                }
            }
        }
    }

    // Hook up our static buttons
    domInvitesBtn.onclick = openInvites;
    domProfileBtn.onclick = () => openProfile();
    domChatlistBtn.onclick = openChatlist;
    domSettingsBtn.onclick = openSettings;
    await wireLoginUi();
    await wireChatUi();
    await initComposer();
    await wireMiniAppsUi();

    // Hook up our drag-n-drop listeners
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        await getCurrentWebview().onDragDropEvent(async (event) => {
            // Emoji pack creator takes priority over chat file send while
            // its panel is open — drops land as new pack emojis instead.
            if (typeof isEmojiPackCreatorOpen === 'function' && isEmojiPackCreatorOpen()) {
                if (event.payload.type === 'drop' && Array.isArray(event.payload.paths)) {
                    await _pcAddPaths(event.payload.paths);
                }
                return;
            }
            // Only accept File Drops if a chat is open
            if (strOpenChat) {
                if (event.payload.type === 'over') {
                    // TODO: add hover effects
                } else if (event.payload.type === 'drop') {
                    // Bring window to foreground when file is dropped
                    try {
                        await getCurrentWindow().setFocus();
                    } catch (e) {
                        console.warn('Failed to focus window:', e);
                    }
                    // Reset reply selection while passing a copy of the reference to the backend
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    // Check if dropped path is a directory or file
                    const droppedPath = event.payload.paths[0];
                    const isDir = await invoke('is_directory', { path: droppedPath });
                    if (isDir) {
                        await openFolderZipPreview(droppedPath, strOpenChat, strReplyRef);
                    } else {
                        await openFilePreview(droppedPath, strOpenChat, strReplyRef);
                    }
                } else {
                    // TODO: remove hover effects
                }
            }
        });

        // Single catch-up entry point for "the user is now actually looking":
        // window regained focus OR tab became visible. Marks the open chat as
        // read and clears its divider, but only when pinned — scrolled-up
        // users haven't seen the new messages just because they refocused.
        const onWindowResumed = () => {
            if (!strOpenChat || !chatPinnedToBottom) return;
            const currentChat = getChat(strOpenChat);
            if (!currentChat?.messages?.length) return;
            const latestNonMine = findLatestContactMessage(currentChat.messages);
            if (latestNonMine) markAsRead(currentChat, latestNonMine);
            clearUnreadDivider();
        };

        await getCurrentWindow().onFocusChanged((event) => {
            const wasActive = isWindowActive();
            windowFocused = !!event.payload;
            if (!wasActive && isWindowActive()) { onWindowResumed(); if (!fInit) consumePendingShare(); }
            syncBackendActiveChat();
        });

        document.addEventListener('visibilitychange', () => {
            const wasActive = isWindowActive();
            documentVisible = !document.hidden;
            // A share that foregrounded the app (onNewIntent stored it) may have been emitted before
            // the WebView resumed; poll for it on every resume so it isn't stranded until a later tap.
            if (!wasActive && isWindowActive()) { onWindowResumed(); if (!fInit) consumePendingShare(); }
            syncBackendActiveChat();
        });
    }


    // Initialize settings
    await initSettings();
    await wireSettingsHelp();
});

/**
 * Confirm-then-open for web links: a Discord-style speed bump showing the true
 * destination before leaving the app, since markdown link text can say
 * anything. Mobile only: desktop's safety affordance is the hover tooltip,
 * while touch has no hover to reveal a labeled link's destination.
 */
async function confirmAndOpenUrl(url) {
    let parsed = null;
    try { parsed = new URL(url); } catch (_) {}
    // Non-web schemes (mailto) keep the direct-open path.
    if (!parsed || (parsed.protocol !== 'http:' && parsed.protocol !== 'https:')) {
        return openUrl(url);
    }
    const full = parsed.toString();
    if (!platformFeatures?.is_mobile) {
        return openUrl(full);
    }
    // Tail-truncate only, so the security-relevant scheme + host stay visible.
    const shown = full.length > 220 ? `${full.slice(0, 220)}…` : full;
    const confirmed = await popupConfirm(
        'Opening Link',
        `This hyperlink redirects to:<br><code class="link-confirm-url">${escapeHtml(shown)}</code>Are you sure you want to continue?`,
        false,
        '',
        'vector_warning.svg'
    );
    if (confirmed) openUrl(full);
}

/**
 * A WYSIWYG link shows its own destination as its visible text (linkified bare
 * URLs, <url> autolinks). Those can't deceive, so neither the hover tooltip
 * nor the open-confirm adds anything. Compared via the href ATTRIBUTE: the
 * .href property normalizes (adds trailing slashes) and would mismatch the
 * verbatim text.
 */
function anchorShowsItsDestination(anchor) {
    const label = (anchor.textContent || '').trim().replace(/\/$/, '');
    const rawHref = (anchor.getAttribute('href') || '').trim().replace(/\/$/, '');
    return !!label && label === rawHref;
}

/**
 * App-chrome anchors (e.g. the Tor attribution logo) use a placeholder `#` or a
 * relative href that resolves to Vector's own origin and open via their own
 * click handler. The phishing tooltip + open-confirm are for EXTERNAL links in
 * untrusted message content only — markdown strips raw <a>, so genuine message
 * links are always cross-origin. Same-origin therefore means "our own UI".
 */
function isAppChromeAnchor(anchor) {
    try { return new URL(anchor.href).origin === location.origin; }
    catch { return true; }
}

// Hover tooltip: the honest counterpart to the open-confirm. Surfaces a
// labeled web link's true destination centered above it, since the visible
// text may say anything. Desktop only: touch has no hover, and the synthetic
// mouseover a tap fires would flash the tooltip beneath the confirm popup.
document.addEventListener('mouseover', (e) => {
    if (platformFeatures?.is_mobile) return;
    const anchor = e.target.closest?.('a[href]');
    if (!anchor || !/^https?:/i.test(anchor.href)) return;
    if (isAppChromeAnchor(anchor)) return;
    if (anchorShowsItsDestination(anchor)) return;
    // Tail-truncate only, so the security-relevant scheme + host stay visible;
    // the tooltip wraps up to a few lines.
    const url = anchor.href;
    showGlobalTooltip(url.length > 140 ? `${url.slice(0, 140)}…` : url, anchor);
});
document.addEventListener('mouseout', (e) => {
    if (e.target.closest?.('a[href]')) hideGlobalTooltip();
});

/**
 * Open a downloaded file on Android.
 *
 * An `.apk` goes to the system package installer, but only once the user has
 * trusted Vector as an install source. That is a "special app access", not a
 * runtime permission: nothing can prompt for it, so the backend routes an
 * untrusted tap to the settings screen that grants it. Say so first, otherwise
 * asking to open a file appears to fling you into Settings for no reason.
 */
async function openAndroidAttachment(filepath) {
    if (/\.apk$/i.test(filepath || '')) {
        const allowed = await invoke('can_install_apks').catch(() => true);
        if (!allowed) {
            showToast('Android needs your permission first — switch on "Allow from this source", then tap the file again');
        }
    }
    return invoke('open_attachment', { path: filepath });
}

// Listen for app-wide click interations
document.addEventListener('click', (e) => {
    // If we're clicking the emoji search, don't close it!
    if (e.target === emojiSearch) return;

    // Any <a> click (including on styled children inside one) routes through
    // the confirm-then-open speed bump — except WYSIWYG links, whose visible
    // text already IS the destination. UI anchors with their own handlers
    // stopPropagation before reaching here.
    const anchor = e.target.closest?.('a');
    if (anchor && anchor.href && !isAppChromeAnchor(anchor)) {
        e.preventDefault();
        if (anchorShowsItsDestination(anchor)) return openUrl(anchor.href);
        return confirmAndOpenUrl(anchor.href);
    }

    // If we're clicking a <summary> to toggle <details>, handle scroll adjustment
    if (e.target.tagName === 'SUMMARY') {
        const details = e.target.parentElement;
        if (details && details.tagName === 'DETAILS') {
            // Add button class if not already present
            if (!e.target.classList.contains('btn')) {
                e.target.classList.add('btn');
            }
            
            const chatMessages = document.getElementById('chat-messages');
            if (chatMessages) {
                // Check scroll position BEFORE toggle
                const wasNearBottom = chatMessages.scrollHeight - chatMessages.scrollTop - chatMessages.clientHeight < 150;
                
                // Wait for the DOM to update after toggle
                requestAnimationFrame(() => {
                    requestAnimationFrame(() => {
                        if (wasNearBottom && details.open) {
                            // Scroll to bottom to reveal expanded content
                            scrollToBottom(chatMessages, true);
                        }
                    });
                });
            }
        }
    }

    // Run the emoji panel open/close logic
    openEmojiPanel(e);

    // Close attachment panel when clicking outside of it
    if (domAttachmentPanel.classList.contains('visible')) {
        const clickedInsidePanel = domAttachmentPanel.contains(e.target);
        const clickedFileButton = domChatMessageInputFile.contains(e.target);
        // Don't close if clicking inside PIVX dialogs, popup prompts, or Mini App launch dialog
        const clickedInsidePivxDialog = e.target.closest('.pivx-dialog-overlay');
        const clickedInsidePopup = e.target.closest('#popup-container');
        const clickedInsideLaunchDialog = e.target.closest('#miniapp-launch-overlay');
        if (!clickedInsidePanel && !clickedFileButton && !clickedInsidePivxDialog && !clickedInsidePopup && !clickedInsideLaunchDialog) {
            closeAttachmentPanel();
        }
    }

    // Close edit history popup when clicking outside of it
    const editHistoryPopup = document.getElementById('edit-history-popup');
    if (editHistoryPopup && editHistoryPopup.style.display !== 'none') {
        if (!editHistoryPopup.contains(e.target) && !e.target.classList.contains('dmsg-edited')) {
            hideEditHistory();
        }
    }
});

// Close edit history popup on Escape key
document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
        const editHistoryPopup = document.getElementById('edit-history-popup');
        if (editHistoryPopup && editHistoryPopup.style.display !== 'none') {
            hideEditHistory();
        }
    }
});
