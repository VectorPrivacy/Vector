// Native calls: the overlay's helper bag, the backend's events, and the reload hydrate.
// The engine lives in Rust; this side only asks and renders.

function registerCallScreen() {
    VectorSvelte.setScreen('call', {
        // Lazy: the helpers live in scripts that load after this one evaluates.
        h: {
            accept: () => invoke('call_accept').catch((e) => VectorSvelte.showToast(String(e))),
            reject: () => invoke('call_reject').catch(() => {}),
            hangup: () => invoke('call_hangup').catch(() => {}),
            setMuted: (on) => invoke('call_set_muted', { muted: on }).catch(() => {}),
            getProfile: (npub) => getProfile(npub),
            getName: (x) => getName(x),
            getProfileAvatarSrc: (p) => getProfileAvatarSrc(p),
            openChat: (npub) => openChat(npub),
        },
    });
    // A reloaded webview finds the call the backend still holds.
    invoke('call_status').then((s) => { if (s) VectorSvelte.setCallState(s); }).catch(() => {});
}

/** Ring a DM contact. The Chat header's call button lands here. */
async function startCall(npub) {
    try {
        await invoke('call_start', { npub });
    } catch (e) {
        VectorSvelte.showToast(String(e));
    }
}

document.addEventListener('DOMContentLoaded', registerCallScreen, { once: true });
