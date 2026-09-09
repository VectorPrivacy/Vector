// Backend → frontend: the Rust event listeners, and the buffers that bridge
// events arriving ahead of the DOM they target. One global scope: this loads
// before main.js and shares its globals.

// In-flight optimistic reaction adds (msg id -> [{id, author_id, emoji, at}]).
// message_update re-applies these over an incoming payload so a spree's earlier
// echo can't visually un-react a later click; entries drop once their own echo
// confirms them (or after the TTL if the send died without an error).
const pendingReactions = new Map();
const PENDING_REACTION_TTL_MS = 10000;

/**
 * Setup our Rust Event listeners, used for relaying the majority of backend changes
 */
async function setupRustListeners() {
    // Fire all listener registrations in parallel (each await listen() is an IPC round-trip)
    const _p = [];
    const _on = (event, handler) => _p.push(listen(event, handler));

    // A Community invite (npub gift-wrap) was parked → surface it as a pending slot.
    _on('community_invite_received', async (evt) => {
        await loadCommunityInvites();
        invitesChanged();
        adjustSize();
    });

    // Cross-device: boot reconcile purged parked invites for communities we already joined elsewhere.
    // Re-pull the (now-pruned) list so those stale invite rows vanish without a restart.
    _on('community_invites_purged', async () => {
        await loadCommunityInvites();
        invitesChanged();
        adjustSize();
    });

    // §6.2 self-removal: a cooperative kick of us, a ban-rekey exclusion, OR a leave another device
    // authored. The backend already wiped this community's local data (retaining the epoch keys for a
    // later self-scrub). Silently mirror it in the UI — close the view + drop it from the list — with no
    // popup (the removal speaks for itself).
    _on('community_kicked', async (evt) => {
        const communityId = evt.payload?.community_id || evt.payload;
        if (!communityId) return;
        await removeCommunityFromUI(communityId);
    });

    // A pack's health verdict changed (revoked / missing / revived) or a pack
    // was deleted. Reload the local mirror so the picker greys out or revives
    // the section live, instead of waiting for the next panel open.
    _on('emoji_packs_updated', () => loadEmojiPacks());

    // Another device pinned or unpinned a chat. Re-sort and repaint — the pin
    // may name a chat this device has not synced yet, which is fine: the rank
    // lookup simply finds it when it arrives.
    _on('pinned_chats_updated', (evt) => {
        arrPinnedChats = Array.isArray(evt.payload) ? evt.payload : [];
        listChanged();
    });

    // The boot DM-relay-list sync adopted/retired relays; repaint the Network
    // panel so an already-open list reflects them without a reopen.
    _on('relay_list_updated', () => renderRelayList());

    // A control change (banlist / roles / metadata / invite-mode) landed in REALTIME (via the 3308
    // control-plane subscription). Re-read this community's summary into the chat list + re-render the
    // overview if it's open, so online members see name/role/mode changes live, not just on next open.
    _on('community_refreshed', async (evt) => {
        const communityId = evt.payload?.community_id || evt.payload;
        if (!communityId) return;
        // A control change (ban/role/mode/metadata) can move the roster — refresh the count immediately.
        refreshCommunityMemberCount(communityId, true);
        // Roster moves can flip moderation-hide authority, so the cached delete-meta
        // verdicts may be stale — clear them to re-resolve on next render/hover.
        dmsgClearDeleteMetaCache();
        try {
            const summary = await invoke('get_community', { communityId });
            // A folded control edition can add, rename or tombstone channels.
            setCommunityChannels(communityId, summary.channels);
            for (const c of arrChats) {
                if (c.metadata?.custom_fields?.community_id !== communityId) continue;
                const f = c.metadata.custom_fields;
                f.name = summary.name;
                f.description = summary.description || '';
                f.is_owner = summary.is_owner ? 'true' : 'false';
                f.dissolved = summary.dissolved ? 'true' : 'false';
                if (summary.owner_npub) f.owner_npub = summary.owner_npub;
            }
        } catch (_) {}
        // The metadata edit may have swapped the icon — re-cache it (URL-keyed fast-path no-ops when
        // unchanged) and repoint avatar_cached, so a received icon change shows live for every member,
        // not only after a hard reload.
        try {
            const cachedPath = await invoke('cache_community_image', { communityId, isBanner: false });
            if (cachedPath) {
                for (const c of arrChats) {
                    if (c.metadata?.custom_fields?.community_id === communityId) c.metadata.avatar_cached = cachedPath;
                }
            }
        } catch (_) {}
        // A control change may have promoted/demoted admins — refresh the cached roster so in-chat
        // admin tags + @everyone reflect it (the open overview re-fetches separately below).
        loadCommunityRoles(communityId);
        // An open moderation console is a live view of who is in the room. A ban, kick
        // or rotation changes exactly that, so it must not keep offering rows the
        // network has already removed — an operator would tick one and act on a roster
        // that no longer exists.
        modNoteControlChange(communityId);
        // A ban hides what they already posted, but only in the QUERIES — the painted
        // timeline and its cache still hold the rows. Drop the cache for this
        // community's channels and re-open the visible one so it re-reads the filtered
        // source. Deliberately NOT a JS-side banlist filter: the rule lives in one
        // place, in SQL, and a second copy here would be the one that goes stale.
        purgeCommunityMessageCache(communityId);
        communityChanged(communityId);
        // Re-render the open overview (re-fetches caps/members/banlist fresh) if it's this community.
        if (domGroupOverview.style.display !== 'none' && VectorSvelte.overviewState().groupId === communityId) {
            const chat = arrChats.find(c => c.metadata?.custom_fields?.community_id === communityId);
            // Live refresh, so an active member filter survives someone else's role/ban change.
            if (chat) renderCommunityOverview(chat, true);
        }
        // Re-render the OPEN channel's header so a live metadata edit (name/description/icon) shows
        // immediately, not only after navigating away and back. The chatlist + overview refresh above.
        if (strOpenChat) {
            const open = arrChats.find(c => c.id === strOpenChat);
            if (open && open.metadata?.custom_fields?.community_id === communityId) {
                setChatHeader(open);
                // A community that seals WHILE it's the open view: openChat won't re-run, so lock the
                // composer + drop the end divider live here (the flag was refreshed above).
                if (chatIsDissolved(open)) applyDissolvedChatUI(open);
            }
        }
    });

    // A v1 community upgraded to Concord v2: the chat row is re-parented in place (same
    // chat_identifier, so history/unread survive). Refresh its metadata to the v2 identity
    // and drop one "Community upgraded" line into the open timeline. Nothing else moves.
    /// Drop cached/painted messages for a community so the next read comes from the
    /// filtered queries.
    ///
    /// Called when control state moves (ban, kick, rotation). The ban rule is enforced
    /// in SQL and nowhere else — re-reading is what makes this correct rather than a
    /// second implementation that can disagree with the first.
    async function purgeCommunityMessageCache(communityId) {
        const channels = arrChats.filter(c => c.metadata?.custom_fields?.community_id === communityId);
        if (!channels.length) return;
        for (const ch of channels) {
            eventCache.clearConversation(ch.id);
            ch.messages = [];
        }
        // Only the visible channel needs repainting; the rest reload when opened.
        const open = channels.find(c => c.id === strOpenChat);
        if (open) await openChat(open.id);
    }

    _on('community_migrated', async (evt) => {
        const v1Id = evt.payload?.v1_community_id;
        const v2Id = evt.payload?.v2_community_id;
        if (!v2Id) return;
        // Re-point any chat row still tagged with the v1 community id to the v2 identity.
        // This is load-bearing (it unlocks the composer + clears the dissolved flag), so it
        // runs UNCONDITIONALLY — the get_community fetch below only decorates name/owner and
        // is best-effort; a failed fetch must not leave the room locked.
        const rows = arrChats.filter(c => {
            const f = c.metadata?.custom_fields;
            return f && (f.community_id === v1Id || f.community_id === v2Id);
        });
        for (const c of rows) {
            const f = c.metadata.custom_fields;
            f.community_id = v2Id;
            f.proto_version = '2';
            f.dissolved = 'false';
        }
        try {
            const summary = await invoke('get_community', { communityId: v2Id });
            for (const c of rows) {
                const f = c.metadata.custom_fields;
                f.name = summary.name;
                if (summary.owner_npub) f.owner_npub = summary.owner_npub;
            }
        } catch (_) {}
        communityChanged(communityId);
        if (strOpenChat) {
            const open = arrChats.find(c => c.id === strOpenChat);
            if (open && open.metadata?.custom_fields?.community_id === v2Id) {
                // The carrier's dissolved-fold ran the composer lockdown (disabled input,
                // hidden file/voice/emoji, "dissolved" notice) a beat before this migration
                // event. The room is ALIVE on v2, so fully RESTORE the composer — mirror
                // openChat's live-chat branch, not just the placeholder.
                VectorSvelte.setNotice('dissolved', '');
                VectorSvelte.setNotice('migrated', true);
                VectorSvelte.setLock(null);
                VectorSvelte.flushSync();
            }
        }
    });

    // A community synced in from another device (cross-device Community List, §6.3) appeared seamlessly —
    // render its metadata via the same path as a manual join so name/crown/members show without a restart.
    _on('community_surfaced', async (evt) => {
        const summary = evt.payload;
        if (!summary || !summary.community_id) return;
        await surfaceCommunitySummary(summary);
        refreshCommunityMemberCount(summary.community_id, true);
    });

    // Listen for system events (member joined/left, etc.)
    _on('system_event', async (evt) => {
        try {
            const { conversation_id, event_id, event_type, member_pubkey, member_name } = evt.payload || {};

            // Deduplication by event_id
            const chat = arrChats.find(c => c.id === conversation_id);
            if (chat && chat.messages.some(msg => msg.id === event_id)) {
                return;
            }

            // Resolve the actor's CURRENT cached name (member_name from the backend is null for
            // community presence; the name lives in our profile cache). Fetch it if unknown so a
            // later repaint/reload shows the real name instead of the npub.
            if (member_pubkey && !arrProfiles.some(p => p.id === member_pubkey) && !strangerProfileRequested.has(member_pubkey)) {
                strangerProfileRequested.add(member_pubkey);
                invoke('load_profile', { npub: member_pubkey }).catch(() => {});
            }
            const content = systemEventContent(event_type, member_pubkey);

            // Use the event's REAL time so it sorts chronologically. A join replayed during history paging /
            // rehydration would otherwise be stamped `now` and sink to the bottom of the chat.
            const atMs = Number(evt.payload?.created_at_ms) || Date.now();

            // Create system event message using the event_id
            const systemMsg = {
                id: event_id,
                at: atMs,
                content: content,
                mine: false,
                attachments: [],
                system_event: {
                    event_type: event_type,
                    member_npub: member_pubkey,
                }
            };

            // Add to chat messages via cache (handles deduplication)
            // Note: chat.messages and cache share the same array reference, so only use cache
            eventCache.addEvent(conversation_id, systemMsg);

            // Cache the latest membership event for the chatlist preview of a message-less community.
            // (chat.messages isn't aliased to the cache when the community isn't open, so the preview
            // can't see this event there.) Patch the row directly — the state hash doesn't track it.
            if (chat && (!chat.lastSystemEvent || atMs >= chat.lastSystemEvent.at)) {
                chat.lastSystemEvent = { event_type, member_npub: member_pubkey, at: atMs };
                if (!chat.messages?.some(m => !m.system_event)) updateChatlistPreview(conversation_id);
            }

            // Paint into the OPEN view only if this is genuinely the newest event AND the live tail is on
            // screen (DOM windowing). A historical replay (paging / rehydration) is already in the cache at
            // its real time and renders in order on the next chat open/render — appending it to the bottom
            // here would misplace it. When windowed-and-scrolled-up the data is in the cache; the next
            // scroll-down windows it in.
            if (strOpenChat === conversation_id && domChatMessages) {
                // Same windowing render gate as message_new: a jumpToUnread resolve
                // freezes the window (data-only), and we paint ONLY on a genuine
                // tail-append (this event lands immediately after the DOM's bottom row).
                // A historical replay sorts into the middle — it must not append here.
                const frozen = CHAT_WINDOW_ENABLED && _unreadJumpResolving;
                const bottomIdx = CHAT_WINDOW_ENABLED ? _windowBottomRenderedIndex() : -1;
                const newIdx = CHAT_WINDOW_ENABLED ? _windowIndexOfId(event_id) : -1;
                // Windowed: only paint at the live tail. Seeked away (windowAtTail false),
                // a newest system event still appends to the bounded slice and would pass
                // the index check, so require isAtDataBottom() too.
                const tailAppend = CHAT_WINDOW_ENABLED
                    ? (isAtDataBottom() && (bottomIdx === -1 || newIdx === bottomIdx + 1))
                    : (atMs >= (chat?.messages || []).reduce((mx, m) => (m.id !== event_id && m.at > mx ? m.at : mx), 0) && isAtDataBottom());
                if (!frozen && tailAppend) {
                    // The list island renders it (and folds a repeat into its run's head).
                    ensureMessageList();
                    _updateChatWindow(chat, [systemMsg], null);
                    softChatScroll();
                    if (CHAT_WINDOW_ENABLED) { _windowReseatAnchorsFromDom(); windowTrimTopIfOver(); }
                }
                refreshChatEmptyState(); // a "X joined" landed in the open chat → drop the start marker
            }

            chatChanged(chat);

            // A member join/leave moved the roster — refresh this community's cached member count.
            const evCommunityId = chat?.metadata?.custom_fields?.community_id;
            if (evCommunityId) refreshCommunityMemberCount(evCommunityId, true);
        } catch (e) {
            console.error('Error handling system_event:', e);
        }
    });

    // Listen for Synchronisation Finish updates
    // Badge cache resolved post-sync — lift emoji-pack limits if we hold the
    // Vector badge. Pure UI gating; the backend enforces authoritatively.
    _on('badges_updated', (evt) => {
        _myBadges = { vector: !!evt.payload?.vector, tier: evt.payload?.tier | 0, bug_hunter: evt.payload?.bug_hunter | 0 };
        applyTierLimits(_myBadges.tier);
        // Our own open profile re-derives its badges from the fresh cache.
        if (domProfile.style.display !== 'none' && VectorSvelte.profileViewState().id === strPubkey) {
            VectorSvelte.touchProfile(strPubkey);
        }
    });

    _on('sync_finished', async (_) => {
        fSyncing = false;
        // Mark sync as complete - this allows real-time messages to be cached
        fSyncComplete = true;
        
        // Retract to the centre — the mirror of the reveal. `progress` is kept for
        // the duration so the bar doesn't flash back to full width before shrinking
        // (dropping the mask restores the whole line instantly); only `active` goes,
        // to stop the pulse.
        domSyncLine.classList.remove('active');
        domSyncLine.classList.add('fade-out');

        // Matches the 0.4s retract — clearing early left the class dangling mid-animation.
        setTimeout(() => {
            domSyncLine.classList.remove('fade-out', 'progress');
            domSyncLine.style.removeProperty('--sync-progress');
            if (!strOpenChat) adjustSize();
        }, 400);
    });

    // Listen for Synchronisation Progress updates
    _on('sync_progress', (evt) => {
        // The quick phase runs inside boot, so gating the whole handler on `fInit`
        // meant it never showed a bar for the fastest, most common sync. Only the
        // layout reflow below needs the gate.
        const { mode, current, total } = evt.payload || {};
        fSyncing = true;
        // `active` is the centre reveal and carries BOTH modes — a determinate sync
        // (the common one, since the quick phase runs inside boot) used to jump
        // straight to `progress` and grow out of the left edge instead.
        domSyncLine.classList.remove('fade-out');
        domSyncLine.classList.add('active');
        if (mode === 'Syncing' && current && total) {
            // Determinate: fill left-to-right within the revealed line (mask).
            domSyncLine.classList.add('progress');
            domSyncLine.style.setProperty('--sync-progress', Math.min(current / total, 1));
        } else {
            // Indeterminate pulse (reconciliation phase — total unknown).
            domSyncLine.classList.remove('progress');
            domSyncLine.style.removeProperty('--sync-progress');
        }
        if (!fInit && !strOpenChat) adjustSize();
    });

    // Every relay refused to reconcile AND the incremental read came back empty:
    // the pool is unreachable, not the inbox empty. Without this the two are
    // indistinguishable and a total sync failure looks like a quiet day.
    _on('sync_unreachable', (evt) => {
        const n = evt.payload?.relays ?? 0;
        showToast(`Couldn't reach any of your ${n} relays, new messages may be missing`);
    });

    // Upload progress lands in the transfer store; the attachment components read it.
    _on('attachment_upload_progress', async (evt) => {
        VectorSvelte.uploadProgressed(evt.payload.id, evt.payload.progress, evt.payload.bytesSent);
    });

    // Listen for backend error toasts
    _on('show_toast', (evt) => {
        showToast(evt.payload || 'An Error Occurred');
    });

    // Listen for Attachment Download Progress events
    _on('attachment_download_progress', async (evt) => {
        if (!strOpenChat) return;
        VectorSvelte.downloadProgressed(evt.payload.id, evt.payload.progress, evt.payload.bytesPerSec);
    });

    // Listen for Attachment Download Results
    _on('attachment_download_result', async (evt) => {
        // When an attachment is being updated (i.e: post-hashing ID change), we reference the original nonce-based hash via old_id, otherwise, we use ID, as nothing changed
        const matchId = evt.payload?.old_id || evt.payload.id;

        // Bookkeeping FIRST: if the message left the render window before this
        // result arrived, the early returns below would otherwise strand these
        // entries — and the auto-download dedup gate would then silently skip
        // this attachment for the rest of the session.
        downloadingAttachmentIds.delete(matchId);
        downloadingAttachmentIds.delete(evt.payload.id);
        VectorSvelte.transferDone(matchId);
        VectorSvelte.transferDone(evt.payload.id);

        // Update the in-memory attachment (works for both DMs and Group Chats)
        let cChat = getChat(evt.payload.profile_id);
        if (!cChat) return;

        let cMsg = cChat.messages.find(m => m.id === evt.payload.msg_id);
        if (!cMsg) return;

        let cAttachment = cMsg.attachments.find(a => a.id === matchId);
        if (!cAttachment) return;

        cAttachment.downloading = false;
        cAttachment.download_failed = false;
        if (evt.payload.success) {
            cAttachment.downloaded = true;
            // Update path from backend result (always has the correct file path)
            if (evt.payload.result) {
                cAttachment.path = evt.payload.result;
            }
            // Update ID if hash changed (nonce → blossom hash)
            if (evt.payload.old_id) {
                cAttachment.id = evt.payload.id;
            }

            // Update ALL not-yet-downloaded in-memory attachments with the same hash (deduplication)
            // and collect their message IDs for re-rendering
            // Skip already-downloaded attachments — they have valid paths and loaded metadata
            const affectedMsgIds = new Set();
            affectedMsgIds.add(evt.payload.msg_id);
            for (const msg of cChat.messages) {
                if (msg.id === evt.payload.msg_id) continue;
                for (const att of msg.attachments) {
                    if (att.id === matchId && !att.downloaded) {
                        att.downloading = false;
                        att.downloaded = true;
                        att.download_failed = false;
                        att.path = cAttachment.path;
                        if (evt.payload.old_id) {
                            att.id = evt.payload.id;
                        }
                        affectedMsgIds.add(msg.id);
                    }
                }
            }

            // Re-render all affected messages in the open chat
            if (strOpenChat === evt.payload.profile_id) {
                const profile = getProfile(evt.payload.profile_id);
                for (const msgId of affectedMsgIds) {
                    const domMsg = document.getElementById(msgId);
                    const memMsg = cChat.messages.find(m => m.id === msgId);
                    if (domMsg && memMsg) {
                        // Shrink + fade out any active spinners before re-rendering
                        const spinners = domMsg.querySelectorAll('.miniapp-downloading-spinner');
                        if (spinners.length) {
                            for (const sp of spinners) {
                                sp.style.transition = 'opacity 0.2s ease, scale 0.2s ease';
                                sp.style.opacity = '0';
                                sp.style.scale = '0.5';
                            }
                            setTimeout(() => {
                                const newEl = updateMessageRow(domMsg, memMsg, profile, msgId);
                                // Grow + fade in the new icon
                                const icon = newEl.querySelector('.custom-audio-player > span[class*="icon-"], .custom-audio-player > img');
                                if (icon) {
                                    icon.style.opacity = '0';
                                    icon.style.scale = '0.5';
                                    icon.style.transition = 'opacity 0.25s ease, scale 0.25s ease';
                                    requestAnimationFrame(() => { icon.style.opacity = '1'; icon.style.scale = '1'; });
                                }
                                softChatScroll();
                            }, 200);
                        } else {
                            updateMessageRow(domMsg, memMsg, profile, msgId);
                        }
                    }
                }
                softChatScroll();
            }
        } else {
            // Download failed — mark EVERY loaded copy failed first. The auto-download
            // gate reads this flag, so it must flip even when the chat is closed or the
            // row is outside the render window; gating the mark on the DOM would leave
            // an unmarked copy free to re-trigger (or strand) the download later.
            // The backend's reason string rides along: the UI renders it and the
            // console keeps it, so a failure is never just a generic red box.
            const failReason = typeof evt.payload.result === 'string' ? evt.payload.result : '';
            console.error(`[AttachmentDownload] ${matchId} failed: ${failReason}`);
            const failedMsgIds = [];
            for (const msg of cChat.messages) {
                let touched = false;
                for (const att of msg.attachments) {
                    if (att.id === matchId) {
                        att.downloading = false;
                        att.download_failed = true;
                        att.download_error = failReason;
                        touched = true;
                    }
                }
                if (touched) failedMsgIds.push(msg.id);
            }
            // Then swap any painted rows over to the failed/retry rendering
            if (strOpenChat === evt.payload.profile_id) {
                const profile = getProfile(evt.payload.profile_id);
                for (const msgId of failedMsgIds) {
                    const domMsg = document.getElementById(msgId);
                    const memMsg = cChat.messages.find(m => m.id === msgId);
                    if (domMsg && memMsg) {
                        updateMessageRow(domMsg, memMsg, profile, msgId);
                    }
                }
            }
        }
    });

    // Listen for profile updates
    _on('profile_update', (evt) => {
        // Check if the frontend is already aware
        const nProfileIdx = arrProfiles.findIndex(p => p.id === evt.payload.id);
        let avatarCacheChanged = false;
        if (nProfileIdx >= 0) {
            // Check if avatar cache changed (for triggering chatlist re-render)
            avatarCacheChanged = arrProfiles[nProfileIdx].avatar_cached !== evt.payload.avatar_cached;

            // Update our frontend memory
            arrProfiles[nProfileIdx] = evt.payload;
            profileIndex.set(evt.payload.id, evt.payload);

            // If this is our profile, make sure to render it's changes
            if (arrProfiles[nProfileIdx].mine) {
                renderCurrentProfile(arrProfiles[nProfileIdx]);
            }
        } else {
            // Add the new profile
            arrProfiles.push(evt.payload);
            avatarCacheChanged = !!evt.payload.avatar_cached;
        }

        // If this user has an open chat, then soft-update the chat header
        if (strOpenChat === evt.payload.id) {
            const chat = getDMChat(evt.payload.id);
            const profile = getProfile(evt.payload.id);
            if (chat && profile) {
                updateChat(chat, [], profile);
            }
        }

        // Every row and list entry showing this profile re-derives (name, avatar, bot mark).
        VectorSvelte.touchProfile(evt.payload.id);
        
        // Update already-painted message rows authored by this npub — name + avatar — so chat
        // history reflects the resolved profile without needing a reopen (matches the system-event
        // and member-list retro-resolve).
        {
            const id = evt.payload.id;
            const newName = evt.payload.nickname || evt.payload.name || evt.payload.display_name || (id.substring(0, 12) + '…');
            const newAvatarSrc = getProfileAvatarSrc(evt.payload);
            // Rows derive their author and avatar from the profile signal; reply
            // quotes and mention chips are vanilla leaves patched here. One grouped
            // scan; the static NodeList keeps replaceWith safe mid-iteration.
            document.querySelectorAll(
                `.dmsg-reply-name[data-npub="${id}"], .dmsg-reply-avatar[data-npub="${id}"], ` +
                `.mention[data-npub="${id}"]`
            ).forEach(el => {
                if (el.classList.contains('dmsg-reply-name')) {
                    // Reply-quote name resolves the same as the author name.
                    el.textContent = newName;
                    twemojify(el);
                } else if (el.classList.contains('dmsg-reply-avatar')) {
                    const fresh = createAvatarImg(newAvatarSrc, 16);
                    fresh.classList.add('dmsg-reply-avatar');
                    fresh.dataset.npub = id;
                    // Re-wire the mini-profile opener the original render attached (replaceWith drops it).
                    fresh.addEventListener('click', (e) => { e.stopPropagation(); showMiniProfile(id, e.currentTarget); });
                    el.replaceWith(fresh);
                } else if (el.classList.contains('mention')) {
                    // Mention chips (@tags in chat + npub tags in profile bios).
                    el.textContent = '@' + newName;
                }
            });
        }
        
        // Skip Expanded Profile View repaints during our own edit mode —
        // backend may emit stale `banner_cached` and clobber the just-picked image.
        if (domProfile.style.display !== 'none' && VectorSvelte.profileViewState().id === evt.payload.id) {
            const isOwnEditingProfile = fProfileEditMode && evt.payload.mine;
            if (!isOwnEditingProfile) {
                renderProfileTab(evt.payload);
            }
        }

        // Everything derived from this profile (the mini profile popup included) repaints.
        VectorSvelte.touchProfile(evt.payload.id);

        // Retro-resolve system events (join/leave lines) that rendered with this
        // npub's stub before the profile loaded — both the cached content and any
        // already-painted DOM line, plus buffered (not-yet-revealed) events.
        for (const chat of arrChats) {
            for (const m of chat.messages || []) {
                if (m.system_event?.member_npub === evt.payload.id) {
                    m.content = systemEventContent(m.system_event.event_type, evt.payload.id);
                    const el = document.getElementById(m.id);
                    if (el) {
                        // Patch only the clickable name span (preserves the affordance + suffix);
                        // fall back to whole-line text for a legacy plain-rendered line.
                        const nameEl = el.querySelector('.system-event-name');
                        if (nameEl) nameEl.textContent = systemEventName(evt.payload.id);
                        else el.textContent = m.content;
                        // Swap the placeholder avatar for the now-cached one.
                        const avatarEl = el.querySelector('.system-event-avatar');
                        if (avatarEl) {
                            const fresh = createAvatarImg(getProfileAvatarSrc(getProfile(evt.payload.id)), 16);
                            fresh.classList.add('system-event-avatar');
                            avatarEl.replaceWith(fresh);
                        }
                    }
                }
            }
        }
        for (const buffer of _systemEventBuffer.values()) {
            for (const m of buffer) {
                if (m.system_event?.member_npub === evt.payload.id) {
                    m.content = systemEventContent(m.system_event.event_type, evt.payload.id);
                }
            }
        }

        // Refresh the Create Group picker if a stranger npub's profile resolved while the
        // panel is open (the island re-derives its rows off the new snapshot).
        if (domCreateGroup?.style.display !== 'none') {
            VectorSvelte.ccProfilesChanged();
        }
        if (activeInviteModalRerender) {
            activeInviteModalRerender();
        }

        // Upgrade any message-less community whose preview shows THIS npub's join from the npub stub
        // to the resolved name. The group row's state hash doesn't track the join actor, so renderChatlist
        // alone wouldn't repaint it — patch the row directly.
        for (const chat of arrChats) {
            const se = latestPreviewSystemEvent(chat);
            if (se && se.member_npub === evt.payload.id) touchChatRow(chat);
        }

        // This profile's DM row re-derives (name, avatar, bot mark); the list re-diffs
        // in case a block flag changed its membership. No other row is touched.
        VectorSvelte.touchProfile(evt.payload.id);
        reorderChatlist();
    });

    _on('chat_muted', (evt) => {
        // Muting someone we've never DM'd: the backend creates a hidden shell
        // row, mirror it so profile UI and sender-mute checks see it.
        let cChat = arrChats.find(c => c.id === evt.payload.chat_id);
        if (!cChat && evt.payload.value && evt.payload.chat_id.startsWith('npub1')) {
            cChat = getOrCreateChat(evt.payload.chat_id, 'DirectMessage');
        }
        if (cChat) {
            cChat.muted = evt.payload.value;
        }

        // If this group's overview is open, update the mute button
        const domGrpMuteBtn = document.getElementById('group-mute-btn');
        if (domGrpMuteBtn && domGroupOverview.style.display !== 'none' && strOpenChat === evt.payload.chat_id) {
            domGrpMuteBtn.querySelector('span').className = `icon icon-volume-${evt.payload.value ? 'mute' : 'max'} navbar-icon`;
            domGrpMuteBtn.querySelector('p').innerText = evt.payload.value ? 'Unmute' : 'Mute';
        }

        // Reflect glow/badge changes now, then pull fresh DB counts. A sender mute
        // changes OTHER chats' (community) badges too.
        chatChanged(chat);
        if (!chatIsGroup(chat)) communitiesChanged();
        scheduleUnreadRefresh();
    });

    _on('profile_nick_changed', (evt) => {
        // Update the profile's nickname
        const cProfile = getProfile(evt.payload.profile_id);
        if (cProfile) {
            cProfile.nickname = evt.payload.value;

            // If this profile is Expanded, update the UI
            if (VectorSvelte.profileViewState().id === evt.payload.profile_id) {
                renderProfileTab(cProfile);
            }
            // One helper owns every surface that shows a name.
            refreshRenderedName(evt.payload.profile_id);
        }
    });

    // PIVX payment events — handler in pivx.js
    _on('pivx_payment_received', handlePivxPaymentReceived);

    // Listen for typing indicator updates (both DMs and Groups)
    _on('typing-update', (evt) => {
        const { conversation_id, typers } = evt.payload;

        // Find the chat (could be DM or group)
        const chat = arrChats.find(c => c.id === conversation_id);
        if (!chat) return;

        // Store the typers array and update timestamp
        chat.active_typers = typers || [];
        chat.last_typing_update = Date.now() / 1000;

        // If this chat is currently open, update the chat header subtext
        if (strOpenChat === conversation_id) {
            updateChatHeaderSubtext(chat);
        }

        // Typing changes one row's preview and nothing about the order.
        touchChatRow(chat);
    });

    // Listen for incoming DM messages
    _on('message_new', (evt) => {
        // chat_id is the npub for DMs, the group id for MLS, the channel id for
        // Communities. Resolve the existing chat by id first; only create when truly new,
        // picking the type by id shape (npub → DM, otherwise a Community channel).
        let chat = arrChats.find(c => c.id === evt.payload.chat_id);
        if (!chat) {
            chat = evt.payload.chat_id.startsWith('npub1')
                ? getOrCreateDMChat(evt.payload.chat_id)
                : getOrCreateChat(evt.payload.chat_id, 'Community');
        }
        
        // Early-unlock an optimistic "Joining…" row the moment a message streams in (proves
        // read access) — the other release path is the control-fold/sync resolving in acceptCommunityInvite.
        if (chat._joining) clearCommunityJoining(chat.metadata?.custom_fields?.community_id);

        // An open moderation console is looking at a roster that was read when it
        // opened. A raid arriving after that was invisible in the one tool meant to
        // remove it, so let arrivals refresh it (debounced inside).
        modNoteActivity(chat.metadata?.custom_fields?.community_id);

        // Get the new message
        const newMessage = evt.payload.message;

        // Add to event cache
        // During sync, only add if this chat is currently open (to avoid cache flooding)
        // After sync complete, always add to cache
        const shouldAddToCache = fSyncComplete || chat.id === strOpenChat;
        let cacheInsertedIntoChatMessages = false;
        if (shouldAddToCache) {
            const added = eventCache.addEvent(chat.id, newMessage);
            if (!added) return;
            // openChat assigns chat.messages = entry.events, so the two often
            // share an array reference. addEvent already inserted into that
            // shared array — a second manual insertion below would duplicate.
            cacheInsertedIntoChatMessages = chat.messages === eventCache.getEventsRef(chat.id);
        }

        // A message from the sender ends their typing indicator immediately (don't wait for the
        // expiry tick). DM typers are keyed by the chat id (the contact npub); Community typers by
        // the sender's own npub, so resolve the sender rather than assuming it's the chat id, then
        // refresh the header/preview right away so it short-circuits.
        if (!newMessage.mine && chat.active_typers && chat.active_typers.length) {
            const senderNpub = newMessage.npub || chat.id;
            chat.active_typers = chat.active_typers.filter(npub => npub !== senderNpub);
            if (strOpenChat === chat.id) updateChatHeaderSubtext(chat);
            touchChatRow(chat);
        }

        if (!cacheInsertedIntoChatMessages) {
            // Find the correct position to insert the message based on timestamp
            const messages = chat.messages;

            // Check if the array is empty or the new message is newer than (or equal to) the newest message
            if (messages.length === 0 || newMessage.at >= messages[messages.length - 1].at) {
                // Insert at the end (newest)
                messages.push(newMessage);
            }
            // Check if the new message is older than the oldest message
            else if (newMessage.at < messages[0].at) {
                // Insert at the beginning (oldest)
                messages.unshift(newMessage);
            }
            // Otherwise, find the correct position in the middle
            else {
                // Binary search for better performance with large message arrays
                let low = 0;
                let high = messages.length - 1;

                while (low <= high) {
                    const mid = Math.floor((low + high) / 2);

                    if (messages[mid].at < newMessage.at) {
                        low = mid + 1;
                    } else {
                        high = mid - 1;
                    }
                }

                // Insert the message at the correct position (low is now the index where it should go)
                messages.splice(low, 0, newMessage);
            }
        }

        // Newest-first chat list sort (independent of how the message landed
        // in chat.messages).
        if (newMessage.at >= (chat.messages[chat.messages.length - 1]?.at ?? 0)) {
            sortChats();
        }

        // If this user has the open chat, then update the chat too
        if (strOpenChat === chat.id) {
            // Any row already rendered quoting THIS message can now draw its strip:
            // it's in chat.messages as of the insert above.
            backfillReplyContext(newMessage.id);
            // DOM windowing render gate. A jumpToUnread resolve freezes the window
            // entirely — its relay-walk/DB-pull echoes are data-only (already in
            // chat.messages above), so skip ALL rendering AND badge updates; the
            // window renders once, at the jump.
            const frozen = CHAT_WINDOW_ENABLED && _unreadJumpResolving;
            // Gate on a GENUINE tail-append, not "at bottom": a row renders only if
            // it lands immediately AFTER the DOM's bottom-rendered message (or the
            // window is empty). An OLDER insert (a back-paged history echo) sorts into
            // the MIDDLE of chat.messages — it must NOT prepend into the DOM.
            const bottomIdx = CHAT_WINDOW_ENABLED ? _windowBottomRenderedIndex() : -1;
            const newIdx = CHAT_WINDOW_ENABLED ? _windowIndexOfId(newMessage.id) : -1;
            // When seeked away (windowAtTail false) chat.messages is a bounded slice
            // whose end is NOT the live tail — a newest arrival still appends to that
            // slice and would satisfy the index check, so it must ALSO be at the tail.
            const atTail = !CHAT_WINDOW_ENABLED || isAtDataBottom();
            const tailAppend = atTail && (bottomIdx === -1 || newIdx === bottomIdx + 1);
            let rendered = false;
            if (frozen) {
                // Data-only: chat.messages/cache already holds it. No DOM, no badge.
                proceduralScrollState.totalMessageCount++;
            } else if (!CHAT_WINDOW_ENABLED) {
                updateChat(chat, [newMessage], null, false, true);
                rendered = true;
                refreshChatEmptyState();
                proceduralScrollState.renderedMessageCount++;
                proceduralScrollState.totalMessageCount++;
            } else if (newMessage.mine && !tailAppend) {
                // Own send while scrolled up / seeked away: re-seat the window at the
                // live tail so the sent row is visible, then pin (windowJumpToBottom snaps).
                windowJumpToBottom();
                rendered = true;
                refreshChatEmptyState();
                proceduralScrollState.renderedMessageCount++;
                proceduralScrollState.totalMessageCount++;
            } else if (tailAppend) {
                // The next message after the rendered bottom (and we're at the tail) →
                // append + trim. The append keeps the window glued to the live tail.
                updateChat(chat, [newMessage], null, false, true);
                windowBottomId = newMessage.id;   // the append made it the bottom row
                windowAtTail = true;              // still glued to the tail
                windowTrimTopIfOver();
                rendered = true;
                refreshChatEmptyState(); // first message in a fresh community → drop the start marker
                proceduralScrollState.renderedMessageCount++;
                proceduralScrollState.totalMessageCount++;
            } else {
                // Not a tail-append. Two very different cases share this branch:
                // an older HISTORY-ECHO beyond the window (pure data, no DOM), and a
                // LIVE arrival that sorts mid-window — a peer whose clock lags stamps
                // behind our own sends, and CORD orders by sender stamp. The latter
                // MUST paint, or the message is invisible until the chat reopens.
                const range = _currentWindowRange();
                const midWindowLive = !newMessage.mine
                    && atTail
                    && chatPinnedToBottom
                    && range && newIdx > range[0] && newIdx < range[1];
                if (midWindowLive) {
                    const repaintChat = chat.id;
                    (async () => {
                        await renderWindow(range[0], range[1]);
                        if (strOpenChat !== repaintChat) return;
                        windowAtTail = true;
                        scrollToBottom(domChatMessages, false);
                    })();
                    rendered = true;
                    proceduralScrollState.renderedMessageCount++;
                }
                proceduralScrollState.totalMessageCount++;
                const winMsgs = _windowMessages();
                const newestIdx = (winMsgs?.length || chat.messages.length) - 1;
                if (!newMessage.mine && !midWindowLive && newIdx === newestIdx) incrementUnreadBelow();
            }
            // Open chat + pinned + window actually visible = user saw it
            // land. Tabbed-out arrivals stay unread until refocus, even when
            // the chat is open and pinned. Only when the row actually rendered at
            // the tail (the user saw it) — never on a data-only/frozen path.
            if (!newMessage.mine && rendered && tailAppend && chatPinnedToBottom && isWindowActive()) {
                markAsRead(chat, newMessage);
                clearUnreadDivider();
            }
            // Own-send catches up to the latest non-mine message AT OR BEFORE this send
            // (never past it — see findLatestContactMessage).
            if (newMessage.mine) {
                const lastContactMsg = findLatestContactMessage(chat.messages, newMessage.at);
                if (lastContactMsg) markAsRead(chat, lastContactMsg);
            }
        } else if (newMessage.mine) {
            // Own message synced from another device — mark read up to the latest contact
            // message no newer than this send. Bounding by `newMessage.at` is what keeps a
            // boot sweep's replay of our OLD sends from marking genuinely-new arrivals read.
            const lastContactMsg = findLatestContactMessage(chat.messages, newMessage.at);
            if (lastContactMsg) markAsRead(chat, lastContactMsg);
        }

        // One row re-derives and the order re-diffs; cheap enough to run with a chat
        // open too, which is what keeps the widescreen list live.
        touchChatRow(chat);
        reorderChatlist();

        // Re-derive unread badges from the DB (a new arrival, or an open-chat auto-read, both move
        // the count). Debounced so a burst of arrivals is one query.
        scheduleUnreadRefresh();

        // Update the back button notification dot (for unread messages in other chats)
    });

    // Listen for existing message updates (works for both DMs and MLS groups)
    _on('message_update', (evt) => {
        // The upload finished or failed; a 100% frame isn't always emitted.
        VectorSvelte.transferDone(evt.payload.old_id);

        // Find the message we're updating
        const cChat = getChat(evt.payload.chat_id);
        if (!cChat) return;

        const nMsgIdx = cChat.messages.findIndex(m => m.id === evt.payload.old_id);
        if (nMsgIdx === -1) return;

        // Update it. The row's key rides along, so the finalized message keeps its row.
        evt.payload.message._key = cChat.messages[nMsgIdx]._key || evt.payload.old_id;
        cChat.messages[nMsgIdx] = evt.payload.message;

        // Re-apply in-flight optimistic reactions this echo predates: during a
        // spree, click #1's echo must not visually un-react click #2. Entries the
        // payload confirms (or that expired) drop; the rest ride the incoming
        // message until their own echo arrives.
        const inflight = pendingReactions.get(evt.payload.old_id);
        if (inflight) {
            const now = Date.now();
            const still = inflight.filter(p =>
                now - p.at < PENDING_REACTION_TTL_MS &&
                !evt.payload.message.reactions.some(r => r.author_id === p.author_id && r.emoji === p.emoji)
            );
            for (const p of still) {
                evt.payload.message.reactions.push({ id: p.id, reference_id: evt.payload.message.id, author_id: p.author_id, emoji: p.emoji, emoji_url: p.emoji_url || null });
            }
            if (still.length) pendingReactions.set(evt.payload.old_id, still);
            else pendingReactions.delete(evt.payload.old_id);
        }
        
        // Also update the event cache
        // This is important for pending->sent transitions where the ID changes
        if (eventCache.has(evt.payload.chat_id)) {
            const cachedEvents = eventCache.getEvents(evt.payload.chat_id);
            if (cachedEvents) {
                const cacheIdx = cachedEvents.findIndex(m => m.id === evt.payload.old_id);
                if (cacheIdx !== -1) {
                    cachedEvents[cacheIdx] = evt.payload.message;
                }
                // Keep the dedup Set in sync with the id swap, else the relay echo of
                // the finalized id renders as a duplicate (esp. for Community sends).
                if (evt.payload.old_id !== evt.payload.message.id) {
                    eventCache.replaceId(evt.payload.chat_id, evt.payload.old_id, evt.payload.message.id);
                    // Carry delete-meta to the finalized id (the Community-send path that
                    // doesn't go through finalizePendingMessage): own sends retain keys +
                    // are never admin-hideable; drop the orphaned optimistic-id entry.
                    dmsgInvalidateDeleteMeta(evt.payload.old_id);
                    if (evt.payload.message.mine) dmsgSetOwnDeleteMeta(evt.payload.message.id, true);
                }
            }
        }

        // If this chat is open, then update the rendered message
        if (strOpenChat === evt.payload.chat_id) {
            // If `message_update` arrives before `message_new` has rendered the row,
            // `domMsg` is null and the `?.replaceWith` below is a no-op. The next
            // `message_new` will render the up-to-date message from chat.messages,
            // so missing the surgical update here is safe.
            const domMsg = document.getElementById(evt.payload.old_id);
            // The row refills its body only when the content signature changes, so a
            // reaction echo leaves video playback and spoiler reveals alone.
            if (domMsg) {
                const profile = getProfile(evt.payload.chat_id);
                updateMessageRow(domMsg, evt.payload.message, profile, evt.payload.old_id);
            }

            // The row may have grown after its initial layout (a reaction chip added in
            // realtime, an edit, an attachment finishing). Keep a bottom-pinned user pinned.
            compensateChatScrollForResize();

            // If the old ID was a pending ID (our message), make sure to update accordingly
            if (evt.payload.old_id.startsWith('pending')) {
                strLastMsgID = evt.payload.message.id;
            }

            // Update any reply contexts that quote this edited message
            const editedMsgId = evt.payload.message.id;
            const newContent = evt.payload.message.content;

            // Find all messages that reply to this edited message and update their reply preview
            const replyElements = document.querySelectorAll(`[id="r-${editedMsgId}"]`);
            for (const replyEl of replyElements) {
                const replyTextSpan = replyEl.querySelector('.dmsg-reply-text');
                if (replyTextSpan && newContent) {
                    replyTextSpan.innerHTML = buildReplyPreviewHtml(newContent);
                    twemojify(replyTextSpan);
                    const editedTags = evt.payload.message.emoji_tags;
                    if (editedTags && editedTags.length && typeof renderCustomEmojiShortcodes === 'function') {
                        renderCustomEmojiShortcodes(replyTextSpan, editedTags);
                    }
                }
            }

            // Also update the replied_to_content in cached message data
            for (const msg of cChat.messages) {
                if (msg.replied_to === editedMsgId) {
                    msg.replied_to_content = newContent;
                }
            }
        }

        // Update chatlist preview if the edited message is the last message in the chat
        // This efficiently updates just the preview text instead of re-rendering the entire chatlist
        const isLastMessage = nMsgIdx === cChat.messages.length - 1;
        if (isLastMessage) {
            updateChatlistPreview(evt.payload.chat_id);
        }
    });

    // Listen for message removal (e.g., cancelled upload, deleted failed message)
    _on('message_removed', (evt) => {
        const { id, chat_id, reason } = evt.payload;
        // A message vanishing (deletion or self-destruct) must not leave you
        // stuck replying to / reacting to it.
        _exitModesForRemovedMessage(id);
        VectorSvelte.transferDone(id);
        const cChat = getChat(chat_id);
        if (!cChat) return;

        // Remove from in-memory messages
        const msgIdx = cChat.messages.findIndex(m => m.id === id);
        if (msgIdx !== -1) cChat.messages.splice(msgIdx, 1);

        // Remove from event cache
        if (eventCache.has(chat_id)) {
            const cachedEvents = eventCache.getEvents(chat_id);
            if (cachedEvents) {
                const cacheIdx = cachedEvents.findIndex(m => m.id === id);
                if (cacheIdx !== -1) cachedEvents.splice(cacheIdx, 1);
            }
        }

        // Fade out and remove DOM element if this chat is open
        if (strOpenChat === chat_id) {
            const domMsg = document.getElementById(id);
            if (domMsg) {
                // If the floating toolbar was anchored to this row, hide it —
                // the row is about to vanish and the toolbar would otherwise
                // stay at its last position pointing at nothing.
                if (_dmsgToolbarTarget === domMsg) hideMessageToolbar();

                // One fewer thing to scroll down to. Measured BEFORE the fade
                // collapses the row, and only for rows actually below the fold:
                // the badge counts arrivals the reader has not reached, so a
                // moderator clearing six posts otherwise leaves "6+ new
                // messages" pointing at rows that no longer exist.
                if (unreadBelowCount > 0
                    && domMsg.offsetTop >= domChatMessages.scrollTop + domChatMessages.clientHeight) {
                    setUnreadBelow(unreadBelowCount - 1);
                    // Nothing unread left below means the divider marks nothing.
                    if (unreadBelowCount === 0) clearUnreadDivider();
                }

                // Remember the row that follows ours so we can re-evaluate its
                // streak attribute after removal (it may flip first ↔ continuation).
                if (reason === 'self-destruct') {
                    // Self-Destruct expiry → Tron "derez" dissolve, not the plain fade.
                    _derezRowDom(domMsg);
                } else {
                    domMsg.style.transition = 'opacity 0.2s ease, max-height 0.3s ease';
                    domMsg.style.opacity = '0';
                    domMsg.style.maxHeight = domMsg.offsetHeight + 'px';
                    domMsg.style.overflow = 'hidden';
                    requestAnimationFrame(() => {
                        domMsg.style.maxHeight = '0';
                        domMsg.style.marginBottom = '0';
                        domMsg.style.paddingTop = '0';
                        domMsg.style.paddingBottom = '0';
                    });
                    setTimeout(() => {
                        // The array was spliced above; the list re-derives (streaks, separators,
                        // merged runs included) once the fade has played.
                        _windowReleaseAnchor(id);
                        VectorSvelte.touchWindow();
                        VectorSvelte.flushSync();
                        _dmsgUpdateLastSentVisibility();
                    }, 100);
                }
            }
        }

        // Toast only for actual cancelled uploads. Deletions (failed-message
        // cleanup, self-delete, admin-hide, cooperative-hide receiver) all
        // skip the toast — the row vanishing is signal enough.
        if (reason === 'cancelled') {
            showToast('Upload Cancelled');
        }

        // The row re-derives whole (unread glow included): deleting an unread message
        // can flip the chat back to fully read.
        chatChanged(evt.payload.chat_id);
        // The in-app chat-list badge is DB-sourced (chat.unread); re-derive it so deleting an unread
        // message drops the badge too. renderChatlist alone repaints the stale pre-deletion count.
        scheduleUnreadRefresh();

        // Recompute the OS taskbar badge — if the deleted message was unread,
        // the badge would otherwise stay stuck on its pre-deletion count.
        invoke('update_unread_counter');
    });

    // A backend heal flipped a message's full-vs-limited delete verdict (v2 scrub
    // keys re-derived during backfill) — drop the cached verdict so it re-resolves.
    _on('message_delete_meta_changed', (evt) => {
        const id = evt.payload?.id;
        if (!id) return;
        dmsgInvalidateDeleteMeta(id);
        // Row on screen: re-resolve now (a cache fill also refreshes an open toolbar).
        if (document.getElementById(id)) dmsgQueueDeleteMeta([id]);
    });

    // The background bot-manifest refresh converged — swap the `/` picker's list in live.
    _on('chat_commands_updated', (evt) => {
        if (commandCtrl && evt.payload?.chat_id) {
            commandCtrl.onCommandsUpdated(evt.payload.chat_id, evt.payload);
        }
    });

    // Listen for headless mark-as-read (e.g., notification "Mark Read" action while app backgrounded)
    _on('chat_mark_read', (evt) => {
        const { chat_id, last_read } = evt.payload;
        const cChat = getChat(chat_id);
        if (cChat && last_read) {
            cChat.last_read = last_read;
            // Re-derive the unread badge from the DB (the read just advanced, possibly to a
            // non-latest message on another device).
            scheduleUnreadRefresh();
            // The row re-derives (border, font color and badge all depend on unread state).
            chatChanged(chat);
        }
    });

    // Listen for attachment URL updates (for file uploads and reuse)
    // Live wallpaper upload progress — drives the conic-gradient ring on
    // the Set Wallpaper button during the encrypt+upload step.
    _on('wallpaper_upload_progress', (evt) => {
        const { chat_id, progress } = evt.payload || {};
        if (!chat_id || strOpenChat !== chat_id) return;
        if (!wallpaperPreviewState || wallpaperPreviewState.chatId !== chat_id) return;
        setWallpaperUploadProgress(progress || 0);
    });

    // Per-DM wallpaper changes. Both directions land here: our own publish
    // emits this with the new active path + slider values, and inbound
    // rumors from the counterparty (or another of our devices) do too.
    _on('wallpaper_updated', (evt) => {
        const { chat_id, path, ts, blur, dim } = evt.payload || {};
        if (!chat_id) return;
        const cChat = getChat(chat_id);
        if (cChat) {
            cChat.wallpaper_path = path || '';
            cChat.wallpaper_ts = ts || 0;
            if (typeof blur === 'number') cChat.wallpaper_blur = blur;
            if (typeof dim === 'number') cChat.wallpaper_dim = dim;
        }
        if (strOpenChat === chat_id) {
            // Ignore if a preview is currently up for this chat — the user
            // is mid-decision and we already swapped the layer to the
            // staged file. The next openChat will hydrate from chat.* fields.
            if (wallpaperPreviewState && wallpaperPreviewState.chatId === chat_id) return;
            applyChatWallpaper(chat_id, path || '', blur, dim, ts);
        }
    });

    _on('attachment_update', (evt) => {
        const { chat_id, message_id, attachment_id, url } = evt.payload;
        const cChat = getChat(chat_id);
        if (!cChat) return;

        // Find the message
        const msg = cChat.messages.find(m => m.id === message_id);
        if (!msg || !msg.attachments) return;

        // Find and update the attachment
        const att = msg.attachments.find(a => a.id === attachment_id);
        if (att) {
            att.url = url;
            // Re-render if this chat is open
            if (strOpenChat === chat_id) {
                const domMsg = document.getElementById(message_id);
                if (domMsg) {
                    const profile = getProfile(chat_id);
                    updateMessageRow(domMsg, msg, profile, message_id);
                }
            }
        }
    });

    // Listen for Vector Voice AI (Whisper) model download progression updates
    _on('whisper_download_progress', (evt) => {
        const { progress, downloaded_bytes, total_bytes } = evt.payload;
        VectorSvelte.setVoiceDownloadProgress(downloaded_bytes && total_bytes
            ? `${(downloaded_bytes / (1024 * 1024)).toFixed(1)}/${(total_bytes / (1024 * 1024)).toFixed(1)} MB`
            : `${Math.round(progress)}%`);
    });

    // Listen for Windows-specific Overlay Icon update requests
    // Note: this API seems unavailable in Tauri's Rust backend, so we're using the JS API as a workaround
    _on('update_overlay_icon', async (evt) => {
        // Enable or Disable our notification badge Overlay Icon
        await getCurrentWindow().setOverlayIcon(evt.payload.enable ? "./icons/icon_badge_notification.png" : undefined);
    });

    _on('blossom_servers_updated', () => {
        if (typeof renderRelayList === 'function') renderRelayList();
    });

    _on('blossom_capabilities_updated', () => {
        if (currentBlossomInfo) {
            renderBlossomCapabilities(currentBlossomInfo.url, ++_blossomCapsToken);
        }
    });

    // Listen for relay status changes
    _on('relay_status_change', (evt) => {
        // Update the relay status in the network list
        const relayItem = document.querySelector(`[data-relay-url="${evt.payload.url}"]`);
        if (relayItem) {
            const statusElement = relayItem.querySelector('.relay-status');
            if (statusElement) {
                // Remove all status classes
                statusElement.classList.remove('connected', 'connecting', 'disconnected', 'pending', 'initialized', 'terminated', 'banned', 'sleeping');
                // Add the new status class
                statusElement.classList.add(evt.payload.status);
                // Update the text
                statusElement.textContent = evt.payload.status;
            }
        }

        // Also update the info dialog if it's open for this relay
        if (currentRelayInfo && currentRelayInfo.url.toLowerCase() === evt.payload.url.toLowerCase()) {
            VectorSvelte.relayInfoDialog.patch({ status: evt.payload.status });
            currentRelayInfo.status = evt.payload.status;
        }
    });

    // Listen for Mini App realtime status updates (peer count changes)
    _on('miniapp_realtime_status', (evt) => {
        const { topic, peer_count, is_active, has_pending_peers, peers } = evt.payload;
        console.log('[MINIAPP] Realtime status update:', topic, 'peers:', peer_count, 'active:', is_active, 'npubs:', peers);

        VectorSvelte.setMiniappStatus(topic, { active: is_active, peerCount: peer_count, peers });
    });

    // Listen for Mini App crashes (Android renderer process crash)
    _on('miniapp_crashed', () => {
        showToast('Mini App Crashed Unexpectedly');
    });

    // NIP-46 bunker lifecycle. `bunker_state` fires on every connection
    // transition (idle → connecting → online → offline). We surface the
    // Offline case as a toast since signing will fail until reconnect.
    // The Connecting/Online transitions stay silent — they're noise on
    // every relay reconnect.
    // Bunker session listeners (bunker_state, bunker_session_staged,
    // bunker_reauthorize_*, bunker_awaiting_approval, bunker_auth_url) are
    // registered EARLY in the DOMContentLoaded init block — not here — so
    // they catch events fired during the pre-login bunker / reauth flows
    // before setupRustListeners has run.

    await Promise.all(_p);

    // Note: Deep link listener is set up early in DOMContentLoaded, before login flow
    // This ensures deep links work even when the app is opened from a closed state
}
