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
