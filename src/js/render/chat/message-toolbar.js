/**
 * Floating hover toolbar for Discord-style message rows.
 *
 * Single instance (#dmsg-toolbar) appended to <body>, NOT per-row buttons.
 * Repositioned via getBoundingClientRect on row hover. Saves ~25k DOM nodes on a
 * 5k-message chat compared to per-row button columns.
 *
 * Buttons: 😀 react, ↩ reply, ✎ edit (mine + text only), 📁 reveal-file (mine + downloaded attachment).
 *
 * Wiring:
 *   - mouseover/mouseleave delegated on .chat-messages
 *   - mouseenter/leave on toolbar itself (cancels/restarts the hide timer)
 *   - scroll on .chat-messages: reposition; if row scrolls out, hide
 *   - click delegated on toolbar: routes to react / reply / edit / reveal-file handlers
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
