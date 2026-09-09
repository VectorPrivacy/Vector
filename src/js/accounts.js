// Accounts: the device's account list, the My Profile switcher, the pre-login
// picker, and the Add Profile flow. One global scope: this loads before main.js
// and shares its globals; nothing here runs at load.

// Free accounts cap, raised by effective tier (3/6/9/unlimited). SOFT gate on the
// Add Account button only — already-added accounts are never hidden or restricted,
// even when the user holds more than the current cap.
const ACCOUNTS_BY_TIER = [3, 6, 9, Infinity];
// Device-wide: the highest tier across ALL accounts (adding a profile spans accounts), from
// get_max_account_tier. Unlike per-account perks, this must NOT drop on an un-badged account.
let _maxAccountTier = 0;
function maxAccountsForTier() {
    return ACCOUNTS_BY_TIER[Math.min(Math.max(_maxAccountTier | 0, 0), 3)];
}

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

/** Account row helpers shared by the My Profile switcher and the pre-login picker. */
const accountRowHelpers = {
    fileSrc: (path) => convertFileSrc(path),
    placeholder: () => { const el = createPlaceholderAvatar(false, 28); el.classList.add('profile-switcher-avatar'); return el; },
};
let profileSwitcherRows = null;
let loginPickerRows = null;

/**
 * In-app My Profile dropdown — full-feature: switch / add / delete.
 * Opened from the Profile screen's My Profile header. Renders accounts via multiAccount.list().
 */
const profileSwitcher = {
    isOpen: false,
    isEditing: false,
    isOpening: false,

    init() {
        const backdrop = document.getElementById('profile-switcher-backdrop');
        const panel = document.getElementById('profile-switcher-panel');
        const addBtn = document.getElementById('profile-switcher-add');
        if (!panel) return;

        // The trigger and the trash toggle are the Profile screen's; it calls toggle() and toggleEditMode().
        backdrop.addEventListener('click', () => this.close());
        addBtn.addEventListener('click', () => this.onAddProfile());

        // Close on Escape
        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape' && this.isOpen) this.close();
        });
    },

    async toggle(anchor) {
        if (this.isOpen) {
            this.close();
        } else {
            await this.open(anchor);
        }
    },

    /** `anchor` is where the panel hangs from: the Profile tab header ('profile',
     *  the default) or the widescreen rail's account chip ('rail'), which drops
     *  UP and trades the blur for a plain click-catcher — it's a menu, not a
     *  takeover of the pane you're looking at. */
    async open(anchor = 'profile') {
        if (this.isOpening) return;
        this.isOpening = true;
        try {
            const accounts = await multiAccount.list();
            this.render(accounts);
            const dropup = anchor === 'rail';
            document.getElementById('profile-switcher-panel').classList.toggle('ws-dropup', dropup);
            document.getElementById('profile-switcher-backdrop').classList.toggle('ws-dropup', dropup);
            document.getElementById('profile-switcher-backdrop').classList.add('visible');
            document.getElementById('profile-switcher-panel').classList.add('open');
            VectorSvelte.setProfileSwitcherOpen(true);
            this.isOpen = true;
            // Android back closes the account list instead of navigating the profile screen away.
            pushBack('profile-switcher', () => profileSwitcher.close());
        } catch (e) {
            console.error('[profile-switcher] open failed:', e);
        } finally {
            this.isOpening = false;
        }
    },

    close() {
        popBack('profile-switcher');
        const panel = document.getElementById('profile-switcher-panel');
        document.getElementById('profile-switcher-backdrop').classList.remove('visible');
        document.getElementById('profile-switcher-backdrop').classList.remove('ws-dropup');
        panel.classList.remove('open');
        // The rail anchors the drop-up to its chip inline; leaving it set would
        // strand the next profile-tab open at the wrong height.
        panel.classList.remove('ws-dropup');
        panel.style.bottom = '';
        VectorSvelte.setProfileSwitcherOpen(false);
        this.isOpen = false;
        // Always reset edit mode on close so the next open starts neutral.
        this.exitEditMode();
    },

    render(accounts) {
        // The account cap is device-wide (highest tier across all accounts); refresh it and
        // re-render if it changed, so the gate is right even on an un-badged active account.
        invoke('get_max_account_tier').then(t => {
            t = t | 0;
            if (t !== _maxAccountTier && this.isOpen) { _maxAccountTier = t; this.render(accounts); }
            else _maxAccountTier = t;
        }).catch(() => {});
        const list = document.getElementById('profile-switcher-list');
        const myProfile = arrProfiles.find(p => p.mine);
        const activeNpub = myProfile?.id || '';
        if (profileSwitcherRows) VectorSvelte.unmountComponent(profileSwitcherRows);
        profileSwitcherRows = VectorSvelte.mountAccountRows(list, {
            accounts, activeNpub, h: accountRowHelpers,
            onPick: (m) => this.onSwitchTo(m),
            onDelete: (m) => this.onDeleteRow(m),
        });
        // Soft cap: disable the Add button at the tier's account ceiling. Existing
        // accounts (even above the cap) stay listed + usable; only adding more is gated.
        const addBtn = document.getElementById('profile-switcher-add');
        if (addBtn) {
            const atCap = accounts.length >= maxAccountsForTier();
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
        // Active account stays anchored in the pill at the top — only
        // alternates appear as switchable rows below it. No delete here (per design).
        if (loginPickerRows) VectorSvelte.unmountComponent(loginPickerRows);
        loginPickerRows = VectorSvelte.mountAccountRows(list, {
            accounts: this.accounts.filter(m => m.npub !== this.activeNpub),
            h: accountRowHelpers,
            onPick: (m) => this.onPick(m),
        });
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

        VectorSvelte.loginScreen('start', true);
        VectorSvelte.loginShowForm(true);

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
        VectorSvelte.loginScreen('start', false);
        VectorSvelte.loginShowForm(false);
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
