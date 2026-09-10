// The fullscreen QR overlay (profile and bunker links) and the camera scanner.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const qrOverlay = popOverlay({ text: '' });


export const qrScanner = $state({ active: false, live: false });
export function setQrScanner(view) { Object.assign(qrScanner, view); }

