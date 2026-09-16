// The one call: a mirror of the backend's `call_state` events, with the stats line
// that rides beside an active one. `receivedAt` lets the timer run between events.
const c = $state({ id: null, peer: null, outgoing: false, phase: null, reason: null,
    muted: false, peerMuted: false, activeMs: 0, receivedAt: 0, stats: null, tick: 0 });
let endedTimer = null;

export function callState() { return c; }

/** `s` is the backend CallState, or null when there is no call. */
export function setCallState(s) {
    clearTimeout(endedTimer);
    endedTimer = null;
    if (!s) {
        c.id = null; c.phase = null; c.stats = null; c.tick++;
        return;
    }
    c.id = s.id; c.peer = s.peer; c.outgoing = s.outgoing; c.phase = s.phase;
    c.reason = s.reason || null; c.muted = s.muted; c.peerMuted = s.peer_muted;
    c.activeMs = s.active_ms; c.receivedAt = Date.now();
    if (s.phase !== 'active') c.stats = null;
    // An ended call lingers long enough to read why.
    if (s.phase === 'ended') {
        endedTimer = setTimeout(() => {
            if (c.id === s.id && c.phase === 'ended') { c.id = null; c.phase = null; c.tick++; }
        }, 2500);
    }
    c.tick++;
}

export function setCallStats(st) {
    if (st && st.id === c.id) c.stats = st;
}
