// The Create Channel / Create Category / Duplicate Channel modal: what it makes, where, and the draft.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const createChannelModal = popOverlay({
    category: false, communityId: '', categoryId: null, categoryName: '',
    kind: 'text', isPrivate: false, name: '', busy: false, error: '',
    // The channel being duplicated ({ id, name, view }), which fixes the type and privacy.
    source: null,
});

export function patchCreateChannel(view) { createChannelModal.patch(view); }
