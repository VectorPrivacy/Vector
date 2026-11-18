/**
 * Attribute-driven message row states (highlight + status).
 *
 * All visual states on a `.dmsg` row are data-attributes — no class churn.
 *
 *   data-status:       pending | sent | failed
 *   data-pinged:       true (when message mentions current user)
 *   data-replying-to:  true (when this row is the active reply target)
 *   data-jumped:       true (ephemeral; flash animation, removed when it ends)
 */

// Ended by animationend, not a timer: a late first frame would cut a timed flash short. The
// timer only catches a row that never animates.
const _DMSG_JUMP_FALLBACK_MS = 4000;

// Track the active jump flash at module scope rather than as a property on the
// row element — that way if the row is removed while the timer is pending, we
// don't keep a strong reference to the detached node, and a fresh jump on a
// different row can correctly cancel the previous flash.
let _dmsgActiveJumpRow = null;
let _dmsgActiveJumpTimer = null;

function _dmsgEndJump() {
    const row = _dmsgActiveJumpRow;
    if (!row) return;
    row.removeEventListener('animationend', _dmsgOnJumpEnd);
    clearTimeout(_dmsgActiveJumpTimer);
    clearHighlight(row, 'jumped');
    _dmsgActiveJumpRow = null;
    _dmsgActiveJumpTimer = null;
}

function _dmsgOnJumpEnd(e) {
    if (e.target === e.currentTarget && e.animationName === 'highlightMessage') _dmsgEndJump();
}

function applyHighlight(rowEl, kind) {
    if (!rowEl || !kind) return;
    switch (kind) {
        case 'jumped': {
            // Spam-clicks on the same row are ignored — let the in-flight
            // animation complete to prevent CSS re-trigger stutter. Jumps to
            // a *different* row cancel the prior flash so two rows can't be
            // simultaneously animating.
            if (_dmsgActiveJumpRow === rowEl) return;
            _dmsgEndJump();
            _dmsgActiveJumpRow = rowEl;
            rowEl.addEventListener('animationend', _dmsgOnJumpEnd);
            // Started on the next frame: the jump's own scroll and layout would otherwise
            // spend the flash before it is first painted.
            requestAnimationFrame(() => { if (_dmsgActiveJumpRow === rowEl) rowEl.dataset.jumped = 'true'; });
            _dmsgActiveJumpTimer = setTimeout(_dmsgEndJump, _DMSG_JUMP_FALLBACK_MS);
            break;
        }
    }
}

function clearHighlight(rowEl, kind) {
    if (!rowEl || !kind) return;
    switch (kind) {
        case 'jumped':       delete rowEl.dataset.jumped; break;
    }
}

