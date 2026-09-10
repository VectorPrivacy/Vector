// Two body-level overlays: the processing card (an image being prepared in the backend)
// and the mini app permission prompt. Each is opened by its module and answers through here.
const processing = $state({ visible: false, message: '' });
export function processingState() { return processing; }
export function showProcessing(message) { processing.message = message; processing.visible = true; }
export function hideProcessing() { processing.visible = false; }

const perm = $state({ open: false, active: false, appName: '', items: [] });
let permAnswer = null;   // { deny(), allow(granted: [[id, bool]]) }
export function permissionState() { return perm; }
export function permissionAnswer() { return permAnswer; }
export function openPermissionPrompt(appName, items, handlers) {
    perm.appName = appName; perm.items = items.map(i => ({ ...i, granted: false })); perm.open = true; perm.active = false;
    permAnswer = handlers;
}
export function activatePermissionPrompt() { perm.active = true; }
export function closePermissionPrompt() { perm.active = false; permAnswer = null; }
export function unmountPermissionPrompt() { perm.open = false; }

// The downgrade block has no dismiss path: shown once, never hidden.
export const downgradeBlock = $state({ open: false, current: '', required: '' });
export function showDowngradeBlock(current, required) {
    downgradeBlock.current = current;
    downgradeBlock.required = required;
    downgradeBlock.open = true;
}

