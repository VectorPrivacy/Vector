// The Your Keys card: the seed phrase and nsec, each hidden until tapped. `saving` is the
// new-account variant for a browser that keeps nothing, which closes only on its button.
import { popOverlay } from './dialog-lifecycle.svelte.js';

// `stage` is 'warn' (Settings first points at Sign in on Another Device) or 'keys'.
export const keysModal = popOverlay({ stage: 'keys', saving: false, nsec: '', seed: '' });
