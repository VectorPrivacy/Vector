// The Community overview: the open community's identity, what the viewer may do to
// it, and the async facts (raid alarm, migration status, an image upload in flight).
import { flushSync } from 'svelte';

const ov = $state({
    // The community the pane is tagged with; the realtime listeners and widescreen
    // read it to know whose roster is up. Set on open, cleared on every close path.
    groupId: '',
    chatId: '', communityId: '',
    name: '', description: '', avatarSrc: null,
    muted: false, isOwner: false, isV2: false,
    caps: {},                 // role-engine capabilities; empty = nothing manageable
    raid: null,               // { suspects } while a raid is flagged
    migration: null,          // migration_status payload when a row is warranted
    upload: null,             // { progress } while the icon uploads
    memberSearch: '',         // the roster filter's text; the input renders in the body or, widescreen, the header
    memberSearchShown: true,  // hidden over an empty roster
});
export function overviewState() { return ov; }
export function setOverview(values) { Object.assign(ov, values); }

/** Tag (or untag) the pane with a community. Flushed: callers read it back synchronously. */
export function setOverviewGroup(id) { ov.groupId = id || ''; flushSync(); }

let headHandlers = {};   // { back, memberSubtext }
export function overviewHeadHandlers() { return headHandlers; }
export function setOverviewHeadHandlers(h) { headHandlers = h || {}; }

// The mounted roster (community/MemberRoster) so community.js can feed it its lists.
let roster = $state.raw(null);
export function overviewRoster() { return roster; }
export function setOverviewRoster(inst) { roster = inst; }
