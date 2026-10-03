// Device transfer card. `role` is this device's side: the signed-in `sender` or the `receiver`
// being set up. `stage` walks:
//   pick (a phone being set up: scan first) | show | enter → connecting → match (receiver) | approve (sender) → sending → sent
//                                finishing (receiver, once the account arrives)
// with `error` reachable from anywhere.
import { popOverlay } from './dialog-lifecycle.svelte.js';

export const transferModal = popOverlay({
    role: 'receiver',
    stage: 'connecting',
    code: '',
    qr: '',
    expiresAt: 0,
    entry: '',
    number: '',
    sas: '',
    sender: '',
    name: '',
    avatar: '',
    canScan: false,
    busy: false,
    unconfirmed: false,
    /** The account arrived but signing in with it failed; Try Again signs in again. */
    finishFailed: false,
    error: '',
});
