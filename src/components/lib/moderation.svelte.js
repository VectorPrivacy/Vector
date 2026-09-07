// The moderation console's state: the backend's intel for one community, the keep set
// (ticked = kept; the unticked set is what a rotation cuts), the list filter and query,
// and the busy lock during a publish.
const m = $state({ communityId: null, loading: false, error: '', filter: 'all', query: '', busy: false, busyTitle: '', busyBody: '' });
let intel = $state.raw(null);
let keep = $state.raw(new Set());

export function modState() { return m; }
export function modIntel() { return intel; }
export function modKeep() { return keep; }
export function modOpen(communityId) {
    m.communityId = communityId; m.loading = true; m.error = ''; m.filter = 'all'; m.query = ''; m.busy = false;
    intel = null; keep = new Set();
}
export function modSetIntel(next, keepSet) { intel = next; keep = keepSet; m.loading = false; m.error = ''; }
export function modSetError(err) { intel = null; m.loading = false; m.error = String(err); }
export function modSetFilter(f) { m.filter = f; }
export function modSetQuery(q) { m.query = q; }
/** Flip one member; a new Set so every derived reader moves. */
export function modToggleKeep(npub) {
    const next = new Set(keep);
    if (next.has(npub)) next.delete(npub); else next.add(npub);
    keep = next;
}
export function modSetBusy(busy, title, body) { m.busy = !!busy; m.busyTitle = title || ''; m.busyBody = body || ''; }
export function modSetProgress(title, body) { if (m.busy) { m.busyTitle = title; m.busyBody = body; } }
