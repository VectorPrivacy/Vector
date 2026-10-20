// The Audit Log Community Settings shows: one community's at a time, read when its section opens.

const PAGE = 50;

const s = $state({
    communityId: null,
    loading: false,
    error: '',
    entries: [],
    compacted: false,
    action: 'all',
    query: '',
    shown: PAGE,
    open: new Set(),
});

export function auditState() { return s; }

export function auditReset() {
    s.communityId = null;
    s.loading = false;
    s.error = '';
    s.entries = [];
    s.compacted = false;
    s.action = 'all';
    s.query = '';
    s.shown = PAGE;
    s.open = new Set();
}

export function auditLoading(communityId) {
    s.communityId = communityId;
    s.loading = true;
    s.error = '';
}

export function auditLoaded(communityId, log) {
    if (s.communityId !== communityId) return;
    s.entries = log?.entries || [];
    s.compacted = !!log?.compacted;
    s.loading = false;
}

export function auditFailed(communityId, error) {
    if (s.communityId !== communityId) return;
    s.error = error || 'The audit log could not be read.';
    s.loading = false;
}

export function auditSetAction(action) {
    s.action = action;
    s.shown = PAGE;
}

export function auditSetQuery(query) {
    s.query = query;
    s.shown = PAGE;
}

export function auditShowMore() { s.shown += PAGE; }

export function auditToggle(id) {
    const open = new Set(s.open);
    if (open.has(id)) open.delete(id); else open.add(id);
    s.open = open;
}
