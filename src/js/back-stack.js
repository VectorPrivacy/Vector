// Lightweight nav stack for the Android hardware back button. Every screen
// that wants to handle "back" registers a close fn via pushBack(id, close).
// Android's onBackPressed forwards into runBack(); when the stack is empty
// the activity backgrounds the task instead of closing the app.

const _backStack = [];

/**
 * Push (or refresh) an entry on the back stack. Re-using the same `id`
 * replaces the existing entry rather than stacking two.
 */
function pushBack(id, closeFn) {
    const i = _backStack.findIndex(e => e.id === id);
    if (i !== -1) _backStack.splice(i, 1);
    _backStack.push({ id, closeFn });
}

/**
 * Remove a specific id from the stack without running its close fn. Used by
 * screens that closed via their own path (e.g., the in-app back arrow) so
 * the stack stays in sync with what's actually visible.
 */
function popBack(id) {
    if (id === undefined) {
        _backStack.pop();
        return;
    }
    const i = _backStack.findIndex(e => e.id === id);
    if (i !== -1) _backStack.splice(i, 1);
}

/**
 * Pop the top entry and invoke its close fn. Returns true when something
 * was handled, false when the stack was empty.
 */
function runBack() {
    if (!_backStack.length) return false;
    const top = _backStack.pop();
    try { top.closeFn(); } catch (e) { console.warn('back-stack close fn threw', e); }
    return true;
}

/**
 * Wipe the entire stack. Called when navigating to the root surface
 * (chatlist), so the next back press exits to the home screen.
 */
function clearBack() {
    _backStack.length = 0;
}

// Hook the Android back press. The Kotlin side calls this synchronously
// via WebView.evaluateJavascript and reads the returned boolean: `true`
// means JS handled it, `false` means the activity should background.
window.__vectorOnAndroidBack = function () {
    return runBack();
};
