// The Community overview: the open community's identity, what the viewer may do to
// it, and the async facts (raid alarm, migration status, an image upload in flight).
const ov = $state({
    chatId: '', communityId: '',
    name: '', description: '', avatarSrc: null,
    muted: false, isOwner: false, isV2: false,
    caps: {},                 // role-engine capabilities; empty = nothing manageable
    raid: null,               // { suspects } while a raid is flagged
    migration: null,          // migration_status payload when a row is warranted
    upload: null,             // { progress } while the icon uploads
});
export function overviewState() { return ov; }
export function setOverview(values) { Object.assign(ov, values); }
