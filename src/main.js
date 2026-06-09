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
};

/** Resolve a system-event actor's display name from cached profiles; npub-prefix fallback. */
function systemEventName(npub) {
    const p = arrProfiles.find(x => x.id === npub);
    return p?.nickname || p?.name || (npub ? npub.substring(0, 12) + '…' : 'Someone');
}

/** Build a system-event line, resolving the actor's CURRENT cached name (the stored content
 * was baked with the raw npub at receive time, before the profile was known). */
function systemEventContent(eventType, npub) {
    const name = systemEventName(npub);
    switch (eventType) {
        case SystemEventType.MemberLeft: return `${name} has left`;
        case SystemEventType.MemberJoined: return `${name} has joined`;
        case SystemEventType.MemberRemoved: return `${name} was removed`;
        case SystemEventType.WallpaperChanged: return `${name} changed the wallpaper`;
        case SystemEventType.WallpaperRemoved: return `${name} removed the wallpaper`;
        default: return name;
    }
}

/**
 * Multi-account API surface. Wraps the Tauri commands that the in-app My
 * Profile dropdown and the pre-login picker both consume. Keeping it in one
 * place so the two callers can't drift on validation or error handling.
 */
const MAX_ACCOUNTS = 5;

const multiAccount = {
    /**
     * List every locally-known account with display metadata (name, avatar,
     * has_encryption flag, last_active timestamp). Sorted by last_active desc.
     */
    list() {
        return invoke('list_accounts_with_metadata');
    },

    /**
     * Switch to a different account. Writes the active-account marker file
     * and triggers a full session reset; the backend emits `session_reload`
     * which the listener at top of setupRustListeners catches and reloads.
     */
    async setActiveAndSwap(npub) {
        // Capture the previous marker so we can roll back if swap_session
        // rejects (e.g. mid-encryption-migration). Otherwise the marker
        // would point at the new account while the in-memory session
        // stayed on the previous one — the next manual launch would boot
        // into the wrong account silently.
        let prev = null;
        try { prev = await invoke('get_current_account'); } catch (_) {}
        await invoke('set_active_account', { npub });
        try {
            await invoke('swap_session');
        } catch (e) {
            if (prev) {
                try { await invoke('set_active_account', { npub: prev }); } catch (_) {}
            } else {
                try { await invoke('clear_active_account'); } catch (_) {}
            }
            throw e;
        }
    },

    /**
     * Permanently delete an account. Returns whether the deleted account was
     * the active one (in which case the backend already ran reset_session and
     * the caller should issue swap_session to fire the reload).
     */
    delete(npub) {
        return invoke('delete_account', { npub });
    },

    /**
     * Reset + reload the session without changing the active account marker.
     * Used after account deletion to surface the picker / fresh boot state.
     */
    swap() {
        return invoke('swap_session');
    },
};

/**
 * Build one row of the My Profile dropdown / pre-login picker.
 * `meta` is an AccountMetadata record from the backend.
 * `isActive` adds the green dot + accent ring to the active row.
 */
function buildAccountRow(meta, { isActive, onClick, onDelete }) {
    const row = document.createElement('div');
    row.className = 'profile-switcher-row' + (isActive ? ' active' : '');
    row.dataset.npub = meta.npub;

    const dot = document.createElement('span');
    dot.className = 'profile-switcher-active-dot';
    row.appendChild(dot);

    // Reuse the shared avatar helper so accounts without a profile-set
    // avatar render the same Nostr placeholder used by chat rows / contact
    // headers, and a failed image load falls back to the placeholder
    // automatically.
    const avatarSrc = meta.avatar_cached
        ? convertFileSrc(meta.avatar_cached)
        : (meta.avatar_url || null);
    const avatar = createAvatarImg(avatarSrc, 28, false);
    avatar.classList.add('profile-switcher-avatar');
    row.appendChild(avatar);

    const meta_el = document.createElement('div');
    meta_el.className = 'profile-switcher-meta';
    const name = document.createElement('span');
    name.className = 'profile-switcher-name';
    name.textContent = meta.display_name || 'Unnamed';
    const npub = document.createElement('span');
    npub.className = 'profile-switcher-npub';
    // Full npub — CSS handles overflow with `text-overflow: ellipsis`,
    // so the visible cut adapts to the row width on any screen size
    // instead of being hard-coded to a slice length.
    npub.textContent = meta.npub;
    meta_el.appendChild(name);
    meta_el.appendChild(npub);
    row.appendChild(meta_el);

    if (onDelete) {
        const trash = document.createElement('button');
        trash.className = 'profile-switcher-row-trash btn';
        trash.setAttribute('aria-label', 'Delete account');
        // Inline SVG — Vector's `.icon` class is position:absolute inside
        // sized parents and would render at 0×0 here. Using SVG keeps the
        // trash icon flowing inline with the row.
        trash.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none"><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m3 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6h14ZM10 11v6M14 11v6" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';
        trash.addEventListener('click', (ev) => {
            ev.stopPropagation();
            onDelete(meta);
        });
        row.appendChild(trash);
    }

    if (onClick && !isActive) {
        row.addEventListener('click', (ev) => {
            onClick(meta);
        });
    }

    return row;
}

/**
 * In-app My Profile dropdown — full-feature: switch / add / delete.
 * Opened by clicking #my-profile-switcher. Renders accounts via multiAccount.list().
 */
const profileSwitcher = {
    isOpen: false,
    isEditing: false,
    isOpening: false,

    init() {
        const trigger = document.getElementById('my-profile-switcher');
        const backdrop = document.getElementById('profile-switcher-backdrop');
        const panel = document.getElementById('profile-switcher-panel');
        const trashToggle = document.getElementById('profile-switcher-trash-toggle');
        const addBtn = document.getElementById('profile-switcher-add');
        if (!trigger || !panel) return;

        trigger.addEventListener('click', () => this.toggle());
        backdrop.addEventListener('click', () => this.close());
        trashToggle.addEventListener('click', (ev) => {
            ev.stopPropagation();
            this.toggleEditMode();
        });
        addBtn.addEventListener('click', () => this.onAddProfile());

        // Close on Escape
        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape' && this.isOpen) this.close();
        });
    },

    async toggle() {
        if (this.isOpen) {
            this.close();
        } else {
            await this.open();
        }
    },

    async open() {
        if (this.isOpening) return;
        this.isOpening = true;
        try {
            const accounts = await multiAccount.list();
            this.render(accounts);
            document.getElementById('profile-switcher-backdrop').classList.add('visible');
            document.getElementById('profile-switcher-panel').classList.add('open');
            document.getElementById('my-profile-switcher').classList.add('open');
            this.isOpen = true;
            const trashToggle = document.getElementById('profile-switcher-trash-toggle');
            if (trashToggle) trashToggle.style.display = '';
        } catch (e) {
            console.error('[profile-switcher] open failed:', e);
        } finally {
            this.isOpening = false;
        }
    },

    close() {
        document.getElementById('profile-switcher-backdrop').classList.remove('visible');
        document.getElementById('profile-switcher-panel').classList.remove('open');
        document.getElementById('my-profile-switcher').classList.remove('open');
        this.isOpen = false;
        const trashToggle = document.getElementById('profile-switcher-trash-toggle');
        if (trashToggle) trashToggle.style.display = 'none';
        // Always reset edit mode on close so the next open starts neutral.
        this.exitEditMode();
    },

    render(accounts) {
        const list = document.getElementById('profile-switcher-list');
        list.innerHTML = '';
        const myProfile = arrProfiles.find(p => p.mine);
        const activeNpub = myProfile?.id || '';
        for (const meta of accounts) {
            const row = buildAccountRow(meta, {
                isActive: meta.npub === activeNpub,
                onClick: (m) => this.onSwitchTo(m),
                onDelete: (m) => this.onDeleteRow(m),
            });
            list.appendChild(row);
        }
        // Cap at MAX_ACCOUNTS — replace the Add button label and disable
        // the click affordance once the user has hit the ceiling.
        const addBtn = document.getElementById('profile-switcher-add');
        if (addBtn) {
            const atCap = accounts.length >= MAX_ACCOUNTS;
            addBtn.classList.toggle('disabled', atCap);
            const label = addBtn.querySelector('.profile-switcher-add-label');
            if (label) label.textContent = atCap ? 'Maximum Accounts' : 'Add Profile';
        }
    },

    toggleEditMode() {
        if (this.isEditing) {
            this.exitEditMode();
        } else {
            document.body.classList.add('profile-switcher-editing');
            this.isEditing = true;
        }
    },
    exitEditMode() {
        document.body.classList.remove('profile-switcher-editing');
        this.isEditing = false;
    },

    async onSwitchTo(meta) {
        // Active row click is no-op (the click handler is gated above).
        try {
            this.close();
            await multiAccount.setActiveAndSwap(meta.npub);
            // Backend emits session_reload; the listener calls window.location.reload().
        } catch (e) {
            console.error('[profile-switcher] switch failed:', e);
            popupConfirm('Switch failed', String(e), true);
        }
    },

    async onDeleteRow(meta) {
        // Pre-flight: if this is the LAST account on the device, the
        // backend cascade will ALSO wipe the shared downloads dir
        // (`~/Downloads/vector` or platform-equivalent) and the legacy
        // MLS folder. Warn the user up-front so they can copy attachments
        // out first if they want to keep them.
        let isLastAccount = false;
        try {
            const all = await multiAccount.list();
            isLastAccount = all.length === 1 && all[0].npub === meta.npub;
        } catch (_) { /* err side: don't block the popup */ }

        const baseMsg = `<span style="color: var(--primary-color);">${meta.display_name || 'This account'}</span> will be permanently removed from this device. Make sure you have the seed phrase or nsec backed up if you want to recover it later.`;
        const lastAccountWarning = `\n\n<b>This is your only Vector account on this device.</b> All downloaded attachments will also be removed. Copy any files you want to keep before continuing.`;
        const message = isLastAccount ? baseMsg + lastAccountWarning : baseMsg;

        const ok = await popupConfirm(
            'Remove Profile?',
            message,
            false,
            '',
            'vector_warning.svg',
        );
        if (!ok) return;
        try {
            const wasActive = await multiAccount.delete(meta.npub);
            if (wasActive) {
                // Backend ran `reset_session` and cleared the marker. If there
                // are other accounts on disk, point the marker at one of them
                // so the post-reload boot lands directly on it. Walk the list
                // in last-active order; if the first candidate's marker write
                // fails for any reason (rare — e.g. a concurrent disk hiccup),
                // try the next so we don't dump the user onto the bare
                // Create / Login screen when other accounts still exist.
                const remaining = await multiAccount.list();
                let restored = false;
                for (const candidate of remaining) {
                    try {
                        await invoke('set_active_account', { npub: candidate.npub });
                        restored = true;
                        break;
                    } catch (e) {
                        console.error('[profile-switcher] failed to point marker at', candidate.npub, e);
                    }
                }
                if (!restored && remaining.length > 0) {
                    console.warn('[profile-switcher] all remaining accounts rejected; landing on Create / Login');
                }
                await multiAccount.swap();
            } else {
                // Refresh dropdown in place.
                const accounts = await multiAccount.list();
                this.render(accounts);
                if (accounts.length === 0) this.close();
            }
        } catch (e) {
            console.error('[profile-switcher] delete failed:', e);
            popupConfirm('Delete failed', String(e), true);
        }
    },

    onAddProfile() {
        const addBtn = document.getElementById('profile-switcher-add');
        if (addBtn?.classList.contains('disabled')) return;
        this.close();
        addAccountFlow.start();
    },
};

/**
 * Pre-login account picker — read-only. Visible only when N>=2 accounts
 * exist locally; lets the user choose which one's PIN/password to enter.
 * Single-account boot stays unchanged (picker is hidden).
 */
const loginPicker = {
    isOpen: false,
    accounts: [],
    activeNpub: null,

    init() {
        const trigger = document.getElementById('login-account-picker');
        if (!trigger) return;
        trigger.addEventListener('click', () => {
            // Single-account form has the .single class and is non-interactive.
            if (trigger.classList.contains('single')) return;
            this.toggle();
        });
        // Close when clicking the backdrop (outside the list itself).
        const backdrop = document.getElementById('login-account-list-backdrop');
        if (backdrop) backdrop.addEventListener('click', () => this.close());
        // Escape closes too.
        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape' && this.isOpen) this.close();
        });
    },

    /**
     * Render and reveal the picker. Caller passes the marker-derived
     * "active" npub so the corresponding row is rendered with the dot/ring.
     *
     * Single-account boots stay completely unchanged — no picker, no name
     * pill — so the unlock screen looks identical to pre-multi-account UX.
     * The picker only appears when there's an actual choice to make.
     */
    async show(activeNpub) {
        const trigger = document.getElementById('login-account-picker');
        try {
            this.accounts = await multiAccount.list();
        } catch (e) {
            console.error('[login-picker] list failed:', e);
            if (trigger) trigger.style.display = 'none';
            return;
        }
        this.activeNpub = activeNpub;
        if (this.accounts.length < 2) {
            if (trigger) trigger.style.display = 'none';
            return;
        }
        // When `activeNpub` is null (marker-missing recovery branch), the
        // pill has no real "active" identity to display. Render a neutral
        // "Select profile" affordance instead of `accounts[0]`'s avatar +
        // name, which read like "you are signed in as accounts[0]" when
        // the user actually has no active session. The list itself
        // correctly shows every row as equally selectable (open() does
        // `isActive: meta.npub === this.activeNpub` and null can't match
        // any real npub).
        const hasActive = !!activeNpub && this.accounts.some(a => a.npub === activeNpub);
        const meta = hasActive
            ? this.accounts.find(a => a.npub === activeNpub)
            : null;
        const avatarSrc = meta
            ? (meta.avatar_cached ? convertFileSrc(meta.avatar_cached) : (meta.avatar_url || null))
            : null;
        const oldImg = document.getElementById('login-account-picker-avatar');
        if (oldImg && oldImg.parentNode) {
            const replacement = createAvatarImg(avatarSrc, 36, false);
            replacement.id = 'login-account-picker-avatar';
            oldImg.parentNode.replaceChild(replacement, oldImg);
        }
        const label = meta ? (meta.display_name || meta.npub) : 'Select Profile';
        document.getElementById('login-account-picker-name').textContent = label;
        trigger.classList.remove('single');
        trigger.style.display = '';
    },

    hide() {
        const trigger = document.getElementById('login-account-picker');
        if (trigger) trigger.style.display = 'none';
        this.close();
    },

    toggle() {
        if (this.isOpen) this.close(); else this.open();
    },

    open() {
        const list = document.getElementById('login-account-list');
        const backdrop = document.getElementById('login-account-list-backdrop');
        const trigger = document.getElementById('login-account-picker');
        list.innerHTML = '';
        // Active account stays anchored in the pill at the top — only
        // alternates appear as switchable rows below it.
        for (const meta of this.accounts) {
            if (meta.npub === this.activeNpub) continue;
            const row = buildAccountRow(meta, {
                isActive: false,
                onClick: (m) => this.onPick(m),
                // No delete in pre-login picker (per design).
            });
            list.appendChild(row);
        }
        // Anchor the list directly below the pill — measure the pill's
        // current bottom edge so the list always sits flush against it,
        // regardless of how #login-form lays out at this viewport size.
        if (trigger) {
            const rect = trigger.getBoundingClientRect();
            list.style.top = `${Math.round(rect.bottom + 8)}px`;
        }
        list.classList.add('open');
        if (backdrop) backdrop.classList.add('visible');
        if (trigger) trigger.classList.add('open');
        this.isOpen = true;
    },

    close() {
        const list = document.getElementById('login-account-list');
        const backdrop = document.getElementById('login-account-list-backdrop');
        const trigger = document.getElementById('login-account-picker');
        if (list) list.classList.remove('open');
        if (backdrop) backdrop.classList.remove('visible');
        if (trigger) trigger.classList.remove('open');
        this.isOpen = false;
    },

    async onPick(meta) {
        this.close();
        if (meta.npub === this.activeNpub) return;
        try {
            await multiAccount.setActiveAndSwap(meta.npub);
        } catch (e) {
            console.error('[login-picker] switch failed:', e);
            popupConfirm('Switch failed', String(e), true);
        }
    },
};

/**
 * Add Profile flow.
 *
 * Two phases:
 *
 *   - **Browsing** (`active && !committed`): the user clicked Add Profile
 *     and is sitting on the login-start screen but hasn't committed to
 *     creating/importing yet. The current account stays fully alive in
 *     memory — DM listeners keep firing, decrypted keys stay in the vault,
 *     STATE keeps its profiles+chats. Back is an instant, free UI restore.
 *
 *   - **Committed** (`active && committed`): set when the user actually
 *     clicks Create Account or Login. We invoke `enter_add_account_mode`
 *     which calls `reset_session` + clears the marker — required because
 *     `login`/`create_account` are guarded by lock-and-check and would
 *     otherwise silently no-op against the still-active session. From
 *     this point Back can no longer be free; if the user backs all the
 *     way out we restore the previous account's marker and reload.
 *
 * Existing accounts on disk are NEVER touched by this flow — switching
 * back via My Profile is always available once the new account is set up.
 */
const ADD_PROFILE_BACK_TARGET = 'vector:add_profile_back_target';

const addAccountFlow = {
    /** Browsing phase active (login overlay shown over current session). */
    active: false,
    /** Committed (`enter_add_account_mode` invoked, original session torn down). */
    committed: false,
    /** Snapshot of which UI panel was visible before Add Profile took over. */
    _restoreFn: null,

    async start() {
        // Set the active flag SYNCHRONOUSLY before any await. If the user
        // rapid-clicks Add Profile → Back, the Back handler must see
        // `active: true` even if our IPC roundtrip is still in flight.
        if (this.active) return;
        this.active = true;
        this.committed = false;

        // Cache who we'll need to restore to if the user backs out AFTER
        // committing. We grab it now while CURRENT_ACCOUNT is still set
        // because by the time we'd need it (post-reset_session), it's gone.
        try {
            const prev = await invoke('get_current_account');
            if (prev) sessionStorage.setItem(ADD_PROFILE_BACK_TARGET, prev);
        } catch (_) {
            // No active account; nothing to restore on back.
        }

        // Snapshot the current UI so back can put it back.
        this._restoreFn = captureMainUiSnapshot();

        // Pure UI swap — no backend touch. Hide every main-app panel and
        // surface the login form with the start screen + back-bar visible.
        domNavbar.style.display = 'none';
        domChats.style.display = 'none';
        domChat.style.display = 'none';
        domProfile.style.display = 'none';
        domSettings.style.display = 'none';
        domInvites.style.display = 'none';
        domGroupOverview.style.display = 'none';

        domLoginImport.style.display = 'none';
        domLoginInvite.style.display = 'none';
        domLoginEncrypt.style.display = 'none';
        domLoginWelcome.style.display = 'none';
        domLoginStart.style.display = '';
        domLoginBackBar.style.display = '';
        document.getElementById('login-form').classList.add('has-back-bar');
        domLogin.style.display = '';

        // Hide the pre-login picker pill during Add Profile — the user is
        // creating a new account, not picking an existing one. Without
        // this, the picker pill renders above the start screen and lets
        // the user switch to another existing account mid-import.
        if (typeof loginPicker !== 'undefined') loginPicker.hide();
    },

    /**
     * The user clicked Create Account or Login from inside the Add Profile
     * overlay. Tear down the current session so the new account's keys can
     * be installed without colliding with the lock-and-check guards.
     */
    async commit() {
        if (this.committed) return;
        await invoke('enter_add_account_mode');
        this.committed = true;
    },

    /** Soft restore — only valid before commit. */
    restore() {
        domLoginImport.style.display = 'none';
        domLoginInvite.style.display = 'none';
        domLoginEncrypt.style.display = 'none';
        domLoginWelcome.style.display = 'none';
        domLoginStart.style.display = '';
        domLoginBackBar.style.display = 'none';
        document.getElementById('login-form').classList.remove('has-back-bar');
        domLogin.style.display = 'none';
        if (this._restoreFn) {
            this._restoreFn();
            this._restoreFn = null;
        }
        this.active = false;
        this.committed = false;
        sessionStorage.removeItem(ADD_PROFILE_BACK_TARGET);
    },

    finish() {
        this.active = false;
        this.committed = false;
        this._restoreFn = null;
        sessionStorage.removeItem(ADD_PROFILE_BACK_TARGET);
    },

    backTarget() {
        return sessionStorage.getItem(ADD_PROFILE_BACK_TARGET);
    },
};

/**
 * Capture which main-app panel is currently visible so the Add Profile
 * back button can put it back. Called before the overlay takes over the
 * viewport. Returns a closure that re-applies the snapshot.
 */
function captureMainUiSnapshot() {
    const visible = {
        navbar: domNavbar.style.display,
        chats: domChats.style.display,
        chat: domChat.style.display,
        profile: domProfile.style.display,
        settings: domSettings.style.display,
        invites: domInvites.style.display,
        groupOverview: domGroupOverview.style.display,
    };
    return () => {
        domNavbar.style.display = visible.navbar;
        domChats.style.display = visible.chats;
        domChat.style.display = visible.chat;
        domProfile.style.display = visible.profile;
        domSettings.style.display = visible.settings;
        domInvites.style.display = visible.invites;
        domGroupOverview.style.display = visible.groupOverview;
    };
}

const domTheme = document.getElementById('theme');

const domLoginStart = document.getElementById('login-start');
const domLoginAccountCreationBtn = document.getElementById('start-account-creation-btn');
const domLoginAccountBtn = document.getElementById('start-login-btn');
const domLoginBunkerStartBtn = document.getElementById('start-bunker-btn');
const domLogin = document.getElementById('login-form');
const domLoginImport = document.getElementById('login-import');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');
const domLoginBunker = document.getElementById('login-bunker');
const domLoginBunkerUrlInput = document.getElementById('bunker-url-input');
const domLoginBunkerConnectBtn = document.getElementById('bunker-connect-btn');
const domLoginBunkerStatus = document.getElementById('bunker-status-text');
const domLoginBunkerQrWrap = document.querySelector('.login-bunker-qr-wrap');
const domLoginBunkerQr = document.getElementById('bunker-qr');
const domLoginBunkerCopyBtn = document.getElementById('bunker-copy-url-btn');

// Active nostrconnect:// URL — captured when start_nostrconnect_session
// returns so the Copy button can place it on the clipboard.
let strBunkerNostrConnectUrl = '';

// Bunker form mode — 'new' for fresh logins / Add Profile, 'reauth' for
// re-pairing an already-committed account whose signer wiped its
// permissions. Module scope so the boot-time login catch can route into
// reauth mode before DOMContentLoaded finishes wiring click handlers.
let bunkerFormMode = 'new';

// Bunker connection URL is single-use and the backend's NostrConnect uses
// a 120s timeout — pair this client-side so the user sees a live countdown
// and we auto-reroll a fresh QR + URL when it expires.
const BUNKER_SESSION_TIMEOUT_MS = 120 * 1000;
let bunkerSessionDeadline = 0;
let bunkerSessionTimerHandle = null;

function stopBunkerSessionTimer() {
    if (bunkerSessionTimerHandle) {
        clearInterval(bunkerSessionTimerHandle);
        bunkerSessionTimerHandle = null;
    }
    bunkerSessionDeadline = 0;
}

function renderBunkerCountdown() {
    const status = document.getElementById('bunker-status-text');
    if (!status) return;
    const remaining = Math.max(0, bunkerSessionDeadline - Date.now());
    if (remaining === 0) {
        stopBunkerSessionTimer();
        status.textContent = 'Refreshing connection link…';
        status.className = 'login-bunker-status connecting';
        startBunkerSession();
        return;
    }
    const secs = Math.ceil(remaining / 1000);
    const m = Math.floor(secs / 60);
    const s = secs % 60;
    status.textContent = `Waiting for signer… (${m}:${s.toString().padStart(2, '0')})`;
    status.className = 'login-bunker-status connecting';
}

function armBunkerSessionTimer() {
    stopBunkerSessionTimer();
    bunkerSessionDeadline = Date.now() + BUNKER_SESSION_TIMEOUT_MS;
    bunkerSessionTimerHandle = setInterval(renderBunkerCountdown, 1000);
    renderBunkerCountdown();
}

/**
 * Render a QR code (SVG) into a container element using the vendored
 * qrcode-generator library. Reusable for future Profile QR / contact-share
 * / Lightning URI flows.
 */
function renderQrInto(containerEl, text, opts = {}) {
    if (!containerEl || !window.qrcode) return false;
    const ecc = opts.ecc || 'M';
    const qr = window.qrcode(0, ecc);
    qr.addData(text);
    qr.make();
    containerEl.innerHTML = qr.createSvgTag({ cellSize: 4, margin: 0, scalable: true });
    return true;
}

/**
 * Kick off a NIP-46 client-initiated session — either fresh
 * (`start_nostrconnect_session`) or re-pairing an existing committed account
 * (`reauthorize_bunker`). Backend returns a `nostrconnect://` URL that we
 * render as a QR + Copy button.
 */
async function startBunkerSession() {
    strBunkerNostrConnectUrl = '';
    if (domLoginBunkerQrWrap) domLoginBunkerQrWrap.classList.remove('ready');
    if (domLoginBunkerCopyBtn) {
        domLoginBunkerCopyBtn.disabled = true;
        domLoginBunkerCopyBtn.classList.remove('copied');
        domLoginBunkerCopyBtn.textContent = 'Copy connection link';
    }
    if (domLoginBunkerStatus) {
        domLoginBunkerStatus.textContent = 'Waiting for signer…';
        domLoginBunkerStatus.className = 'login-bunker-status connecting';
    }
    try {
        // Reauth re-uses the existing client keypair from MY_SECRET_KEY and
        // is for already-committed accounts — no Add Profile commit step.
        const cmd = bunkerFormMode === 'reauth' ? 'reauthorize_bunker' : 'start_nostrconnect_session';
        if (bunkerFormMode !== 'reauth' && typeof addAccountFlow !== 'undefined' && addAccountFlow.active) {
            await addAccountFlow.commit();
        }
        // Recover from a missed `bunker_reauthorize_succeeded` — if the
        // frontend reloaded between the event firing and the listener
        // registering, the backend stashes the npub in a one-shot slot we
        // can poll here. (No-op when nothing was stashed.)
        if (bunkerFormMode === 'reauth') {
            try {
                const recoveredNpub = await invoke('get_pending_reauth_result');
                if (recoveredNpub) {
                    // The new pairing was already installed by the bg task;
                    // we just need to put the UI back where the user came
                    // from. Mirror the success-listener restore logic.
                    strPubkey = recoveredNpub;
                    stopBunkerSessionTimer();
                    const origin = bunkerReauthOrigin;
                    hideBunkerForm();
                    if (origin) {
                        if (domLoginBackBar) domLoginBackBar.style.display = 'none';
                        const lf = document.getElementById('login-form');
                        if (lf) lf.classList.remove('has-back-bar', 'bunker-active');
                        if (domLogin) domLogin.style.display = 'none';
                        bunkerReauthOrigin = null;
                        if (origin === 'settings' && typeof openSettings === 'function') {
                            openSettings();
                        } else if (typeof closeChat === 'function') {
                            closeChat();
                        }
                    } else {
                        // No origin = came from the boot-time popup; full boot.
                        invoke('connect').catch(() => {});
                        login(true);
                    }
                    return;
                }
            } catch (_) { /* missing-command-fail-open */ }
        }
        const url = await invoke(cmd);
        strBunkerNostrConnectUrl = url;
        const rendered = renderQrInto(domLoginBunkerQr, url, { ecc: 'M' });
        if (rendered && domLoginBunkerQrWrap) {
            domLoginBunkerQrWrap.classList.add('ready');
        }
        if (domLoginBunkerCopyBtn) domLoginBunkerCopyBtn.disabled = false;
        // Start the live countdown — auto-rerolls a fresh QR when the
        // 120s backend timeout expires so the user isn't stranded.
        armBunkerSessionTimer();
    } catch (e) {
        stopBunkerSessionTimer();
        if (domLoginBunkerStatus) {
            domLoginBunkerStatus.textContent = String(e);
            domLoginBunkerStatus.className = 'login-bunker-status error';
        }
    }
}

/** Tracks which main-app panel was visible when the bunker form took over
 *  (reauth from Settings, etc.) so the back button can restore it. Null
 *  when entering from the login screen. */
let bunkerReauthOrigin = null;

/**
 * Show the bunker form (QR + Copy + paste fallback). `mode` is 'new' for
 * regular login / Add Profile entry, 'reauth' for the recovery flow when a
 * signer has wiped its permissions.
 */
function showBunkerForm(mode = 'new') {
    bunkerFormMode = mode;
    // Reauth enters from inside the main app, where Settings (or another
    // panel) is rendered behind #login-form and shows through. Snapshot the
    // visible panel and hide every major view so the bunker form gets the
    // full viewport with no see-through layout.
    if (mode === 'reauth') {
        const settingsVisible = typeof domSettings !== 'undefined' && domSettings
            && domSettings.style.display !== 'none';
        bunkerReauthOrigin = settingsVisible ? 'settings' : 'chats';
        if (typeof domNavbar !== 'undefined' && domNavbar) domNavbar.style.display = 'none';
        if (typeof domSettings !== 'undefined' && domSettings) domSettings.style.display = 'none';
        if (typeof domChats !== 'undefined' && domChats) domChats.style.display = 'none';
        if (typeof domProfile !== 'undefined' && domProfile) domProfile.style.display = 'none';
        if (typeof domInvites !== 'undefined' && domInvites) domInvites.style.display = 'none';
        if (typeof domGroupOverview !== 'undefined' && domGroupOverview) domGroupOverview.style.display = 'none';
    } else {
        bunkerReauthOrigin = null;
    }
    if (domLoginImport) domLoginImport.style.display = 'none';
    if (domLoginStart) domLoginStart.style.display = 'none';
    if (domLoginInvite) domLoginInvite.style.display = 'none';
    if (typeof domLoginEncrypt !== 'undefined' && domLoginEncrypt) domLoginEncrypt.style.display = 'none';
    // Also show the parent login form + back-bar in case we're entering
    // from the main app (reauth path can fire from anywhere).
    const loginForm = document.getElementById('login-form');
    if (loginForm) {
        loginForm.classList.add('bunker-active');
        loginForm.classList.add('has-back-bar');
    }
    if (typeof domLogin !== 'undefined' && domLogin) domLogin.style.display = '';
    if (typeof domLoginBackBar !== 'undefined' && domLoginBackBar) domLoginBackBar.style.display = '';
    domLoginBunker.classList.remove('is-hidden');
    domLoginBunker.style.display = '';
    if (domLoginBunkerStatus) {
        domLoginBunkerStatus.textContent = '';
        domLoginBunkerStatus.className = 'login-bunker-status';
    }
    // Fresh URL per open — single-use, can't be cached.
    startBunkerSession();
}
window.showBunkerForm = showBunkerForm;

/** Hide the bunker form and clear its in-memory state. */
function hideBunkerForm() {
    stopBunkerSessionTimer();
    domLoginBunker.classList.add('is-hidden');
    domLoginBunker.style.display = 'none';
    const loginForm = document.getElementById('login-form');
    if (loginForm) loginForm.classList.remove('bunker-active');
    if (domLoginBunkerUrlInput) domLoginBunkerUrlInput.value = '';
    strBunkerNostrConnectUrl = '';
    if (domLoginBunkerQr) domLoginBunkerQr.innerHTML = '';
    if (domLoginBunkerQrWrap) domLoginBunkerQrWrap.classList.remove('ready');
}
window.hideBunkerForm = hideBunkerForm;

const domLoginImportError = document.getElementById('login-import-error');

const domLoginBackBar = document.getElementById('login-back-bar');
const domLoginBackBtn = document.getElementById('login-back-btn');

const domLoginInvite = document.getElementById('login-invite');
const domInviteInput = document.getElementById('invite-input');
const domInviteBtn = document.getElementById('invite-btn');

const domLoginWelcome = document.getElementById('login-welcome');

const domLoginEncrypt = document.getElementById('login-encrypt');
const domLoginEncryptTitle = document.getElementById('login-encrypt-title');
const domLoginEncryptPinRow = document.getElementById('login-encrypt-pins');
const domLoginEncryptPassword = document.getElementById('login-encrypt-password');
const domLoginPasswordInput = document.getElementById('login-password-input');
const domLoginEncryptTypeSelect = document.getElementById('login-encrypt-type-select');

const domProfile = document.getElementById('profile');
const domProfileBackBtn = document.getElementById('profile-back-btn');
const domProfileHeaderAvatarContainer = document.getElementById('profile-header-avatar-container');
const domProfileName = document.getElementById('profile-name');
const domProfileStatus = document.getElementById('profile-status');
// Note: these are 'let' due to needing to use `.replaceWith` when hot-swapping profile elements
let fProfileEditMode = false;
let objProfileEditSnapshot = {};
let strPendingProfileAvatarPath = null;
let strPendingProfileBannerPath = null;
const domProfileEditBtn = document.getElementById('profile-edit-btn');
const domProfileEditBar = document.getElementById('profile-edit-bar');
const domProfileEditCancelBtn = document.getElementById('profile-edit-cancel-btn');
const domProfileEditSaveBtn = document.getElementById('profile-edit-save-btn');
let domProfileBanner = document.getElementById('profile-banner');
let domProfileAvatar = document.getElementById('profile-avatar');
const domProfileNameSecondary = document.getElementById('profile-secondary-name');
const domProfileStatusSecondary = document.getElementById('profile-secondary-status');
const domProfileBadgeInvite = document.getElementById('profile-badge-invites');
const domProfileBadgeFawkes = document.getElementById('profile-badge-fawkes');
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

/** Resolve whether `npub` holds the V for Vector badge, with caching.
 *  Own profile → the persisted `badge_vector` flag (no network). Others →
 *  fetched once per session via check_fawkes_badge, then cached. */
async function resolveFawkesBadge(npub, isMine) {
    if (isMine) {
        if (_myBadges) return !!_myBadges.vector;
        try {
            _myBadges = await invoke('get_my_badges');
            return !!_myBadges?.vector;
        } catch { return false; }
    }
    if (_fawkesBadgeCache.has(npub)) return _fawkesBadgeCache.get(npub);
    try {
        const has = await invoke('check_fawkes_badge', { npub });
        _fawkesBadgeCache.set(npub, has);
        return has;
    } catch { return false; }
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
const domGroupOverviewName = document.getElementById('group-overview-name');
const domGroupOverviewStatus = document.getElementById('group-overview-status');
let domGroupOverviewAvatar = document.getElementById('group-overview-avatar');
const domGroupOverviewNameSecondary = document.getElementById('group-overview-secondary-name');
const domGroupOverviewDescription = document.getElementById('group-overview-description');
const domGroupOverviewMembers = document.getElementById('group-overview-members');
const domGroupMemberSearchInput = document.getElementById('group-member-search-input');
const domGroupInviteMemberBtn = document.getElementById('group-invite-member-btn');
const domGroupLeaveBtn = document.getElementById('group-leave-btn');

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
const domChatMessageInput = document.getElementById('chat-input');
const domChatMessageInputFile = document.getElementById('chat-input-file');
const domChatMessageInputCancel = document.getElementById('chat-input-cancel');
const domChatMessageInputEmoji = document.getElementById('chat-input-emoji');
const domAttachmentPanel = document.getElementById('attachment-panel');
const domAttachmentPanelMain = document.getElementById('attachment-panel-main');
const domAttachmentPanelFile = document.getElementById('attachment-panel-file');
const domAttachmentPanelFolder = document.getElementById('attachment-panel-folder');
const domAttachmentPanelMiniApps = document.getElementById('attachment-panel-miniapps');
const domAttachmentPanelMiniAppsView = document.getElementById('attachment-panel-miniapps-view');
const domMiniAppsGrid = document.getElementById('miniapps-grid');
const domMiniAppsSearch = document.getElementById('miniapps-search');
const domAttachmentPanelBack = document.getElementById('attachment-panel-back');
const domAttachmentPanelMarketplace = document.getElementById('attachment-panel-marketplace');
const domMarketplacePanel = document.getElementById('marketplace-panel');
const domMarketplaceBackBtn = document.getElementById('marketplace-back-btn');
const domMarketplaceContent = document.getElementById('marketplace-content');
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
const domCreateGroupBackBtn = document.getElementById('create-group-back-text-btn');
const domCreateGroupName = document.getElementById('create-group-name');
const domCreateGroupFilter = document.getElementById('create-group-filter');
const domCreateGroupList = document.getElementById('create-group-list');
const domCreateGroupCreateBtn = document.getElementById('create-group-create-btn');
const domCreateGroupCancelBtn = document.getElementById('create-group-cancel-btn');
const domCreateGroupStatus = document.getElementById('create-group-status');
const domCreateGroupDescription = document.getElementById('create-group-description');
const domCreateGroupAvatarPicker = document.getElementById('create-group-avatar-picker');
const domCreateGroupAvatarPreview = document.getElementById('create-group-avatar-preview');
const domCreateGroupAvatarPlaceholder = document.getElementById('create-group-avatar-placeholder');
const domCreateGroupAvatarEditIcon = document.getElementById('create-group-avatar-edit-icon');
const domSettings = document.getElementById('settings');
const domSettingsThemeSelect = document.getElementById('theme-select');
const domSettingsWhisperModelInfo = document.getElementById('whisper-model-info');
const domSettingsWhisperAutoTranslateInfo = document.getElementById('whisper-auto-translate-info');
const domSettingsWhisperAutoTranscribeInfo = document.getElementById('whisper-auto-transcribe-info');
const domSettingsPrivacyWebPreviewsInfo = document.getElementById('privacy-web-previews-info');
const domSettingsPrivacyStripTrackingInfo = document.getElementById('privacy-strip-tracking-info');
const domSettingsPrivacySendTypingInfo = document.getElementById('privacy-send-typing-info');
const domSettingsPrivacyTorInfo = document.getElementById('privacy-tor-info');
const domSettingsDisplayImageTypesInfo = document.getElementById('display-image-types-info');
const domSettingsChatBgInfo = document.getElementById('chat-bg-info');
const domSettingsNotifMuteInfo = document.getElementById('notif-mute-info');
const domSettingsNotifMuteEveryoneInfo = document.getElementById('notif-mute-everyone-info');
const domSettingsExportAccountInfo = document.getElementById('export-account-info');
const domSettingsChangePinInfo = document.getElementById('change-pin-info');
const domSettingsChangePinLabel = document.getElementById('change-pin-label');
const domSettingsLogoutInfo = document.getElementById('logout-info');
const domSettingsDonorsInfo = document.getElementById('donors-info');
const domDonorPivx = document.getElementById('donor-pivx');
const domDonorGitcoin = document.getElementById('donor-gitcoin');
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
    
    // Position tooltip above the element, centered horizontally
    tooltip.style.left = `${rect.left + rect.width / 2}px`;
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
 * Helper function to escape HTML
 */
function escapeHtml(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
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
    let text = contentToPreviewText(content);
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
    const truncated = truncateGraphemes(plain, maxLength);
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
 * @property {string} author_id - The HEX ID of the author who reacted.
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
    domChatMessageInput.disabled = true;
    domChatMessageInput.placeholder = 'This community has been dissolved.';
    domChatMessageInput.style.paddingLeft = '15px';
    domChatMessageInputFile.style.display = 'none';
    domChatMessageInputVoice.style.display = 'none';
    domChatMessageInputEmoji.style.display = 'none';
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
function resolveCommunityAvatars() {
    for (const chat of arrChats) {
        if (chat.chat_type !== 'Community') continue;
        const cf = chat.metadata?.custom_fields || {};
        if (cf.icon === '1' && cf.community_id && !chat.metadata.avatar_cached) {
            invoke('cache_community_image', { communityId: cf.community_id, isBanner: false })
                .then(path => {
                    if (path) {
                        chat.metadata.avatar_cached = path;
                        if (!fInit) renderChatlist();
                    }
                })
                .catch(() => {});
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
            domMsg.replaceWith(renderMessage(msg, profile, oldId));
        }
        strLastMsgID = eventId;
        softChatScroll();
    }
}

/**
 * Compute a timestamp for sorting chats, falling back to metadata for empty groups.
 * @param {Chat} chat
 * @returns {number}
 */
function getChatSortTimestamp(chat) {
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
    let lastActivity = lastMessage?.at || 0;

    if (!lastActivity) {
        // No real messages yet — fall back to join/creation time so a fresh community
        // sorts by when we joined, not to the bottom. custom_fields values are strings.
        lastActivity = Number(
            chat.metadata?.created_at ||
            chat.metadata?.custom_fields?.created_at ||
            chat.created_at ||
            0
        );
    }

    return lastActivity || 0;
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
function getProfile(npub) {
    return arrProfiles.find(p => p.id === npub);
}

/**
 * Get the best avatar URL for a profile
 * Prefers cached local file (for offline support), falls back to remote URL
 * @param {Profile} profile - The profile object
 * @returns {string|null} - The avatar URL to use, or null if none available
 */
function getProfileAvatarSrc(profile) {
    if (!profile) return null;
    // Prefer cached local path for offline support
    if (profile.avatar_cached) {
        return convertFileSrc(profile.avatar_cached);
    }
    // Fall back to remote URL
    return profile.avatar || null;
}

/**
 * Get the best banner URL for a profile
 * Prefers cached local file (for offline support), falls back to remote URL
 * @param {Profile} profile - The profile object
 * @returns {string|null} - The banner URL to use, or null if none available
 */
function getProfileBannerSrc(profile) {
    if (!profile) return null;
    // Prefer cached local path for offline support
    if (profile.banner_cached) {
        return convertFileSrc(profile.banner_cached);
    }
    // Fall back to remote URL
    return profile.banner || null;
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
                    if (domChats.style.display !== 'none') {
                        updateChatlistPreview(chat.id);
                    }
                }
            }
        });

        // Update chatlist timestamps every 6th tick (~30 seconds)
        if (maintenanceTick % 6 === 0 && domChats.style.display !== 'none') {
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
 * Load pending Community invites from the backend. The private bundle carries the name
 * (no icon/description), so we parse it out for display.
 */
async function loadCommunityInvites() {
    try {
        const invites = await invoke('list_community_invites');
        arrCommunityInvites = (invites || []).map(inv => {
            let name = 'Community';
            let channels = [];
            // The bundle carries name + the channel id(s) + keys; we pull the ids so Accept can
            // render the real community row optimistically (same channel id → no swap on reconcile).
            try {
                const b = JSON.parse(inv.bundle_json);
                name = b.name || name;
                channels = (b.channels || []).map(c => ({ id: c.id, name: c.name }));
            } catch (_) {}
            return { community_id: inv.community_id, name, inviter_npub: inv.inviter_npub, channels };
        });
        updateChatBackNotification();
    } catch (e) {
        console.error('Failed to load community invites:', e);
    }
}

/**
 * Add/refresh a Community's channel chats in the running session from a backend
 * CommunitySummary (so a freshly-joined/created Community appears without a reload).
 * Returns the first channel id (to navigate to), or null.
 */
async function surfaceCommunitySummary(summary) {
    if (!summary) return null;
    let firstChannel = null;
    let firstSync = null;
    for (const ch of summary.channels || []) {
        const chat = getOrCreateChat(ch.channel_id, 'Community');
        chat.metadata = chat.metadata || {};
        chat.metadata.custom_fields = chat.metadata.custom_fields || {};
        chat.metadata.custom_fields.name = summary.name;
        chat.metadata.custom_fields.description = summary.description || '';
        chat.metadata.custom_fields.community_id = summary.community_id;
        chat.metadata.custom_fields.is_owner = summary.is_owner ? 'true' : 'false';
        chat.metadata.custom_fields.dissolved = summary.dissolved ? 'true' : 'false';
        // Stamp the join moment so an empty community sorts to the top right away. Reloads
        // re-source this from the persisted DB created_at via upsert_community_chat.
        if (!chat.metadata.custom_fields.created_at) {
            chat.metadata.custom_fields.created_at = String(Date.now());
        }
        // Proven owner (verified attestation) → drives the crown + hoist + in-chat Owner tag.
        if (summary.owner_npub) chat.metadata.custom_fields.owner_npub = summary.owner_npub;
        else delete chat.metadata.custom_fields.owner_npub;
        if (summary.has_icon) chat.metadata.custom_fields.icon = '1';
        // The page-1 sync pulls existing history (e.g. the owner's welcome message) so the
        // channel isn't empty on open. Backend anti-stampede dedups a later open.
        const p = invoke('sync_community_channel', { channelId: ch.channel_id, beforeMs: null }).catch(() => {});
        if (!firstChannel) { firstChannel = ch.channel_id; firstSync = p; }
    }
    loadCommunityRoles(summary.community_id);
    resolveCommunityAvatars();
    renderChatlist();
    // If a warmed preload was promoted on Accept, the chat is ALREADY populated (its messages were
    // emitted by the backend), so open immediately — the first sync trues it up in the background.
    // Only await the sync when NOT preloaded (a cold join would otherwise open to an empty chat).
    if (firstSync && !summary.preloaded) await firstSync;
    renderChatlist();
    return firstChannel;
}

/**
 * Preload a community's admin roster into its channel chats' metadata. Message rendering reads
 * `metadata.admins` to show the admin tag + chip @everyone from admin senders; without this the
 * roster only loaded when Group Info opened, so admin tags + @everyone were dead until then.
 * Owner status comes from `owner_npub` (already on the chat); this fills the non-owner admin set.
 */
async function loadCommunityRoles(communityId) {
    if (!communityId) return;
    let adminNpubs;
    try { adminNpubs = await invoke('get_community_admins', { communityId }); } catch (_) { return; }
    for (const c of arrChats) {
        if (c.chat_type === 'Community' && c.metadata?.custom_fields?.community_id === communityId) {
            c.metadata = c.metadata || {};
            c.metadata.admins = adminNpubs.slice();
        }
    }
}

/**
 * Accept a pending Community invite → join + open it. Optimistic removal from the list.
 */
async function acceptCommunityInvite(communityId) {
    const snapshot = arrCommunityInvites;
    const invite = arrCommunityInvites.find(i => i.community_id === communityId);
    arrCommunityInvites = arrCommunityInvites.filter(i => i.community_id !== communityId);

    // Optimistic row: the bundle carries the channel id, so we render the real community row INSTANTLY
    // (locked, "Joining…") instead of leaving a dead zone. Same channel id means surfaceCommunitySummary
    // reconciles this exact chat later — no swap/flicker. It unlocks once read/writeable (control-fold/sync
    // resolves, or a message streams in — see the message_new handler).
    const optimisticChannelId = invite?.channels?.[0]?.id || null;
    if (optimisticChannelId) {
        const chat = getOrCreateChat(optimisticChannelId, 'Community');
        chat.metadata = chat.metadata || {};
        chat.metadata.custom_fields = chat.metadata.custom_fields || {};
        chat.metadata.custom_fields.name = invite.name || 'Community';
        chat.metadata.custom_fields.community_id = communityId;
        if (!chat.metadata.custom_fields.created_at) chat.metadata.custom_fields.created_at = String(Date.now());
        chat._joining = true; // renders locked
        // Re-sort so the fresh created_at floats the joining row to the TOP (renderChatlist itself
        // renders arrChats in order; the new chat was pushed to the end).
        arrChats.sort((a, b) => getChatSortTimestamp(b) - getChatSortTimestamp(a));
    }
    updateChatBackNotification();
    renderChatlist();
    adjustSize();

    try {
        const summary = await invoke('accept_community_invite', { communityId });
        await loadCommunityInvites();
        // surfaceCommunitySummary awaits the page-1 sync = control-folded + read/writeable.
        const channelId = await surfaceCommunitySummary(summary);
        clearCommunityJoining(communityId);
        adjustSize();
        if (channelId) openChat(channelId);
    } catch (e) {
        console.error('Failed to accept community invite:', e);
        // Roll back the optimistic row + restore the invite.
        if (optimisticChannelId) {
            const idx = arrChats.findIndex(c => c.id === optimisticChannelId);
            if (idx !== -1) arrChats.splice(idx, 1);
        }
        arrCommunityInvites = snapshot;
        updateChatBackNotification();
        renderChatlist();
        adjustSize();
        popupConfirm('Error', 'Failed to join Community: ' + escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * Release the "Joining…" lock on a community's channel rows (read/writeable now). Idempotent;
 * re-renders only if a locked row actually flipped, so the message_new early-unlock is cheap.
 */
function clearCommunityJoining(communityId) {
    let changed = false;
    for (const c of arrChats) {
        if (c._joining && c.chat_type === 'Community' && c.metadata?.custom_fields?.community_id === communityId) {
            c._joining = false;
            changed = true;
        }
    }
    if (changed) renderChatlist();
}

/**
 * Decline a pending Community invite (drops the parked bundle locally).
 */
async function declineCommunityInvite(communityId) {
    const snapshot = arrCommunityInvites;
    arrCommunityInvites = arrCommunityInvites.filter(i => i.community_id !== communityId);
    updateChatBackNotification();
    renderChatlist();
    adjustSize();
    try {
        await invoke('decline_community_invite', { communityId });
    } catch (e) {
        // Roll back the optimistic removal — otherwise the invite is gone from the UI but still
        // parked in the backend, and silently reappears on the next invite refresh.
        console.error('Failed to decline community invite:', e);
        arrCommunityInvites = snapshot;
        updateChatBackNotification();
        renderChatlist();
        adjustSize();
        popupConfirm('Error', 'Failed to decline invite: ' + escapeHtml(String(e)), true, '', 'vector_warning.svg');
    }
}

/**
 * Preview a public invite URL (or fragment) and offer to join. Shows the community name +
 * description with Join / Ignore. On Join, accepts and navigates into the new channel.
 */
let _communityJoinInFlight = false;
async function previewAndJoinCommunityLink(url) {
    // Re-entrancy guard: a double-paste, deep-link-while-pasting, or double-tap must not fire
    // two concurrent joins (which race surfaceCommunitySummary and hijack the shared popup).
    if (_communityJoinInFlight) return;
    _communityJoinInFlight = true;
    try {
        let preview;
        // Fetching the encrypted bundle off the relays can take several seconds — a PERSISTENT
        // toast (not the auto-timeout one) keeps feedback up for the whole await.
        showToast('Loading community invite…', true);
        try {
            preview = await invoke('preview_public_invite', { url });
        } catch (e) {
            hideToast();
            popupConfirm('Invalid Invite', 'This invite link could not be loaded.<br><br>' + escapeHtml(String(e)), true, '', 'vector_warning.svg');
            return;
        }
        const descHtml = preview.description ? `<br><br><span style="opacity:0.8;">${escapeHtml(preview.description)}</span>` : '';
        // Show the community's own logo when it has one, else the same placeholder the chat list
        // uses for logo-less communities (group-placeholder.svg).
        let iconSrc = 'group-placeholder.svg';
        if (preview.icon) {
            try {
                const path = await invoke('cache_invite_logo', { image: preview.icon });
                if (path) iconSrc = convertFileSrc(path);
            } catch (e) { console.debug('invite logo decrypt failed, using placeholder', e); }
        }
        hideToast();
        const confirmed = await popupConfirm(
            `Join ${escapeHtml(preview.name)}?`,
            `You've been invited to join <b>${escapeHtml(preview.name)}</b>.${descHtml}`,
            false, '', iconSrc, '', null, true
        );
        if (!confirmed) return;
        showToast(`Joining ${preview.name}…`, true);
        try {
            const summary = await invoke('accept_public_invite', { url });
            // Await the first-page sync so the channel opens with its history (not empty) and lands
            // in the right chat-list slot instead of at the bottom.
            const channelId = await surfaceCommunitySummary(summary);
            hideToast();
            // Only auto-open if the user is still parked on the chat list (no chat open) — don't
            // yank them out of a DM they opened while the multi-second join was in flight.
            if (channelId && !strOpenChat) openChat(channelId);
        } catch (e) {
            hideToast();
            popupConfirm('Failed to Join', escapeHtml(String(e)), true, '', 'vector_warning.svg');
        }
    } finally {
        _communityJoinInFlight = false;
    }
}

/** Detect a Vector community invite URL (or bare fragment) in pasted/typed text. */
function isCommunityInviteUrl(text) {
    return typeof text === 'string' && /vectorapp\.io\/invite\/?#|^#?[A-Za-z0-9_-]{40,}$/.test(text.trim()) && text.includes('#');
}

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

// Track pending status hide timeout
let statusHideTimeout = null;

/**
 * Update the chat header subtext (status/typing indicator) for the currently open chat
 * @param {Object} chat - The chat object
 */
// Cached member count per community id, for the chat-header subtext + overview status. Membership is
// derived from observed activity (best-effort), so this is refreshed live as people join/speak.
const communityMemberCounts = new Map();
const _communityCountLastFetch = new Map();
const _communityCountInFlight = new Set();

/** Render text for a community's member count, or '' if not yet known. */
function communityMemberSubtext(communityId) {
    const n = communityMemberCounts.get(communityId);
    return (n == null) ? '' : `${n} member${n === 1 ? '' : 's'}`;
}

/**
 * Refresh a community's cached member count and live-update the header/overview if open. Throttled to
 * one fetch per 2s per community (pass force=true to bypass, e.g. on a join/leave/control change).
 */
async function refreshCommunityMemberCount(communityId, force = false) {
    if (!communityId || _communityCountInFlight.has(communityId)) return;
    if (!force && Date.now() - (_communityCountLastFetch.get(communityId) || 0) < 2000) return;
    _communityCountInFlight.add(communityId);
    let members;
    try {
        members = await invoke('get_community_members', { communityId });
    } catch (_) {
        _communityCountInFlight.delete(communityId);
        return;
    }
    _communityCountInFlight.delete(communityId);
    _communityCountLastFetch.set(communityId, Date.now());
    if (communityMemberCounts.get(communityId) === members.length) return; // unchanged, no re-render
    communityMemberCounts.set(communityId, members.length);
    // Live-update the open channel's header (its chat carries this community_id) + the overview status.
    const openChat = strOpenChat ? arrChats.find(c => c.id === strOpenChat) : null;
    if (openChat && openChat.metadata?.custom_fields?.community_id === communityId) {
        updateChatHeaderSubtext(openChat);
    }
    if (domGroupOverview.getAttribute('data-group-id') === communityId) {
        domGroupOverviewStatus.textContent = communityMemberSubtext(communityId);
    }
}

function updateChatHeaderSubtext(chat) {
    if (!chat) return;

    // Clear any pending hide timeout
    if (statusHideTimeout) {
        clearTimeout(statusHideTimeout);
        statusHideTimeout = null;
    }

    let newStatusText = '';
    let shouldAddGradient = false;

    const isCommunity = chat.chat_type === 'Community';
    const fNotes = chat.id === strPubkey;

    // Check for typing indicators first (shared logic)
    const typingText = generateTypingText(chat);

    if (fNotes) {
        newStatusText = 'Encrypted Notes to Self';
        shouldAddGradient = false;
    } else if (typingText) {
        // Someone is typing - use shared helper
        newStatusText = typingText;
        shouldAddGradient = true;
    } else if (isCommunity) {
        // Show the member count as the subtext (typing, handled above, takes priority). The count is
        // per-community (a channel chat carries its community_id in custom_fields). Throttled refresh
        // keeps it live and re-renders this line when the count changes.
        const communityId = chat.metadata?.custom_fields?.community_id;
        newStatusText = communityMemberSubtext(communityId);
        shouldAddGradient = false;
        refreshCommunityMemberCount(communityId);
    } else {
        // DM - not typing, show profile status
        const profile = getProfile(chat.id);
        newStatusText = profile?.status?.title || '';
        shouldAddGradient = false;
    }
    
    const currentHasStatus = !!domChatContactStatus.textContent && !domChatContactStatus.classList.contains('status-hidden');
    const newHasStatus = !!newStatusText;
    
    if (newHasStatus) {
        // Show status: remove hidden class, update content, ensure visible
        domChatContactStatus.classList.remove('status-hidden');
        domChatContactStatus.style.display = ''; // Reset display in case it was hidden by else branch
        domChatContactStatus.textContent = newStatusText;
        domChatContactStatus.classList.toggle('typing-indicator-text', shouldAddGradient);
        if (!shouldAddGradient) {
            twemojify(domChatContactStatus);
        }
        domChatContact.classList.remove('chat-contact');
        domChatContact.classList.add('chat-contact-with-status');
    } else if (currentHasStatus) {
        // Hide status: add hidden class, wait for animation, then clear content
        domChatContactStatus.classList.add('status-hidden');
        domChatContact.classList.remove('chat-contact-with-status');
        domChatContact.classList.add('chat-contact');
        
        // Clear content after animation completes (300ms matches CSS transition)
        statusHideTimeout = setTimeout(() => {
            domChatContactStatus.textContent = '';
            domChatContactStatus.classList.remove('typing-indicator-text');
            statusHideTimeout = null;
        }, 300);
    }
    // If both are false (no status before, no status now), do nothing
}

/**
 * Replace @npub1... mentions in text with display names for previews/notifications
 * @param {string} text
 * @returns {string}
 */
function resolveMentionText(text) {
    if (!text) return text;
    return text.replace(/@(npub1[a-z0-9]{58})/g, (full, npub) => {
        const profile = getProfile(npub);
        if (profile) {
            return '@' + (profile.nickname || profile.name || npub);
        }
        return full;
    });
}

function updateChatBackNotification() {
    if (!domChatBackNotificationDot) return;
    
    // Check if we're currently in a chat
    if (!strOpenChat) {
        domChatBackNotificationDot.style.display = 'none';
        return;
    }
    
    // Check if there are any unanswered MLS invites
    const hasUnansweredInvites = arrCommunityInvites.length > 0;
    
    // Check if any OTHER chat has unread messages
    const hasOtherUnreads = arrChats.some(chat => {
        // Skip the currently open chat
        if (chat.id === strOpenChat) return false;
        
        // Skip chats with no messages (same as chatlist rendering)
        if (!chat.messages || chat.messages.length === 0) return false;
        
        // Skip our own profile (bookmarks/notes)
        if (chat.id === strPubkey) return false;
        
        // Get profile for DM chats
        const isGroup = chatIsGroup(chat);
        const profile = !isGroup && chat.participants.length === 1 ? getProfile(chat.id) : null;
        
        // Skip muted chats
        if (chat.muted) return false;
        
        // Check if this chat has unread messages
        return countUnreadMessages(chat) > 0;
    });
    
    // Show or hide the notification dot (show if there are unread messages OR unanswered invites)
    domChatBackNotificationDot.style.display = (hasOtherUnreads || hasUnansweredInvites) ? '' : 'none';
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

function markAsRead(chat, message) {
    // If we have a chat, and we haven't already marked as read, update its last_read and notify backend
    if (chat && message.id !== chat.last_read) {
        chat.last_read = message.id;

        // Persist via backend using chat-based API
        invoke("mark_as_read", { chatId: chat.id, messageId: message.id });
    }
}

/**
 * Send a NIP-17 message to a Nostr user
 * @param {string} pubkey - The user's pubkey
 * @param {string} content - The content of the message
 * @param {string?} replied_to - The reference of the message, if any
 */
async function message(pubkey, content, replied_to) {
    // Community channels send through their own envelope path (the DM/MLS `message`
    // command can't address a channel id). The backend drives the pending→sent/failed
    // lifecycle (optimistic bubble + finalize), so there's no pending id to finalize here.
    const chat = arrChats.find(c => c.id === pubkey);
    if (chat && chat.chat_type === 'Community') {
        await invoke('send_community_message', { channelId: pubkey, content, repliedTo: replied_to || '' });
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
            await invoke('send_community_files', { channelId: pubkey, content: '', filePaths: [filepath], nameOverrides: [''], useCompression: false, repliedTo: replied_to || '' });
        } else {
            // DMs use the protocol-agnostic file_message command.
            const result = await invoke("file_message", { receiver: pubkey, repliedTo: replied_to, filePath: filepath, nameOverride: '' });
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
 * Latest upload progress per pending message id. Buffers values that arrive before
 * the spinner DOM is rendered (updateChat awaits an IPC call before rendering, so the
 * first progress events can race ahead of message_new). renderMessage reads this when
 * creating an upload spinner so the initial paint matches the latest known progress.
 */
const pendingUploadProgress = new Map();

function applyPendingUploadProgress(spinner, pendingId) {
    const progress = pendingUploadProgress.get(pendingId);
    if (progress !== undefined) {
        spinner.style.setProperty('--progress', `${progress}%`);
    }
}

/**
 * Setup our Rust Event listeners, used for relaying the majority of backend changes
 */
async function setupRustListeners() {
    // Fire all listener registrations in parallel (each await listen() is an IPC round-trip)
    const _p = [];
    const _on = (event, handler) => _p.push(listen(event, handler));

    // A Community invite (npub gift-wrap) was parked → surface it as a pending slot.
    _on('community_invite_received', async (evt) => {
        await loadCommunityInvites();
        renderChatlist();
        adjustSize();
    });

    // §6.2 self-removal: a cooperative kick of us, a ban-rekey exclusion, OR a leave another device
    // authored. The backend already wiped this community's local data (retaining the epoch keys for a
    // later self-scrub). Silently mirror it in the UI — close the view + drop it from the list — with no
    // popup (the removal speaks for itself).
    _on('community_kicked', async (evt) => {
        const communityId = evt.payload?.community_id || evt.payload;
        if (!communityId) return;
        await removeCommunityFromUI(communityId);
    });

    // A control change (banlist / roles / metadata / invite-mode) landed in REALTIME (via the 3308
    // control-plane subscription). Re-read this community's summary into the chat list + re-render the
    // overview if it's open, so online members see name/role/mode changes live, not just on next open.
    _on('community_refreshed', async (evt) => {
        const communityId = evt.payload?.community_id || evt.payload;
        if (!communityId) return;
        // A control change (ban/role/mode/metadata) can move the roster — refresh the count immediately.
        refreshCommunityMemberCount(communityId, true);
        try {
            const summary = await invoke('get_community', { communityId });
            for (const c of arrChats) {
                if (c.metadata?.custom_fields?.community_id !== communityId) continue;
                const f = c.metadata.custom_fields;
                f.name = summary.name;
                f.description = summary.description || '';
                f.is_owner = summary.is_owner ? 'true' : 'false';
                f.dissolved = summary.dissolved ? 'true' : 'false';
                if (summary.owner_npub) f.owner_npub = summary.owner_npub;
            }
        } catch (_) {}
        // A control change may have promoted/demoted admins — refresh the cached roster so in-chat
        // admin tags + @everyone reflect it (the open overview re-fetches separately below).
        loadCommunityRoles(communityId);
        renderChatlist();
        // Re-render the open overview (re-fetches caps/members/banlist fresh) if it's this community.
        if (domGroupOverview.style.display !== 'none' && domGroupOverview.getAttribute('data-group-id') === communityId) {
            const chat = arrChats.find(c => c.metadata?.custom_fields?.community_id === communityId);
            if (chat) renderCommunityOverview(chat);
        }
        // Re-render the OPEN channel's header so a live metadata edit (name/description/icon) shows
        // immediately, not only after navigating away and back. The chatlist + overview refresh above.
        if (strOpenChat) {
            const open = arrChats.find(c => c.id === strOpenChat);
            if (open && open.metadata?.custom_fields?.community_id === communityId) {
                const isGroup = chatIsGroup(open);
                setChatHeader(open, isGroup ? null : getProfile(open.id), isGroup, open.id === strPubkey);
                // A community that seals WHILE it's the open view: openChat won't re-run, so lock the
                // composer + drop the end divider live here (the flag was refreshed above).
                if (chatIsDissolved(open)) applyDissolvedChatUI(open);
            }
        }
    });

    // A community synced in from another device (cross-device Community List, §6.3) appeared seamlessly —
    // render its metadata via the same path as a manual join so name/crown/members show without a restart.
    _on('community_surfaced', async (evt) => {
        const summary = evt.payload;
        if (!summary || !summary.community_id) return;
        await surfaceCommunitySummary(summary);
        refreshCommunityMemberCount(summary.community_id, true);
    });

    // Listen for system events (member joined/left, etc.)
    _on('system_event', async (evt) => {
        try {
            const { conversation_id, event_id, event_type, member_pubkey, member_name } = evt.payload || {};
            console.log('[System Event] Received:', event_id, event_type);

            // Deduplication by event_id
            const chat = arrChats.find(c => c.id === conversation_id);
            if (chat && chat.messages.some(msg => msg.id === event_id)) {
                console.log('[System Event] Skipping duplicate:', event_id);
                return;
            }

            // Resolve the actor's CURRENT cached name (member_name from the backend is null for
            // community presence; the name lives in our profile cache). Fetch it if unknown so a
            // later repaint/reload shows the real name instead of the npub.
            if (member_pubkey && !arrProfiles.some(p => p.id === member_pubkey) && !strangerProfileRequested.has(member_pubkey)) {
                strangerProfileRequested.add(member_pubkey);
                invoke('load_profile', { npub: member_pubkey }).catch(() => {});
            }
            const content = systemEventContent(event_type, member_pubkey);

            // Use the event's REAL time so it sorts chronologically. A join replayed during history paging /
            // rehydration would otherwise be stamped `now` and sink to the bottom of the chat.
            const atMs = Number(evt.payload?.created_at_ms) || Date.now();

            // Create system event message using the event_id
            const systemMsg = {
                id: event_id,
                at: atMs,
                content: content,
                mine: false,
                attachments: [],
                system_event: {
                    event_type: event_type,
                    member_npub: member_pubkey,
                }
            };

            // Add to chat messages via cache (handles deduplication)
            // Note: chat.messages and cache share the same array reference, so only use cache
            eventCache.addEvent(conversation_id, systemMsg);

            // Paint into the OPEN view only if this is genuinely the newest event. A historical replay (paging
            // / rehydration) is already in the cache at its real time and renders in order on the next chat
            // open/render — appending it to the bottom here would misplace it.
            if (strOpenChat === conversation_id && domChatMessages) {
                const newestAt = (chat?.messages || []).reduce((mx, m) => (m.id !== event_id && m.at > mx ? m.at : mx), 0);
                if (atMs >= newestAt) {
                    const systemElement = insertSystemEvent(content);
                    domChatMessages.appendChild(systemElement);
                    softChatScroll();
                }
                refreshChatEmptyState(); // a "X joined" landed in the open chat → drop the start marker
            }

            // Re-render chatlist
            renderChatlist();

            // A member join/leave moved the roster — refresh this community's cached member count.
            const evCommunityId = chat?.metadata?.custom_fields?.community_id;
            if (evCommunityId) refreshCommunityMemberCount(evCommunityId, true);
        } catch (e) {
            console.error('Error handling system_event:', e);
        }
    });

    // Listen for Synchronisation Finish updates
    // Badge cache resolved post-sync — lift emoji-pack limits if we hold the
    // Vector badge. Pure UI gating; the backend enforces authoritatively.
    _on('badges_updated', (evt) => {
        _myBadges = { vector: !!evt.payload?.vector };
        if (typeof applyBadgeLimits === 'function') applyBadgeLimits(_myBadges.vector);
        // If our own profile is open, reveal the badge live (no reopen needed).
        const ownProfileOpen = domProfile.style.display !== 'none'
            && domProfileId.textContent === strPubkey;
        if (_myBadges.vector && ownProfileOpen) {
            domProfileBadgeFawkes.style.display = '';
            domProfileBadgeFawkes.onclick = () => {
                popupConfirm('V for Vector Badge', `Acquired by logging in on Guy Fawkes Day&nbsp;(November 5, 2025).<br><br><i style="opacity: 0.5; font-size: 13px;">Remember, remember the 5th of November...</i>`, true, '', 'fawkes_mask.svg');
            };
        }
    });

    _on('sync_finished', async (_) => {
        // Mark sync as complete - this allows real-time messages to be cached
        fSyncComplete = true;
        
        // Fade out the sync line
        domSyncLine.classList.remove('active', 'progress');
        domSyncLine.style.removeProperty('--sync-progress');
        domSyncLine.classList.add('fade-out');

        // Wait for fade animation to complete, then reset
        setTimeout(() => {
            domSyncLine.classList.remove('fade-out');
            if (!strOpenChat) adjustSize();
        }, 300);
    });

    // Listen for Synchronisation Progress updates
    _on('sync_progress', (evt) => {
        if (fInit) return;
        const { mode, current, total } = evt.payload || {};
        if (mode === 'Syncing' && current && total) {
            // Determinate progress bar: fill left-to-right
            domSyncLine.classList.remove('active', 'fade-out');
            domSyncLine.classList.add('progress');
            domSyncLine.style.setProperty('--sync-progress', Math.min(current / total, 1));
        } else {
            // Indeterminate pulse (reconciliation phase — total unknown)
            if (!domSyncLine.classList.contains('active')) {
                domSyncLine.classList.remove('fade-out', 'progress');
                domSyncLine.classList.add('active');
            }
        }
        if (!strOpenChat) adjustSize();
    });

    // Listen for Attachment Upload Progress events
    // The spinner DOM is created asynchronously after message_new (updateChat awaits an IPC
    // call before rendering), so early progress events can arrive before the element exists.
    // Record the latest progress per pending_id so renderMessage can pick it up on creation.
    _on('attachment_upload_progress', async (evt) => {
        pendingUploadProgress.set(evt.payload.id, evt.payload.progress);
        const divUpload = document.getElementById(evt.payload.id + '_file');
        if (divUpload) {
            divUpload.style.setProperty('--progress', `${evt.payload.progress}%`);
        }
    });

    // Smoothly interpolated download speed display: id → { display, target, factor, lastUpdate, raf }
    const downloadSpeedLerp = new Map();

    function updateSpeedDisplay(attachId) {
        const lerp = downloadSpeedLerp.get(attachId);
        if (!lerp) return;

        // Adaptive lerp: factor is tuned so animation spans ~1 chunk interval
        lerp.display += (lerp.target - lerp.display) * lerp.factor;

        // Snap when close enough (within 0.5 KB/s)
        if (Math.abs(lerp.target - lerp.display) < 500) lerp.display = lerp.target;

        const escapedId = CSS.escape(attachId);
        const speedDecimals = lerp.display >= 1048576 ? 2 : 0;
        const speedText = lerp.display > 0 ? ` · ${formatBytes(lerp.display, speedDecimals, true)}/s` : '';
        const statusEls = document.querySelectorAll(`.miniapp-downloading-spinner[data-attachment-id="${escapedId}"]`);
        for (const spinner of statusEls) {
            const fileBox = spinner.closest('.custom-audio-player');
            if (!fileBox) continue;
            const status = fileBox.querySelector('.file-status');
            if (status) status.innerText = `Downloading${speedText}`;
        }

        // Keep animating if not settled
        if (lerp.display !== lerp.target) {
            lerp.raf = requestAnimationFrame(() => updateSpeedDisplay(attachId));
        } else {
            lerp.raf = null;
        }
    }

    // Listen for backend error toasts
    _on('show_toast', (evt) => {
        showToast(evt.payload || 'An Error Occurred');
    });

    // Listen for Attachment Download Progress events
    _on('attachment_download_progress', async (evt) => {
        if (!strOpenChat) return;
        const attachId = evt.payload.id;
        const escapedId = CSS.escape(attachId);

        // Update speed lerp target from backend
        if (evt.payload.bytesPerSec != null && evt.payload.bytesPerSec > 0) {
            const now = performance.now();
            let lerp = downloadSpeedLerp.get(attachId);
            if (!lerp) {
                lerp = { display: evt.payload.bytesPerSec, target: evt.payload.bytesPerSec, factor: 0.05, lastUpdate: now, raf: null };
                downloadSpeedLerp.set(attachId, lerp);
            } else {
                // Adapt lerp factor based on time between chunks
                // factor = 3 / (dt_seconds * 60) → animation reaches ~95% in ~dt seconds
                const dtSec = (now - lerp.lastUpdate) / 1000;
                if (dtSec > 0.05) {
                    lerp.factor = Math.min(0.15, Math.max(0.008, 3.0 / (dtSec * 60)));
                }
                lerp.target = evt.payload.bytesPerSec;
                lerp.lastUpdate = now;
            }
            if (!lerp.raf) {
                lerp.raf = requestAnimationFrame(() => updateSpeedDisplay(attachId));
            }
        }

        // Clean up on completion
        if (evt.payload.progress >= 100) {
            const lerp = downloadSpeedLerp.get(attachId);
            if (lerp && lerp.raf) cancelAnimationFrame(lerp.raf);
            downloadSpeedLerp.delete(attachId);
        }

        // Update ALL conical progress spinners with this attachment ID (handles deduplication)
        const spinners = document.querySelectorAll(`.miniapp-downloading-spinner[data-attachment-id="${escapedId}"]`);
        if (spinners.length) {
            for (const spinner of spinners) {
                spinner.style.setProperty('--progress', `${evt.payload.progress}%`);
            }
        }

        // Update file-status text (initial update before lerp kicks in)
        if (!downloadSpeedLerp.has(attachId)) {
            const statusEls = document.querySelectorAll(`.miniapp-downloading-spinner[data-attachment-id="${escapedId}"]`);
            for (const spinner of statusEls) {
                const fileBox = spinner.closest('.custom-audio-player');
                if (!fileBox) continue;
                const status = fileBox.querySelector('.file-status');
                if (status) status.innerText = 'Downloading';
            }
        }
    });

    // Listen for Attachment Download Results
    _on('attachment_download_result', async (evt) => {
        // Update the in-memory attachment (works for both DMs and Group Chats)
        let cChat = getChat(evt.payload.profile_id);
        if (!cChat) return;

        let cMsg = cChat.messages.find(m => m.id === evt.payload.msg_id);
        if (!cMsg) return;

        // When an attachment is being updated (i.e: post-hashing ID change), we reference the original nonce-based hash via old_id, otherwise, we use ID, as nothing changed
        const matchId = evt.payload?.old_id || evt.payload.id;
        let cAttachment = cMsg.attachments.find(a => a.id === matchId);
        if (!cAttachment) return;

        cAttachment.downloading = false;
        cAttachment.download_failed = false;
        downloadingAttachmentIds.delete(matchId);
        downloadingAttachmentIds.delete(evt.payload.id);
        for (const id of [matchId, evt.payload.id]) {
            const lerp = downloadSpeedLerp.get(id);
            if (lerp) { if (lerp.raf) cancelAnimationFrame(lerp.raf); downloadSpeedLerp.delete(id); }
        }
        if (evt.payload.success) {
            cAttachment.downloaded = true;
            // Update path from backend result (always has the correct file path)
            if (evt.payload.result) {
                cAttachment.path = evt.payload.result;
            }
            // Update ID if hash changed (nonce → blossom hash)
            if (evt.payload.old_id) {
                cAttachment.id = evt.payload.id;
            }

            // Update ALL not-yet-downloaded in-memory attachments with the same hash (deduplication)
            // and collect their message IDs for re-rendering
            // Skip already-downloaded attachments — they have valid paths and loaded metadata
            const affectedMsgIds = new Set();
            affectedMsgIds.add(evt.payload.msg_id);
            for (const msg of cChat.messages) {
                if (msg.id === evt.payload.msg_id) continue;
                for (const att of msg.attachments) {
                    if (att.id === matchId && !att.downloaded) {
                        att.downloading = false;
                        att.downloaded = true;
                        att.download_failed = false;
                        att.path = cAttachment.path;
                        if (evt.payload.old_id) {
                            att.id = evt.payload.id;
                        }
                        affectedMsgIds.add(msg.id);
                    }
                }
            }

            // Re-render all affected messages in the open chat
            if (strOpenChat === evt.payload.profile_id) {
                const profile = getProfile(evt.payload.profile_id);
                for (const msgId of affectedMsgIds) {
                    const domMsg = document.getElementById(msgId);
                    const memMsg = cChat.messages.find(m => m.id === msgId);
                    if (domMsg && memMsg) {
                        // Shrink + fade out any active spinners before re-rendering
                        const spinners = domMsg.querySelectorAll('.miniapp-downloading-spinner');
                        if (spinners.length) {
                            for (const sp of spinners) {
                                sp.style.transition = 'opacity 0.2s ease, scale 0.2s ease';
                                sp.style.opacity = '0';
                                sp.style.scale = '0.5';
                            }
                            setTimeout(() => {
                                const newEl = renderMessage(memMsg, profile, msgId);
                                domMsg.replaceWith(newEl);
                                // Grow + fade in the new icon
                                const icon = newEl.querySelector('.custom-audio-player > span[class*="icon-"], .custom-audio-player > img');
                                if (icon) {
                                    icon.style.opacity = '0';
                                    icon.style.scale = '0.5';
                                    icon.style.transition = 'opacity 0.25s ease, scale 0.25s ease';
                                    requestAnimationFrame(() => { icon.style.opacity = '1'; icon.style.scale = '1'; });
                                }
                                softChatScroll();
                            }, 200);
                        } else {
                            domMsg.replaceWith(renderMessage(memMsg, profile, msgId));
                        }
                    }
                }
                softChatScroll();
            }
        } else {
            // Download failed — mark as failed to prevent auto-download retry loop, then re-render
            if (strOpenChat === evt.payload.profile_id) {
                const profile = getProfile(evt.payload.profile_id);
                for (const msg of cChat.messages) {
                    const hasAtt = msg.attachments.some(a => a.id === matchId);
                    if (hasAtt) {
                        const domMsg = document.getElementById(msg.id);
                        if (domMsg) {
                            for (const att of msg.attachments) {
                                if (att.id === matchId) {
                                    att.downloading = false;
                                    att.download_failed = true;
                                }
                            }
                            domMsg.replaceWith(renderMessage(msg, profile, msg.id));
                        }
                    }
                }
            }
        }
    });

    // Listen for profile updates
    _on('profile_update', (evt) => {
        // Check if the frontend is already aware
        const nProfileIdx = arrProfiles.findIndex(p => p.id === evt.payload.id);
        let avatarCacheChanged = false;
        if (nProfileIdx >= 0) {
            // Check if avatar cache changed (for triggering chatlist re-render)
            avatarCacheChanged = arrProfiles[nProfileIdx].avatar_cached !== evt.payload.avatar_cached;

            // Update our frontend memory
            arrProfiles[nProfileIdx] = evt.payload;

            // If this is our profile, make sure to render it's changes
            if (arrProfiles[nProfileIdx].mine) {
                renderCurrentProfile(arrProfiles[nProfileIdx]);
            }
        } else {
            // Add the new profile
            arrProfiles.push(evt.payload);
            avatarCacheChanged = !!evt.payload.avatar_cached;
        }

        // If this user has an open chat, then soft-update the chat header
        if (strOpenChat === evt.payload.id) {
            const chat = getDMChat(evt.payload.id);
            const profile = getProfile(evt.payload.id);
            if (chat && profile) {
                updateChat(chat, [], profile);
            }
        }

        // Re-render chatlist if avatar cache changed (so cached images show up)
        if (avatarCacheChanged && !strOpenChat) {
            renderChatlist();
        }
        
        // Update any profile previews in the chat messages for this npub (regardless of which chat is open)
        const profilePreviews = document.querySelectorAll(`.msg-profile-preview[data-npub="${evt.payload.id}"]`);
        profilePreviews.forEach(preview => {
            updateNostrProfilePreview(preview, evt.payload);
        });
        
        // Skip Expanded Profile View repaints during our own edit mode —
        // backend may emit stale `banner_cached` and clobber the just-picked image.
        if (domProfile.style.display !== 'none' && domProfileId.textContent === evt.payload.id) {
            const isOwnEditingProfile = fProfileEditMode && evt.payload.mine;
            if (!isOwnEditingProfile) {
                renderProfileTab(evt.payload);
            }
        }

        // Refresh the mini profile popup if it's open for this npub.
        if (typeof refreshMiniProfileIfMatches === 'function') {
            refreshMiniProfileIfMatches(evt.payload.id);
        }

        // Re-render create group or invite modal if a stranger npub's profile resolved
        if (arrSelectedGroupMembers.includes(evt.payload.id) && domCreateGroup?.style.display !== 'none') {
            renderCreateGroupList(domCreateGroupFilter?.value || '');
        }
        if (activeInviteModalRerender) {
            activeInviteModalRerender();
        }

        // Render the Chat List
        renderChatlist();
    });

    _on('chat_muted', (evt) => {
        const cChat = arrChats.find(c => c.id === evt.payload.chat_id);
        if (cChat) {
            cChat.muted = evt.payload.value;
        }

        // If this chat's profile is expanded, update the Mute UI
        const domMuteBtn = document.getElementById('profile-option-mute');
        if (domMuteBtn && domProfileId.textContent === evt.payload.chat_id) {
            domMuteBtn.querySelector('span').classList.replace('icon-volume-' + (evt.payload.value ? 'max' : 'mute'), 'icon-volume-' + (evt.payload.value ? 'mute' : 'max'));
            domMuteBtn.querySelector('p').innerText = evt.payload.value ? 'Unmute' : 'Mute';
        }

        // If this group's overview is open, update the mute button
        const domGrpMuteBtn = document.getElementById('group-mute-btn');
        if (domGrpMuteBtn && domGroupOverview.style.display !== 'none' && strOpenChat === evt.payload.chat_id) {
            domGrpMuteBtn.querySelector('span').className = `icon icon-volume-${evt.payload.value ? 'mute' : 'max'} navbar-icon`;
            domGrpMuteBtn.querySelector('p').innerText = evt.payload.value ? 'Unmute' : 'Mute';
        }

        // Re-render the chat list to immediately reflect glow/badge changes
        renderChatlist();
    });

    _on('profile_nick_changed', (evt) => {
        // Update the profile's nickname
        const cProfile = getProfile(evt.payload.profile_id);
        if (cProfile) {
            cProfile.nickname = evt.payload.value;

            // If this profile is Expanded, update the UI
            if (domProfileId.textContent === evt.payload.profile_id) {
                renderProfileTab(cProfile);
            }
        }
    });

    // PIVX payment events — handler in pivx.js
    _on('pivx_payment_received', handlePivxPaymentReceived);

    // Listen for typing indicator updates (both DMs and Groups)
    _on('typing-update', (evt) => {
        const { conversation_id, typers } = evt.payload;

        // Find the chat (could be DM or group)
        const chat = arrChats.find(c => c.id === conversation_id);
        if (!chat) return;

        // Store the typers array and update timestamp
        chat.active_typers = typers || [];
        chat.last_typing_update = Date.now() / 1000;

        // If this chat is currently open, update the chat header subtext
        if (strOpenChat === conversation_id) {
            updateChatHeaderSubtext(chat);
        }

        // Update the chat list preview in-place (typing doesn't affect sort
        // order, so a full renderChatlist() would just churn DOM and waste
        // cycles on every keystroke from the other side).
        updateChatlistPreview(conversation_id);
    });

    // Listen for incoming DM messages
    _on('message_new', (evt) => {
        // chat_id is the npub for DMs, the group id for MLS, the channel id for
        // Communities. Resolve the existing chat by id first; only create when truly new,
        // picking the type by id shape (npub → DM, otherwise a Community channel).
        let chat = arrChats.find(c => c.id === evt.payload.chat_id);
        if (!chat) {
            chat = evt.payload.chat_id.startsWith('npub1')
                ? getOrCreateDMChat(evt.payload.chat_id)
                : getOrCreateChat(evt.payload.chat_id, 'Community');
        }
        
        // Early-unlock an optimistic "Joining…" row the moment a message streams in (proves
        // read access) — the other release path is the control-fold/sync resolving in acceptCommunityInvite.
        if (chat._joining) clearCommunityJoining(chat.metadata?.custom_fields?.community_id);

        // Get the new message
        const newMessage = evt.payload.message;

        // Add to event cache
        // During sync, only add if this chat is currently open (to avoid cache flooding)
        // After sync complete, always add to cache
        const shouldAddToCache = fSyncComplete || chat.id === strOpenChat;
        let cacheInsertedIntoChatMessages = false;
        if (shouldAddToCache) {
            const added = eventCache.addEvent(chat.id, newMessage);
            if (!added) return;
            // openChat assigns chat.messages = entry.events, so the two often
            // share an array reference. addEvent already inserted into that
            // shared array — a second manual insertion below would duplicate.
            cacheInsertedIntoChatMessages = chat.messages === eventCache.getEventsRef(chat.id);
        }

        // Clear typing indicator for the sender when they send a message
        if (!newMessage.mine && chat.active_typers) {
            // Remove the sender from active typers
            chat.active_typers = chat.active_typers.filter(npub => npub !== chat.id);
        }

        if (!cacheInsertedIntoChatMessages) {
            // Find the correct position to insert the message based on timestamp
            const messages = chat.messages;

            // Check if the array is empty or the new message is newer than (or equal to) the newest message
            if (messages.length === 0 || newMessage.at >= messages[messages.length - 1].at) {
                // Insert at the end (newest)
                messages.push(newMessage);
            }
            // Check if the new message is older than the oldest message
            else if (newMessage.at < messages[0].at) {
                // Insert at the beginning (oldest)
                messages.unshift(newMessage);
            }
            // Otherwise, find the correct position in the middle
            else {
                // Binary search for better performance with large message arrays
                let low = 0;
                let high = messages.length - 1;

                while (low <= high) {
                    const mid = Math.floor((low + high) / 2);

                    if (messages[mid].at < newMessage.at) {
                        low = mid + 1;
                    } else {
                        high = mid - 1;
                    }
                }

                // Insert the message at the correct position (low is now the index where it should go)
                messages.splice(low, 0, newMessage);
            }
        }

        // Newest-first chat list sort (independent of how the message landed
        // in chat.messages).
        if (newMessage.at >= (chat.messages[chat.messages.length - 1]?.at ?? 0)) {
            arrChats.sort((a, b) => getChatSortTimestamp(b) - getChatSortTimestamp(a));
        }

        // If this user has the open chat, then update the chat too
        if (strOpenChat === chat.id) {
            updateChat(chat, [newMessage]);
            refreshChatEmptyState(); // first message in a fresh community → drop the start marker
            // Increment rendered count since we're adding a new message
            proceduralScrollState.renderedMessageCount++;
            proceduralScrollState.totalMessageCount++;
            // Open chat + pinned + window actually visible = user saw it
            // land. Tabbed-out arrivals stay unread until refocus, even when
            // the chat is open and pinned.
            if (!newMessage.mine && chatPinnedToBottom && isWindowActive()) {
                markAsRead(chat, newMessage);
                clearUnreadDivider();
            }
            // Own-send catches up to the latest non-mine message AT OR BEFORE this send
            // (never past it — see findLatestContactMessage).
            if (newMessage.mine) {
                const lastContactMsg = findLatestContactMessage(chat.messages, newMessage.at);
                if (lastContactMsg) markAsRead(chat, lastContactMsg);
            }
        } else if (newMessage.mine) {
            // Own message synced from another device — mark read up to the latest contact
            // message no newer than this send. Bounding by `newMessage.at` is what keeps a
            // boot sweep's replay of our OLD sends from marking genuinely-new arrivals read.
            const lastContactMsg = findLatestContactMessage(chat.messages, newMessage.at);
            if (lastContactMsg) markAsRead(chat, lastContactMsg);
        }

        // Render the Chat List (only when user is viewing it)
        if (!strOpenChat) renderChatlist();

        // Update the back button notification dot (for unread messages in other chats)
        updateChatBackNotification();
    });

    // Listen for existing message updates (works for both DMs and MLS groups)
    _on('message_update', (evt) => {
        // Drop any buffered upload progress for this pending id (spinner is gone after re-render)
        pendingUploadProgress.delete(evt.payload.old_id);

        // Find the message we're updating
        const cChat = getChat(evt.payload.chat_id);
        if (!cChat) return;

        const nMsgIdx = cChat.messages.findIndex(m => m.id === evt.payload.old_id);
        if (nMsgIdx === -1) return;

        // Update it
        cChat.messages[nMsgIdx] = evt.payload.message;
        
        // Also update the event cache
        // This is important for pending->sent transitions where the ID changes
        if (eventCache.has(evt.payload.chat_id)) {
            const cachedEvents = eventCache.getEvents(evt.payload.chat_id);
            if (cachedEvents) {
                const cacheIdx = cachedEvents.findIndex(m => m.id === evt.payload.old_id);
                if (cacheIdx !== -1) {
                    cachedEvents[cacheIdx] = evt.payload.message;
                }
                // Keep the dedup Set in sync with the id swap, else the relay echo of
                // the finalized id renders as a duplicate (esp. for Community sends).
                if (evt.payload.old_id !== evt.payload.message.id) {
                    eventCache.replaceId(evt.payload.chat_id, evt.payload.old_id, evt.payload.message.id);
                }
            }
        }

        // If this chat is open, then update the rendered message
        if (strOpenChat === evt.payload.chat_id) {
            // If `message_update` arrives before `message_new` has rendered the row,
            // `domMsg` is null and the `?.replaceWith` below is a no-op. The next
            // `message_new` will render the up-to-date message from chat.messages,
            // so missing the surgical update here is safe.
            const domMsg = document.getElementById(evt.payload.old_id);

            // Reaction-only updates (the common case) skip the full rebuild so
            // video playback, audio playhead, and spoiler reveal aren't reset.
            // Anything else (edits, pending→sent, attachment downloaded, etc.)
            // still rebuilds the whole row.
            if (domMsg && _dmsgIsReactionOnlyChange(domMsg._dmsgMsg, evt.payload.message)) {
                _dmsgReplaceReactions(domMsg, evt.payload.message);
            } else {
                const profile = getProfile(evt.payload.chat_id);
                domMsg?.replaceWith(renderMessage(evt.payload.message, profile, evt.payload.old_id));
            }

            // If the old ID was a pending ID (our message), make sure to update and scroll accordingly
            if (evt.payload.old_id.startsWith('pending')) {
                strLastMsgID = evt.payload.message.id;
                softChatScroll();
            }

            // Update any reply contexts that quote this edited message
            const editedMsgId = evt.payload.message.id;
            const newContent = evt.payload.message.content;

            // Find all messages that reply to this edited message and update their reply preview
            const replyElements = document.querySelectorAll(`[id="r-${editedMsgId}"]`);
            for (const replyEl of replyElements) {
                const replyTextSpan = replyEl.querySelector('.dmsg-reply-text');
                if (replyTextSpan && newContent) {
                    replyTextSpan.innerHTML = buildReplyPreviewHtml(newContent);
                    twemojify(replyTextSpan);
                    const editedTags = evt.payload.message.emoji_tags;
                    if (editedTags && editedTags.length && typeof renderCustomEmojiShortcodes === 'function') {
                        renderCustomEmojiShortcodes(replyTextSpan, editedTags);
                    }
                }
            }

            // Also update the replied_to_content in cached message data
            for (const msg of cChat.messages) {
                if (msg.replied_to === editedMsgId) {
                    msg.replied_to_content = newContent;
                }
            }
        }

        // Update chatlist preview if the edited message is the last message in the chat
        // This efficiently updates just the preview text instead of re-rendering the entire chatlist
        const isLastMessage = nMsgIdx === cChat.messages.length - 1;
        if (isLastMessage) {
            updateChatlistPreview(evt.payload.chat_id);
        }
    });

    // Listen for message removal (e.g., cancelled upload, deleted failed message)
    _on('message_removed', (evt) => {
        const { id, chat_id, reason } = evt.payload;
        // Drop any buffered upload progress (e.g. on cancel)
        pendingUploadProgress.delete(id);
        const cChat = getChat(chat_id);
        if (!cChat) return;

        // Remove from in-memory messages
        const msgIdx = cChat.messages.findIndex(m => m.id === id);
        if (msgIdx !== -1) cChat.messages.splice(msgIdx, 1);

        // Remove from event cache
        if (eventCache.has(chat_id)) {
            const cachedEvents = eventCache.getEvents(chat_id);
            if (cachedEvents) {
                const cacheIdx = cachedEvents.findIndex(m => m.id === id);
                if (cacheIdx !== -1) cachedEvents.splice(cacheIdx, 1);
            }
        }

        // Fade out and remove DOM element if this chat is open
        if (strOpenChat === chat_id) {
            const domMsg = document.getElementById(id);
            if (domMsg) {
                // If the floating toolbar was anchored to this row, hide it —
                // the row is about to vanish and the toolbar would otherwise
                // stay at its last position pointing at nothing.
                if (_dmsgToolbarTarget === domMsg) hideMessageToolbar();

                // Remember the row that follows ours so we can re-evaluate its
                // streak attribute after removal (it may flip first ↔ continuation).
                const followingRow = domMsg.classList.contains('dmsg')
                    ? _dmsgWalkForwardToRow(domMsg.nextElementSibling)
                    : null;

                domMsg.style.transition = 'opacity 0.2s ease, max-height 0.3s ease';
                domMsg.style.opacity = '0';
                domMsg.style.maxHeight = domMsg.offsetHeight + 'px';
                domMsg.style.overflow = 'hidden';
                requestAnimationFrame(() => {
                    domMsg.style.maxHeight = '0';
                    domMsg.style.marginBottom = '0';
                    domMsg.style.paddingTop = '0';
                    domMsg.style.paddingBottom = '0';
                });
                setTimeout(() => {
                    domMsg.remove();
                    // Remove trailing timestamp if it's now the last element in the chat
                    const lastChild = domChatMessages.lastElementChild;
                    if (lastChild && lastChild.classList.contains('msg-inline-timestamp')) {
                        lastChild.remove();
                    }
                    // Re-evaluate streak on the row that now succeeds the gap.
                    if (followingRow) {
                        const msg = _dmsgLookupMessage(followingRow);
                        if (msg) followingRow.dataset.streak = _dmsgComputeStreakAttr(msg, followingRow.previousElementSibling);
                    }
                }, 100);
            }
        }

        // Toast only for actual cancelled uploads. Deletions (failed-message
        // cleanup, self-delete, admin-hide, cooperative-hide receiver) all
        // skip the toast — the row vanishing is signal enough.
        if (reason === 'cancelled') {
            showToast('Upload Cancelled');
        }

        // Re-render the chatlist (not just the preview) so the unread glow
        // recomputes — when an unread message is deleted the chat may flip
        // back to a fully-read state, which an in-place preview update can't
        // express. Deletions are rare enough that a full render is fine.
        renderChatlist();

        // Recompute the OS taskbar badge — if the deleted message was unread,
        // the badge would otherwise stay stuck on its pre-deletion count.
        invoke('update_unread_counter');
    });

    // Listen for headless mark-as-read (e.g., notification "Mark Read" action while app backgrounded)
    _on('chat_mark_read', (evt) => {
        const { chat_id, last_read } = evt.payload;
        const cChat = getChat(chat_id);
        if (cChat && last_read) {
            cChat.last_read = last_read;
            // Re-render the chat preview element in-place (border, font color, etc. all depend on unread state)
            const oldEl = document.getElementById(`chatlist-${chat_id}`);
            if (oldEl) {
                const primaryColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
                const newEl = renderChat(cChat, primaryColor);
                oldEl.replaceWith(newEl);
            }
        }
    });

    // Listen for attachment URL updates (for file uploads and reuse)
    // Live wallpaper upload progress — drives the conic-gradient ring on
    // the Set Wallpaper button during the encrypt+upload step.
    _on('wallpaper_upload_progress', (evt) => {
        const { chat_id, progress } = evt.payload || {};
        if (!chat_id || strOpenChat !== chat_id) return;
        if (!wallpaperPreviewState || wallpaperPreviewState.chatId !== chat_id) return;
        setWallpaperUploadProgress(progress || 0);
    });

    // Per-DM wallpaper changes. Both directions land here: our own publish
    // emits this with the new active path + slider values, and inbound
    // rumors from the counterparty (or another of our devices) do too.
    _on('wallpaper_updated', (evt) => {
        const { chat_id, path, ts, blur, dim } = evt.payload || {};
        if (!chat_id) return;
        const cChat = getChat(chat_id);
        if (cChat) {
            cChat.wallpaper_path = path || '';
            cChat.wallpaper_ts = ts || 0;
            if (typeof blur === 'number') cChat.wallpaper_blur = blur;
            if (typeof dim === 'number') cChat.wallpaper_dim = dim;
        }
        if (strOpenChat === chat_id) {
            // Ignore if a preview is currently up for this chat — the user
            // is mid-decision and we already swapped the layer to the
            // staged file. The next openChat will hydrate from chat.* fields.
            if (wallpaperPreviewState && wallpaperPreviewState.chatId === chat_id) return;
            applyChatWallpaper(chat_id, path || '', blur, dim, ts);
        }
    });

    _on('attachment_update', (evt) => {
        const { chat_id, message_id, attachment_id, url } = evt.payload;
        const cChat = getChat(chat_id);
        if (!cChat) return;

        // Find the message
        const msg = cChat.messages.find(m => m.id === message_id);
        if (!msg || !msg.attachments) return;

        // Find and update the attachment
        const att = msg.attachments.find(a => a.id === attachment_id);
        if (att) {
            att.url = url;
            // Re-render if this chat is open
            if (strOpenChat === chat_id) {
                const domMsg = document.getElementById(message_id);
                if (domMsg) {
                    const profile = getProfile(chat_id);
                    domMsg.replaceWith(renderMessage(msg, profile, message_id));
                }
            }
        }
    });

    // Listen for Vector Voice AI (Whisper) model download progression updates
    _on('whisper_download_progress', async (evt) => {
        const { progress, downloaded_bytes, total_bytes, speed_bps } = evt.payload;
        const spanProgression = document.getElementById('voice-model-download-progression');
        if (spanProgression) {
            if (downloaded_bytes && total_bytes) {
                const dlMB = (downloaded_bytes / (1024 * 1024)).toFixed(1);
                const totalMB = (total_bytes / (1024 * 1024)).toFixed(1);
                spanProgression.textContent = `(${dlMB}/${totalMB} MB)`;
            } else {
                spanProgression.textContent = `(${progress}%)`;
            }
        }
    });

    // Listen for Windows-specific Overlay Icon update requests
    // Note: this API seems unavailable in Tauri's Rust backend, so we're using the JS API as a workaround
    _on('update_overlay_icon', async (evt) => {
        // Enable or Disable our notification badge Overlay Icon
        await getCurrentWindow().setOverlayIcon(evt.payload.enable ? "./icons/icon_badge_notification.png" : undefined);
    });

    _on('blossom_servers_updated', () => {
        if (typeof renderRelayList === 'function') renderRelayList();
    });

    _on('blossom_capabilities_updated', () => {
        if (currentBlossomInfo) {
            renderBlossomCapabilities(currentBlossomInfo.url, ++_blossomCapsToken);
        }
    });

    // Listen for relay status changes
    _on('relay_status_change', (evt) => {
        // Update the relay status in the network list
        const relayItem = document.querySelector(`[data-relay-url="${evt.payload.url}"]`);
        if (relayItem) {
            const statusElement = relayItem.querySelector('.relay-status');
            if (statusElement) {
                // Remove all status classes
                statusElement.classList.remove('connected', 'connecting', 'disconnected', 'pending', 'initialized', 'terminated', 'banned', 'sleeping');
                // Add the new status class
                statusElement.classList.add(evt.payload.status);
                // Update the text
                statusElement.textContent = evt.payload.status;
            }
        }

        // Also update the info dialog if it's open for this relay
        if (currentRelayInfo && currentRelayInfo.url.toLowerCase() === evt.payload.url.toLowerCase()) {
            const dialogStatus = document.getElementById('relay-info-status');
            if (dialogStatus) {
                dialogStatus.textContent = evt.payload.status;
                dialogStatus.className = `relay-status ${evt.payload.status}`;
            }
            currentRelayInfo.status = evt.payload.status;
        }
    });

    // Listen for Mini App realtime status updates (peer count changes)
    _on('miniapp_realtime_status', (evt) => {
        const { topic, peer_count, is_active, has_pending_peers, peers } = evt.payload;
        console.log('[MINIAPP] Realtime status update:', topic, 'peers:', peer_count, 'active:', is_active, 'npubs:', peers);

        // Find all Mini App attachments with this topic and update their status
        const attachments = document.querySelectorAll(`.miniapp-attachment[data-webxdc-topic="${topic}"]`);

        attachments.forEach(attachment => {
            if (attachment._updateMiniAppStatus) {
                attachment._updateMiniAppStatus(is_active, peer_count, peers);
            }
        });
    });

    // Listen for Mini App crashes (Android renderer process crash)
    _on('miniapp_crashed', () => {
        showToast('Mini App Crashed Unexpectedly');
    });

    // NIP-46 bunker lifecycle. `bunker_state` fires on every connection
    // transition (idle → connecting → online → offline). We surface the
    // Offline case as a toast since signing will fail until reconnect.
    // The Connecting/Online transitions stay silent — they're noise on
    // every relay reconnect.
    // Bunker session listeners (bunker_state, bunker_session_staged,
    // bunker_reauthorize_*, bunker_awaiting_approval, bunker_auth_url) are
    // registered EARLY in the DOMContentLoaded init block — not here — so
    // they catch events fired during the pre-login bunker / reauth flows
    // before setupRustListeners has run.

    await Promise.all(_p);

    // Note: Deep link listener is set up early in DOMContentLoaded, before login flow
    // This ensures deep links work even when the app is opened from a closed state
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
 * Renders the relay list and media servers in the Settings Network section
 */
async function renderRelayList() {
    try {
        const relays = await invoke('get_relays');
        const networkList = document.getElementById('network-list');

        // Clear existing content
        networkList.innerHTML = '';

        // Add Nostr Relays header with info and add buttons
        const relaysTitleContainer = document.createElement('div');
        relaysTitleContainer.className = 'relay-section-header';

        const relaysTitle = document.createElement('h3');
        relaysTitle.className = 'network-section-title';
        relaysTitle.style.display = 'inline-flex';
        relaysTitle.style.alignItems = 'center';
        relaysTitle.textContent = 'Nostr Relays';

        const relaysInfoBtn = document.createElement('span');
        relaysInfoBtn.className = 'icon icon-info btn';
        relaysInfoBtn.style.width = '16px';
        relaysInfoBtn.style.height = '16px';
        relaysInfoBtn.style.position = 'relative';
        relaysInfoBtn.style.display = 'inline-block';
        relaysInfoBtn.style.marginLeft = '8px';
        relaysInfoBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Nostr Relays', 'Nostr Relays are <b>decentralized servers that store and relay your messages</b> across the Nostr network.<br><br>Vector connects to multiple relays simultaneously to ensure your messages are delivered reliably and are censorship-resistant.', true);
        };

        const addRelayBtn = document.createElement('button');
        addRelayBtn.className = 'relay-add-btn';
        addRelayBtn.textContent = '+';
        addRelayBtn.title = 'Add Custom Relay';
        addRelayBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            openAddRelayDialog();
        };

        relaysTitle.appendChild(relaysInfoBtn);
        relaysTitleContainer.appendChild(relaysTitle);
        relaysTitleContainer.appendChild(addRelayBtn);
        networkList.appendChild(relaysTitleContainer);

        // Create relay items
        relays.forEach(relay => {
            const relayItem = document.createElement('div');
            relayItem.className = 'relay-item' + (relay.enabled ? '' : ' disabled');
            relayItem.setAttribute('data-relay-url', relay.url);
            relayItem.setAttribute('data-relay-is-default', relay.is_default);
            relayItem.setAttribute('data-relay-is-custom', relay.is_custom);

            // Content container (clickable area)
            const relayContent = document.createElement('div');
            relayContent.className = 'relay-item-content';
            relayContent.onclick = () => openRelayInfoDialog(relay);

            const relayUrl = document.createElement('span');
            relayUrl.className = 'relay-url';
            relayUrl.textContent = relay.url.replace(/^wss?:\/\//, '');

            // Mode badge (only for custom relays or non-default modes)
            if (relay.is_custom && relay.mode !== 'both') {
                const modeBadge = document.createElement('span');
                modeBadge.className = 'relay-mode-badge';
                modeBadge.textContent = relay.mode === 'read' ? 'R' : 'W';
                relayContent.appendChild(modeBadge);
            }

            // Default badge
            if (relay.is_default) {
                const defaultBadge = document.createElement('span');
                defaultBadge.className = 'relay-default-badge';
                defaultBadge.textContent = 'default';
                relayContent.appendChild(defaultBadge);
            }

            relayContent.appendChild(relayUrl);

            // Status badge
            const relayStatus = document.createElement('span');
            relayStatus.className = `relay-status ${relay.status}`;
            relayStatus.textContent = relay.status;

            // Actions container
            const actionsContainer = document.createElement('div');
            actionsContainer.className = 'relay-item-actions';

            // Toggle switch
            const toggle = document.createElement('input');
            toggle.type = 'checkbox';
            toggle.className = 'relay-toggle';
            toggle.checked = relay.enabled;
            toggle.onclick = (e) => e.stopPropagation();
            toggle.onchange = async (e) => {
                const enabled = e.target.checked;
                try {
                    if (relay.is_default) {
                        // Show warning for default relays
                        if (!enabled) {
                            const confirmed = await popupConfirm(
                                'Disable Default Relay?',
                                'This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?',
                                false
                            );
                            if (!confirmed) {
                                e.target.checked = true;
                                return;
                            }
                        }
                        await invoke('toggle_default_relay', { url: relay.url, enabled });
                    } else {
                        await invoke('toggle_custom_relay', { url: relay.url, enabled });
                    }
                    // Refresh the list
                    renderRelayList();
                } catch (err) {
                    console.error('Failed to toggle relay:', err);
                    e.target.checked = !enabled; // Revert on error
                }
            };

            actionsContainer.appendChild(relayStatus);
            actionsContainer.appendChild(toggle);

            relayItem.appendChild(relayContent);
            relayItem.appendChild(actionsContainer);
            networkList.appendChild(relayItem);
        });
        
        const blossomServers = await invoke('get_blossom_servers_config');

        const mediaTitleContainer = document.createElement('div');
        mediaTitleContainer.className = 'relay-section-header';
        mediaTitleContainer.style.marginTop = '2rem';

        const mediaTitle = document.createElement('h3');
        mediaTitle.className = 'network-section-title';
        mediaTitle.style.display = 'inline-flex';
        mediaTitle.style.alignItems = 'center';
        mediaTitle.textContent = 'Media Servers';

        const mediaInfoBtn = document.createElement('span');
        mediaInfoBtn.className = 'icon icon-info btn';
        mediaInfoBtn.style.width = '16px';
        mediaInfoBtn.style.height = '16px';
        mediaInfoBtn.style.position = 'relative';
        mediaInfoBtn.style.display = 'inline-block';
        mediaInfoBtn.style.marginLeft = '8px';
        mediaInfoBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Media Servers', 'Media Servers are <b>Blossom-compatible servers that store your files</b> (images, videos, documents) for sharing in messages and for storage in an encrypted cloud.<br><br>Your server list syncs automatically across your devices.', true);
        };

        const addMediaBtn = document.createElement('button');
        addMediaBtn.className = 'relay-add-btn';
        addMediaBtn.textContent = '+';
        addMediaBtn.title = 'Add Custom Media Server';
        addMediaBtn.onclick = async (e) => {
            e.preventDefault();
            e.stopPropagation();
            const url = await popupConfirm(
                'Add Media Server',
                'Enter the address of a Blossom-compatible server. A bare domain like <b>blossom.primal.net</b> works. Vector adds <b>https://</b> automatically.',
                false,
                'blossom.primal.net',
            );
            if (!url) return;
            try {
                await addCustomBlossomServer(url.trim());
                renderRelayList();
            } catch (err) {
                popupConfirm('Could not add server', String(err), true, '', 'vector_warning.svg');
            }
        };

        mediaTitle.appendChild(mediaInfoBtn);
        mediaTitleContainer.appendChild(mediaTitle);
        mediaTitleContainer.appendChild(addMediaBtn);
        networkList.appendChild(mediaTitleContainer);

        blossomServers.forEach(server => {
            const serverItem = document.createElement('div');
            serverItem.className = 'relay-item media-server-item' + (server.enabled ? '' : ' disabled');
            serverItem.setAttribute('data-server-url', server.url);

            const serverContent = document.createElement('div');
            serverContent.className = 'relay-item-content';
            serverContent.onclick = () => openBlossomServerInfoDialog(server);

            if (server.is_default) {
                const defaultBadge = document.createElement('span');
                defaultBadge.className = 'relay-default-badge';
                defaultBadge.textContent = 'default';
                serverContent.appendChild(defaultBadge);
            }

            const serverUrlSpan = document.createElement('span');
            serverUrlSpan.className = 'relay-url';
            serverUrlSpan.textContent = server.url.replace(/^https?:\/\//, '');
            serverContent.appendChild(serverUrlSpan);

            const statusBadge = document.createElement('span');
            statusBadge.className = `relay-status ${server.enabled ? 'connected' : 'disabled'}`;
            statusBadge.textContent = server.enabled ? 'active' : 'disabled';

            serverItem.appendChild(serverContent);
            serverItem.appendChild(statusBadge);
            networkList.appendChild(serverItem);
        });
    } catch (error) {
        console.error('Failed to fetch network info:', error);
    }
}

// =============================================================================
// Relay Dialog Management
// =============================================================================

/** Currently selected relay for info dialog */
let currentRelayInfo = null;
/** Interval for refreshing relay info dialog data */
let relayInfoRefreshInterval = null;

/**
 * Opens the Add Relay dialog
 */
function openAddRelayDialog() {
    const overlay = document.getElementById('add-relay-overlay');
    const urlInput = document.getElementById('add-relay-url');
    const modeSelect = document.getElementById('add-relay-mode');

    // Reset form
    urlInput.value = '';
    modeSelect.value = 'both';

    // Show dialog
    overlay.classList.add('active');
    urlInput.focus();
}

/**
 * Closes the Add Relay dialog
 */
function closeAddRelayDialog() {
    const overlay = document.getElementById('add-relay-overlay');
    overlay.classList.remove('active');
}

/**
 * Handles adding a new relay from the dialog
 */
async function handleAddRelay() {
    const urlInput = document.getElementById('add-relay-url');
    const modeSelect = document.getElementById('add-relay-mode');
    let url = urlInput.value.trim();
    const mode = modeSelect.value;

    if (!url) {
        popupConfirm('Invalid URL', 'Please enter a relay URL.', true);
        return;
    }

    // Normalize URL: strip protocol if present and add wss://
    url = url.replace(/^wss?:\/\//i, '');
    url = 'wss://' + url;

    try {
        await invoke('add_custom_relay', { url, mode });
        closeAddRelayDialog();
        renderRelayList();
    } catch (err) {
        popupConfirm('Failed to Add Relay', escapeHtml(err.toString()), true);
    }
}

/**
 * Refreshes the data displayed in the Relay Info dialog
 */
async function refreshRelayInfoDialog() {
    if (!currentRelayInfo) return;

    const url = currentRelayInfo.url;

    // Fetch fresh relay data
    try {
        const relays = await invoke('get_relays');
        const freshRelay = relays.find(r => r.url.toLowerCase() === url.toLowerCase());
        if (freshRelay) {
            currentRelayInfo = freshRelay;

            // Update status
            const statusEl = document.getElementById('relay-info-status');
            statusEl.textContent = freshRelay.status;
            statusEl.className = `relay-status ${freshRelay.status}`;

            // Update disable button text
        const disableBtn = document.getElementById('relay-info-disable');
        if (freshRelay.is_default) {
            disableBtn.innerHTML = freshRelay.enabled 
                ? '<span class="icon icon-disable"></span> Disable'
                : '<span class="icon icon-disable"></span> Enable';
        }
        }
    } catch (err) {
        console.error('Failed to refresh relay data:', err);
    }

    // Refresh metrics
    try {
        const metrics = await invoke('get_relay_metrics', { url });
        const pingEl = document.getElementById('relay-info-ping');
        if (metrics.ping_ms) {
            pingEl.textContent = `${metrics.ping_ms}ms`;
            pingEl.style.color = metrics.ping_ms < 200 ? 'var(--status-excellent)'
                : metrics.ping_ms < 500 ? 'var(--status-good)'
                : metrics.ping_ms < 1000 ? 'var(--status-fair)'
                : 'var(--status-poor)';
        } else {
            pingEl.textContent = '--';
            pingEl.style.color = '';
        }
        if (metrics.last_check) {
            const lastCheck = new Date(metrics.last_check * 1000);
            const now = new Date();
            const diffSecs = Math.floor((now - lastCheck) / 1000);
            let lastCheckText;
            if (diffSecs < 60) {
                lastCheckText = `${diffSecs}s ago`;
            } else if (diffSecs < 3600) {
                lastCheckText = `${Math.floor(diffSecs / 60)}m ago`;
            } else {
                lastCheckText = lastCheck.toLocaleTimeString();
            }
            document.getElementById('relay-info-last-check').textContent = lastCheckText;
        } else {
            document.getElementById('relay-info-last-check').textContent = '--';
        }
    } catch (err) {
        console.error('Failed to load relay metrics:', err);
    }

    // Refresh logs
    try {
        const logs = await invoke('get_relay_logs', { url });
        const logsList = document.getElementById('relay-info-logs');
        logsList.innerHTML = '';

        if (logs.length === 0) {
            const emptyLi = document.createElement('li');
            emptyLi.className = 'relay-log-empty';
            emptyLi.textContent = 'No activity recorded yet';
            logsList.appendChild(emptyLi);
        } else {
            logs.forEach(log => {
                const li = document.createElement('li');
                const time = new Date(log.timestamp * 1000).toLocaleTimeString();
                li.innerHTML = `<span class="relay-log-time">${escapeHtml(time)}</span><span class="relay-log-message ${escapeHtml(log.level)}">${escapeHtml(log.message)}</span>`;
                logsList.appendChild(li);
            });
        }
    } catch (err) {
        console.error('Failed to load relay logs:', err);
    }
}

/**
 * Opens the Relay Info dialog
 * @param {Object} relay - The relay object
 */
async function openRelayInfoDialog(relay) {
    // Clear any existing interval
    if (relayInfoRefreshInterval) {
        clearInterval(relayInfoRefreshInterval);
        relayInfoRefreshInterval = null;
    }

    currentRelayInfo = relay;
    const overlay = document.getElementById('relay-info-overlay');
    const urlEl = document.getElementById('relay-info-url');
    const modeSelect = document.getElementById('relay-info-mode');

    // Set static info (URL doesn't change)
    urlEl.textContent = relay.url.replace(/^wss?:\/\//, '');

    // Set mode (only editable for custom relays)
    modeSelect.value = relay.mode || 'both';
    modeSelect.disabled = relay.is_default;

    // Initial data load
    await refreshRelayInfoDialog();

    // Start refresh interval (every 1 second)
    relayInfoRefreshInterval = setInterval(refreshRelayInfoDialog, 1000);

    // Show dialog
    overlay.classList.add('active');
}

/**
 * Closes the Relay Info dialog
 */
function closeRelayInfoDialog() {
    // Clear the refresh interval
    if (relayInfoRefreshInterval) {
        clearInterval(relayInfoRefreshInterval);
        relayInfoRefreshInterval = null;
    }

    const overlay = document.getElementById('relay-info-overlay');
    overlay.classList.remove('active');
    currentRelayInfo = null;
}

/**
 * Handles mode change from the info dialog
 */
async function handleRelayModeChange() {
    if (!currentRelayInfo || currentRelayInfo.is_default) return;

    const modeSelect = document.getElementById('relay-info-mode');
    const newMode = modeSelect.value;

    try {
        await invoke('update_relay_mode', { url: currentRelayInfo.url, mode: newMode });
        currentRelayInfo.mode = newMode;
        renderRelayList();
    } catch (err) {
        console.error('Failed to update relay mode:', err);
        popupConfirm('Error', 'Failed to update relay mode: ' + err.toString(), true);
    }
}

// =============================================================================
// Blossom Media Server Info Dialog
// =============================================================================

/** Currently-open blossom server (info dialog). */
let currentBlossomInfo = null;

function openBlossomServerInfoDialog(server) {
    currentBlossomInfo = server;
    const overlay = document.getElementById('blossom-info-overlay');
    document.getElementById('blossom-info-url').textContent = server.url.replace(/^https?:\/\//, '');

    const statusEl = document.getElementById('blossom-info-status');
    statusEl.className = `relay-status relay-status-small ${server.enabled ? 'connected' : 'disabled'}`;
    statusEl.textContent = server.enabled ? 'enabled' : 'disabled';

    const actionBtn = document.getElementById('blossom-info-action');
    if (server.is_custom) {
        actionBtn.textContent = 'Remove Server';
    } else {
        actionBtn.textContent = server.enabled ? 'Disable Server' : 'Enable Server';
    }

    overlay.classList.add('active');
    // Reset slot synchronously so stale data doesn't flash mid-fetch.
    const slot = document.getElementById('blossom-info-capabilities');
    if (slot) {
        slot.textContent = 'Loading…';
        slot.style.opacity = '0.6';
    }
    const token = ++_blossomCapsToken;
    renderBlossomCapabilities(server.url, token);
}

/** Monotonic token — rapid open(A) → open(B) races resolve in B's favour. */
let _blossomCapsToken = 0;

/** Chip distinguishing encrypted (chat) from public (avatar/banner) contexts. */
function buildBlossomContextBadge(isEncrypted) {
    const badge = document.createElement('span');
    badge.className = 'blossom-cap-context';
    if (isEncrypted) {
        badge.textContent = 'encrypted';
        badge.title = 'Tested with encrypted chat data';
    } else {
        badge.textContent = 'public';
        badge.title = 'Tested with public uploads (avatar, banner, etc.)';
    }
    return badge;
}

async function renderBlossomCapabilities(url, token) {
    const slot = document.getElementById('blossom-info-capabilities');
    if (!slot) return;
    try {
        const caps = await getBlossomServerCapabilities(url);
        if (token !== _blossomCapsToken) return;
        if (!caps || caps.length === 0) {
            slot.textContent = 'No capability data yet. Vector learns each server’s file-type and size limits as you send files.';
            slot.style.opacity = '0.6';
            return;
        }
        // outcome 1 = accepted, 2 = MIME rejected, 3 = size-only seed.
        const accepted = caps.filter(c => c.outcome === 1 && c.max_accepted_size > 0);
        const sizeLimited = caps.filter(c =>
            (c.outcome === 3 && c.min_rejected_size != null) ||
            (c.outcome === 1 && c.max_accepted_size === 0 && c.min_rejected_size != null)
        );
        const rejected = caps.filter(c => c.outcome === 2);
        slot.innerHTML = '';
        slot.style.opacity = '';

        if (accepted.length) {
            const h = document.createElement('div');
            h.className = 'blossom-cap-group-label blossom-cap-accepted';
            h.textContent = 'Accepts';
            slot.appendChild(h);
            const ul = document.createElement('ul');
            ul.className = 'blossom-cap-list';
            for (const c of accepted) {
                const li = document.createElement('li');
                const marker = document.createElement('span');
                marker.className = 'blossom-cap-marker blossom-cap-accepted';
                marker.textContent = '✓';
                marker.setAttribute('aria-label', 'accepted');
                const mime = document.createElement('span');
                mime.className = 'blossom-cap-mime';
                mime.textContent = c.mime_type;
                li.appendChild(marker);
                li.appendChild(mime);
                li.appendChild(buildBlossomContextBadge(c.is_encrypted));
                const size = document.createElement('span');
                size.className = 'blossom-cap-size';
                size.textContent = `${formatBytes(c.max_accepted_size, 1)} max`;
                li.appendChild(size);
                ul.appendChild(li);
            }
            slot.appendChild(ul);
        }
        if (sizeLimited.length) {
            const h = document.createElement('div');
            h.className = 'blossom-cap-group-label blossom-cap-limited';
            h.textContent = 'Size-limited';
            slot.appendChild(h);
            const ul = document.createElement('ul');
            ul.className = 'blossom-cap-list';
            for (const c of sizeLimited) {
                const li = document.createElement('li');
                li.classList.add('blossom-cap-limited-row');
                const marker = document.createElement('span');
                marker.className = 'blossom-cap-marker blossom-cap-limited';
                marker.textContent = '⚠';
                marker.setAttribute('aria-label', 'size limited');
                const mime = document.createElement('span');
                mime.className = 'blossom-cap-mime';
                mime.textContent = c.mime_type;
                const size = document.createElement('span');
                size.className = 'blossom-cap-size';
                size.textContent = `rejects ≥ ${formatBytes(c.min_rejected_size, 1)}`;
                li.appendChild(marker);
                li.appendChild(mime);
                li.appendChild(buildBlossomContextBadge(c.is_encrypted));
                li.appendChild(size);
                ul.appendChild(li);
            }
            slot.appendChild(ul);
        }
        if (rejected.length) {
            const h = document.createElement('div');
            h.className = 'blossom-cap-group-label blossom-cap-rejected';
            h.textContent = 'Rejects';
            slot.appendChild(h);
            const ul = document.createElement('ul');
            ul.className = 'blossom-cap-list';
            for (const c of rejected) {
                const li = document.createElement('li');
                li.classList.add('blossom-cap-rejected-row');
                const mime = document.createElement('span');
                mime.className = 'blossom-cap-mime';
                mime.textContent = c.mime_type;
                const marker = document.createElement('span');
                marker.className = 'blossom-cap-marker blossom-cap-rejected';
                marker.textContent = '✕';
                marker.setAttribute('aria-label', 'rejected');
                li.appendChild(mime);
                li.appendChild(buildBlossomContextBadge(c.is_encrypted));
                li.appendChild(marker);
                ul.appendChild(li);
            }
            slot.appendChild(ul);
        }
    } catch (err) {
        console.error('Failed to load blossom capabilities:', err);
        if (token !== _blossomCapsToken) return;
        slot.textContent = 'Could not load capability data.';
        slot.style.opacity = '0.6';
    }
}

function closeBlossomServerInfoDialog() {
    document.getElementById('blossom-info-overlay').classList.remove('active');
    currentBlossomInfo = null;
}

async function handleBlossomAction() {
    if (!currentBlossomInfo) return;
    const server = currentBlossomInfo;
    try {
        if (server.is_custom) {
            const ok = await popupConfirm(
                'Remove media server?',
                `<b>${server.url}</b> will be removed from your list. Existing uploads on that server remain accessible.`,
                false,
            );
            if (!ok) return;
            await removeCustomBlossomServer(server.url);
            closeBlossomServerInfoDialog();
            renderRelayList();
        } else {
            const newEnabled = !server.enabled;
            await toggleDefaultBlossomServer(server.url, newEnabled);
            currentBlossomInfo = { ...server, enabled: newEnabled };
            openBlossomServerInfoDialog(currentBlossomInfo);
            renderRelayList();
        }
    } catch (err) {
        popupConfirm('Error', String(err), true, '', 'vector_warning.svg');
    }
}

/**
 * Handles disable/remove button from info dialog
 */
async function handleRelayDisable() {
    if (!currentRelayInfo) return;

    const relay = currentRelayInfo;

    if (relay.is_default) {
        // Toggle default relay
        const newEnabled = !relay.enabled;
        if (!newEnabled) {
            // Show warning before disabling default relay
            const confirmed = await popupConfirm(
                'Disable Default Relay?',
                'This is a <b>default relay</b>. Disabling it may affect message delivery and sync reliability.<br><br>Are you sure you want to disable it?',
                false
            );
            if (confirmed) {
                try {
                    await invoke('toggle_default_relay', { url: relay.url, enabled: false });
                    closeRelayInfoDialog();
                    renderRelayList();
                } catch (err) {
                    popupConfirm('Error', 'Failed to disable relay: ' + err.toString(), true);
                }
            }
        } else {
            // Re-enable without warning
            try {
                await invoke('toggle_default_relay', { url: relay.url, enabled: true });
                closeRelayInfoDialog();
                renderRelayList();
            } catch (err) {
                popupConfirm('Error', 'Failed to enable relay: ' + err.toString(), true);
            }
        }
    } else {
        // Remove custom relay
        const confirmed = await popupConfirm(
            'Remove Relay?',
            `Are you sure you want to remove <b>${relay.url.replace(/^wss?:\/\//, '')}</b>?`,
            false
        );
        if (confirmed) {
            try {
                await invoke('remove_custom_relay', { url: relay.url });
                closeRelayInfoDialog();
                renderRelayList();
            } catch (err) {
                popupConfirm('Error', 'Failed to remove relay: ' + err.toString(), true);
            }
        }
    }
}

/**
 * Initialize relay dialog event listeners
 */
function initRelayDialogs() {
    // Add Relay Dialog
    document.getElementById('add-relay-close').onclick = closeAddRelayDialog;
    document.getElementById('add-relay-cancel').onclick = closeAddRelayDialog;
    document.getElementById('add-relay-confirm').onclick = handleAddRelay;
    document.getElementById('add-relay-overlay').onclick = (e) => {
        if (e.target.id === 'add-relay-overlay') closeAddRelayDialog();
    };

    // Allow Enter key to submit
    document.getElementById('add-relay-url').onkeydown = (e) => {
        if (e.key === 'Enter') handleAddRelay();
    };

    // Relay Info Dialog
    document.getElementById('relay-info-close').onclick = closeRelayInfoDialog;
    document.getElementById('relay-info-done').onclick = closeRelayInfoDialog;
    document.getElementById('relay-info-disable').onclick = handleRelayDisable;
    document.getElementById('relay-info-mode').onchange = handleRelayModeChange;
    document.getElementById('relay-info-overlay').onclick = (e) => {
        if (e.target.id === 'relay-info-overlay') closeRelayInfoDialog();
    };

    // Copy logs button
    document.getElementById('relay-logs-copy').onclick = copyRelayLogs;

    // Blossom server info dialog
    document.getElementById('blossom-info-close').onclick = closeBlossomServerInfoDialog;
    document.getElementById('blossom-info-done').onclick = closeBlossomServerInfoDialog;
    document.getElementById('blossom-info-action').onclick = handleBlossomAction;
    document.getElementById('blossom-info-overlay').onclick = (e) => {
        if (e.target.id === 'blossom-info-overlay') closeBlossomServerInfoDialog();
    };
}

/**
 * Copies relay logs to clipboard in a formatted way
 */
function copyRelayLogs() {
    if (!currentRelayInfo) return;

    // Read logs from the displayed DOM to avoid async clipboard permission issues
    const logsList = document.getElementById('relay-info-logs');
    const logItems = logsList.querySelectorAll('li:not(.relay-log-empty)');

    let text;
    if (logItems.length === 0) {
        text = 'No activity recorded yet';
    } else {
        const header = `Relay Logs: ${currentRelayInfo.url.replace(/^wss?:\/\//, '')}\n${'='.repeat(50)}\n`;
        const logs = Array.from(logItems).map(li => {
            const time = li.querySelector('.relay-log-time')?.textContent || '';
            const msg = li.querySelector('.relay-log-message')?.textContent || '';
            const level = li.querySelector('.relay-log-message')?.classList.contains('error') ? 'ERROR' :
                          li.querySelector('.relay-log-message')?.classList.contains('warn') ? 'WARN' : 'INFO';
            return `[${time}] [${level}] ${msg}`;
        }).join('\n');
        text = header + logs;
    }

    navigator.clipboard.writeText(text).then(() => {
        // Visual feedback - change icon briefly
        const copyBtn = document.getElementById('relay-logs-copy');
        const icon = copyBtn.querySelector('.icon');
        icon.classList.remove('icon-copy');
        icon.classList.add('icon-check');
        setTimeout(() => {
            icon.classList.remove('icon-check');
            icon.classList.add('icon-copy');
        }, 1500);
    }).catch(err => {
        console.error('Failed to copy relay logs:', err);
    });
}

// =============================================================================

/**
 * Login to the Nostr network
 * @param {boolean} skipAnimations - Skip intro animations (for instant login without PIN)
 */
async function login(skipAnimations = false) {
    if (strPubkey) {
        // Successful end of the Add Profile flow — drop the back-target
        // and reset flags so the next session starts clean.
        addAccountFlow.finish();
        // Fire connect + all listener registrations in parallel (no sequential IPC waits)
        console.time('[Boot] connect + listeners');
        const _connectP = invoke("connect");
        const _listenersP = setupRustListeners();

        // Setup unified progress operation event listener
        const _progressP = listen('progress_operation', (evt) => {
            const { type, current, total, message } = evt.payload;
            
            switch (type) {
                case 'start':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.add('typing-indicator-text');
                    domLoginEncryptTitle.style.color = '';
                    break;
                    
                case 'progress':
                    if (current && total) {
                        const progress = Math.round((current / total) * 100);
                        domLoginEncryptTitle.textContent = `${message} (${progress}%)`;
                    } else {
                        domLoginEncryptTitle.textContent = message;
                    }
                    break;
                    
                case 'complete':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('typing-indicator-text');
                    break;
                    
                case 'error':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('typing-indicator-text');
                    domLoginEncryptTitle.style.color = 'red';
                    break;
            }
        });


        // Setup a Rust Listener for the backend's init finish
        // (helper hoisted above this block — see runWithTorBootstrapStatus)
        const _initFinishedP = listen('init_finished', async (evt) => {
            console.timeEnd('[Boot] login() total');
            console.time('[Boot] init_finished handler');
            // The backend now sends both profiles (without messages) and chats (with messages)
            arrProfiles = evt.payload.profiles || [];
            arrChats = evt.payload.chats || [];

            // Resolve Community logos in the background (Community metadata rides the
            // chat payload; only the encrypted logo needs a lazy cache step).
            resolveCommunityAvatars();

            // Warm the active theme's emoji pack in the background so it's pinned
            // instantly on the first picker open (it's runtime-injected, not in
            // the subscription DB, so it would otherwise fetch on open). Also
            // registers it with the send resolver, so `:shortcode:` posts as a
            // real emoji even before the picker is ever opened. The fetch's own
            // timeout absorbs relay-connect latency, so firing it here is safe.
            const _themeNaddr = THEME_EMOJI_PACKS[_currentThemeName()];
            if (_themeNaddr) {
                _fetchThemePack(_themeNaddr)
                    .then(() => _registerThemeEmoji(_cachedThemePack()))
                    .catch(() => {});
            }

            // Helper to show the main UI after login
            const showMainUI = async () => {
                console.time('[Boot] showMainUI:dom');
                domLoginInput.value = "";
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';

                // Show navbar and bookmarks
                domNavbar.style.display = '';
                domChatBookmarksBtn.style.display = 'flex';

                // Land on the Chat tab. Without an explicit reset here, the
                // visibility of the main panels is whatever they were before
                // login fired — which is fine for fresh boots (chats panel
                // is visible by default) but not for the Add Profile flow,
                // which hid every panel and never re-showed them. Always
                // resetting to Chat gives one consistent landing point for
                // new accounts, imported accounts, and normal logins alike.
                domChats.style.display = '';
                domChat.style.display = 'none';
                domProfile.style.display = 'none';
                domSettings.style.display = 'none';
                domInvites.style.display = 'none';
                if (typeof domGroupOverview !== 'undefined') {
                    domGroupOverview.style.display = 'none';
                }
                navbarSelect('chat-btn');

                // Render our profile
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = '';
                console.timeEnd('[Boot] showMainUI:dom');

                // Refresh our own profile from the network
                if (cProfile?.id) {
                    invoke("queue_profile_sync", {
                        npub: cProfile.id,
                        priority: "critical",
                        forceRefresh: true
                    });
                }

                // Finished boot!
                fInit = false;

                // Render the chatlist
                console.time('[Boot] showMainUI:renderChatlist');
                renderChatlist();
                console.timeEnd('[Boot] showMainUI:renderChatlist');

                // Show the New Chat buttons
                if (domChatNewDM) {
                    domChatNewDM.style.display = '';
                    domChatNewDM.onclick = openNewChat;
                }
                if (domChatNewGroup) {
                    domChatNewGroup.style.display = '';
                    domChatNewGroup.onclick = openCreateGroup;
                }

                // Adjust the Chat List sizes (deferred — layout reflows don't block first paint)
                requestAnimationFrame(() => adjustSize());

                // Prompt for background service / battery optimization (mobile only, once)
                // Deferred so login animations finish first
                if (platformFeatures.is_mobile) {
                    setTimeout(async () => {
                        try {
                            const prompted = await invoke('get_background_service_prompted');
                            console.log('[Battery] prompted =', prompted);
                            if (!prompted) {
                                await invoke('set_background_service_prompted');
                                await showBackgroundServicePrompt();
                            }
                        } catch (e) {
                            console.error('[Battery] prompt error:', e);
                        }
                    }, 1500);
                }
            };

            if (skipAnimations) {
                console.time('[Boot] showMainUI');
                await showMainUI();
                console.timeEnd('[Boot] showMainUI');
                console.timeEnd('[Boot] init_finished handler');
                console.log('[Boot] UI visible - instant login complete');

                // Apply the same intro animations as the encryption flow
                domChatBookmarksBtn.style.opacity = '0';
                domNavbar.classList.add('fadein-anim');
                domNavbar.addEventListener('animationend', () => {
                    domNavbar.classList.remove('fadein-anim');
                    domChatBookmarksBtn.style.opacity = '';
                    domChatBookmarksBtn.classList.add('fadein-anim');
                    domChatBookmarksBtn.addEventListener('animationend', () => domChatBookmarksBtn.classList.remove('fadein-anim'), { once: true });
                }, { once: true });

                domAccount.classList.add('fadein-anim');
                domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                domChatList.classList.add('intro-anim');
                domChatList.addEventListener('animationend', () => domChatList.classList.remove('intro-anim'), { once: true });

                if (domChatNewDM) {
                    domChatNewDM.classList.add('intro-anim');
                    domChatNewDM.addEventListener('animationend', () => domChatNewDM.classList.remove('intro-anim'), { once: true });
                }
                if (domChatNewGroup) {
                    domChatNewGroup.classList.add('intro-anim');
                    domChatNewGroup.addEventListener('animationend', () => domChatNewGroup.classList.remove('intro-anim'), { once: true });
                }
            } else {
                // Fadeout the login and encryption UI with animation
                domLogin.classList.add('fadeout-anim');
                domLogin.addEventListener('animationend', async () => {
                    domLogin.classList.remove('fadeout-anim');
                    await showMainUI();

                    // Add fade-in animations
                    domChatBookmarksBtn.style.opacity = '0';
                    domNavbar.classList.add('fadein-anim');
                    domNavbar.addEventListener('animationend', () => {
                        domNavbar.classList.remove('fadein-anim');
                        domChatBookmarksBtn.style.opacity = '';
                        domChatBookmarksBtn.classList.add('fadein-anim');
                        domChatBookmarksBtn.addEventListener('animationend', () => domChatBookmarksBtn.classList.remove('fadein-anim'), { once: true });
                    }, { once: true });

                    domAccount.classList.add('fadein-anim');
                    domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                    domChatList.classList.add('intro-anim');
                    domChatList.addEventListener('animationend', () => domChatList.classList.remove('intro-anim'), { once: true });

                    if (domChatNewDM) {
                        domChatNewDM.classList.add('intro-anim');
                        domChatNewDM.addEventListener('animationend', () => domChatNewDM.classList.remove('intro-anim'), { once: true });
                    }
                    if (domChatNewGroup) {
                        domChatNewGroup.classList.add('intro-anim');
                        domChatNewGroup.addEventListener('animationend', () => domChatNewGroup.classList.remove('intro-anim'), { once: true });
                    }
                }, { once: true });
            }

            // Setup a subscription for new websocket messages (runs in both animation modes)
            invoke("notifs");

            // Apply badge-gated limits from the cached flag (a prior session's
            // result), so perks are live before this session's post-sync refresh.
            invoke("get_my_badges").then(b => {
                _myBadges = b;
                if (typeof applyBadgeLimits === 'function') applyBadgeLimits(!!b?.vector);
            }).catch(() => {});

            // Setup our Unread Counters
            await invoke("update_unread_counter");

            // Monitor relay connections
            invoke("monitor_relay_connections");

            // Render the initial relay list
            renderRelayList();

            // Initialize the updater
            initializeUpdater();

            // Re-initialize encryption settings now that login is complete,
            // so the toggle reflects the actual backend state.
            initEncryptionSettings();

            // Execute any pending deep link action that was received before login
            try {
                const pendingAction = await invoke('get_pending_deep_link');
                if (pendingAction) {
                    console.log('Executing pending deep link action:', pendingAction);
                    await executeDeepLinkAction(pendingAction);
                }
            } catch (e) {
                console.error('Failed to check for pending deep link:', e);
            }

            // Handle a share (file/text from another app) that arrived on a cold
            // start before the live listener was attached.
            try {
                const pendingShare = await invoke('get_pending_share');
                if (pendingShare) await handleIncomingShare(pendingShare);
            } catch (e) {
                console.error('Failed to check for pending share:', e);
            }
        });

        // Wait for connect + all listener registrations to complete
        await Promise.all([_connectP, _listenersP, _progressP, _initFinishedP]);
        console.timeEnd('[Boot] connect + listeners');

        // Load and Decrypt our database; fetching the full chat state from disk for immediate bootup
        domLoginEncryptTitle.textContent = `Decrypting Database...`;

        // Note: this also begins the Rust backend's iterative sync, thus, init should ONLY be called once, to initiate it
        init(true);
    }
}

/**
 * Renders the user's own profile UI in the chat list
 * @param {Profile} cProfile 
 */
function renderCurrentProfile(cProfile) {
    /* Chatlist Tab */

    // Clear and render avatar
    domAccountAvatarContainer.innerHTML = '';
    const accountAvatarSrc = getProfileAvatarSrc(cProfile);
    const domAvatar = createAvatarImg(accountAvatarSrc, 22, false);
    domAvatar.classList.add('btn');
    domAvatar.onclick = () => openProfile();
    domAccountAvatarContainer.appendChild(domAvatar);

    // Render our Display Name
    domAccountName.textContent = cProfile?.nickname || cProfile?.name || strPubkey.substring(0, 10) + '…';
    domAccountName.onclick = () => openProfile();
    if (cProfile?.nickname || cProfile?.name) twemojify(domAccountName);

    // Render our status
    domAccountStatus.textContent = cProfile?.status?.title || 'Set a Status';
    domAccountStatus.onclick = askForStatus;
    twemojify(domAccountStatus);

}

/**
 * Render the Profile tab based on a given profile
 * @param {Profile} cProfile 
 */
function renderProfileTab(cProfile) {
    // Header Mini Avatar
    domProfileHeaderAvatarContainer.innerHTML = '';
    const headerAvatarSrc = getProfileAvatarSrc(cProfile);
    const domHeaderAvatar = createAvatarImg(headerAvatarSrc, 22, false);
    domHeaderAvatar.classList.add('btn');
    domProfileHeaderAvatarContainer.appendChild(domHeaderAvatar);

    // Header title: "My Profile ⌄" switcher when viewing our own profile,
    // otherwise the contact's display name. The switcher opens the multi-
    // account dropdown; the name is non-interactive.
    const domSwitcher = document.getElementById('my-profile-switcher');
    if (cProfile?.mine) {
        domProfileName.style.display = 'none';
        if (domSwitcher) domSwitcher.style.display = '';
    } else {
        if (domSwitcher) domSwitcher.style.display = 'none';
        domProfileName.style.display = '';
        domProfileName.innerHTML = cProfile?.nickname || cProfile?.name || (cProfile?.id ? cProfile.id.substring(0, 10) + '…' : 'Unknown');
        if (cProfile?.nickname || cProfile?.name) twemojify(domProfileName);
    }

    // Status
    const strStatusPlaceholder = cProfile.mine ? 'Set a Status' : '';
    domProfileStatus.innerHTML = cProfile?.status?.title || strStatusPlaceholder;
    if (cProfile?.status?.title) twemojify(domProfileStatus);

    // Adjust our Profile Name class to manage space according to Status visibility
    domProfileName.classList.toggle('chat-contact', !domProfileStatus.textContent);
    domProfileName.classList.toggle('chat-contact-with-status', !!domProfileStatus.textContent);

    // Banner - keep original structure but add click handler
    const bannerSrc = getProfileBannerSrc(cProfile);
    if (bannerSrc) {
        if (domProfileBanner.tagName === 'DIV') {
            const newBanner = document.createElement('img');
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
        domProfileBanner.src = bannerSrc;
        // On error, replace with solid color placeholder
        domProfileBanner.onerror = function() {
            const placeholder = document.createElement('div');
            placeholder.style.backgroundColor = 'rgb(27, 27, 27)';
            placeholder.classList.add('profile-banner');
            if (cProfile.mine) {
                placeholder.classList.add('btn');
                placeholder.onclick = null;
            }
            domProfileBanner.replaceWith(placeholder);
            domProfileBanner = placeholder;
        };
    } else {
        if (domProfileBanner.tagName === 'IMG') {
            const newBanner = document.createElement('div');
            newBanner.style.backgroundColor = 'rgb(27, 27, 27)';
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
    }
    domProfileBanner.classList.add('profile-banner');
    domProfileBanner.onclick = null;

    // Avatar - keep original structure but add click handler
    const profileAvatarSrc = getProfileAvatarSrc(cProfile);
    if (profileAvatarSrc) {
        if (domProfileAvatar.tagName === 'DIV') {
            const newAvatar = document.createElement('img');
            domProfileAvatar.replaceWith(newAvatar);
            domProfileAvatar = newAvatar;
        }
        domProfileAvatar.src = profileAvatarSrc;
        // On error, replace with placeholder
        domProfileAvatar.onerror = function() {
            const placeholder = createPlaceholderAvatar(false, 175);
            placeholder.classList.add('profile-avatar');
            domProfileAvatar.replaceWith(placeholder);
            domProfileAvatar = placeholder;
        };
    } else {
        const newAvatar = createPlaceholderAvatar(false, 175);
        domProfileAvatar.replaceWith(newAvatar);
        domProfileAvatar = newAvatar;
    }
    domProfileAvatar.classList.add('profile-avatar');
    domProfileAvatar.onclick = null;

    // Secondary Display Name - use profile's npub as fallback
    const strNamePlaceholder = cProfile.mine ? 'Set a Display Name' : (cProfile?.id ? cProfile.id.substring(0, 10) + '…' : '');
    domProfileNameSecondary.innerHTML = cProfile?.nickname || cProfile?.name || strNamePlaceholder;
    if (cProfile?.nickname || cProfile?.name) twemojify(domProfileNameSecondary);

    // Secondary Status
    domProfileStatusSecondary.innerHTML = domProfileStatus.innerHTML;

    // Badges
    domProfileBadgeInvite.style.display = 'none';
    invoke("get_invited_users", { npub: cProfile.id }).then(count => {
        if (count > 0) {
            domProfileBadgeInvite.style.display = '';
            domProfileBadgeInvite.onclick = () => {
                popupConfirm('Vector Beta Inviter', `Acquired by inviting <b>${count} ${count === 1 ? 'user' : 'users'}</b> to the Vector Beta!`, true, '', 'vector_badge_placeholder.svg');
            }
        }
    }).catch(e => {});

    // Guy Fawkes Day Badge (5th November 2025 - Vector v0.2 Open Beta)
    domProfileBadgeFawkes.style.display = 'none';
    const fawkesNpub = cProfile.id;
    resolveFawkesBadge(fawkesNpub, !!cProfile.mine).then(hasBadge => {
        // Guard against the user having navigated to a different profile while
        // the (first-time, uncached) lookup was in flight.
        if (hasBadge && domProfileId.textContent === fawkesNpub) {
            domProfileBadgeFawkes.style.display = '';
            domProfileBadgeFawkes.onclick = () => {
                popupConfirm('V for Vector Badge', `Acquired by logging in on Guy Fawkes Day&nbsp;(November 5, 2025).<br><br><i style="opacity: 0.5; font-size: 13px;">Remember, remember the 5th of November...</i>`, true, '', 'fawkes_mask.svg');
            };
        }
    }).catch(e => {});

    // npub display
    const profileNpub = document.getElementById('profile-npub');
    if (profileNpub) {
        profileNpub.dataset.fullNpub = cProfile.id;
        profileNpub.textContent = cProfile.id.slice(0, 16) + '...' + cProfile.id.slice(-16);
        document.getElementById('profile-npub-label').textContent = cProfile.mine ? 'My nPub Key' : 'nPub Key';
    }

    // Description
    const strDescriptionPlaceholder = cProfile.mine ? (cProfile?.about || 'Set an About Me') : '';
    domProfileDescription.textContent = cProfile?.about || strDescriptionPlaceholder;
    twemojify(domProfileDescription);

    // npub
    domProfileId.textContent = cProfile.id;

    // Add npub copy functionality
    document.getElementById('profile-npub-copy').onclick = (e) => {
        const npub = document.getElementById('profile-npub')?.dataset.fullNpub;
        if (npub) {
            // Copy the full profile URL for easy sharing
            navigator.clipboard.writeText(npub).then(() => {
                showToast('Copied Profile Link');
            }).catch(() => {
                showToast('Failed to Copy');
                const copyBtn = e.target.closest('#profile-npub-copy');
                if (copyBtn) {
                    copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                    setTimeout(() => {
                        copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
                    }, 2000);
                }
            });
        }
    };

    // If this is OUR profile: make the elements clickable, hide the "Contact Options"
    if (cProfile.mine) {
        document.getElementById('profile').classList.add('is-own-profile');
        // Hide Contact Options
        domProfileOptions.style.display = 'none';

        // Show edit buttons and set their click handlers
        
        document.querySelector('.profile-banner-edit').style.display = 'flex';
        document.querySelector('.profile-banner-edit').onclick = enterProfileEditMode;
        domProfileEditCancelBtn.onclick = () => exitProfileEditMode(true);
        domProfileEditSaveBtn.onclick = () => exitProfileEditMode(false);

        // Show Share button on own profile (top-right of banner)
        const ownShareBtn = document.getElementById('profile-share-btn');
        ownShareBtn.style.display = 'block';
        ownShareBtn.onclick = () => {
            const npub = document.getElementById('profile-npub')?.dataset.fullNpub;
            if (npub) {
                const profileUrl = `https://vectorapp.io/profile/${npub}`;
                navigator.clipboard.writeText(profileUrl).then(() => {
                    const icon = ownShareBtn.querySelector('span');
                    showToast('Profile Link Copied');
                    icon.classList.replace('icon-share', 'icon-check');
                    setTimeout(() => icon.classList.replace('icon-check', 'icon-share'), 2000);
                }).catch(() => {
                    showToast('Failed to Copy Profile Link');
                });
            }
        };
        
        // Hide the 'Back' button and deregister its clickable function
        domProfileBackBtn.style.display = 'none';
        domProfileBackBtn.onclick = null;

        // Force banner on profile edit screen
        domProfileBanner.backgroundColor = 'rgb(27, 27, 27)';
        domProfileBanner.height = '';
        
        // Display the Navbar
        domNavbar.style.display = '';
        document.getElementById('profile-header-avatar-container').style.display = 'none';
        document.getElementById('profile-name').textContent = 'My Profile';
        document.getElementById('profile-status').style.display = 'none';

        // Configure other clickables
        domProfileName.classList.add('btn');
        domProfileStatus.classList.add('btn');
        domProfileName.onclick = () => { if (fProfileEditMode) askForUsername(); };
        domProfileStatus.onclick = () => { if (fProfileEditMode) askForStatus(); };
        domProfileNameSecondary.onclick = () => { if (fProfileEditMode) askForUsername(); };
        domProfileStatusSecondary.onclick = () => { if (fProfileEditMode) askForStatus(); };
        domProfileDescription.onclick = () => { if (fProfileEditMode) editProfileDescription(); };
        domProfileDescription.classList.add('btn');
    } else {
        document.getElementById('profile').classList.remove('is-own-profile');
        // Show Contact Options
        domProfileOptions.style.display = '';
        document.getElementById('profile-header-avatar-container').style.display = '';
        document.getElementById('profile-status').style.display = '';

        // Setup Mute option
        const cMuteChat = arrChats.find(c => c.id === cProfile.id);
        const isMuted = cMuteChat ? cMuteChat.muted : false;
        domProfileOptionMute.querySelector('span').classList.replace('icon-volume-' + (isMuted ? 'max' : 'mute'), 'icon-volume-' + (isMuted ? 'mute' : 'max'));
        domProfileOptionMute.querySelector('p').innerText = isMuted ? 'Unmute' : 'Mute';
        domProfileOptionMute.onclick = () => invoke('toggle_chat_mute', { chatId: cProfile.id });

        // Setup Message option
        domProfileOptionMessage.onclick = () => openChat(cProfile.id);

        // Setup Share option
        domProfileOptionShare.onclick = () => {
            const npub = document.getElementById('profile-npub')?.dataset.fullNpub;
            if (npub) {
                const profileUrl = `https://vectorapp.io/profile/${npub}`;
                navigator.clipboard.writeText(profileUrl).then(() => {
                    // Brief visual feedback
                    const icon = domProfileOptionShare.querySelector('span');
                    showToast('Profile Link Copied');
                    icon.classList.replace('icon-share', 'icon-check');
                    setTimeout(() => icon.classList.replace('icon-check', 'icon-share'), 2000);
                    }).catch(() => {
                    showToast('Failed to Copy Profile Link');
                });
            }
        };

        // Setup Block option (inside More dropdown)
        const isBlocked = cProfile.is_blocked || false;
        const blockIcon = domProfileOptionBlock.querySelector('.icon');
        const blockLabel = domProfileOptionBlock.querySelector('span:first-child');
        domProfileOptionBlock.classList.add('is-danger');
        if (blockLabel) {
            blockLabel.textContent = isBlocked ? 'Unblock' : 'Block';
        }
        domProfileOptionBlock.onclick = async () => {
            domProfileMoreDropdown.style.display = 'none';
            if (isBlocked) {
                await invoke('unblock_user', { npub: cProfile.id });
                showToast('User Unblocked');
                renderChatlist();
            } else {
                const confirmed = await popupConfirm('Block User', 'Are you sure you want to block this user? You will no longer receive DMs from them.', false, '', 'vector_warning.svg');
                if (!confirmed) return;
                await invoke('block_user', { npub: cProfile.id });
                showToast('User Blocked');
                renderChatlist();
            }
        };

        // Setup Nickname option (inside More dropdown)
        domProfileOptionNickname.onclick = async () => {
            domProfileMoreDropdown.style.display = 'none';
            const nick = await popupConfirm('Choose a Nickname', '', false, 'Nickname');
            if (nick === false) return;
            if (nick.length >= 30) return popupConfirm('Woah woah!', 'A ' + nick.length + '-character nickname seems excessive!', true, '', 'vector_warning.svg');
            await invoke('set_nickname', { npub: cProfile.id, nickname: nick });
        };

        // Setup More dropdown toggle
        domProfileMoreDropdown.style.display = 'none';
        domProfileOptionMore.onclick = (e) => {
            e.stopPropagation();
            const isOpen = domProfileMoreDropdown.style.display !== 'none';
            domProfileMoreDropdown.style.display = isOpen ? 'none' : 'block';
            domProfileOptionMore.classList.toggle('active', !isOpen);
        };

        // Hide edit buttons and own-profile share
        document.querySelector('.profile-banner-edit').style.display = 'none';
        document.getElementById('profile-share-btn').style.display = 'none';
        
        // Remove click handlers from avatar and banner
        domProfileAvatar.onclick = null;
        domProfileAvatar.classList.remove('btn');
        domProfileBanner.onclick = null;
        domProfileBanner.classList.remove('btn');
        if (!cProfile.banner) {
            domProfileBanner.style.backgroundColor = '';
            domProfileBanner.style.height = '115px';
        } else {
            domProfileBanner.style.backgroundColor = 'rgb(27, 27, 27)';
            domProfileBanner.style.height = '';
        }
        
        // Show the 'Back' button and link it to the profile's chat
        domProfileBackBtn.style.display = '';
        domProfileBackBtn.onclick = () => {
            // If we came from a chat (especially a group chat), return to it
            if (previousChatBeforeProfile) {
                const chatToOpen = previousChatBeforeProfile;
                previousChatBeforeProfile = ''; // Clear before opening to avoid loops
                openChat(chatToOpen);
            } else {
                // Default to opening DM with this user
                openChat(cProfile.id);
            }
        };
        
        // Hide the Navbar
        domNavbar.style.display = 'none';

        // Remove other clickables
        domProfileName.onclick = null;
        domProfileName.classList.remove('btn');
        domProfileStatus.onclick = null;
        domProfileStatus.classList.remove('btn');
        domProfileNameSecondary.onclick = null;
        domProfileNameSecondary.classList.remove('btn');
        domProfileStatusSecondary.onclick = null;
        domProfileStatusSecondary.classList.remove('btn');
        domProfileDescription.onclick = null;
        domProfileDescription.classList.remove('btn');
    }
}

/**
 * Display the Invite code input flow.
 */
function openInviteFlow() {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = '';
    
    // Focus on the invite input
    domInviteInput.focus();
    
    // Handle invite code submission
    domInviteBtn.onclick = async () => {
        const inviteCode = domInviteInput.value.trim();
        if (!inviteCode) {
            return popupConfirm('Please enter an invite code', '', true, '', 'vector_warning.svg');
        }
        
        try {
            // Accept the invite code
            await invoke('accept_invite_code', { inviteCode });
            
            // Hide invite screen and show welcome screen
            domLoginInvite.style.display = 'none';
            showWelcomeScreen();
        } catch (e) {
            // Display the specific error from the backend
            const errorMessage = e.toString() || 'Please check your invite code and try again.';
            popupConfirm('Invalid invite code', errorMessage, true, '', 'vector_warning.svg');
        }
    };
    
    // Handle enter key on invite input
    domInviteInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' || evt.code === 'NumpadEnter') {
            evt.preventDefault();
            domInviteBtn.click();
        }
    };
    
    // Handle enter key on invite input
    domInviteInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' || evt.code === 'NumpadEnter') {
            evt.preventDefault();
            domInviteBtn.click();
        }
    };
}

/**
 * Display the welcome screen after successful invite code acceptance
 */
function showWelcomeScreen() {
    // Hide the logo and subtext
    const domLogo = document.querySelector('.login-logo');
    const domSubtext = document.querySelector('.login-subtext');
    domLogo.style.display = 'none';
    domSubtext.style.display = 'none';

    // Show the welcome screen
    domLoginWelcome.style.display = '';

    // After 5 seconds, transition to the encryption flow
    setTimeout(() => {
        domLoginWelcome.style.display = 'none';
        // Restore the logo and subtext
        domLogo.style.display = '';
        domSubtext.style.display = '';
        openEncryptionFlow(false);
    }, 5000);
}

/**
 * Display the Encryption/Decryption flow.
 * @param {boolean} fUnlock - Whether we're unlocking an existing key, or encrypting a new one.
 * @param {string} securityType - "pin" or "password" (determines which UI to show)
 */
function openEncryptionFlow(fUnlock = false, securityType = 'pin') {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = 'none';
    domLoginEncrypt.style.display = '';
    // Hide the picker only for the NEW-account PIN-setup path (fUnlock=false).
    // The unlock path keeps it visible so the user can switch between
    // existing accounts before entering their PIN/password.
    if (!fUnlock) loginPicker.hide();

    // Hide all input variants initially
    domLoginEncryptPinRow.style.display = 'none';
    domLoginEncryptPassword.style.display = 'none';
    domLoginEncryptTypeSelect.style.display = 'none';

    // Track chosen security type
    let chosenSecurityType = securityType;

    // AbortControllers for listener cleanup (avoids cloning DOM — mobile WebViews
    // don't reliably handle cloned inputs)
    let pinAbortController = null;
    let passwordAbortController = null;

    // If unlocking, go straight to the appropriate input
    if (fUnlock) {
        startCredentialEntry(chosenSecurityType);
    } else {
        // New account setup — show security type selection first
        showSecurityTypeSelector();
    }

    /** Show the security type selection phase */
    function showSecurityTypeSelector() {
        // Hide lock icon header — the type selector uses the login logo above instead
        document.querySelector('.login-encrypt-header').style.display = 'none';
        domLoginEncryptPinRow.style.display = 'none';
        domLoginEncryptPassword.style.display = 'none';
        domLoginEncryptTypeSelect.style.display = '';

        const btnPin = document.getElementById('security-type-pin');
        const btnPassword = document.getElementById('security-type-password');
        const btnSkip = document.getElementById('security-type-skip');

        btnPin.onclick = () => {
            chosenSecurityType = 'pin';
            domLoginEncryptTypeSelect.style.display = 'none';
            startCredentialEntry('pin');
        };

        btnPassword.onclick = () => {
            chosenSecurityType = 'password';
            domLoginEncryptTypeSelect.style.display = 'none';
            startCredentialEntry('password');
        };

        btnSkip.onclick = async () => {
            // Skip encryption — backend stores the key in plaintext (key never crosses IPC)
            domLoginEncryptTypeSelect.style.display = 'none';
            document.querySelector('.login-encrypt-header').style.display = '';
            document.querySelector('.login-lock-icon').style.display = 'none';
            domLoginEncryptTitle.textContent = 'Setting up your account...';
            domLoginEncryptTitle.classList.add('startup-subtext-gradient');
            try {
                await invoke('skip_encryption');
                login();
            } catch (e) {
                // Backend rejected (disk full, DB locked by AV, migration
                // in flight, etc.) — PENDING_NSEC is preserved server-side
                // so a retry is possible. Surface the error and bring the
                // user back to the type-selector so they can try again.
                domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                await popupConfirm('Could not finish setup', String(e), true);
                domLoginEncryptTypeSelect.style.display = '';
            }
        };
    }

    /** Start the credential entry phase for the chosen type */
    function startCredentialEntry(type) {
        // Re-show lock icon header (hidden during type selector phase)
        document.querySelector('.login-encrypt-header').style.display = '';
        if (type === 'password') {
            startPasswordFlow();
        } else {
            startPinFlow();
        }
    }

    // ========================================================================
    // PIN Flow (existing 6-digit input logic)
    // ========================================================================
    function startPinFlow() {
        // Abort previous listeners if startPinFlow is called again
        if (pinAbortController) pinAbortController.abort();
        pinAbortController = new AbortController();
        const signal = pinAbortController.signal;

        let strPinLast = [];
        let strPinCurrent = Array(6).fill('-');

        const DECRYPTION_PROMPT = `Enter your Decryption Pin`;
        const INITIAL_ENCRYPTION_PROMPT = `Enter your Pin`;
        const RE_ENTER_PROMPT = `Re-enter your Pin`;
        const DECRYPTING_MSG = `Decrypting your keys...`;
        const ENCRYPTING_MSG = `Encrypting your keys...`;
        const INCORRECT_PIN_MSG = `Incorrect pin, try again`;
        const MISMATCH_PIN_MSG = `Pin doesn't match, re-try`;

        // Always query fresh from the live DOM
        const pinRow = document.getElementById('login-encrypt-pins');
        const arrPinDOMs = pinRow.querySelectorAll('input');

        function updateStatusMessage(message, isProcessing = false) {
            domLoginEncryptTitle.textContent = message;
            if (isProcessing) {
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                pinRow.style.display = 'none';
                // Past the point of no return — backend is decrypting or
                // encrypting against THIS account. Mid-flight account swap
                // would race the in-progress crypto and bind the wrong
                // session to the result.
                if (typeof loginPicker !== 'undefined') loginPicker.hide();
            } else {
                domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                pinRow.style.display = '';
                // Back to input state. On the unlock path, re-show the
                // picker so a wrong-PIN retry can swap accounts. On the
                // new-account setup path (fUnlock=false) the picker was
                // intentionally hidden by openEncryptionFlow and stays so.
                if (fUnlock && typeof loginPicker !== 'undefined'
                    && loginPicker.accounts && loginPicker.accounts.length >= 2) {
                    loginPicker.show(loginPicker.activeNpub);
                }
            }
            domLoginEncryptPassword.style.display = 'none';
        }

        function resetPinDisplay(focusFirst = true, revertTitleFromErrorState = true) {
            strPinCurrent = Array(6).fill('-');
            arrPinDOMs.forEach(input => input.value = '');
            if (revertTitleFromErrorState) {
                const currentTitle = domLoginEncryptTitle.textContent;
                if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                    const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                    updateStatusMessage(newTitle);
                }
            }
            if (focusFirst && arrPinDOMs.length > 0) {
                arrPinDOMs[0].focus();
            }
        }

        let pinProcessing = false;

        async function handleFullPinEntered() {
            if (pinProcessing) return;
            pinProcessing = true;
            const currentPinString = strPinCurrent.join('');

            if (strPinLast.length === 0) {
                if (fUnlock) {
                    // For bunker accounts the decrypt is sub-second but the
                    // bunker bootstrap RPC takes most of the wait — surface
                    // that instead of leaving "Decrypting…" up the whole time.
                    const loadingMsg = window.__activeSignerType === 'bunker'
                        ? 'Connecting to Signer…'
                        : DECRYPTING_MSG;
                    updateStatusMessage(loadingMsg, true);
                    try {
                        // Decrypt and login entirely in backend (key never crosses IPC).
                        // The wrapper polls Tor's bootstrap state so the title flips
                        // to "Bootstrapping Tor…" while Arti is fetching consensus,
                        // instead of leaving "Decrypting…" up for 5-15s.
                        const npub = await runWithTorBootstrapStatus(() =>
                            invoke("login_from_stored_key", { password: currentPinString })
                        );
                        strPubkey = npub;
                        login();
                    } catch (e) {
                        // Distinguish bunker-unreachable from wrong-PIN: the
                        // PIN was already validated by internal_decrypt, so a
                        // post-decrypt failure (signer unreachable) shouldn't
                        // be presented as "Incorrect PIN".
                        const handled = typeof window.handleBunkerLoginError === 'function'
                            ? await window.handleBunkerLoginError(e)
                            : false;
                        if (handled) { pinProcessing = false; return; }
                        updateStatusMessage(INCORRECT_PIN_MSG);
                        resetPinDisplay(true, false);
                        pinProcessing = false;
                    }
                } else {
                    strPinLast = [...strPinCurrent];
                    updateStatusMessage(RE_ENTER_PROMPT);
                    resetPinDisplay(true, false);
                    pinProcessing = false;
                }
            } else {
                const isMatching = strPinLast.every((char, idx) => char === strPinCurrent[idx]);
                if (isMatching) {
                    updateStatusMessage(ENCRYPTING_MSG, true);
                    // Encrypt and store key entirely in backend (key never crosses IPC).
                    // Wrap in try/catch — `setup_encryption` can reject (disk
                    // full, DB locked, migration mid-flight). Backend preserves
                    // PENDING_NSEC on failure so a retry is possible.
                    try {
                        await invoke('setup_encryption', { password: strPinLast.join(''), securityType: chosenSecurityType });
                        login();
                    } catch (e) {
                        await popupConfirm('Could not save your PIN', String(e), true);
                        strPinLast = [];
                        resetPinDisplay(true, true);
                        pinProcessing = false;
                    }
                } else {
                    updateStatusMessage(MISMATCH_PIN_MSG);
                    strPinLast = [];
                    resetPinDisplay(true, true);
                    pinProcessing = false;
                }
            }
        }

        // Attach listeners directly to each original input with AbortController signal
        arrPinDOMs.forEach((input, nIndex) => {
            input.addEventListener('keydown', (event) => {
                if (event.key === 'Backspace') {
                    event.preventDefault();
                    const currentTitle = domLoginEncryptTitle.textContent;
                    if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                        const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                        updateStatusMessage(newTitle);
                    }
                    if (input.value !== '') {
                        input.value = '';
                        strPinCurrent[nIndex] = '-';
                    } else if (nIndex > 0) {
                        const prev = arrPinDOMs[nIndex - 1];
                        prev.value = '';
                        strPinCurrent[nIndex - 1] = '-';
                        prev.focus();
                    }
                } else if (event.key === 'ArrowLeft') {
                    event.preventDefault();
                    if (nIndex > 0) arrPinDOMs[nIndex - 1].focus();
                } else if (event.key === 'ArrowRight') {
                    event.preventDefault();
                    if (nIndex + 1 < arrPinDOMs.length) arrPinDOMs[nIndex + 1].focus();
                } else if (event.key.length === 1 && !event.key.match(/^[0-9]$/)) {
                    event.preventDefault();
                }
            }, { signal });

            input.addEventListener('input', async () => {
                let sanitizedValue = input.value.replace(/[^0-9]/g, '');
                if (sanitizedValue.length > 1) sanitizedValue = sanitizedValue.charAt(0);
                input.value = sanitizedValue;

                if (sanitizedValue) {
                    strPinCurrent[nIndex] = sanitizedValue;
                    if (nIndex + 1 < arrPinDOMs.length) arrPinDOMs[nIndex + 1].focus();
                } else {
                    strPinCurrent[nIndex] = '-';
                }

                if (!strPinCurrent.includes('-')) {
                    await handleFullPinEntered();
                }
            }, { signal });

            input.value = '';
        });

        updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);
        if (arrPinDOMs.length > 0) arrPinDOMs[0].focus();
    }

    // ========================================================================
    // Password Flow (text input)
    // ========================================================================
    function startPasswordFlow() {
        let lastPassword = '';
        let passwordProcessing = false;

        const DECRYPTION_PROMPT = `Enter your Password`;
        const INITIAL_ENCRYPTION_PROMPT = `Choose a Password`;
        const RE_ENTER_PROMPT = `Re-enter your Password`;
        const DECRYPTING_MSG = `Decrypting your keys...`;
        const ENCRYPTING_MSG = `Encrypting your keys...`;
        const INCORRECT_MSG = `Incorrect password, try again`;
        const MISMATCH_MSG = `Passwords don't match, re-try`;
        const TOO_SHORT_MSG = `Password must be at least 4 characters`;

        function updateStatusMessage(message, isProcessing = false) {
            domLoginEncryptTitle.textContent = message;
            if (isProcessing) {
                domLoginEncryptTitle.classList.add('startup-subtext-gradient');
                domLoginEncryptPassword.style.display = 'none';
                // Past the point of no return — see PIN flow.
                if (typeof loginPicker !== 'undefined') loginPicker.hide();
            } else {
                domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
                domLoginEncryptPassword.style.display = '';
                // See PIN flow for the rationale.
                if (fUnlock && typeof loginPicker !== 'undefined'
                    && loginPicker.accounts && loginPicker.accounts.length >= 2) {
                    loginPicker.show(loginPicker.activeNpub);
                }
            }
            domLoginEncryptPinRow.style.display = 'none';
        }

        updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);

        // Abort previous password listeners if startPasswordFlow is called again
        if (passwordAbortController) passwordAbortController.abort();
        passwordAbortController = new AbortController();
        const signal = passwordAbortController.signal;

        const newInput = document.getElementById('login-password-input');
        newInput.value = '';
        newInput.focus();

        // Login button
        const loginBtn = document.getElementById('login-password-btn');

        async function handlePasswordSubmit() {
            if (passwordProcessing) return;

            const password = newInput.value;

            if (fUnlock) {
                // Unlock flow — single password entry
                if (!password) return;
                passwordProcessing = true;
                // Bunker accounts spend the bulk of the wait on the signer
                // RPC, not decryption — show the more accurate message.
                const loadingMsg = window.__activeSignerType === 'bunker'
                    ? 'Connecting to Signer…'
                    : DECRYPTING_MSG;
                updateStatusMessage(loadingMsg, true);
                try {
                    // Decrypt and login entirely in backend (key never crosses IPC).
                    // Wrapper flips the title to "Bootstrapping Tor…" if Arti is
                    // mid-bootstrap during the call.
                    const npub = await runWithTorBootstrapStatus(() =>
                        invoke("login_from_stored_key", { password })
                    );
                    strPubkey = npub;
                    login();
                } catch (e) {
                    // Bunker-unreachable case: pass through to re-auth flow
                    // instead of telling the user their password is wrong.
                    const handled = typeof window.handleBunkerLoginError === 'function'
                        ? await window.handleBunkerLoginError(e)
                        : false;
                    if (handled) { passwordProcessing = false; return; }
                    updateStatusMessage(INCORRECT_MSG);
                    newInput.value = '';
                    newInput.focus();
                    passwordProcessing = false;
                }
            } else if (!lastPassword) {
                // First entry — set password
                if (password.length < 4) {
                    updateStatusMessage(TOO_SHORT_MSG);
                    return;
                }
                lastPassword = password;
                updateStatusMessage(RE_ENTER_PROMPT);
                newInput.value = '';
                newInput.focus();
            } else {
                // Confirmation entry
                if (password === lastPassword) {
                    passwordProcessing = true;
                    updateStatusMessage(ENCRYPTING_MSG, true);
                    // Encrypt and store key entirely in backend (key never crosses IPC).
                    // Wrap in try/catch — `setup_encryption` can reject (disk
                    // full, DB locked, migration mid-flight). Backend preserves
                    // PENDING_NSEC on failure so a retry is possible.
                    try {
                        await invoke('setup_encryption', { password: lastPassword, securityType: chosenSecurityType });
                        login();
                    } catch (e) {
                        await popupConfirm('Could not save your password', String(e), true);
                        lastPassword = '';
                        newInput.value = '';
                        newInput.focus();
                        passwordProcessing = false;
                    }
                } else {
                    updateStatusMessage(MISMATCH_MSG);
                    lastPassword = '';
                    newInput.value = '';
                    newInput.focus();
                }
            }
        }

        newInput.addEventListener('keydown', (event) => {
            if (event.key === 'Enter') {
                event.preventDefault();
                handlePasswordSubmit();
            }
        }, { signal });

        if (loginBtn) loginBtn.addEventListener('click', handlePasswordSubmit, { signal });

        newInput.focus();
    }
}



/**
 * A simple state tracker for the last message ID, if it changes, we auto-scroll
 */
let strLastMsgID = "";

/**
 * The current Message ID being replied to
 */
let strCurrentReplyReference = "";

/**
 * The current Message ID being edited (if in edit mode)
 */
let strCurrentEditMessageId = "";

/**
 * The original content of the message being edited (for cancel restoration)
 */
let strCurrentEditOriginalContent = "";

/**
 * Updates the current chat (to display incoming and outgoing messages)
 * @param {Chat} chat - The chat to update
 * @param {Array<Message>} arrMessages - The messages to efficiently insert into the chat
 * @param {Profile} profile - Optional profile for display info
 * @param {boolean} fClicked - Whether the chat was opened manually or not
 */
/**
 * Synchronously set the chat header (avatar + name + subtext + click handlers).
 * Called both from openChat (immediately, so the header is visible the instant
 * the chat panel reveals) and from updateChat (in case profile data changed
 * while the chat was open).
 *
 * @param {Object} chat - The chat object (may be null while loading)
 * @param {Profile} profile - DM profile (null for groups)
 * @param {boolean} isGroup
 * @param {boolean} fNotes - Self-DM "Notes" mode
 */
/** Build the chat-header overflow ("hamburger") menu items for a chat.
 *  Single source of truth for both the click handler and the button's
 *  visibility — when this returns empty (e.g. group chats, which have no
 *  per-chat options yet), the button is hidden rather than opening an
 *  empty menu. */
function buildChatMenuItems(chat) {
    const items = [];
    if (chat?.chat_type === 'DirectMessage') {
        items.push({
            label: 'Change Wallpaper',
            icon: 'image',
            onClick: () => startWallpaperChange(strOpenChat),
        });
        if (chat?.wallpaper_path) {
            items.push({
                label: 'Remove Wallpaper',
                icon: 'trash',
                onClick: () => removeWallpaper(strOpenChat),
            });
        }
    }
    return items;
}

function setChatHeader(chat, profile, isGroup, fNotes) {
    domChatHeaderAvatarContainer.innerHTML = '';
    let domChatAvatar;
    if (fNotes) {
        domChatAvatar = null;
    } else if (isGroup) {
        const groupAvatarSrc = chat?.metadata?.avatar_cached ? convertFileSrc(chat.metadata.avatar_cached) : null;
        domChatAvatar = createAvatarImg(groupAvatarSrc, 22, true);
        domChatAvatar.classList.add('btn');
        domChatAvatar.onclick = () => {
            closeChat();
            openGroupOverview(chat);
        };
    } else {
        const chatAvatarSrc = getProfileAvatarSrc(profile);
        domChatAvatar = createAvatarImg(chatAvatarSrc, 22, false);
        domChatAvatar.classList.add('btn');
        domChatAvatar.onclick = () => {
            previousChatBeforeProfile = strOpenChat;
            openProfile(profile);
        };
    }
    if (domChatAvatar) domChatHeaderAvatarContainer.appendChild(domChatAvatar);

    if (fNotes) {
        domChatContact.textContent = 'Notes';
        domChatContact.classList.remove('btn');
        domChatContact.onclick = null;
    } else if (isGroup) {
        domChatContact.textContent = chat?.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
        domChatContact.onclick = () => {
            closeChat();
            openGroupOverview(chat);
        };
        domChatContact.classList.add('btn');
    } else {
        domChatContact.textContent = profile?.nickname || profile?.name || strOpenChat.substring(0, 10) + '…';
        if (profile?.nickname || profile?.name) twemojify(domChatContact);
        domChatContact.onclick = () => {
            previousChatBeforeProfile = strOpenChat;
            openProfile(profile);
        };
        domChatContact.classList.add('btn');
    }

    if (chat) {
        updateChatHeaderSubtext(chat);
    } else {
        // Clear stale subtext from a previously-open chat when switching to a
        // contact that hasn't been synced yet (no entry in arrChats).
        domChatContactStatus.textContent = '';
        domChatContactStatus.classList.remove('typing-indicator-text');
        domChatContactStatus.classList.add('status-hidden');
    }

    // Hide the overflow menu button when the chat has no menu options.
    const domChatMenuBtn = document.getElementById('chat-menu-btn');
    if (domChatMenuBtn) {
        domChatMenuBtn.style.display = buildChatMenuItems(chat).length ? '' : 'none';
    }
}

async function updateChat(chat, arrMessages = [], profile = null, fClicked = false) {
    // Queue profiles for this chat — fire-and-forget so rendering is not delayed
    // by an IPC roundtrip. Awaiting this here used to race the very first
    // attachment_upload_progress events ahead of the spinner DOM, leaving the
    // upload ring frozen.
    if (chat) {
        invoke("queue_chat_profiles_sync", {
            chatId: chat.id,
            isOpening: true
        });
    }
    
    // Check if this is a group chat
    const isGroup = chatIsGroup(chat);

    // If no profile is provided and it's not a group, try to get it from the chat ID
    if (!profile && chat && !isGroup) {
        profile = getProfile(chat.id);
    }
    
    // If this chat is our own npub: then we consider this our Bookmarks/Notes section
    const fNotes = strOpenChat === strPubkey;

    // Header is set synchronously by openChat before this runs, but call it
    // again here in case profile data has changed while the chat was open.
    setChatHeader(chat, profile, isGroup, fNotes);

    if (chat?.messages.length || arrMessages.length) {

        // markAsRead is handled by callers (openChat synchronously, message_new
        // handlers for real-time arrivals, closeChat on exit, onFocusChanged on
        // window refocus). An async focus-gated markAsRead used to live here,
        // but the IPC could hang/fail and leave chat.last_read stuck behind.

        if (!arrMessages.length) return;

        // Sort messages by timestamp (oldest first) to ensure correct insertion order
        // This is critical for timestamp insertion logic - without this, newer messages
        // get inserted first and older messages compare gaps against distant ancestors
        // instead of their actual chronological neighbors
        const sortedMessages = [...arrMessages].sort((a, b) => a.at - b.at);

        // Track last message time for timestamp insertion
        let nLastMsgTime = null;

        /* Dedup guard: skip any message already present in the DOM by ID */
         // Process each message for insertion
        for (const msg of sortedMessages) {
            // Guard against duplicate insertions if the DOM already contains this message ID
            if (document.getElementById(msg.id)) {
                continue;
            }
            // Quick check for empty chat - simple append (with leading day separator).
            if (domChatMessages.children.length === 0) {
                insertTimestamp(msg.at, domChatMessages);
                nLastMsgTime = msg.at;
                domChatMessages.appendChild(renderMessage(msg, profile));
                continue;
            }

            // Messages are managed by the procedural scroll system

            // Direct comparison with newest and oldest messages (most common cases)
            // This avoids expensive DOM operations for the common cases

            // Get the newest message in the DOM
            const newestMsgElement = domChatMessages.lastElementChild;
            const newestMsg = chat.messages.find(m => m.id === newestMsgElement.id);
            if (newestMsg && msg.at > newestMsg.at) {
                // It's the newest message, append it

                // Day-boundary separator (e.g. crossing midnight while chat is open).
                if (nLastMsgTime === null) {
                    nLastMsgTime = newestMsg.at;
                }

                if (_dmsgIsDifferentDay(nLastMsgTime, msg.at)) {
                    insertTimestamp(msg.at, domChatMessages);
                    nLastMsgTime = msg.at;
                }

                // Render message post-time-insert for improved message rendering context
                const domMsg = renderMessage(msg, profile);
                if (!msg.mine && arrMessages.length === 1) {
                    domMsg.classList.add('new-anim');
                    domMsg.addEventListener('animationend', () => {
                        // Remove the animation class once it finishes
                        domMsg?.classList?.remove('new-anim');
                    }, { once: true });
                    // Bump the scroll-down badge if the user is reading
                    // above; softChatScroll is a no-op for them, so this is
                    // the only signal that something new arrived. Also drop
                    // a divider when the window is inactive — they're pinned
                    // but tabbed out, so they haven't actually seen it.
                    if (!chatPinnedToBottom) {
                        incrementUnreadBelow();
                        insertUnreadDivider(domMsg);
                    } else if (!isWindowActive()) {
                        insertUnreadDivider(domMsg);
                    }
                }

                domChatMessages.appendChild(domMsg);

                // If this was our pending message, then snap the view to the bottom
                // and clear the unread divider — sending counts as "I've read up
                // to here". Covers every send path (text/file/voice/miniapp) with
                // a single hook since they all funnel through this rendering.
                if (msg.mine && msg.pending) {
                    scrollToBottom(domChatMessages, false);
                    clearUnreadDivider();
                }
                continue;
            }

            // Get the oldest message in the DOM. Match any rendered element
            // that maps to a message — including system events (which render
            // as .msg-inline-timestamp, not .dmsg). Anchoring only to .dmsg
            // would let older prepended messages slip BELOW a system event
            // stranded at the top, pinning it there out of chronological order.
            let oldestMsgElement = null;
            for (let i = 0; i < domChatMessages.children.length; i++) {
                const child = domChatMessages.children[i];
                if (child.id && chat.messages.some(m => m.id === child.id)) {
                    oldestMsgElement = child;
                    break;
                }
            }

            if (oldestMsgElement) {
                const oldestMsg = chat.messages.find(m => m.id === oldestMsgElement.id);
                if (oldestMsg && msg.at < oldestMsg.at) {
                    // Prepend the new oldest. The existing day-separator above
                    // old_oldest must stay glued to it (it labels old_oldest's
                    // day) — otherwise each different-day prepend leaves a
                    // stale separator stranded at the top of the chat.
                    const domMsg = renderMessage(msg, profile, '', oldestMsgElement);
                    const existingSep = oldestMsgElement.previousElementSibling;
                    const existingSepIsDate = existingSep
                        && existingSep.classList?.contains('msg-inline-timestamp')
                        && !existingSep.classList.contains('unread-divider');

                    domChatMessages.insertBefore(domMsg, oldestMsgElement);
                    if (_dmsgIsDifferentDay(msg.at, oldestMsg.at)) {
                        if (existingSepIsDate) {
                            // Reseat the existing sep between the new oldest
                            // and old_oldest, where it still labels old_oldest's day.
                            domChatMessages.insertBefore(existingSep, oldestMsgElement);
                        }
                        const newSep = insertTimestamp(msg.at);
                        domChatMessages.insertBefore(newSep, domMsg);
                    }
                    // Same-day case: the existing sep now correctly sits above
                    // both X and old_oldest, no work needed.
                    continue;
                }
            }

            // If we get here, the message belongs somewhere in the middle
            // This is a less common case, so we'll do a linear scan
            let inserted = false;

            // Get the message elements sorted by time (oldest to newest).
            // Include system events (.msg-inline-timestamp with an id) so a
            // mid-list insert lands in true chronological order relative to
            // them, not just relative to .dmsg rows.
            let messageNodes = [];
            for (let i = 0; i < domChatMessages.children.length; i++) {
                const child = domChatMessages.children[i];
                if (child.id) {
                    const childMsg = chat.messages.find(m => m.id === child.id);
                    if (childMsg) {
                        messageNodes.push({ element: child, message: childMsg });
                    }
                }
            }

            // Sort by timestamp if needed (they might not be in order in the DOM)
            messageNodes.sort((a, b) => a.message.at - b.message.at);

            // Find the correct position to insert
            for (let i = 0; i < messageNodes.length - 1; i++) {
                const currentNode = messageNodes[i];
                const nextNode = messageNodes[i + 1];

                if (currentNode.message.at <= msg.at && msg.at <= nextNode.message.at) {
                    // Day-boundary separator if the inserted message is on a new day vs. the previous one.
                    if (_dmsgIsDifferentDay(currentNode.message.at, msg.at)) {
                        const timestamp = insertTimestamp(msg.at);
                        domChatMessages.insertBefore(timestamp, nextNode.element);
                    }

                    // Insert between these two messages
                    const domMsg = renderMessage(msg, profile, '', nextNode.element);
                    domChatMessages.insertBefore(domMsg, nextNode.element);
                    inserted = true;
                    break;
                }
            }

            // If somehow not inserted by the above logic, append as fallback
            if (!inserted) {
                // Day-boundary separator vs. the last existing message.
                const lastMsg = messageNodes[messageNodes.length - 1]?.message;
                if (lastMsg && _dmsgIsDifferentDay(lastMsg.at, msg.at)) {
                    insertTimestamp(msg.at, domChatMessages);
                }

                const domMsg = renderMessage(msg, profile);
                domChatMessages.appendChild(domMsg);
            }
        }

        // Rebuild date separators from the final `.dmsg` order so any
        // orphans or misplaced separators from per-message inserts get
        // healed in one pass.
        _dedupeAdjacentDaySeparators();

        // Auto-scroll on new messages (if the user hasn't scrolled up, or on manual chat open)
        const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
        if (pxFromBottom < 500 || fClicked) {
            const cLastMsg = chat.messages[chat.messages.length - 1];
            if (strLastMsgID !== cLastMsg.id || fClicked) {
                strLastMsgID = cLastMsg.id;
                adjustSize();
                // Force an auto-scroll, given soft-scrolling won't accurately work when the entire list has just rendered
                scrollToBottom(domChatMessages, false);
            }
        }
    } else {
        // Probably a 'New Chat', as such, we'll mostly render an empty chat
        // Clear existing messages when opening a new chat (fClicked = true)
        // This prevents messages from the previous chat from showing
        if (fClicked) {
            domChatMessages.innerHTML = '';
        }
        
        // Render chat header avatar
        domChatHeaderAvatarContainer.innerHTML = '';
        let domChatAvatar;
        if (fNotes) {
            // Notes: no avatar icon
            domChatAvatar = null;
        } else if (isGroup) {
            const groupAvatarSrc = chat.metadata?.avatar_cached ? convertFileSrc(chat.metadata.avatar_cached) : null;
            domChatAvatar = createAvatarImg(groupAvatarSrc, 22, true);
            domChatAvatar.classList.add('btn');
            domChatAvatar.onclick = () => {
                closeChat();
                openGroupOverview(chat);
            };
        } else {
            // DM: use profile avatar or placeholder
            const dmAvatarSrc = getProfileAvatarSrc(profile);
            domChatAvatar = createAvatarImg(dmAvatarSrc, 22, false);
        }
        if (domChatAvatar) domChatHeaderAvatarContainer.appendChild(domChatAvatar);

        if (fNotes) {
            domChatContact.textContent = 'Notes';
            domChatContact.onclick = null;
            domChatContact.classList.remove('btn');
            domChatContactStatus.textContent = 'Encrypted Notes to Self';
            domChatContactStatus.classList.remove('typing-indicator-text');
        } else if (isGroup) {
            domChatContact.textContent = chat?.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
            domChatContact.onclick = () => {
                closeChat();
                openGroupOverview(chat);
            };
            domChatContact.classList.add('btn');

            // Ensure the member count/status renders even before the first message
            updateChatHeaderSubtext(chat);
        } else {
            domChatContact.textContent = profile?.nickname || profile?.name || strOpenChat.substring(0, 10) + '…';
            domChatContact.onclick = null;
            domChatContact.classList.remove('btn');
            domChatContactStatus.textContent = '';
            domChatContactStatus.classList.remove('typing-indicator-text');
        }

        domChatContact.classList.toggle('chat-contact', !domChatContactStatus.textContent);
        domChatContact.classList.toggle('chat-contact-with-status', !!domChatContactStatus.textContent);
        domChatContactStatus.style.display = !domChatContactStatus.textContent ? 'none' : '';
    }

    adjustSize();
    
    // Update the back button notification dot after chat updates
    updateChatBackNotification();
}

/**
 * Helper function to create and insert a timestamp
 * @param {number} timestamp - Unix timestamp in seconds
 * @param {HTMLElement} parent - Optional parent to append to
 * @returns {HTMLElement} - The created timestamp element
 */
// Cached formatters — Intl.DateTimeFormat construction is expensive vs. .format()
const _insertTimestampTimeFmt = new Intl.DateTimeFormat([], { hour: 'numeric', minute: '2-digit', hour12: true });
const _insertTimestampDateFmt = new Intl.DateTimeFormat();

/**
 * Wipe and rebuild date separators from the current `.dmsg` order. Runs
 * after batch operations (procedural scroll prepends) so any orphans or
 * misplaced separators left by the per-message insert paths get healed
 * in a single O(N) sweep. Skips `.unread-divider` so the "New" marker
 * survives untouched.
 */
function _dedupeAdjacentDaySeparators() {
    if (!domChatMessages) return;

    // Drop every existing date divider (system events and the "New" divider
    // share `.msg-inline-timestamp` styling but are NOT date dividers, so
    // the dedicated `.date-divider` class scopes this pass safely).
    const stale = [];
    for (const child of domChatMessages.children) {
        if (child.classList?.contains('date-divider')) stale.push(child);
    }
    for (const sep of stale) sep.remove();

    // Re-insert one date divider above the first day-content element (a
    // `.dmsg` row OR a system event — both carry `dataset.at`) that starts a
    // new day. The "New" divider and stale separators have no `at`, so the
    // Number.isFinite guard skips them.
    let prevAt = null;
    const inserts = [];
    for (const child of domChatMessages.children) {
        const at = parseInt(child.dataset?.at, 10);
        if (!Number.isFinite(at)) continue;
        if (prevAt === null || _dmsgIsDifferentDay(prevAt, at)) {
            inserts.push({ before: child, at });
        }
        prevAt = at;
    }
    for (const { before, at } of inserts) {
        domChatMessages.insertBefore(insertTimestamp(at), before);
    }
}

function insertTimestamp(timestamp, parent = null) {
    const pTimestamp = document.createElement('p');
    // `.date-divider` distinguishes day-boundary timestamps from system
    // events (which reuse `msg-inline-timestamp` styling) so the rebuild
    // pass below only touches date dividers.
    pTimestamp.classList.add('msg-inline-timestamp', 'date-divider');
    const messageDate = new Date(timestamp);
    const timeStr = _insertTimestampTimeFmt.format(messageDate);

    // Render the time contextually (day/date in bold)
    if (isToday(messageDate)) {
        pTimestamp.innerHTML = `<strong>Today</strong>, ${timeStr}`;
    } else if (isYesterday(messageDate)) {
        pTimestamp.innerHTML = `<strong>Yesterday</strong>, ${timeStr}`;
    } else {
        const dateStr = _insertTimestampDateFmt.format(messageDate);
        pTimestamp.innerHTML = `<strong>${dateStr}</strong>, ${timeStr}`;
    }

    if (parent) {
        parent.appendChild(pTimestamp);
    }

    return pTimestamp;
}

/**
 * Helper function to create and insert a system event (member joined/left, etc.)
 * Uses the same styling as timestamps (centered, lower opacity)
 * @param {string} content - The system event text (e.g., "John has left")
 * @param {HTMLElement} parent - Optional parent to append to
 * @returns {HTMLElement} - The created system event element
 */
function insertSystemEvent(content, parent = null) {
    const pSystemEvent = document.createElement('p');
    pSystemEvent.classList.add('msg-inline-timestamp'); // Reuse timestamp styling
    pSystemEvent.textContent = content;

    if (parent) {
        parent.appendChild(pSystemEvent);
    }

    return pSystemEvent;
}

// ============================================================================
// Per-DM Wallpaper
// ============================================================================
// Three states the chat can be in:
//   • No wallpaper — empty `chat.wallpaper_path`, default chat background.
//   • Active wallpaper — `chat.wallpaper_path` set, layer renders that file.
//   • Previewing — user picked an image but hasn't confirmed yet. The layer
//     is swapped to the staged preview file and the slider bar appears
//     above the composer. The bar's lifecycle is tracked by
//     `wallpaperPreviewState`.
//
// Visual settings (blur + brightness) are driven by CSS variables on
// `#chat-wallpaper-layer` so live slider drags are GPU-friendly and don't
// hit the rumor pipeline. The values are persisted alongside the image on
// confirm.

/** {chatId, previewPath, blur, dim} while a preview is active, else null. */
let wallpaperPreviewState = null;

const WALLPAPER_DEFAULT_BLUR = 5;
const WALLPAPER_DEFAULT_DIM = 50;

/** Cache key (`path|ts`) for the wallpaper currently on `--wp-image`.
 *  Slider drags reuse the same key so the image URL isn't re-issued
 *  (which would cache-bust + flicker). A new rumor advances `ts`, which
 *  changes the key and forces a fresh fetch even when the on-disk
 *  filename is identical (deterministic `<chat_npub>.<ext>` path). */
let _lastAppliedWallpaperKey = null;

/** Apply a wallpaper file path + visual settings to the open chat. The
 *  image URL is only re-set when the path actually changes — slider
 *  drags reuse the loaded image and only touch the blur/brightness vars. */
function applyChatWallpaper(chatId, path, blur, dim, ts) {
    if (strOpenChat !== chatId) return;
    const chatEl = document.getElementById('chat');
    const layer = document.getElementById('chat-wallpaper-layer');
    if (!chatEl || !layer) return;
    // Honor the global "Background Wallpaper" display toggle: when it's off,
    // suppress the committed wallpaper so the default theme shows through.
    // A live preview still renders (the user may be setting it for their
    // chat partner and needs to see what they're picking).
    const previewing = chatEl.getAttribute('data-wallpaper-previewing') === 'true';
    const bgDisabled = document.body.classList.contains('chat-bg-disabled');
    const newPath = (bgDisabled && !previewing) ? '' : (path || '');
    // The on-disk filename is deterministic per chat, so an inbound rumor
    // overwrites bytes at the same path. Include `ts` in the cache key
    // so a new wallpaper forces a re-fetch even when the path is unchanged.
    const newKey = newPath + '|' + (ts || 0);
    if (newKey !== _lastAppliedWallpaperKey) {
        if (newPath) {
            const url = convertFileSrc(newPath);
            const busted = url + (url.includes('?') ? '&' : '?') + 't=' + (ts || Date.now());
            layer.style.setProperty('--wp-image', `url("${busted}")`);
            chatEl.setAttribute('data-wallpaper', 'true');
        } else {
            layer.style.removeProperty('--wp-image');
            chatEl.removeAttribute('data-wallpaper');
        }
        _lastAppliedWallpaperKey = newKey;
    }
    const blurPx = Math.max(0, Math.min(30, blur ?? WALLPAPER_DEFAULT_BLUR));
    const brightness = Math.max(0, Math.min(100, dim ?? WALLPAPER_DEFAULT_DIM)) / 100;
    // Build the filter directly. `blur(0px)` clashes with brightness() in
    // WebKit (the layer washes out to solid white), so omit blur entirely at
    // zero rather than passing a 0px radius.
    layer.style.filter = blurPx > 0
        ? `blur(${blurPx}px) brightness(${brightness})`
        : `brightness(${brightness})`;
}

/** Refresh the wallpaper layer from the open chat's persisted state. */
function refreshChatWallpaper() {
    const chat = getChat(strOpenChat);
    applyChatWallpaper(
        strOpenChat,
        chat?.wallpaper_path || '',
        chat?.wallpaper_blur,
        chat?.wallpaper_dim,
        chat?.wallpaper_ts,
    );
}

/** Show or hide the wallpaper edit UI: the bottom slider/trash bar plus the
 *  Cancel/Save overlay on the chat header. Also flags the chat so the
 *  scroll-return button can be hidden via CSS while the preview is up. */
function setWallpaperPreviewBarVisible(visible) {
    const bar = document.getElementById('wallpaper-preview-bar');
    if (bar) bar.style.display = visible ? '' : 'none';
    const editBar = document.getElementById('wallpaper-edit-bar');
    if (editBar) {
        if (visible) {
            editBar.style.opacity = '0';
            editBar.style.display = 'flex';
            setTimeout(() => { editBar.style.opacity = '1'; }, 10);
        } else {
            editBar.style.opacity = '0';
            setTimeout(() => { editBar.style.display = 'none'; }, 250);
        }
    }
    const chatEl = document.getElementById('chat');
    if (chatEl) {
        if (visible) chatEl.setAttribute('data-wallpaper-previewing', 'true');
        else chatEl.removeAttribute('data-wallpaper-previewing');
    }
}

/** Lock the edit-bar buttons while a publish/removal is in flight. */
function setWallpaperEditBusy(busy) {
    for (const id of ['wallpaper-edit-save-btn', 'wallpaper-edit-cancel-btn']) {
        const el = document.getElementById(id);
        if (!el) continue;
        el.style.pointerEvents = busy ? 'none' : '';
        el.style.opacity = busy ? '0.5' : '';
    }
}

/** Read the current slider values from the preview bar. */
function readWallpaperSliders() {
    const blurEl = document.getElementById('wallpaper-blur-slider');
    const dimEl = document.getElementById('wallpaper-dim-slider');
    const blur = blurEl ? parseInt(blurEl.value, 10) : WALLPAPER_DEFAULT_BLUR;
    const dim = dimEl ? parseInt(dimEl.value, 10) : WALLPAPER_DEFAULT_DIM;
    return {
        blur: Number.isFinite(blur) ? blur : WALLPAPER_DEFAULT_BLUR,
        dim: Number.isFinite(dim) ? dim : WALLPAPER_DEFAULT_DIM,
    };
}

/** Compute the 0..100% the slider's value occupies of its range. */
function _wallpaperSliderPct(el) {
    if (!el) return 0;
    const min = parseFloat(el.min) || 0;
    const max = parseFloat(el.max) || 100;
    const val = parseFloat(el.value) || 0;
    if (max === min) return 0;
    return Math.max(0, Math.min(100, ((val - min) / (max - min)) * 100));
}

/** Set the slider values + sync the CSS variable that drives the track fill
 *  gradient. Without this the WebKit `accent-color` fill drifts away from
 *  the thumb position. */
function writeWallpaperSliders(blur, dim) {
    const blurEl = document.getElementById('wallpaper-blur-slider');
    const dimEl = document.getElementById('wallpaper-dim-slider');
    if (blurEl) blurEl.value = String(blur);
    if (dimEl) dimEl.value = String(dim);
    if (blurEl) blurEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(blurEl)}%`);
    if (dimEl) dimEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(dimEl)}%`);
}

/**
 * Open the image picker, hand the result to the backend, and switch the
 * chat into preview mode. Animated sources are converted to a static
 * first-frame server-side; we surface a friendly notice when that happens.
 */
async function startWallpaperChange(chatId) {
    if (!chatId) return;
    const chat = getChat(chatId);
    if (!chat || chat.chat_type !== 'DirectMessage') return;

    try {
        if (platformFeatures.os === 'android') {
            // The Android picker is async (file input -> change event), and
            // its onchange handler calls applyWallpaperPreview directly.
            window.__wallpaperFileInput?.click();
            return;
        }
        // Desktop: Tauri open dialog with image filter.
        const { open } = window.__TAURI__.dialog;
        const filePath = await open({
            multiple: false,
            directory: false,
            filters: [
                { name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif'] },
            ],
        });
        if (!filePath) return;
        const previewResult = await invoke('preview_wallpaper', {
            chatId,
            filePath,
            bytes: null,
            filename: null,
        });
        await applyWallpaperPreview(chatId, previewResult);
    } catch (err) {
        popupConfirm('Couldn’t use that image', String(err), true);
    }
}

/** Apply the staged preview file to the chat + open the slider bar. */
async function applyWallpaperPreview(chatId, previewResult) {
    if (!previewResult?.path) return;
    if (previewResult.was_animated) {
        await popupConfirm(
            'Static wallpapers only',
            'Vector wallpapers don’t animate. We grabbed the first frame of your image to use instead.',
            true,
        );
    }
    // Pick the slider's initial values. If the chat already has a wallpaper,
    // re-picking preserves the user's previous customisation. For first-time
    // picks, fall back to the backend's per-image suggested brightness so
    // photos that are very bright don't ship with text-killing contrast.
    const chat = getChat(chatId);
    const hadWallpaper = !!chat?.wallpaper_path;
    const blur = hadWallpaper ? (chat.wallpaper_blur ?? WALLPAPER_DEFAULT_BLUR) : WALLPAPER_DEFAULT_BLUR;
    const dim = hadWallpaper
        ? (chat.wallpaper_dim ?? WALLPAPER_DEFAULT_DIM)
        : (previewResult.recommended_dim ?? WALLPAPER_DEFAULT_DIM);
    writeWallpaperSliders(blur, dim);
    // Use Date.now() as the cache key so picking a second image with the
    // same extension (same on-disk preview path) forces a refetch.
    const previewTs = Date.now();
    wallpaperPreviewState = { chatId, previewPath: previewResult.path, blur, dim, ts: previewTs };
    clearWallpaperUploadProgress();
    setWallpaperEditBusy(false);
    // Flag previewing FIRST so applyChatWallpaper renders the staged image
    // even when the global "Background Wallpaper" toggle is off.
    setWallpaperPreviewBarVisible(true);
    applyChatWallpaper(chatId, previewResult.path, blur, dim, previewTs);
}

/** Slider input → live-update the layer's CSS variables and preview state. */
function onWallpaperSliderInput() {
    if (!wallpaperPreviewState) return;
    const { blur, dim } = readWallpaperSliders();
    wallpaperPreviewState.blur = blur;
    wallpaperPreviewState.dim = dim;
    const blurEl = document.getElementById('wallpaper-blur-slider');
    const dimEl = document.getElementById('wallpaper-dim-slider');
    if (blurEl) blurEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(blurEl)}%`);
    if (dimEl) dimEl.style.setProperty('--slider-pct', `${_wallpaperSliderPct(dimEl)}%`);
    applyChatWallpaper(wallpaperPreviewState.chatId, wallpaperPreviewState.previewPath, blur, dim, wallpaperPreviewState.ts);
}

/** Publish the preview as the chat's wallpaper (with current slider values). */
async function confirmWallpaperChange() {
    if (!wallpaperPreviewState) return;
    const { chatId, blur, dim } = wallpaperPreviewState;
    setWallpaperEditBusy(true);
    setWallpaperUploadProgress(0);
    try {
        await invoke('publish_wallpaper', { chatId, blur, dim });
        wallpaperPreviewState = null;
        setWallpaperPreviewBarVisible(false);
        // The wallpaper_updated event will land momentarily with the final
        // cached path; nothing else to do here.
        if (document.body.classList.contains('chat-bg-disabled')) {
            // Setting is off — clear the just-previewed image and let them know
            // it's hidden on their end (their chat partner still sees it).
            applyChatWallpaper(chatId, '', 0, WALLPAPER_DEFAULT_DIM, Date.now());
            popupConfirm(
                'Background Wallpaper',
                'Your wallpaper is set. If you want to be able to see it, please enable <b>Settings → Display → Background Wallpaper</b>.',
                true,
            );
        }
    } catch (err) {
        popupConfirm('Wallpaper not sent', String(err), true);
    } finally {
        clearWallpaperUploadProgress();
        setWallpaperEditBusy(false);
    }
}

/** Drive the header label as the encrypted blob streams to Blossom, so the
 *  user knows something is happening even before the first chunk lands. */
function setWallpaperUploadProgress(percentage) {
    const label = document.getElementById('wallpaper-edit-mode-label');
    if (!label) return;
    const pct = Math.max(0, Math.min(100, Math.round(percentage || 0)));
    label.textContent = pct > 0 ? `Uploading… ${pct}%` : 'Uploading…';
}

function clearWallpaperUploadProgress() {
    const label = document.getElementById('wallpaper-edit-mode-label');
    if (label) label.textContent = 'Edit Mode is enabled.';
}

/** Remove the chat's wallpaper, reverting both sides to the default theme.
 *  Reached from the "Remove Wallpaper" chat-menu item (the edit overlay
 *  covers the menu button, so this never fires mid-preview). */
async function removeWallpaper(chatId) {
    if (!chatId) return;
    const ok = await popupConfirm(
        'Remove wallpaper?',
        'This will remove the wallpaper and revert to the default theme. This change syncs to your contact and your other devices.',
        false, '', 'vector_warning.svg'
    );
    if (!ok) return;
    try {
        await invoke('remove_wallpaper', { chatId });
        applyChatWallpaper(chatId, '', 0, 50, Date.now());
    } catch (err) {
        popupConfirm('Wallpaper not removed', String(err), true, '', 'vector_warning.svg');
    }
}

/** Discard the staged preview and revert the chat background. */
async function cancelWallpaperChange() {
    if (!wallpaperPreviewState) return;
    const { chatId } = wallpaperPreviewState;
    wallpaperPreviewState = null;
    setWallpaperPreviewBarVisible(false);
    refreshChatWallpaper();
    try {
        await invoke('cancel_wallpaper_preview', { chatId });
    } catch (err) {
        console.warn('[wallpaper] cancel cleanup failed:', err);
    }
}

/**
 * Creates a file attachment box (the .custom-audio-player styled div) for all download states.
 * @param {Object} cAttachment - the attachment object
 * @param {'downloaded'|'download'|'downloading'} state - the download state
 * @returns {{ fileDiv: HTMLElement, isMiniApp: boolean, descriptionSpan: HTMLElement, iconElement: HTMLElement, updateMiniAppStatus: Function|null, statusSpan: HTMLElement|null }}
 */
/**
 * Create a conical progress spinner for file box icons and replace the target element.
 * Handles clearing the text margin so the in-flow spinner doesn't cause layout shift.
 * @param {HTMLElement} target - the icon element to replace (or null to just create)
 * @param {object} [opts] - options: id (element id), attachmentId (data-attribute)
 * @returns {HTMLDivElement} the spinner element
 */
function createFileBoxSpinner(target, opts = {}) {
    const spinner = document.createElement('div');
    spinner.className = 'miniapp-downloading-spinner';
    if (opts.id) spinner.id = opts.id;
    if (opts.attachmentId) spinner.setAttribute('data-attachment-id', opts.attachmentId);
    // Pick up any progress that was emitted before this DOM was attached
    if (opts.id) applyPendingUploadProgress(spinner, opts.id.replace(/_file$/, ''));
    // Match the Mini App icon position (marginLeft:5px + padding:10px = 15px from edge)
    spinner.style.position = 'absolute';
    spinner.style.left = '15px';
    spinner.style.top = '0';
    spinner.style.bottom = '0';
    spinner.style.margin = 'auto';
    spinner.style.width = '40px';
    spinner.style.height = '40px';
    spinner.style.opacity = '0';
    spinner.style.scale = '0.5';
    spinner.style.transition = 'opacity 0.25s ease, scale 0.25s ease, --progress 0.3s ease';
    const settleTransition = () => {
        // After intro animation, keep only the progress transition
        spinner.style.transition = '--progress 0.3s ease';
    };
    if (target) {
        // Shrink + fade out icon, then swap to spinner and grow + fade it in
        target.style.transition = 'opacity 0.2s ease, scale 0.2s ease';
        target.style.opacity = '0';
        target.style.scale = '0.5';
        setTimeout(() => {
            // Spinner is absolute-positioned — push sibling text past it
            const textSibling = target.parentElement?.querySelector('span');
            if (textSibling) textSibling.style.marginLeft = '60px';
            target.replaceWith(spinner);
            requestAnimationFrame(() => { spinner.style.opacity = '1'; spinner.style.scale = '1'; });
            setTimeout(settleTransition, 300);
        }, 200);
    } else {
        // No target (initial render as downloading) — just grow + fade in
        requestAnimationFrame(() => { spinner.style.opacity = '1'; spinner.style.scale = '1'; });
        setTimeout(settleTransition, 300);
    }
    return spinner;
}

function isSpoilerAttachment(attachment) {
    // DMs carry the filename on `.name`; MLS group attachments carry it on
    // `.mls_filename` (`.name` is empty for the MLS path). Without checking
    // both, MLS spoilers render unspoilered.
    const fileName = attachment.name || attachment.mls_filename || '';
    return fileName.toUpperCase().startsWith('SPOILER_');
}

function createFileBox(cAttachment, state = 'downloaded') {
    const ext = (cAttachment.extension || '').toLowerCase();
    const fileTypeInfo = getFileTypeInfo(ext);
    const isMiniApp = fileTypeInfo.isMiniApp === true;

    const fileDiv = document.createElement('div');
    if (cAttachment.path) {
        fileDiv.setAttribute('filepath', cAttachment.path);
    }
    if (isMiniApp) {
        fileDiv.classList.add('miniapp-attachment');
    }

    // Create the main container
    const btnDiv = document.createElement('div');
    btnDiv.className = isMiniApp ? 'custom-audio-player' : 'btn custom-audio-player';
    btnDiv.style.display = 'flex';
    btnDiv.style.alignItems = 'center';
    btnDiv.style.padding = '10px';
    btnDiv.style.paddingRight = '15px';

    // Create the icon element (span for regular files, img for Mini Apps with icons)
    let iconElement;
    if (isMiniApp) {
        iconElement = document.createElement('img');
        iconElement.style.marginLeft = '5px';
        iconElement.style.width = '40px';
        iconElement.style.height = '40px';
        iconElement.style.borderRadius = '8px';
        iconElement.style.objectFit = 'cover';
        iconElement.style.backgroundColor = 'transparent';
        iconElement.src = 'data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="%23fff"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z"/></svg>';
    } else {
        iconElement = document.createElement('span');
        iconElement.className = `icon icon-${fileTypeInfo.icon}`;
        iconElement.style.marginLeft = '5px';
        iconElement.style.width = '50px';
        iconElement.style.backgroundColor = 'rgba(255, 255, 255, 0.75)';
    }

    // Create the text container span
    const textContainerSpan = document.createElement('span');
    textContainerSpan.style.color = 'rgba(255, 255, 255, 0.85)';
    textContainerSpan.style.marginLeft = isMiniApp ? '15px' : '60px';
    textContainerSpan.style.lineHeight = '1.2';
    textContainerSpan.style.minWidth = '0';

    // Create the description span
    const descriptionSpan = document.createElement('span');
    descriptionSpan.className = 'cutoff';
    descriptionSpan.style.color = 'var(--icon-color-primary)';
    descriptionSpan.style.fontWeight = '400';
    descriptionSpan.innerText = cAttachment.name || fileTypeInfo.description;

    // Create the small element for file details
    const smallElement = document.createElement('small');

    let updateMiniAppStatus = null;
    let statusSpan = null;

    if (state === 'downloaded' && isMiniApp) {
        // Downloaded Mini App: full UI with realtime, peer badge, etc.
        smallElement.style.display = 'flex';
        smallElement.style.alignItems = 'center';
        smallElement.style.gap = '10px';

        const playSpan = document.createElement('span');
        playSpan.style.color = 'rgba(255, 255, 255, 0.7)';
        playSpan.style.fontWeight = '400';
        playSpan.innerText = 'Click to Play';
        smallElement.appendChild(playSpan);

        // Create peer count badge (hidden by default)
        const peerBadge = document.createElement('span');
        peerBadge.style.padding = '2px 8px';
        peerBadge.style.borderRadius = '10px';
        peerBadge.style.backgroundColor = 'rgba(46, 213, 115, 0.15)';
        peerBadge.style.color = 'rgb(46, 213, 115)';
        peerBadge.style.fontSize = '0.85em';
        peerBadge.style.fontWeight = '500';
        peerBadge.style.border = '0.5px solid rgba(46, 213, 115, 0.3)';
        peerBadge.style.display = 'none';
        smallElement.appendChild(peerBadge);

        // Store topic for event updates
        const topicId = cAttachment.webxdc_topic;
        if (topicId) {
            fileDiv.setAttribute('data-webxdc-topic', topicId);
        }

        // Helper function to update the peer badge with avatar stack
        const updatePeerBadge = (peerCount, isPlaying, peerNpubs) => {
            // session_peers is the single source of truth — use its length as the count
            const totalPlayers = (peerNpubs && peerNpubs.length > 0) ? peerNpubs.length : (isPlaying ? peerCount + 1 : peerCount);
            if (totalPlayers > 0) {
                peerBadge.innerHTML = '';

                // Avatar stack — show up to 5 tiny overlapping profile pictures
                const npubs = (peerNpubs || []).sort(); // deterministic order
                const shown = npubs.slice(0, 5);
                if (shown.length > 0) {
                    const stack = document.createElement('span');
                    stack.style.display = 'inline-flex';
                    stack.style.alignItems = 'center';
                    stack.style.marginRight = '4px';
                    shown.forEach((npub, i) => {
                        const wrapper = document.createElement('span');
                        wrapper.style.position = 'relative';
                        wrapper.style.zIndex = String(shown.length - i);
                        if (i > 0) wrapper.style.marginLeft = '-5px';
                        const img = document.createElement('img');
                        const profile = getProfile(npub);
                        const src = getProfileAvatarSrc(profile);
                        const displayName = profile?.nickname || profile?.name;
                        if (displayName) {
                            wrapper.addEventListener('mouseenter', () => showGlobalTooltip(displayName, wrapper));
                            wrapper.addEventListener('mouseleave', hideGlobalTooltip);
                        }
                        img.onerror = function() { this.src = 'icons/contact-placeholder.svg'; };
                        img.src = src || 'icons/contact-placeholder.svg';
                        img.style.width = '14px';
                        img.style.height = '14px';
                        img.style.borderRadius = '50%';
                        img.style.border = '1px solid #1a1a2e';
                        img.style.objectFit = 'cover';
                        img.style.display = 'block';
                        wrapper.appendChild(img);
                        stack.appendChild(wrapper);
                    });
                    peerBadge.appendChild(stack);
                } else {
                    const groupIcon = document.createElement('img');
                    groupIcon.src = 'icons/group-placeholder.svg';
                    groupIcon.style.width = '14px';
                    groupIcon.style.height = '14px';
                    groupIcon.style.verticalAlign = 'middle';
                    groupIcon.style.marginRight = '4px';
                    peerBadge.appendChild(groupIcon);
                }

                peerBadge.appendChild(document.createTextNode(`${totalPlayers} online`));
                peerBadge.style.display = 'inline-flex';
                peerBadge.style.alignItems = 'center';
            } else {
                peerBadge.style.display = 'none';
            }
        };

        // Track last known peer npubs (preserved across status updates that omit it)
        let lastPeerNpubs = [];

        // Helper function to update the UI based on status
        updateMiniAppStatus = (isPlaying, peerCount, peerNpubs) => {
            if (peerNpubs) lastPeerNpubs = peerNpubs;
            if (isPlaying) {
                playSpan.innerText = 'Playing';
                playSpan.style.color = '#2ed573';
                fileDiv.style.cursor = 'default';
                fileDiv.setAttribute('data-playing', 'true');
            } else if (peerCount > 0) {
                playSpan.innerText = 'Click to Join';
                playSpan.style.color = 'rgba(255, 255, 255, 0.7)';
                fileDiv.style.cursor = 'pointer';
                fileDiv.removeAttribute('data-playing');
            } else {
                playSpan.innerText = 'Click to Play';
                playSpan.style.color = 'rgba(255, 255, 255, 0.7)';
                fileDiv.style.cursor = 'pointer';
                fileDiv.removeAttribute('data-playing');
            }
            updatePeerBadge(peerCount, isPlaying, lastPeerNpubs);
        };

        // Load Mini App info asynchronously to get name and icon
        const miniAppPath = cAttachment.path;
        if (miniAppPath) {
            loadMiniAppInfo(miniAppPath).then(info => {
                if (info) {
                    descriptionSpan.innerText = info.name || 'Mini App';
                    if (info.icon_data) {
                        iconElement.src = info.icon_data;
                    }
                }
            }).catch(err => {
                console.warn('Failed to load Mini App info from path:', miniAppPath, err);
            });
        }

        // Check for realtime channel status if we have a topic
        if (topicId) {
            invoke('miniapp_get_realtime_status', { topicId })
                .then(status => {
                    console.log('[MiniApp] Realtime status:', status);
                    const peerCount = (status?.peer_count || 0) > 0
                        ? status.peer_count
                        : (status?.pending_peer_count || 0);
                    updateMiniAppStatus(status?.active || false, peerCount, status?.peers);
                })
                .catch(err => {
                    console.debug('Could not get realtime status:', err);
                });

            fileDiv._updateMiniAppStatus = updateMiniAppStatus;
        }
    } else if (state === 'downloaded') {
        // Downloaded regular file: show extension and size
        if (cAttachment.name) {
            // Name already includes extension, just show size
            const sizeSpan = document.createElement('span');
            sizeSpan.innerText = formatBytes(cAttachment.size);
            smallElement.appendChild(sizeSpan);
        } else {
            const extSpan = document.createElement('span');
            extSpan.style.color = 'white';
            extSpan.style.fontWeight = '400';
            extSpan.innerText = `.${ext}`;

            const sizeSpan = document.createElement('span');
            sizeSpan.innerText = ` — ${formatBytes(cAttachment.size)}`;

            smallElement.appendChild(extSpan);
            smallElement.appendChild(sizeSpan);
        }
    } else {
        // Non-downloaded states: 'download' or 'downloading'
        statusSpan = document.createElement('span');
        statusSpan.className = 'file-status';
        statusSpan.style.color = 'rgba(255, 255, 255, 0.7)';
        statusSpan.style.fontWeight = '400';

        if (state === 'downloading') {
            statusSpan.innerText = 'Downloading';
            fileDiv.style.cursor = 'default';

            // Replace icon with conical progress spinner
            iconElement = createFileBoxSpinner(null, { attachmentId: cAttachment.id });
            // Spinner is absolute-positioned (left:10px, width:40px) — push text past it
            textContainerSpan.style.marginLeft = '60px';
        } else {
            // 'download' — waiting for user to click
            let strSize = '';
            if (cAttachment.size > 0) {
                strSize = ` · ${formatBytes(cAttachment.size)}`;
            }
            statusSpan.innerText = `Click to Download${strSize}`;

            // Tag the icon so click handlers can swap it for a spinner
            iconElement.setAttribute('data-attachment-id', cAttachment.id);
        }
        smallElement.appendChild(statusSpan);

        // For undownloaded Mini Apps, try to resolve name and icon from the Nexus Marketplace cache
        if (isMiniApp) {
            const iconRef = iconElement; // capture before possible spinner swap
            invoke('marketplace_get_app_by_hash', { fileHash: cAttachment.id })
                .then(app => {
                    if (!app) return;
                    if (app.name) descriptionSpan.innerText = app.name;
                    const iconSrc = app.icon_cached ? convertFileSrc(app.icon_cached) : app.icon_url;
                    if (iconSrc && iconRef.tagName === 'IMG') {
                        iconRef.src = iconSrc;
                    }
                })
                .catch(() => { /* Marketplace lookup is best-effort */ });
        }
    }

    // Assemble the structure
    textContainerSpan.appendChild(descriptionSpan);
    textContainerSpan.appendChild(smallElement);
    btnDiv.appendChild(iconElement);
    btnDiv.appendChild(textContainerSpan);
    fileDiv.appendChild(btnDiv);

    return { fileDiv, isMiniApp, descriptionSpan, iconElement, updateMiniAppStatus, statusSpan };
}

/**
 * Start downloading an attachment and update all deduped file boxes in the DOM.
 * Shared by thumbhash-fallback and non-image click handlers.
 */
function startAttachmentDownload(cAttachment, msg, isGroupChat, strOpenChat, sender) {
    if (downloadingAttachmentIds.has(cAttachment.id)) return;
    downloadingAttachmentIds.add(cAttachment.id);
    cAttachment.download_failed = false;
    const downloadNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
    invoke('download_attachment', { npub: downloadNpub, msgId: msg.id, attachmentId: cAttachment.id })
        .catch(() => downloadingAttachmentIds.delete(cAttachment.id));
    // Update ALL file boxes with the same attachment ID (dedup support)
    const escapedId = CSS.escape(cAttachment.id);
    const allIcons = document.querySelectorAll(`[data-attachment-id="${escapedId}"]`);
    for (const oldIcon of allIcons) {
        if (oldIcon.classList.contains('miniapp-downloading-spinner')) continue;
        const parentBox = oldIcon.closest('.custom-audio-player');
        if (parentBox) {
            const status = parentBox.querySelector('.file-status');
            if (status) status.innerText = 'Downloading';
            parentBox.style.cursor = 'default';
        }
        createFileBoxSpinner(oldIcon, { attachmentId: cAttachment.id });
    }
}

async function retryFailedMessage(msg) {
    const chatId = strOpenChat;
    if (!chatId) return;
    try {
        await invoke('delete_failed_message', { messageId: msg.id });
    } catch (e) {
        console.error('Failed to delete failed message for retry:', e);
        return;
    }
    // Re-send: use file_message if attachment exists, otherwise text message
    try {
        const chat = arrChats.find(c => c.id === chatId);
        const isCommunity = chat && chat.chat_type === 'Community';
        if (msg.attachments && msg.attachments.length > 0) {
            if (isCommunity) {
                // Community resends all attachments + caption in one event (multi-attachment).
                // Bail if any local file is gone — silently resending fewer files would
                // publish a different message than the one that failed.
                const paths = msg.attachments.map(a => a.path);
                if (paths.some(p => !p)) {
                    popupConfirm('Cannot retry', 'One or more attachments are no longer available locally. Re-attach the files to send again.', true, '', 'vector_warning.svg');
                    return;
                }
                // Preserve each attachment's name (incl. SPOILER_ prefix) on resend. The local
                // files were already compressed on first send, so don't re-compress.
                const names = msg.attachments.map(a => a.name || '');
                await invoke('send_community_files', { channelId: chatId, content: msg.content || '', filePaths: paths, nameOverrides: names, useCompression: false, repliedTo: msg.replied_to || '' });
            } else {
                const att = msg.attachments[0];
                await invoke('file_message', {
                    receiver: chatId,
                    repliedTo: msg.replied_to || '',
                    filePath: att.path,
                    nameOverride: att.name || ''
                });
            }
        } else {
            // Route through message() so Community retries hit send_community_message
            // (not the DM `message` command, which can't address a channel id).
            await message(chatId, msg.content, msg.replied_to || '');
        }
    } catch (e) {
        console.error('Retry send failed:', e);
    }
}

async function deleteFailedMessage(msgId) {
    try {
        await invoke('delete_failed_message', { messageId: msgId });
    } catch (e) {
        console.error('Failed to delete failed message:', e);
    }
}


/**
 * Cancel any ongoing replies and reset the messaging interface
 */
function cancelReply() {
    // Reset the message UI
    domChatMessageInputFile.style.display = '';
    domChatMessageInputCancel.style.display = 'none';
    domChatMessageInput.setAttribute('placeholder', strOriginalInputPlaceholder);

    // Focus the message input (desktop only - mobile keyboards are disruptive)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Clear the replying-to highlight on the previously-selected row.
    if (strCurrentReplyReference) {
        const domMsg = document.getElementById(strCurrentReplyReference);
        if (domMsg) clearHighlight(domMsg, 'replying');
    }

    // Remove the reply ID
    strCurrentReplyReference = '';

    // Reset send button state based on current input
    const hasText = domChatMessageInput.value.trim().length > 0;
    if (hasText) {
        domChatMessageInputSend.classList.add('active');
        domChatMessageInputSend.style.display = '';
        domChatMessageInputVoice.style.display = 'none';
    } else {
        domChatMessageInputSend.classList.remove('active');
        domChatMessageInputSend.style.display = 'none';
        domChatMessageInputVoice.style.display = '';
    }
}

/**
 * Start editing a message
 * @param {string} messageId - The ID of the message to edit
 * @param {string} content - The current content of the message
 */
function startEditMessage(messageId, content) {
    // Cancel any existing reply first
    if (strCurrentReplyReference) {
        cancelReply();
    }

    // Cancel any existing edit
    if (strCurrentEditMessageId) {
        cancelEdit();
    }

    // Store the edit state
    strCurrentEditMessageId = messageId;
    strCurrentEditOriginalContent = content;

    // Populate the input with the message content
    domChatMessageInput.value = content;

    // Show the cancel button, hide file button
    domChatMessageInputFile.style.display = 'none';
    domChatMessageInputCancel.style.display = '';

    // Update placeholder
    domChatMessageInput.setAttribute('placeholder', 'Editing message...');

    // Show the send button (since we have text)
    domChatMessageInputSend.classList.add('active');
    domChatMessageInputSend.style.display = '';
    domChatMessageInputVoice.style.display = 'none';

    // Focus the input and move cursor to end
    domChatMessageInput.focus();
    domChatMessageInput.setSelectionRange(content.length, content.length);

    // Auto-resize the input
    autoResizeChatInput();
}

/**
 * Cancel editing and restore the input to normal state
 */
function cancelEdit() {
    // Clear the edit state
    strCurrentEditMessageId = '';
    strCurrentEditOriginalContent = '';

    // Clear the input
    domChatMessageInput.value = '';

    // Reset the message UI
    domChatMessageInputFile.style.display = '';
    domChatMessageInputCancel.style.display = 'none';
    domChatMessageInput.setAttribute('placeholder', strOriginalInputPlaceholder);

    // Reset send button state
    domChatMessageInputSend.classList.remove('active');
    domChatMessageInputSend.style.display = 'none';
    domChatMessageInputVoice.style.display = '';

    // Focus the input (desktop only)
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    // Auto-resize the input back to normal
    autoResizeChatInput();
}

/**
 * Show the edit history popup for a message
 * @param {string} messageId - The ID of the message to show history for
 * @param {HTMLElement} targetElement - The element that was clicked (for positioning)
 */
let strCurrentEditHistoryMsgId = '';

function showEditHistory(messageId, targetElement) {
    const popup = document.getElementById('edit-history-popup');
    const content = document.getElementById('edit-history-content');
    if (!popup || !content) return;

    // If clicking the same message that's already open, ignore
    if (strCurrentEditHistoryMsgId === messageId && popup.style.display !== 'none') {
        return;
    }

    // Find the message in the current chat
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat) return;

    const msg = chat.messages.find(m => m.id === messageId);
    if (!msg || !msg.edit_history || msg.edit_history.length === 0) {
        return;
    }

    // Track which message's history is open
    strCurrentEditHistoryMsgId = messageId;

    // Clear previous content
    content.innerHTML = '';

    // Format date/time
    const formatTime = (timestamp) => {
        const date = new Date(timestamp);
        return date.toLocaleString(undefined, {
            month: 'short',
            day: 'numeric',
            hour: '2-digit',
            minute: '2-digit'
        });
    };

    // Build edit history entries (oldest to newest)
    const totalEntries = msg.edit_history.length;
    const entryElements = [];
    msg.edit_history.forEach((entry, index) => {
        const div = document.createElement('div');
        div.classList.add('edit-history-entry');

        // Mark original and current
        const isOriginal = index === 0;
        const isCurrent = index === totalEntries - 1;
        if (isOriginal) div.classList.add('original');
        if (isCurrent) div.classList.add('current');

        // Time and label
        const timeDiv = document.createElement('div');
        timeDiv.classList.add('edit-history-time');
        timeDiv.textContent = formatTime(entry.edited_at);
        if (isOriginal) {
            const label = document.createElement('span');
            label.classList.add('edit-history-label');
            label.textContent = 'Original';
            timeDiv.appendChild(label);
        } else if (isCurrent) {
            const label = document.createElement('span');
            label.classList.add('edit-history-label');
            label.textContent = 'Current';
            timeDiv.appendChild(label);
        }

        // Content
        const textDiv = document.createElement('div');
        textDiv.classList.add('edit-history-text');
        textDiv.textContent = entry.content;

        div.appendChild(timeDiv);
        div.appendChild(textDiv);
        content.appendChild(div);
        entryElements.push(div);
    });

    // Find the message bubble (p element) for positioning
    const msgBubble = targetElement.closest('.dmsg');
    const rect = msgBubble ? msgBubble.getBoundingClientRect() : targetElement.getBoundingClientRect();

    // Reset position and show popup to measure its actual dimensions
    popup.style.top = '0';
    popup.style.left = '0';
    popup.style.visibility = 'hidden';
    popup.style.display = 'block';

    // Force layout recalculation then measure
    const popupHeight = popup.getBoundingClientRect().height;
    const popupWidth = popup.getBoundingClientRect().width;

    // Position above or below the bubble depending on space
    let top = rect.top - popupHeight - 4;
    const showBelow = top < 10;
    if (showBelow) {
        top = rect.bottom + 4;
    }

    // Apply staggered animation delays based on position
    // Above: latest (bottom) fades first, oldest (top) last
    // Below: oldest (top) fades first, latest (bottom) last
    entryElements.forEach((el, index) => {
        const delay = showBelow ? index * 50 : (totalEntries - 1 - index) * 50;
        el.style.animationDelay = `${delay}ms`;
    });

    // Align horizontally with the bubble edge, keep within viewport
    let left = rect.left;
    left = Math.max(10, Math.min(left, window.innerWidth - popupWidth - 10));

    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
    popup.style.visibility = 'visible';

    // Scroll to show the current (latest) entry
    content.scrollTop = content.scrollHeight;
}

/**
 * Hide the edit history popup
 */
function hideEditHistory() {
    const popup = document.getElementById('edit-history-popup');
    if (popup) {
        popup.style.display = 'none';
    }
    strCurrentEditHistoryMsgId = '';
}

/**
 * Open a chat with a particular contact
 * @param {string} contact
 */
// System events (wallpaper/membership changes) are synthesized app-data
// events stored apart from the message-views pagination (kind 30078,
// distinguished by a `d` tag, so the kind-filtered message window skips them).
// To give them the SAME on-demand windowing as messages, the full set is
// fetched once into this side buffer, then revealed into the message cache
// only as far back as the loaded message window reaches — and progressively
// as the user scrolls older messages into view.
const _systemEventBuffer = new Map(); // chatId -> sorted array of system-event msg objects

/** Reveal buffered system events that fall within the currently-loaded
 *  message window into the event cache. Returns the newly-revealed ones so
 *  the caller can hand them to updateChat. Dedup-safe via cache.addEvent. */
function revealSystemEventsInWindow(chatId) {
    const buffer = _systemEventBuffer.get(chatId);
    if (!buffer || !buffer.length) return [];

    // Lower bound of the loaded window = oldest real (non-system) message in
    // the cache. Below that, messages haven't been paged in yet, so their
    // system events stay hidden. Once every message is loaded, the bound drops
    // away and the remaining (oldest) system events reveal too.
    const stats = eventCache.getStats(chatId);
    let bound = -Infinity;
    if (!stats?.isFullyLoaded) {
        const loaded = eventCache.getEvents(chatId) || [];
        let oldestReal = Infinity;
        for (const m of loaded) {
            if (!m.system_event && m.at < oldestReal) oldestReal = m.at;
        }
        if (oldestReal !== Infinity) bound = oldestReal;
    }

    const revealed = [];
    for (const sm of buffer) {
        if (sm.at >= bound && eventCache.addEvent(chatId, sm)) {
            revealed.push(sm);
        }
    }
    return revealed;
}

/**
 * Show/hide the "start of the channel" marker for an empty Community (no messages or system events
 * yet). Discord-style placeholder so a fresh/quiet community never opens to a blank void; removed
 * the moment any content (a message or a "X joined" event) lands. Idempotent — safe to call on every
 * render/content change for the open chat.
 */
function refreshChatEmptyState() {
    const existing = document.getElementById('chat-empty-state');
    const chat = strOpenChat ? arrChats.find(c => c.id === strOpenChat) : null;
    const isEmptyCommunity = !!chat && chat.chat_type === 'Community' && (!chat.messages || chat.messages.length === 0);
    if (isEmptyCommunity) {
        if (!existing && domChatMessages) {
            const name = chat.metadata?.custom_fields?.name || 'this community';
            const el = document.createElement('div');
            el.id = 'chat-empty-state';
            el.className = 'chat-empty-state';
            // .icon is position:absolute → wrap it in a relative, sized span (same pattern as elsewhere).
            el.innerHTML = `<div class="chat-empty-state-icon"><span class="icon icon-users-multi"></span></div>`
                + `<h3>Welcome to ${escapeHtml(name)}</h3>`
                + `<p>This is the very beginning of the channel — say hello! 👋</p>`;
            domChatMessages.appendChild(el);
        }
    } else if (existing) {
        existing.remove();
    }
}

async function openChat(contact) {
    pushBack('chat', closeChat);
    // Abandon a wallpaper preview staged in a different chat so its edit
    // overlay doesn't leak onto this header.
    if (wallpaperPreviewState && wallpaperPreviewState.chatId !== contact) {
        const stale = wallpaperPreviewState;
        wallpaperPreviewState = null;
        setWallpaperPreviewBarVisible(false);
        invoke('cancel_wallpaper_preview', { chatId: stale.chatId }).catch(() => {});
    }
    // Display the Chat UI
    navbarSelect('chat-btn');
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChatNew.style.display = 'none';
    domChats.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domChat.style.display = '';
    // Match the fade transition the navbar/account tabs use for visual cohesion.
    domChat.classList.add('fadein-anim');
    domChat.addEventListener('animationend', () => domChat.classList.remove('fadein-anim'), { once: true });
    domSettingsBtn.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = `none`;

    // Clear existing messages so they're fully re-rendered (picks up state changes like blocking)
    domChatMessages.innerHTML = '';
    // Only reset revealed blocked messages when switching to a different chat
    if (strOpenChat !== contact) revealedBlockedMessages.clear();

    // Warm up GIF server connection early (non-blocking)
    preconnectGifServer();

    // Get the chat (could be DM or Group)
    const chat = arrChats.find(c => c.id === contact);
    // A community channel id with no chat = a community we no longer hold (e.g. torn down mid-open by a
    // removal). Bail to the chatlist rather than render a phantom — updateChat(undefined) would throw and
    // jam the render loop. (DMs legitimately open with no prior chat, so this only guards channel ids.)
    if (!chat && !contact.startsWith('npub1')) {
        console.warn('openChat: no chat for community channel, returning to list:', contact);
        return openChatlist();
    }
    const isGroup = chatIsGroup(chat);
    const profile = !isGroup ? getProfile(contact) : null;
    strOpenChat = contact;
    // Snapshot last_read BEFORE the open-time markAsRead — the divider needs
    // the stale value to find the boundary, but we still want to advance
    // chat.last_read so the OS badge clears immediately on entering the chat.
    const lastReadOnOpen = chat?.last_read || '';
    if (chat?.messages?.length) {
        const latestNonMine = findLatestContactMessage(chat.messages);
        if (latestNonMine) markAsRead(chat, latestNonMine);
    }

    // Apply the chat's wallpaper to the layer before any messages render,
    // so the first paint already shows the bg + filter.
    applyChatWallpaper(contact, chat?.wallpaper_path || '', chat?.wallpaper_blur, chat?.wallpaper_dim, chat?.wallpaper_ts);

    // Render the header SYNCHRONOUSLY using whatever in-memory data we have,
    // so the user sees the contact name + avatar the instant the chat panel
    // appears — no more black flash while async cache/DB loads run.
    setChatHeader(chat, profile, isGroup, contact === strPubkey);

    // Pre-paint: synchronously render in-memory messages so the chat has
    // content the moment the panel reveals. The subsequent eventCache load
    // layers any newer/older messages on top via updateChat's dedup guard
    // (each msg id is checked against the existing DOM before re-rendering).
    if (chat?.messages?.length) {
        const preBatch = chat.messages.slice(-proceduralScrollState.messagesPerBatch);
        // Fire-and-forget — updateChat's DOM building is synchronous; only
        // its mark-as-read tail is async, which we don't need to wait on.
        updateChat(chat, preBatch, profile, false);
    }

    // Queue profile sync for DMs (on-demand refresh when opening)
    if (!isGroup && contact) {
        invoke('queue_profile_sync', {
            npub: contact,
            priority: 'high',
            forceRefresh: false
        }).catch(err => console.error('Failed to queue DM profile sync:', err));
    }

    // Clear any existing auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Record when the chat was opened
    chatOpenTimestamp = Date.now();

    // Chat-open paths render the latest messages and scroll-to-bottom,
    // so the user starts pinned. Reset here in case the previous chat
    // was scrolled up. Also wipes the unread badge + divider from the
    // prior chat — re-entering a chat counts as "I've read up to here".
    chatPinnedToBottom = true;
    clearUnreadBelow();
    clearUnreadDivider();
    syncBackendActiveChat();

    // After 100ms, stop auto-scrolling on media loads
    chatOpenAutoScrollTimer = setTimeout(() => {
        chatOpenTimestamp = 0; // Reset timestamp to disable auto-scrolling
        chatOpenAutoScrollTimer = null;
    }, 100);

    // Load events from cache (on-demand loading)
    // This uses the LRU event cache for efficient memory management
    // Load events from cache (will fetch from DB if not cached)
    const initialMessages = await eventCache.loadInitialEvents(
        contact,
        proceduralScrollState.messagesPerBatch
    );

    // Merge any historical PIVX payments — helper in pivx.js
    await mergePivxPaymentsIntoChat(contact, initialMessages);

    // No on-open Community sync: NIP-17-parity means catch-up happens at boot (sync_communities_boot)
    // and on relay reconnect, with realtime delivering everything in between. Opening a channel reads
    // DB history; older pages load on scroll-up.

    // Load system events (wallpaper/membership changes). They're fetched in
    // full but buffered — only the ones inside the initially-loaded message
    // window are revealed now; the rest surface as the user scrolls older
    // messages into view (same on-demand windowing as messages).
    try {
        const systemEvents = await invoke('get_system_events', { conversationId: contact });
        const buffer = (systemEvents || [])
            .filter(event => !initialMessages.find(m => m.id === event.id))
            .map(event => ({
                id: event.id,
                at: event.at,
                // Rebuild from the actor's CURRENT cached name rather than the npub-baked
                // stored content. Fetch unknown profiles so the next open resolves them.
                content: (() => {
                    const np = event.member_npub;
                    if (np && !arrProfiles.some(p => p.id === np) && !strangerProfileRequested.has(np)) {
                        strangerProfileRequested.add(np);
                        invoke('load_profile', { npub: np }).catch(() => {});
                    }
                    return systemEventContent(event.event_type, np);
                })(),
                mine: false,
                attachments: [],
                system_event: {
                    event_type: event.event_type,
                    member_npub: event.member_npub,
                },
            }))
            .sort((a, b) => a.at - b.at);
        _systemEventBuffer.set(contact, buffer);
        // revealSystemEventsInWindow adds into the cache array, which is
        // aliased to initialMessages — re-sort so the pre-paint renders them
        // in chronological order.
        if (revealSystemEventsInWindow(contact).length > 0) {
            initialMessages.sort((a, b) => a.at - b.at);
        }
    } catch (e) {
        console.warn('Failed to load system events:', e);
    }

    // Get cache stats for procedural scroll
    const cacheStats = eventCache.getStats(contact);
    const totalMessages = cacheStats?.totalInDb || initialMessages.length;

    // Update the chat object's messages array for compatibility
    // (Some parts of the code still reference chat.messages)
    if (chat) {
        chat.messages = initialMessages;
    }

    // Initialize procedural scroll state with actual counts
    initProceduralScrollWithCache(contact, initialMessages.length, totalMessages);
    
    await updateChat(chat, initialMessages, profile, true);
    refreshChatEmptyState(); // empty community → show the "start of channel" marker

    // Drop a "New" divider above the first non-mine message after
    // `last_read`. Only fires when last_read matches a loaded message —
    // stale markers (id drift, deleted msg) would otherwise stick the
    // divider above the latest contact on every reopen.
    if (initialMessages.length > 0 && lastReadOnOpen) {
        const idx = initialMessages.findIndex(m => m.id === lastReadOnOpen);
        if (idx >= 0) {
            let firstUnread = null;
            for (let i = idx + 1; i < initialMessages.length; i++) {
                const m = initialMessages[i];
                if (m.system_event) continue;
                if (!m.mine) { firstUnread = m; break; }
            }
            if (firstUnread) {
                const node = document.getElementById(firstUnread.id);
                if (node) insertUnreadDivider(node);
            }
        }
    }

    // If the user is blocked (DM only), disable the chat input and show a system message
    const isBlockedChat = !isGroup && profile?.is_blocked;
    // Dissolved community: the backend seals it (no new events accepted), so disable the
    // composer the same way a blocked DM does and mark the timeline's end.
    const isDissolvedChat = chatIsDissolved(chat);
    // Remove any previous blocked / dissolved notice before (re-)evaluating
    document.getElementById('blocked-notice')?.remove();
    document.getElementById('dissolved-notice')?.remove();
    if (isBlockedChat) {
        domChatMessageInput.disabled = true;
        domChatMessageInput.placeholder = 'Unblock to send messages';
        domChatMessageInput.style.paddingLeft = '15px';
        domChatMessageInputFile.style.display = 'none';
        domChatMessageInputVoice.style.display = 'none';
        domChatMessageInputEmoji.style.display = 'none';
        // Append a system-style blocked notice at the bottom of the chat
        const blockedNotice = insertSystemEvent('Blocked — You won\'t receive new messages from them');
        blockedNotice.id = 'blocked-notice';
        blockedNotice.style.marginBottom = '20px';
        domChatMessages.appendChild(blockedNotice);
    } else if (isDissolvedChat) {
        applyDissolvedChatUI(chat);
    } else {
        domChatMessageInput.disabled = false;
        domChatMessageInput.placeholder = 'Enter message...';
        domChatMessageInput.style.paddingLeft = '';
        domChatMessageInputFile.style.display = '';
        domChatMessageInputVoice.style.display = '';
        domChatMessageInputEmoji.style.display = '';
    }

    // last_read is not advanced on open — the divider needs the stale value
    // to anchor above the first missed message. closeChat / msg-while-pinned
    // / onFocusChanged are the catch-up signals.

    // Update the back button notification dot
    updateChatBackNotification();

    // Focus chat input on desktop (mobile keyboards are intrusive)
    if (!platformFeatures.is_mobile && !isBlockedChat && !isDissolvedChat) {
        domChatMessageInput.focus();
    }

    // Inbound share: if the user picked this chat to share into, attach now.
    if (pendingShareToSend) {
        const share = pendingShareToSend;
        pendingShareToSend = null;
        if (share.uris && share.uris.length) {
            if (share.uris.length > 1) {
                console.warn(`[Share] ${share.uris.length} files shared; sending the first (multi-file is a follow-up)`);
            }
            // content:// URIs are read + cached immediately by openFilePreview,
            // then the user captions/confirms the send in the preview UI.
            openFilePreview(share.uris[0], contact, '').catch(e => console.error('[Share] preview failed:', e));
        } else if (share.text) {
            domChatMessageInput.value = share.text;
            domChatMessageInput.focus();
        }
    }
}

/** Inbound share awaiting a chat selection (set when another app shares into Vector). */
let pendingShareToSend = null;

/**
 * Handle a file/text share received from another app. Drops the user into the
 * chat list; opening a chat then attaches the share (see openChat tail).
 */
async function handleIncomingShare(payload) {
    if (!payload || ((!payload.uris || !payload.uris.length) && !payload.text)) return;
    pendingShareToSend = payload;
    // If a chat was left open (e.g. the app was backgrounded mid-chat, then
    // foregrounded by the share), tear it down — closeChat() returns to the
    // list. Otherwise just show the list. Either way we land on a clean chat
    // list for picking a destination.
    if (strOpenChat) {
        await closeChat();
    } else {
        await openChatlist();
    }
    showToast('Choose a chat to forward to');
}

/**
 * Open the dialog for starting a new chat
 */
function openNewChat() {
    pushBack('new-chat', closeChat);
    // Display the UI
    domChatNew.style.display = '';
    domChats.style.display = 'none';
    domChat.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = 'none';
}

/**
 * Closes the current chat, taking the user back to the chat list
 */
async function closeChat() {
    popBack('chat');
    popBack('new-chat');
    // Stop all audio engine playback (voice messages, music, etc.)
    invoke('audio_stop_all').catch(() => {});

    // Clear any auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Attempt to completely release memory (force garbage collection...) of in-chat media
    while (domChatMessages.firstElementChild) {
        const domChild = domChatMessages.firstElementChild;

        // For media (images, audio, video); we ensure they're fully unloaded
        const domMedias = domChild?.querySelectorAll('img, audio, video');
        for (const domMedia of domMedias) {
            // Streamable media (audio + video) should be paused, then force-unloaded
            if (domMedia instanceof HTMLMediaElement) {
                domMedia.pause();
                domMedia.removeAttribute('src'); // Better than setting to empty string
                domMedia.load();
            }
            // Static media (images) should simply be unloaded
            if (domMedia instanceof HTMLImageElement) {
                if (domMedia.src.startsWith('blob:')) {
                    URL.revokeObjectURL(domMedia.src);
                }
                domMedia.removeAttribute('src');
            }
        }

        // Now we explicitly drop them
        domChild.remove();
    }

    // Only catch up last_read on close when the user is actually at the
    // bottom. Marking on close while scrolled up would lie about messages the
    // user never scrolled down to see — the OS badge has to stay accurate.
    if (strOpenChat && chatPinnedToBottom) {
        const closedChat = arrChats.find(c => c.id === strOpenChat);
        if (closedChat?.messages?.length) {
            const lastContactMsg = findLatestContactMessage(closedChat.messages);
            if (lastContactMsg) {
                markAsRead(closedChat, lastContactMsg);
            }
        }
    }

    // Drop the divider ref so the next openChat starts from a clean slate.
    clearUnreadDivider();

    // Drop any in-flight wallpaper preview so the new chat doesn't inherit
    // a stale confirm bar or a foreign preview file on the background.
    if (wallpaperPreviewState) {
        const stalePreview = wallpaperPreviewState;
        wallpaperPreviewState = null;
        setWallpaperPreviewBarVisible(false);
        invoke('cancel_wallpaper_preview', { chatId: stalePreview.chatId })
            .catch(() => { /* best-effort cleanup */ });
    }
    // Strip the wallpaper from the layer so it doesn't flash through
    // during the chat list re-render.
    applyChatWallpaper(strOpenChat, '', 0, 100);

    // Trim the event cache for this chat to free memory
    // (keeps max 100 events, removes older ones loaded during scroll)
    if (strOpenChat) {
        eventCache.trimConversation(strOpenChat);
    }

    // Reset the chat UI
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domSettingsBtn.style.display = '';
    domChatNew.style.display = 'none';
    domChat.style.display = 'none';
    strOpenChat = "";
    previousChatBeforeProfile = ""; // Clear when closing chat
    nLastTypingIndicator = 0;
    syncBackendActiveChat();
    
    // Clear the chat header to prevent flicker when opening next chat
    domChatContact.textContent = '';
    domChatContactStatus.textContent = '';
    domChatContactStatus.classList.add('status-hidden');
    domChatContactStatus.classList.remove('typing-indicator-text');
    domChatHeaderAvatarContainer.innerHTML = '';
    
    // Reset procedural scroll state
    resetProceduralScroll();
    
    // Hide the back button notification dot when closing chat
    updateChatBackNotification();

    // Display the Navbar
    domNavbar.style.display = ``;

    // Cancel any ongoing replies or selections
    strCurrentReactionReference = "";
    strCurrentReplyReference = "";
    cancelReply();

    // Navigate back to chat list with animation
    await openChatlist();

    // Update the Chat List
    renderChatlist();

    // Ensure the chat list re-adjusts to fit
    adjustSize();
}

/**
 * Open the Expanded Profile view, optionally with a non-default profile
 * @param {Profile} cProfile - An optional profile to render
 */
async function openProfile(cProfile) {
    pushBack('profile', () => {
        domProfile.style.display = 'none';
        if (previousChatBeforeProfile) openChat(previousChatBeforeProfile);
        else openChatlist();
    });
    navbarSelect('profile-btn');
    domNavbar.style.display = '';
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    domChat.style.display = 'none'; // Hide the chat view when opening profile
    domSettingsBtn.style.display = ''; // Ensure settings button is visible (may have been hidden by openChat)

    // Scroll profile back to top
    setTimeout(() => {
        document.getElementById('profile')?.scrollTo(0, 0);
        document.querySelector('.profile-content')?.scrollTo(0, 0);
    }, 50);

    // Render our own profile by default, but otherwise; the given one
    if (!cProfile) {
        cProfile = arrProfiles.find(a => a.mine);
        // Clear previous chat when opening our own profile from navbar
        previousChatBeforeProfile = '';
    }

    // Force immediate refresh when user views profile
    if (cProfile && cProfile.id) {
        invoke("refresh_profile_now", { npub: cProfile.id });

        // Start periodic refresh while viewing this profile (every 30 seconds)
        clearInterval(profileRefreshInterval);
        profileRefreshInterval = setInterval(() => {
            // Only refresh if profile tab is still open
            if (domProfile.style.display === '') {
                invoke("refresh_profile_now", { npub: cProfile.id });
            } else {
                // Profile tab closed, stop refreshing
                clearInterval(profileRefreshInterval);
                profileRefreshInterval = null;
            }
        }, 30000);
    }

    renderProfileTab(cProfile);

    if (domProfile.style.display !== '') {
        // Run a subtle fade-in animation
        domProfile.classList.add('fadein-subtle-anim');
        domProfile.addEventListener('animationend', () => domProfile.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domProfile.style.display = '';
    }
}

/**
 * Open the Group Overview view for a specific group chat
 * @param {Chat} chat - The group chat object
 */
async function openGroupOverview(chat) {
    if (!chat || !chatIsGroup(chat)) return;

    pushBack('group-overview', () => {
        domGroupOverview.style.display = 'none';
        domGroupOverview.removeAttribute('data-group-id');
        openChat(chat.id);
    });

    navbarSelect('chat-btn');
    domNavbar.style.display = 'none';
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChat.style.display = 'none';

    // Store which group is being viewed
    domGroupOverview.setAttribute('data-group-id', chat.id);

    // Only Communities are group-like now (chatIsGroup gate above), so render the
    // Community overview (no member roster; invite by link/npub).
    await renderCommunityOverview(chat);

    if (domGroupOverview.style.display !== '') {
        // Run a subtle fade-in animation
        domGroupOverview.classList.add('fadein-subtle-anim');
        domGroupOverview.addEventListener('animationend', () => domGroupOverview.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domGroupOverview.style.display = '';
    }
}


/**
 * Render the overview panel for a Community channel. Reuses the group-overview DOM, but:
 * editable name/avatar/description (owner only) via the Community commands, NO member
 * roster (membership is hidden), and an invite panel offering a shareable link + by-npub.
 * @param {Chat} chat - The Community channel chat
 */
/**
 * Silently tear the local UI down for a community that's gone (the involuntary KICK path; the backend has
 * already dropped its keys + DB rows). Closes the open channel if it belongs to the community, removes its
 * channels from the chat list + overview, and re-renders. Voluntary leave keeps its own inline teardown.
 */
async function removeCommunityFromUI(communityId) {
    const ids = new Set(
        arrChats.filter(c => c.metadata?.custom_fields?.community_id === communityId).map(c => c.id)
    );
    const wasViewing = ids.has(strOpenChat);
    if (wasViewing) {
        await closeChat();
        domGroupOverview.style.display = 'none';
        domGroupOverview.removeAttribute('data-group-id');
    }
    arrChats = arrChats.filter(c => c.metadata?.custom_fields?.community_id !== communityId);
    renderChatlist();
    if (wasViewing) openChatlist();
}

async function renderCommunityOverview(chat) {
    const cf = chat.metadata?.custom_fields || {};
    const communityId = cf.community_id;
    const isOwner = cf.is_owner === 'true';
    // Role-engine capabilities (NOT an owner check — the owner is just the top role). Each management
    // affordance gates on the matching bit, so an admin whose role carries a permission sees the same
    // button as the owner. Falls back to no-caps on error (hide everything management).
    let caps = {};
    try { caps = await invoke('get_community_capabilities', { communityId }); } catch (_) {}
    // Tag the overview with its community so the realtime `community_refreshed` listener knows to re-render
    // it when a control change (ban/role/metadata/mode) lands live.
    domGroupOverview.setAttribute('data-group-id', communityId);
    const name = cf.name || `Community ${chat.id.substring(0, 10)}...`;
    const description = cf.description || '';

    // Header: name + member count as subtext (the description has its own block below). Shows the
    // cached count instantly; the member fetch further down refreshes it.
    domGroupOverviewName.textContent = name;
    domGroupOverviewStatus.textContent = communityMemberSubtext(communityId);

    const headerAvatarContainer = document.getElementById('group-overview-header-avatar-container');
    if (headerAvatarContainer) {
        headerAvatarContainer.innerHTML = '';
        const src = chat.metadata?.avatar_cached ? convertFileSrc(chat.metadata.avatar_cached) : null;
        headerAvatarContainer.appendChild(createAvatarImg(src, 22, true));
    }

    // Large center avatar (+ owner edit overlay).
    const avatarParent = domGroupOverviewAvatar.parentElement;
    const avatarSrc = chat.metadata?.avatar_cached ? convertFileSrc(chat.metadata.avatar_cached) : null;
    const prevImg = avatarParent.querySelector('img');
    if (prevImg) prevImg.remove();
    if (avatarSrc) {
        const img = document.createElement('img');
        img.src = avatarSrc;
        img.style.cssText = 'width:100px;height:100px;object-fit:cover;border-radius:50%;';
        img.onerror = () => { img.replaceWith(domGroupOverviewAvatar); domGroupOverviewAvatar.style.display = 'inline-block'; };
        domGroupOverviewAvatar.style.display = 'none';
        avatarParent.appendChild(img);
    } else {
        domGroupOverviewAvatar.style.display = 'inline-block';
    }
    const prevOverlay = avatarParent.querySelector('.group-avatar-edit-overlay');
    if (prevOverlay) prevOverlay.remove();
    if (caps.manage_metadata) {
        const overlay = document.createElement('div');
        overlay.className = 'group-avatar-edit-overlay';
        overlay.innerHTML = '<span class="icon icon-edit" style="width:16px;height:16px;background-color:#fff;"></span>';
        overlay.onclick = async (e) => {
            e.stopPropagation();
            try {
                const { open } = window.__TAURI__.dialog;
                const selected = await open({ multiple: false, filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] }] });
                const filePath = typeof selected === 'string' ? selected : selected?.path;
                if (!filePath) return;
                overlay.style.opacity = '0.5';
                await invoke('set_community_image', { communityId, filepath: filePath, isBanner: false });
                cf.icon = '1';
                const cachedPath = await invoke('cache_community_image', { communityId, isBanner: false });
                if (cachedPath) chat.metadata.avatar_cached = cachedPath;
                await renderCommunityOverview(chat);
                renderChatlist();
            } catch (err) {
                console.error('Failed to set community image:', err);
                overlay.style.opacity = '';
                showToast('Failed to update the image');
            }
        };
        avatarParent.appendChild(overlay);
    }

    // Mute button (same as groups).
    const domGroupMuteBtn = document.getElementById('group-mute-btn');
    if (domGroupMuteBtn) {
        const updateMuteBtn = (muted) => {
            domGroupMuteBtn.querySelector('span').className = `icon icon-volume-${muted ? 'mute' : 'max'} navbar-icon`;
            domGroupMuteBtn.querySelector('p').innerText = muted ? 'Unmute' : 'Mute';
        };
        updateMuteBtn(chat.muted);
        domGroupMuteBtn.onclick = async () => { updateMuteBtn(await invoke('toggle_chat_mute', { chatId: chat.id })); };
    }

    // Editable name (owner only).
    domGroupOverviewNameSecondary.textContent = name;
    if (caps.manage_metadata) {
        domGroupOverviewNameSecondary.classList.add('group-editable');
        domGroupOverviewNameSecondary.onclick = () => {
            const input = document.createElement('input');
            input.type = 'text'; input.className = 'group-name-input'; input.value = name; input.maxLength = 100;
            domGroupOverviewNameSecondary.replaceWith(input);
            input.focus(); input.select();
            let saved = false;
            const save = async () => {
                if (saved) return; saved = true;
                const newName = input.value.trim();
                input.replaceWith(domGroupOverviewNameSecondary);
                if (newName && newName !== name) {
                    domGroupOverviewNameSecondary.textContent = newName;
                    domGroupOverviewName.textContent = newName;
                    cf.name = newName;
                    try { await invoke('update_community_metadata', { communityId, name: newName, description: null }); renderChatlist(); }
                    catch (e) {
                        console.error('Failed to rename community:', e);
                        // Revert the optimistic header text.
                        cf.name = name;
                        domGroupOverviewNameSecondary.textContent = name;
                        domGroupOverviewName.textContent = name;
                        showToast('Failed to update the name');
                    }
                }
            };
            input.onblur = save;
            input.onkeydown = (e) => { if (e.key === 'Enter') { e.preventDefault(); input.blur(); } if (e.key === 'Escape') { saved = true; input.replaceWith(domGroupOverviewNameSecondary); } };
        };
    } else {
        domGroupOverviewNameSecondary.classList.remove('group-editable');
        domGroupOverviewNameSecondary.onclick = null;
    }

    // Editable description (anyone with manage-metadata); shown to everyone if set.
    domGroupOverviewDescription.style.display = (description || caps.manage_metadata) ? '' : 'none';
    domGroupOverviewDescription.textContent = description || (caps.manage_metadata ? 'Add a description...' : '');
    domGroupOverviewDescription.classList.toggle('group-placeholder', !description && caps.manage_metadata);
    if (caps.manage_metadata) {
        domGroupOverviewDescription.classList.add('group-editable');
        domGroupOverviewDescription.onclick = () => {
            const input = document.createElement('textarea');
            input.className = 'group-name-input'; input.value = description; input.maxLength = 500; input.rows = 2;
            domGroupOverviewDescription.replaceWith(input);
            input.focus();
            let saved = false;
            const save = async () => {
                if (saved) return; saved = true;
                const newDesc = input.value.trim();
                input.replaceWith(domGroupOverviewDescription);
                if (newDesc !== description) {
                    cf.description = newDesc;
                    domGroupOverviewDescription.textContent = newDesc || (caps.manage_metadata ? 'Add a description...' : '');
                    domGroupOverviewDescription.classList.toggle('group-placeholder', !newDesc);
                    domGroupOverviewStatus.textContent = newDesc;
                    try { await invoke('update_community_metadata', { communityId, name: null, description: newDesc }); }
                    catch (e) { console.error('Failed to update community description:', e); showToast('Failed to update the description'); }
                }
            };
            input.onblur = save;
            input.onkeydown = (e) => { if (e.key === 'Escape') { saved = true; input.replaceWith(domGroupOverviewDescription); } };
        };
    } else {
        domGroupOverviewDescription.classList.remove('group-editable');
        domGroupOverviewDescription.onclick = null;
    }

    // Member list = observed participants (best-effort): everyone who has posted across the
    // Community's channels. Lurkers and link-joiners who haven't spoken don't appear (membership
    // isn't authoritative). Join announcements (presence) surface here too once that ships.
    if (domGroupOverviewMembers) {
        const membersEl = domGroupOverviewMembers;
        const searchEl = domGroupMemberSearchInput;
        const myNpub = arrProfiles.find(p => p.mine)?.id;
        const ownerNpub = cf.owner_npub || null; // PROVEN owner (verified attestation), or null
        let memberList = [];
        try { memberList = await invoke('get_community_members', { communityId }); } catch (_) {}
        // Cache the count for the header/overview subtext (the overview's own authoritative fetch).
        communityMemberCounts.set(communityId, memberList.length);
        _communityCountLastFetch.set(communityId, Date.now());
        domGroupOverviewStatus.textContent = communityMemberSubtext(communityId);
        // Admins (members holding a management role) drive the gold crown. MVP: the OWNER elects /
        // removes admins (no role hierarchy yet, so that's the only real promotion path). The
        // backend authorizes on the MANAGE_ROLES permission (futureproof — `can_manage_community_roles`);
        // the UI just exposes the toggle to the owner for now.
        let adminNpubs = [];
        try { adminNpubs = await invoke('get_community_admins', { communityId }); } catch (_) {}
        // Cache admins onto this community's channel chats so message rendering can chip @everyone
        // from admin senders (owner is handled separately via owner_npub). Mirrors the group design.
        for (const c of arrChats) {
            if (c.chat_type === 'Community' && c.metadata?.custom_fields?.community_id === communityId) {
                c.metadata.admins = adminNpubs.slice();
            }
        }
        const iAmOwner = !!(myNpub && ownerNpub && myNpub === ownerNpub);
        // Per-member outrank for moderation, expressed in role-engine POSITIONS (owner = pos 0 via
        // ownerNpub, admin = pos 1 via adminNpubs, member = none). You may moderate a target you outrank:
        // the owner outranks everyone; an admin outranks only non-admins. The backend re-verifies the real
        // can_act_on_member, so this is just which buttons to show (best-effort, never authoritative).
        const iOutrank = (npub) => {
            if (npub === ownerNpub || npub === myNpub) return false;     // never the owner, never yourself
            if (iAmOwner) return true;                                   // pos 0 outranks all
            return !adminNpubs.includes(npub);                           // an admin outranks only non-admins
        };
        // The banlist (for the unban list + to hide banned members), shown to anyone who can BAN.
        let bannedList = [];
        if (caps.ban) { try { bannedList = await invoke('get_community_banlist', { communityId }); } catch (_) {} }

        const renderMembers = (filterText = '') => {
            const f = (filterText || '').trim().toLowerCase();
            const frag = document.createDocumentFragment();
            let shown = 0;
            // Hoist tiers: owner → admins → members. Within a tier, sort alphabetically by display
            // (nickname → name → npub), so rows read A→Z inside each tier.
            const tierOf = (npub) => npub === ownerNpub ? 0 : (adminNpubs.includes(npub) ? 1 : 2);
            const displayOf = (m) => {
                const profile = arrProfiles.find(p => p.id === m.npub) || null;
                const name = profile ? (profile.nickname || profile.name || '') : '';
                return name || (m.npub.substring(0, 10) + '...' + m.npub.substring(m.npub.length - 6));
            };
            const ordered = [...memberList].sort((a, b) =>
                (tierOf(a.npub) - tierOf(b.npub)) ||
                displayOf(a).toLowerCase().localeCompare(displayOf(b).toLowerCase()));
            for (const m of ordered) {
                const isCommunityOwner = m.npub === ownerNpub;
                const profile = arrProfiles.find(p => p.id === m.npub) || null;
                const name = profile ? (profile.nickname || profile.name || '') : '';
                const display = name || (m.npub.substring(0, 10) + '...' + m.npub.substring(m.npub.length - 6));
                if (f && !(display + ' ' + m.npub).toLowerCase().includes(f)) continue;
                // Reuse the existing member-row design (display-only: no selection indicator).
                const row = document.createElement('div');
                row.className = 'member-pick-row';
                const bg = document.createElement('div');
                bg.className = 'member-pick-hover';
                row.appendChild(bg);
                row.addEventListener('mouseenter', () => {
                    const c = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
                    bg.style.background = `linear-gradient(to right, ${c}40, transparent)`;
                });
                // LEFT crown slot (fixed width so avatars always align): gold crown for the owner,
                // green for admins (matches the in-chat tags); a promotable member shows a faint crown
                // on row-hover; everyone else gets the empty spacer. Clicking toggles the Admin role.
                const isAdminMember = adminNpubs.includes(m.npub);
                const crownSlot = document.createElement('div');
                crownSlot.className = 'member-crown-slot';
                const buildCrown = (active, promote) => {
                    const c = document.createElement('div');
                    c.className = 'member-pick-admin' + (active ? ' active' : '') + (promote ? ' promote' : '');
                    c.innerHTML = '<span class="icon icon-crown"></span>';
                    return c;
                };
                if (isCommunityOwner) {
                    const crown = buildCrown(true, false);
                    crown.classList.add('owner-crown'); // gold; admins stay green
                    crown.title = 'Owner';
                    crown.style.cursor = 'default';
                    crownSlot.appendChild(crown);
                } else if (isAdminMember || caps.manage_admin_role) {
                    // Admin → green (someone who can manage the @admin role can click to demote). Plain
                    // member → faint hover-crown they can click to promote. (manage_admin_role is the
                    // position rule: outrank the @admin role; owner-only in the MVP, but not hardcoded.)
                    const crown = buildCrown(isAdminMember, !isAdminMember && caps.manage_admin_role);
                    if (caps.manage_admin_role) {
                        crown.title = isAdminMember ? 'Remove admin' : 'Make admin';
                        crown.style.cursor = 'pointer';
                        crown.onclick = async (e) => {
                            e.stopPropagation();
                            const makeAdmin = !isAdminMember;
                            const confirmed = await popupConfirm(
                                makeAdmin ? 'Make Admin' : 'Remove Admin',
                                makeAdmin
                                    ? `Make <b>${escapeHtml(display)}</b> an admin? They'll be able to moderate this community (ban, hide messages, manage settings).`
                                    : `Remove <b>${escapeHtml(display)}</b> as an admin? They'll lose all moderation powers.`,
                                false, '', 'vector_warning.svg');
                            if (!confirmed) return;
                            // Spinner on the crown through the publish + broadcast wait.
                            crown.classList.add('active');
                            crown.style.pointerEvents = 'none';
                            crown.innerHTML = '<span class="icon icon-loading spin"></span>';
                            try {
                                await invoke(makeAdmin ? 'grant_community_admin' : 'revoke_community_admin', { communityId, npub: m.npub });
                                if (makeAdmin) { if (!adminNpubs.includes(m.npub)) adminNpubs.push(m.npub); }
                                else { adminNpubs = adminNpubs.filter(n => n !== m.npub); }
                                renderMembers(searchEl?.value || '');
                            } catch (err) {
                                crown.style.pointerEvents = '';
                                crown.innerHTML = '<span class="icon icon-crown"></span>';
                                crown.classList.toggle('active', isAdminMember);
                                showToast(String(err));
                            }
                        };
                    } else {
                        crown.title = 'Admin';
                        crown.style.cursor = 'default';
                    }
                    crownSlot.appendChild(crown);
                }
                row.appendChild(crownSlot);
                const avatar = createAvatarImg(profile ? getProfileAvatarSrc(profile) : null, 25, false);
                avatar.className = 'member-pick-avatar';
                row.appendChild(avatar);
                const nameSpan = document.createElement('div');
                nameSpan.className = 'compact-member-name';
                nameSpan.textContent = display;
                if (name) twemojify(nameSpan);
                row.appendChild(nameSpan);
                // Role-engine moderation: shown to anyone who holds KICK/BAN AND outranks this member
                // (the owner outranks all; an admin outranks non-admins — never the owner, never self).
                // Two tiers (§7 escalation ladder): KICK is cooperative + soft (they self-remove, can
                // rejoin with a new invite); BAN is forceful (suppressed + read-cut in a private community).
                if (iOutrank(m.npub) && (caps.kick || caps.ban)) {
                    const actions = document.createElement('div');
                    actions.className = 'member-pick-actions';

                    if (caps.kick) {
                    const kickBtn = document.createElement('button');
                    kickBtn.className = 'cmt-btn cmt-btn-sm cmt-btn-secondary';
                    kickBtn.title = 'Kick (they can rejoin with a new invite)';
                    kickBtn.innerHTML = '<span class="icon icon-x"></span>Kick';
                    kickBtn.onclick = async (e) => {
                        e.stopPropagation();
                        const confirmed = await popupConfirm('Kick member', `Kick <b>${escapeHtml(display)}</b>? They'll be removed from the community but can rejoin with a new invite.`, false, '', 'vector_warning.svg');
                        if (!confirmed) return;
                        kickBtn.disabled = true;
                        kickBtn.innerHTML = '<span class="icon icon-loading spin"></span>Kicking';
                        try {
                            await invoke('kick_community_member', { communityId, npub: m.npub });
                            memberList = memberList.filter(x => x.npub !== m.npub);
                            renderMembers(searchEl?.value || '');
                        } catch (err) {
                            kickBtn.disabled = false;
                            kickBtn.innerHTML = '<span class="icon icon-x"></span>Kick';
                            showToast(String(err));
                        }
                    };
                    actions.appendChild(kickBtn);
                    }

                    if (caps.ban) {
                    const banBtn = document.createElement('button');
                    banBtn.className = 'cmt-btn cmt-btn-sm cmt-btn-danger';
                    banBtn.title = 'Ban from community';
                    banBtn.innerHTML = '<span class="icon icon-x-user"></span>Ban';
                    banBtn.onclick = async (e) => {
                        e.stopPropagation();
                        const confirmed = await popupConfirm('Ban member', `Ban <b>${escapeHtml(display)}</b>? They'll be removed from the community and can't rejoin unless you unban them.`, false, '', 'vector_warning.svg');
                        if (!confirmed) return;
                        // Banning publishes to relays + rebuilds the subscription (a few seconds);
                        // show a spinner so the button isn't dead during the wait.
                        banBtn.disabled = true;
                        banBtn.innerHTML = '<span class="icon icon-loading spin"></span>Banning';
                        try {
                            await invoke('ban_community_member', { communityId, npub: m.npub });
                            memberList = memberList.filter(x => x.npub !== m.npub);
                            renderMembers(searchEl?.value || '');
                        } catch (err) {
                            banBtn.disabled = false;
                            banBtn.innerHTML = '<span class="icon icon-x-user"></span>Ban';
                            // A private-community ban can fail with the (long, important) bunker read-cut
                            // explanation — show it as a persistent notice, not a fleeting toast.
                            await popupConfirm("Couldn't ban", escapeHtml(String(err)), true, '', 'vector_warning.svg');
                        }
                    };
                    actions.appendChild(banBtn);
                    }
                    row.appendChild(actions);
                }
                frag.appendChild(row);
                shown++;
            }
            membersEl.innerHTML = '';
            if (!shown) {
                const empty = document.createElement('p');
                empty.className = 'group-placeholder';
                empty.style.cssText = 'text-align:center;padding:14px;';
                empty.textContent = f ? 'No matches.' : 'No one has spoken yet. Members appear here once they post.';
                membersEl.appendChild(empty);
            } else {
                membersEl.appendChild(frag);
            }

            // Owner-only "Banned" section: the banlist isn't otherwise visible (banned members
            // are excluded from the list above), so surface it here with an unban affordance.
            if (caps.ban && bannedList.length && !f) {
                const hdr = document.createElement('div');
                hdr.textContent = `Banned (${bannedList.length})`;
                hdr.style.cssText = 'font-size:12px;text-transform:uppercase;letter-spacing:0.06em;opacity:0.5;margin:16px 0 6px;padding-left:2px;';
                membersEl.appendChild(hdr);
                for (const bnpub of bannedList) {
                    const p = arrProfiles.find(x => x.id === bnpub) || null;
                    const nm = p ? (p.nickname || p.name || '') : '';
                    const disp = nm || (bnpub.substring(0, 10) + '...' + bnpub.substring(bnpub.length - 6));
                    const row = document.createElement('div');
                    row.className = 'member-pick-row';
                    const av = createAvatarImg(p ? getProfileAvatarSrc(p) : null, 25, false);
                    av.className = 'member-pick-avatar';
                    av.style.opacity = '0.5';
                    row.appendChild(av);
                    const ns = document.createElement('div');
                    ns.className = 'compact-member-name';
                    ns.textContent = disp;
                    ns.style.opacity = '0.6';
                    if (nm) twemojify(ns);
                    row.appendChild(ns);
                    const unbanBtn = document.createElement('button');
                    unbanBtn.className = 'cmt-btn cmt-btn-sm cmt-btn-secondary';
                    unbanBtn.title = 'Unban';
                    unbanBtn.style.marginLeft = 'auto';
                    unbanBtn.innerHTML = '<span class="icon icon-add-user"></span>Unban';
                    unbanBtn.onclick = async (e) => {
                        e.stopPropagation();
                        unbanBtn.disabled = true;
                        unbanBtn.innerHTML = '<span class="icon icon-loading spin"></span>Unbanning';
                        try {
                            await invoke('unban_community_member', { communityId, npub: bnpub });
                            bannedList = bannedList.filter(x => x !== bnpub);
                            renderMembers(searchEl?.value || '');
                        } catch (err) {
                            unbanBtn.disabled = false;
                            unbanBtn.innerHTML = '<span class="icon icon-add-user"></span>Unban';
                            showToast(String(err));
                        }
                    };
                    row.appendChild(unbanBtn);
                    membersEl.appendChild(row);
                }
            }
        };
        renderMembers();

        // Resolve unknown member + banned-member profiles (name/avatar), then re-render once.
        const unknowns = [...memberList.map(m => m.npub), ...bannedList].filter(np => !arrProfiles.some(p => p.id === np) && !strangerProfileRequested.has(np));
        unknowns.forEach(np => strangerProfileRequested.add(np));
        if (unknowns.length) {
            Promise.allSettled(unknowns.map(np => invoke('load_profile', { npub: np }))).then(() => {
                if (domGroupOverview.getAttribute('data-group-id') === chat.id) renderMembers(searchEl?.value || '');
            });
        }

        if (searchEl) {
            // Hide the whole search row (icon + input), not just the input — else the magnifying glass
            // hovers orphaned above an empty member list.
            const searchContainer = searchEl.parentElement;
            if (searchContainer) searchContainer.style.display = memberList.length ? '' : 'none';
            searchEl.value = '';
            searchEl.oninput = () => renderMembers(searchEl.value || '');
        }
    }

    // Invite (owner only — link + by-npub).
    if (caps.create_invite) {
        domGroupInviteMemberBtn.style.display = 'flex';
        domGroupInviteMemberBtn.onclick = () => openCommunityInvitePanel(chat);
    } else {
        domGroupInviteMemberBtn.style.display = 'none';
    }

    // Leave / Delete Community. A member leaves (local drop). The OWNER can't meaningfully leave their own
    // root (§6.1): their button DELETES (dissolves) the community for everyone via an owner tombstone, then
    // tears down locally. The button label is set in BOTH branches (shared DOM, else a stale label leaks).
    const isCommunityOwner = chat.metadata?.custom_fields?.is_owner === 'true';
    const leaveLabel = domGroupLeaveBtn.querySelectorAll('span')[1];
    domGroupLeaveBtn.style.display = 'flex';
    domGroupLeaveBtn.style.opacity = '';
    domGroupLeaveBtn.style.pointerEvents = '';
    // Shared local teardown after a leave OR a delete: the backend dropped keys/rows; mirror it in the
    // local chat list (its own copy), resetting the open chat first so late events can't paint an orphan.
    const tearDownCommunityLocally = async () => {
        const goneChannelIds = new Set(
            arrChats.filter(c => c.metadata?.custom_fields?.community_id === communityId).map(c => c.id)
        );
        if (goneChannelIds.has(strOpenChat)) await closeChat();
        arrChats = arrChats.filter(c => c.metadata?.custom_fields?.community_id !== communityId);
        domGroupOverview.style.display = 'none';
        domGroupOverview.removeAttribute('data-group-id');
        renderChatlist();
        openChatlist();
    };
    if (isCommunityOwner) {
        if (leaveLabel) leaveLabel.innerText = 'Delete Community';
        domGroupLeaveBtn.onclick = async () => {
            // Type-to-confirm: the destructive action requires typing the community name exactly, so it
            // can't be fat-fingered (it ends the community for everyone, irreversibly).
            const typed = await popupConfirm(
                'Delete this community?',
                `This permanently ends "<b>${escapeHtml(name)}</b>" for everyone, including you. No new messages can be sent and no one can rejoin. People can still delete their own past messages. This cannot be undone.<br><br>Type the community name to confirm:`,
                false, name, 'vector_warning.svg');
            if (typed === false) return;
            if (String(typed).trim() !== name) {
                await popupConfirm('Not Deleted', 'The name did not match, so nothing was changed.', true, '', 'vector_warning.svg');
                return;
            }
            domGroupLeaveBtn.style.opacity = '0.5';
            domGroupLeaveBtn.style.pointerEvents = 'none';
            try {
                await invoke('delete_community', { communityId });
                await tearDownCommunityLocally();
            } catch (e) {
                domGroupLeaveBtn.style.opacity = '';
                domGroupLeaveBtn.style.pointerEvents = '';
                await popupConfirm('Failed to Delete', escapeHtml(String(e)), true, '', 'vector_warning.svg');
            }
        };
    } else {
        if (leaveLabel) leaveLabel.innerText = 'Leave';
        domGroupLeaveBtn.onclick = async () => {
            const confirmed = await popupConfirm('Leave Community', `Leave "<b>${escapeHtml(name)}</b>"? You'll need a new invite to rejoin.`, false, '', 'vector_warning.svg');
            if (!confirmed) return;
            domGroupLeaveBtn.style.opacity = '0.5';
            domGroupLeaveBtn.style.pointerEvents = 'none';
            try {
                await invoke('leave_community', { communityId });
                await tearDownCommunityLocally();
            } catch (e) {
                domGroupLeaveBtn.style.opacity = '';
                domGroupLeaveBtn.style.pointerEvents = '';
                await popupConfirm('Failed to Leave', escapeHtml(String(e)), true, '', 'vector_warning.svg');
            }
        };
    }

    domGroupOverviewBackBtn.onclick = () => {
        popBack('group-overview');
        domGroupOverview.style.display = 'none';
        domGroupOverview.removeAttribute('data-group-id');
        openChat(chat.id);
    };
}

/**
 * The Community invite panel: generate/copy/revoke shareable links, and invite by npub.
 * @param {Chat} chat - The Community channel chat
 */
/**
 * A non-interactable, unclosable progress modal that guides the user through a multi-second rekey
 * (privatize / private-ban). Listens to `community_rekey_progress` (emitted per phase by the backend:
 * reroll → per-member key prep → send → per-edition repost → finalize) and fills a determinate ring.
 * Awaits the listener registration before returning so early phases aren't missed. Returns { finish, close }.
 */
async function showRekeyProgressModal(title) {
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay rekey-progress-overlay';
    overlay.onclick = (e) => e.stopPropagation(); // swallow — no backdrop dismiss
    const box = document.createElement('div');
    box.className = 'modal-box rekey-progress-box';
    box.innerHTML = `
        <div class="rekey-ring"><span class="rekey-pct">0%</span></div>
        <p class="rekey-title">${escapeHtml(title || 'Updating community keys')}</p>
        <p class="rekey-step">Starting...</p>
    `;
    overlay.appendChild(box);
    document.body.appendChild(overlay);
    const ring = box.querySelector('.rekey-ring');
    const pctEl = box.querySelector('.rekey-pct');
    const stepEl = box.querySelector('.rekey-step');
    const setProgress = (pct, label) => {
        const p = Math.max(0, Math.min(100, pct | 0));
        ring.style.setProperty('--rekey-pct', `${p}%`); // the @property transition sweeps the ring to it
        if (label) stepEl.textContent = label;
    };
    // Drive the % readout from the LIVE animated ring value so the number counts up in lockstep with the
    // sweep, instead of snapping to the target ahead of it.
    let rafId = requestAnimationFrame(function tick() {
        const cur = parseFloat(getComputedStyle(ring).getPropertyValue('--rekey-pct')) || 0;
        pctEl.textContent = `${Math.round(cur)}%`;
        rafId = requestAnimationFrame(tick);
    });
    // Register BEFORE the caller invokes the op, so we don't miss the opening phases.
    const unlisten = await listen('community_rekey_progress', (evt) => {
        const { pct, label } = evt.payload || {};
        setProgress(typeof pct === 'number' ? pct : 0, label);
    });
    return {
        // Fill to 100% with a closing label and hold so the sweep + count-up finish before we close.
        finish: async (label) => { setProgress(100, label || 'Done!'); await new Promise(r => setTimeout(r, 700)); },
        close: () => { cancelAnimationFrame(rafId); unlisten(); overlay.remove(); },
    };
}

async function openCommunityInvitePanel(chat) {
    const communityId = chat.metadata?.custom_fields?.community_id;
    if (!communityId) return;

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay';
    let busy = false; // a critical op (link create / revoke+rekey / direct invite) is in flight — lock the panel
    let unlistenRefresh = null; // community_refreshed subscription, torn down on dismiss
    const dismiss = () => { if (unlistenRefresh) { unlistenRefresh(); unlistenRefresh = null; } overlay.remove(); };
    overlay.onclick = (e) => { if (e.target === overlay && !busy) dismiss(); };

    const box = document.createElement('div');
    box.className = 'modal-box cmt-modal';
    box.innerHTML = `
        <div class="cmt-header">
            <div class="cmt-header-icon"><span class="icon icon-users-multi"></span></div>
            <div class="cmt-header-text">
                <h3 class="cmt-title">Invite to ${escapeHtml(chat.metadata.custom_fields.name || 'Community')}</h3>
                <p class="cmt-subtitle">Bring people into your community.</p>
            </div>
        </div>

        <section class="cmt-section">
            <div class="cmt-section-head">
                <span class="icon icon-share"></span>
                <div>
                    <p class="cmt-section-title">Invite Links <span id="cmt-mode" class="cmt-mode-pill"></span></p>
                    <p class="cmt-section-desc">Anyone with a link can join. Revoke every link to go private again.</p>
                </div>
            </div>
            <div id="cmt-links"></div>
            <button id="cmt-new-link" class="cmt-btn cmt-btn-secondary"><span class="icon icon-plus"></span>Create invite link</button>
        </section>

        <section class="cmt-section">
            <div class="cmt-section-head">
                <span class="icon icon-add-user"></span>
                <div>
                    <p class="cmt-section-title">Direct Invites</p>
                    <p class="cmt-section-desc">Pick contacts to invite, or paste an npub to add someone new.</p>
                </div>
            </div>
            <input id="cmt-npub" type="text" placeholder="Search contacts or paste an npub..." autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false" />
            <div id="cmt-contacts" class="cmt-contacts"></div>
            <button id="cmt-send-npub" class="cmt-btn cmt-btn-primary cmt-invite-btn" disabled><span class="icon icon-send"></span><span id="cmt-invite-label">Invite</span></button>
        </section>

        <div id="cmt-status" class="cmt-status"></div>
        <button id="cmt-close" class="cmt-btn cmt-btn-ghost cmt-close">Done</button>
    `;
    overlay.appendChild(box);
    document.body.appendChild(overlay);
    pushBack('community-invite', dismiss);

    const status = box.querySelector('#cmt-status');
    const linksDiv = box.querySelector('#cmt-links');
    const setStatus = (msg, isError) => {
        status.textContent = msg || '';
        status.classList.toggle('cmt-err', !!isError);
        status.classList.toggle('cmt-ok', !!msg && !isError);
    };
    box.querySelector('#cmt-close').onclick = () => { if (busy) return; popBack('community-invite'); dismiss(); };
    // Lock the ENTIRE panel during a critical op: disable every control + block close/backdrop-dismiss, so a
    // link create / revoke (which re-keys) / direct invite can't be raced or interrupted half-applied. Restores
    // each control's prior disabled state on release (e.g. the Invite button stays disabled if nothing's picked).
    const setBusy = (on) => {
        busy = on;
        box.classList.toggle('cmt-busy', on);
        box.querySelectorAll('button, input').forEach(el => {
            if (on) { el.dataset.cmtPrev = el.disabled ? '1' : '0'; el.disabled = true; }
            else if (el.dataset.cmtPrev !== undefined) { el.disabled = el.dataset.cmtPrev === '1'; delete el.dataset.cmtPrev; }
        });
    };

    // Track the GLOBAL link state (across every creator, §10) so the create/revoke handlers know when a
    // click crosses the Public⇄Private boundary. The mode is the folded registry, NOT just my own links —
    // another admin's live link keeps the community Public even when I hold none.
    let currentLinkCount = 0;   // MY own links (drives the per-row revoke confirm)
    let globalLinkCount = 0;    // every creator's links combined (drives the mode boundary)
    const renderLinks = async () => {
        linksDiv.innerHTML = '';
        let links = [];
        try { links = await invoke('list_public_invites', { communityId }); } catch (_) {}
        currentLinkCount = links.length;
        // §10 computed mode + per-creator breakdown from the folded registry (the authoritative source).
        let summary = { is_public: links.length > 0, creators: [] };
        try { summary = await invoke('get_community_invite_summary', { communityId }); } catch (_) {}
        globalLinkCount = (summary.creators || []).reduce((n, c) => n + (c.count || 0), 0);
        const modeEl = box.querySelector('#cmt-mode');
        if (modeEl) {
            const pub = !!summary.is_public;
            modeEl.textContent = pub ? 'Public' : 'Private';
            modeEl.classList.toggle('is-public', pub);
            modeEl.title = pub ? 'Anyone with a link can join' : 'Invite-only — no public links';
        }
        // Other creators' active links (mine are listed individually below). Surfaces the multi-creator
        // reality: "Alice has 2 active invite links" — so the mode isn't a mystery when I hold no links.
        const others = (summary.creators || []).filter(c => c.npub !== strPubkey && (c.count || 0) > 0);
        if (others.length) {
            const note = document.createElement('div');
            note.className = 'cmt-others';
            for (const c of others) {
                const line = document.createElement('p');
                line.className = 'cmt-other-line';
                line.textContent = `${systemEventName(c.npub)} has ${c.count} active invite link${c.count === 1 ? '' : 's'}`;
                note.appendChild(line);
            }
            linksDiv.appendChild(note);
        }
        if (!links.length) {
            if (!others.length) linksDiv.innerHTML = '<p class="cmt-empty">No active links yet. Create one to start inviting.</p>';
            else linksDiv.insertAdjacentHTML('beforeend', '<p class="cmt-empty">You have no links of your own yet. Create one to start inviting.</p>');
            return;
        }
        for (const link of links) {
            const row = document.createElement('div');
            row.className = 'cmt-link-row';
            const url = document.createElement('span');
            url.className = 'cmt-link-url';
            url.textContent = link.url;
            const copyBtn = document.createElement('button');
            copyBtn.className = 'cmt-icon-btn'; copyBtn.title = 'Copy link';
            copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
            copyBtn.onclick = () => {
                navigator.clipboard.writeText(link.url);
                copyBtn.classList.add('cmt-copied');
                copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                setTimeout(() => { copyBtn.classList.remove('cmt-copied'); copyBtn.innerHTML = '<span class="icon icon-copy"></span>'; }, 1200);
            };
            const revokeBtn = document.createElement('button');
            revokeBtn.className = 'cmt-icon-btn cmt-icon-btn-danger'; revokeBtn.title = 'Revoke link';
            revokeBtn.innerHTML = '<span class="icon icon-trash"></span>';
            revokeBtn.onclick = async () => {
                // Revoking the last GLOBAL link (across every creator) flips the community back to Private —
                // a re-founding rekey that cuts off link-joined lurkers. Confirm + warn (it's slow). If
                // another creator still has a link, this revoke is a quiet, instant edit (mode stays Public).
                const wouldPrivatize = globalLinkCount === 1;
                if (wouldPrivatize) {
                    const ok = await popupConfirm('Make community private?',
                        'Revoking the last invite link makes this community <b>private</b> again. This can take a few seconds.',
                        false, '', 'vector_warning.svg', '', 'Make private');
                    if (!ok) return;
                }
                setBusy(true); // lock the whole panel — revoking the last link re-keys, a critical op
                revokeBtn.innerHTML = '<span class="icon icon-loading spin"></span>';
                // Privatizing re-keys (multi-second): show the guided progress ring. A plain revoke is quick.
                const prog = wouldPrivatize ? await showRekeyProgressModal('Making community private') : null;
                if (!wouldPrivatize) setStatus('Revoking…');
                try {
                    await invoke('revoke_public_invite', { communityId, token: link.token });
                    if (prog) await prog.finish('Community is now private');
                    setStatus('');
                    setBusy(false);
                    if (prog) prog.close();
                    await renderLinks();
                } catch (e) {
                    setBusy(false);
                    if (prog) prog.close();
                    revokeBtn.innerHTML = '<span class="icon icon-trash"></span>';
                    setStatus('');
                    if (isLast) {
                        // Privatizing re-keys; on a bunker account that fails with a long explanation —
                        // show it as a persistent notice rather than a one-line status that scrolls away.
                        await popupConfirm("Couldn't make private", escapeHtml(String(e)), true, '', 'vector_warning.svg');
                    } else {
                        setStatus(String(e), true);
                    }
                }
            };
            row.append(url, copyBtn, revokeBtn);
            linksDiv.appendChild(row);
        }
    };
    await renderLinks();
    // Live-refresh when a control change folds in (a remote create/revoke by another admin, or our own
    // privatize re-founding), so the mode pill + per-creator counts update without a manual close/reopen.
    // Skipped while a local critical op is in flight (its own handler re-renders on completion).
    unlistenRefresh = await listen('community_refreshed', (evt) => {
        const cid = evt.payload?.community_id || evt.payload;
        if (cid === communityId && !busy) renderLinks();
    });

    box.querySelector('#cmt-new-link').onclick = async (e) => {
        const btn = e.currentTarget; // capture before await — currentTarget is null after it
        // The FIRST link ANYWHERE (across all creators) flips a private community to Public (anyone with the
        // link can join). Confirm that boundary crossing; if it's already Public (someone holds a link), a
        // new link doesn't change the mode, so skip the warning.
        if (globalLinkCount === 0) {
            const ok = await popupConfirm('Make community public?',
                'Creating an invite link makes this community <b>public</b>: anyone with the link can join. You can make it private again later by revoking every link.',
                false, '', 'vector_warning.svg', '', 'Make public');
            if (!ok) return;
        }
        // Optional label — the attribution bucket ("Reddit", "Conf"). Shows up as "joined via <label>".
        const labelInput = await popupConfirm('Label this link', 'Optional. A label lets you see which link people join through (e.g. "Reddit", "Twitter"). Leave blank to skip.', false, 'Label (optional)', '', '', 'Create link');
        if (labelInput === false) return; // cancelled
        const label = (typeof labelInput === 'string' && labelInput.trim()) ? labelInput.trim() : null;
        setBusy(true); // lock the panel — the FIRST link flips public + the publish is a critical op
        btn.innerHTML = '<span class="icon icon-loading spin"></span>Creating…'; setStatus('Creating link...');
        try { await invoke('create_public_invite', { communityId, expiresInSecs: null, label }); setStatus(''); }
        catch (err) { setStatus(String(err), true); }
        finally { setBusy(false); btn.innerHTML = '<span class="icon icon-plus"></span>Create invite link'; await renderLinks(); }
    };

    // ── Direct Invites: a multi-select contact list (DM contacts) with paste-to-add ──
    const npubInput = box.querySelector('#cmt-npub');
    const contactsDiv = box.querySelector('#cmt-contacts');
    const inviteBtn = box.querySelector('#cmt-send-npub');
    const inviteLabel = box.querySelector('#cmt-invite-label');
    const myNpub = arrProfiles.find(p => p.mine)?.id;
    const selectedInvitees = new Set();   // npubs chosen to invite
    const strangerInvitees = new Set();   // pasted npubs not in our contacts
    // Banned npubs can't be invited (§7) — hide them from the picker (the backend also refuses). Empty if
    // we lack the ban permission to read the list (then the backend refusal is the only guard).
    let bannedSet = new Set();
    try { bannedSet = new Set(await invoke('get_community_banlist', { communityId })); } catch (_) {}

    const buildContactRow = (npub, profile) => {
        const isSel = selectedInvitees.has(npub);
        const row = document.createElement('div');
        row.className = 'member-pick-row';
        const bg = document.createElement('div');
        bg.className = 'member-pick-hover';
        row.appendChild(bg);
        row.addEventListener('mouseenter', () => {
            const c = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
            bg.style.background = `linear-gradient(to right, ${c}40, transparent)`;
        });
        const avatar = createAvatarImg(profile ? getProfileAvatarSrc(profile) : null, 25, false);
        avatar.className = 'member-pick-avatar';
        row.appendChild(avatar);
        const nameSpan = document.createElement('div');
        nameSpan.className = 'compact-member-name';
        const nm = profile ? (profile.nickname || profile.name || '') : '';
        nameSpan.textContent = nm || (npub.substring(0, 10) + '...' + npub.substring(npub.length - 6));
        if (nm) twemojify(nameSpan);
        row.appendChild(nameSpan);
        const indicator = document.createElement('div');
        indicator.className = 'member-pick-indicator' + (isSel ? ' selected' : '');
        row.appendChild(indicator);
        row.addEventListener('click', (e) => {
            e.preventDefault(); e.stopPropagation();
            if (selectedInvitees.has(npub)) selectedInvitees.delete(npub);
            else selectedInvitees.add(npub);
            renderContacts(npubInput.value || '');
        });
        return row;
    };

    const renderContacts = (filterText = '') => {
        contactsDiv.innerHTML = '';
        const f = (filterText || '').trim().toLowerCase();
        const filterNpub = extractNpub(filterText);
        const dmIds = new Set(arrChats.filter(c => c.chat_type === 'DirectMessage').map(c => c.id));
        const frag = document.createDocumentFragment();

        // Pasted strangers (show if selected, or matching the current filter npub) — never a banned npub.
        for (const npub of strangerInvitees) {
            if (bannedSet.has(npub)) continue;
            if (!selectedInvitees.has(npub) && filterNpub !== npub) continue;
            frag.appendChild(buildContactRow(npub, arrProfiles.find(p => p.id === npub) || null));
        }

        // DM contacts: selected first, then most-recent conversation. Banned npubs are excluded.
        const contacts = arrProfiles
            .filter(p => p && p.id && p.id !== myNpub && !p.is_blocked && !bannedSet.has(p.id) && dmIds.has(p.id))
            .sort((a, b) => {
                const aSel = selectedInvitees.has(a.id), bSel = selectedInvitees.has(b.id);
                if (aSel !== bSel) return aSel ? -1 : 1;
                const at = getChatSortTimestamp(arrChats.find(c => c.id === a.id) || {});
                const bt = getChatSortTimestamp(arrChats.find(c => c.id === b.id) || {});
                return (bt || 0) - (at || 0);
            });
        for (const p of contacts) {
            const name = p.nickname || p.name || '';
            if (f && !(name + ' ' + p.id).toLowerCase().includes(filterNpub || f)) continue;
            frag.appendChild(buildContactRow(p.id, p));
        }

        if (!frag.childElementCount) {
            contactsDiv.innerHTML = `<p class="cmt-empty" style="text-align:center;">${f ? 'No matches.' : 'No contacts yet. Paste an npub to invite someone.'}</p>`;
        } else {
            contactsDiv.appendChild(frag);
        }

        const n = selectedInvitees.size;
        inviteBtn.disabled = n === 0;
        inviteLabel.textContent = n === 0 ? 'Invite' : `Invite ${n}`;
    };
    renderContacts();

    // Typing filters; a pasted/typed valid npub gets added to the list and auto-selected.
    npubInput.oninput = () => {
        const np = extractNpub(npubInput.value || '');
        if (np && np !== myNpub) {
            if (!arrProfiles.some(p => p.id === np)) {
                strangerInvitees.add(np);
                if (!strangerProfileRequested.has(np)) {
                    strangerProfileRequested.add(np);
                    invoke('load_profile', { npub: np }).then(() => renderContacts(npubInput.value || '')).catch(() => {});
                }
            }
            selectedInvitees.add(np);
        }
        renderContacts(npubInput.value || '');
    };

    inviteBtn.onclick = async (e) => {
        const targets = [...selectedInvitees];
        if (!targets.length) return;
        e.currentTarget.disabled = true;
        setStatus(`Inviting ${targets.length}...`);
        let ok = 0, fail = 0;
        for (const np of targets) {
            try { await invoke('invite_to_community', { communityId, inviteeNpub: np }); ok++; }
            catch (_) { fail++; }
        }
        if (fail === 0) {
            setStatus(`Invited ${ok} ${ok === 1 ? 'person' : 'people'}!`);
            selectedInvitees.clear(); strangerInvitees.clear(); npubInput.value = '';
        } else {
            setStatus(`Invited ${ok}, ${fail} failed.`, ok === 0);
        }
        renderContacts('');
    };
}


async function openChatlist() {
    // Chatlist is the root — clearing the back stack means the next back
    // press exits to the home screen instead of replaying old open fns.
    clearBack();
    navbarSelect('chat-btn');
    domNavbar.style.display = '';
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    if (domChats.style.display !== '') {
        // Run a subtle fade-in animation
        domChats.classList.add('fadein-subtle-anim');
        domChats.addEventListener('animationend', () => domChats.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domChats.style.display = '';
    }
    
    // Load and display pending Community invites (adjust layout before/after for consistency)
    adjustSize();
    await loadCommunityInvites();
    adjustSize();

    // Refresh timestamps immediately so they're not stale after viewing a chat
    updateChatlistTimestamps();
}

/** Apply the current bunker connection state to the Security panel's
 *  status dot. State strings match the backend's `bunker_state` event:
 *  'idle' | 'connecting' | 'online' | 'offline'. Idle clears the dot. */
function applyRemoteSignerDot(state) {
    const dot = document.getElementById('remote-signer-dot');
    if (!dot) return;
    dot.classList.remove('online', 'offline', 'connecting');
    if (state === 'online' || state === 'offline' || state === 'connecting') {
        dot.classList.add(state);
    }
}

/** Populate the Remote Signer card in Security settings, or hide it
 *  entirely for local-key accounts. Also hides the Export Account row
 *  for bunker accounts since the identity key isn't on this device. */
async function refreshRemoteSignerCard() {
    const card = document.getElementById('settings-remote-signer');
    const exportRow = document.getElementById('export-account-row');
    if (!card) return;
    try {
        const status = await invoke('get_bunker_status');
        if (!status) {
            card.style.display = 'none';
            if (exportRow) exportRow.style.display = '';
            return;
        }
        const pkEl = document.getElementById('remote-signer-pubkey');
        if (pkEl) {
            const npub = status.remote_npub || '';
            pkEl.textContent = npub
                ? `${npub.slice(0, 12)}…${npub.slice(-6)}`
                : '—';
            pkEl.title = npub;
        }
        card.style.display = '';
        if (exportRow) exportRow.style.display = 'none';
    } catch (e) {
        console.warn('[settings] get_bunker_status failed:', e);
        card.style.display = 'none';
        if (exportRow) exportRow.style.display = '';
    }
}

function openSettings() {
    pushBack('settings', () => openChatlist());
    navbarSelect('settings-btn');
    domNavbar.style.display = '';
    domSettings.style.display = '';

    // Hide the other tabs
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domInvites.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Update the Storage Breakdown
    initStorageSection();

    // Refresh blocked users list, logs cache, and Remote Signer card
    loadBlockedUsersList();
    invoke('get_logs').then((log) => { window._cachedLogs = log || ''; });
    refreshRemoteSignerCard();

    // If an update is available, scroll to the updates section
    const updateDot = document.getElementById('settings-update-dot');
    if (updateDot && updateDot.style.display !== 'none') {
        // Give the settings tab time to render
        setTimeout(() => {
            const updatesSection = document.getElementById('settings-updates');
            if (updatesSection) {
                updatesSection.scrollIntoView({ behavior: 'smooth', block: 'start' });
                // Hide the notification dot after scrolling
                updateDot.style.display = 'none';
            }
        }, 100);
    }
}

async function openInvites() {
    pushBack('invites', () => openChatlist());
    navbarSelect('invites-btn');
    domNavbar.style.display = '';
    domInvites.style.display = '';

    // Hide the other tabs
    if (fProfileEditMode) exitProfileEditMode(true);
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domGroupOverview.style.display = 'none';
    previousChatBeforeProfile = ""; // Clear when navigating away

    // Fetch and display the invite code
    const inviteCodeElement = document.getElementById('invite-code');
    inviteCodeElement.textContent = 'Loading';
    
    try {
        const inviteCode = await invoke('get_or_create_invite_code');
        inviteCodeElement.textContent = inviteCode;
        document.getElementById('invite-code-twitter').href = buildXIntentUrl(inviteCode);
        
        // Add invite code copy functionality
        const copyBtn = document.getElementById('invite-code-copy');
        if (copyBtn) {
            // Remove any existing listeners to prevent duplicates
            copyBtn.replaceWith(copyBtn.cloneNode(true));
            const newCopyBtn = document.getElementById('invite-code-copy');
            
            newCopyBtn.addEventListener('click', (e) => {
                if (inviteCode && inviteCode !== 'Loading...' && inviteCode !== 'Error loading code') {
                    navigator.clipboard.writeText(inviteCode).then(() => {
                        const btn = e.target.closest('.invite-code-copy-btn');
                        if (btn) {
                            btn.innerHTML = '<span class="icon icon-check"></span>';
                            setTimeout(() => {
                                btn.innerHTML = '<span class="icon icon-copy"></span>';
                            }, 2000);
                        }
                    });
                }
            });
        }
    } catch (error) {
        inviteCodeElement.textContent = 'Error loading code';
        console.error('Failed to get invite code:', error);
    }

    // Note: MLS invites are now shown in the Chat tab, not here
}

/**
 * Edit the profile description inline
 */
function updateProfileEditLabel() {
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;
    const nameInput = document.querySelector('#profile-edit-name input');
    const statusInput = document.querySelector('#profile-edit-status input');
    const bioInput = document.querySelector('#profile-edit-bio textarea');
    const label = document.getElementById('profile-edit-mode-label');
    if (!label) return;

    const nameChanged = nameInput?.value.trim() !== (objProfileEditSnapshot.name || '');
    const statusChanged = statusInput?.value.trim() !== (objProfileEditSnapshot.status?.title ?? objProfileEditSnapshot.status ?? '');
    const bioChanged = bioInput?.value.trim() !== (objProfileEditSnapshot.about || '');
    const avatarChanged = strPendingProfileAvatarPath !== null;
    const bannerChanged = strPendingProfileBannerPath !== null;

    if (nameChanged || statusChanged || bioChanged || avatarChanged || bannerChanged) {
        label.textContent = 'Unsaved Changes Made';
        label.style.opacity = '0.8';
    } else {
        label.textContent = 'Edit Mode is Enabled';
        label.style.opacity = '0.8';
    }
}

function enterProfileEditMode() {
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;
    objProfileEditSnapshot = {
        name: cProfile.name || '',
        status: cProfile.status || '',
        about: cProfile.about || '',
        avatar: getProfileAvatarSrc(cProfile) || null,
        banner: cProfile.banner || null
    };
    strPendingProfileAvatarPath = null;
    strPendingProfileBannerPath = null;
    fProfileEditMode = true;
    domProfileEditBar.style.opacity = '0';
    domProfileEditBar.style.display = 'flex';
    setTimeout(() => domProfileEditBar.style.opacity = '1', 10);
    domProfileBackBtn.style.display = 'none';
    document.querySelector('.profile-header-info').style.display = 'none';
    domProfileBanner.onclick = async () => {
        if (!fProfileEditMode) return;
        const { open } = window.__TAURI__.dialog;
        const file = await open({
            title: 'Choose Banner Image',
            multiple: false,
            directory: false,
            filters: [{ name: 'Image', extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp'] }]
        });
        if (!file) return;
        strPendingProfileBannerPath = file;
        updateProfileEditLabel();
        if (domProfileBanner.tagName === 'DIV') {
            const newBanner = document.createElement('img');
            newBanner.className = domProfileBanner.className;
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
        domProfileBanner.src = convertFileSrc(file);
    };
    document.getElementById('profile-edit-btn').style.display = 'none';
    document.getElementById('profile-share-btn').style.display = 'none';
    document.getElementById('profile-npub-label').style.display = 'none';
    document.getElementById('profile-npub-container').style.display = 'none';
    document.getElementById('profile-badges').style.display = 'none';
    document.getElementById('profile-secondary-name').style.display = 'none';
    document.getElementById('profile-secondary-status').style.display = 'none';
    document.getElementById('profile-description').style.display = 'none';
    const editName = document.getElementById('profile-edit-name');
    const editStatus = document.getElementById('profile-edit-status');
    const editBio = document.getElementById('profile-edit-bio');

    editName.closest('.profile-edit-field-wrapper').style.position = 'relative';
    editName.innerHTML = `<input type="text" value="${cProfile.name || ''}" maxlength="50" style="background: none; border: none; outline: none; color: inherit; font-size: 16px; width: 100%;">`;
    editStatus.innerHTML = `<input type="text" value="${cProfile.status?.title || ''}" style="background: none; border: none; outline: none; color: inherit; font-size: 16px; width: 100%;">`;
    editBio.innerHTML = `<textarea style="background: none; border: none; outline: none; color: inherit; font-size: 16px; width: 100%; resize: none; min-height: 60px;">${typeof cProfile.about === 'string' ? cProfile.about : ''}</textarea>`;
    const bioTextarea = editBio.querySelector('textarea');
    setTimeout(() => {
        bioTextarea.style.height = 'auto';
        bioTextarea.style.height = bioTextarea.scrollHeight + 'px';
    }, 10);
    bioTextarea.addEventListener('input', () => {
        bioTextarea.style.height = 'auto';
        bioTextarea.style.height = bioTextarea.scrollHeight + 'px';
    });
    const nameInput = document.querySelector('#profile-edit-name input');
    const statusInput = document.querySelector('#profile-edit-status input');
    nameInput?.addEventListener('input', updateProfileEditLabel);
    statusInput?.addEventListener('input', updateProfileEditLabel);
    bioTextarea.addEventListener('input', updateProfileEditLabel);
    document.getElementById('profile-edit-fields').style.display = 'flex';
    document.getElementById('profile').classList.add('profile-edit-active');

    domProfileAvatar.classList.add('btn');
    domProfileAvatar.onclick = async () => {
        if (!fProfileEditMode) return;
        const { open } = window.__TAURI__.dialog;
        const file = await open({
            title: 'Choose Profile Picture',
            multiple: false,
            directory: false,
            filters: [{ name: 'Image', extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp'] }]
        });
        if (!file) return;
        strPendingProfileAvatarPath = file;
        updateProfileEditLabel();
        domProfileAvatar.src = convertFileSrc(file);
    };

    // Reset label to clean state on entry
    updateProfileEditLabel();

    const bannerContainer = document.getElementById('profile-banner-container');
    const avatarContainer = document.querySelector('.profile-avatar-container');
    bannerContainer._editMoveHandler = (e) => {
        const bannerRect = bannerContainer.getBoundingClientRect();
        const avatarRect = avatarContainer.getBoundingClientRect();
        const inBanner = e.clientY <= bannerRect.top + 200;
        const inAvatar = (
            e.clientX >= avatarRect.left &&
            e.clientX <= avatarRect.right &&
            e.clientY >= avatarRect.top &&
            e.clientY <= avatarRect.bottom
        );
        if (inAvatar || !inBanner) {
            bannerContainer.classList.add('avatar-hovered');
        } else {
            bannerContainer.classList.remove('avatar-hovered');
        }
    };
    bannerContainer.addEventListener('mousemove', bannerContainer._editMoveHandler);
}

function exitProfileEditMode(fCancel = false) {
    fProfileEditMode = false;
    domProfileEditBar.style.opacity = '0';
    setTimeout(() => domProfileEditBar.style.display = 'none', 250);
    document.querySelector('.profile-header-info').style.display = '';
    domProfileBackBtn.style.display = 'none';
    document.getElementById('profile-npub-label').style.display = '';
    document.getElementById('profile-npub-container').style.display = '';
    document.getElementById('profile-badges').style.display = '';
    document.getElementById('profile-edit-fields').style.display = 'none';
    document.getElementById('profile-edit-btn').style.display = '';
    document.getElementById('profile-share-btn').style.display = '';
    document.getElementById('profile-secondary-name').style.display = '';
    document.getElementById('profile-secondary-status').style.display = '';
    document.getElementById('profile-description').style.display = '';
    document.getElementById('profile').classList.remove('profile-edit-active');

    // Reset label back to clean state
    const label = document.getElementById('profile-edit-mode-label');
    if (label) {
        label.textContent = 'Edit Mode is Enabled';
        label.style.opacity = '0.8';
    }

    const cProfile = arrProfiles.find(a => a.mine);
    if (cProfile) {
        if (fCancel) {
            cProfile.name = objProfileEditSnapshot.name;
            cProfile.status = objProfileEditSnapshot.status;
            cProfile.about = objProfileEditSnapshot.about;
            // Revert avatar preview
            if (strPendingProfileAvatarPath) {
                strPendingProfileAvatarPath = null;
                const originalSrc = objProfileEditSnapshot.avatar;
                if (originalSrc) {
                    if (domProfileAvatar.tagName === 'DIV') {
                        const newAvatar = document.createElement('img');
                        newAvatar.className = domProfileAvatar.className;
                        domProfileAvatar.replaceWith(newAvatar);
                        domProfileAvatar = newAvatar;
                    }
                    domProfileAvatar.src = originalSrc;
                } else {
                    const placeholder = createPlaceholderAvatar(false, 175);
                    placeholder.classList.add('profile-avatar');
                    domProfileAvatar.replaceWith(placeholder);
                    domProfileAvatar = placeholder;
                }
            }
            // Revert banner preview
            if (strPendingProfileBannerPath) {
                strPendingProfileBannerPath = null;
                const originalBannerSrc = objProfileEditSnapshot.banner;
                if (originalBannerSrc) {
                    if (domProfileBanner.tagName === 'DIV') {
                        const newBanner = document.createElement('img');
                        newBanner.className = domProfileBanner.className;
                        domProfileBanner.replaceWith(newBanner);
                        domProfileBanner = newBanner;
                    }
                    domProfileBanner.src = originalBannerSrc;
                } else {
                    domProfileBanner.src = '';
                    domProfileBanner.style.backgroundColor = 'rgb(27, 27, 27)';
                }
            }
        } else {
            const nameInput = document.querySelector('#profile-edit-name input');
            const statusInput = document.querySelector('#profile-edit-status input');
            const bioInput = document.querySelector('#profile-edit-bio textarea');
            const newName = nameInput ? nameInput.value.trim() : cProfile.name;
            const newStatus = statusInput ? statusInput.value.trim() : (cProfile.status?.title ?? '');
            const newAbout = bioInput ? bioInput.value.trim() : (cProfile.about ?? '');
            const prevName = objProfileEditSnapshot.name || '';
            const prevStatus = objProfileEditSnapshot.status?.title ?? objProfileEditSnapshot.status ?? '';
            const prevAbout = objProfileEditSnapshot.about || '';

            cProfile.name = newName;
            if (cProfile.status) cProfile.status.title = newStatus;
            cProfile.about = newAbout;

            const nameChanged = newName !== prevName;
            const aboutChanged = newAbout !== prevAbout;
            const statusChanged = newStatus !== prevStatus;
            if (nameChanged || aboutChanged) {
                invoke('update_profile', {
                    name: nameChanged ? newName : '',
                    avatar: '',
                    banner: '',
                    about: aboutChanged ? (newAbout.length > 0 ? newAbout : ' ') : '',
                }).then(ok => {
                    if (!ok) popupConfirm('Profile Update Failed!', 'Failed to broadcast profile update to the network.', true, '', 'vector_warning.svg');
                }).catch(e => popupConfirm('Profile Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
            }
            if (statusChanged) {
                invoke('update_status', { status: newStatus }).catch(e => popupConfirm('Status Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
            }
            if (strPendingProfileAvatarPath) {
                const pendingAvatarPath = strPendingProfileAvatarPath;
                const prevAvatarCached = cProfile.avatar_cached;
                // Keep the just-picked avatar on screen through save instead of
                // flashing back to the old one; the backend's upload_avatar
                // updates avatar_cached authoritatively once the upload lands.
                cProfile.avatar_cached = pendingAvatarPath;
                invoke('upload_avatar', { filepath: pendingAvatarPath, uploadType: 'avatar' })
                    .then(avatarUrl => {
                        if (avatarUrl) {
                            invoke('update_profile', {
                                name: '',
                                avatar: avatarUrl,
                                banner: '',
                                about: '',
                            }).then(ok => {
                                if (!ok) popupConfirm('Avatar Update Failed!', 'Failed to broadcast avatar update to the network.', true, '', 'vector_warning.svg');
                            }).catch(e => popupConfirm('Avatar Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
                        } else {
                            // Upload produced no URL — revert to the prior avatar.
                            cProfile.avatar_cached = prevAvatarCached;
                            if (domProfile.style.display !== 'none') renderProfileTab(cProfile);
                        }
                    })
                    .catch(e => {
                        cProfile.avatar_cached = prevAvatarCached;
                        if (domProfile.style.display !== 'none') renderProfileTab(cProfile);
                        popupConfirm('Avatar Upload Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
                    });
                strPendingProfileAvatarPath = null;
            }
            if (strPendingProfileBannerPath) {
                const pendingBannerPath = strPendingProfileBannerPath;
                const prevBannerCached = cProfile.banner_cached;
                // Keep the just-picked banner on screen through save instead of
                // flashing back to the old one; the backend's upload_avatar
                // updates banner_cached authoritatively once the upload lands.
                cProfile.banner_cached = pendingBannerPath;
                invoke('upload_avatar', { filepath: pendingBannerPath, uploadType: 'banner' })
                    .then(bannerUrl => {
                        if (bannerUrl) {
                            invoke('update_profile', {
                                name: '',
                                avatar: '',
                                banner: bannerUrl,
                                about: '',
                            }).then(ok => {
                                if (!ok) popupConfirm('Banner Update Failed!', 'Failed to broadcast banner update to the network.', true, '', 'vector_warning.svg');
                            }).catch(e => popupConfirm('Banner Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
                        } else {
                            // Upload produced no URL — revert to the prior banner.
                            cProfile.banner_cached = prevBannerCached;
                            if (domProfile.style.display !== 'none') renderProfileTab(cProfile);
                        }
                    })
                    .catch(e => {
                        cProfile.banner_cached = prevBannerCached;
                        if (domProfile.style.display !== 'none') renderProfileTab(cProfile);
                        popupConfirm('Banner Upload Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg');
                    });
                strPendingProfileBannerPath = null;
            }

            showToast('Profile Saved');
        }
        renderProfileTab(cProfile);
    }
    document.getElementById('profile-banner-container').classList.remove('avatar-hovered');
    domProfileBanner.onclick = null;
    const _bc = document.getElementById('profile-banner-container');
    if (_bc._editMoveHandler) {
        _bc.removeEventListener('mousemove', _bc._editMoveHandler);
        _bc._editMoveHandler = null;
    }
}

function editProfileDescription() {
    // Get the current profile
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;

    // Set the textarea content to current description
    domProfileDescriptionEditor.value = cProfile.about || '';

    // Hide the span and show the textarea
    domProfileDescription.style.display = 'none';
    domProfileDescriptionEditor.style.display = '';

    // Focus the text
    domProfileDescriptionEditor.focus();

    // Handle blur event to save and return to view mode
    domProfileDescriptionEditor.onblur = () => {
        // Hide textarea and show span
        domProfileDescriptionEditor.style.display = 'none';
        domProfileDescription.style.display = '';

        // Remove the blur event listener
        domProfileDescriptionEditor.onblur = null;

        // If nothing was edited, don't change anything
        if (domProfileDescriptionEditor.value === cProfile.about) return;

        // Update the profile's about property
        cProfile.about = domProfileDescriptionEditor.value;

        // Update the span content
        domProfileDescription.textContent = cProfile.about;
        twemojify(domProfileDescription);

        // Upload new About Me to Nostr
        invoke('update_profile', {
            name: '',
            avatar: '',
            banner: '',
            about: cProfile.about,
        }).then(ok => {
            if (!ok) popupConfirm('Bio Update Failed!', 'Failed to broadcast bio update to the network.', true, '', 'vector_warning.svg');
        }).catch(e => popupConfirm('Bio Update Failed!', escapeHtml(String(e)), true, '', 'vector_warning.svg'));
    };

    // Resize it to match the content size (CSS cannot scale textareas based on content)
    domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';

    // Handle input events to resize the textarea dynamically
    domProfileDescriptionEditor.oninput = () => {
        domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';
    };

    // Handle Enter key to submit (excluding Shift+Enter for line breaks)
    domProfileDescriptionEditor.onkeydown = (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domProfileDescriptionEditor.blur(); // Trigger the blur event to save
        }
    };
}

/**
 * A utility to "select" one Navbar item, deselecting the rest automatically.
 */
function navbarSelect(strSelectionID = '') {
    for (const navItem of domNavbar.querySelectorAll('div')) {
        if (strSelectionID === navItem.id) navItem.classList.remove('navbar-btn-inactive');
        else navItem.classList.add('navbar-btn-inactive');
    }
}

/**
 * Our Bech32 Nostr Public Key
 */
let strPubkey;

/**
 * The timestamp we sent our last typing indicator
 * 
 * Ensure this is wiped when the chat is closed!
 */
let nLastTypingIndicator = 0;

const strOriginalInputPlaceholder = domChatMessageInput.getAttribute('placeholder');

/**
 * Auto-resize the chat input textarea based on content.
 * Expands up to max-height defined in CSS (150px), then scrolls.
 * Only expands when content actually needs more space (multi-line).
 */
function autoResizeChatInput() {
    // Get actual computed styles
    const computed = window.getComputedStyle(domChatMessageInput);
    const lineHeight = parseFloat(computed.lineHeight) || 24;
    const paddingTop = parseFloat(computed.paddingTop) || 10;
    const paddingBottom = parseFloat(computed.paddingBottom) || 10;
    const padding = paddingTop + paddingBottom;
    
    // Single line scrollHeight = lineHeight + padding
    const singleLineScrollHeight = lineHeight + padding;
    
    // Track previous state for scroll adjustment
    const wasExpanded = domChatMessageInput.style.overflowY === 'auto';
    
    // Reset height and ensure overflow is hidden for accurate measurement
    // Setting overflow:hidden before measuring prevents scrollbar space from affecting layout
    domChatMessageInput.style.overflowY = 'hidden';
    domChatMessageInput.style.height = '0';
    
    // Get scrollHeight - this tells us how much space content actually needs
    const scrollHeight = domChatMessageInput.scrollHeight;
    
    // Only expand if content needs more than single line
    if (scrollHeight > singleLineScrollHeight) {
        // Set height to content needs minus padding (CSS height is content-box)
        domChatMessageInput.style.height = (scrollHeight - padding) + 'px';
        domChatMessageInput.style.overflowY = 'auto';
        
        // Soft scroll to keep chat at bottom when expanding
        softChatScroll();
    } else {
        // Single line - use default CSS height, keep overflow hidden
        domChatMessageInput.style.height = '';
        
        // If we just collapsed from multi-line, also soft scroll
        if (wasExpanded) {
            softChatScroll();
        }
    }
}

window.addEventListener("DOMContentLoaded", async () => {
    // Once login fade-in animation ends, remove it
    domLogin.addEventListener('animationend', () => domLogin.classList.remove('fadein-anim'), { once: true });

    // Fetch platform features to determine OS-specific behavior
    await fetchPlatformFeatures();

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
        
        // User is logged in, execute the action immediately
        await executeDeepLinkAction(evt.payload);
    });

    // Inbound share from another app (Android share sheet). If not logged in yet,
    // Rust has stored it pending and the post-login poll will pick it up.
    await listen('share_received', async (evt) => {
        if (fInit) return;
        await handleIncomingShare(evt.payload);
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
        const bunkerFormVisible = domLoginBunker
            && !domLoginBunker.classList.contains('is-hidden');
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
        if (domLoginBunker && !domLoginBunker.classList.contains('is-hidden')) {
            setTimeout(() => {
                if (domLoginBunker && !domLoginBunker.classList.contains('is-hidden')) {
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
            const formVisible = domLoginBunker
                && !domLoginBunker.classList.contains('is-hidden')
                && domLoginBunker.style.display !== 'none';
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
                if (domLoginBackBar) domLoginBackBar.style.display = 'none';
                const loginForm = document.getElementById('login-form');
                if (loginForm) loginForm.classList.remove('has-back-bar', 'bunker-active');
                if (domLogin) domLogin.style.display = 'none';
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
        const formVisible = domLoginBunker
            && !domLoginBunker.classList.contains('is-hidden')
            && domLoginBunker.style.display !== 'none';
        if (!formVisible) return;
        const err = evt?.payload?.error || 'Re-authorization failed';
        const status = document.getElementById('bunker-status-text');
        if (status) {
            status.textContent = String(err);
            status.className = 'login-bunker-status error';
        }
        // Auto-reroll on reauth timeout too (same rationale as pairing).
        if (domLoginBunker && !domLoginBunker.classList.contains('is-hidden')) {
            setTimeout(() => {
                if (domLoginBunker && !domLoginBunker.classList.contains('is-hidden')) {
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
    await listen('session_reload', () => {
        window.location.reload();
    });

    // Immediately load and apply theme settings (visual only, don't save)
    const strTheme = await invoke('get_theme');
    if (strTheme) {
        applyTheme(strTheme);
    }

    // Show the main window now that content is ready (prevents white flash on startup)
    // The window starts hidden via tauri.conf.json and Rust setup hides it explicitly
    // The WKWebView background is set to dark natively in lib.rs so no delay is needed
    // Only needed on desktop - mobile doesn't have this issue
    if (!platformFeatures.is_mobile) {
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
                
                // Setup Rust listeners
                await setupRustListeners();

                // Resolve Community logos (metadata already rides the chat payload).
                resolveCommunityAvatars();

                // Hide login UI and show main UI
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';
                domNavbar.style.display = '';
                domChatBookmarksBtn.style.display = 'flex';
                
                // Render our profile
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = '';
                
                // Mark init as complete so renderChatlist works
                fInit = false;
                
                // Render the chatlist
                renderChatlist();
                
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
                domLoginStart.style.display = 'none';
                domLoginEncrypt.style.display = '';
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
                    domLoginEncrypt.style.display = 'none';
                    domLoginStart.style.display = '';
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
    domLoginAccountCreationBtn.onclick = async () => {
        try {
            // Add Profile commit point: tear down the existing session
            // before generating a new keypair, otherwise create_account's
            // lock-and-check guard would silently reuse the old client.
            if (addAccountFlow.active) await addAccountFlow.commit();

            const { public: pubKey } = await invoke("create_account");
            strPubkey = pubKey;

            // Connect to Nostr network
            await invoke("connect");

            // Skip invite flow - go directly to encryption (key stays backend-only)
            openEncryptionFlow(false);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    };
    domLoginAccountBtn.onclick = () => {
        domLoginImport.style.display = '';
        domLoginStart.style.display = 'none';
        domLoginBackBar.style.display = '';
        document.getElementById('login-form').classList.add('has-back-bar');
        // Hide the picker pill — once the user is entering an nsec / seed
        // phrase, the active-account-from-marker context no longer applies.
        loginPicker.hide();
    };
    // Bunker form helpers (renderQrInto, startBunkerSession, showBunkerForm,
    // hideBunkerForm) are now defined at module scope, near the DOM-ref
    // block — they need to be accessible to the boot-time login catch which
    // runs before this DOMContentLoaded handler reaches button wiring.
    // hideBunkerForm hoisted to module scope; window.hideBunkerForm assigned there.
    if (domLoginBunkerStartBtn) {
        // Lives inside the Login screen (not the entry screen). Bunker is a
        // login flow — your signer *is* the identity, so there's no "create"
        // path. Surfacing it as a secondary action under the nsec/seed input
        // keeps the entry screen clean for the 98% of users who don't run a
        // remote signer.
        domLoginBunkerStartBtn.onclick = showBunkerForm;
    }
    if (domLoginBunkerCopyBtn) {
        domLoginBunkerCopyBtn.onclick = async () => {
            if (!strBunkerNostrConnectUrl) return;
            try {
                await navigator.clipboard.writeText(strBunkerNostrConnectUrl);
                domLoginBunkerCopyBtn.classList.add('copied');
                domLoginBunkerCopyBtn.textContent = 'Copied — paste in your signer';
                setTimeout(() => {
                    domLoginBunkerCopyBtn.classList.remove('copied');
                    domLoginBunkerCopyBtn.textContent = 'Copy connection link';
                }, 2500);
            } catch (err) {
                if (domLoginBunkerStatus) {
                    domLoginBunkerStatus.textContent = 'Could not copy to clipboard';
                    domLoginBunkerStatus.className = 'login-bunker-status error';
                }
            }
        };
    }
    domLoginBackBtn.onclick = async () => {
        // Add Profile flow back has two cases — independent of which sub-
        // screen the user happens to be on (start / import / encryption /
        // welcome). Without this, backing out from the encryption screen
        // after Create Account left both `domLoginStart` and
        // `domLoginEncrypt` visible at the same time.
        //
        //   - Browsing (not committed): the original session is still alive
        //     in memory. Soft-restore the main UI; no backend touch, no
        //     reload, the user keeps their decrypted keys + listeners.
        //
        //   - Committed: enter_add_account_mode already tore the session
        //     down. We have to write the previous-account marker back and
        //     reload so the next boot lands on the original account.
        if (addAccountFlow.active) {
            if (!addAccountFlow.committed) {
                addAccountFlow.restore();
                return;
            }
            const target = addAccountFlow.backTarget();
            try {
                if (target) {
                    await invoke('set_active_account', { npub: target });
                }
            } catch (e) {
                console.error('[add-account] restore marker failed:', e);
                popupConfirm('Could not return to your account', String(e), true);
                return;
            }
            addAccountFlow.finish();
            window.location.reload();
            return;
        }
        // If the bunker form was visible, the user is bailing out of a
        // staged-but-not-committed session — drain it on the backend so the
        // next attempt doesn't see a leaked NOSTR_CLIENT. No-op when no
        // staged session exists.
        const wasOnBunkerForm = domLoginBunker
            && !domLoginBunker.classList.contains('is-hidden')
            && domLoginBunker.style.display !== 'none';
        if (wasOnBunkerForm) {
            invoke('cancel_bunker_session').catch((err) => {
                console.warn('[back] cancel_bunker_session failed:', err);
            });
        }
        // Reauth path: we're inside an active session, came from Settings /
        // Chats. Restore the panel the user was on; don't fall through to
        // the login-start picker.
        if (wasOnBunkerForm && bunkerReauthOrigin) {
            const origin = bunkerReauthOrigin;
            hideBunkerForm();
            if (domLoginBackBar) domLoginBackBar.style.display = 'none';
            const loginForm = document.getElementById('login-form');
            if (loginForm) loginForm.classList.remove('has-back-bar');
            if (domLogin) domLogin.style.display = 'none';
            bunkerReauthOrigin = null;
            if (origin === 'settings' && typeof openSettings === 'function') {
                openSettings();
            } else if (typeof closeChat === 'function') {
                closeChat();
            }
            return;
        }
        // Regular login back: collapse every sub-screen back to the start
        // picker. Encrypt + welcome were missing here, which is what made
        // the post-commit Add Profile case render two panels at once.
        domLoginImport.style.display = 'none';
        domLoginInvite.style.display = 'none';
        domLoginEncrypt.style.display = 'none';
        domLoginWelcome.style.display = 'none';
        hideBunkerForm();
        domLoginBackBar.style.display = 'none';
        domLoginStart.style.display = '';
        domLoginInput.value = '';
        document.getElementById('login-form').classList.remove('has-back-bar');
        // Re-reveal the picker pill if we have ≥2 accounts on disk. The
        // Login button's onclick hides the picker (the user is about to
        // import a key, so it'd be confusing to show), and without this
        // restore the picker stays hidden after the user backs out —
        // effectively removing their ability to switch accounts from the
        // start screen without restarting the app.
        if (typeof loginPicker !== 'undefined'
            && loginPicker.accounts && loginPicker.accounts.length >= 2) {
            loginPicker.show(loginPicker.activeNpub);
        }
    };
    domLoginBtn.onclick = async () => {
        // Import and derive our keys
        try {
            // Add Profile commit point: tear down the existing session
            // before importing the new key.
            if (addAccountFlow.active) await addAccountFlow.commit();

            const { public: pubKey, existing } = await invoke("login", { importKey: domLoginInput.value.trim() });
            strPubkey = pubKey;

            // Pasted key matches an account already on disk; the backend has
            // armed `session_reload` to swap into it. Skip the encryption-
            // setup flow — the boot path will load the stored credentials.
            if (existing) return;

            // Connect to Nostr
            await invoke("connect");

            // Skip invite flow - go directly to encryption (key stays backend-only)
            openEncryptionFlow(false);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    }
    if (domLoginBunkerConnectBtn) {
        domLoginBunkerConnectBtn.onclick = async () => {
            const url = (domLoginBunkerUrlInput?.value || '').trim();
            if (!url.toLowerCase().startsWith('bunker://')) {
                domLoginBunkerStatus.textContent = 'Must start with bunker://';
                domLoginBunkerStatus.className = 'login-bunker-status error';
                return;
            }
            // Disable inputs while the bunker handshake runs (5–10s typical
            // while the user taps "approve" on their signer). Re-enable on
            // failure so they can retry without leaving the screen.
            const _disable = (v) => {
                domLoginBunkerConnectBtn.disabled = v;
                domLoginBunkerUrlInput.disabled = v;
                domLoginBunkerStartBtn && (domLoginBunkerStartBtn.disabled = v);
                if (domLoginBunkerCopyBtn) domLoginBunkerCopyBtn.disabled = v;
            };
            _disable(true);
            domLoginBunkerStatus.textContent = 'Connecting to signer…';
            domLoginBunkerStatus.className = 'login-bunker-status connecting';
            try {
                if (addAccountFlow.active) await addAccountFlow.commit();
                const { public: pubKey, existing } = await invoke('connect_bunker', {
                    bunkerUrl: url,
                });
                strPubkey = pubKey;
                domLoginBunkerUrlInput.value = '';
                if (existing) {
                    // Bunker identity matches an existing account; backend has
                    // armed `session_reload`. Just hide the form — the document
                    // reload will switch into the stored account.
                    domLoginBunkerStatus.textContent = 'Account already added — switching…';
                    domLoginBunkerStatus.className = 'login-bunker-status online';
                    hideBunkerForm();
                    return;
                }
                domLoginBunkerStatus.textContent = 'Connected. Choosing security…';
                domLoginBunkerStatus.className = 'login-bunker-status online';
                // UI advances first; relay connect runs in the background so a
                // hang there doesn't strand the user on the bunker screen.
                hideBunkerForm();
                openEncryptionFlow(false);
                invoke('connect').catch((err) => {
                    console.warn('[connect_bunker] connect() failed:', err);
                });
            } catch (e) {
                domLoginBunkerStatus.textContent = String(e);
                domLoginBunkerStatus.className = 'login-bunker-status error';
                _disable(false);
            }
        };
    }
    domChatBackBtn.onclick = closeChat;
    domChatBookmarksBtn.onclick = () => {
        openChat(strPubkey);
    };
    domChatNewBackBtn.onclick = closeChat;

    // Chat-header overflow menu — dropdown of chat-scoped actions. Currently
    // hosts "Change Wallpaper" for DM chats. Group chats don't get wallpapers
    // by design, so the option only renders when the open chat is a DM.
    const domChatMenuBtn = document.getElementById('chat-menu-btn');
    if (domChatMenuBtn) {
        domChatMenuBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            const rect = domChatMenuBtn.getBoundingClientRect();
            const items = buildChatMenuItems(getChat(strOpenChat));
            if (!items.length) return;
            showContextMenu({ x: rect.right, y: rect.bottom + 4, items });
        });
    }

    // Wallpaper edit UI — header Cancel/Save overlay, bottom sliders.
    const wallpaperEditSave = document.getElementById('wallpaper-edit-save-btn');
    const wallpaperEditCancel = document.getElementById('wallpaper-edit-cancel-btn');
    const wallpaperBlurSlider = document.getElementById('wallpaper-blur-slider');
    const wallpaperDimSlider = document.getElementById('wallpaper-dim-slider');
    if (wallpaperEditSave) {
        wallpaperEditSave.onclick = () => confirmWallpaperChange();
    }
    if (wallpaperEditCancel) {
        wallpaperEditCancel.onclick = () => cancelWallpaperChange();
    }
    if (wallpaperBlurSlider) {
        wallpaperBlurSlider.addEventListener('input', onWallpaperSliderInput);
    }
    if (wallpaperDimSlider) {
        wallpaperDimSlider.addEventListener('input', onWallpaperSliderInput);
    }

    // Hidden Android file input for wallpaper picking. The HTML <input
    // type="file"> hands us a Blob (with content-URI semantics on Android),
    // which we read into bytes and forward to the backend.
    const wallpaperFileInput = document.createElement('input');
    wallpaperFileInput.type = 'file';
    wallpaperFileInput.accept = 'image/*';
    wallpaperFileInput.style.display = 'none';
    document.body.appendChild(wallpaperFileInput);
    wallpaperFileInput.onchange = async (e) => {
        const file = e.target.files?.[0];
        wallpaperFileInput.value = '';
        if (!file || !strOpenChat) return;
        try {
            const buf = new Uint8Array(await file.arrayBuffer());
            const result = await invoke('preview_wallpaper', {
                chatId: strOpenChat,
                bytes: Array.from(buf),
                filename: file.name,
            });
            await applyWallpaperPreview(strOpenChat, result);
        } catch (err) {
            popupConfirm('Couldn’t use that image', String(err), true);
        }
    };
    window.__wallpaperFileInput = wallpaperFileInput;
    
    // Add scroll event listener for procedural message loading + intent tracking
    let scrollTimeout;
    domChatMessages.addEventListener('scroll', () => {
        handleChatScrollIntent();
        if (scrollTimeout) clearTimeout(scrollTimeout);
        scrollTimeout = setTimeout(() => {
            handleProceduralScroll();
        }, 100);
    });
    // Mark user-initiated scroll intent. handleChatScrollIntent only updates
    // chatPinnedToBottom when a scroll event lands within USER_SCROLL_WINDOW_MS
    // of one of these inputs, so reflow-driven and programmatic scrolls
    // (softChatScroll → scrollToBottom) can't unpin a user who's at bottom
    // and shouldn't pin a user who is reading above.
    domChatMessages.addEventListener('wheel', markUserScrollIntent, { passive: true });
    domChatMessages.addEventListener('touchstart', markUserScrollIntent, { passive: true });
    domChatMessages.addEventListener('touchmove', markUserScrollIntent, { passive: true });
    domChatMessages.addEventListener('keydown', (e) => {
        if (['PageUp', 'PageDown', 'ArrowUp', 'ArrowDown', 'Home', 'End', ' ', 'Spacebar'].includes(e.key)) {
            markUserScrollIntent();
        }
    });
    // Scroll-to-bottom button is an explicit "I want to be at bottom" signal.
    if (domChatMessagesScrollReturnBtn) {
        domChatMessagesScrollReturnBtn.addEventListener('click', () => {
            chatPinnedToBottom = true;
            syncBackendActiveChat();
        });
    }
    domChatNewStartBtn.onclick = () => {
        let inputValue = domChatNewInput.value.trim();
        // A pasted Community invite link → preview + join flow (not a DM).
        if (isCommunityInviteUrl(inputValue)) {
            domChatNewInput.value = ``;
            previewAndJoinCommunityLink(inputValue);
            return;
        }
        // Parse npub from vectorapp.io profile URL if pasted
        const profileUrlMatch = inputValue.match(/https?:\/\/vectorapp\.io\/profile\/(npub1[a-z0-9]{58})/i);
        if (profileUrlMatch) {
            inputValue = profileUrlMatch[1];
        }
        openChat(inputValue);
        domChatNewInput.value = ``;
    };
    domChatNewInput.onkeydown = async (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domChatNewStartBtn.click();
        }
    };
    domChatNewInput.addEventListener('input', function() {
        domChatNewStartBtn.style.display = this.value.length > 0 ? '' : 'none';
    });

    // Tooltip for help icon
    document.querySelector('.chat-new-help-link').addEventListener('mouseenter', function() {
    showGlobalTooltip('Visit the Vector Privacy Docs', this);
    });
    document.querySelector('.chat-new-help-link').addEventListener('mouseleave', hideGlobalTooltip);

    domChatMessageInputCancel.onclick = () => {
        // Cancel edit mode if active, otherwise cancel reply
        if (strCurrentEditMessageId) {
            cancelEdit();
        } else {
            cancelReply();
        }
    };

    // Hook up a scroll handler in the chat to display UI elements at certain scroll depths
    createScrollHandler(domChatMessages, domChatMessagesScrollReturnBtn, {
        threshold: 500,
        isPinned: () => chatPinnedToBottom,
        onClick: clearUnreadBelow,
    });

    // Hook up an in-chat File Upload listener
    const isAndroid = platformFeatures.os === 'android';
    
    if (isAndroid) {
        // On Android, use a hidden file input to leverage WebView's built-in file picker
        // This handles content URI permissions correctly
        const androidFileInput = document.createElement('input');
        androidFileInput.type = 'file';
        androidFileInput.style.display = 'none';
        androidFileInput.accept = '*/*';
        document.body.appendChild(androidFileInput);
        
        androidFileInput.onchange = async (e) => {
            const file = e.target.files?.[0];
            if (file) {
                // Reset reply selection while passing a copy of the reference to the backend
                const strReplyRef = strCurrentReplyReference;
                cancelReply();
                
                const fileName = file.name;
                const ext = fileName.split('.').pop()?.toLowerCase() || '';
                
                // Open preview with the File object directly (more efficient)
                await openFilePreviewWithFile(file, fileName, ext, strOpenChat, strReplyRef);
            }
            // Reset the input so the same file can be selected again
            androidFileInput.value = '';
        };
        
        // Toggle attachment panel when clicking the add-file button
        domChatMessageInputFile.onclick = () => {
            toggleAttachmentPanel();
        };

        // Handle File button in attachment panel (Android)
        domAttachmentPanelFile.onclick = () => {
            closeAttachmentPanel();
            androidFileInput.click();
        };
    } else {
        // Toggle attachment panel when clicking the add-file button
        domChatMessageInputFile.onclick = () => {
            toggleAttachmentPanel();
        };

        // Handle File button in attachment panel (Desktop - use Tauri dialog)
        domAttachmentPanelFile.onclick = async () => {
            closeAttachmentPanel();
            let filepath = await selectFile();
            if (filepath) {
                // Reset reply selection while passing a copy of the reference to the backend
                const strReplyRef = strCurrentReplyReference;
                cancelReply();
                // Show file preview instead of sending directly
                await openFilePreview(filepath, strOpenChat, strReplyRef);
            }
        };

        // Show Folder button on desktop only
        if (domAttachmentPanelFolder) {
            domAttachmentPanelFolder.style.display = '';
            domAttachmentPanelFolder.onclick = async () => {
                closeAttachmentPanel();
                let folderPath = await selectFolder();
                if (folderPath) {
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    await openFolderZipPreview(folderPath, strOpenChat, strReplyRef);
                }
            };
        }
    }

    // Handle Mini Apps button in attachment panel - shows the Mini Apps list view
    domAttachmentPanelMiniApps.onclick = async () => {
        await showAttachmentPanelMiniApps();
    };

    // Handle Back button in Mini Apps view - returns to main attachment panel
    domAttachmentPanelBack.onclick = () => {
        showAttachmentPanelMain();
    };

    // Handle search input in Mini Apps view
    if (domMiniAppsSearch) {
        domMiniAppsSearch.addEventListener('input', (e) => {
            filterMiniApps(e.target.value);
        });
    }

    // Setup hold-to-edit mode for Mini Apps
    setupMiniAppsEditMode();

    // Mini App Launch Dialog event handlers
    domMiniAppLaunchCancel.onclick = closeMiniAppLaunchDialog;
    domMiniAppLaunchSolo.onclick = playMiniAppSolo;
    domMiniAppLaunchInvite.onclick = playMiniAppAndInvite;
    
    // Close dialog when clicking outside
    domMiniAppLaunchOverlay.onclick = (e) => {
        if (e.target === domMiniAppLaunchOverlay) {
            closeMiniAppLaunchDialog();
        }
    };

    // Marketplace event handlers
    if (domAttachmentPanelMarketplace) {
        domAttachmentPanelMarketplace.onclick = async () => {
            closeAttachmentPanel();
            showMarketplacePanel();
        };
    }

    if (domMarketplaceBackBtn) {
        domMarketplaceBackBtn.onclick = () => {
            hideMarketplacePanel();
        };
    }

    // Hook up an in-chat File Paste listener
    document.onpaste = async (evt) => {
        if (strOpenChat) {
            // Check if the clipboard data contains an image
            const arrItems = Array.from(evt.clipboardData.items);
            const imageItem = arrItems.find(item => item.type.startsWith('image/'));
            if (imageItem) {
                evt.preventDefault();

                // Get the image as a blob
                const blob = imageItem.getAsFile();
                if (!blob) return;

                // Read the blob as bytes
                const arrayBuffer = await blob.arrayBuffer();
                const bytes = new Uint8Array(arrayBuffer);

                // Determine file extension from MIME type
                const mimeType = imageItem.type;
                let ext = 'png'; // Default
                if (mimeType.includes('jpeg') || mimeType.includes('jpg')) {
                    ext = 'jpg';
                } else if (mimeType.includes('gif')) {
                    ext = 'gif';
                } else if (mimeType.includes('webp')) {
                    ext = 'webp';
                } else if (mimeType.includes('png')) {
                    ext = 'png';
                } else if (mimeType.includes('tiff')) {
                    ext = 'tiff';
                } else if (mimeType.includes('bmp')) {
                    ext = 'bmp';
                }

                // Generate a filename
                const fileName = `pasted_image.${ext}`;

                // Get reply reference before opening preview
                const strReplyRef = strCurrentReplyReference;
                
                // Cancel the reply UI (the reference is passed to the preview)
                cancelReply();

                // Open the file preview dialog with the pasted image bytes
                openFilePreviewWithBytes(bytes, fileName, ext, bytes.length, strOpenChat, strReplyRef);
            }
        }
    };

// Unified message sending function
async function sendMessage(messageText) {
    if (!messageText || !messageText.trim()) return;

    // Clean tracking parameters from any URLs in the message for privacy (if enabled)
    let cleanedText = messageText.trim();
    if (fStripTrackingEnabled) {
        const urlPattern = /(https?:\/\/[^\s<>"{}|\\^`\[\]]+)/gi;
        cleanedText = cleanedText.replace(urlPattern, (match) => {
            try {
                return cleanTrackingFromUrl(match);
            } catch (e) {
                // If cleaning fails, return original URL
                return match;
            }
        });
    }

    // Replace @DisplayName with @npub1... for any tracked mentions
    if (mentionCtrl) {
        const tracked = mentionCtrl.getMentions();
        // Sort by name length descending so longer names are replaced first,
        // preventing partial matches (e.g. "Al" matching inside "Alice")
        const sorted = tracked.slice().sort((a, b) => b.name.length - a.name.length);
        for (const m of sorted) {
            // Escape regex special chars in the display name
            const escaped = m.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
            // Match @Name only at word boundaries to avoid substring collisions
            const re = new RegExp('(?<=^|\\s)@' + escaped + '(?=\\s|[.,!?;:]|$)', 'g');
            cleanedText = cleanedText.replace(re, '@' + m.npub);
        }
    }

    // Check if we're in edit mode
    if (strCurrentEditMessageId) {
        // Don't send if content hasn't changed
        if (cleanedText === strCurrentEditOriginalContent) {
            cancelEdit();
            return;
        }

        // Clear input and show editing state
        domChatMessageInput.value = '';
        resetSendMicButtons(); // Immediately reset to mic button (avoids animation race)
        domChatMessageInput.style.height = '';
        domChatMessageInput.style.overflowY = 'hidden';
        domChatMessageInput.setAttribute('placeholder', 'Saving edit...');

        try {
            const editMsgId = strCurrentEditMessageId;
            const originalContent = strCurrentEditOriginalContent;
            cancelEdit();

            // Instantly update the message in the DOM for responsive UX
            const msgElement = document.getElementById(editMsgId);
            if (msgElement) {
                const spanMessage = msgElement.querySelector('.dmsg-text');
                if (spanMessage) {
                    spanMessage.innerHTML = parseMarkdown(cleanedText.trim());
                    linkifyUrls(spanMessage);
                    processInlineImages(spanMessage);
                    renderMentions(spanMessage, false);
                    twemojify(spanMessage);
                }
                // Add edited indicator if not already present
                const dmsgContent = msgElement.querySelector('.dmsg-content');
                if (dmsgContent && !dmsgContent.querySelector('.dmsg-edited')) {
                    const spanEdited = document.createElement('span');
                    spanEdited.classList.add('dmsg-edited', 'btn');
                    spanEdited.textContent = '(edited)';
                    spanEdited.setAttribute('data-msg-id', editMsgId);
                    spanEdited.title = 'Click to view edit history';
                    dmsgContent.appendChild(spanEdited);
                }
            }

            // Update in cache as well
            const chat = arrChats.find(c => c.id === strOpenChat);
            if (chat) {
                const msg = chat.messages.find(m => m.id === editMsgId);
                if (msg) {
                    // Build edit history if it doesn't exist yet
                    if (!msg.edit_history) {
                        msg.edit_history = [];
                        // Add original content as first entry
                        msg.edit_history.push({
                            content: originalContent,
                            edited_at: msg.created_at * 1000 // Convert to milliseconds
                        });
                    }
                    // Add new edit entry
                    msg.edit_history.push({
                        content: cleanedText,
                        edited_at: Date.now()
                    });
                    msg.content = cleanedText;
                    msg.edited = true;
                }
            }

            // Send edit to backend (fire and forget for responsiveness). Community
            // channels use their own envelope path (the edit rides a kind-3302 event).
            const editChatForRoute = arrChats.find(c => c.id === strOpenChat);
            const editPromise = editChatForRoute?.chat_type === 'Community'
                ? invoke('edit_community_message', { channelId: strOpenChat, messageId: editMsgId, newContent: cleanedText })
                : invoke('edit_message', { messageId: editMsgId, chatId: strOpenChat, newContent: cleanedText });
            editPromise.catch(e => {
                console.error('Failed to edit message:', e);
                // Optionally: revert the UI change on failure
            });

            nLastTypingIndicator = 0;
        } catch(e) {
            console.error('Failed to edit message:', e);
        }
        return;
    }

    // Clear input and show sending state
    domChatMessageInput.value = '';
    resetSendMicButtons(); // Immediately reset to mic button (avoids animation race)
    domChatMessageInput.style.height = ''; // Reset textarea height
    domChatMessageInput.style.overflowY = 'hidden'; // Reset overflow
    domChatMessageInput.setAttribute('placeholder', 'Sending...');

    try {
        const replyRef = strCurrentReplyReference;
        cancelReply();

        // Send message (unified function handles both DMs and MLS groups)
        await message(strOpenChat, cleanedText, replyRef);

        nLastTypingIndicator = 0;
        if (mentionCtrl) mentionCtrl.clearMentions();
    } catch(e) {
        console.error('Failed to send message:', e);
    }
}

    // Desktop/iOS - traditional keydown approach (not for Android)
    if (platformFeatures.os !== 'android') {
        domChatMessageInput.addEventListener('keydown', async (evt) => {
            // Skip send if mention/emoji selector is consuming this keypress
            if (mentionCtrl && mentionCtrl.isOpen && mentionCtrl.isOpen()) return;
            if (emojiShortcodeCtrl && emojiShortcodeCtrl.isOpen && emojiShortcodeCtrl.isOpen()) return;
            if ((evt.key === 'Enter' || evt.keyCode === 13) && !evt.shiftKey) {
                evt.preventDefault();
                await sendMessage(domChatMessageInput.value);
            }
            // ESC key cancels reply/edit mode
            if (evt.key === 'Escape') {
                if (strCurrentEditMessageId) {
                    cancelEdit();
                } else if (strCurrentReplyReference) {
                    cancelReply();
                }
            }
        });
    }

// --- Mention Selector ---
const mentionCtrl = typeof initMentionSelector === 'function' ? initMentionSelector(
    domChatMessageInput,
    () => {
        const chat = arrChats.find(c => c.id === strOpenChat);
        if (!chat) return [];
        const isCommunity = chat.chat_type === 'Community';
        // Build a map of each participant's most recent message timestamp
        const lastActive = {};
        if (chat.messages) {
            for (let i = chat.messages.length - 1; i >= 0; i--) {
                const m = chat.messages[i];
                const sender = m.npub || (m.mine ? strPubkey : chat.id);
                if (!lastActive[sender]) lastActive[sender] = m.at || 0;
            }
        }
        // Taggable npubs = explicit participants ∪ (for communities) everyone who's posted.
        // Community rosters aren't mirrored into `participants`, so observed senders are the
        // reliable taggable set — without this the selector is empty in Community channels.
        const npubs = new Set(chat.participants || []);
        if (isCommunity) {
            for (const np of Object.keys(lastActive)) npubs.add(np);
        }
        const candidates = [...npubs]
            .filter(npub => npub && npub !== strPubkey && npub.startsWith('npub1'))
            .map(npub => {
                const p = getProfile(npub);
                return {
                    npub,
                    name: p?.nickname || p?.name || npub.substring(0, 12) + '...',
                    avatarSrc: p ? getProfileAvatarSrc(p) : null,
                    lastActive: lastActive[npub] || 0
                };
            })
            .sort((a, b) => b.lastActive - a.lastActive);
        // Disambiguate duplicate display names with a short npub suffix
        const nameCount = {};
        for (const c of candidates) nameCount[c.name] = (nameCount[c.name] || 0) + 1;
        for (const c of candidates) {
            if (nameCount[c.name] > 1) {
                c.name = c.name + ' (~' + c.npub.slice(5, 9) + ')';
            }
        }
        // @everyone: lowest-priority option (bottom of the list), placeholder avatar — the original
        // group design. Offered only to those who can actually use it (owner or admin); a non-admin's
        // @everyone is ignored, so suggesting it would mislead. Roles preload at boot, so this gate is
        // reliable now (the earlier always-show was a stopgap for when admins weren't loaded yet).
        if (isCommunity) {
            const cf = chat.metadata?.custom_fields || {};
            const canPingEveryone = cf.is_owner === 'true' || (chat.metadata?.admins || []).includes(strPubkey);
            if (canPingEveryone) {
                candidates.push({ npub: 'everyone', name: 'everyone', avatarSrc: null, lastActive: -1 });
            }
        }
        return candidates;
    },
    document.getElementById('chat-box')
) : null;

// --- Emoji Shortcode Selector ---
const emojiShortcodeCtrl = typeof initEmojiShortcodeSelector === 'function'
    ? initEmojiShortcodeSelector(domChatMessageInput, document.getElementById('chat-box'))
    : null;

/**
 * Immediately reset send/mic buttons to mic state (no animation)
 * Used after sending messages to avoid animation race conditions
 */
function resetSendMicButtons() {
    // Clear any animation classes
    domChatMessageInputSend.classList.remove('active', 'button-swap-in', 'button-swap-out');
    domChatMessageInputVoice.classList.remove('button-swap-in', 'button-swap-out');
    // Set correct display states
    domChatMessageInputSend.style.display = 'none';
    domChatMessageInputVoice.style.display = '';
}

    // Hook up an 'input' listener on the Message Box for typing indicators
domChatMessageInput.oninput = async () => {
    // Auto-resize the textarea based on content
    autoResizeChatInput();
    
    // Toggle send button active state based on text content
    const hasText = domChatMessageInput.value.trim().length > 0;
    if (hasText) {
        // Swap: Hide mic, show send button with animation
        if (domChatMessageInputVoice.style.display !== 'none') {
            domChatMessageInputVoice.classList.add('button-swap-out');
            domChatMessageInputVoice.addEventListener('animationend', () => {
                domChatMessageInputVoice.style.display = 'none';
                domChatMessageInputVoice.classList.remove('button-swap-out');
                domChatMessageInputSend.style.display = '';
                domChatMessageInputSend.classList.add('button-swap-in');
                domChatMessageInputSend.addEventListener('animationend', () => {
                    domChatMessageInputSend.classList.remove('button-swap-in');
                }, { once: true });
            }, { once: true });
        }
        domChatMessageInputSend.classList.add('active');
    } else {
        // Swap: Hide send, show mic button with animation
        if (domChatMessageInputSend.style.display !== 'none') {
            domChatMessageInputSend.classList.add('button-swap-out');
            domChatMessageInputSend.classList.remove('active');
            domChatMessageInputSend.addEventListener('animationend', () => {
                domChatMessageInputSend.style.display = 'none';
                domChatMessageInputSend.classList.remove('button-swap-out');
                domChatMessageInputVoice.style.display = '';
                domChatMessageInputVoice.classList.add('button-swap-in');
                domChatMessageInputVoice.addEventListener('animationend', () => {
                    domChatMessageInputVoice.classList.remove('button-swap-in');
                }, { once: true });
            }, { once: true });
        }
    }

    // Send a Typing Indicator only when content actually changes and setting is enabled
    // Don't send typing indicators while editing a message (it's not a new message)
    if (fSendTypingIndicators && !strCurrentEditMessageId && nLastTypingIndicator + 30000 < Date.now()) {
        nLastTypingIndicator = Date.now();
        await invoke("start_typing", { receiver: strOpenChat });
    }
};

    // Hook up the send button click handler (handles both text and voice messages)
    domChatMessageInputSend.onclick = async () => {
        // Check if we're in voice preview mode first
        if (recorder.isInPreview) {
            const sent = recorder.send();
            if (sent && strOpenChat) {
                domChatMessageInput.setAttribute('placeholder', 'Sending...');
                try {
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    await invoke('send_recording', {
                        receiver: strOpenChat,
                        repliedTo: strReplyRef
                    });
                } catch (e) {
                    if (!e || !e.toString().includes('Upload cancelled')) {
                        popupConfirm(e, '', true, '', 'vector_warning.svg');
                    }
                }
                domChatMessageInput.setAttribute('placeholder', 'Enter message...');
                nLastTypingIndicator = 0;
            }
            return;
        }
        
        // Otherwise, handle normal text message send
        const messageText = domChatMessageInput.value;
        if (messageText && messageText.trim()) {
            await sendMessage(messageText);
        }
    };

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
            if (!wasActive && isWindowActive()) onWindowResumed();
            syncBackendActiveChat();
        });

        document.addEventListener('visibilitychange', () => {
            const wasActive = isWindowActive();
            documentVisible = !document.hidden;
            if (!wasActive && isWindowActive()) onWindowResumed();
            syncBackendActiveChat();
        });
    }

    // Hook up our voice message recorder with Telegram-like UX
    const recorder = new VoiceRecorder(domChatMessageInputVoice, domChatInputContainer);
    
    // Handle state changes for UI updates
    recorder.onStateChange = (newState, oldState) => {
        if (newState === 'idle') {
            // Reset placeholder when returning to idle
            domChatMessageInput.setAttribute('placeholder', 'Enter message...');
        } else if (newState === 'recording' || newState === 'locked') {
            // Clear input and show recording status
            domChatMessageInput.value = '';
            domChatMessageInput.style.height = '';
            domChatMessageInput.style.overflowY = 'hidden';
        }
    };
    
    // Handle cancel callback
    recorder.onCancel = () => {
        domChatMessageInput.setAttribute('placeholder', 'Enter message...');
        cancelReply();
    };

    // Initialize voice transcription with default model
    window.cTranscriber = new VoiceTranscriptionUI();
    window.voiceSettings = new VoiceSettings();

    // Only load whisper models if transcription is supported
    if (platformFeatures.transcription) {
        await window.voiceSettings.loadWhisperModels();
    }
    
    window.voiceSettings.initVoiceSettings();

    // Initialize settings
    await initSettings();

    // Hook up our "Help Prompts" to give users easy feature explainers in ambiguous or complex contexts
    // Note: since some of these overlap with Checkbox Labels: we prevent event bubbling so that clicking the Info Icon doesn't also trigger other events
    domSettingsWhisperModelInfo.onclick = (e) => {
        popupConfirm('Vector Voice AI Model', 'The Vector Voice AI model <b>determines the Quality of your transcriptions.</b><br><br>A larger model will provide more accurate transcriptions & translations, but require more Disk Space, Memory and CPU power to run.', true);
    };
    domSettingsWhisperAutoTranslateInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Vector Voice Translations', 'Vector Voice AI can <b>automatically detect non-English languages and translate them in to English text for you.</b><br><br>You can decide whether Vector Voice transcribes in to their native spoken language, or instead translates in to English on your behalf.', true);
    };
    domSettingsWhisperAutoTranscribeInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Vector Voice Transcriptions', 'Vector Voice AI can <b>automatically transcribe incoming Voice Messages</b> for immediate reading, without needing to listen.<br><br>You can decide whether Vector Voice transcribes automatically, or if you prefer to transcribe each message explicitly.', true);
    };
    domSettingsPrivacyWebPreviewsInfo.onclick = async (e) => {
        e.preventDefault();
        e.stopPropagation();
        // Render contextually based on Tor preference. When Tor is enabled,
        // every preview fetch is forced through Tor (or blackholes during
        // bootstrap) by the network failsafe — no clearnet leak path.
        let torEnabled = false;
        try {
            const torState = await invoke('tor_get_state');
            torEnabled = !!(torState && torState.enabled);
        } catch (_) { /* fall through to default warning */ }
        const message = torEnabled
            ? 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>You have <b>Tor enabled</b>, so preview fetches route through the Tor network. Your IP address stays hidden from the linked sites.'
            : 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>This may expose your IP address to the linked sites. <b>Use Tor</b> (Privacy, Route traffic through Tor) <b>or a VPN</b> if that\'s a concern.';
        popupConfirm('Web Previews', message, true);
    };
    domSettingsPrivacyStripTrackingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Strip Tracking Markers', 'When enabled, Vector will <b>automatically remove tracking markers</b> from URLs before displaying or sending them.<br><br>This helps reduce your footprint and enhances your privacy with no loss in functionality, only disable if you know what you\'re doing.', true);
    };
    domSettingsPrivacySendTypingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Send Typing Indicators', 'When enabled, Vector will <b>notify your contacts when you are typing</b> a message to them.<br><br>Disable this if you prefer to type without others knowing you are composing a message.', true);
    };
    if (domSettingsPrivacyTorInfo) {
        domSettingsPrivacyTorInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            // Trademark notice + non-endorsement disclaimer included per the
            // Tor Project's trademark policy (https://www.torproject.org/about/trademark/).
            popupConfirm(
                'Route traffic through Tor',
                'When enabled, Vector routes <b>all TCP traffic</b> (Nostr relays, Blossom uploads, link previews, image fetches) through the Tor network using an embedded Arti client.<br><br>'
                + 'This hides your IP address from relays and remote servers, at the cost of slower connections (Tor circuits add latency).<br><br>'
                + '<small style="opacity: 0.6;">Tor and the Tor logo are trademarks of The Tor Project; all rights reserved. More information at <b>torproject.org</b>. Vector is not endorsed or sponsored by, or affiliated with, The Tor Project.</small>',
                true
            );
        };
    }
    // Open torproject.org when the small attribution logo is clicked.
    const torAttributionLink = document.getElementById('tor-attribution-link');
    if (torAttributionLink) {
        torAttributionLink.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            openUrl('https://torproject.org');
        };
    }
    domSettingsDisplayImageTypesInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Display Image Types', 'When enabled, images in chat will display a <b>small badge showing the file type</b> (e.g., PNG, GIF, WEBP) in the corner.<br><br>This helps identify image formats at a glance.', true);
    };
    domSettingsChatBgInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Background Wallpaper', 'This feature enables and disables background images inside of Chats (Private & Group Chats).<br><br>Only applies to certain themes.', true);
    };
    const domSettingsBatteryBgServiceInfo = document.getElementById('battery-bg-service-info');
    if (domSettingsBatteryBgServiceInfo) {
        domSettingsBatteryBgServiceInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm('Run in Background', 'When enabled, Vector runs a <b>background service</b> to keep your connection alive and deliver <b>instant notifications</b>.<br><br>This requires disabling Android\'s battery optimization for Vector, otherwise the system may kill the service and delay or prevent notifications.', true);
        };
    }
    domSettingsNotifMuteInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Mute Notification Sounds', 'When enabled, Vector will <b>not play any notification sounds</b> for incoming messages.<br><br>You will still receive visual notifications and badges.', true);
    };
    domSettingsNotifMuteEveryoneInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Mute @everyone Pings', 'When enabled, <b>@everyone</b> mentions from group admins will <b>not bypass</b> your group mute setting.<br><br>By default, @everyone pings from admins will notify you even if the group is muted.', true);
    };

    domSettingsExportAccountInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Export Account', 'Export Account will display a backup of your encryption keys. Keep it safe to restore your account later.', true);
    };

    if (domSettingsChangePinInfo) {
        domSettingsChangePinInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm(
                fSecurityType === 'password' ? 'Change Password' : 'Change PIN',
                fSecurityType === 'password'
                    ? 'Your password encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new password.'
                    : 'Your PIN encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new PIN.',
                true
            );
        };
    }

    // Info button for Copy Logs
    const domCrashLogInfo = document.getElementById('crash-log-info');
    if (domCrashLogInfo) {
        domCrashLogInfo.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            popupConfirm(
                'Logs',
                'Copies error logs and crash details to your clipboard.<br><br>Share with developers when reporting bugs to help diagnose issues.',
                true
            );
        };
    }

    domSettingsLogoutInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Logout', 'Logout will erase the local database and remove all stored keys. You will lose access to group chats unless you have a backup.', true);
    };

    if (domRemoteSignerReauthBtn) {
        domRemoteSignerReauthBtn.onclick = (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (typeof window.showBunkerForm === 'function') {
                window.showBunkerForm('reauth');
            }
        };
    }

    // Donors & Contributors info button
    domSettingsDonorsInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Donors & Contributors', 'Your support keeps Vector Privacy independent, open-source, and free. If you would be interested in donating, please visit our GitBook to learn more.<br><br>We extend our sincere gratitude to these supporters for helping make Vector possible.', true);
    };

    // PIVX donor logo click - opens pivx.org
    domDonorPivx.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        openUrl('https://pivx.org');
    };

    // Gitcoin donor logo click - opens gitcoin.co
    domDonorGitcoin.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        openUrl('https://www.gitcoin.co');
    };

    // Footer Hyperlinks
    document.getElementById('footer-donate').onclick = (e) => {
        e.preventDefault();
        openUrl('https://vector-privacy.gitbook.io/vector-privacy/vector-messenger/more/donations');
    };
    document.getElementById('footer-gitbook').onclick = (e) => {
        e.preventDefault();
        openUrl('https://docs.vectorapp.io');
    };
    document.getElementById('footer-privacy').onclick = (e) => {
        e.preventDefault();
        openUrl('https://vectorapp.io/privacy-policy');
    };

});

// Listen for app-wide click interations
document.addEventListener('click', (e) => {
    // If we're clicking the emoji search, don't close it!
    if (e.target === emojiSearch) return;

    // If we're clicking an <a> link, handle it with our openUrl function
    if (e.target.tagName === 'A' && e.target.href) {
        e.preventDefault();
        return openUrl(e.target.href);
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

    // If we're clicking an edited indicator, show the edit history
    if (e.target.classList.contains("dmsg-edited")) {
        const msgId = e.target.getAttribute('data-msg-id');
        if (msgId) {
            showEditHistory(msgId, e.target);
        }
    }

    // If we're clicking a File Reveal button, reveal/open the file. Android has
    // no "reveal in folder", so open it with the user's chosen app instead.
    if (e.target.getAttribute('filepath')) {
        const filepath = e.target.getAttribute('filepath');
        if (platformFeatures.os === 'android') {
            return invoke('open_attachment', { path: filepath });
        }
        return revealItemInDir(filepath);
    }

    // If we're clicking a Reply context, center the referenced message in view
    {
        const replyEl = e.target.closest('.dmsg-reply');
        if (replyEl) {
            // The `substring(2)` removes the `r-` prefix
            const targetMsgId = replyEl.id.substring(2);
            const domMsg = document.getElementById(targetMsgId);
            if (domMsg) {
                centerInView(domMsg);
                applyHighlight(domMsg, 'jumped');
            } else {
                // Message not rendered yet, load it and surrounding messages
                loadAndScrollToMessage(targetMsgId);
            }
            return;
        }
    }

    // If we're clicking a Metadata Preview, open it's URL, if one is attached
    {
        const previewEl = e.target.closest('.dmsg-preview');
        if (previewEl) {
            const strURL = previewEl.getAttribute('url');
            if (strURL) openUrl(strURL);
            return;
        }
    }

    // If we're clicking a Payment URI, open it's URL
    if (e.target.getAttribute('pay-uri')) {
        return openUrl(e.target.getAttribute('pay-uri'));
    }

    // If we're clicking a Contact in the main chat list (NOT inside the Create Group panel), open the chat
    const cg = document.getElementById('create-group');
    const inCreateGroup = cg && cg.style.display !== 'none' && cg.contains(e.target);
    const chatlistItem = e.target.closest('.chatlist-contact');
    if (!inCreateGroup && chatlistItem) {
        // Don't open chat if clicking on an invite item, or a community still joining (locked
        // until the control-fold/sync makes it read/writeable).
        if (chatlistItem.classList.contains("chatlist-invite") || chatlistItem.classList.contains("chatlist-joining")) {
            return;
        }
        const chatId = chatlistItem.id.replace('chatlist-', '');
        return openChat(chatId);
    }

    // If we're clicking an Attachment Download button, request the download
    if (e.target.hasAttribute('download')) {
        const attId = e.target.getAttribute('data-attachment-id');
        const dlNpub = e.target.getAttribute('npub');
        const dlMsgId = e.target.getAttribute('msg');
        if (downloadingAttachmentIds.has(attId)) return;
        downloadingAttachmentIds.add(attId);
        // Swap download button for a centered progress spinner overlay
        const overlay = document.createElement('div');
        overlay.className = 'attachment-progress-overlay';
        const spinner = document.createElement('div');
        spinner.className = 'miniapp-downloading-spinner';
        spinner.setAttribute('data-attachment-id', attId);
        spinner.style.cssText = 'width: 48px; height: 48px;';
        overlay.appendChild(spinner);
        e.target.replaceWith(overlay);
        return invoke('download_attachment', { npub: dlNpub, msgId: dlMsgId, attachmentId: attId })
            .catch(() => downloadingAttachmentIds.delete(attId));
    }

    // Click an existing reaction badge to add your own matching reaction (one per emoji per user)
    const clickedReaction = e.target.closest('.reaction');
    if (clickedReaction && isReactionLongPressed()) return;
    if (clickedReaction && !clickedReaction.hasAttribute('data-reacted')) {
        const emoji = clickedReaction.getAttribute('data-emoji');
        const msgId = clickedReaction.getAttribute('data-msg-id');
        if (emoji && msgId) {
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === msgId);
                if (!cMsg) continue;
                // Check if we already reacted with this emoji
                const alreadyReacted = cMsg.reactions.some(r => r.emoji === emoji && r.author_id === strPubkey);
                if (alreadyReacted) break;
                // Mark as reacted to prevent double-clicks before backend re-render
                clickedReaction.setAttribute('data-reacted', 'true');
                // Optimistic UI: increment the displayed count
                const countNode = [...clickedReaction.childNodes].find(n => n.nodeType === Node.TEXT_NODE);
                if (countNode) {
                    const count = parseInt(countNode.textContent.trim()) || 1;
                    countNode.textContent = ` ${count + 1}`;
                }
                reactToMessageRouted(msgId, cChat.id, emoji);
                break;
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

/**
 * Resize certain tricky components (i.e: the Chat Box) on window resizes.
 * 
 * This can also be re-called when some components are spawned, since they can
 * affect the height and width of other components, too.
 */
function adjustSize() {
    // Chat List: resize the list to fit within the screen after the upper Account area
    // Note: no idea why the `- 50px` is needed below, magic numbers, I guess.
    const nNewChatBtnHeight = domChatNewDM?.getBoundingClientRect().height || 0;
    const nNavbarHeight = domNavbar.getBoundingClientRect().height;
    domChatList.style.maxHeight = (window.innerHeight - (domChatList.offsetTop + nNewChatBtnHeight + nNavbarHeight)) + 50 + 'px';

    // Re-calculate chat input size on window resize (text may reflow)
    autoResizeChatInput();

    // If the chat is open, and they've not significantly scrolled up: auto-scroll down to correct against container resizes
    softChatScroll();
}

/**
 * Scrolls the chat to the bottom if the user has not already scrolled upwards substantially.
 * 
 * This is used to correct against container resizes, i.e: if an image loads, or a message is received.
 */
/**
 * Tracks whether the user wants to be pinned to the bottom of the chat.
 *
 * Only flipped by *user-initiated* scrolls — wheel, touch, keyboard. Pure
 * scroll events from a programmatic scrollTo, or from layout reflow as
 * media loads in, are ignored. This separation is what makes the chat
 * "self-heal" during chat-open: every async load fires softChatScroll,
 * which re-snaps to bottom; the resulting scroll event would normally
 * confuse a snapshot-based check during transitional layout, but here
 * it never sees a recent user-input timestamp and so leaves pinned=true
 * alone.
 *
 * Initial true: chat-open paths scroll to bottom synchronously, so the
 * user starts pinned by definition.
 */
let chatPinnedToBottom = true;

// "Window active" = the user can actually see the chat (window focused on
// desktop, page visible on mobile). Real-time arrivals must NOT auto-mark
// as read while the user is tabbed out; the catch-up fires when activity
// resumes (handled in setup_listeners).
let windowFocused = true;
let documentVisible = typeof document !== 'undefined' ? !document.hidden : true;
function isWindowActive() { return windowFocused && documentVisible; }

/** Tell the backend which chat the user is actively watching, so inbound
 *  messages in that chat auto-mark as read on arrival. Bumps badge counts
 *  in lock-step with our FE markAsRead — without this the on_dm_received
 *  task can race ahead and tick the dock badge before markAsRead lands. */
let _lastReportedActiveChat = '__init__';
function syncBackendActiveChat() {
    const id = (strOpenChat && chatPinnedToBottom && isWindowActive()) ? strOpenChat : null;
    if (id === _lastReportedActiveChat) return;
    _lastReportedActiveChat = id;
    invoke('set_active_chat', { chatId: id }).catch(() => { /* best-effort */ });
}

const PIN_THRESHOLD_PX = 80;
// 500ms covers iOS/Android momentum-scroll: scroll events keep firing
// after touchend until momentum decays. A shorter window would miss
// late-firing pxFromBottom updates and leave pinned stale.
const USER_SCROLL_WINDOW_MS = 500;

let lastUserScrollIntentAt = 0;
function markUserScrollIntent() { lastUserScrollIntentAt = Date.now(); }

let unreadBelowCount = 0;
let unreadDividerEl = null;
const domChatScrollReturnBadge = document.getElementById('chat-scroll-return-badge');

/**
 * Insert (or reuse) the "New" divider above the given message element.
 * Persists for the chat session — only the first unread message gets a
 * divider; later messages just stack under it. Cleared by openChat()
 * (close + re-enter) and by sending a message.
 */
function insertUnreadDivider(beforeEl) {
    if (unreadDividerEl || !beforeEl?.parentNode) return;
    const p = document.createElement('p');
    p.classList.add('msg-inline-timestamp', 'unread-divider');
    p.textContent = 'New';
    beforeEl.parentNode.insertBefore(p, beforeEl);
    unreadDividerEl = p;
}
function clearUnreadDivider() {
    if (unreadDividerEl) {
        unreadDividerEl.remove();
        unreadDividerEl = null;
    }
}
function setUnreadBelow(n) {
    unreadBelowCount = Math.max(0, n);
    if (!domChatScrollReturnBadge) return;
    if (unreadBelowCount > 0) {
        domChatScrollReturnBadge.textContent = unreadBelowCount > 99 ? '99+' : String(unreadBelowCount);
        domChatScrollReturnBadge.classList.add('visible');
    } else {
        domChatScrollReturnBadge.textContent = '';
        domChatScrollReturnBadge.classList.remove('visible');
    }
}
function incrementUnreadBelow() { setUnreadBelow(unreadBelowCount + 1); }
function clearUnreadBelow() { setUnreadBelow(0); }

/**
 * Recompute chatPinnedToBottom — but only honour scroll events that
 * trail a recent user input. Programmatic scrollTo() and overflow-anchor
 * adjustments still fire scroll events, but we don't want those to flip
 * the user's intent.
 */
function handleChatScrollIntent() {
    if (!strOpenChat || !domChatMessages) return;
    if (Date.now() - lastUserScrollIntentAt > USER_SCROLL_WINDOW_MS) return;
    const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
    const wasPinned = chatPinnedToBottom;
    chatPinnedToBottom = pxFromBottom < PIN_THRESHOLD_PX;
    // User scrolled themselves back into pin range — clear the badge and
    // advance last_read so the OS unread indicator reflects reality. The
    // divider stays put until the chat is closed.
    if (!wasPinned && chatPinnedToBottom) {
        clearUnreadBelow();
        const currentChat = getChat(strOpenChat);
        if (currentChat?.messages?.length) {
            const latestNonMine = findLatestContactMessage(currentChat.messages);
            if (latestNonMine) markAsRead(currentChat, latestNonMine);
        }
    }
    if (wasPinned !== chatPinnedToBottom) syncBackendActiveChat();
}

function softChatScroll() {
    if (!strOpenChat) return;
    if (!chatPinnedToBottom) return;
    scrollToBottom(domChatMessages, false);
}

window.onresize = adjustSize;

// ===== Create Group: state and helpers =====
/**
 * Selected members (npubs) for the group being created.
 * Keep this decoupled from arrChats.
 */
let arrSelectedGroupMembers = [];
let arrSelectedGroupAdmins = [];
/** Path to the selected group avatar image file (null if none selected) */
let strCreateGroupAvatarPath = null;
/**
 * Tracks whether the user attempted to create the group.
 * Used to only show inline validation after an explicit attempt.
 */
let fCreateGroupAttempt = false;


/**
 * Render the filterable, scrollable contact list with checkboxes.
 * Reuses arrProfiles as the source of truth.
 */
function renderCreateGroupList(filterText = '') {
    if (!domCreateGroupList) return;
    domCreateGroupList.innerHTML = '';

    const f = (filterText || '').trim().toLowerCase();

    // Exclude our own profile from selection
    const mine = arrProfiles.find(p => p.mine)?.id;

    // Build a fragment for performance
    const frag = document.createDocumentFragment();

    // Collect stranger npubs: selected npubs that are NOT in arrProfiles
    const knownIds = new Set(arrProfiles.map(p => p.id));
    const strangerNpubs = arrSelectedGroupMembers.filter(id => !knownIds.has(id));

    // Check if the filter text itself is a valid stranger npub
    const filterNpub = extractNpub(filterText);
    if (filterNpub && filterNpub !== mine && !knownIds.has(filterNpub) && !strangerNpubs.includes(filterNpub)) {
        strangerNpubs.push(filterNpub);
    }

    // Sort profiles: selected members first (by selection order), then unselected by last message time
    const sortedProfiles = [...arrProfiles].sort((a, b) => {
        const aSelectedIndex = arrSelectedGroupMembers.indexOf(a?.id);
        const bSelectedIndex = arrSelectedGroupMembers.indexOf(b?.id);
        const aSelected = aSelectedIndex !== -1;
        const bSelected = bSelectedIndex !== -1;

        // Selected members come first
        if (aSelected && !bSelected) return -1;
        if (!aSelected && bSelected) return 1;

        // For selected members: sort by selection order (first selected = first in list)
        if (aSelected && bSelected) {
            return aSelectedIndex - bSelectedIndex;
        }

        // For unselected members: sort by last message time (most recent first)
        const aChatTimestamp = getChatSortTimestamp(arrChats.find(c => c.id === a?.id) || {});
        const bChatTimestamp = getChatSortTimestamp(arrChats.find(c => c.id === b?.id) || {});

        // If both have timestamps, sort by most recent
        if (aChatTimestamp && bChatTimestamp) {
            return bChatTimestamp - aChatTimestamp;
        }
        // Contacts with messages come before those without
        if (aChatTimestamp && !bChatTimestamp) return -1;
        if (!aChatTimestamp && bChatTimestamp) return 1;

        // Fallback: sort alphabetically
        const aName = (a?.nickname || a?.name || '').toLowerCase();
        const bName = (b?.nickname || b?.name || '').toLowerCase();
        return aName.localeCompare(bName);
    });

    // Helper to build a member-pick row
    const buildRow = (npub, profile) => {
        const name = profile ? (profile.nickname || profile.name || '') : '';
        const isSelected = arrSelectedGroupMembers.includes(npub);

        const row = document.createElement('div');
        row.id = `cg-${npub}`;
        row.className = 'member-pick-row';

        const bgDiv = document.createElement('div');
        bgDiv.className = 'member-pick-hover';
        row.appendChild(bgDiv);

        row.addEventListener('mouseenter', () => {
            const primaryColor = getComputedStyle(document.documentElement).getPropertyValue('--icon-color-primary').trim();
            bgDiv.style.background = `linear-gradient(to right, ${primaryColor}40, transparent)`;
        });

        const avatarSrc = profile ? getProfileAvatarSrc(profile) : null;
        const avatar = createAvatarImg(avatarSrc, 25, false);
        avatar.className = 'member-pick-avatar';
        row.appendChild(avatar);

        const nameSpan = document.createElement('div');
        nameSpan.className = 'compact-member-name';
        nameSpan.textContent = name || (npub.substring(0, 10) + '...' + npub.substring(npub.length - 6));
        if (name) twemojify(nameSpan);
        row.appendChild(nameSpan);

        const isAdmin = arrSelectedGroupAdmins.includes(npub);
        const adminToggle = document.createElement('div');
        adminToggle.className = 'member-pick-admin' + (isAdmin ? ' active' : '');
        adminToggle.style.display = isSelected ? '' : 'none';
        adminToggle.innerHTML = '<span class="icon icon-crown"></span>';
        adminToggle.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (arrSelectedGroupAdmins.includes(npub)) {
                arrSelectedGroupAdmins = arrSelectedGroupAdmins.filter(n => n !== npub);
            } else {
                arrSelectedGroupAdmins.push(npub);
            }
            renderCreateGroupList(domCreateGroupFilter?.value || '');
        });
        row.appendChild(adminToggle);

        const indicator = document.createElement('div');
        indicator.className = 'member-pick-indicator' + (isSelected ? ' selected' : '');
        row.appendChild(indicator);

        row.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (arrSelectedGroupMembers.includes(npub)) {
                arrSelectedGroupMembers = arrSelectedGroupMembers.filter(n => n !== npub);
                arrSelectedGroupAdmins = arrSelectedGroupAdmins.filter(n => n !== npub);
            } else {
                arrSelectedGroupMembers.push(npub);
            }
            updateCreateGroupValidation(true);
            const currentFilter = domCreateGroupFilter?.value || '';
            renderCreateGroupList(currentFilter);
        });

        return row;
    };

    // Render stranger npubs (selected ones first, then filter-matched)
    for (const npub of strangerNpubs) {
        const isSelected = arrSelectedGroupMembers.includes(npub);
        // Show if selected (always) or if it matches the current filter npub
        if (!isSelected && filterNpub !== npub) continue;
        frag.appendChild(buildRow(npub, null));
        // Fire-and-forget relay lookup
        if (!strangerProfileRequested.has(npub)) {
            strangerProfileRequested.add(npub);
            invoke('load_profile', { npub }).catch(() => {});
        }
    }

    for (const p of sortedProfiles) {
        if (!p || !p.id) continue;
        if (p.id === mine) continue;
        if (p.is_blocked) continue;

        // Filter by nickname/name/npub (use extracted npub if input is a profile URL)
        const name = p.nickname || p.name || '';
        const hay = (name + ' ' + p.id).toLowerCase();
        if (f && !hay.includes(filterNpub || f)) continue;

        frag.appendChild(buildRow(p.id, p));
    }

    // If no matches
    if (!frag.childElementCount) {
        const empty = document.createElement('p');
        empty.style.textAlign = 'center';
        empty.style.opacity = '0.7';
        empty.textContent = f ? 'No matches' : 'No contacts found';
        domCreateGroupList.appendChild(empty);
    } else {
        domCreateGroupList.appendChild(frag);
    }
}

/**
 * Enable/disable Create button and show inline hint
 */
function updateCreateGroupValidation(showInline = false) {
    if (!domCreateGroupCreateBtn) return;
    // A Community needs only a name — members aren't added at creation (you invite via
    // link/npub afterward), so there's no member-selection requirement.
    const nameOk = !!domCreateGroupName?.value.trim();

    domCreateGroupCreateBtn.disabled = !nameOk;
    if (nameOk) {
        domCreateGroupCreateBtn.removeAttribute('disabled');
    } else {
        domCreateGroupCreateBtn.setAttribute('disabled', '');
    }

    // Only show status after an explicit attempt, or when forced via parameter
    const shouldShow = showInline || fCreateGroupAttempt;

    if (domCreateGroupStatus) {
        if (shouldShow && !nameOk) {
            domCreateGroupStatus.style.display = '';
            domCreateGroupStatus.textContent = 'A name is required';
        } else {
            domCreateGroupStatus.style.display = 'none';
            domCreateGroupStatus.textContent = '';
        }
    }
}

/**
 * Open Create Group tab
 */
function openCreateGroup() {
    pushBack('create-group', closeCreateGroup);
    // Show panel
    domCreateGroup.style.display = '';
    // Hide others
    domChats.style.display = 'none';
    domChat.style.display = 'none';
    domNavbar.style.display = 'none';

    // Reset state
    arrSelectedGroupMembers = [];
    arrSelectedGroupAdmins = [];
    strCreateGroupAvatarPath = null;
    fCreateGroupAttempt = false;
    if (domCreateGroupName) domCreateGroupName.value = '';
    if (domCreateGroupDescription) domCreateGroupDescription.value = '';
    if (domCreateGroupFilter) domCreateGroupFilter.value = '';
    if (domCreateGroupStatus) {
        domCreateGroupStatus.style.display = 'none';
        domCreateGroupStatus.textContent = '';
    }
    // Reset avatar picker
    if (domCreateGroupAvatarPreview) {
        domCreateGroupAvatarPreview.style.display = 'none';
        domCreateGroupAvatarPreview.src = '';
    }
    if (domCreateGroupAvatarPlaceholder) domCreateGroupAvatarPlaceholder.style.display = '';
    if (domCreateGroupAvatarPicker) domCreateGroupAvatarPicker.classList.remove('has-image');

    // Communities don't pick members at creation (invite via link/npub afterward) — hide
    // the member filter + list so the create panel is just name + description + avatar.
    if (domCreateGroupFilter) domCreateGroupFilter.style.display = 'none';
    if (domCreateGroupList) domCreateGroupList.style.display = 'none';
    updateCreateGroupValidation(false);

    // Focus name
    domCreateGroupName?.focus();
}

/**
 * Close Create Group tab and go back to Chat list
 */
async function closeCreateGroup() {
    popBack('create-group');
    domCreateGroup.style.display = 'none';
    fCreateGroupAttempt = false;

    // Restore navbar to follow the same flow as "Start New Chat" close (see closeChat())
    domNavbar.style.display = '';

    // Navigate back to chat list
    await openChatlist();

    // Adjust layout after UI visibility changes
    adjustSize();
}

/**
 * Wire up the Create Community UI (the panel is still id'd "CreateGroup" for now).
 * Creates a single-channel Community via `create_community`, then applies optional
 * description (`update_community_metadata`) and icon (`set_community_image`), and
 * navigates to the new channel. Any failure surfaces via popupConfirm + the inline status.
 */
(function wireCreateGroupUI() {
    if (!domCreateGroup) return;

    domCreateGroupBackBtn.onclick = closeCreateGroup;
    domCreateGroupCancelBtn.onclick = closeCreateGroup;

    domCreateGroupName.oninput = () => updateCreateGroupValidation(true);
    domCreateGroupFilter.oninput = (e) => renderCreateGroupList(e.target.value || '');

    // Avatar picker: open file dialog on click
    if (domCreateGroupAvatarPicker) {
        domCreateGroupAvatarPicker.onclick = async () => {
            const { open } = window.__TAURI__.dialog;
            const file = await open({
                title: 'Choose Group Avatar',
                multiple: false,
                directory: false,
                filters: [{
                    name: 'Image',
                    extensions: ['png', 'jpeg', 'jpg', 'gif', 'webp']
                }]
            });
            if (!file) return;
            strCreateGroupAvatarPath = file;
            // Show local preview, hide placeholder
            if (domCreateGroupAvatarPreview) {
                domCreateGroupAvatarPreview.src = convertFileSrc(file);
                domCreateGroupAvatarPreview.style.display = '';
            }
            if (domCreateGroupAvatarPlaceholder) domCreateGroupAvatarPlaceholder.style.display = 'none';
            domCreateGroupAvatarPicker.classList.add('has-image');
        };
    }

    domCreateGroupCreateBtn.onclick = async () => {
        const name = (domCreateGroupName?.value || '').trim();

        // Mark that the user attempted to create
        fCreateGroupAttempt = true;

        if (!name) {
            updateCreateGroupValidation(true);
            return;
        }

        // Loading state
        const prevTxt = domCreateGroupCreateBtn.textContent;
        domCreateGroupCreateBtn.textContent = 'Creating...';
        domCreateGroupCreateBtn.disabled = true;

        if (domCreateGroupStatus) {
            domCreateGroupStatus.style.display = '';
            domCreateGroupStatus.textContent = 'Preparing devices...';
        }

        try {
            const description = (domCreateGroupDescription?.value || '').trim() || null;

            if (domCreateGroupStatus) domCreateGroupStatus.textContent = 'Creating...';
            // Single-channel Community: defaults the channel to "general" + trusted relays.
            const created = await invoke('create_community', { name, channelName: null, relays: null });
            const communityId = created.community_id;
            const channelId = created.channel_id;

            // Description + avatar are set after creation (the Community must exist first).
            if (description) {
                try { await invoke('update_community_metadata', { communityId, name: null, description }); }
                catch (err) { console.error('Set community description failed:', err); showToast('Community created, but the description failed to save'); }
            }
            if (strCreateGroupAvatarPath) {
                if (domCreateGroupStatus) domCreateGroupStatus.textContent = 'Uploading avatar...';
                try { await invoke('set_community_image', { communityId, filepath: strCreateGroupAvatarPath, isBanner: false }); }
                catch (err) { console.error('Set community avatar failed:', err); showToast('Community created, but the avatar upload failed'); }
            }

            // Surface the new channel chat in the running session (mirrors what the
            // unified load would produce at next startup).
            const chat = getOrCreateChat(channelId, 'Community');
            chat.metadata = chat.metadata || {};
            chat.metadata.custom_fields = chat.metadata.custom_fields || {};
            chat.metadata.custom_fields.name = name;
            chat.metadata.custom_fields.description = description || '';
            chat.metadata.custom_fields.community_id = communityId;
            chat.metadata.custom_fields.is_owner = 'true';
            // Stamp creation time so the empty community sorts to the TOP right away (mirrors the join
            // path); reloads re-source this from the persisted DB created_at.
            chat.metadata.custom_fields.created_at = String(Date.now());
            if (strCreateGroupAvatarPath) {
                chat.metadata.custom_fields.icon = '1';
                invoke('cache_community_image', { communityId, isBanner: false })
                    .then(path => { if (path) { chat.metadata.avatar_cached = path; renderChatlist(); } })
                    .catch(() => {});
            }
            renderChatlist();

            // Navigate to the new channel + hide the panel.
            openChat(channelId);
            domCreateGroup.style.display = 'none';
        } catch (e) {
            const friendly = typeof e === 'string' ? e : (e?.message || e || '').toString();
            popupConfirm('Community creation failed', friendly, true, '', 'vector_warning.svg');
            if (domCreateGroupStatus) {
                domCreateGroupStatus.style.display = '';
                domCreateGroupStatus.textContent = friendly;
            }
        } finally {
            domCreateGroupCreateBtn.textContent = prevTxt || 'Create';
            updateCreateGroupValidation();
        }
    };
})();
