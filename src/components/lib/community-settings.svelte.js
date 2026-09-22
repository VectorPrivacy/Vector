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
    saving: false,
    progress: 0,
    // Bumped when a close is refused over unsaved changes, so the bar can say so.
    nudge: 0,
    section: 'overview',
    query: '',
});

export function csState() { return s; }

/** Whether the draft differs from what the community holds. */
export function csDirty() {
    return s.draft.name !== s.saved.name
        || s.draft.description !== s.saved.description
        || !!s.draft.iconPath;
}

export function csOpen(communityId) {
    s.communityId = communityId;
    s.loading = !!communityId;
    s.saved = blank();
    s.draft = { name: '', description: '', iconPath: null, iconPreview: null };
    s.relays = [];
    s.canEdit = false;
    s.saving = false;
    s.progress = 0;
    s.section = 'overview';
    s.query = '';
}

export function csLoaded({ name, description, iconSrc, relays, canEdit }) {
    s.saved = { name, description, iconSrc };
    s.draft = { name, description, iconPath: null, iconPreview: null };
    s.relays = relays || [];
    s.canEdit = !!canEdit;
    s.loading = false;
}

export function csSetDraft(patch) { Object.assign(s.draft, patch); }

export function csReset() {
    s.draft = { name: s.saved.name, description: s.saved.description, iconPath: null, iconPreview: null };
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
