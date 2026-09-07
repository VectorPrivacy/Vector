// The community invite panel's link section: my links, the folded registry summary (mode +
// per-creator counts), and what is in flight. One panel is open at a time.
const s = $state({ busy: false, creating: false, revoking: null, copied: null });
let links = $state.raw([]);
let summary = $state.raw({ is_public: false, creators: [] });

export function ilState() { return s; }
export function ilLinks() { return links; }
export function ilSummary() { return summary; }
export function ilSet(nextLinks, nextSummary) { links = nextLinks || []; summary = nextSummary || { is_public: false, creators: [] }; }
export function ilSetBusy(on) { s.busy = !!on; }
export function ilSetCreating(on) { s.creating = !!on; }
export function ilSetRevoking(token) { s.revoking = token || null; }
export function ilSetCopied(token) { s.copied = token || null; }
export function ilReset() { s.busy = false; s.creating = false; s.revoking = null; s.copied = null; links = []; summary = { is_public: false, creators: [] }; }
