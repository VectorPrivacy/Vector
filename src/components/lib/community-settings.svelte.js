// Community Settings: what the community holds (`saved`), what the editor has made of it
// (`draft`), and the overlay's lifecycle. Edits stay in the draft until Save, so a rename
// and a new icon go out as one deliberate act rather than a publish per keystroke.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const csOverlay = popOverlay({});

const blank = () => ({ name: '', description: '', iconSrc: null });

const s = $state({
    communityId: null,
    loading: false,
    saved: blank(),
    // iconPath: a picked file awaiting upload; iconPreview: its displayable copy.
    draft: { name: '', description: '', iconPath: null, iconPreview: null },
    relays: [],
    canEdit: false,
    // Bans act at once, not through the save bar: the list IS the published state.
    canBan: false,
    canRoles: false,
    bans: [],               // npubs
    bansMax: 500,
    banSel: new Set(),      // npubs picked for the next unban
    unbanning: false,       // a batch unban is publishing
    // Roles: the published view, and the drafts standing against it. `order` is a
    // dragged arrangement (role ids, top first); `edit` is the role open in the
    // editor, `id` null for one not yet created, with `base` what it was opened at.
    roles: { view: null, order: null, edit: null, members: [], busy: false },
    saving: false,
    progress: 0,
    // Bumped when a close is refused over unsaved changes, so the bar can say so.
    nudge: 0,
    section: 'overview',
    query: '',
});

export function csState() { return s; }

/** Whether the Overview draft differs from what the community holds. */
export function csOverviewDirty() {
    return s.draft.name !== s.saved.name
        || s.draft.description !== s.saved.description
        || !!s.draft.iconPath;
}

/** Whether a dragged order differs from the published one. */
export function csRoleOrderDirty() {
    const order = s.roles.order;
    const view = s.roles.view;
    if (!order || !view) return false;
    return order.join() !== view.roles.map((r) => r.role_id).join();
}

/** Whether the role in the editor has unsaved changes (a new one always does). */
export function csRoleEditDirty() {
    const e = s.roles.edit;
    if (!e) return false;
    if (e.id === null) return true;
    return e.name !== e.base.name || e.color !== e.base.color || e.permissions !== e.base.permissions;
}

/** Anything unsaved, anywhere: what the save bar and the close refusal answer to. */
export function csDirty() {
    return csOverviewDirty() || csRoleOrderDirty() || csRoleEditDirty();
}

export function csSetRolesView(view) { s.roles.view = view; }
export function csSetRoleMembers(npubs) { s.roles.members = npubs || []; }
export function csSetRolesBusy(on) { s.roles.busy = !!on; }
export function csSetRoleOrder(ids) { s.roles.order = ids; }

/** Open a role in the editor: an existing one by its view entry, or a fresh draft. */
export function csEditRole(role) {
    if (!role) { s.roles.edit = null; return; }
    const base = { name: role.name || '', color: role.color || 0, permissions: role.permissions || '0' };
    s.roles.edit = { id: role.role_id ?? null, channel_id: role.channel_id ?? null, ...base, base };
}
export function csPatchRoleEdit(patch) { if (s.roles.edit) Object.assign(s.roles.edit, patch); }

export function csOpen(communityId) {
    s.communityId = communityId;
    s.loading = !!communityId;
    s.saved = blank();
    s.draft = { name: '', description: '', iconPath: null, iconPreview: null };
    s.relays = [];
    s.canEdit = false;
    s.canBan = false;
    s.canRoles = false;
    s.bans = [];
    s.banSel = new Set();
    s.unbanning = false;
    s.roles = { view: null, order: null, edit: null, members: [], busy: false };
    s.saving = false;
    s.progress = 0;
    s.section = 'overview';
    s.query = '';
}

export function csLoaded({ name, description, iconSrc, relays, canEdit, canBan, canRoles }) {
    s.saved = { name, description, iconSrc };
    s.draft = { name, description, iconPath: null, iconPreview: null };
    s.relays = relays || [];
    s.canEdit = !!canEdit;
    s.canBan = !!canBan;
    s.canRoles = !!canRoles;
    s.loading = false;
}

export function csSetBans(npubs, max) {
    s.bans = npubs || [];
    if (max) s.bansMax = max;
}
export function csSetUnbanning(on) { s.unbanning = !!on; }

/** Pick or drop `npubs` for the next unban; `on` null toggles each. */
export function csSelectBans(npubs, on = null) {
    const next = new Set(s.banSel);
    for (const n of npubs) {
        const want = on === null ? !next.has(n) : on;
        if (want) next.add(n); else next.delete(n);
    }
    s.banSel = next;
}
export function csClearBanSel() { s.banSel = new Set(); }

/** Unbanned: out of the list and out of the selection. */
export function csRemoveBans(npubs) {
    const gone = new Set(npubs);
    s.bans = s.bans.filter((n) => !gone.has(n));
    s.banSel = new Set([...s.banSel].filter((n) => !gone.has(n)));
}

export function csSetDraft(patch) { Object.assign(s.draft, patch); }

export function csReset() {
    s.draft = { name: s.saved.name, description: s.saved.description, iconPath: null, iconPreview: null };
    s.roles.order = null;
    const e = s.roles.edit;
    // A role never created has nothing to fall back to: resetting discards it.
    if (e && e.id === null) s.roles.edit = null;
    else if (e) Object.assign(e, { name: e.base.name, color: e.base.color, permissions: e.base.permissions });
}

/** What a save committed, folded into `saved` so the bar only lingers for what failed. */
export function csCommitted(patch) {
    if ('name' in patch) s.saved.name = patch.name;
    if ('description' in patch) s.saved.description = patch.description;
    if ('iconSrc' in patch) {
        s.saved.iconSrc = patch.iconSrc;
        s.draft.iconPath = null;
        s.draft.iconPreview = null;
    }
}

export function csSetSaving(on, progress = 0) { s.saving = on; s.progress = progress; }
export function csNudge() { s.nudge++; }
export function csSetSection(id) { s.section = id; }
export function csSetQuery(q) { s.query = q; }
