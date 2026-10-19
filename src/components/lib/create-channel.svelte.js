// The Create Channel / Create Category modal: what it makes, where, and the draft.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const createChannelModal = popOverlay({
    category: false, communityId: '', categoryId: null, categoryName: '',
    kind: 'text', isPrivate: false, name: '', busy: false, error: '',
});

export function patchCreateChannel(view) { createChannelModal.patch(view); }
