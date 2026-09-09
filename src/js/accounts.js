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
};

/**
 * In-app My Profile dropdown — full-feature: switch / add / delete. The panel is the
 * shell's ProfileSwitcher component; this drives its store and answers its clicks.
 */
const profileSwitcher = {
    isOpen: false,
    isEditing: false,
    isOpening: false,

    init() {
        VectorSvelte.setSwitcherHandlers({
            close: () => this.close(),
            onPick: (m) => this.onSwitchTo(m),
            onDelete: (m) => this.onDeleteRow(m),
            onAdd: () => this.onAddProfile(),
            rowHelpers: accountRowHelpers,
        });
        // The trigger and the trash toggle are the Profile screen's; it calls toggle() and toggleEditMode().
        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape' && this.isOpen) this.close();
        });
    },

    async toggle(anchor, anchorEl) {
        if (this.isOpen) {
            this.close();
        } else {
            await this.open(anchor, anchorEl);
        }
    },

    /** `anchor` is where the panel hangs from: the Profile tab header ('profile',
     *  the default) or the widescreen rail's account chip ('rail'), which drops
     *  UP and trades the blur for a plain click-catcher — it's a menu, not a
     *  takeover of the pane you're looking at. `anchorEl` pins the drop-up to the
     *  chip itself, whatever the chip's height or the rail's footer padding become. */
    async open(anchor = 'profile', anchorEl = null) {
        if (this.isOpening) return;
        this.isOpening = true;
        try {
            const accounts = await multiAccount.list();
            this.render(accounts);
            const dropup = anchor === 'rail';
            const bottomPx = dropup && anchorEl ? window.innerHeight - anchorEl.getBoundingClientRect().top + 6 : null;
            VectorSvelte.openSwitcher(dropup, bottomPx);
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
        VectorSvelte.closeSwitcher();
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
        const myProfile = arrProfiles.find(p => p.mine);
        VectorSvelte.setSwitcherRows(accounts, myProfile?.id || '');
        // Soft cap: disable the Add button at the tier's account ceiling. Existing
        // accounts (even above the cap) stay listed + usable; only adding more is gated.
        const atCap = accounts.length >= maxAccountsForTier();
        VectorSvelte.setSwitcherAdd(atCap, atCap ? 'Maximum Accounts' : 'Add Profile');
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
        if (VectorSvelte.switcherState().addDisabled) return;
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
    accounts: [],
    activeNpub: null,
    get isOpen() { return VectorSvelte.loginPickerState().open; },

    init() {
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
        try {
            this.accounts = await multiAccount.list();
        } catch (e) {
            console.error('[login-picker] list failed:', e);
            VectorSvelte.patchPicker({ shown: false });
            return;
        }
        this.activeNpub = activeNpub;
        if (this.accounts.length < 2) {
            VectorSvelte.patchPicker({ shown: false });
            return;
        }
        // With no active identity (marker-missing recovery) the pill is a neutral
        // "Select profile" affordance rather than accounts[0] posing as signed in.
        const hasActive = !!activeNpub && this.accounts.some(a => a.npub === activeNpub);
        const meta = hasActive ? this.accounts.find(a => a.npub === activeNpub) : null;
        const avatarSrc = meta
            ? (meta.avatar_cached ? convertFileSrc(meta.avatar_cached) : (meta.avatar_url || null))
            : null;
        VectorSvelte.patchPicker({
            shown: true, avatar: avatarSrc, label: meta ? (meta.display_name || meta.npub) : 'Select Profile',
            accounts: this.accounts, activeNpub: this.activeNpub,
        });
    },

    hide() {
        VectorSvelte.patchPicker({ shown: false });
        this.close();
    },

    toggle() {
        if (this.isOpen) this.close(); else this.open();
    },

    open() {
        // The active account stays anchored in the pill; the component lists the
        // alternates as switchable rows. No delete here (per design).
        VectorSvelte.patchPicker({ accounts: this.accounts, activeNpub: this.activeNpub, open: true });
    },

    close() {
        VectorSvelte.patchPicker({ open: false });
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
        VectorSvelte.showPane('navbar', false);
        VectorSvelte.showPane('chats', false);
        VectorSvelte.showPane('chat', false);
        VectorSvelte.showPane('profile', false);
        VectorSvelte.showPane('settings', false);
        VectorSvelte.showPane('invites', false);
        VectorSvelte.showPane('groupOverview', false);

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
    const panes = VectorSvelte.panesSnapshot();
    return () => VectorSvelte.restorePanes(panes);
}
