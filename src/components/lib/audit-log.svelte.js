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

// Advanced Mode's sync report: this device's view of the community's control state,
// structure only, for comparing two devices that disagree. `null` while closed.
const r = $state({ report: null });

export function auditReportState() { return r; }
export function auditReportOpen() { r.report = { loading: true, data: null, error: '' }; }
export function auditReportLoaded(data) { if (r.report) r.report = { loading: false, data, error: '' }; }
export function auditReportFailed(error) { if (r.report) r.report = { loading: false, data: null, error: error || 'The report could not be made.' }; }
export function auditReportClose() { r.report = null; }
