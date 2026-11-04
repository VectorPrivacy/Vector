// Vector's clock: this device's, moved by what the backend's relays agree on. Every read of
// "now" in the app goes through `now()`, so a message's time and the time it's judged against
// come from the same clock.

export const clock = $state({
    decentralised: true,
    measuredOffsetMs: 0,
    offsetMs: 0,
    answered: 0,
    agreeing: 0,
    checkedAt: 0,
});

/** Take the backend's clock status (`get_clock_status`, `clock_changed`). */
export function setClock(s) {
    if (!s) return;
    clock.decentralised = s.decentralised !== false;
    clock.measuredOffsetMs = s.measured_offset_ms || 0;
    clock.offsetMs = s.offset_ms || 0;
    clock.answered = s.answered || 0;
    clock.agreeing = s.agreeing || 0;
    clock.checkedAt = s.checked_at || 0;
}

/** Now, in unix ms, on Vector's clock. */
export function now() {
    return Date.now() + clock.offsetMs;
}

/** Now as a Date, on Vector's clock. */
export function nowDate() {
    return new Date(now());
}
