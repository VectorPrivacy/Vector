/**
 * Floating hover toolbar for Discord-style message rows.
 *
 * Single instance (#dmsg-toolbar) appended to <body>, NOT per-row buttons.
 * Repositioned via getBoundingClientRect on row hover. Saves ~25k DOM nodes on a
 * 5k-message chat compared to per-row button columns.
 *
 * Buttons: 😀 react, ↩ reply, ✎ edit (mine + text only), 📁 reveal-file (mine + downloaded attachment), 🗑 delete (mine).
 *
 * Wiring:
 *   - mouseover/mouseleave delegated on .chat-messages
 *   - mouseenter/leave on toolbar itself (cancels/restarts the hide timer)
 *   - scroll on .chat-messages: reposition; if row scrolls out, hide
 *   - click delegated on toolbar: routes to react / reply / edit / reveal-file / delete handlers
 */

let _dmsgToolbarEl = null;
let _dmsgToolbarTarget = null;
let _dmsgToolbarHideTimer = null;
let _dmsgToolbarListenersAttached = false;

function initMessageToolbar() {
    if (typeof domChatMessages === 'undefined' || !domChatMessages) return;

    // (Re)create the toolbar element if missing or detached. domChatMessages is
    // cleared on every chat open, which removes the toolbar with it — but the
    // element reference and listeners-attached flag survive the call. So check
    // isConnected each time and rebuild the element when needed.
    if (!_dmsgToolbarEl || !_dmsgToolbarEl.isConnected) {
        // Explicitly drop the old element (in the rare case it's detached but
        // still referenced) so its inner listeners get GC'd along with it.
        if (_dmsgToolbarEl) _dmsgToolbarEl.remove();
        _dmsgToolbarTarget = null;
        _dmsgCancelToolbarHide();

        _dmsgToolbarEl = document.createElement('div');
        _dmsgToolbarEl.id = 'dmsg-toolbar';
        _dmsgToolbarEl.hidden = true;
        _dmsgToolbarEl.innerHTML = `
            <button class="dmsg-toolbar-btn btn" data-action="react" aria-label="Add reaction" title="Add reaction"><span class="icon icon-smile-face"></span></button>
            <button class="dmsg-toolbar-btn btn" data-action="reply" aria-label="Reply" title="Reply"><span class="icon icon-reply"></span></button>
            <button class="dmsg-toolbar-btn btn" data-action="edit" aria-label="Edit" title="Edit" hidden><span class="icon icon-edit"></span></button>
            <button class="dmsg-toolbar-btn btn" data-action="reveal-file" aria-label="Reveal in folder" title="Reveal in folder" hidden><span class="icon icon-file-search"></span></button>
            <button class="dmsg-toolbar-btn btn dmsg-toolbar-btn-danger" data-action="delete" aria-label="Delete message" title="Delete message" hidden><span class="icon icon-trash"></span></button>
        `;
        // Append INSIDE the scrolling container so the toolbar moves with the message
        // content automatically (no per-frame reposition on scroll = no lag).
        // Requires .chat-messages { position: relative } for absolute children to
        // anchor to its scroll content — that rule lives in the .dmsg CSS section.
        domChatMessages.appendChild(_dmsgToolbarEl);

        // Listeners that hang off the toolbar element itself must be re-attached
        // every rebuild (the previous instance is gone).
        _dmsgToolbarEl.addEventListener('mouseenter', _dmsgCancelToolbarHide);
        _dmsgToolbarEl.addEventListener('mouseleave', _dmsgScheduleToolbarHide);
        _dmsgToolbarEl.addEventListener('click', _dmsgHandleToolbarClick);
    }

    // Listeners that hang off domChatMessages itself only need attaching once —
    // the container survives chat switches, only its children get cleared.
    if (_dmsgToolbarListenersAttached) return;

    domChatMessages.addEventListener('mouseover', (e) => {
        const row = e.target.closest('.dmsg');
        if (!row || !domChatMessages.contains(row)) return;
        // Skip rows mid-fade-out (set by message_removed handler).
        if (row.style.opacity === '0') return;
        if (row === _dmsgToolbarTarget) {
            _dmsgCancelToolbarHide();
            return;
        }
        showMessageToolbar(row);
    });
    domChatMessages.addEventListener('mouseleave', () => {
        _dmsgScheduleToolbarHide();
    });
    // The toolbar now scrolls naturally with content, so no per-scroll reposition.
    // We still listen so we can hide the toolbar if the target row scrolls out of view.
    domChatMessages.addEventListener('scroll', () => {
        if (!_dmsgToolbarTarget || !_dmsgToolbarEl || _dmsgToolbarEl.hidden) return;
        const rowTop = _dmsgToolbarTarget.offsetTop;
        const rowBottom = rowTop + _dmsgToolbarTarget.offsetHeight;
        const viewTop = domChatMessages.scrollTop;
        const viewBottom = viewTop + domChatMessages.clientHeight;
        if (rowBottom < viewTop || rowTop > viewBottom) {
            hideMessageToolbar();
        }
    }, { passive: true });

    _dmsgToolbarListenersAttached = true;
}

function showMessageToolbar(rowEl) {
    // Idempotent — re-creates the element if it was detached on a chat switch.
    initMessageToolbar();
    if (!_dmsgToolbarEl) return;

    _dmsgToolbarTarget = rowEl;
    _dmsgCancelToolbarHide();
    _dmsgToolbarEl.hidden = false;
    _dmsgToolbarEl.dataset.target = rowEl.id;

    const mine = rowEl.dataset.mine === 'true';
    const reactBtn = _dmsgToolbarEl.querySelector('[data-action="react"]');
    const editBtn = _dmsgToolbarEl.querySelector('[data-action="edit"]');
    const revealBtn = _dmsgToolbarEl.querySelector('[data-action="reveal-file"]');
    const deleteBtn = _dmsgToolbarEl.querySelector('[data-action="delete"]');

    const msg = _dmsgLookupMessage(rowEl);
    const hasContent = !!(msg && msg.content);
    const hasAttachments = !!(msg && msg.attachments && msg.attachments.length);

    // React: hidden once the message hits the unique-emoji ceiling (matches the
    // inline "+" shortcut gating in _dmsgBuildReactions).
    const uniqueEmojiCount = msg && msg.reactions
        ? new Set(msg.reactions.map(r => r.emoji)).size
        : 0;
    reactBtn.hidden = uniqueEmojiCount >= 8;

    // Edit: own text-only messages (parity with legacy edit gate).
    editBtn.hidden = !(mine && hasContent && !hasAttachments);

    // Reveal-file: any message with at least one downloaded attachment.
    // Mirrors legacy behavior — the reveal button isn't restricted to own
    // messages; you can open downloaded files received from others (esp. in
    // group chats where this is the primary path to media you've saved).
    const downloadedPath = (msg && msg.attachments)
        ? (msg.attachments.find(a => a.downloaded) || {}).path
        : null;
    revealBtn.hidden = !downloadedPath;
    if (downloadedPath) {
        revealBtn.dataset.path = downloadedPath;
    } else {
        delete revealBtn.dataset.path;
    }

    // Delete / Hide button visibility:
    //   - Own messages: SYNC reveal at full opacity (always actionable).
    //     The click always does something useful — relay nuke if we
    //     hold retained keys, otherwise cooperative-hide + Blossom
    //     blob delete on attachments + local hide. The popup explains
    //     what will happen.
    //   - Others' group messages: ASYNC reveal only if the user is an
    //     admin (admin-hide flow). Costs one round-trip per hover for
    //     non-mine rows; cached via the data-mode attribute so repeats
    //     don't re-fetch.
    //   - Pending/failed messages: hidden — those have their own
    //     cancel-upload / delete-failed paths.
    const isPending = rowEl.dataset.status === 'pending' || rowEl.dataset.status === 'failed';
    deleteBtn.hidden = true;
    delete deleteBtn.dataset.mode;
    delete deleteBtn.dataset.partial;
    delete deleteBtn.dataset.hasAttachments;
    deleteBtn.style.opacity = '';
    if (!isPending) {
        if (mine) {
            // Sync reveal — the icon is always present on own rows, so
            // there's no async pop-in. Opacity is resolved by the
            // backend round-trip below; until then we render at full
            // opacity (the common case) and dial it down only if the
            // backend reports no retained keys.
            deleteBtn.dataset.mode = 'delete';
            deleteBtn.setAttribute('aria-label', 'Delete message');
            deleteBtn.setAttribute('title', 'Delete message');
            deleteBtn.hidden = false;
        }
        const targetId = rowEl.id;
        invoke('get_message_delete_options', { messageId: targetId }).then(opts => {
            // Bail if the toolbar moved on — async hop may resolve
            // after hover ends.
            if (_dmsgToolbarTarget !== rowEl) return;
            let widthChanged = false;
            if (opts.mine) {
                // Reduced opacity when full network deletion isn't
                // available (no retained keys). The icon stays clickable
                // — the popup explains what will and won't happen.
                const fullyDeletable = opts.has_retained_keys;
                deleteBtn.style.opacity = fullyDeletable ? '' : '0.45';
                deleteBtn.dataset.partial = fullyDeletable ? '' : '1';
                deleteBtn.dataset.hasAttachments = opts.has_attachments ? '1' : '';
                const tip = fullyDeletable
                    ? 'Delete message'
                    : 'Delete message (limited)';
                deleteBtn.setAttribute('title', tip);
            } else if (opts.can_admin_hide) {
                deleteBtn.dataset.mode = 'hide';
                deleteBtn.setAttribute('aria-label', 'Hide message');
                deleteBtn.setAttribute('title', 'Hide message');
                if (deleteBtn.hidden) widthChanged = true;
                deleteBtn.hidden = false;
            }
            // Width changed only when admin-hide reveals an icon that
            // wasn't there at sync time. Mine rows already had the icon
            // at sync time, so no reposition needed there.
            if (widthChanged) _dmsgPositionToolbar(rowEl);
        }).catch(() => {/* opacity stays default for own messages */});
    }

    _dmsgPositionToolbar(rowEl);
}

function hideMessageToolbar() {
    if (!_dmsgToolbarEl) return;
    _dmsgToolbarEl.hidden = true;
    _dmsgToolbarTarget = null;
    delete _dmsgToolbarEl.dataset.target;
}

function _dmsgScheduleToolbarHide() {
    _dmsgCancelToolbarHide();
    _dmsgToolbarHideTimer = setTimeout(() => hideMessageToolbar(), 100);
}

function _dmsgCancelToolbarHide() {
    if (_dmsgToolbarHideTimer) {
        clearTimeout(_dmsgToolbarHideTimer);
        _dmsgToolbarHideTimer = null;
    }
}

function _dmsgPositionToolbar(rowEl) {
    if (!_dmsgToolbarEl || !rowEl) return;

    // Position is computed in the chat-messages SCROLL CONTENT coordinate space,
    // not the viewport — the toolbar is now an absolutely-positioned child of
    // domChatMessages, so it scrolls with the content automatically and no
    // per-frame reposition is needed during scroll.
    const tbW = _dmsgToolbarEl.offsetWidth || 140;
    const rowTop = rowEl.offsetTop;
    const rowRight = rowEl.offsetLeft + rowEl.offsetWidth;

    // Anchor: top-right of the row, raised so it overlaps the row above.
    let top = rowTop - 16;
    let left = rowRight - tbW - 8;

    // Don't push above the very top of the scroll content.
    if (top < 0) top = rowTop + 4;
    // Don't bleed off the left edge of the chat content area.
    if (left < 0) left = 0;

    _dmsgToolbarEl.style.top = `${top}px`;
    _dmsgToolbarEl.style.left = `${left}px`;
}

function _dmsgHandleToolbarClick(e) {
    const btn = e.target.closest('.dmsg-toolbar-btn');
    if (!btn) return;
    const action = btn.dataset.action;
    const targetId = _dmsgToolbarEl.dataset.target;
    if (!targetId) return;
    e.stopPropagation();

    // The toolbar stays visible after a click — actions don't dismiss it. The
    // natural mouseleave handler hides it when the cursor leaves the row.
    // Hiding on click + auto-reshow because the cursor is still over the row
    // caused a visual flicker.
    switch (action) {
        case 'react':
            _dmsgOpenReactionPicker(targetId);
            break;
        case 'reply':
            _dmsgSelectReply(targetId);
            break;
        case 'edit': {
            const row = document.getElementById(targetId);
            const msg = row ? _dmsgLookupMessage(row) : null;
            if (msg) startEditMessage(targetId, msg.content);
            break;
        }
        case 'reveal-file': {
            const path = btn.dataset.path;
            if (path) revealItemInDir(path);
            break;
        }
        case 'delete': {
            const mode = btn.dataset.mode === 'hide' ? 'hide' : 'delete';
            const partial = btn.dataset.partial === '1';
            const hasAttachments = btn.dataset.hasAttachments === '1';
            _dmsgConfirmAndDelete(targetId, mode, { partial, hasAttachments });
            break;
        }
    }
}

/**
 * Confirm-and-delete flow. Asks the user once, then issues the delete
 * via the backend. The backend publishes NIP-09 deletions against every
 * retained gift-wrap (real "delete from network") and emits
 * `message_removed` to fade the row out — same handler as cancel-upload
 * and delete-failed, so no extra DOM work needed here.
 *
 * Honest framing: we promise removal from inbox relays, not from
 * recipients who already received the message. If the message predates
 * the retention feature (no ephemeral keys held) we refuse outright and
 * explain — silently local-deleting would mislead the user into
 * thinking the message is gone when it's still sitting on relays.
 */
async function _dmsgConfirmAndDelete(targetMsgId, mode, opts) {
    opts = opts || {};
    // Three flows behind one button:
    //   delete (full)    = own message + retained keys: real
    //                      delete-from-network (NIP-09 against retained
    //                      wraps + cooperative-hide + Blossom blob delete)
    //   delete (partial) = own message, no retained keys: cooperative-hide
    //                      + Blossom blob delete + local hide. The relay
    //                      copy of the encrypted wrapper persists. Popup
    //                      explains why.
    //   hide             = admin moderation of someone else's group
    //                      message. Cooperative-only (kind-5 inside MLS).
    if (mode === 'hide') {
        const confirmed = await popupConfirm(
            'Hide this message?',
            'Are you sure you want to hide this message?',
            false,
            '',
            'vector_warning.svg'
        );
        if (!confirmed) return;
        const groupId = strOpenChat;
        if (!groupId) {
            popupConfirm('Hide Failed', 'Could not determine the group this message belongs to.', true, '', 'vector_warning.svg');
            return;
        }
        try {
            await invoke('admin_hide_message', { groupId, messageId: targetMsgId });
        } catch (err) {
            popupConfirm('Hide Failed', escapeHtml(String(err)), true, '', 'vector_warning.svg');
        }
        return;
    }

    // delete branch
    let title, body;
    if (opts.partial) {
        title = 'Limited Delete';
        if (opts.hasAttachments) {
            body = 'This message can\'t be fully removed from relays because the signing key isn\'t on this device (sent from another device, or predates deletion support).<br><br>' +
                'We can still:<br>' +
                '<b>Delete the attached file</b> from the storage server.<br>' +
                '<b>Notify other Vector users</b> to drop the message.<br><br>' +
                'Continue?';
        } else {
            body = 'This message can\'t be fully removed from relays because the signing key isn\'t on this device (sent from another device, or predates deletion support).<br><br>' +
                'We can still <b>notify other Vector users</b> to drop the message from their copy.<br><br>' +
                'Continue?';
        }
    } else {
        title = 'Delete this message?';
        body = 'Are you sure you want to delete this message?';
    }
    const confirmed = await popupConfirm(title, body, false, '', 'vector_warning.svg');
    if (!confirmed) return;
    try {
        await invoke('delete_own_message', { messageId: targetMsgId });
    } catch (err) {
        popupConfirm('Delete Failed', escapeHtml(String(err)), true, '', 'vector_warning.svg');
    }
}

/**
 * Open the emoji picker in reaction-mode for a given message.
 *
 * `openEmojiPanel` (in main.js) was originally wired to a click event on a
 * per-row `.add-reaction` chip, and to enter reaction-mode it inspects
 * `e.target.classList.contains('dmsg-react-trigger')` and walks up two parents
 * for the message id. Since the new shell has no per-row chip, we synthesize
 * a detached DOM tree with the right shape and pass it as `e.target`.
 */
function _dmsgOpenReactionPicker(targetMsgId) {
    const fakeRoot = document.createElement('div');
    fakeRoot.id = targetMsgId;
    const fakeMid = document.createElement('div');
    fakeRoot.appendChild(fakeMid);
    const fakeBtn = document.createElement('span');
    fakeBtn.classList.add('dmsg-react-trigger');
    fakeMid.appendChild(fakeBtn);
    openEmojiPanel({ target: fakeBtn });
}

/**
 * Mark the row as the active reply target and configure the chat input for
 * a reply (cancel button visible, placeholder, focus). Replaces the prior
 * row's highlight (so only one row is ever highlighted as the reply target).
 */
function _dmsgSelectReply(targetMsgId) {
    if (strCurrentReplyReference) {
        const prev = document.getElementById(strCurrentReplyReference);
        if (prev) clearHighlight(prev, 'replying');
    }

    strCurrentReplyReference = targetMsgId;

    domChatMessageInputFile.style.display = 'none';
    domChatMessageInputCancel.style.display = '';
    domChatMessageInput.setAttribute('placeholder', 'Enter reply...');
    if (!platformFeatures.is_mobile) {
        domChatMessageInput.focus();
    }

    const target = document.getElementById(targetMsgId);
    if (target) applyHighlight(target, 'replying');
}
