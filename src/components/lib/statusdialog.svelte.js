// The Set Status dialog: the composer host, the live preview row and the emoji panel state.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const statusDialog = popOverlay({
    panelOpen: false, avatarSrc: null, text: '', empty: true, count: '', low: false, clearHidden: true,
});

