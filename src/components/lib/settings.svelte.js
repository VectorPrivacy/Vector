// Settings-screen state (Phase 4). The Tor card derives from the last TorState the
// backend reported (or an optimistic one the toggle handler set); the blocked-users
// list re-fetches when its version moves.
const tor = $state({
    state: null,          // TorState from the backend, or the handler's optimistic one
    statusOverride: '',   // handler-supplied status text ("Bootstrapping…", "Failed: …")
    locked: false,        // an operation is in flight: the toggle stays disabled
    advancedOpen: false,  // the Advanced disclosure is expanded
    circuits: { phase: 'idle', hops: [], error: '' },   // idle | loading | ok | error
});

export function torState() { return tor; }
export function setTorState(state, statusOverride = '') {
    tor.state = state || null;
    tor.statusOverride = statusOverride || '';
    // Pre-connect there is nothing to inspect; a disconnect collapses the disclosure.
    if (!state || !state.running) tor.advancedOpen = false;
}
export function setTorLocked(locked) { tor.locked = !!locked; }
export function setTorAdvancedOpen(open) { tor.advancedOpen = !!open; }
export function setTorCircuits(circuits) { tor.circuits = circuits; }

const blocked = $state({ v: 0 });
export function blockedVersion() { return blocked.v; }
export function reloadBlockedUsers() { blocked.v++; }
