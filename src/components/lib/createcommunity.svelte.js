// The New Message / Create Community panel: which of the two it is, the community's name and
// icon, the chat's pick, and the action's progress. `seq` bumps per open so the panel
// remounts its picker fresh. Create Community runs in steps: 1 name, 2 who can join, 3 the
// way in; `created` is set once the community exists, from step 3 on.
const s = $state({
    seq: 0, mode: 'chat', name: '', filter: '', avatarPath: null, avatarPreview: '', avatarCrop: null,
    selected: 0, busy: false, status: '', error: false, profilesSeq: 0,
    step: 1, access: 'private', created: null, link: '', linkError: '', copied: false,
    prog: null,   // the running action's { label, pct }: creating, minting the link, inviting
});

export function ccState() { return s; }
/** @param {'chat'|'community'} mode */
export function ccOpen(mode = 'chat') {
    s.seq++; s.mode = mode; s.name = ''; s.filter = ''; s.avatarPath = null; s.avatarPreview = ''; s.avatarCrop = null;
    s.selected = 0; s.busy = false; s.status = ''; s.error = false;
    s.step = 1; s.access = 'private'; s.created = null; s.link = ''; s.linkError = ''; s.copied = false;
    s.prog = null;
}
export function ccSetAvatar(path, preview, crop = null) { s.avatarPath = path; s.avatarPreview = preview || ''; s.avatarCrop = crop; }
export function ccSetSelected(n) { s.selected = n; }
export function ccSetBusy(busy, status) { s.busy = !!busy; s.status = status || ''; s.error = false; }
export function ccSetError(msg) { s.busy = false; s.status = msg || ''; s.error = !!msg; }
/** Create Community's progress past step 2: { step, created, link, linkError, copied, prog }. */
export function ccPatch(fields) { Object.assign(s, fields); }
/** A profile load landed while the panel is open: the picker swaps its snapshot. */
export function ccProfilesChanged() { s.profilesSeq++; }
