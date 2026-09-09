// Fade-in / fade-out dialogs whose handlers are fixed at mount: the Add Relay form, the
// relay and media server info dialogs, and the mini app launch dialog. `open` mounts the
// element, `active` drives the fade; `close` drops `active` and unmounts after the fade.
export function fadeDialog(fields) {
    const s = $state({ open: false, active: false, ...fields });
    return {
        state: () => s,
        /** Show with these fields; re-opening while open just repaints. */
        open(view) {
            Object.assign(s, view);
            if (s.open) { s.active = true; return; }
            s.open = true;
            s.active = false;
            setTimeout(() => { if (s.open) s.active = true; }, 10);
        },
        patch(view) { Object.assign(s, view); },
        close() {
            s.active = false;
            setTimeout(() => { if (!s.active) s.open = false; }, 300);
        },
    };
}

export const addRelayDialog = fadeDialog({ url: '', mode: 'both' });

export const relayInfoDialog = fadeDialog({
    url: '', status: '', isDefault: false, enabled: true, mode: 'both',
    ping: '--', pingColor: '', lastCheck: '--', copied: false,
});

export const blossomInfoDialog = fadeDialog({ url: '', enabled: true, isCustom: false });

export const launchDialog = fadeDialog({ name: '', actionText: 'Play', updateMode: false, icon: null });

// Pop-in overlays: display flips with `active`, the card replays its pop animation when
// `pop` moves (the component adds the class after the overlay has rendered, because
// WebKit never starts an animation declared on a subtree emerging from display:none),
// and `closing` runs the mirrored pop-out before the overlay actually hides.
function popOverlay(fields) {
    const s = $state({ active: false, closing: false, pop: 0, ...fields });
    let timer = null;
    return {
        state: () => s,
        open(view) {
            clearTimeout(timer);
            Object.assign(s, view);
            s.closing = false;
            s.active = true;
            s.pop++;
        },
        patch(view) { Object.assign(s, view); },
        /** True when a close is already running; the caller then does nothing. */
        closing: () => s.closing,
        close() {
            if (s.closing || !s.active) return;
            s.closing = true;
            timer = setTimeout(() => { s.active = false; s.closing = false; }, 160);
        },
    };
}

export const qrOverlay = popOverlay({ text: '' });

export const statusDialog = popOverlay({
    panelOpen: false, avatarSrc: null, text: '', empty: true, count: '', low: false, clearHidden: true,
});

export const modOverlay = popOverlay({});

export const qrScanner = $state({ active: false, live: false });
export function setQrScanner(view) { Object.assign(qrScanner, view); }

// The downgrade block has no dismiss path: shown once, never hidden.
export const downgradeBlock = $state({ open: false, current: '', required: '' });
export function showDowngradeBlock(current, required) {
    downgradeBlock.current = current;
    downgradeBlock.required = required;
    downgradeBlock.open = true;
}

// The Invites screen: the account's code, or the state of fetching it.
export const invites = $state({ phase: 'loading', code: '', xUrl: '' });   // loading | ok | error
export function setInvites(view) { Object.assign(invites, view); }
