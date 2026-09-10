// The community invite modal: the panel's own state and the bags its two islands need.
// community.js fills the bags and the local reads first, then flips `open`, so the modal
// mounts in one paint.
const st = $state({ open: false, busy: false, ctaBusy: false, name: '', status: { text: '', error: false }, selected: 0, search: '' });
let handlers = $state.raw({});      // close(), cta(), searchInput(value), links (InviteLinks h), contactProps (ContactPicker props)
let picker = null;      // the mounted ContactPicker's exports
export function inviteModalState() { return st; }
export function inviteModalHandlers() { return handlers; }
export function setInviteModalHandlers(h) { handlers = h || {}; }
export function setInviteModal(patch) { Object.assign(st, patch); }
export function setInviteModalStatus(text, error = false) { st.status = { text: text || '', error: !!error }; }
export function inviteModalPicker() { return picker; }
export function setInviteModalPicker(api) { picker = api; }
