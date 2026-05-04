/**
 * Attribute-driven message row states (highlight + status).
 *
 * All visual states on a `.dmsg` row are data-attributes — no class churn.
 *
 *   data-status:       pending | sent | failed
 *   data-pinged:       true (when message mentions current user)
 *   data-replying-to:  true (when this row is the active reply target)
 *   data-jumped:       true (ephemeral; flash animation, removed after ~1500ms)
 */

const _DMSG_JUMP_FLASH_MS = 1500;

// Track the active jump flash at module scope rather than as a property on the
// row element — that way if the row is removed while the timer is pending, we
// don't keep a strong reference to the detached node, and a fresh jump on a
// different row can correctly cancel the previous flash.
let _dmsgActiveJumpRow = null;
let _dmsgActiveJumpTimer = null;

function applyHighlight(rowEl, kind) {
    if (!rowEl || !kind) return;
    switch (kind) {
        case 'pinged':
            rowEl.dataset.pinged = 'true';
            break;
        case 'replying':
            rowEl.dataset.replyingTo = 'true';
            break;
        case 'jumped': {
            // Spam-clicks on the same row are ignored — let the in-flight
            // animation complete to prevent CSS re-trigger stutter. Jumps to
            // a *different* row cancel the prior flash so two rows can't be
            // simultaneously animating.
            if (_dmsgActiveJumpRow === rowEl) return;
            if (_dmsgActiveJumpRow) clearHighlight(_dmsgActiveJumpRow, 'jumped');
            if (_dmsgActiveJumpTimer) clearTimeout(_dmsgActiveJumpTimer);
            _dmsgActiveJumpRow = rowEl;
            rowEl.dataset.jumped = 'true';
            _dmsgActiveJumpTimer = setTimeout(() => {
                clearHighlight(_dmsgActiveJumpRow, 'jumped');
                _dmsgActiveJumpRow = null;
                _dmsgActiveJumpTimer = null;
            }, _DMSG_JUMP_FLASH_MS);
            break;
        }
    }
}

function clearHighlight(rowEl, kind) {
    if (!rowEl || !kind) return;
    switch (kind) {
        case 'pinged':       delete rowEl.dataset.pinged; break;
        case 'replying':     delete rowEl.dataset.replyingTo; break;
        case 'jumped':       delete rowEl.dataset.jumped; break;
    }
}

