// The composer: attachments, paste, the send pipeline, keyboard, the mention, shortcode
// and command selectors, and the voice recorder. The chrome, popups and command strip are
// islands over lib/composer.svelte.js; this side is the orchestration those islands drive.
// One global scope: loads after main.js and shares its globals.

/** The add-file button and the attachment panel's file and folder pickers. */
function initComposerAttachments() {
    const isAndroid = platformFeatures.os === 'android';

    if (isAndroid) {
        // Toggle attachment panel when clicking the add-file button
        domChatMessageInputFile.onclick = () => {
            toggleAttachmentPanel();
        };

        // Handle File button in attachment panel (Android). Use the native
        // picker (dialog.open) -> content URI -> openFilePreview, which reads via
        // ContentResolver. The WebView <input type=file> hands back a File whose
        // arrayBuffer() can't read documents-provider content URIs.
        domAttachmentPanelFile.onclick = async () => {
            closeAttachmentPanel();
            const filepath = await selectFile();
            if (filepath) {
                const strReplyRef = strCurrentReplyReference;
                cancelReply();
                await openFilePreview(filepath, strOpenChat, strReplyRef);
            }
        };
    } else {
        // Toggle attachment panel when clicking the add-file button
        domChatMessageInputFile.onclick = () => {
            toggleAttachmentPanel();
        };

        // Handle File button in attachment panel (Desktop - use Tauri dialog)
        domAttachmentPanelFile.onclick = async () => {
            closeAttachmentPanel();
            let filepath = await selectFile();
            if (filepath) {
                // Reset reply selection while passing a copy of the reference to the backend
                const strReplyRef = strCurrentReplyReference;
                cancelReply();
                // Show file preview instead of sending directly
                await openFilePreview(filepath, strOpenChat, strReplyRef);
            }
        };

        // Show Folder button on desktop only
        if (domAttachmentPanelFolder) {
            domAttachmentPanelFolder.style.display = '';
            domAttachmentPanelFolder.onclick = async () => {
                closeAttachmentPanel();
                let folderPath = await selectFolder();
                if (folderPath) {
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    await openFolderZipPreview(folderPath, strOpenChat, strReplyRef);
                }
            };
        }
    }

}

/** A paste into the app: a native file, an in-band blob, or a clipboard bitmap, else plain text. */
async function handleComposerPaste(evt) {
        if (strOpenChat) {
            const dt = evt.clipboardData;
            // clipboardData is only valid during synchronous dispatch — capture any
            // file item + its blob NOW, since the await below would invalidate it.
            // ANY file kind, not just images: when the native path read fails (a
            // clipboard held by another process, a manager that dropped CF_HDROP),
            // the in-band blob is the universal fallback and carries a real name.
            const arrItems = Array.from(dt?.items || []);
            const fileItem = arrItems.find(item => item.kind === 'file')
                || arrItems.find(item => item.type.startsWith('image/'));
            const fileBlob = fileItem ? fileItem.getAsFile() : null;
            const fileMime = fileItem ? fileItem.type : '';
            const strPlain = dt ? (dt.getData('text/plain') || '') : '';

            // A file copy (Finder/Explorer) also carries a text representation of the
            // path, so the default paste inserts the filename into the input. We must
            // preventDefault SYNCHRONOUSLY (before any await) to stop that — a late
            // call is ignored. Detect a file from the synchronous clipboard signals.
            const dtTypes = Array.from(dt?.types || []);
            const hasFile = (dt?.files && dt.files.length > 0)
                || arrItems.some(it => it.kind === 'file')
                || dtTypes.includes('Files')
                // WebKitGTK never advertises `Files`; a copied file arrives as a URI list.
                || dtTypes.includes('text/uri-list');
            if (hasFile || fileBlob) evt.preventDefault();

            // Snapshot the composer so we can scrub any filename text that still
            // slipped in (e.g. a folder copy whose sync signal we couldn't read).
            const inputBefore = domChatMessageInput ? domChatMessageInput.value : null;
            const restoreInput = () => {
                if (domChatMessageInput && inputBefore !== null && domChatMessageInput.value !== inputBefore) {
                    domChatMessageInput.value = inputBefore;
                    if (typeof autoResizeChatInput === 'function') autoResizeChatInput();
                }
            };

            // Native file paste: Finder/Explorer "Copy file" puts file references on
            // the OS clipboard that the WebView never exposes to JS, so ask the
            // backend. A real file routes through the same path as a drag-drop
            // (preview → send). Falls through to the image-bytes path below when the
            // clipboard holds raw image data (e.g. a screenshot).
            try {
                const filePaths = await invoke('read_clipboard_files');
                if (Array.isArray(filePaths) && filePaths.length) {
                    restoreInput();
                    const droppedPath = filePaths[0]; // mirror drag-drop: first item
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    const isDir = await invoke('is_directory', { path: droppedPath }).catch(() => false);
                    if (isDir) {
                        await openFolderZipPreview(droppedPath, strOpenChat, strReplyRef);
                    } else {
                        await openFilePreview(droppedPath, strOpenChat, strReplyRef);
                    }
                    return;
                }
            } catch (e) {
                console.warn('[paste] native file read failed, falling back to image bytes:', e);
            }

            // Fall back to the in-band blob: screenshot bytes, or a copied file
            // whose native path read failed (its content still rides the event).
            if (fileBlob) {
                restoreInput();

                // Read the blob as bytes
                const arrayBuffer = await fileBlob.arrayBuffer();
                const bytes = new Uint8Array(arrayBuffer);

                // Prefer the blob's real filename; synthesize one from the MIME
                // type only for anonymous data (screenshots).
                let ext = 'png'; // Default
                if (fileMime.includes('jpeg') || fileMime.includes('jpg')) {
                    ext = 'jpg';
                } else if (fileMime.includes('gif')) {
                    ext = 'gif';
                } else if (fileMime.includes('webp')) {
                    ext = 'webp';
                } else if (fileMime.includes('png')) {
                    ext = 'png';
                } else if (fileMime.includes('tiff')) {
                    ext = 'tiff';
                } else if (fileMime.includes('bmp')) {
                    ext = 'bmp';
                } else if (!fileMime.startsWith('image/')) {
                    ext = 'bin';
                }
                let fileName = `pasted_image.${ext}`;
                if (fileBlob.name && fileBlob.name.includes('.')) {
                    fileName = fileBlob.name;
                    ext = fileBlob.name.split('.').pop().toLowerCase();
                } else if (fileBlob.name) {
                    fileName = fileBlob.name;
                }

                // Get reply reference before opening preview
                const strReplyRef = strCurrentReplyReference;

                // Cancel the reply UI (the reference is passed to the preview)
                cancelReply();

                // Open the file preview dialog with the pasted image bytes
                openFilePreviewWithBytes(bytes, fileName, ext, bytes.length, strOpenChat, strReplyRef);
                return;
            }

            // WebKitGTK hands JS an empty `text/uri-list` and an `<img>` tag for a
            // copied bitmap — the pixels sit on the GTK clipboard the webview never
            // exposes. Only reached when the paste carried no text and no in-band
            // file, so a plain-text paste never pays for it.
            if (!strPlain) {
                const clipBytes = await readClipboardImageBytes();
                if (clipBytes) {
                    restoreInput();
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    openFilePreviewWithBytes(clipBytes, 'pasted_image.png', 'png', clipBytes.length, strOpenChat, strReplyRef);
                }
            }
        }
}

/**
 * Read a bitmap off the OS clipboard and encode it as PNG bytes.
 * Returns null whenever the clipboard holds no image — the common case, not an error.
 */
async function readClipboardImageBytes() {
    try {
        const cm = window.__TAURI__?.clipboardManager;
        if (!cm?.readImage) return null;
        const img = await cm.readImage();
        const { width, height } = await img.size();
        if (!width || !height) return null;
        const rgba = await img.rgba();
        const canvas = document.createElement('canvas');
        canvas.width = width;
        canvas.height = height;
        canvas.getContext('2d').putImageData(
            new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0
        );
        const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
        if (!blob) return null;
        return new Uint8Array(await blob.arrayBuffer());
    } catch (e) {
        return null;
    }
}

// Unified message sending function
async function sendMessage(messageText) {
    if (!messageText || !messageText.trim()) return;

    // Clean tracking parameters from any URLs in the message for privacy (if enabled)
    let cleanedText = messageText.trim();
    if (fStripTrackingEnabled) {
        const urlPattern = /(https?:\/\/[^\s<>"{}|\\^`\[\]]+)/gi;
        cleanedText = cleanedText.replace(urlPattern, (match) => {
            try {
                return cleanTrackingFromUrl(match);
            } catch (e) {
                // If cleaning fails, return original URL
                return match;
            }
        });
    }

    // Replace @DisplayName with @npub1... for any tracked mentions
    if (mentionCtrl) {
        const tracked = mentionCtrl.getMentions();
        // Sort by name length descending so longer names are replaced first,
        // preventing partial matches (e.g. "Al" matching inside "Alice")
        const sorted = tracked.slice().sort((a, b) => b.name.length - a.name.length);
        for (const m of sorted) {
            // Match @Name only at word boundaries to avoid substring collisions,
            // and under any typographic variant, so a name the OS re-punctuated
            // still tags rather than sending as plain text.
            const re = new RegExp('(?<=^|\\s)@' + cmpNamePattern(m.name) + '(?=\\s|[.,!?;:]|$)', 'g');
            cleanedText = cleanedText.replace(re, '@' + m.npub);
        }
    }

    // Check if we're in edit mode
    if (strCurrentEditMessageId) {
        // Don't send if content hasn't changed
        if (cleanedText === strCurrentEditOriginalContent) {
            cancelEdit();
            return;
        }

        // Clear input and show editing state
        domChatMessageInput.value = '';
        resetSendMicButtons(); // Immediately reset to mic button (avoids animation race)
        resetChatInputSize();
        VectorSvelte.setComposerStatus('Saving edit...');

        try {
            const editMsgId = strCurrentEditMessageId;
            const originalContent = strCurrentEditOriginalContent;
            cancelEdit();

            // Update the in-memory message first; the row re-derives from it below.
            const chat = arrChats.find(c => c.id === strOpenChat);
            if (chat) {
                const msg = chat.messages.find(m => m.id === editMsgId);
                if (msg) {
                    // Build edit history if it doesn't exist yet
                    if (!msg.edit_history) {
                        msg.edit_history = [];
                        // Add original content as first entry
                        msg.edit_history.push({
                            content: originalContent,
                            edited_at: msg.created_at * 1000 // Convert to milliseconds
                        });
                    }
                    // Add new edit entry
                    msg.edit_history.push({
                        content: cleanedText,
                        edited_at: Date.now()
                    });
                    msg.content = cleanedText;
                    msg.edited = true;
                    // The optimistic row needs the tags the edit will carry: the equipped
                    // packs' shortcodes present in the new text, ahead of the backend's copy.
                    msg.emoji_tags = mergeEmojiTags(msg.emoji_tags, equippedEmojiTags())
                        .filter(t => cleanedText.includes(`:${t.shortcode}:`));
                    // Instant repaint for responsive UX; the backend's authoritative
                    // message_update lands on the same row afterwards.
                    const msgElement = document.getElementById(editMsgId);
                    if (msgElement) updateMessageRow(msgElement, msg, getProfile(strOpenChat), editMsgId);
                }
            }

            // Send edit to backend (fire and forget for responsiveness). Community
            // channels use their own envelope path (the edit rides a kind-3302 event).
            const editChatForRoute = arrChats.find(c => c.id === strOpenChat);
            const editPromise = editChatForRoute?.chat_type === 'Community'
                ? invoke('edit_community_message', { channelId: strOpenChat, messageId: editMsgId, newContent: cleanedText })
                : invoke('edit_message', { messageId: editMsgId, chatId: strOpenChat, newContent: cleanedText });
            editPromise
                .catch(e => { console.error('Failed to edit message:', e); })
                .finally(() => VectorSvelte.setComposerStatus(''));

            nLastTypingIndicator = 0;
        } catch(e) {
            console.error('Failed to edit message:', e);
            VectorSvelte.setComposerStatus('');
        }
        return;
    }

    // Slash command routing: a KNOWN bot command with bad arguments blocks the
    // send (draft preserved, error shown) — sending it would just post a broken
    // invocation the bot ignores. Valid commands carry their bot's routing tag;
    // unknown "/words" stay ordinary chat.
    let commandBot = null;
    if (commandCtrl) {
        const route = commandCtrl.routeForSend(cleanedText);
        if (route && route.error) {
            showToast(route.error);
            return;
        }
        if (route) commandBot = route.bot || null;
    }

    // Clear input and show sending state
    domChatMessageInput.value = '';
    resetSendMicButtons(); // Immediately reset to mic button (avoids animation race)
    resetChatInputSize();
    VectorSvelte.setComposerStatus('Sending...');

    try {
        const replyRef = strCurrentReplyReference;
        cancelReply();

        // Record this message's distinct emojis (stock + custom) for frecency,
        // in one batched IPC — captures typed/pasted/picked uniformly.
        bumpEmojiUsageBatch(extractMessageEmojis(cleanedText));

        // Send message (unified function handles both DMs and MLS groups)
        await message(strOpenChat, cleanedText, replyRef, commandBot);

        nLastTypingIndicator = 0;
        if (mentionCtrl) mentionCtrl.clearMentions();
    } catch(e) {
        console.error('Failed to send message:', e);
    } finally {
        VectorSvelte.setComposerStatus('');
    }
}

/** Enter sends, Escape leaves reply or edit mode; a selector that is open owns the key. */
async function handleComposerKeydown(evt) {
        // Skip send if mention/emoji/command selector is consuming this keypress
        if (mentionCtrl && mentionCtrl.isOpen && mentionCtrl.isOpen()) return;
        if (emojiShortcodeCtrl && emojiShortcodeCtrl.isOpen && emojiShortcodeCtrl.isOpen()) return;
        if (commandCtrl && commandCtrl.isOpen && commandCtrl.isOpen()) return;
        if ((evt.key === 'Enter' || evt.keyCode === 13) && !evt.shiftKey) {
            evt.preventDefault();
            await sendMessage(domChatMessageInput.value);
        }
        // ESC key cancels reply/edit mode
        if (evt.key === 'Escape') {
            if (strCurrentEditMessageId) {
                cancelEdit();
            } else if (strCurrentReplyReference) {
                cancelReply();
            }
        }
}

// --- Mention Selector ---
// Shared by the @mention selector AND the command composer's User params —
// one source for "who is taggable in the open chat".
/** The taggable pool for the open chat.
 *
 *  `includeSelf` because the two callers want different pools: an @mention of
 *  yourself pings nobody, while a command's user parameter is often ABOUT you —
 *  `/why` and `/pardon` on your own npub were unreachable from the UI. */
function getMentionCandidates(includeSelf = false) {
        const chat = arrChats.find(c => c.id === strOpenChat);
        if (!chat) return [];
        const isCommunity = chat.chat_type === 'Community';
        // Build a map of each participant's most recent message timestamp
        const lastActive = {};
        if (chat.messages) {
            for (let i = chat.messages.length - 1; i >= 0; i--) {
                const m = chat.messages[i];
                // A join/leave line names its member — a fresh joiner who has
                // not spoken yet is taggable the moment their line lands, not
                // only after the throttled roster refresh notices them.
                const sender = m.npub || m.system_event?.member_npub || (m.mine ? strPubkey : chat.id);
                if (!lastActive[sender]) lastActive[sender] = m.at || 0;
            }
        }
        // Taggable npubs = explicit participants ∪ (for communities) the roster ∪ observed senders.
        // The roster (cached from get_community_members) covers join-presence-only members the
        // Member List already shows; observed senders cover anyone the roster fetch hasn't
        // caught up with yet (it refreshes throttled while the chat is open).
        const npubs = new Set(chat.participants || []);
        // Not always a participant of the chat it is in, and never an observed
        // sender in one nobody has spoken in yet.
        if (includeSelf && strPubkey) npubs.add(strPubkey);
        if (isCommunity) {
            for (const np of Object.keys(lastActive)) npubs.add(np);
            const communityId = chat.metadata?.custom_fields?.community_id;
            for (const m of communityMembersCache.get(communityId) || []) {
                npubs.add(m.npub);
                // Roster last_active is SECONDS; message timestamps are ms. Only a
                // fallback — a real message timestamp wins the recency sort.
                if (!lastActive[m.npub]) lastActive[m.npub] = (m.last_active || 0) * 1000;
            }
        }
        const candidates = [...npubs]
            .filter(npub => npub && (includeSelf || npub !== strPubkey) && npub.startsWith('npub1'))
            .map(npub => {
                const p = getProfile(npub);
                return {
                    npub,
                    // Marked, because a moderator picking a subject from a list
                    // of names should not have to recognise their own.
                    name: npub === strPubkey ? getName(npub) + ' (you)' : getName(npub),
                    avatarSrc: p ? getProfileAvatarSrc(p) : null,
                    lastActive: lastActive[npub] || 0
                };
            })
            .sort((a, b) => b.lastActive - a.lastActive);
        // Disambiguate duplicate display names with a short npub suffix
        const nameCount = {};
        for (const c of candidates) nameCount[c.name] = (nameCount[c.name] || 0) + 1;
        for (const c of candidates) {
            if (nameCount[c.name] > 1) {
                c.name = c.name + ' (~' + c.npub.slice(5, 9) + ')';
            }
        }
        // @everyone: lowest-priority option (bottom of the list), placeholder avatar — the original
        // group design. Offered only to those who can actually use it (owner or admin); a non-admin's
        // @everyone is ignored, so suggesting it would mislead. Roles preload at boot, so this gate is
        // reliable now (the earlier always-show was a stopgap for when admins weren't loaded yet).
        if (isCommunity) {
            const cf = chat.metadata?.custom_fields || {};
            const canPingEveryone = cf.is_owner === 'true' || (chat.metadata?.admins || []).includes(strPubkey);
            if (canPingEveryone) {
                candidates.push({ npub: 'everyone', name: 'everyone', avatarSrc: null, lastActive: -1 });
            }
        }
        return candidates;
}

let mentionCtrl = null;
let emojiShortcodeCtrl = null;
let recorder = null;

/** Build the selectors over the editor and hand the composer its mention source. */
function initComposerControllers() {
    mentionCtrl = typeof initMentionSelector === 'function' ? initMentionSelector(
        domChatMessageInput,
        getMentionCandidates
    ) : null;
    // Hand the composer its mention source now that one exists, so an inserted
    // `@Name` renders as a pill.
    composerMentionLookup = () => (mentionCtrl && mentionCtrl.getMentions ? mentionCtrl.getMentions() : []);
    emojiShortcodeCtrl = typeof initEmojiShortcodeSelector === 'function'
        ? initEmojiShortcodeSelector(domChatMessageInput)
        : null;
}

/**
 * Re-render the open chat's untagged `/cmd args` rows after its bot-command
 * manifest finishes loading. The manifest is fetched asynchronously on chat
 * open, usually after the timeline has already painted, so a DM invocation
 * first renders as plain text; once the command set is known it can flip to its
 * action line. Only untagged rows with arguments can change verdict — a bare
 * `/cmd` and a tagged invocation already render correctly without the manifest.
 */
function _upgradeCommandRows(chatId) {
    if (chatId !== strOpenChat) return;
    // No bot commands in this chat → no untagged row can become an action line.
    const known = commandCtrl && commandCtrl.commandNames(strOpenChat);
    if (!known || !known.size) return;
    const chat = arrChats.find(c => c.id === strOpenChat);
    if (!chat?.messages) return;
    const profile = getProfile(strOpenChat);
    for (const msg of chat.messages) {
        if (msg.addressed_bots && msg.addressed_bots.length) continue;
        if (!/^\s*\/[a-z0-9_-]{1,32}\s+\S/.test(msg.content || '')) continue;
        const domMsg = document.getElementById(msg.id);
        if (!domMsg) continue;
        // Already an action line: re-rendering it produces the same row, and
        // `replaceWith` above the viewport costs the reader their scroll
        // position for nothing. This runs on every command-set load, so
        // without the check a row churns every time.
        if (domMsg.querySelector('.dmsg-command-line')) continue;
        updateMessageRow(domMsg, msg, profile, msg.id);
    }
}

/** The slash-command selector over the open chat's bot manifests. */
function initCommandController() {
    commandCtrl = typeof initCommandSelector === 'function' ? initCommandSelector(
        domChatMessageInput,
        {
            load: (chatId) => invoke('get_chat_commands', { chatId }),
            chatId: () => strOpenChat,
            accountNpub: () => strPubkey,
            botProfile: (npub) => {
                const p = getProfile(npub);
                return { name: getName(npub), avatarSrc: p ? getProfileAvatarSrc(p) : null };
            },
            // User params: the same taggable-member pool the @mention selector
            // uses ('everyone' excluded — a User arg is one real npub).
            // Self included: `/why` and `/pardon` are most often asked about the
            // person asking. `@everyone` is dropped by the npub filter, which is
            // right — it is a ping, not somebody a command can name.
            mentionCandidates: () => getMentionCandidates(true).filter(c => c.npub.startsWith('npub1')),
            // The structured composer assembles the final "/cmd args" text and
            // hands it to the ordinary send pipeline (validation + bot tag ride
            // routeForSend inside sendMessage).
            submit: (text) => sendMessage(text),
            composerToggled: (active) => {
                // The structured composer submits through the send button, so it stays shown.
                VectorSvelte.setDraftEmpty(!active, false);
                VectorSvelte.flushSync();
                // BOTH directions. The command composer grows the input area as it
                // slides in and shrinks it as it slides out, and a bottom-pinned
                // reader has to stay glued to the live tail through either — the
                // reply bar does the same. Only the growth was followed, so sending
                // a command left the view hanging above the bottom afterwards.
                followPinThroughComposerResize();
            },
            // The command manifest loads async, often after the timeline painted;
            // upgrade any untagged `/cmd args` rows once it is known (DM invocations).
            commandsReady: (chatId) => _upgradeCommandRows(chatId)
        }
    ) : null;
}

/** Glue a bottom-pinned reader to the live tail across a composer resize.
 *
 *  Frame by frame rather than once: the input area animates, so a single
 *  scroll-to-bottom lands on the height it had at that instant and the rest of
 *  the transition slides out from under the reader. */
function followPinThroughComposerResize() {
    if (!chatPinnedToBottom || (CHAT_WINDOW_ENABLED && !isAtDataBottom())) return;
    const start = performance.now();
    const followPin = () => {
        beginProgrammaticScroll();
        domChatMessages.scrollTop = domChatMessages.scrollHeight;
        if (performance.now() - start < 280) requestAnimationFrame(followPin);
    };
    requestAnimationFrame(followPin);
}

/**
 * Immediately reset send/mic buttons to mic state (no animation)
 * Used after sending messages to avoid animation race conditions
 */
function resetSendMicButtons() {
    VectorSvelte.setDraftEmpty(true, false);
    VectorSvelte.flushSync();
}

/** Every draft change: resize, the mic/send swap, and a throttled typing indicator. */
async function handleComposerInput(e) {
        // Auto-resize the textarea based on content
        autoResizeChatInput();

        // Mic ↔ send follows the draft; a typed change animates the swap.
        VectorSvelte.setDraftEmpty(domChatMessageInput.value.trim().length === 0, true);

        // Send a Typing Indicator only when content actually changes and setting is enabled.
        // Don't send while editing (not a new message), while the draft is a `/` command
        // (an instruction to a bot, not conversation), or on a DELETION — removing text,
        // including backspacing a leading `/`, isn't composing and must not slip past the
        // slash exclusion.
        const isDeletion = e?.inputType?.startsWith('delete');
        if (fSendTypingIndicators && !strCurrentEditMessageId && !isDeletion
            && !domChatMessageInput.value.startsWith('/')
            && nLastTypingIndicator + 30000 < Date.now()) {
            nLastTypingIndicator = Date.now();
            await invoke("start_typing", { receiver: strOpenChat });
        }
}

/** The send button: a structured command, a voice preview, or the text draft. */
async function handleSendClick() {
        // Structured command composer open: the button submits the parts.
        if (commandCtrl && commandCtrl.isComposing()) {
            commandCtrl.submitComposer();
            return;
        }
        // Check if we're in voice preview mode first
        if (recorder.isInPreview) {
            const sent = recorder.send();
            if (sent && strOpenChat) {
                VectorSvelte.setComposerStatus('Sending...');
                try {
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    await invoke('send_recording', {
                        receiver: strOpenChat,
                        repliedTo: strReplyRef
                    });
                } catch (e) {
                    if (!e || !e.toString().includes('Upload cancelled')) {
                        popupConfirm(e, '', true, '', 'vector_warning.svg');
                    }
                }
                VectorSvelte.setComposerStatus('');
                nLastTypingIndicator = 0;
            }
            return;
        }

        // Otherwise, handle normal text message send
        const messageText = domChatMessageInput.value;
        if (messageText && messageText.trim()) {
            await sendMessage(messageText);
        }
}

/** The voice recorder and the transcription models. */
async function initComposerVoice() {
    // Hook up our voice message recorder with Telegram-like UX
    recorder = new VoiceRecorder(domChatMessageInputVoice, domChatInputContainer);

    // Handle state changes for UI updates
    recorder.onStateChange = (newState, oldState) => {
        if (newState === 'idle') {
            // Reset placeholder when returning to idle
            VectorSvelte.setComposerStatus('');
        } else if (newState === 'recording' || newState === 'locked') {
            // Clear input and show recording status
            domChatMessageInput.value = '';
            resetChatInputSize();
        }
    };

    // Handle cancel callback
    recorder.onCancel = () => {
        VectorSvelte.setComposerStatus('');
        cancelReply();
    };

    // Initialize voice transcription with default model
    window.cTranscriber = new VoiceTranscriptionUI();
    window.voiceSettings = new VoiceSettings();

    // Only load whisper models if transcription is supported
    if (platformFeatures.transcription) {
        await window.voiceSettings.loadWhisperModels();
    }

    window.voiceSettings.initVoiceSettings();
}

/** Wire the composer once the DOM is up. */
async function initComposer() {
    initComposerAttachments();
    document.onpaste = handleComposerPaste;
    // Android composes through its own keyboard actions.
    if (platformFeatures.os !== 'android') domChatMessageInput.addEventListener('keydown', handleComposerKeydown);
    initComposerControllers();
    initCommandController();
    domChatMessageInput.oninput = handleComposerInput;
    domChatMessageInputSend.onclick = handleSendClick;
    await initComposerVoice();
}
