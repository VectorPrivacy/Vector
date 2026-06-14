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
            <button class="dmsg-toolbar-btn btn" data-action="copy-file" aria-label="Copy" title="Copy" hidden><span class="icon icon-copy"></span></button>
            <button class="dmsg-toolbar-btn btn" data-action="retry" aria-label="Retry send" title="Retry send" hidden><span class="icon icon-refresh"></span></button>
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
    // mouseout (bubbles) — fires when the cursor leaves a row, including
    // when it lands in the gap between rows or on chat-messages chrome.
    // Schedule a hide; the next mouseover (on another row, or the toolbar's
    // own mouseenter) cancels the pending timer if the cursor is just
    // transiting between hover targets.
    domChatMessages.addEventListener('mouseout', (e) => {
        const fromRow = e.target.closest('.dmsg');
        if (!fromRow) return;
        const to = e.relatedTarget;
        // Same row (cursor moved to a child within the row): ignore.
        if (to && fromRow.contains(to)) return;
        // Onto the toolbar: its own mouseenter handler cancels the hide,
        // so it's safe to still schedule here — the cancel wins the race.
        _dmsgScheduleToolbarHide();
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

    _dmsgInitGestures();

    _dmsgToolbarListenersAttached = true;
}

function showMessageToolbar(rowEl) {
    // Mobile has no hover — taps would otherwise pop this corner toolbar.
    // Touch surfaces use press-and-hold (context menu) + swipe (reply) instead.
    if (platformFeatures?.is_mobile) return;
    // Idempotent — re-creates the element if it was detached on a chat switch.
    initMessageToolbar();
    if (!_dmsgToolbarEl) return;

    _dmsgToolbarTarget = rowEl;
    _dmsgCancelToolbarHide();

    // Pending messages aren't on the wire yet — cancel-send lives on the
    // upload spinner itself, so suppress the hover toolbar entirely.
    const status = rowEl.dataset.status;
    if (status === 'pending') {
        _dmsgToolbarEl.hidden = true;
        return;
    }

    const mine = rowEl.dataset.mine === 'true';

    // Dissolved community: the backend drops every new event (react/reply/edit). Only
    // own-message delete still works (data ownership), so suppress the toolbar entirely
    // for others' messages and offer nothing here for own ones — delete lives in the
    // right-click / long-press menu, which is gated to the same single action.
    if (rowIsInDissolvedCommunity() && !mine) {
        _dmsgToolbarEl.hidden = true;
        return;
    }

    _dmsgToolbarEl.hidden = false;
    _dmsgToolbarEl.dataset.target = rowEl.id;
    const reactBtn = _dmsgToolbarEl.querySelector('[data-action="react"]');
    const replyBtn = _dmsgToolbarEl.querySelector('[data-action="reply"]');
    const editBtn = _dmsgToolbarEl.querySelector('[data-action="edit"]');
    const revealBtn = _dmsgToolbarEl.querySelector('[data-action="reveal-file"]');
    const copyFileBtn = _dmsgToolbarEl.querySelector('[data-action="copy-file"]');
    const retryBtn = _dmsgToolbarEl.querySelector('[data-action="retry"]');
    const deleteBtn = _dmsgToolbarEl.querySelector('[data-action="delete"]');

    // Failed sends: only retry + delete make sense (the message isn't on
    // the wire, so react/reply/edit/reveal don't apply).
    if (status === 'failed') {
        reactBtn.hidden = true;
        replyBtn.hidden = true;
        editBtn.hidden = true;
        revealBtn.hidden = true;
        copyFileBtn.hidden = true;
        retryBtn.hidden = false;
        deleteBtn.hidden = false;
        deleteBtn.dataset.mode = 'failed';
        deleteBtn.setAttribute('aria-label', 'Delete failed message');
        deleteBtn.setAttribute('title', 'Delete failed message');
        _dmsgPositionToolbar(rowEl);
        return;
    }
    retryBtn.hidden = true;
    replyBtn.hidden = false;

    const msg = _dmsgLookupMessage(rowEl);
    const hasContent = !!(msg && msg.content);
    const hasAttachments = !!(msg && msg.attachments && msg.attachments.length);

    // Dissolved community: react/reply/edit all produce events the backend drops, so
    // offering them would lie. Own-message delete still works and is handled below.
    const dissolved = rowIsInDissolvedCommunity();

    // React: hidden once the message hits the unique-emoji ceiling (matches the
    // inline "+" shortcut gating in _dmsgBuildReactions).
    const uniqueEmojiCount = msg && msg.reactions
        ? new Set(msg.reactions.map(r => r.emoji)).size
        : 0;
    reactBtn.hidden = dissolved || uniqueEmojiCount >= 8;
    replyBtn.hidden = dissolved;

    // Edit: own text-only messages (parity with legacy edit gate).
    editBtn.hidden = dissolved || !(mine && hasContent && !hasAttachments);

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

    // Copy-file: put the downloaded attachment on the OS clipboard as a real file
    // (paste into Finder/Explorer or another chat). Same gating as reveal — the
    // file must exist on disk. Desktop only for now (the backend errors elsewhere).
    copyFileBtn.hidden = !downloadedPath;
    if (downloadedPath) {
        copyFileBtn.dataset.path = downloadedPath;
    } else {
        delete copyFileBtn.dataset.path;
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
    // Pending/failed are handled by the early-return above — those rows
    // have their own cancel-upload / delete-failed UI on the row itself.
    deleteBtn.hidden = true;
    delete deleteBtn.dataset.mode;
    delete deleteBtn.dataset.partial;
    delete deleteBtn.dataset.hasAttachments;
    deleteBtn.style.opacity = '';
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
        case 'copy-file': {
            const path = btn.dataset.path;
            if (path) {
                invoke('write_clipboard_files', { paths: [path] })
                    .then(() => showToast('Copied file to clipboard'))
                    .catch((err) => showToast(String(err)));
            }
            break;
        }
        case 'retry': {
            const row = document.getElementById(targetId);
            const msg = row ? _dmsgLookupMessage(row) : null;
            if (msg && typeof retryFailedMessage === 'function') retryFailedMessage(msg);
            break;
        }
        case 'delete': {
            // Failed-message delete is local cleanup only (no NIP-09).
            if (btn.dataset.mode === 'failed') {
                if (typeof deleteFailedMessage === 'function') deleteFailedMessage(targetId);
                break;
            }
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

    // Owner moderation-hide (someone ELSE's Community message): a permanent cooperative hide
    // for everyone. No undo.
    if (mode === 'hide') {
        const confirmed = await popupConfirm('Hide this message?', 'Permanently hide this message for everyone in the community. This can\'t be undone.', false, '', 'vector_warning.svg');
        if (!confirmed) return;
        try {
            await invoke('hide_community_message', { channelId: strOpenChat, messageId: targetMsgId });
        } catch (err) {
            popupConfirm('Hide Failed', escapeHtml(String(err)), true, '', 'vector_warning.svg');
        }
        return;
    }

    // Three flows behind one button:
    //   delete (full)    = own message + retained keys: real
    //                      delete-from-network (NIP-09 against retained
    //                      wraps + cooperative-hide + Blossom blob delete)
    //   delete (partial) = own message, no retained keys: cooperative-hide
    //                      + Blossom blob delete + local hide. The relay
    //                      copy of the encrypted wrapper persists. Popup
    //                      explains why.

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
        // Community channels self-delete via their own retained-key path (§9), keyed by
        // the inner message id; DMs/MLS use the unified delete_own_message.
        const openChat = arrChats.find(c => c.id === strOpenChat);
        if (openChat && openChat.chat_type === 'Community') {
            await invoke('delete_community_message', { messageId: targetMsgId });
        } else {
            await invoke('delete_own_message', { messageId: targetMsgId });
        }
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

/* ──────────────────────────────────────────────────────────────────────────
   Chat gestures / context menu
   - Mobile: press-and-hold a row → menu; swipe a row left → reply. The hover
     toolbar is suppressed on mobile (see showMessageToolbar).
   - Desktop: right-click a row → the same menu (hover toolbar stays too).
   All delegated on domChatMessages.
   ────────────────────────────────────────────────────────────────────────── */

const _G_MOVE_TOL = 10;       // px before a touch counts as a drag, not a tap
const _G_SWIPE_MAX = 102;     // max leftward travel (rubber-banded past this)
const _G_SWIPE_TRIGGER = 60;  // travel needed to commit the reply
const _G_LONGPRESS_MS = 500;

let _gTouchRow = null;
let _gStartX = 0, _gStartY = 0;
let _gLastDX = 0;
let _gAxis = null;            // null until locked, then 'h' | 'v'
let _gLongTimer = null;
let _gLongFired = false;
let _gSwipeIcon = null;
let _dmsgRightClickHadSelection = false;

function _dmsgClearLongTimer() {
    if (_gLongTimer) { clearTimeout(_gLongTimer); _gLongTimer = null; }
}

function _dmsgCopyText(text) {
    if (!text) return;
    navigator.clipboard.writeText(text)
        .then(() => showToast('Copied to Clipboard'))
        .catch(() => showToast('Failed to Copy'));
}

function _dmsgEnsureSwipeIcon() {
    // Shared, content-space child of domChatMessages (like the toolbar) so it
    // tracks scroll for free. domChatMessages is wiped on chat switch, so
    // rebuild when detached.
    if (_gSwipeIcon && _gSwipeIcon.isConnected) return _gSwipeIcon;
    const el = document.createElement('div');
    el.className = 'dmsg-swipe-reply';
    el.hidden = true;
    const glyph = document.createElement('span');
    glyph.className = 'dmsg-swipe-reply__icon icon icon-reply';
    el.appendChild(glyph);
    // Prepend, never append: the chip is positioned by computed coords, so DOM
    // order is irrelevant to where it renders — but appending would make it the
    // container's :last-child and steal the trailing margin from the last row.
    domChatMessages.prepend(el);
    _gSwipeIcon = el;
    return el;
}

function _dmsgBeginSwipeVisual(rowEl) {
    rowEl.style.transition = 'none';
    const icon = _dmsgEnsureSwipeIcon();
    icon.hidden = false;
    icon.classList.remove('past');
    // Revert to the CSS transition (glow/border ease; transform + opacity are
    // driven per-frame below, so they stay instant by not being listed there).
    icon.style.transition = '';
    icon.style.opacity = '0';
    icon.style.transform = 'scale(0.4)';
    icon.style.top = `${rowEl.offsetTop + (rowEl.offsetHeight / 2) - 18}px`;
    // Pulled in from the right edge so the chip + accent glow clear the scrollbar.
    icon.style.left = `${rowEl.offsetLeft + rowEl.offsetWidth - 58}px`;
}

function _dmsgUpdateSwipeVisual(rowEl, offset, past) {
    rowEl.style.transform = `translateX(${offset}px)`;
    if (_gSwipeIcon) {
        const p = Math.min(1, Math.abs(offset) / _G_SWIPE_TRIGGER);
        _gSwipeIcon.style.opacity = String(p);
        // Scale up as the gesture nears commit; the pop past threshold is a
        // one-shot keyframe on the inner glyph (see CSS), so no conflict here.
        _gSwipeIcon.style.transform = `scale(${(0.4 + 0.6 * p).toFixed(3)})`;
        _gSwipeIcon.classList.toggle('past', past);
    }
}

function _dmsgResetSwipe(animate, rowEl, spring) {
    if (rowEl) {
        // Non-committed let-go gets a small springy overshoot; a committed
        // swipe returns flat (it's handing off to reply mode).
        if (!animate) rowEl.style.transition = 'none';
        else if (spring) rowEl.style.transition = 'transform 0.3s cubic-bezier(0.34, 1.4, 0.7, 1)';
        else rowEl.style.transition = 'transform 0.18s ease';
        rowEl.style.transform = '';
    }
    if (_gSwipeIcon) {
        _gSwipeIcon.style.transition = animate ? 'opacity 0.2s ease, transform 0.2s ease' : 'none';
        _gSwipeIcon.style.opacity = '0';
        _gSwipeIcon.style.transform = 'scale(0.4)';
        _gSwipeIcon.classList.remove('past');
    }
}

function _dmsgInitGestures() {
    // Desktop: right-click a row opens the same menu (mobile uses press-and-hold
    // below). Reaction chips keep their own right-click handler.
    //
    // The browser auto-selects the word under a right-click on `mousedown`
    // (before `contextmenu`), so suppressing it there is too late. We instead
    // block that auto-select on the right mousedown — UNLESS the user already
    // had text highlighted, in which case we leave the selection alone and
    // defer to the native menu so they can act on their snippet.
    domChatMessages.addEventListener('mousedown', (e) => {
        if (e.button !== 2 || platformFeatures?.is_mobile) return;
        const sel = window.getSelection();
        _dmsgRightClickHadSelection = !!(sel && !sel.isCollapsed && sel.toString().trim());
        if (_dmsgRightClickHadSelection) return;
        const row = e.target.closest('.dmsg');
        if (!row || !domChatMessages.contains(row) || e.target.closest('.reaction, .dmsg-avatar, .dmsg-author')) return;
        e.preventDefault();
    });
    domChatMessages.addEventListener('contextmenu', (e) => {
        if (platformFeatures?.is_mobile) return;
        const row = e.target.closest('.dmsg');
        if (!row || !domChatMessages.contains(row)) return;
        if (row.style.opacity === '0') return;
        if (e.target.closest('.reaction')) return;
        // Avatar / username act like a left-click (open the mini-profile), not
        // the message menu.
        const profileBtn = e.target.closest('.dmsg-avatar, .dmsg-author');
        if (profileBtn) {
            if (profileBtn.dataset.npub) {
                e.preventDefault();
                showMiniProfile(profileBtn.dataset.npub, profileBtn);
            }
            return;
        }
        if (_dmsgRightClickHadSelection) return;          // had a highlight → native menu
        const sel = window.getSelection();
        if (sel) sel.removeAllRanges();                   // drop any word the right-click grabbed
        e.preventDefault();
        _dmsgOpenMessageMenu(row, e.clientX, e.clientY);
    });

    domChatMessages.addEventListener('touchstart', (e) => {
        if (!platformFeatures?.is_mobile) return;
        if (e.touches.length !== 1) return;           // ignore pinch/multi-touch
        const row = e.target.closest('.dmsg');
        if (!row || !domChatMessages.contains(row)) return;
        if (row.style.opacity === '0') return;        // mid fade-out
        // Reaction chips own their own long-press (details popup) — yield the
        // whole gesture so we don't stack the message menu on top of it.
        if (e.target.closest('.reaction')) return;
        // Avatar / username act like a tap (open the mini-profile), never the
        // message menu — captured here so a hold still resolves to the profile.
        const profileBtn = e.target.closest('.dmsg-avatar, .dmsg-author');
        _gTouchRow = row;
        const t = e.touches[0];
        _gStartX = t.clientX; _gStartY = t.clientY;
        _gLastDX = 0; _gAxis = null; _gLongFired = false;
        _dmsgClearLongTimer();
        _gLongTimer = setTimeout(() => {
            _gLongTimer = null;
            _gLongFired = true;
            // Suppress the WebView's synthetic mouse events / selection callout
            // for this gesture so they don't immediately dismiss the menu.
            try { e.preventDefault(); } catch (_e) {}
            if (profileBtn && profileBtn.dataset.npub) {
                showMiniProfile(profileBtn.dataset.npub, profileBtn);
            } else {
                _dmsgOpenMessageMenu(row, _gStartX, _gStartY);
            }
        }, _G_LONGPRESS_MS);
    }, { passive: false });

    domChatMessages.addEventListener('touchmove', (e) => {
        if (!_gTouchRow || _gLongFired) return;
        const t = e.touches[0];
        const dx = t.clientX - _gStartX;
        const dy = t.clientY - _gStartY;
        if (_gAxis === null) {
            if (Math.abs(dx) < _G_MOVE_TOL && Math.abs(dy) < _G_MOVE_TOL) return;
            // Past tap tolerance: it's a drag, not a press. Lock the axis.
            _dmsgClearLongTimer();
            _gAxis = Math.abs(dx) > Math.abs(dy) ? 'h' : 'v';
            if (_gAxis === 'v') { _gTouchRow = null; return; }  // vertical = scroll
            _dmsgBeginSwipeVisual(_gTouchRow);
        }
        if (_gAxis !== 'h') return;
        // Claim the gesture from the scroller and render leftward travel only,
        // rubber-banding both an accidental rightward pull and past the cap.
        e.preventDefault();
        let off = Math.min(0, dx);
        if (off < -_G_SWIPE_MAX) off = -_G_SWIPE_MAX + (off + _G_SWIPE_MAX) * 0.2;
        _gLastDX = dx;
        _dmsgUpdateSwipeVisual(_gTouchRow, off, off <= -_G_SWIPE_TRIGGER);
    }, { passive: false });

    const onEnd = () => {
        _dmsgClearLongTimer();
        const row = _gTouchRow, axis = _gAxis, dx = _gLastDX, longFired = _gLongFired;
        _gTouchRow = null; _gAxis = null; _gLastDX = 0; _gLongFired = false;
        if (longFired || !row) return;
        if (axis === 'h') {
            // Reply commits on a leftward swipe only — a rightward drag never
            // moves the row, so it must never fire either.
            const status = row.dataset.status;
            const willReply = dx <= -_G_SWIPE_TRIGGER && status !== 'pending' && status !== 'failed';
            // Spring-back only on a non-committed let-go; a committed swipe
            // returns flat (it's handing off to reply mode, not bouncing).
            _dmsgResetSwipe(true, row, !willReply);
            if (willReply) _dmsgSelectReply(row.id);
        }
    };
    domChatMessages.addEventListener('touchend', onEnd);
    domChatMessages.addEventListener('touchcancel', () => {
        _dmsgClearLongTimer();
        if (_gTouchRow) _dmsgResetSwipe(true, _gTouchRow, true);  // abandoned: spring back
        _gTouchRow = null; _gAxis = null; _gLastDX = 0; _gLongFired = false;
    });
}

/**
 * Build and show the message action menu for a row (mobile long-press / desktop
 * right-click). Mirrors the hover toolbar's visibility logic 1:1, including the
 * backend round-trip that decides delete vs. limited-delete vs. admin-hide.
 * Reveal-in-folder is desktop-only (no Android filesystem equivalent).
 */
async function _dmsgOpenMessageMenu(rowEl, x, y) {
    if (!rowEl) return;
    const targetId = rowEl.id;
    const status = rowEl.dataset.status;
    const mine = rowEl.dataset.mine === 'true';
    const msg = _dmsgLookupMessage(rowEl);
    const items = [];

    // Failed sends: local retry / delete only (not on the wire yet).
    if (status === 'failed') {
        items.push({ label: 'Retry send', icon: 'refresh', onClick: () => { if (msg) retryFailedMessage(msg); } });
        items.push({ label: 'Delete', icon: 'trash', danger: true, onClick: () => deleteFailedMessage(targetId) });
        showContextMenu({ x, y, items });
        return;
    }
    // Pending: nothing actionable (cancel-send lives on the upload spinner).
    if (status === 'pending') return;

    const hasContent = !!(msg && msg.content);
    const hasAttachments = !!(msg && msg.attachments && msg.attachments.length);
    const uniqueEmojiCount = msg && msg.reactions ? new Set(msg.reactions.map(r => r.emoji)).size : 0;

    // Dissolved community: react/reply/edit produce events the backend drops, so skip them. But Copy (and
    // revealing a downloaded file) are benign LOCAL actions and stay available on ANY message, own or not.
    // Own messages also keep Delete; admin Hide on others is blocked below (the backend rejects moderation
    // in a dead community anyway).
    const dissolved = rowIsInDissolvedCommunity();

    if (!dissolved) {
        if (uniqueEmojiCount < 8) {
            items.push({ label: 'React', icon: 'smile-face', onClick: () => _dmsgOpenReactionPicker(targetId) });
        }
        items.push({ label: 'Reply', icon: 'reply', onClick: () => _dmsgSelectReply(targetId) });
        if (mine && hasContent && !hasAttachments) {
            items.push({ label: 'Edit', icon: 'edit', onClick: () => { if (msg) startEditMessage(targetId, msg.content); } });
        }
    }
    // Reveal/Open a downloaded attachment. Desktop reveals it in the file
    // manager; Android has no "reveal in folder", so open it with the user's
    // chosen app (ACTION_VIEW chooser via the backend).
    {
        const downloadedPath = (msg && msg.attachments)
            ? (msg.attachments.find(a => a.downloaded) || {}).path
            : null;
        if (downloadedPath) {
            if (platformFeatures?.os === 'android') {
                items.push({ label: 'Open', icon: 'file-search', onClick: () => invoke('open_attachment', { path: downloadedPath }) });
                items.push({ label: 'Share', icon: 'share', onClick: () => invoke('share_attachment', { path: downloadedPath }) });
            } else if (!platformFeatures?.is_mobile) {
                items.push({ label: 'Reveal in folder', icon: 'file-search', onClick: () => revealItemInDir(downloadedPath) });
                items.push({ label: 'Copy', icon: 'copy', onClick: () => {
                    invoke('write_clipboard_files', { paths: [downloadedPath] })
                        .then(() => showToast('Copied file to clipboard'))
                        .catch((err) => showToast(String(err)));
                } });
            }
        }
    }
    // Copy — text selection is off on mobile. If the message carries markdown,
    // offer both flavours (plain vs as-sent); if it's provably plaintext
    // (stripping is a no-op), a single plain Copy is all that's needed.
    if (hasContent) {
        const raw = msg.content;
        const plain = stripMarkdownToPlain(raw);
        if (plain === raw.trim()) {
            items.push({ label: 'Copy', icon: 'copy', onClick: () => _dmsgCopyText(raw) });
        } else {
            items.push({ label: 'Copy', icon: 'copy', onClick: () => _dmsgCopyText(plain) });
            items.push({ label: 'Copy', hint: '(markdown)', icon: 'file-code', onClick: () => _dmsgCopyText(raw) });
        }
    }

    // Delete / Hide: same backend probe the desktop toolbar uses.
    let deleteItem = null;
    try {
        const opts = await invoke('get_message_delete_options', { messageId: targetId });
        if (opts.mine) {
            const partial = !opts.has_retained_keys;
            deleteItem = {
                label: partial ? 'Delete (limited)' : 'Delete',
                icon: 'trash', danger: true,
                onClick: () => _dmsgConfirmAndDelete(targetId, 'delete', { partial, hasAttachments: !!opts.has_attachments }),
            };
        } else if (opts.can_admin_hide && !dissolved) {
            // Moderation-hide is a new authority action the backend drops once dissolved — don't offer it.
            deleteItem = {
                label: 'Hide', icon: 'trash', danger: true,
                onClick: () => _dmsgConfirmAndDelete(targetId, 'hide', {}),
            };
        }
    } catch (_e) { /* no delete option for this row */ }
    if (deleteItem) {
        items.push({ divider: true });
        items.push(deleteItem);
    }

    showContextMenu({ x, y, items });
}
