const { invoke, convertFileSrc } = window.__TAURI__.core;
const { getCurrentWebview } = window.__TAURI__.webview;
const { getCurrentWindow } = window.__TAURI__.window;
const { listen } = window.__TAURI__.event;
const { openUrl, revealItemInDir } = window.__TAURI__.opener;

const domTheme = document.getElementById('theme');

const domLoginStart = document.getElementById('login-start');
const domLoginAccountCreationBtn = document.getElementById('start-account-creation-btn');
const domLoginAccountBtn = document.getElementById('start-login-btn');
const domLogin = document.getElementById('login-form');
const domLoginImport = document.getElementById('login-import');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');

const domLoginInvite = document.getElementById('login-invite');
const domInviteInput = document.getElementById('invite-input');
const domInviteBtn = document.getElementById('invite-btn');

const domLoginWelcome = document.getElementById('login-welcome');

const domLoginEncrypt = document.getElementById('login-encrypt');
const domLoginEncryptTitle = document.getElementById('login-encrypt-title');
const domLoginEncryptPinRow = document.getElementById('login-encrypt-pins');

const domProfile = document.getElementById('profile');
const domProfileBackBtn = document.getElementById('profile-back-btn');
const domProfileName = document.getElementById('profile-name');
const domProfileStatus = document.getElementById('profile-status');
// Note: these are 'let' due to needing to use `.replaceWith` when hot-swapping profile elements
let domProfileBanner = document.getElementById('profile-banner');
let domProfileAvatar = document.getElementById('profile-avatar');
const domProfileNameSecondary = document.getElementById('profile-secondary-name');
const domProfileStatusSecondary = document.getElementById('profile-secondary-status');
const domProfileBadgeInvite = document.getElementById('profile-badge-invites');
const domProfileDescription = document.getElementById('profile-description');
const domProfileDescriptionEditor = document.getElementById('profile-description-editor');
const domProfileOptions = document.getElementById('profile-option-list');
const domProfileOptionMute = document.getElementById('profile-option-mute');
const domProfileOptionNickname = document.getElementById('profile-option-nickname');
const domProfileId = document.getElementById('profile-id');

const domChats = document.getElementById('chats');
const domChatBookmarksBtn = document.getElementById('chat-bookmarks-btn');
const domAccount = document.getElementById('account');
const domSyncStatusContainer = document.getElementById('sync-status-container');
const domSyncStatus = document.getElementById('sync-status');
const domChatList = document.getElementById('chat-list');
const domChatNewDM = document.getElementById('new-chat-btn');
const domChatNewGroup = document.getElementById('create-group-btn');
const domNavbar = document.getElementById('navbar');
const domInvites = document.getElementById('invites');
const domInvitesBtn = document.getElementById('invites-btn');
const domProfileBtn = document.getElementById('profile-btn');
const domChatlistBtn = document.getElementById('chat-btn');
const domSettingsBtn = document.getElementById('settings-btn');

const domChat = document.getElementById('chat');
const domChatBackBtn = document.getElementById('chat-back-btn');
const domChatContact = document.getElementById('chat-contact');
const domChatContactStatus = document.getElementById('chat-contact-status');
const domChatMessagesFade = document.getElementById('msg-top-fade');
const domChatMessages = document.getElementById('chat-messages');
const domChatMessageBox = document.getElementById('chat-box');
const domChatMessagesScrollReturnBtn = document.getElementById('chat-scroll-return');
const domChatMessageInput = document.getElementById('chat-input');
const domChatMessageInputFile = document.getElementById('chat-input-file');
const domChatMessageInputCancel = document.getElementById('chat-input-cancel');
const domChatMessageInputEmoji = document.getElementById('chat-input-emoji');
const domChatMessageInputVoice = document.getElementById('chat-input-voice');
const domChatMessageInputSend = document.getElementById('chat-input-send');

const domChatNew = document.getElementById('chat-new');
const domChatNewBackBtn = document.getElementById('chat-new-back-text-btn');
const domShareNpub = document.getElementById('share-npub');
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

// Create Group UI refs
const domCreateGroup = document.getElementById('create-group');
const domCreateGroupBackBtn = document.getElementById('create-group-back-text-btn');
const domCreateGroupName = document.getElementById('create-group-name');
const domCreateGroupFilter = document.getElementById('create-group-filter');
const domCreateGroupList = document.getElementById('create-group-list');
const domCreateGroupCreateBtn = document.getElementById('create-group-create-btn');
const domCreateGroupCancelBtn = document.getElementById('create-group-cancel-btn');
const domCreateGroupStatus = document.getElementById('create-group-status');
const domSettings = document.getElementById('settings');
const domSettingsThemeSelect = document.getElementById('theme-select');
const domSettingsWhisperModelInfo = document.getElementById('whisper-model-info');
const domSettingsWhisperAutoTranslateInfo = document.getElementById('whisper-auto-translate-info');
const domSettingsWhisperAutoTranscribeInfo = document.getElementById('whisper-auto-transcribe-info');
const domSettingsPrivacyWebPreviewsInfo = document.getElementById('privacy-web-previews-info');
const domSettingsPrivacyStripTrackingInfo = document.getElementById('privacy-strip-tracking-info');
const domSettingsLogout = document.getElementById('logout-btn');
const domSettingsExport = document.getElementById('export-account-btn');

const domApp = document.getElementById('popup-container');
const domPopup = document.getElementById('popup');
const domPopupIcon = document.getElementById('popupIcon');
const domPopupTitle = document.getElementById('popupTitle');
const domPopupSubtext = document.getElementById('popupSubtext');
const domPopupConfirmBtn = document.getElementById('popupConfirm');
const domPopupCancelBtn = document.getElementById('popupCancel');
const domPopupInput = document.getElementById('popupInput');

const picker = document.querySelector('.emoji-picker');
/** @type {HTMLInputElement} */
const emojiSearch = document.getElementById('emoji-search-input');
const emojiResults = document.getElementById('emoji-results');

/**
 * The current reaction reference - i.e: a message being reacted to.
 * 
 * When empty, emojis are simply injected to the current chat input.
 */
let strCurrentReactionReference = "";

/**
 * Opens the Emoji Input Panel
 * 
 * If a DOM element is passed, the panel will be rendered 'floating' near the element.
 * If none is specified, it opens in the default location near the Message Input.
 * @param {MouseEvent?} e - An associated click event
 */
function openEmojiPanel(e) {
    const isDefaultPanel = e.target === domChatMessageInputEmoji || domChatMessageInputEmoji.contains(e.target);

    // Open or Close the panel depending on it's state
    const strReaction = e.target.classList.contains('add-reaction') ? e.target.parentElement.parentElement.id : '';
    const fClickedInputOrReaction = isDefaultPanel || strReaction;
    if (fClickedInputOrReaction && picker.style.display !== `block`) {
        // Render our most used emojis by default
        let nDisplayedEmojis = 0;
        emojiResults.innerHTML = ``;
        for (const cEmoji of getMostUsedEmojis()) {
            // Only display 8
            if (nDisplayedEmojis >= 8) break;
            // Push it in to the results
            const spanEmoji = document.createElement('span');
            spanEmoji.textContent = cEmoji.emoji;
            emojiResults.appendChild(spanEmoji);
            nDisplayedEmojis++;
        }

        // Twemojify!
        twemojify(emojiResults);

        // Setup the picker UI
        /** @type {DOMRect} */
        const rect = (isDefaultPanel ? domChatContact : e.target).getBoundingClientRect();

        // Display the picker
        picker.style.display = `block`;

        // Compute its position based on the element calling it
        const pickerRect = picker.getBoundingClientRect();
        if (isDefaultPanel) {
            // Note: No idea why the 5px extra height is needed, but this prevents the picker from overlapping too much with the chat box
            picker.style.top = `${document.body.clientHeight - pickerRect.height - rect.height - 5}px`
            picker.classList.add('emoji-picker-message-type');
            // Set it to the right side always for the default panel
            picker.style.right = `0px`;
            // Change the emoji button to a wink while the panel is open (removed on close)
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-wink-face"></span>`;
        } else {
            picker.classList.remove('emoji-picker-message-type');
            const fLargeMessage = rect.y < rect.height;
            
            // Calculate the vertical position of the picker
            const yAxisTarget = fLargeMessage ? rect.y : rect.y - rect.height;
            const yAxisCorrection = fLargeMessage ? 0 : pickerRect.height / 2;
            
            // Calculate if the picker would overflow the bottom of the app window
            const pickerBottomPos = yAxisTarget + yAxisCorrection + pickerRect.height;
            const appBottomBoundary = document.body.clientHeight;
            const willOverflowBottom = pickerBottomPos > appBottomBoundary;
            
            // Set vertical position - if it will overflow the bottom, position it above the target
            if (willOverflowBottom) {
                picker.style.top = `${rect.y - pickerRect.height}px`;
            } else {
                picker.style.top = `${yAxisTarget + yAxisCorrection}px`;
            }
            
            // Calculate horizontal position
            // Try to position it next to the element that triggered it
            const xPos = rect.x + rect.width;
            const willOverflowRight = xPos + pickerRect.width > document.body.clientWidth;
            
            // If it would overflow the right side, align to right edge
            if (willOverflowRight) {
                picker.style.right = `0px`;
                picker.style.left = ``;
            } else {
                // Position it next to the triggering element
                picker.style.left = `${xPos}px`;
                picker.style.right = ``;
            }
        }

        // If this is a Reaction, let's cache the Reference ID
        if (strReaction) {
            // Message IDs are stored on the parent of the React button
            strCurrentReactionReference = strReaction;
        } else {
            strCurrentReactionReference = '';
        }

        // Focus on the emoji search box for easy searching
        emojiSearch.focus();
    } else {
        // Hide and reset the UI
        emojiSearch.value = '';
        picker.style.display = ``;
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;
    }
}

// Listen for emoji searches
emojiSearch.addEventListener('input', (e) => {
    // Search for the requested emojis and render them, if it's empty, just use our favorites
    let nDisplayedEmojis = 0;
    emojiResults.innerHTML = ``;
    for (const cEmoji of emojiSearch.value ? searchEmojis(emojiSearch.value) : getMostUsedEmojis()) {
        // Only display 8
        if (nDisplayedEmojis >= 8) break;
        // Push it in to the results
        const spanEmoji = document.createElement('span');
        spanEmoji.textContent = cEmoji.emoji;
        // In searches; the first emoji gets a special tag denoting 'Enter' key selection
        if (emojiSearch.value) {
            if (nDisplayedEmojis === 0) {
                spanEmoji.style.opacity = 1;
            } else {
                spanEmoji.style.opacity = 0.75;
            }
        }
        emojiResults.appendChild(spanEmoji);
        nDisplayedEmojis++;
    }

    // Twemojify!
    twemojify(emojiResults);

    // If there's none, sad!
    if (nDisplayedEmojis === 0) {
        emojiResults.textContent = `No emojis found`;
    }
});

// When hitting Enter on the emoji search - choose the first emoji
emojiSearch.onkeydown = async (e) => {
    if ((e.code === 'Enter' || e.code === 'NumpadEnter')) {
        e.preventDefault();

        // Register the selection in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === emojiResults.firstElementChild.firstElementChild.alt);
        cEmoji.used++;

        // If this is a Reaction - let's send it!
        if (strCurrentReactionReference) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Add a 'decoy' reaction for good UX (no waiting for the network to register the reaction)
                const spanReaction = document.createElement('span');
                spanReaction.classList.add('reaction');
                spanReaction.textContent = `${cEmoji.emoji} 1`;
                twemojify(spanReaction);

                // Replace the Reaction button
                // DOM Tree: msg-(them/me) -> msg-extras -> add-reaction
                const divMessage = document.getElementById(cMsg.id);
                divMessage.querySelector(`.msg-extras span`).replaceWith(spanReaction);

                // Send the Reaction to the network (protocol-agnostic)
                invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
            }
        } else {
            // Add it to the message input
            domChatMessageInput.value += cEmoji.emoji;
        }

        // Reset the UI state
        emojiSearch.value = '';
        picker.style.display = ``;
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Bring the focus back to the chat
        domChatMessageInput.focus();
    } else if (e.code === 'Escape') {
        // Close the dialog
        emojiSearch.value = '';
        picker.style.display = ``;
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Bring the focus back to the chat
        domChatMessageInput.focus();
    }
};

// Emoji selection
picker.addEventListener('click', (e) => {
    if (e.target.tagName === 'IMG') {
        // Register the click in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === e.target.alt);
        cEmoji.used++;

        // If this is a Reaction - let's send it!
        if (strCurrentReactionReference) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.messages.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cChat.id;

                // Add a 'decoy' reaction for good UX (no waiting for the network to register the reaction)
                const spanReaction = document.createElement('span');
                spanReaction.classList.add('reaction');
                spanReaction.textContent = `${cEmoji.emoji} 1`;
                twemojify(spanReaction);

                // Replace the Reaction button
                // DOM Tree: msg-(them/me) -> msg-extras -> add-reaction
                const divMessage = document.getElementById(cMsg.id);
                divMessage.querySelector(`.msg-extras span`).replaceWith(spanReaction);

                // Send the Reaction to the network (protocol-agnostic)
                invoke('react_to_message', { referenceId: strCurrentReactionReference, chatId: strReceiverPubkey, emoji: cEmoji.emoji });
            }
        } else {
            // Add it to the message input
            domChatMessageInput.value += cEmoji.emoji;
        }

        // Reset the UI state
        emojiSearch.value = '';
        picker.classList.remove('active');
        strCurrentReactionReference = '';

        // Change the emoji button to the regular face
        domChatMessageInputEmoji.innerHTML = `<span class="icon icon-smile-face"></span>`;

        // Bring the focus back to the chat
        domChatMessageInput.focus();
    }
});

/**
 * Represents a user profile.
 * @typedef {Object} Profile
 * @property {string} id - Unique identifier for the profile.
 * @property {string} name - The name of the user.
 * @property {string} avatar - URL to the user's avatar image.
 * @property {string} last_read - ID of the last message that was read.
 * @property {Status} status - The current status of the user.
 * @property {number} last_updated - Timestamp indicating when the profile was last updated.
 * @property {number} typing_until - Timestamp until which the user is considered typing.
 * @property {boolean} mine - Indicates if this profile belongs to the current user.
 */

/**
 * Represents a message in the system.
 * @typedef {Object} Message
 * @property {string} id - Unique identifier for the message.
 * @property {string} content - The content of the message.
 * @property {string} replied_to - ID of the message this is replying to, if any.
 * @property {Object} preview_metadata - Metadata for link previews, if any.
 * @property {Attachment[]} attachments - Array of file attachments.
 * @property {Reaction[]} reactions - An array of reactions to this message.
 * @property {number} at - Timestamp when the message was sent.
 * @property {boolean} pending - Whether the message is still being sent.
 * @property {boolean} failed - Whether the message failed to send.
 * @property {boolean} mine - Indicates if this message was sent by the current user.
 */

/**
 * Represents a file attachment in a message.
 * @typedef {Object} Attachment
 * @property {string} id - The unique file ID (encryption nonce).
 * @property {string} key - The encryption key.
 * @property {string} nonce - The encryption nonce.
 * @property {string} extension - The file extension.
 * @property {string} url - The host URL, typically a NIP-96 server.
 * @property {string} path - The storage directory path.
 * @property {number} size - The download size of the encrypted file.
 * @property {boolean} downloading - Whether the file is currently being downloaded.
 * @property {boolean} downloaded - Whether the file has been downloaded.
 */

/**
 * Represents metadata for a website preview.
 * @typedef {Object} SiteMetadata
 * @property {string} domain - The domain of the website.
 * @property {string} [og_title] - Open Graph title.
 * @property {string} [og_description] - Open Graph description.
 * @property {string} [og_image] - Open Graph image URL.
 * @property {string} [og_url] - Open Graph URL.
 * @property {string} [og_type] - Open Graph content type.
 * @property {string} [title] - Website title.
 * @property {string} [description] - Website description.
 * @property {string} [favicon] - Website favicon URL.
 */

/**
 * Represents the status of a user.
 * @typedef {Object} Status
 * @property {string} title - The title of the status.
 * @property {string} purpose - Description or purpose of the status.
 * @property {string} url - URL associated with the status, if any.
 */

/**
 * Represents a reaction to a message.
 * @typedef {Object} Reaction
 * @property {string} id - Unique identifier for the reaction.
 * @property {string} reference_id - The HEX Event ID of the message being reacted to.
 * @property {string} author_id - The HEX ID of the author who reacted.
 * @property {string} emoji - The emoji used for the reaction.
 */

/**
 * Represents a chat between users.
 * @typedef {Object} Chat
 * @property {string} id - Chat ID (npub for DMs).
 * @property {string} chat_type - Type of chat (DirectMessage, Group, etc).
 * @property {string[]} participants - Array of participant npubs.
 * @property {Message[]} messages - Array of messages in this chat.
 * @property {string} last_read - ID of the last read message.
 * @property {number} created_at - Timestamp when chat was created.
 * @property {Object} metadata - Additional chat metadata.
 * @property {boolean} muted - Whether the chat is muted.
 */

/**
 * Represents an MLS group invite.
 * @typedef {Object} MLSWelcome
 * @property {string} id - Unique identifier for the welcome/invite.
 * @property {string} group_id - The MLS group ID.
 * @property {string} group_name - Name of the group.
 * @property {string} welcomer_pubkey - Pubkey of the person who invited.
 * @property {number} member_count - Number of members in the group.
 * @property {string} [image] - Optional group avatar image.
 * @property {string} [description] - Optional group description.
 */

/**
 * Represents an MLS message record.
 * @typedef {Object} MLSMessageRecord
 * @property {string} inner_event_id - The inner event ID for deduplication.
 * @property {string} wrapper_event_id - The wrapper event ID.
 * @property {string} author_pubkey - The sender's pubkey.
 * @property {string} content - The message content.
 * @property {number} created_at - Timestamp in seconds.
 * @property {Array<Array<string>>} tags - Nostr tags.
 * @property {boolean} mine - Whether this message was sent by the current user.
 */

/**
 * A cache of all profiles (without messages)
 * @type {Profile[]}
 */
let arrProfiles = [];

/**
 * A cache of all chats (with messages)
 * @type {Chat[]}
 */
let arrChats = [];

/**
 * A cache of MLS group invites
 * @type {MLSWelcome[]}
 */
let arrMLSInvites = [];

/**
 * The current open chat (by npub)
 */
let strOpenChat = "";

/**
 * Get a DM chat for a user
 * @param {string} npub - The user's npub
 * @returns {Chat|undefined} - The chat if it exists
 */
function getDMChat(npub) {
    return arrChats.find(c => c.chat_type === 'DirectMessage' && c.id === npub);
}

/**
 * Get a group chat by ID
 * @param {string} groupId - The group's ID
 * @returns {Chat|undefined} - The chat if it exists
 */
function getGroupChat(groupId) {
    return arrChats.find(c => c.chat_type === 'MlsGroup' && c.id === groupId);
}

/**
 * Get a chat by ID (works for both DMs and Group Chats)
 * @param {string} id - The chat ID (npub for DM, group_id for MlsGroup)
 * @returns {Chat|undefined} - The chat if it exists
 */
function getChat(id) {
    return arrChats.find(c => c.id === id);
}

/**
 * Get or create a chat (DM or MLS Group)
 * @param {string} id - The chat ID (npub for DM, group_id for MlsGroup)
 * @param {string} chatType - 'DirectMessage' or 'MlsGroup'
 * @returns {Chat} - The chat (existing or newly created)
 */
function getOrCreateChat(id, chatType = 'DirectMessage') {
    let chat = chatType === 'MlsGroup' ? getGroupChat(id) : getDMChat(id);
    if (!chat) {
        chat = {
            id: id,
            chat_type: chatType,
            participants: chatType === 'MlsGroup' ? [] : [id],
            messages: [],
            last_read: '',
            created_at: Math.floor(Date.now() / 1000),
            metadata: chatType === 'MlsGroup' ? { group_id: id } : {},
            muted: false
        };
        arrChats.push(chat);
    }
    return chat;
}

/**
 * Get or create a DM chat for a user
 * @param {string} npub - The user's npub
 * @returns {Chat} - The chat (existing or newly created)
 */
function getOrCreateDMChat(npub) {
    return getOrCreateChat(npub, 'DirectMessage');
}

/**
 * Get a profile by npub
 * @param {string} npub - The user's npub
 * @returns {Profile|undefined} - The profile if it exists
 */
function getProfile(npub) {
    return arrProfiles.find(p => p.id === npub);
}

/**
 * Tracks if we're in the initial chat open period for auto-scrolling
 */
let chatOpenAutoScrollTimer = null;

/**
 * Tracks the timestamp when a chat was opened for media load auto-scrolling
 */
let chatOpenTimestamp = 0;

/**
 * Synchronise all messages from the backend
 */
async function init() {
    // Proceed to load and decrypt the database, and begin iterative Nostr synchronisation
    await invoke("fetch_messages", { init: true });

    // Begin an asynchronous loop to refresh profile data
    await invoke("load_profile", { npub: strPubkey });
    fetchProfiles().finally(async () => {
        setAsyncInterval(fetchProfiles, 45000);
    });

    // Load MLS groups and sync welcomes on init
    loadMLSGroups();
    syncMLSWelcomes();

    // Run a very slow loop to update dynamic elements, like "last message time"
    setInterval(() => {
        // If the chatlist is open: re-render to update timestamps
        if (domChats.style.display !== 'none') renderChatlist();

        // If the chat is open; run a 'soft' render
        if (strOpenChat) {
            const chat = arrChats.find(c => c.id === strOpenChat);
            if (chat) updateChat(chat, []);
        }
    }, 30000);

    // Run a faster loop to clear expired typing indicators (every 5 seconds)
    setInterval(() => {
        // Check all chats for expired typing indicators
        const now = Date.now() / 1000;
        arrChats.forEach(chat => {
            if (chat.active_typers && chat.active_typers.length > 0) {
                // Clear the array if we haven't received an update in 35 seconds
                // (30s expiry + 5s grace period for network delays)
                if (!chat.last_typing_update || now - chat.last_typing_update > 35) {
                    // Only log for groups
                    if (chat.chat_type === 'MlsGroup') {
                        console.log('[TYPING] ⏰ Clearing expired typing indicators for group:', chat.id.substring(0, 16));
                    }
                    chat.active_typers = [];
                    
                    // If this is the open chat, refresh the display
                    if (strOpenChat === chat.id) {
                        openChat(chat.id);
                    }
                    
                    // Refresh chat list
                    if (domChats.style.display !== 'none') {
                        renderChatlist();
                    }
                }
            }
        });
    }, 5000);
}

/**
 * Load MLS groups and integrate them into the chat list
 */
async function loadMLSGroups() {
    try {
        const detailed = await invoke('list_mls_groups_detailed');
        const groupsToRefresh = new Set();

        console.log('MLS groups loaded (detailed):', Array.isArray(detailed) ? detailed.length : 0);
        if (!Array.isArray(detailed)) {
            throw new Error('Backend did not return detailed MLS groups array');
        }

        for (const info of detailed) {
            const groupId = info.group_id || info.groupId || info.id;
            if (!groupId) continue;
            groupsToRefresh.add(groupId);

            const chat = getOrCreateChat(groupId, 'MlsGroup');

            // Populate metadata for UI rendering and send path
            chat.metadata = chat.metadata || {};
            chat.metadata.custom_fields = chat.metadata.custom_fields || {};
            
            // Store in custom_fields (backend format)
            const groupName = info.name || chat.metadata.custom_fields.name || `Group ${String(groupId).substring(0, 8)}...`;
            chat.metadata.custom_fields.name = groupName;
            chat.metadata.custom_fields.engine_group_id = info.engine_group_id || info.engineGroupId || chat.metadata.custom_fields.engine_group_id || '';
            chat.metadata.custom_fields.avatar_ref = info.avatar_ref || info.avatarRef || chat.metadata.custom_fields.avatar_ref || null;
            chat.metadata.custom_fields.created_at = String(info.created_at || info.createdAt || chat.metadata.custom_fields.created_at || 0);
            chat.metadata.custom_fields.updated_at = String(info.updated_at || info.updatedAt || chat.metadata.custom_fields.updated_at || 0);
            
            // Preserve member count from detailed API to avoid showing "0 members" before refresh
            const memberCount = (typeof info.member_count === 'number')
                ? info.member_count
                : (typeof info.memberCount === 'number' ? info.memberCount : (chat.metadata.custom_fields.member_count ? parseInt(chat.metadata.custom_fields.member_count) : null));
            if (typeof memberCount === 'number') {
                chat.metadata.custom_fields.member_count = String(memberCount);
            }

            // Load initial messages if not already in memory
            if ((chat.messages || []).length === 0) {
                try {
                    // Use unified chat message storage (same as DMs)
                    const messages = await invoke('get_chat_messages', {
                        chatId: groupId,
                        limit: 50
                    }) || [];

                    console.log(`Loaded ${messages.length} messages for group ${String(groupId).substring(0, 8)}...`);

                    // Messages are already in the correct format from unified storage!
                    chat.messages = messages;
                } catch (e) {
                    console.error(`Failed to load messages for group ${groupId}:`, e);
                    chat.messages = [];
                }
            }
        }

        // After loading groups, refresh their member counts
        await Promise.all(Array.from(groupsToRefresh).map(gid => refreshGroupMemberCount(gid)));
        
        // Sort chats to ensure groups appear properly
        arrChats.sort((a, b) => {
            // Groups without messages go to the bottom
            const aLastMsg = a.messages[a.messages.length - 1];
            const bLastMsg = b.messages[b.messages.length - 1];

            if (!aLastMsg && !bLastMsg) return 0;
            if (!aLastMsg) return 1;
            if (!bLastMsg) return -1;

            return bLastMsg.at - aLastMsg.at;
        });

        // Re-render the chat list
        renderChatlist();
    } catch (e) {
        console.error('Failed to load MLS groups (detailed):', e);
    }
}

/**
 * Refresh and cache the member count for a given MLS group.
 * Also updates open chat header and re-renders chat list.
 */
async function refreshGroupMemberCount(groupId) {
    try {
        const members = await invoke('get_mls_group_members', { groupId });
        const chat = getOrCreateChat(groupId, 'MlsGroup');
        if (Array.isArray(members)) {
            chat.metadata = chat.metadata || {};
            chat.metadata.custom_fields = chat.metadata.custom_fields || {};
            chat.metadata.custom_fields.member_count = String(members.length);
            chat.participants = members.slice();
        }
        if (strOpenChat === groupId) {
            // Update the chat header subtext (respects typing indicators)
            updateChatHeaderSubtext(chat);
        }
    } catch (e) {
        console.warn('Failed to refresh group member count for', groupId, e);
    }
}

/**
 * Load MLS welcomes/invites (no manual sync needed - handled by fetch_messages)
 */
async function syncMLSWelcomes() {
    try {
        // Welcomes are already synced via fetch_messages() -> handle_event()
        // Just load the pending welcomes from the engine
        await loadMLSInvites();
    } catch (e) {
        console.error('Failed to load MLS welcomes:', e);
    }
}

/**
 * Load pending MLS invites and render them
 */
async function loadMLSInvites() {
    try {
        const raw = await invoke('list_pending_mls_welcomes');
        // Normalize shape: backend should return an array; support {welcomes} or {items} fallback
        const welcomes = Array.isArray(raw) ? raw : (raw?.welcomes || raw?.items || []);
        arrMLSInvites = (welcomes || []).filter(Boolean);

        console.log('Pending MLS invites:', arrMLSInvites.length);

        // Render the invites UI
        renderMLSInvites();
    } catch (e) {
        console.error('Failed to load MLS invites:', e);
        // Ensure the section hides on error
        const invitesSection = document.getElementById('mls-invites-section');
        if (invitesSection) invitesSection.style.display = 'none';
        // Recompute layout after invites section visibility change
        adjustSize();
    }
}

/**
 * Render MLS invites in the chat tab
 */
function renderMLSInvites() {
    const invitesSection = document.getElementById('mls-invites-section');
    const invitesContainer = document.getElementById('mls-invites-container');
    if (!invitesSection || !invitesContainer) return;

    // Clear existing content
    invitesContainer.innerHTML = '';

    // Hide section if no invites
    const count = Array.isArray(arrMLSInvites) ? arrMLSInvites.length : 0;
    if (!count) {
        invitesSection.style.display = 'none';
        // Recompute layout since invites section visibility changed
        adjustSize();
        return;
    }

    // Show section if there are invites
    invitesSection.style.display = '';

    // Render each invite safely (avoid exceptions on missing fields)
    for (const invite of arrMLSInvites) {
        try {
            const divInvite = document.createElement('div');
            divInvite.classList.add('mls-invite-item');

            const groupId = invite.group_id || invite.id || '';
            const groupName =
                invite.group_name ||
                invite.name ||
                (groupId ? `Group ${String(groupId).substring(0, 8)}...` : 'Unnamed Group');

            // Backend returns 'welcomer' as npub string
            const welcomerNpub = invite.welcomer || '';

            // Get display name from profile (nickname > name > npub prefix)
            let welcomerDisplay = 'Unknown';
            if (welcomerNpub) {
                const welcomerProfile = getProfile(welcomerNpub);
                welcomerDisplay = welcomerProfile?.nickname ||
                                 welcomerProfile?.name ||
                                 welcomerNpub.substring(0, 16) + '...';
            }

            const memberCount =
                (invite.member_count ??
                    (Array.isArray(invite.members) ? invite.members.length : invite.memberCount)) || 0;

            // Group name
            const h4Name = document.createElement('h4');
            h4Name.textContent = groupName;
            divInvite.appendChild(h4Name);

            // Inviter info
            const pInfo = document.createElement('p');
            pInfo.textContent = `Invited by ${welcomerDisplay} • ${memberCount} ${memberCount === 1 ? 'member' : 'members'}`;
            divInvite.appendChild(pInfo);

            // Action buttons
            const divActions = document.createElement('div');
            divActions.classList.add('invite-actions');

            const btnAccept = document.createElement('button');
            btnAccept.classList.add('btn', 'btn-bounce', 'accept-btn');
            btnAccept.textContent = 'Accept';
            btnAccept.onclick = () => acceptMLSInvite(invite.id || invite.welcome_event_id || groupId);

            const btnDecline = document.createElement('button');
            btnDecline.classList.add('btn', 'btn-bounce', 'logout-btn');
            btnDecline.textContent = 'Decline';
            btnDecline.onclick = () => declineMLSInvite(invite.id || invite.welcome_event_id || groupId);

            divActions.appendChild(btnAccept);
            divActions.appendChild(btnDecline);
            divInvite.appendChild(divActions);

            invitesContainer.appendChild(divInvite);
        } catch (err) {
            console.warn('Failed to render MLS invite', invite, err);
        }
    }

    // After rendering invites, recompute layout to avoid oversized chat list
    adjustSize();
}

/**
 * Accept an MLS group invite
 * @param {string} welcomeEventId - The welcome event ID
 */
async function acceptMLSInvite(welcomeEventId) {
    try {
        console.log('Accepting MLS invite:', welcomeEventId);
        const success = await invoke('accept_mls_welcome', {
            welcomeEventIdHex: welcomeEventId
        });
        
        if (success) {
            // Reload invites and groups
            await loadMLSInvites();
            await loadMLSGroups();

            // After rendering UI changes, ensure layout recalculates to prevent oversized chat list
            adjustSize();
        }
    } catch (e) {
        console.error('Failed to accept invite:', e);
        popupConfirm('Error', 'Failed to join group: ' + e, true, '', 'vector_warning.svg');
    }
}

/**
 * Decline an MLS invite (UI-only hide; backend filtering handles persistence)
 * @param {string} welcomeEventId - The welcome event ID
 */
function declineMLSInvite(welcomeEventId) {
    // Remove from UI; next backend fetch will exclude if server persisted dismissal
    arrMLSInvites = arrMLSInvites.filter(i => i.id !== welcomeEventId);
    renderMLSInvites();

    // After rendering UI changes, ensure layout recalculates to prevent oversized chat list
    adjustSize();
}

/**
 * A "thread" function dedicated to refreshing Profile data in the background
 */
async function fetchProfiles() {
    // Poll for changes in profiles
    for (const profile of arrProfiles) {
        await invoke("load_profile", { npub: profile.id });
    }
}

/**
 * Update the chat header subtext (status/typing indicator) for the currently open chat
 * @param {Object} chat - The chat object
 */
function updateChatHeaderSubtext(chat) {
    if (!chat) return;
    
    const isGroup = chat.chat_type === 'MlsGroup';
    const fNotes = chat.id === strPubkey;
    
    if (fNotes) {
        domChatContactStatus.textContent = 'Encrypted Notes to Self';
        domChatContactStatus.classList.remove('text-gradient');
    } else if (isGroup) {
        // Check for typing indicators in groups
        const activeTypers = chat.active_typers || [];
        const fIsTyping = activeTypers.length > 0;
        
        if (fIsTyping) {
            // Display typing indicator with Discord-style multi-user support
            if (activeTypers.length === 1) {
                const typer = getProfile(activeTypers[0]);
                const name = typer?.nickname || typer?.name || 'Someone';
                domChatContactStatus.textContent = `${name} is typing...`;
            } else if (activeTypers.length === 2) {
                const typer1 = getProfile(activeTypers[0]);
                const typer2 = getProfile(activeTypers[1]);
                const name1 = typer1?.nickname || typer1?.name || 'Someone';
                const name2 = typer2?.nickname || typer2?.name || 'Someone';
                domChatContactStatus.textContent = `${name1} and ${name2} are typing...`;
            } else if (activeTypers.length === 3) {
                const typer1 = getProfile(activeTypers[0]);
                const typer2 = getProfile(activeTypers[1]);
                const typer3 = getProfile(activeTypers[2]);
                const name1 = typer1?.nickname || typer1?.name || 'Someone';
                const name2 = typer2?.nickname || typer2?.name || 'Someone';
                const name3 = typer3?.nickname || typer3?.name || 'Someone';
                domChatContactStatus.textContent = `${name1}, ${name2}, and ${name3} are typing...`;
            } else {
                // 4+ people typing
                domChatContactStatus.textContent = `Several people are typing...`;
            }
            domChatContactStatus.classList.add('text-gradient');
        } else {
            const memberCount = chat.metadata?.custom_fields?.member_count ? parseInt(chat.metadata.custom_fields.member_count) : null;
            if (typeof memberCount === 'number') {
                const label = memberCount === 1 ? 'member' : 'members';
                domChatContactStatus.textContent = `${memberCount} ${label}`;
            } else {
                // Avoid misleading "0 members" before first count refresh
                domChatContactStatus.textContent = 'Members syncing...';
            }
            domChatContactStatus.classList.remove('text-gradient');
        }
    } else {
        // Check for typing indicators in DMs (using chat.active_typers)
        const activeTypers = chat.active_typers || [];
        const fIsTyping = activeTypers.length > 0;
        
        if (fIsTyping) {
            // For DMs, there should only be one typer (the other person)
            const typer = getProfile(activeTypers[0]);
            const name = typer?.nickname || typer?.name || 'User';
            domChatContactStatus.textContent = `${name} is typing...`;
            domChatContactStatus.classList.add('text-gradient');
        } else {
            const profile = getProfile(chat.id);
            domChatContactStatus.textContent = profile?.status?.title || '';
            domChatContactStatus.classList.remove('text-gradient');
            twemojify(domChatContactStatus);
        }
    }
}

/**
 * A "thread" function dedicated to rendering the Chat UI in real-time
 */
function renderChatlist() {
    if (fInit) return;

    // Check if the order of chats has changed
    const currentOrder = Array.from(domChatList.children).map(el => el.id.replace('chatlist-', ''));
    const orderChanged = JSON.stringify(currentOrder) !== JSON.stringify(arrChats.map(chat => chat.id));

    // If the order of the chatlist changes (i.e: new message), prep a fragment to re-render the full list in one sweep
    const fragment = document.createDocumentFragment();
    for (const chat of arrChats) {
        // For groups, we show them even if they have no messages yet
        // For DMs, we only show them if they have messages
        if (chat.chat_type !== 'MlsGroup' && chat.messages.length === 0) continue;

        // Do not render our own profile: it is accessible via the Bookmarks/Notes section
        if (chat.id === strPubkey) continue;

        // If the chat order changed; append to fragment instead of directly to the DOM for full list re-render efficiency
        const divContact = renderChat(chat);
        if (orderChanged) {
            fragment.appendChild(divContact);
        } else {
            // The order hasn't changed, so it's more efficient to replace the existing elements
            const domExistingContact = document.getElementById(`chatlist-${chat.id}`);
            domExistingContact.replaceWith(divContact);
            // Note: we don't check if domExistingContact exists now, as it should be guarenteed by the very first orderChanged check
        }
    }

    // Give the final element a bottom-margin boost to allow scrolling past the fadeout
    if (fragment.lastElementChild) fragment.lastElementChild.style.marginBottom = `50px`;

    // Add all elements at once for performance
    if (orderChanged) {
        // Nuke the existing list
        while (domChatList.firstChild) {
            domChatList.removeChild(domChatList.firstChild);
        }
        // Append our new fragment
        domChatList.appendChild(fragment);

        // Add a fade-in
        const divFade = document.createElement('div');
        divFade.classList.add(`fadeout-bottom`);
        divFade.style.bottom = `65px`;
        domChatList.appendChild(divFade);
    }
}

/**
 * Render a Chat Preview for the Chat List
 * @param {Chat} chat - The profile we're rendering
 */
function renderChat(chat) {
    // For groups, we don't have a profile, for DMs we do
    const isGroup = chat.chat_type === 'MlsGroup';
    const profile = !isGroup && chat.participants.length === 1 ? getProfile(chat.id) : null;
    
    // Collect the Unread Message count for 'Unread' emphasis and badging
    // Ensure muted chats OR muted profiles do not show unread glow
    const nUnread = (chat.muted || (profile && profile.muted)) ? 0 : countUnreadMessages(chat);

    // The Chat container (The ID is the Contact's npub)
    const divContact = document.createElement('div');
    if (nUnread) divContact.style.borderColor = '#59fcb3';
    divContact.classList.add('chatlist-contact');
    divContact.id = chat.id;

    // The Username + Message Preview container
    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');

    // The avatar, if one exists
    const divAvatarContainer = document.createElement('div');
    divAvatarContainer.style.position = `relative`;
    divAvatarContainer.style.zIndex = `-1`;
    
    if (isGroup) {
        // For groups, show a group icon or placeholder
        const groupIcon = document.createElement('div');
        groupIcon.style.width = '50px';
        groupIcon.style.height = '50px';
        groupIcon.style.borderRadius = '50%';
        groupIcon.style.backgroundColor = '#444';
        groupIcon.style.display = 'flex';
        groupIcon.style.alignItems = 'center';
        groupIcon.style.justifyContent = 'center';
        groupIcon.innerHTML = '<span class="icon icon-chats" style="color: #fff;"></span>';
        divAvatarContainer.appendChild(groupIcon);
    } else if (profile?.avatar) {
        const imgAvatar = document.createElement('img');
        imgAvatar.src = profile?.avatar;
        divAvatarContainer.appendChild(imgAvatar);
    } else {
        // Otherwise, generate a Gradient Avatar
        divAvatarContainer.appendChild(pubkeyToAvatar(profile?.id || chat.id, profile?.nickname || profile?.name, 50));
    }

    // Add the "Status Icon" to the avatar, then plug-in the avatar container
    // TODO: currently, we "emulate" the status; messages in the last 5m are "online", messages in the last 30m are "away", otherwise; offline.
    const divStatusIcon = document.createElement('div');
    divStatusIcon.classList.add('avatar-status-icon');
    
    // Find the last message from the contact (not from the user)
    let cLastContactMsg = null;
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        if (!chat.messages[i].mine) {
            cLastContactMsg = chat.messages[i];
            break;
        }
    }
    
    if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 5) {
        // set the divStatusIcon .backgroundColor to green (online)
        divStatusIcon.style.backgroundColor = '#59fcb3';
        divAvatarContainer.appendChild(divStatusIcon);
    }
    else if (cLastContactMsg && cLastContactMsg.at > Date.now() - 60000 * 30) {
        // set to orange (away)
        divStatusIcon.style.backgroundColor = '#fce459';
        divAvatarContainer.appendChild(divStatusIcon);
    }
    // offline... don't show status icon at all (no need to append the divStatusIcon)
    
    divContact.appendChild(divAvatarContainer);

    // Add the name to the chat preview
    const h4ContactName = document.createElement('h4');
    if (isGroup) {
        // For groups, extract name from metadata or use a default
        h4ContactName.textContent = chat.metadata?.custom_fields?.name || `Group ${chat.id.substring(0, 8)}...`;
    } else {
        h4ContactName.textContent = profile?.nickname || profile?.name || chat.id;
        if (profile?.nickname || profile?.name) twemojify(h4ContactName);
    }
    h4ContactName.classList.add('cutoff')
    divPreviewContainer.appendChild(h4ContactName);

    // Display either their Last Message or Typing Indicator
    const cLastMsg = chat.messages[chat.messages.length - 1];
    const pChatPreview = document.createElement('p');
    pChatPreview.classList.add('cutoff');
    
    if (isGroup) {
        // Check for typing indicators in groups
        const activeTypers = chat.active_typers || [];
        const fIsTyping = activeTypers.length > 0;
        pChatPreview.classList.toggle('text-gradient', fIsTyping);
        
        if (fIsTyping) {
            // Display typing indicator with Discord-style multi-user support
            if (activeTypers.length === 1) {
                const typer = getProfile(activeTypers[0]);
                const name = typer?.nickname || typer?.name || 'Someone';
                pChatPreview.textContent = `${name} is typing...`;
            } else if (activeTypers.length === 2) {
                const typer1 = getProfile(activeTypers[0]);
                const typer2 = getProfile(activeTypers[1]);
                const name1 = typer1?.nickname || typer1?.name || 'Someone';
                const name2 = typer2?.nickname || typer2?.name || 'Someone';
                pChatPreview.textContent = `${name1} and ${name2} are typing...`;
            } else {
                // 3+ people typing
                const typer1 = getProfile(activeTypers[0]);
                const name1 = typer1?.nickname || typer1?.name || 'Someone';
                pChatPreview.textContent = `${name1} and ${activeTypers.length - 1} others are typing...`;
            }
        } else {
            const memberCount = chat.metadata?.custom_fields?.member_count ? parseInt(chat.metadata.custom_fields.member_count) : null;
            if (!cLastMsg) {
                pChatPreview.textContent = (memberCount != null)
                    ? `${memberCount} ${memberCount === 1 ? 'member' : 'members'}`
                    : 'No messages yet';
            } else if (cLastMsg.pending) {
                pChatPreview.textContent = `Sending...`;
            } else if (!cLastMsg.content && cLastMsg.attachments?.length) {
                // No text content but has attachments; display as an attachment
                let senderPrefix = '';
                if (cLastMsg.mine) {
                    senderPrefix = 'You: ';
                } else if (cLastMsg.npub) {
                    // Get sender's display name (nickname > name > npub prefix)
                    const senderProfile = getProfile(cLastMsg.npub);
                    const senderName = senderProfile?.nickname || senderProfile?.name || cLastMsg.npub.substring(0, 16);
                    senderPrefix = `${senderName}: `;
                }
                pChatPreview.textContent = senderPrefix + 'Sent a ' + getFileTypeInfo(cLastMsg.attachments[0].extension).description;
            } else {
                let senderPrefix = '';
                if (cLastMsg.mine) {
                    senderPrefix = 'You: ';
                } else if (cLastMsg.npub) {
                    // Get sender's display name (nickname > name > npub prefix)
                    const senderProfile = getProfile(cLastMsg.npub);
                    const senderName = senderProfile?.nickname || senderProfile?.name || cLastMsg.npub.substring(0, 16);
                    senderPrefix = `${senderName}: `;
                }
                pChatPreview.textContent = senderPrefix + cLastMsg.content;
                twemojify(pChatPreview);
            }
        }
    } else {
        // Check for typing indicators in DMs (using chat.active_typers)
        const activeTypers = chat.active_typers || [];
        const fIsTyping = activeTypers.length > 0;
        pChatPreview.classList.toggle('text-gradient', fIsTyping);
        
        if (fIsTyping) {
            // Typing; display the glowy indicator!
            pChatPreview.textContent = `Typing...`;
        } else if (!cLastMsg) {
            pChatPreview.textContent = 'Start a conversation';
        } else if (!cLastMsg.content && cLastMsg.attachments?.length && !cLastMsg.pending) {
            // Not typing, and no text; display as an attachment
            pChatPreview.textContent = (cLastMsg.mine ? 'You: ' : '') + 'Sent a ' + getFileTypeInfo(cLastMsg.attachments[0].extension).description;
        } else if (cLastMsg.pending) {
            // A message is pending: thus, we're still sending one
            pChatPreview.textContent = `Sending...`;
        } else {
            // Not typing; display their last message
            pChatPreview.textContent = (cLastMsg.mine ? 'You: ' : '') + cLastMsg.content;
            twemojify(pChatPreview);
        }
    }
    divPreviewContainer.appendChild(pChatPreview);

    // Add the Chat Preview to the contact UI
    // Note: as a hacky trick to make `divContact` receive all clicks, we set the z-index lower on it's children
    divPreviewContainer.style.zIndex = `-1`; // Note: used to prevent the button from appearing in front of the `Popup` UI
    divContact.appendChild(divPreviewContainer);

    // Display the "last message" time
    const pTimeAgo = document.createElement('p');
    pTimeAgo.classList.add('chatlist-contact-timestamp');
    if (cLastMsg) {
        pTimeAgo.textContent = timeAgo(cLastMsg.at);
        if (pTimeAgo.textContent !== 'Now') pTimeAgo.textContent += ` ago`;
    }
    // Apply 'Unread' final styling
    if (nUnread) pTimeAgo.style.color = '#59fcb3';
    divContact.appendChild(pTimeAgo);

    return divContact;
}

/**
 * Count the quantity of unread messages
 * @param {Chat} chat - The Chat we're checking
 * @returns {number} - The amount of unread messages, if any
 */
function countUnreadMessages(chat) {
    // If no messages, return 0
    if (!chat.messages || !chat.messages.length) return 0;
    
    // If last_read is set, count messages after it
    if (chat.last_read) {
        // Find the index of the last read message
        const lastReadIndex = chat.messages.findIndex(msg => msg.id === chat.last_read);
        if (lastReadIndex !== -1) {
            // Count non-mine messages after the last read message
            let unreadCount = 0;
            for (let i = lastReadIndex + 1; i < chat.messages.length; i++) {
                if (!chat.messages[i].mine) {
                    unreadCount++;
                }
            }
            return unreadCount;
        }
        // If last_read message not found, fall back to walk-back logic
    }
    
    // No last_read set or not found - walk backwards from the end
    let unreadCount = 0;
    
    // Iterate messages in reverse order (newest first)
    for (let i = chat.messages.length - 1; i >= 0; i--) {
        if (chat.messages[i].mine) {
            // If we find our own message first, everything before it is considered read
            // (because we responded to those messages)
            break;
        } else {
            // Count non-mine messages (unread)
            unreadCount++;
        }
    }
    
    return unreadCount;
}

/**
 * Sets a specific message as the last read message
 * @param {Chat} chat - The Chat to update
 * @param {Message|string} message - The Message object or message ID to set as last read
 */
function markAsRead(chat, message) {
    // If a Message object was provided, extract its ID
    const messageId = typeof message === 'string' ? message : message.id;

    // If we have a chat, update its last_read and notify backend
    if (chat) {
        chat.last_read = messageId;

        // Persist via backend using chat-based API
        invoke("mark_as_read", { chatId: chat.id, messageId: messageId });
    }
    // If no chat is supplied, do nothing (no profile-based persistence here)
}

/**
 * Send a NIP-17 message to a Nostr user
 * @param {string} pubkey - The user's pubkey
 * @param {string} content - The content of the message
 * @param {string?} replied_to - The reference of the message, if any
 */
async function message(pubkey, content, replied_to) {
    await invoke("message", { receiver: pubkey, content: content, repliedTo: replied_to });
}

/**
 * Send a file via NIP-96 server to a Nostr user or group
 * @param {string} pubkey - The user's pubkey or group_id
 * @param {string?} replied_to - The reference of the message, if any
 * @param {string} filepath - The absolute file path
 */
async function sendFile(pubkey, replied_to, filepath) {
    try {
        // Use the protocol-agnostic file_message command for both DMs and MLS groups
        await invoke("file_message", { receiver: pubkey, repliedTo: replied_to, filePath: filepath });
    } catch (e) {
        // Notify of an attachment send failure
        popupConfirm(e, '', true, '', 'vector_warning.svg');
    }
    nLastTypingIndicator = 0;
}

/**
 * A blocking function that continually polls NIP-96 servers for their configs.
 * 
 * Note: This function should only be called once, and COULD block for a very long time (i.e: if offline).
 */
async function warmupUploadServers() {
    // This simple function continually polls Vector's NIP-96 servers until configs are cached, for faster file uploads later
    while (true) {
        if (await invoke('warmup_nip96_servers')) break;
        // If we reach here, this warmup attempt failed: sleep for a bit and try again soon
        await sleep(5000);
    }
}

/**
 * Setup our Rust Event listeners, used for relaying the majority of backend changes
 */
async function setupRustListeners() {
    // Listen for MLS message events
    await listen('mls_message_new', async (evt) => {
        const { group_id, message } = evt.payload;
        console.log('MLS message received for group:', group_id, 'pending:', message?.pending, 'attachments:', message?.attachments?.length);
        
        // Validate message has required fields
        if (!message || !message.id || !message.at) {
            console.warn('Invalid message received (missing id or timestamp):', message);
            return;
        }
        
        // Find or create the group chat
        const chat = getOrCreateChat(group_id, 'MlsGroup');
        
        // Message is now a full Message object from unified storage!
        // It already has the correct format with attachments, reactions, etc.
        const newMessage = {
            id: message.id,
            content: message.content,
            replied_to: message.replied_to || null,
            preview_metadata: message.preview_metadata || null,
            attachments: message.attachments || [],
            reactions: message.reactions || [],
            at: message.at, // Already in milliseconds
            pending: message.pending || false,
            failed: message.failed || false,
            mine: message.mine,
            npub: message.npub || null // Sender's npub for group chats
        };
        
        // Check for duplicates
        const existingMsg = chat.messages.find(m => m.id === newMessage.id);
        if (existingMsg) {
            console.log('Duplicate message detected (already in memory):', newMessage.id);
            return;
        }
        
        // Clear typing indicator for the sender when they send a message
        if (!newMessage.mine && chat.active_typers && message.sender_npub) {
            // Remove the sender from active typers
            chat.active_typers = chat.active_typers.filter(npub => npub !== message.sender_npub);
            console.log('[TYPING] 💬 Cleared typing indicator after group message from:', message.sender_npub.substring(0, 16));
            
            // If this is the open chat, refresh the display
            if (strOpenChat === group_id) {
                openChat(group_id);
            }
        }
        
        // Add message to chat
        chat.messages.push(newMessage);
        
        // Sort messages by time
        chat.messages.sort((a, b) => a.at - b.at);
        
        // If this group has the open chat, update it
        if (strOpenChat === group_id) {
            console.log('Updating open group chat with new message, pending:', newMessage.pending);
            updateChat(chat, [newMessage]);
        } else {
            console.log('Group chat not open, message added to background chat');
        }
        
        // Resort chat list order by last message time so groups bubble to the top
        arrChats.sort((a, b) => {
            const aLast = a.messages[a.messages.length - 1];
            const bLast = b.messages[b.messages.length - 1];
            if (!aLast) return 1;
            if (!bLast) return -1;
            return bLast.at - aLast.at;
        });
        
        // Re-render chat list
        renderChatlist();
    });

    // Listen for MLS invite received events (real-time)
    await listen('mls_invite_received', async (evt) => {
        console.log('MLS invite received in real-time, refreshing invites list');
        // Reload invites list to show the new invite
        await loadMLSInvites();
    });

    // Listen for MLS welcome accepted events
    await listen('mls_welcome_accepted', async (evt) => {
        console.log('MLS welcome accepted, refreshing groups and invites');
        // Reload invites and groups
        await loadMLSInvites();
        await loadMLSGroups();
    });

    // Listen for MLS initial sync completion after joining a group
    await listen('mls_group_initial_sync', async (evt) => {
        try {
            const { group_id, processed, new: newCount } = evt.payload || {};
            console.log('MLS initial group sync complete:', group_id, 'processed:', processed, 'new:', newCount);

            // Ensure the group chat exists even if there are no messages yet
            const chat = getOrCreateChat(group_id, 'MlsGroup');
            await refreshGroupMemberCount(group_id);

            // If there are no messages loaded yet, fetch a small recent slice so we can render previews/order
            if (!chat.messages || chat.messages.length === 0) {
                try {
                    // Use unified chat message storage (same as DMs)
                    const messages = await invoke('get_chat_messages', {
                        chatId: group_id,
                        limit: 50
                    }) || [];

                    // Messages are already in the correct format from unified storage!
                    chat.messages = messages;
                } catch (e) {
                    console.warn('Failed to load initial group messages after sync:', group_id, e);
                    // Keep chat with zero messages; UI will show "No messages yet"
                    chat.messages = [];
                }
            }

            // Resort chat list order by last message time to reflect any loaded timeline
            arrChats.sort((a, b) => {
                const aLast = a.messages[a.messages.length - 1];
                const bLast = b.messages[b.messages.length - 1];
                if (!aLast) return 1;
                if (!bLast) return -1;
                return bLast.at - aLast.at;
            });

            // Re-render the chat list so empty groups show with "No messages yet" preview
            renderChatlist();
        } catch (e) {
            console.error('Error handling mls_group_initial_sync:', e);
        }
    });

    // Listen for Synchronisation Finish updates
    await listen('sync_finished', async (_) => {
        // Display that we finished syncing
        domSyncStatus.textContent = 'Synchronised';

        // Wait 1 second, then slide out and hide when done
        await slideout(domSyncStatusContainer, { delay: 1000 });

        // Reset the text and adjust the UI if necessary
        domSyncStatus.textContent = '';
        if (!strOpenChat) adjustSize();
    });

    // Listen for Synchronisation Slice updates
    await listen('sync_slice_finished', (_) => {
        // Continue synchronising until event `sync_finished` is emitted
        invoke("fetch_messages", { init: false });
    });

    // Listen for Synchronisation Progress updates
    await listen('sync_progress', (evt) => {
        // Display the dates we're syncing between
        const options = { month: 'short', day: 'numeric' };
        const start = new Date(evt.payload.since * 1000).toLocaleDateString('en-US', options);
        const end = new Date(evt.payload.until * 1000).toLocaleDateString('en-US', options);
        if (!fInit) domSyncStatusContainer.style.display = ``;
        domSyncStatus.textContent = `Syncing Messages between ${start} - ${end}`;
        if (!strOpenChat) adjustSize();
    });

    // Listen for Attachment Upload Progress events
    await listen('attachment_upload_progress', async (evt) => {
        if (strOpenChat) {
            let divUpload = document.getElementById(evt.payload.id + '_file');
            if (divUpload) {
                // Update the Download Progress bar
                divUpload.style.width = `${evt.payload.progress}%`;
            }
        }
    });

    // Listen for Attachment Download Progress events
    await listen('attachment_download_progress', async (evt) => {
        // Update the in-memory attachment
        if (strOpenChat) {
            let divDownload = document.getElementById(evt.payload.id);
            if (divDownload) {
                // Check if we need a text label (for non-image files)
                let iLabel = divDownload.querySelector('.download-label');
                if (iLabel) {
                    // Update the text with progress
                    iLabel.textContent = `Downloading... (${evt.payload.progress}%)`;
                }
                
                let divBar = divDownload.querySelector('.progress-bar');
                if (divBar) {
                    // Update the Download Progress bar
                    divBar.style.width = `${evt.payload.progress}%`;
                } else {
                    // Create the Download Progress container
                    let newDivDownload = document.createElement('div');
                    newDivDownload.id = evt.payload.id;
                    newDivDownload.style.minWidth = `200px`;
                    newDivDownload.style.textAlign = `center`;
                    
                    // For non-image files, add a text label
                    // Check if the element being updated is a non-image file by looking for existing download button attributes
                    const attachmentElement = document.getElementById(evt.payload.id);
                    const isNonImageAttachment = attachmentElement && (attachmentElement.hasAttribute('download') || attachmentElement.classList.contains('btn'));
                    
                    if (isNonImageAttachment) {
                        const iLabel = document.createElement('i');
                        iLabel.classList.add('download-label');
                        iLabel.textContent = `Downloading... (${evt.payload.progress}%)`;
                        iLabel.style.display = `block`;
                        iLabel.style.marginBottom = `5px`;
                        newDivDownload.appendChild(iLabel);
                    }

                    // Create the Download Progress bar
                    divBar = document.createElement('div');
                    divBar.classList.add('progress-bar');
                    divBar.style.width = `0%`;
                    divBar.style.height = `5px`;
                    divBar.style.marginTop = `0`;
                    newDivDownload.appendChild(divBar);

                    // Replace the previous UI
                    divDownload.replaceWith(newDivDownload);
                }
            }
        }
    });

    // Listen for Attachment Download Results
    await listen('attachment_download_result', async (evt) => {
        // Update the in-memory attachment (works for both DMs and Group Chats)
        let cChat = getChat(evt.payload.profile_id);
        if (!cChat) return;
        
        let cMsg = cChat.messages.find(m => m.id === evt.payload.msg_id);
        if (!cMsg) return;
    
        // When an attachment is being updated (i.e: post-hashing ID change), we reference the original nonce-based hash via old_id, otherwise, we use ID, as nothing changed
        let cAttachment = cMsg.attachments.find(a => a.id === evt.payload?.old_id || evt.payload.id);
        if (!cAttachment) return;

        cAttachment.downloading = false;
        if (evt.payload.success) {
            cAttachment.downloaded = true;
            // Update our ID and Path
            if (evt.payload.old_id) {
                cAttachment.id = evt.payload.id;
                cAttachment.path = cAttachment.path.replace(evt.payload.old_id, evt.payload.id);
            }

            // If this user has an open chat, then update the rendered message
            if (strOpenChat === evt.payload.profile_id) {
                const domMsg = document.getElementById(evt.payload.msg_id);
                const profile = getProfile(evt.payload.profile_id);
                domMsg?.replaceWith(renderMessage(cMsg, profile, evt.payload.msg_id));

                // Scroll accordingly
                softChatScroll();
            }
        } else {
            // Display the reason the download failed and allow restarting it
            const divDownload = document.getElementById(evt.payload.id);
            const iFailed = document.createElement('i');
            iFailed.id = evt.payload.id;
            iFailed.toggleAttribute('download', true);
            iFailed.setAttribute('npub', evt.payload.profile_id);
            iFailed.setAttribute('msg', evt.payload.msg_id);
            iFailed.classList.add('btn');
            iFailed.textContent = `Retry Download (${evt.payload.result})`;
            divDownload.replaceWith(iFailed);
        }
    });

    // Listen for profile updates
    await listen('profile_update', (evt) => {
        // Check if the frontend is already aware
        const nProfileIdx = arrProfiles.findIndex(p => p.id === evt.payload.id);
        if (nProfileIdx >= 0) {
            // Update our frontend memory
            arrProfiles[nProfileIdx] = evt.payload;

            // If this is our profile, make sure to render it's changes
            if (arrProfiles[nProfileIdx].mine) {
                renderCurrentProfile(arrProfiles[nProfileIdx]);
            }
        } else {
            // Add the new profile
            arrProfiles.push(evt.payload);
        }
        
        // If this user has an open chat, then soft-update the chat header
        if (strOpenChat === evt.payload.id) {
            const chat = getDMChat(evt.payload.id);
            const profile = getProfile(evt.payload.id);
            if (chat && profile) {
                updateChat(chat, [], profile);
            }
        }
        
        // If this user is being viewed in the Expanded Profile View, update it
        // Note: no need to update our own, it makes editing very weird
        if (!evt.payload.mine && domProfileId.textContent === evt.payload.id) {
            renderProfileTab(evt.payload);
        }
        
        // Render the Chat List
        renderChatlist();
    });

    await listen('profile_muted', (evt) => {
        // Update the chat's muted status
        const cChat = getDMChat(evt.payload.profile_id);
        if (cChat) {
            cChat.muted = evt.payload.value;
        }
        
        // Also update profile if it exists
        const cProfile = getProfile(evt.payload.profile_id);
        if (cProfile) {
            cProfile.muted = evt.payload.value;
        }

        // If this profile is Expanded, update the Mute UI
        if (domProfileId.textContent === evt.payload.profile_id && cProfile) {
            domProfileOptionMute.querySelector('span').classList.replace('icon-volume-' + (cProfile.muted ? 'max' : 'mute'), 'icon-volume-' + (cProfile.muted ? 'mute' : 'max'));
            domProfileOptionMute.querySelector('p').innerText = cProfile.muted ? 'Unmute' : 'Mute';
        }

        // Re-render the chat list to immediately reflect glow/badge changes
        renderChatlist();
    });

    await listen('profile_nick_changed', (evt) => {
        // Update the profile's nickname
        const cProfile = getProfile(evt.payload.profile_id);
        if (cProfile) {
            cProfile.nickname = evt.payload.value;
            
            // If this profile is Expanded, update the UI
            if (domProfileId.textContent === evt.payload.profile_id) {
                renderProfileTab(cProfile);
            }
        }
    });

    // Listen for typing indicator updates (both DMs and Groups)
    await listen('typing-update', (evt) => {
        const { conversation_id, typers } = evt.payload;
        
        // Find the chat (could be DM or group)
        const chat = arrChats.find(c => c.id === conversation_id);
        if (!chat) return;
        
        // Only log for groups
        const isGroup = chat.chat_type === 'MlsGroup';
        if (isGroup) {
            console.log('[TYPING] 📥 Frontend received typing-update:', { conversation_id: conversation_id.substring(0, 16), typers });
        }
        
        // Store the typers array and update timestamp
        chat.active_typers = typers || [];
        chat.last_typing_update = Date.now() / 1000;
        
        if (isGroup) {
            console.log('[TYPING] 💾 Updated chat.active_typers:', { chat_id: chat.id.substring(0, 16), active_typers: chat.active_typers });
        }
        
        // If this chat is currently open, update the chat header subtext
        if (strOpenChat === conversation_id) {
            if (isGroup) console.log('[TYPING] 🔄 Updating chat header subtext');
            updateChatHeaderSubtext(chat);
        }
        
        // Update the chat list preview
        if (isGroup) console.log('[TYPING] 🔄 Refreshing chat list');
        renderChatlist();
    });

    // Listen for incoming DM messages
    await listen('message_new', (evt) => {
        // Get the chat for this message (chat_id is the npub for DMs)
        let chat = getOrCreateDMChat(evt.payload.chat_id);
        
        // Get the new message
        const newMessage = evt.payload.message;

        // Double-check we haven't received this twice
        const existingMsg = chat.messages.find(m => m.id === newMessage.id);
        if (existingMsg) return;

        // Clear typing indicator for the sender when they send a message
        if (!newMessage.mine && chat.active_typers) {
            // Remove the sender from active typers
            const senderNpub = evt.payload.chat_id;
            chat.active_typers = chat.active_typers.filter(npub => npub !== senderNpub);
            
            // If this is the open chat, refresh the display
            if (strOpenChat === evt.payload.chat_id) {
                openChat(evt.payload.chat_id);
            }
        }

        // Find the correct position to insert the message based on timestamp
        const messages = chat.messages;

        // Check if the array is empty or the new message is newer than the newest message
        if (messages.length === 0 || newMessage.at > messages[messages.length - 1].at) {
            // Insert at the end (newest)
            messages.push(newMessage);

            // Sort chats by last message time
            arrChats.sort((a, b) => {
                const aLastMsg = a.messages[a.messages.length - 1];
                const bLastMsg = b.messages[b.messages.length - 1];
                if (!aLastMsg) return 1;
                if (!bLastMsg) return -1;
                return bLastMsg.at - aLastMsg.at;
            });
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

        // If this user has the open chat, then update the chat too
        if (strOpenChat === evt.payload.chat_id) {
            updateChat(chat, [newMessage]);
        }

        // Render the Chat List
        renderChatlist();
    });

    // Listen for existing message updates
    await listen('message_update', (evt) => {
        // Find the message we're updating - works for both DMs and groups
        const cChat = getChat(evt.payload.chat_id);
        if (!cChat) return;
        const nMsgIdx = cChat.messages.findIndex(m => m.id === evt.payload.old_id);
        if (nMsgIdx === -1) return;

        // Update it
        cChat.messages[nMsgIdx] = evt.payload.message;

        // If this chat is open, then update the rendered message
        if (strOpenChat === evt.payload.chat_id) {
            // TODO: is there a slight possibility of a race condition here? i.e: `message_update` calls before `message_new` and thus domMsg isn't found?
            const domMsg = document.getElementById(evt.payload.old_id);
            
            // For DMs, get the profile; for groups, profile will be null
            const profile = getProfile(evt.payload.chat_id);
            domMsg?.replaceWith(renderMessage(evt.payload.message, profile, evt.payload.old_id));

            // If the old ID was a pending ID (our message), make sure to update and scroll accordingly
            if (evt.payload.old_id.startsWith('pending')) {
                strLastMsgID = evt.payload.message.id;
                softChatScroll();
            }
        }

        // Render the Chat List
        renderChatlist();
    });

    // Listen for Vector Voice AI (Whisper) model download progression updates
    await listen('whisper_download_progress', async (evt) => {
        // Update the progression UI
        const spanProgression = document.getElementById('voice-model-download-progression');
        if (spanProgression) spanProgression.textContent = `(${evt.payload.progress}%)`;
    });

    // Listen for Windows-specific Overlay Icon update requests
    // Note: this API seems unavailable in Tauri's Rust backend, so we're using the JS API as a workaround
    await listen('update_overlay_icon', async (evt) => {
        // Enable or Disable our notification badge Overlay Icon
        await getCurrentWindow().setOverlayIcon(evt.payload.enable ? "./icons/icon_badge_notification.png" : undefined);
    });

    // Listen for relay status changes
    await listen('relay_status_change', (evt) => {
        // Update the relay status in the network list
        const relayItem = document.querySelector(`[data-relay-url="${evt.payload.url}"]`);
        if (relayItem) {
            const statusElement = relayItem.querySelector('.relay-status');
            if (statusElement) {
                // Remove all status classes
                statusElement.classList.remove('connected', 'connecting', 'disconnected', 'pending', 'initialized', 'terminated', 'banned');
                // Add the new status class
                statusElement.classList.add(evt.payload.status);
                // Update the text
                statusElement.textContent = evt.payload.status;
            }
        }
    });
}

/**
 * A flag that indicates when Vector is still in it's initiation sequence
 */
let fInit = true;

/**
 * Renders the relay list in the Settings Network section
 */
async function renderRelayList() {
    try {
        const relays = await invoke('get_relays');
        const networkList = document.getElementById('network-list');
        
        // Clear existing content
        networkList.innerHTML = '';
        
        // Create relay items
        relays.forEach(relay => {
            const relayItem = document.createElement('div');
            relayItem.className = 'relay-item';
            relayItem.setAttribute('data-relay-url', relay.url);
            
            const relayUrl = document.createElement('span');
            relayUrl.className = 'relay-url';
            relayUrl.textContent = relay.url;
            
            const relayStatus = document.createElement('span');
            relayStatus.className = `relay-status ${relay.status}`;
            relayStatus.textContent = relay.status;
            
            relayItem.appendChild(relayUrl);
            relayItem.appendChild(relayStatus);
            networkList.appendChild(relayItem);
        });
    } catch (error) {
        console.error('Failed to fetch relays:', error);
    }
}

/**
 * Login to the Nostr network
 */
async function login() {
    if (strPubkey) {
        // Connect to Nostr
        await invoke("connect");

        // Warmup our Upload Servers
        warmupUploadServers();

        // Setup our Rust Event listeners for efficient back<-->front sync
        await setupRustListeners();

        // Setup unified progress operation event listener
        await listen('progress_operation', (evt) => {
            const { type, current, total, message } = evt.payload;
            
            switch (type) {
                case 'start':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.add('text-gradient');
                    domLoginEncryptTitle.style.color = '';
                    break;
                    
                case 'progress':
                    if (current && total) {
                        const progress = Math.round((current / total) * 100);
                        domLoginEncryptTitle.textContent = `${message} (${progress}%)`;
                    } else {
                        domLoginEncryptTitle.textContent = message;
                    }
                    break;
                    
                case 'complete':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('text-gradient');
                    break;
                    
                case 'error':
                    domLoginEncryptTitle.textContent = message;
                    domLoginEncryptTitle.classList.remove('text-gradient');
                    domLoginEncryptTitle.style.color = 'red';
                    break;
            }
        });

        // Setup a Rust Listener for the backend's init finish
        await listen('init_finished', (evt) => {
            // The backend now sends both profiles (without messages) and chats (with messages)
            arrProfiles = evt.payload.profiles || [];
            arrChats = evt.payload.chats || [];

            // Fadeout the login and encryption UI
            domLogin.classList.add('fadeout-anim');
            domLogin.addEventListener('animationend', async () => {
                domLogin.classList.remove('fadeout-anim');
                domLoginInput.value = "";
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';

                // Fade-in the navbar
                domNavbar.style.display = '';
                domNavbar.classList.add('fadein-anim');
                domNavbar.addEventListener('animationend', () => {
                    domNavbar.classList.remove('fadein-anim');

                    // Fade-in the bookmarks icon
                    domChatBookmarksBtn.style.display = 'flex';
                    domChatBookmarksBtn.classList.add('fadein-anim');
                    domChatBookmarksBtn.addEventListener('animationend', () => domChatBookmarksBtn.classList.remove('fadein-anim'), { once: true });
                }, { once: true });

                // Render our profile with an intro animation
                const cProfile = arrProfiles.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.style.display = ``;
                domAccount.classList.add('fadein-anim');
                domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                // Display our Synchronisation Status
                domSyncStatusContainer.classList.add('intro-anim');
                domSyncStatusContainer.addEventListener('animationend', () => domSyncStatusContainer.classList.remove('intro-anim'), { once: true });
                if (domSyncStatus.textContent) domSyncStatusContainer.style.display = ``;

                // Finished boot!
                fInit = false;

                // Render the chatlist with an intro animation
                domChatList.classList.add('intro-anim');
                renderChatlist();
                domChatList.addEventListener('animationend', () => domChatList.classList.remove('intro-anim'), { once: true });

                // Show and animate the New Chat buttons
                if (domChatNewDM) {
                    domChatNewDM.style.display = '';
                    domChatNewDM.classList.add('intro-anim');
                    domChatNewDM.onclick = openNewChat;
                    domChatNewDM.addEventListener('animationend', () => domChatNewDM.classList.remove('intro-anim'), { once: true });
                }
                if (domChatNewGroup) {
                    domChatNewGroup.style.display = '';
                    domChatNewGroup.classList.add('intro-anim');
                    domChatNewGroup.onclick = openCreateGroup;
                    domChatNewGroup.addEventListener('animationend', () => domChatNewGroup.classList.remove('intro-anim'), { once: true });
                }

                // Setup a subscription for new websocket messages
                invoke("notifs");

                // Setup our Unread Counters
                await invoke("update_unread_counter");

                // Monitor relay connections
                invoke("monitor_relay_connections");

                // Render the initial relay list
                renderRelayList();
                
                // Initialize the updater
                initializeUpdater();
            }, { once: true });
        });

        // Load and Decrypt our database; fetching the full chat state from disk for immediate bootup
        domLoginEncryptTitle.textContent = `Decrypting Database...`;

        // Note: this also begins the Rust backend's iterative sync, thus, init should ONLY be called once, to initiate it
        init();
    }
}

/**
 * Renders the user's own profile UI in the chat list
 * @param {Profile} cProfile 
 */
function renderCurrentProfile(cProfile) {
    /* Chatlist Tab */

    // Reset any existing UI
    domAccount.innerHTML = ``;

    // Create the 'Name + Avatar' row
    const divRow = document.createElement('div');
    divRow.classList.add('row');

    // Render our avatar (if we have one)
    let domAvatar;
    if (cProfile?.avatar) {
        domAvatar = document.createElement('img');
        domAvatar.src = cProfile.avatar;
    } else {
        // Display our Gradient Avatar
        domAvatar = pubkeyToAvatar(strPubkey, cProfile?.nickname || cProfile?.name, 50);
    }
    domAvatar.classList.add('btn');
    domAvatar.onclick = () => openProfile();
    divRow.appendChild(domAvatar);

    // Render our Display Name and npub
    const h2DisplayName = document.createElement('h2');
    h2DisplayName.textContent = cProfile?.nickname || cProfile?.name || strPubkey.substring(0, 10) + '…';
    h2DisplayName.classList.add('btn', 'cutoff');
    h2DisplayName.style.fontFamily = `Rubik`;
    h2DisplayName.style.marginTop = `auto`;
    h2DisplayName.style.marginBottom = `auto`;
    h2DisplayName.style.maxWidth = `calc(100% - 150px)`;
    h2DisplayName.onclick = () => openProfile();
    if (cProfile?.nickname || cProfile?.name) twemojify(h2DisplayName);
    divRow.appendChild(h2DisplayName);

    // Add the username row
    domAccount.appendChild(divRow);

    // Render our status
    const pStatus = document.createElement('p');
    pStatus.textContent = cProfile?.status?.title || 'Set a Status';
    pStatus.classList.add('status', 'btn', 'cutoff', 'chat-contact-status');
    pStatus.onclick = askForStatus;
    twemojify(pStatus);
    domAccount.appendChild(pStatus);

    /* Start Chat Tab */
    // Render our Share npub
    domShareNpub.textContent = strPubkey;
}

/**
 * Render the Profile tab based on a given profile
 * @param {Profile} cProfile 
 */
function renderProfileTab(cProfile) {
    // Display Name
    domProfileName.innerHTML = cProfile?.nickname || cProfile?.name || strPubkey.substring(0, 10) + '…';
    if (cProfile?.nickname || cProfile?.name) twemojify(domProfileName);

    // Status
    const strStatusPlaceholder = cProfile.mine ? 'Set a Status' : '';
    domProfileStatus.innerHTML = cProfile?.status?.title || strStatusPlaceholder;
    if (cProfile?.status?.title) twemojify(domProfileStatus);

    // Adjust our Profile Name class to manage space according to Status visibility
    domProfileName.classList.toggle('chat-contact', !domProfileStatus.textContent);
    domProfileName.classList.toggle('chat-contact-with-status', !!domProfileStatus.textContent);

    // Banner - keep original structure but add click handler
    if (cProfile.banner) {
        if (domProfileBanner.tagName === 'DIV') {
            const newBanner = document.createElement('img');
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
        domProfileBanner.src = cProfile.banner;
    } else {
        if (domProfileBanner.tagName === 'IMG') {
            const newBanner = document.createElement('div');
            newBanner.style.backgroundColor = 'rgb(27, 27, 27)';
            domProfileBanner.replaceWith(newBanner);
            domProfileBanner = newBanner;
        }
    }
    domProfileBanner.classList.add('profile-banner');
    domProfileBanner.onclick = cProfile.mine ? askForBanner : null;
    if (cProfile.mine) domProfileBanner.classList.add('btn');

    // Avatar - keep original structure but add click handler
    if (cProfile.avatar) {
        if (domProfileAvatar.tagName === 'DIV') {
            const newAvatar = document.createElement('img');
            domProfileAvatar.replaceWith(newAvatar);
            domProfileAvatar = newAvatar;
        }
        domProfileAvatar.src = cProfile.avatar;
    } else {
        const newAvatar = pubkeyToAvatar(strPubkey, cProfile?.nickname || cProfile?.name, 175);
        domProfileAvatar.replaceWith(newAvatar);
        domProfileAvatar = newAvatar;
    }
    domProfileAvatar.classList.add('profile-avatar');
    domProfileAvatar.onclick = cProfile.mine ? askForAvatar : null;
    if (cProfile.mine) domProfileAvatar.classList.add('btn');

    // Secondary Display Name
    const strNamePlaceholder = cProfile.mine ? 'Set a Display Name' : '';
    domProfileNameSecondary.innerHTML = cProfile?.nickname || cProfile?.name || strNamePlaceholder;
    if (cProfile?.nickname || cProfile?.name) twemojify(domProfileNameSecondary);

    // Secondary Status
    domProfileStatusSecondary.innerHTML = domProfileStatus.innerHTML;

    // Badges
    domProfileBadgeInvite.style.display = 'none';
    invoke("get_invited_users", { npub: cProfile.id }).then(count => {
        if (count > 0) {
            domProfileBadgeInvite.style.display = '';
            domProfileBadgeInvite.onclick = () => {
                popupConfirm('Vector Beta Inviter', `${cProfile.mine ? 'You' : 'They' } have invited <b>${count} ${count === 1 ? 'user' : 'users'}</b> to the Vector Beta!`, true, '', 'vector_badge_placeholder.svg');
            }
        }
    }).catch(e => {});

    // npub display
    const profileNpub = document.getElementById('profile-npub');
    if (profileNpub) {
        profileNpub.textContent = cProfile.id;
    }

    // Description
    const strDescriptionPlaceholder = cProfile.mine ? (cProfile?.about || 'Set an About Me') : '';
    domProfileDescription.textContent = cProfile?.about || strDescriptionPlaceholder;
    twemojify(domProfileDescription);

    // npub
    domProfileId.textContent = cProfile.id;

    // Add npub copy functionality
    document.getElementById('profile-npub-copy')?.addEventListener('click', (e) => {
        const npub = document.getElementById('profile-npub')?.textContent;
        if (npub) {
            navigator.clipboard.writeText(npub).then(() => {
                const copyBtn = e.target.closest('.profile-npub-copy');
                if (copyBtn) {
                    copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                    setTimeout(() => {
                        copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
                    }, 2000);
                }
            });
        }
    });

    // If this is OUR profile: make the elements clickable, hide the "Contact Options"
    if (cProfile.mine) {
        // Hide Contact Options
        domProfileOptions.style.display = 'none';

        // Show edit buttons and set their click handlers
        document.querySelector('.profile-avatar-edit').style.display = 'flex';
        document.querySelector('.profile-avatar-edit').onclick = askForAvatar;
        
        document.querySelector('.profile-banner-edit').style.display = 'flex';
        document.querySelector('.profile-banner-edit').onclick = askForBanner;
        
        // Hide the 'Back' button and deregister its clickable function
        domProfileBackBtn.style.display = 'none';
        domProfileBackBtn.onclick = null;

        // Force banner on profile edit screen
        domProfileBanner.backgroundColor = 'rgb(27, 27, 27)';
        domProfileBanner.height = '';
        
        // Display the Navbar
        domNavbar.style.display = '';

        // Configure other clickables
        domProfileName.onclick = askForUsername;
        domProfileName.classList.add('btn');
        domProfileStatus.onclick = askForStatus;
        domProfileStatus.classList.add('btn');
        domProfileNameSecondary.onclick = askForUsername;
        domProfileNameSecondary.classList.add('btn');
        domProfileStatusSecondary.onclick = askForStatus;
        domProfileStatusSecondary.classList.add('btn');
        domProfileDescription.onclick = editProfileDescription;
        domProfileDescription.classList.add('btn');
    } else {
        // Show Contact Options
        domProfileOptions.style.display = '';

        // Setup Mute option
        domProfileOptionMute.querySelector('span').classList.replace('icon-volume-' + (cProfile.muted ? 'max' : 'mute'), 'icon-volume-' + (cProfile.muted ? 'mute' : 'max'));
        domProfileOptionMute.querySelector('p').innerText = cProfile.muted ? 'Unmute' : 'Mute';
        domProfileOptionMute.onclick = () => invoke('toggle_muted', { npub: cProfile.id });

        // Setup Nickname option
        domProfileOptionNickname.onclick = async () => {
            const nick = await popupConfirm('Choose a Nickname', '', false, 'Nickname');
            // Check if they cancelled the nicknaming (resetting a nickname with an empty '' result is fine, though)
            if (nick === false) return;
            // Ensure it's not massive
            if (nick.length >= 30) return popupConfirm('Woah woah!', 'A ' + nick.length + '-character nickname seems excessive!', true, '', 'vector_warning.svg');
            await invoke('set_nickname', { npub: cProfile.id, nickname: nick });
        }

        // Hide edit buttons
        document.querySelector('.profile-avatar-edit').style.display = 'none';
        document.querySelector('.profile-banner-edit').style.display = 'none';
        
        // Remove click handlers from avatar and banner
        domProfileAvatar.onclick = null;
        domProfileAvatar.classList.remove('btn');
        domProfileBanner.onclick = null;
        domProfileBanner.classList.remove('btn');
        if (!cProfile.banner) {
            domProfileBanner.style.backgroundColor = '';
            domProfileBanner.style.height = '115px';
        } else {
            domProfileBanner.style.backgroundColor = 'rgb(27, 27, 27)';
            domProfileBanner.style.height = '';
        }
        
        // Show the 'Back' button and link it to the profile's chat
        domProfileBackBtn.style.display = '';
        domProfileBackBtn.onclick = () => openChat(cProfile.id);
        
        // Hide the Navbar
        domNavbar.style.display = 'none';

        // Remove other clickables
        domProfileName.onclick = null;
        domProfileName.classList.remove('btn');
        domProfileStatus.onclick = null;
        domProfileStatus.classList.remove('btn');
        domProfileNameSecondary.onclick = null;
        domProfileNameSecondary.classList.remove('btn');
        domProfileStatusSecondary.onclick = null;
        domProfileStatusSecondary.classList.remove('btn');
        domProfileDescription.onclick = null;
        domProfileDescription.classList.remove('btn');
    }
}

/**
 * Display the Invite code input flow.
 * @param {string} pkey - A private key to encrypt.
 */
function openInviteFlow(pkey) {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = '';
    
    // Focus on the invite input
    domInviteInput.focus();
    
    // Handle invite code submission
    domInviteBtn.onclick = async () => {
        const inviteCode = domInviteInput.value.trim();
        if (!inviteCode) {
            return popupConfirm('Please enter an invite code', '', true, '', 'vector_warning.svg');
        }
        
        try {
            // Accept the invite code
            await invoke('accept_invite_code', { inviteCode });
            
            // Hide invite screen and show welcome screen
            domLoginInvite.style.display = 'none';
            showWelcomeScreen(pkey);
        } catch (e) {
            // Display the specific error from the backend
            const errorMessage = e.toString() || 'Please check your invite code and try again.';
            popupConfirm('Invalid invite code', errorMessage, true, '', 'vector_warning.svg');
        }
    };
    
    // Handle enter key on invite input
    domInviteInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' || evt.code === 'NumpadEnter') {
            evt.preventDefault();
            domInviteBtn.click();
        }
    };
    
    // Handle enter key on invite input
    domInviteInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' || evt.code === 'NumpadEnter') {
            evt.preventDefault();
            domInviteBtn.click();
        }
    };
}

/**
 * Display the welcome screen after successful invite code acceptance
 * @param {string} pkey - A private key to encrypt after the welcome screen
 */
function showWelcomeScreen(pkey) {
    // Hide the logo and subtext
    const domLogo = document.querySelector('.login-logo');
    const domSubtext = document.querySelector('.login-subtext');
    domLogo.style.display = 'none';
    domSubtext.style.display = 'none';
    
    // Show the welcome screen
    domLoginWelcome.style.display = '';
    
    // After 5 seconds, transition to the encryption flow
    setTimeout(() => {
        domLoginWelcome.style.display = 'none';
        // Restore the logo and subtext
        domLogo.style.display = '';
        domSubtext.style.display = '';
        openEncryptionFlow(pkey, false);
    }, 5000);
}

/**
 * Display the Encryption/Decryption flow.
 * @param {string} pkey - A private key to encrypt.
 * @param {boolean} fUnlock - Whether we're unlocking an existing key, or encrypting the given one.
 */
function openEncryptionFlow(pkey, fUnlock = false) {
    domLoginStart.style.display = 'none';
    domLoginImport.style.display = 'none';
    domLoginInvite.style.display = 'none';
    domLoginEncrypt.style.display = '';

    let strPinLast = []; // Stores the first entered PIN for confirmation
    let strPinCurrent = Array(6).fill('-'); // Current PIN being entered, '-' represents an empty digit

    // Reusable Message Constants
    const DECRYPTION_PROMPT = `Enter your Decryption Pin`;
    const INITIAL_ENCRYPTION_PROMPT = `Enter your Pin`;
    const RE_ENTER_PROMPT = `Re-enter your Pin`;
    const DECRYPTING_MSG = `Decrypting your keys...`;
    const ENCRYPTING_MSG = `Encrypting your keys...`;
    const INCORRECT_PIN_MSG = `Incorrect pin, try again`;
    const MISMATCH_PIN_MSG = `Pin doesn't match, re-try`;

    const arrPinDOMs = document.querySelectorAll('.pin-row input');
    const pinContainer = arrPinDOMs[0].closest('.pin-row');

    /** Updates the status message displayed to the user. */
    function updateStatusMessage(message, isProcessing = false) {
        domLoginEncryptTitle.textContent = message;
        if (isProcessing) {
            domLoginEncryptTitle.classList.add('startup-subtext-gradient');
            domLoginEncryptPinRow.style.display = 'none'; // Hide PIN inputs during processing
        } else {
            domLoginEncryptTitle.classList.remove('startup-subtext-gradient');
            domLoginEncryptPinRow.style.display = ''; // Ensure PIN inputs are visible
        }
    }

    /** Resets the PIN input fields and optionally reverts the title from an error state. */
    function resetPinDisplay(focusFirst = true, revertTitleFromErrorState = true) {
        strPinCurrent = Array(6).fill('-');
        arrPinDOMs.forEach(input => input.value = '');

        if (revertTitleFromErrorState) {
            const currentTitle = domLoginEncryptTitle.textContent;
            // If an error message is shown, change it back to the appropriate prompt
            if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                updateStatusMessage(newTitle);
            }
        }
        if (focusFirst && arrPinDOMs.length > 0) {
            arrPinDOMs[0].focus();
        }
    }

    /** Focuses the PIN input at the specified index. */
    function focusPinInput(index) {
        if (index >= 0 && index < arrPinDOMs.length) {
            arrPinDOMs[index].focus();
        } else if (index >= arrPinDOMs.length && arrPinDOMs.length > 0) { // Wrap to first on last input
            arrPinDOMs[0].focus(); // Reached end, focus first (or handle submission if all filled)
        }
        // If index < 0 (e.g., backspace from the first input), focus remains on the current (first) input.
    }

    /** Handles the logic once all PIN digits have been entered. */
    async function handleFullPinEntered() {
        const currentPinString = strPinCurrent.join('');

        if (strPinLast.length === 0) { // Initial PIN entry (for decryption or first step of new encryption)
            if (fUnlock) {
                updateStatusMessage(DECRYPTING_MSG, true);
                try {
                    const decryptedPkey = await loadAndDecryptPrivateKey(currentPinString);
                    const { public: pubKey /*, _private: privKey */ } = await invoke("login", { importKey: decryptedPkey });
                    strPubkey = pubKey; // Store public key
                    login(); // Proceed to login
                } catch (e) {
                    updateStatusMessage(INCORRECT_PIN_MSG);
                    resetPinDisplay(true, false); // Keep error message, reset input fields
                }
            } else { // First PIN entry for new encryption
                strPinLast = [...strPinCurrent]; // Store the entered PIN
                updateStatusMessage(RE_ENTER_PROMPT);
                resetPinDisplay(true, false); // Keep "Re-enter" message, reset input fields
            }
        } else { // Second PIN entry (confirmation for new encryption)
            const isMatching = strPinLast.every((char, idx) => char === strPinCurrent[idx]);
            if (isMatching) {
                updateStatusMessage(ENCRYPTING_MSG, true);
                await saveAndEncryptPrivateKey(pkey, strPinLast.join(''));
                login(); // Proceed to login
            } else {
                updateStatusMessage(MISMATCH_PIN_MSG);
                strPinLast = []; // Clear the stored first PIN, requiring user to start over
                resetPinDisplay(true, true); // Reset inputs and revert title from error to the initial prompt
            }
        }
    }

    // --- Event Handlers (Delegated to pinContainer) ---

    /** Handles keydown events, primarily for Backspace and preventing non-numeric input. */
    function handleKeyDown(event) {
        const targetInput = event.target;
        // Ensure the event target is one of our designated PIN input fields
        if (!Array.from(arrPinDOMs).includes(targetInput)) {
            return;
        }

        const nIndex = Array.from(arrPinDOMs).indexOf(targetInput);

        if (event.key === 'Backspace') {
            event.preventDefault(); // Prevent default browser backspace behavior (e.g., navigation)

            // If an error message is currently displayed, revert it to the relevant prompt for clarity
            const currentTitle = domLoginEncryptTitle.textContent;
            if (currentTitle === INCORRECT_PIN_MSG || currentTitle === MISMATCH_PIN_MSG) {
                const newTitle = fUnlock ? DECRYPTION_PROMPT : (strPinLast.length > 0 ? RE_ENTER_PROMPT : INITIAL_ENCRYPTION_PROMPT);
                updateStatusMessage(newTitle);
            }

            targetInput.value = ''; // Clear the input field's value
            strPinCurrent[nIndex] = '-'; // Update the current PIN state
            if (nIndex > 0) {
                focusPinInput(nIndex - 1); // Move focus to the previous input field
            }
        } else if (event.key.length === 1 && !event.key.match(/^[0-9]$/)) {
            // Prevent single character non-numeric keys (allows Tab, Shift, Ctrl, Meta, etc.)
            event.preventDefault();
        }
    }

    /** Handles input events for digit entry, sanitization, and moving focus forward. */
    async function handleInput(event) {
        const targetInput = event.target;
        if (!Array.from(arrPinDOMs).includes(targetInput)) {
            return;
        }

        const nIndex = Array.from(arrPinDOMs).indexOf(targetInput);
        let sanitizedValue = targetInput.value.replace(/[^0-9]/g, ''); // Keep only digits

        if (sanitizedValue.length > 1) { // If multiple digits were pasted, use only the first
            sanitizedValue = sanitizedValue.charAt(0);
        }
        targetInput.value = sanitizedValue; // Update the input field with the sanitized value

        if (sanitizedValue) { // If there's a digit
            strPinCurrent[nIndex] = sanitizedValue;
            focusPinInput(nIndex + 1); // Move focus to the next input field or wrap around
        } else {
            // If input became empty (e.g., via 'Delete' key or invalid paste), update state
            strPinCurrent[nIndex] = '-';
        }

        // Check if all PIN digits have been entered
        if (!strPinCurrent.includes('-')) {
            await handleFullPinEntered();
        }
    }

    // --- Initial Setup ---
    updateStatusMessage(fUnlock ? DECRYPTION_PROMPT : INITIAL_ENCRYPTION_PROMPT);
    resetPinDisplay(true, false); // Ensure inputs are clear, set focus, keep initial message

    // Attach the event listeners to the common parent container
    pinContainer.addEventListener('keydown', handleKeyDown);
    pinContainer.addEventListener('input', handleInput);
}



/**
 * A simple state tracker for the last message ID, if it changes, we auto-scroll
 */
let strLastMsgID = "";

/**
 * The current Message ID being replied to
 */
let strCurrentReplyReference = "";

/**
 * Updates the current chat (to display incoming and outgoing messages)
 * @param {Chat} chat - The chat to update
 * @param {Array<Message>} arrMessages - The messages to efficiently insert into the chat
 * @param {Profile} profile - Optional profile for display info
 * @param {boolean} fClicked - Whether the chat was opened manually or not
 */
async function updateChat(chat, arrMessages = [], profile = null, fClicked = false) {
    // Check if this is a group chat
    const isGroup = chat?.chat_type === 'MlsGroup';

    // If no profile is provided and it's not a group, try to get it from the chat ID
    if (!profile && chat && !isGroup) {
        profile = getProfile(chat.id);
    }
    
    // If this chat is our own npub: then we consider this our Bookmarks/Notes section
    const fNotes = strOpenChat === strPubkey;

    if (chat?.messages.length || arrMessages.length) {
        // Prefer displaying their name, otherwise, npub/group name
        if (fNotes) {
            domChatContact.textContent = 'Notes';
            domChatContact.classList.remove('btn');
        } else if (isGroup) {
            domChatContact.textContent = chat.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
            domChatContact.classList.remove('btn');
        } else {
            domChatContact.textContent = profile?.nickname || profile?.name || strOpenChat.substring(0, 10) + '…';
            if (profile?.nickname || profile?.name) twemojify(domChatContact);
            // When the name or status is clicked, expand their Profile
            domChatContact.onclick = () => {
                closeChat();
                openProfile(profile);
            };
            domChatContact.classList.add('btn');
        }

        // Display either their Status or Typing Indicator
        updateChatHeaderSubtext(chat);

        // Adjust our Contact Name class to manage space according to Status visibility
        domChatContact.classList.toggle('chat-contact', !domChatContactStatus.textContent);
        domChatContact.classList.toggle('chat-contact-with-status', !!domChatContactStatus.textContent);
        domChatContactStatus.style.display = !domChatContactStatus.textContent ? 'none' : '';

        // Auto-mark messages as read when chat is opened AND window is focused
        if (chat?.messages?.length) {
            // Check window focus before auto-marking
            const isWindowFocused = (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios')
                ? await getCurrentWindow().isFocused()
                : true;
            
            if (isWindowFocused) {
                // Find the latest message from the other person (not from current user)
                let lastContactMsg = null;
                for (let i = chat.messages.length - 1; i >= 0; i--) {
                    if (!chat.messages[i].mine) {
                        lastContactMsg = chat.messages[i];
                        break;
                    }
                }
                
                // If we found a message and it's not already marked as read, update the read status
                if (lastContactMsg && chat.last_read !== lastContactMsg.id) {
                    // Update the chat's last_read
                    chat.last_read = lastContactMsg.id;
                    markAsRead(chat, lastContactMsg);
                }
            }
        }

        if (!arrMessages.length) return;

        // Track last message time for timestamp insertion
        let nLastMsgTime = null;

        /* Dedup guard: skip any message already present in the DOM by ID */
         // Process each message for insertion
        for (const msg of arrMessages) {
            // Guard against duplicate insertions if the DOM already contains this message ID
            if (document.getElementById(msg.id)) {
                continue;
            }
            // Quick check for empty chat - simple append
            if (domChatMessages.children.length === 0) {
                domChatMessages.appendChild(renderMessage(msg, profile));
                continue;
            }

            // Ensure there's no more than 50 existing messages at max
            if (domChatMessages.childElementCount >= 50) {
                domChatMessages.firstElementChild.remove();
            }

            // Direct comparison with newest and oldest messages (most common cases)
            // This avoids expensive DOM operations for the common cases

            // Get the newest message in the DOM
            const newestMsgElement = domChatMessages.lastElementChild;
            const newestMsg = chat.messages.find(m => m.id === newestMsgElement.id);
            if (newestMsg && msg.at > newestMsg.at) {
                // It's the newest message, append it

                // Add timestamp if needed
                if (nLastMsgTime === null) {
                    nLastMsgTime = newestMsg.at;
                }

                if (msg.at - nLastMsgTime > 600 * 1000) {
                    insertTimestamp(msg.at, domChatMessages);
                    nLastMsgTime = msg.at;
                }

                // Render message post-time-insert for improved message rendering context
                const domMsg = renderMessage(msg, profile);
                if (!msg.mine && arrMessages.length === 1) {
                    domMsg.classList.add('new-anim');
                    domMsg.addEventListener('animationend', () => {
                        // Remove the animation class once it finishes
                        domMsg?.classList?.remove('new-anim');
                    }, { once: true });
                }

                domChatMessages.appendChild(domMsg);

                // If this was our pending message, then snap the view to the bottom
                if (msg.mine && msg.pending) scrollToBottom(domChatMessages, false);
                continue;
            }

            // Get the oldest message in the DOM
            let oldestMsgElement = null;
            for (let i = 0; i < domChatMessages.children.length; i++) {
                const child = domChatMessages.children[i];
                if (child.getAttribute('sender')) {
                    oldestMsgElement = child;
                    break;
                }
            }

            if (oldestMsgElement) {
                const oldestMsg = chat.messages.find(m => m.id === oldestMsgElement.id);
                if (oldestMsg && msg.at < oldestMsg.at) {
                    // It's the oldest message, prepend it
                    const domMsg = renderMessage(msg, profile);
                    domChatMessages.insertBefore(domMsg, oldestMsgElement);
                    continue;
                }
            }

            // If we get here, the message belongs somewhere in the middle
            // This is a less common case, so we'll do a linear scan
            let inserted = false;

            // Get the message elements sorted by time (oldest to newest)
            // We'll do a linear scan since we expect this to be rare and the chat isn't likely huge
            let messageNodes = [];
            for (let i = 0; i < domChatMessages.children.length; i++) {
                const child = domChatMessages.children[i];
                if (child.id && child.getAttribute('sender')) {
                    const childMsg = chat.messages.find(m => m.id === child.id);
                    if (childMsg) {
                        messageNodes.push({ element: child, message: childMsg });
                    }
                }
            }

            // Sort by timestamp if needed (they might not be in order in the DOM)
            messageNodes.sort((a, b) => a.message.at - b.message.at);

            // Find the correct position to insert
            for (let i = 0; i < messageNodes.length - 1; i++) {
                const currentNode = messageNodes[i];
                const nextNode = messageNodes[i + 1];

                if (currentNode.message.at <= msg.at && msg.at <= nextNode.message.at) {
                    // Add timestamp if needed
                    if (msg.at - currentNode.message.at > 600 * 1000) {
                        const timestamp = insertTimestamp(msg.at);
                        domChatMessages.insertBefore(timestamp, nextNode.element);
                    }

                    // Insert between these two messages
                    const domMsg = renderMessage(msg, profile);
                    domChatMessages.insertBefore(domMsg, nextNode.element);
                    inserted = true;
                    break;
                }
            }

            // If somehow not inserted by the above logic, append as fallback
            if (!inserted) {
                // Check if we need a timestamp
                const lastMsg = messageNodes[messageNodes.length - 1]?.message;
                if (lastMsg && msg.at - lastMsg.at > 600 * 1000) {
                    insertTimestamp(msg.at, domChatMessages);
                }

                const domMsg = renderMessage(msg, profile);
                domChatMessages.appendChild(domMsg);
            }
        }

        // Auto-scroll on new messages (if the user hasn't scrolled up, or on manual chat open)
        const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
        if (pxFromBottom < 500 || fClicked) {
            const cLastMsg = chat.messages[chat.messages.length - 1];
            if (strLastMsgID !== cLastMsg.id || fClicked) {
                strLastMsgID = cLastMsg.id;
                adjustSize();
                // Force an auto-scroll, given soft-scrolling won't accurately work when the entire list has just rendered
                scrollToBottom(domChatMessages, false);
            }
        }
    } else {
        // Probably a 'New Chat', as such, we'll mostly render an empty chat
        if (fNotes) {
            domChatContact.textContent = 'Notes';
        } else if (isGroup) {
            domChatContact.textContent = chat?.metadata?.custom_fields?.name || `Group ${strOpenChat.substring(0, 10)}...`;
        } else {
            domChatContact.textContent = profile?.nickname || profile?.name || strOpenChat.substring(0, 10) + '…';
        }
        // There's no profile to render; so don't allow clicking them to expand it
        domChatContact.onclick = null;
        domChatContact.classList.remove('btn');

        // Force wipe the 'Status' and it's styling
        domChatContactStatus.textContent = fNotes ? domChatContactStatus.textContent = 'Encrypted Notes to Self' : '';
        if (!domChatContactStatus.textContent) {
            domChatContact.classList.add('chat-contact');
            domChatContact.classList.remove('chat-contact-with-status');
            domChatContactStatus.style.display = 'none';
        } else {
            domChatContact.classList.add('chat-contact-with-status');
            domChatContact.classList.remove('chat-contact');
            domChatContactStatus.style.display = '';
        }
    }

    adjustSize();
}

/**
 * Helper function to create and insert a timestamp
 * @param {number} timestamp - Unix timestamp in seconds
 * @param {HTMLElement} parent - Optional parent to append to
 * @returns {HTMLElement} - The created timestamp element
 */
function insertTimestamp(timestamp, parent = null) {
    const pTimestamp = document.createElement('p');
    pTimestamp.classList.add('msg-inline-timestamp');
    const messageDate = new Date(timestamp);

    // Render the time contextually
    if (isToday(messageDate)) {
        pTimestamp.textContent = messageDate.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true });
    } else if (isYesterday(messageDate)) {
        pTimestamp.textContent = `Yesterday, ${messageDate.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true })}`;
    } else {
        pTimestamp.textContent = messageDate.toLocaleString();
    }

    if (parent) {
        parent.appendChild(pTimestamp);
    }

    return pTimestamp;
}

/**
 * Convert a Message in to a rendered HTML Element
 * @param {Message} msg - the Message to be converted
 * @param {Profile} sender - the Profile of the message sender
 * @param {string?} editID - the ID of the message being edited, used for improved renderer context
 */
function renderMessage(msg, sender, editID = '') {
    // Construct the message container (the DOM ID is the HEX Nostr Event ID)
    const divMessage = document.createElement('div');
    divMessage.id = msg.id;

    // Add a subset of the sender's ID so we have context of WHO sent it, even in group contexts
    // For group chats, use msg.npub; for DMs, use sender.id
    const otherId = sender?.id || msg.npub || '';
    const strShortSenderID = (msg.mine ? strPubkey : otherId).substring(0, 8);
    divMessage.setAttribute('sender', strShortSenderID);
    
    // Check if we're in a group chat
    const currentChat = arrChats.find(c => c.id === strOpenChat);
    const isGroupChat = currentChat?.chat_type === 'MlsGroup';

    // Render it appropriately depending on who sent it
    divMessage.classList.add('msg-' + (msg.mine ? 'me' : 'them'));

    // Prepare the message container
    const pMessage = document.createElement('p');

    // Prepare our message container - including avatars and contextual bubble rendering
    const domPrevMsg = editID ? document.getElementById(editID).previousElementSibling : domChatMessages.lastElementChild;
    const fIsMsg = !!domPrevMsg?.getAttribute('sender');
    if (!domPrevMsg || domPrevMsg.getAttribute('sender') != strShortSenderID) {
        // Add an avatar if this is not OUR message
        if (!msg.mine) {
            let avatarEl = null;
            // Resolve sender profile for group chats
            // For group chats, use msg.npub; for DMs, use sender
            const otherFullId = msg.npub || sender?.id || '';
            const authorProfile = sender || (otherFullId ? getProfile(otherFullId) : null);
            if (authorProfile?.avatar) {
                const imgAvatar = document.createElement('img');
                imgAvatar.classList.add('avatar', 'btn');
                imgAvatar.onclick = () => {
                    closeChat();
                    openProfile(authorProfile);
                };
                imgAvatar.src = authorProfile.avatar;
                avatarEl = imgAvatar;
            } else {
                // Provide a deterministic placeholder when no avatar URL is available
                const displayName = authorProfile?.nickname || authorProfile?.name || '';
                const placeholder = pubkeyToAvatar(otherFullId, displayName, 35);
                // Ensure visual sizing and interactivity match real avatars
                placeholder.classList.add('avatar', 'btn');
                // Only wire profile click if we have an identifiable user
                if (otherFullId) {
                    placeholder.onclick = () => {
                        const prof = getProfile(otherFullId) || authorProfile;
                        closeChat();
                        openProfile(prof || { id: otherFullId });
                    };
                }
                avatarEl = placeholder;
            }
            
            // Create a container for avatar and username
            if (avatarEl) {
                const avatarContainer = document.createElement('div');
                avatarContainer.style.position = 'relative';
                avatarContainer.style.marginRight = '10px';
                
                // Only add username label in group chats
                if (isGroupChat) {
                    const usernameLabel = document.createElement('span');
                    usernameLabel.classList.add('msg-username-label', 'btn');
                    const displayName = authorProfile?.nickname || authorProfile?.name || '';
                    usernameLabel.textContent = displayName || otherFullId.substring(0, 8);
                    if (displayName) twemojify(usernameLabel);
                    
                    // Make username clickable to open profile
                    if (otherFullId) {
                        usernameLabel.onclick = () => {
                            const prof = getProfile(otherFullId) || authorProfile;
                            closeChat();
                            openProfile(prof || { id: otherFullId });
                        };
                    }
                    
                    avatarContainer.appendChild(usernameLabel);
                }
                
                avatarContainer.appendChild(avatarEl);
                
                // Remove the margin from the avatar since container handles it
                avatarEl.style.marginRight = '0';
                divMessage.appendChild(avatarContainer);
            }
        }

        // If there is a message before them, and it isn't theirs, apply additional edits
        if (domPrevMsg && fIsMsg) {
            // Check if the previous message was from the contact (!mine) 
            const prevSenderID = domPrevMsg.getAttribute('sender');
            const wasPrevMsgFromContact = prevSenderID !== strPubkey.substring(0, 8);
            
            // Only curve the previous message's bottom-left border if it was from the user (mine)
            // If it was from the contact, we need to check if it should have a rounded corner (last in streak)
            if (!wasPrevMsgFromContact) {
                const pMsg = domPrevMsg.querySelector('p');
                if (pMsg) {
                    pMsg.style.borderBottomLeftRadius = `15px`;
                }
            } else {
                // The previous message was from the contact - check if it needs rounding as last in streak
                // Look back to see if it had previous messages from same sender
                const prevPrevMsg = domPrevMsg.previousElementSibling;
                const hadPreviousFromSameSender = prevPrevMsg && prevPrevMsg.getAttribute('sender') === prevSenderID;
                
                // Look forward to see if there are more messages from same sender after this one
                // (which would make this a middle message, not the last)
                const prevNextMsg = domPrevMsg.nextElementSibling;
                const hasNextFromSameSender = prevNextMsg && prevNextMsg.getAttribute('sender') === prevSenderID;
                
                // Only round if it had previous messages AND no next messages from same sender (making it the last)
                if (hadPreviousFromSameSender && !hasNextFromSameSender) {
                    const pMsg = domPrevMsg.querySelector('p');
                    if (pMsg && !pMsg.classList.contains('no-background')) {
                        pMsg.style.borderBottomLeftRadius = `15px`;
                    }
                }
            }

            // Add some additional margin to separate the senders visually (extra space for username in groups)
            divMessage.style.marginTop = isGroupChat ? `30px` : `15px`;
        }
        
        // Check if this is a singular message (no next message from same sender)
        // This check happens after the message is rendered (at the end of the function)
    } else {
        // Add additional margin to simulate avatar space
        // We always reserve space for non-mine messages since we render an avatar or placeholder for the first in a streak
        if (!msg.mine) {
            pMessage.style.marginLeft = `45px`;
        }

        // Flatten the top border to act as a visual continuation
        const pMsg = domPrevMsg.querySelector('p');
        if (pMsg) {
            if (msg.mine) {
                pMessage.style.borderTopRightRadius = `0`;
            } else {
                pMessage.style.borderTopLeftRadius = `0`;
            }
        }
    }

    // If we're replying to this, give it a glowing border
    const fReplying = strCurrentReplyReference === msg.id;
    const strEmojiCleaned = msg.content.replace(/\s/g, '');
    const fEmojiOnly = isEmojiOnly(strEmojiCleaned) && strEmojiCleaned.length <= 6;
    if (fReplying) {
        // Only display if replying
        pMessage.style.borderColor = `#ffffff`;
    }

    // If it's a reply: inject a preview of the replied-to message, if we have knowledge of it
    if (msg.replied_to) {
        // Try to find the referenced message in the current chat
        // For DMs, use sender profile; for groups, use the currently open chat
        const chat = sender ? getDMChat(sender.id) : arrChats.find(c => c.id === strOpenChat);
        const cMsg = chat?.messages.find(m => m.id === msg.replied_to);
        if (cMsg) {
            // Render the reply in a quote-like fashion
            const divRef = document.createElement('div');
            divRef.classList.add('msg-reply', 'btn');
            divRef.id = `r-${cMsg.id}`;

            // Name + Message
            const spanName = document.createElement('span');
            spanName.style.color = `rgba(255, 255, 255, 0.7)`;

            // Name - for group chats, use cMsg.npub; for DMs, use sender
            const cSenderProfile = !cMsg.mine
                ? (cMsg.npub ? getProfile(cMsg.npub) : sender)
                : getProfile(strPubkey);
            if (cSenderProfile?.nickname || cSenderProfile?.name) {
                spanName.textContent = cSenderProfile.nickname || cSenderProfile.name;
                twemojify(spanName);
            } else {
                const fallbackId = cMsg.npub || cSenderProfile?.id || '';
                spanName.textContent = fallbackId ? fallbackId.substring(0, 10) + '…' : 'Unknown';
            }

            // Replied-to content (Text or Attachment)
            let spanRef;
            if (cMsg.content) {
                spanRef = document.createElement('span');
                spanRef.style.color = `rgba(255, 255, 255, 0.45)`;
                spanRef.textContent = cMsg.content.length < 50 ? cMsg.content : cMsg.content.substring(0, 50) + '…';
                twemojify(spanRef);
            } else if (cMsg.attachments.length) {
                // For Attachments, we display an additional icon for quickly inferring the replied-to content
                spanRef = document.createElement('div');
                spanRef.style.display = `flex`;
                const cFileType = getFileTypeInfo(cMsg.attachments[0].extension);

                // Icon
                const spanIcon = document.createElement('span');
                spanIcon.classList.add('icon', 'icon-' + cFileType.icon);
                spanIcon.style.position = `relative`;
                spanIcon.style.backgroundColor = `rgba(255, 255, 255, 0.45)`;
                spanIcon.style.width = `18px`;
                spanIcon.style.height = `18px`;
                spanIcon.style.margin = `0px`;

                // Description
                const spanDesc = document.createElement('span');
                spanDesc.style.color = `rgba(255, 255, 255, 0.45)`;
                spanDesc.style.marginLeft = `5px`;
                spanDesc.textContent = cFileType.description;

                // Combine
                spanRef.append(spanIcon, spanDesc);
            }

            divRef.appendChild(spanName);
            divRef.appendChild(document.createElement('br'));
            divRef.appendChild(spanRef);
            pMessage.appendChild(divRef);
        }
    }

    // Render the text - if it's emoji-only and/or file-only, and less than four emojis, format them nicely
    const spanMessage = document.createElement('span');
    if (fEmojiOnly) {
        // Preserve linebreaks for creative emoji rendering (tophats on wolves)
        spanMessage.textContent = msg.content;
        spanMessage.style.whiteSpace = `pre-wrap`;
        // Add an emoji-only CSS format
        pMessage.classList.add('emoji-only');
        spanMessage.classList.add('emoji-only-content');
        // Align the emoji depending on who sent it
        spanMessage.style.textAlign = msg.mine ? 'right' : 'left';
    } else {
        // Render their text content (using our custom Markdown renderer)
        // NOTE: the input IS HTML-sanitised, however, heavy auditing of the sanitisation method should be done, it is a bit sketchy
        // NOTE: parseMarkdown internally protects URLs from being mangled by markdown formatting
        spanMessage.innerHTML = parseMarkdown(msg.content.trim());
        
        // Make URLs clickable (after markdown parsing, before twemojify)
        linkifyUrls(spanMessage);
    }

    // Twemojify!
    twemojify(spanMessage);

    // Append the message contents
    pMessage.appendChild(spanMessage);

    // Append attachments
    let strRevealAttachmentPath = '';
    if (msg.attachments.length) {
        // Float the content depending on who's it is
        pMessage.style.float = msg.mine ? 'right' : 'left';
        // Remove any message bubbles
        pMessage.classList.add('no-background');
    }
    for (const cAttachment of msg.attachments) {
        if (cAttachment.downloaded) {
            // Save the path for our File Explorer shortcut
            strRevealAttachmentPath = cAttachment.path;

            // Convert the absolute file path to a Tauri asset
            const assetUrl = convertFileSrc(cAttachment.path);

            // Render the attachment appropriately for it's type
            if (['png', 'jpeg', 'jpg', 'gif', 'webp', 'svg', 'bmp'].includes(cAttachment.extension)) {
                // Images
                const imgPreview = document.createElement('img');
                // SVGs need a specific width to scale properly
                if (cAttachment.extension === 'svg') {
                    imgPreview.style.width = `25vw`;
                } else {
                    imgPreview.style.maxWidth = `100%`;
                }
                imgPreview.style.height = `auto`;
                imgPreview.style.borderRadius = `8px`;
                imgPreview.src = assetUrl;
                // Add event listener for auto-scrolling within the first 100ms of chat opening
                imgPreview.addEventListener('load', () => {
                    // Auto-scroll if within 100ms of chat opening
                    if (chatOpenTimestamp && Date.now() - chatOpenTimestamp < 100) {
                        scrollToBottom(domChatMessages, false);
                    }
                    // Also do soft scroll for normal layout adjustments
                    softChatScroll();
                }, { once: true });
                pMessage.appendChild(imgPreview);
                } else if (['wav', 'mp3', 'flac', 'aac', 'm4a', 'ogg', 'opus'].includes(cAttachment.extension)) {
                // Audio
                handleAudioAttachment(cAttachment, assetUrl, pMessage, msg);
                } else if (['mp4', 'webm', 'mov'].includes(cAttachment.extension)) {
                // Videos
                const handleMetadataLoaded = (video) => {
                    // Seek a tiny amount to force the frame 'poster' to load
                    video.currentTime = 0.1;
                    
                    // Auto-scroll if within 100ms of chat opening
                    if (chatOpenTimestamp && Date.now() - chatOpenTimestamp < 100) {
                        scrollToBottom(domChatMessages, false);
                    }
                    // Also do soft scroll for normal layout adjustments
                    softChatScroll();
                };
                
                // Platform-specific video creation
                if (platformFeatures.os === 'android') {
                    // Android always uses blob method with size limit
                    createAndroidVideo(assetUrl, cAttachment, handleMetadataLoaded, (element) => {
                        pMessage.appendChild(element);
                    });
                } else {
                    // Standard video element for other platforms
                    const vidPreview = document.createElement('video');
                    vidPreview.setAttribute('controlsList', 'nodownload');
                    vidPreview.controls = true;
                    vidPreview.style.width = `100%`;
                    vidPreview.style.height = `auto`;
                    vidPreview.style.borderRadius = `8px`;
                    vidPreview.style.cursor = `pointer`;
                    vidPreview.preload = "metadata";
                    vidPreview.playsInline = true;
                    vidPreview.src = assetUrl;
                    
                    // Add metadata loaded handler
                    vidPreview.addEventListener('loadedmetadata', () => {
                        handleMetadataLoaded(vidPreview);
                    }, { once: true });
                    
                    pMessage.appendChild(vidPreview);
                }
            } else {
                // Unknown attachment
                const iUnknown = document.createElement('i');
                iUnknown.classList.add('text-gradient');
                iUnknown.textContent = `Previews not supported for "${cAttachment.extension}" files yet`;
                pMessage.appendChild(iUnknown);
            }

            // If the message is mine, and pending: display an uploading status
            if (msg.mine && msg.pending) {
                // Lower the attachment opacity
                pMessage.lastElementChild.style.opacity = 0.25;

                // Create the Progress Bar
                const divBar = document.createElement('div');
                divBar.id = msg.id + '_file';
                divBar.classList.add('progress-bar');
                divBar.style.width = `100%`;
                divBar.style.height = `5px`;
                divBar.style.marginTop = `0`;
                divBar.style.transitionDuration = `0.75s`;
                divBar.style.width = `0%`;
                pMessage.appendChild(divBar);
            }
        } else if (cAttachment.downloading) {
            // For images, show blurhash preview while downloading (only for formats that support blurhash)
            if (['png', 'jpeg', 'jpg', 'gif', 'webp'].includes(cAttachment.extension)) {
                // Generate blurhash preview for downloading image
                // For group chats, use chat ID; for DMs, use sender.id
                const blurhashNpub2 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                invoke('generate_blurhash_preview', { npub: blurhashNpub2, msgId: msg.id })
                    .then(base64Image => {
                        const imgPreview = document.createElement('img');
                        imgPreview.style.width = `100%`;
                        imgPreview.style.height = `auto`;
                        imgPreview.style.borderRadius = `8px`;
                        imgPreview.style.opacity = `0.7`;
                        imgPreview.src = base64Image;
                        // Add soft scroll on blurhash load to prevent scrolling issues
                        imgPreview.addEventListener('load', softChatScroll, { once: true });
                        
                        // Create container for relative positioning
                        const container = document.createElement('div');
                        container.style.position = `relative`;
                        container.appendChild(imgPreview);
                        
                        // Add downloading indicator (for progress bar targeting)
                        const iDownloading = document.createElement('i');
                        iDownloading.id = cAttachment.id;
                        iDownloading.textContent = `Downloading`;
                        iDownloading.style.position = `absolute`;
                        iDownloading.style.top = `50%`;
                        iDownloading.style.left = `50%`;
                        iDownloading.style.transform = `translate(-50%, -50%)`;
                        iDownloading.style.backgroundColor = `rgba(0, 0, 0, 0.7)`;
                        iDownloading.style.padding = `5px 10px`;
                        iDownloading.style.borderRadius = `4px`;
                        iDownloading.style.color = `white`;
                        container.appendChild(iDownloading);
                        
                        pMessage.appendChild(container);
                    })
                    .catch(() => {
                        // Fallback to simple downloading indicator if blurhash fails
                        const iDownloading = document.createElement('i');
                        iDownloading.id = cAttachment.id;
                        iDownloading.textContent = `Downloading`;
                        pMessage.appendChild(iDownloading);
                    });
            } else {
                // Display download progression UI for non-images
                const iDownloading = document.createElement('i');
                iDownloading.id = cAttachment.id;
                iDownloading.textContent = `Downloading`;
                pMessage.appendChild(iDownloading);
            }
            } else {
                // Check if this attachment will auto-download
                const willAutoDownload = cAttachment.size > 0 && cAttachment.size <= MAX_AUTO_DOWNLOAD_BYTES;

                // For images, show blurhash preview with download button (unless auto-downloading)
                if (['png', 'jpeg', 'jpg', 'gif', 'webp'].includes(cAttachment.extension)) {
                    // Generate blurhash preview for undownloaded image
                    // For group chats, use chat ID; for DMs, use sender.id
                    const blurhashNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    invoke('generate_blurhash_preview', { npub: blurhashNpub, msgId: msg.id })
                        .then(base64Image => {
                            const imgPreview = document.createElement('img');
                            imgPreview.style.width = `100%`;
                            imgPreview.style.height = `auto`;
                            imgPreview.style.borderRadius = `8px`;
                            imgPreview.style.opacity = willAutoDownload ? `0.8` : `0.6`;
                            imgPreview.src = base64Image;
                            // Add soft scroll on blurhash load to prevent scrolling issues
                            imgPreview.addEventListener('load', softChatScroll, { once: true });
                            
                            // Create container for relative positioning
                            const container = document.createElement('div');
                            container.style.position = `relative`;
                            container.appendChild(imgPreview);

                            // Only show download button if NOT auto-downloading
                            if (!willAutoDownload) {
                                // Determine and display file size
                                let strSize = 'Unknown Size';
                                if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);

                                // Create download button overlay
                                const iDownload = document.createElement('i');
                                iDownload.id = cAttachment.id;
                                iDownload.toggleAttribute('download', true);
                                // For group chats, use chat ID; for DMs, use sender.id
                                const downloadNpub2 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                                iDownload.setAttribute('npub', downloadNpub2);
                                iDownload.setAttribute('msg', msg.id);
                                iDownload.classList.add('btn');
                                iDownload.textContent = `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                                iDownload.style.position = `absolute`;
                                iDownload.style.top = `50%`;
                                iDownload.style.left = `50%`;
                                iDownload.style.transform = `translate(-50%, -50%)`;
                                iDownload.style.backgroundColor = `rgba(0, 0, 0, 0.8)`;
                                iDownload.style.padding = `8px 15px`;
                                iDownload.style.borderRadius = `6px`;
                                iDownload.style.color = `white`;
                                iDownload.style.cursor = `pointer`;
                                iDownload.style.fontSize = `12px`;
                                iDownload.style.whiteSpace = `nowrap`;
                                iDownload.style.textAlign = `center`;
                                iDownload.style.maxWidth = `90%`;
                                iDownload.style.overflow = `hidden`;
                                iDownload.style.textOverflow = `ellipsis`;
                                container.appendChild(iDownload);
                            } else {
                                // For auto-downloading images, create a hidden element for progress bar targeting
                                const iHidden = document.createElement('i');
                                iHidden.id = cAttachment.id;
                                iHidden.style.display = `none`;
                                container.appendChild(iHidden);
                            }
                            
                            pMessage.appendChild(container);
                        })
                        .catch(() => {
                            // Fallback when blurhash fails
                            if (!willAutoDownload) {
                                // Manual download: show download button
                                let strSize = 'Unknown Size';
                                if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);

                                const iDownload = document.createElement('i');
                                iDownload.id = cAttachment.id;
                                iDownload.toggleAttribute('download', true);
                                // For group chats, use chat ID; for DMs, use sender.id
                                const downloadNpub3 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                                iDownload.setAttribute('npub', downloadNpub3);
                                iDownload.setAttribute('msg', msg.id);
                                iDownload.classList.add('btn');
                                iDownload.textContent = `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                                pMessage.appendChild(iDownload);
                            } else {
                                // Auto-download: show downloading indicator for progress bar targeting
                                const iDownloading = document.createElement('i');
                                iDownloading.id = cAttachment.id;
                                iDownloading.textContent = `Downloading image...`;
                                iDownloading.style.textAlign = `center`;
                                iDownloading.style.display = `block`;
                                pMessage.appendChild(iDownloading);
                            }
                        });
                } else if (!willAutoDownload) {
                    // Only show download prompt for non-images if NOT auto-downloading
                    // Determine and display file size
                    let strSize = 'Unknown Size';
                    if (cAttachment.size > 0) strSize = formatBytes(cAttachment.size);

                    // Display download prompt UI for non-images
                    const iDownload = document.createElement('i');
                    iDownload.id = cAttachment.id;
                    iDownload.toggleAttribute('download', true);
                    // For group chats, use chat ID; for DMs, use sender.id
                    const downloadNpub = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    iDownload.setAttribute('npub', downloadNpub);
                    iDownload.setAttribute('msg', msg.id);
                    iDownload.classList.add('btn');
                    iDownload.textContent = `Download ${cAttachment.extension.toUpperCase()} (${strSize})`;
                    pMessage.appendChild(iDownload);
                }

                // If the size is known and within auto-download range; immediately begin downloading
                if (willAutoDownload) {
                    // For non-images (which don't have blurhash previews), create a placeholder for progress bar targeting
                    if (!['png', 'jpeg', 'jpg', 'gif', 'webp'].includes(cAttachment.extension)) {
                        const iDownloading = document.createElement('i');
                        iDownloading.id = cAttachment.id;
                        iDownloading.textContent = `Starting download...`;
                        iDownloading.style.textAlign = `center`;
                        iDownloading.style.display = `block`;
                        pMessage.appendChild(iDownloading);
                    }
                    
                    // For group chats, use chat ID; for DMs, use sender.id
                    const downloadNpub4 = isGroupChat ? strOpenChat : (sender?.id || strOpenChat);
                    invoke('download_attachment', { npub: downloadNpub4, msgId: msg.id, attachmentId: cAttachment.id });
                }
            }
    }

    // Append Payment Shortcuts (i.e: Bitcoin Payment URIs, etc)
    const cAddress = detectCryptoAddress(msg.content);
    if (cAddress) {
        // Render the Payment UI
        pMessage.appendChild(renderCryptoAddress(cAddress));
    }

    // Append Metadata Previews (i.e: OpenGraph data from URLs, etc) - only if enabled
    if (!msg.pending && !msg.failed && fWebPreviewsEnabled) {
        if (msg.preview_metadata?.og_image) {
            // Setup the Preview container
            const divPrevContainer = document.createElement('div');
            divPrevContainer.classList.add('msg-preview-container', 'btn');
            divPrevContainer.setAttribute('url', msg.preview_metadata.og_url || msg.preview_metadata.domain);

            // Setup the Favicon
            const imgFavicon = document.createElement('img');
            imgFavicon.classList.add('favicon');
            imgFavicon.src = msg.preview_metadata.favicon;
            imgFavicon.addEventListener('load', softChatScroll, { once: true });

            // Add the title (prefixed with the Favicon)
            const spanPreviewTitle = document.createElement('span');
            spanPreviewTitle.appendChild(imgFavicon);
            const spanText = document.createTextNode(msg.preview_metadata.title || msg.preview_metadata.og_title);
            spanPreviewTitle.appendChild(spanText);
            divPrevContainer.appendChild(spanPreviewTitle);

            // Load the Preview image
            const imgPreview = document.createElement('img');
            imgPreview.classList.add('msg-preview-img');
            imgPreview.src = msg.preview_metadata.og_image;
            // Auto-scroll the chat to correct against container resizes
            imgPreview.addEventListener('load', softChatScroll, { once: true });
            divPrevContainer.appendChild(imgPreview);

            // Render the Preview
            pMessage.appendChild(divPrevContainer);
        } else if (!msg.preview_metadata) {
            // Grab the message's metadata (currently, only URLs can have extracted metadata)
            if (msg.content && msg.content.includes('https')) {
                // Pass the chat ID so backend can find both DMs and group chats
                invoke("fetch_msg_metadata", { chatId: strOpenChat, msgId: msg.id });
            }
        }
    }

    // If the message is pending or failed, let's adjust it
    if (msg.pending && !msg.attachments.length) {
        divMessage.style.opacity = 0.75;
    }
    if (msg.failed) {
        pMessage.style.color = 'red';
    }

    // Add message reactions
    // TODO: while currently limited to one; add support for multi-reactions with a nice UX
    const cReaction = msg.reactions[0];
    let spanReaction;
    if (cReaction) {
        // Aggregate the 'reactions' of this reaction's type
        const nReacts = msg.reactions.reduce((a, b) => b.emoji === cReaction.emoji ? a + 1 : a, 0);
        spanReaction = document.createElement('span');
        spanReaction.classList.add('reaction');
        spanReaction.textContent = `${cReaction.emoji} ${nReacts}`;
        twemojify(spanReaction);
    } else if (!msg.mine) {
        // No reaction on the contact's message, so let's display the 'Add Reaction' UI
        spanReaction = document.createElement('span');
        spanReaction.classList.add('add-reaction', 'hideable', 'icon', 'icon-smile-face');
    }

    // Construct our "extras" (reactions, reply button, etc)
    // TODO: placeholder style, looks awful, but works!
    const divExtras = document.createElement('div');
    divExtras.classList.add('msg-extras');
    if (msg.mine) divExtras.style.marginRight = `5px`;
    else divExtras.style.marginLeft = `5px`;

    // These can ONLY be shown on fully sent messages (inherently does not apply to received msgs)
    if (!msg.pending && !msg.failed) {
        // Reactions
        if (spanReaction) {
            if (msg.mine) {
                // My message: reactions on the left
                spanReaction.style.marginLeft = '-10px';
            }
            divExtras.append(spanReaction);
        } else {
            // No reactions: just render the message
            divMessage.appendChild(pMessage);
        }

        // Reply Icon (if we're not already replying!)
        if (!fReplying) {
            const spanReply = document.createElement('span');
            spanReply.classList.add('reply-btn', 'hideable', 'icon', 'icon-reply');
            divExtras.append(spanReply);
        }

        // File Reveal Icon (if a file was attached)
        if (strRevealAttachmentPath) {
            const spanReveal = document.createElement('span');
            spanReveal.setAttribute('filepath', strRevealAttachmentPath);
            spanReveal.classList.add('hideable', 'icon', 'icon-file-search');
            divExtras.append(spanReveal);
        }
    }

    // Depending on who it is: render the extras appropriately
    if (msg.mine) divMessage.append(divExtras, pMessage);
    else divMessage.append(pMessage, divExtras);

    // After rendering, check message corner styling for received messages
    // This needs to be done post-render when the message is in the DOM
    setTimeout(() => {
        if (!msg.mine && domChatMessages.contains(divMessage)) {
            const nextMsg = divMessage.nextElementSibling;
            const prevMsg = divMessage.previousElementSibling;
            
            // Check if previous message exists and is from a different sender
            const isFirstFromSender = !prevMsg || prevMsg.getAttribute('sender') !== strShortSenderID;
            
            // Check if next message exists and is from the same sender
            const hasNextFromSameSender = nextMsg && nextMsg.getAttribute('sender') === strShortSenderID;
            
            // If we're continuing a message streak (not first from sender), we need to update the previous message
            if (!isFirstFromSender && prevMsg) {
                // The previous message is no longer the last in the streak, so remove its rounded corner
                const prevPMsg = prevMsg.querySelector('p');
                if (prevPMsg && !prevPMsg.classList.contains('no-background')) {
                    // Check if the previous message was styled as last (had rounded corner)
                    if (prevPMsg.style.borderBottomLeftRadius === '15px') {
                        // Remove the rounded corner since it's no longer the last
                        prevPMsg.style.borderBottomLeftRadius = '';
                    }
                }
            }
            
            // Now style the current message appropriately
            if (isFirstFromSender && !hasNextFromSameSender) {
                // This is a singular message - apply sharp bottom-left corner
                const pMsg = divMessage.querySelector('p');
                if (pMsg && !pMsg.classList.contains('no-background')) {
                    // Make the bottom-left corner sharp (0px radius)
                    pMsg.style.borderBottomLeftRadius = '0px';
                }
            }
            // If this is the last message in a multi-message streak (has previous from same sender, but no next from same sender)
            else if (!isFirstFromSender && !hasNextFromSameSender) {
                // This is the last message in a streak - apply rounded bottom-left corner
                const pMsg = divMessage.querySelector('p');
                if (pMsg && !pMsg.classList.contains('no-background')) {
                    // Make the bottom-left corner rounded (15px radius) to close the bubble group
                    pMsg.style.borderBottomLeftRadius = '15px';
                }
            }
        }
    }, 0);

    return divMessage;
}

/**
 * Select a message to begin replying to
 * @param {MouseEvent} e 
 */
function selectReplyingMessage(e) {
    // Cancel any existing reply-focus
    if (strCurrentReplyReference) {
        document.getElementById(strCurrentReplyReference).querySelector('p').style.borderColor = ``;
    }
    // Get the reply ID
    strCurrentReplyReference = e.target.parentElement.parentElement.id;
    // Hide the File UI and Display the cancel UI
    domChatMessageInputFile.style.display = 'none';
    domChatMessageInputCancel.style.display = '';
    // Display a replying placeholder
    domChatMessageInput.setAttribute('placeholder', 'Enter reply...');
    // Focus the message input
    domChatMessageInput.focus();
    // Add a reply-focus
    e.target.parentElement.parentElement.querySelector('p').style.borderColor = `#ffffff`;
}

/**
 * Cancel any ongoing replies and reset the messaging interface
 */
function cancelReply() {
    // Reset the message UI
    domChatMessageInputFile.style.display = '';
    domChatMessageInputCancel.style.display = 'none';
    domChatMessageInput.setAttribute('placeholder', strOriginalInputPlaceholder);

    // Focus the message input
    domChatMessageInput.focus();

    // Cancel any existing reply-focus
    if (strCurrentReplyReference) {
        let domMsg = document.getElementById(strCurrentReplyReference);
        if (domMsg) domMsg.querySelector('p').style.borderColor = ``;
    }

    // Remove the reply ID
    strCurrentReplyReference = '';

    // Reset send button state based on current input
    const hasText = domChatMessageInput.value.trim().length > 0;
    if (hasText) {
        domChatMessageInputSend.classList.add('active');
        domChatMessageInputVoice.style.display = 'none';
    } else {
        domChatMessageInputSend.classList.remove('active');
        domChatMessageInputVoice.style.display = '';
    }
}

/**
 * Open a chat with a particular contact
 * @param {string} contact 
 */
function openChat(contact) {
    // Display the Chat UI
    navbarSelect('chat-btn');
    domProfile.style.display = 'none';
    domChatNew.style.display = 'none';
    domChats.style.display = 'none';
    domChat.style.display = '';
    domSettingsBtn.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = `none`;

    // Get the chat (could be DM or Group)
    const chat = arrChats.find(c => c.id === contact);
    const isGroup = chat?.chat_type === 'MlsGroup';
    const profile = !isGroup ? getProfile(contact) : null;
    strOpenChat = contact;
    if (isGroup) { refreshGroupMemberCount(contact); }

    // Clear any existing auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Record when the chat was opened
    chatOpenTimestamp = Date.now();

    // After 100ms, stop auto-scrolling on media loads
    chatOpenAutoScrollTimer = setTimeout(() => {
        chatOpenTimestamp = 0; // Reset timestamp to disable auto-scrolling
        chatOpenAutoScrollTimer = null;
    }, 100);

    // If it's a group, load messages from unified storage (no sync needed - live subscription handles it)
    if (isGroup && chat) {
        (async () => {
            // Load messages using unified storage
            try {
                const messages = await invoke('get_chat_messages', {
                    chatId: contact,
                    limit: 50
                }) || [];

                console.log(`Loaded ${messages.length} messages for group ${String(contact).substring(0, 8)}...`);

                // Messages are already in the correct format from unified storage!
                // Merge with existing messages to avoid duplicates
                const existingIds = new Set((chat.messages || []).map(m => m.id));
                const newOnly = messages.filter(m => !existingIds.has(m.id));

                // Append new messages and keep chronological order
                chat.messages = (chat.messages || []).concat(newOnly);
                chat.messages.sort((a, b) => a.at - b.at);

                // Insert only the new messages into the open chat
                if (newOnly.length) {
                    updateChat(chat, newOnly, null, false);
                    // Preserve typing indicator after async message load
                    updateChatHeaderSubtext(chat);
                }
            } catch (e) {
                console.error('Failed to load group messages:', e);
            }
        })();
    }

    // TODO: enable procedural rendering when the user scrolls up, this is a temp renderer optimisation
    updateChat(chat, (chat?.messages || []).slice(-100), profile, true);
}

/**
 * Open the dialog for starting a new chat
 */
function openNewChat() {
    // Display the UI
    domChatNew.style.display = '';
    domChats.style.display = 'none';
    domChat.style.display = 'none';

    // Hide the Navbar
    domNavbar.style.display = 'none';
}

/**
 * Closes the current chat, taking the user back to the chat list
 */
async function closeChat() {
    // Clear any auto-scroll timer
    if (chatOpenAutoScrollTimer) {
        clearTimeout(chatOpenAutoScrollTimer);
        chatOpenAutoScrollTimer = null;
    }

    // Attempt to completely release memory (force garbage collection...) of in-chat media
    while (domChatMessages.firstElementChild) {
        const domChild = domChatMessages.firstElementChild;

        // For media (images, audio, video); we ensure they're fully unloaded
        const domMedias = domChild?.querySelectorAll('img, audio, video');
        for (const domMedia of domMedias) {
            // Streamable media (audio + video) should be paused, then force-unloaded
            if (domMedia instanceof HTMLMediaElement) {
                domMedia.pause();
                
                // For Android blob URLs, revoke them before clearing
                if (platformFeatures.os === 'android' && domMedia.src.startsWith('blob:')) {
                    URL.revokeObjectURL(domMedia.src);
                }
                
                domMedia.removeAttribute('src'); // Better than setting to empty string
                domMedia.load();
            }
            // Static media (images) should simply be unloaded
            if (domMedia instanceof HTMLImageElement) {
                // Also check for blob URLs on images if you use them
                if (domMedia.src.startsWith('blob:')) {
                    URL.revokeObjectURL(domMedia.src);
                }
                domMedia.removeAttribute('src');
            }
        }

        // Now we explicitly drop them
        domChild.remove();
    }

    // Reset the chat UI
    domProfile.style.display = 'none';
    domSettingsBtn.style.display = '';
    domChatNew.style.display = 'none';
    domChat.style.display = 'none';
    strOpenChat = "";
    nLastTypingIndicator = 0;

    // Display the Navbar
    domNavbar.style.display = ``;

    // Cancel any ongoing replies or selections
    strCurrentReactionReference = "";
    strCurrentReplyReference = "";
    cancelReply();

    // Navigate back to chat list with animation
    await openChatlist();

    // Update the Chat List
    renderChatlist();

    // Ensure the chat list re-adjusts to fit
    adjustSize();
}

/**
 * Open the Expanded Profile view, optionally with a non-default profile
 * @param {Profile} cProfile - An optional profile to render
 */
function openProfile(cProfile) {
    navbarSelect('profile-btn');
    domChats.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';

    // Render our own profile by default, but otherwise; the given one
    if (!cProfile) cProfile = arrProfiles.find(a => a.mine);
    renderProfileTab(cProfile);

    if (domProfile.style.display !== '') {
        // Run a subtle fade-in animation
        domProfile.classList.add('fadein-subtle-anim');
        domProfile.addEventListener('animationend', () => domProfile.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domProfile.style.display = '';
    }
}

async function openChatlist() {
    navbarSelect('chat-btn');
    domProfile.style.display = 'none';
    domSettings.style.display = 'none';
    domInvites.style.display = 'none';

    if (domChats.style.display !== '') {
        // Run a subtle fade-in animation
        domChats.classList.add('fadein-subtle-anim');
        domChats.addEventListener('animationend', () => domChats.classList.remove('fadein-subtle-anim'), { once: true });

        // Open the tab
        domChats.style.display = '';
    }
    
    // Load and display MLS invites in the Chat tab
    await loadMLSInvites();
}

function openSettings() {
    navbarSelect('settings-btn');
    domSettings.style.display = '';

    // Hide the other tabs
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domInvites.style.display = 'none';

    // If an update is available, scroll to the updates section
    const updateDot = document.getElementById('settings-update-dot');
    if (updateDot && updateDot.style.display !== 'none') {
        // Give the settings tab time to render
        setTimeout(() => {
            const updatesSection = document.getElementById('settings-updates');
            if (updatesSection) {
                updatesSection.scrollIntoView({ behavior: 'smooth', block: 'start' });
                // Hide the notification dot after scrolling
                updateDot.style.display = 'none';
            }
        }, 100);
    }
}

async function openInvites() {
    navbarSelect('invites-btn');
    domInvites.style.display = '';

    // Hide the other tabs
    domProfile.style.display = 'none';
    domChats.style.display = 'none';
    domSettings.style.display = 'none';

    // Fetch and display the invite code
    const inviteCodeElement = document.getElementById('invite-code');
    inviteCodeElement.textContent = 'Loading';
    
    try {
        const inviteCode = await invoke('get_or_create_invite_code');
        inviteCodeElement.textContent = inviteCode;
        document.getElementById('invite-code-twitter').href = buildXIntentUrl(inviteCode);
        
        // Add invite code copy functionality
        const copyBtn = document.getElementById('invite-code-copy');
        if (copyBtn) {
            // Remove any existing listeners to prevent duplicates
            copyBtn.replaceWith(copyBtn.cloneNode(true));
            const newCopyBtn = document.getElementById('invite-code-copy');
            
            newCopyBtn.addEventListener('click', (e) => {
                if (inviteCode && inviteCode !== 'Loading...' && inviteCode !== 'Error loading code') {
                    navigator.clipboard.writeText(inviteCode).then(() => {
                        const btn = e.target.closest('.invite-code-copy-btn');
                        if (btn) {
                            btn.innerHTML = '<span class="icon icon-check"></span>';
                            setTimeout(() => {
                                btn.innerHTML = '<span class="icon icon-copy"></span>';
                            }, 2000);
                        }
                    });
                }
            });
        }
    } catch (error) {
        inviteCodeElement.textContent = 'Error loading code';
        console.error('Failed to get invite code:', error);
    }

    // Note: MLS invites are now shown in the Chat tab, not here
}

/**
 * Edit the profile description inline
 */
function editProfileDescription() {
    // Get the current profile
    const cProfile = arrProfiles.find(a => a.mine);
    if (!cProfile) return;

    // Set the textarea content to current description
    domProfileDescriptionEditor.value = cProfile.about || '';

    // Hide the span and show the textarea
    domProfileDescription.style.display = 'none';
    domProfileDescriptionEditor.style.display = '';

    // Focus the text
    domProfileDescriptionEditor.focus();

    // Handle blur event to save and return to view mode
    domProfileDescriptionEditor.onblur = () => {
        // Hide textarea and show span
        domProfileDescriptionEditor.style.display = 'none';
        domProfileDescription.style.display = '';

        // Remove the blur event listener
        domProfileDescriptionEditor.onblur = null;

        // If nothing was edited, don't change anything
        if (!domProfileDescriptionEditor.value ||
            domProfileDescriptionEditor.value === cProfile.about
        ) return;

        // Update the profile's about property
        cProfile.about = domProfileDescriptionEditor.value;

        // Update the span content
        domProfileDescription.textContent = cProfile.about;
        twemojify(domProfileDescription);

        // Upload new About Me to Nostr
        setAboutMe(cProfile.about);
    };

    // Resize it to match the content size (CSS cannot scale textareas based on content)
    domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';

    // Handle input events to resize the textarea dynamically
    domProfileDescriptionEditor.oninput = () => {
        domProfileDescriptionEditor.style.height = Math.min(domProfileDescriptionEditor.scrollHeight, 100) + 'px';
    };

    // Handle Enter key to submit (excluding Shift+Enter for line breaks)
    domProfileDescriptionEditor.onkeydown = (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domProfileDescriptionEditor.blur(); // Trigger the blur event to save
        }
    };
}

/**
 * A utility to "select" one Navbar item, deselecting the rest automatically.
 */
function navbarSelect(strSelectionID = '') {
    for (const navItem of domNavbar.querySelectorAll('div')) {
        if (strSelectionID === navItem.id) navItem.classList.remove('navbar-btn-inactive');
        else navItem.classList.add('navbar-btn-inactive');
    }
}

/**
 * Our Bech32 Nostr Public Key
 */
let strPubkey;

/**
 * The timestamp we sent our last typing indicator
 * 
 * Ensure this is wiped when the chat is closed!
 */
let nLastTypingIndicator = 0;

const strOriginalInputPlaceholder = domChatMessageInput.getAttribute('placeholder');
window.addEventListener("DOMContentLoaded", async () => {
    // Once login fade-in animation ends, remove it
    domLogin.addEventListener('animationend', () => domLogin.classList.remove('fadein-anim'), { once: true });

    // Fetch platform features to determine OS-specific behavior
    await fetchPlatformFeatures();

    // Immediately load and apply theme settings
    const strTheme = await invoke('get_theme');
    if (strTheme) {
        await setTheme(strTheme);
    }

    // If a local encrypted key exists, boot up the decryption UI
    if (await hasKey()) {
        // Private Key is available, login screen!
        openEncryptionFlow(null, true);
    }

    // Hook up our static buttons
    domInvitesBtn.onclick = openInvites;
    domProfileBtn.onclick = () => openProfile();
    domChatlistBtn.onclick = openChatlist;
    domSettingsBtn.onclick = openSettings;
    domLoginAccountCreationBtn.onclick = async () => {
        try {
            const { public: pubKey, private: privKey } = await invoke("create_account");
            strPubkey = pubKey;
            
            // Connect to Nostr network
            await invoke("connect");
            
            // Skip invite flow - go directly to encryption
            openEncryptionFlow(privKey, false);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    };
    domLoginAccountBtn.onclick = () => {
        domLoginImport.style.display = '';
        domLoginStart.style.display = 'none';
    };
    domLoginBtn.onclick = async () => {
        // Import and derive our keys
        try {
            const { public: pubKey, private: privKey } = await invoke("login", { importKey: domLoginInput.value.trim() });
            strPubkey = pubKey;

            // Connect to Nostr
            await invoke("connect");

            // Skip invite flow - go directly to encryption
            openEncryptionFlow(privKey);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true, '', 'vector_warning.svg');
        }
    }
    domChatBackBtn.onclick = closeChat;
    domChatBookmarksBtn.onclick = () => {
        openChat(strPubkey);
    };
    domChatNewBackBtn.onclick = closeChat;
    domChatNewStartBtn.onclick = () => {
        openChat(domChatNewInput.value.trim());
        domChatNewInput.value = ``;
    };
    domChatNewInput.onkeydown = async (evt) => {
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            domChatNewStartBtn.click();
        }
    };
    domChatMessageInputCancel.onclick = cancelReply;

    // Hook up a scroll handler in the chat to display UI elements at certain scroll depths
    createScrollHandler(domChatMessages, domChatMessagesScrollReturnBtn, { threshold: 500 })

    // Hook up an in-chat File Upload listener
    domChatMessageInputFile.onclick = async () => {
        let filepath = await selectFile();
        if (filepath) {
            // Reset reply selection while passing a copy of the reference to the backend
            const strReplyRef = strCurrentReplyReference;
            cancelReply();
            await sendFile(strOpenChat, strReplyRef, filepath);
        }
    };

    // Hook up an in-chat File Paste listener
    document.onpaste = async (evt) => {
        if (strOpenChat) {
            // Check if the clipboard data contains an image
            const arrItems = Array.from(evt.clipboardData.items);
            if (arrItems.some(item => item.type.startsWith('image/'))) {
                evt.preventDefault();

                // Determine if this image supports Transparency or not
                // Note: this is necessary to account for the accidental "zeroing" of Alpha values
                // ... in non-PNG/GIF formats, which led to completely blank JPEGs.
                const fTransparent = arrItems.some(item => item.type.includes('png') || item.type.includes('gif'));

                // Reset reply selection while passing a copy of the reference to the backend
                const strReplyRef = strCurrentReplyReference;
                cancelReply();

                // Tell the Rust backend to acquire the image from clipboard and send it to the current chat
                await invoke('paste_message', {
                    receiver: strOpenChat,
                    repliedTo: strReplyRef,
                    transparent: fTransparent
                });

                nLastTypingIndicator = 0;
            }
        }
    };

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

    // Clear input and show sending state
    domChatMessageInput.value = '';
    domChatMessageInput.setAttribute('placeholder', 'Sending...');
    
    // Remove active state from send button and show mic button since input is now empty
    domChatMessageInputSend.classList.remove('active');
    domChatMessageInputVoice.style.display = '';

    try {
        const replyRef = strCurrentReplyReference;
        cancelReply();
        
        // Check if current chat is a group
        const chat = arrChats.find(c => c.id === strOpenChat);
        if (chat?.chat_type === 'MlsGroup') {
            // Send group message with cleaned text
            const wrapperId = await invoke('send_mls_group_message', {
                groupId: strOpenChat,
                text: cleanedText,
                repliedTo: replyRef || null
            });
            // Message is already added optimistically by send_mls_group_message
            // Live subscription will handle receiving it back from the relay
        } else {
            // Send regular DM with cleaned text
            await message(strOpenChat, cleanedText, replyRef, "");
        }
        
        nLastTypingIndicator = 0;
    } catch(e) {
        console.error('Failed to send message:', e);
    }
}

    // Desktop/iOS - traditional keydown approach
    domChatMessageInput.addEventListener('keydown', async (evt) => {
        if ((evt.key === 'Enter' || evt.keyCode === 13) && !evt.shiftKey) {
            evt.preventDefault();
            await sendMessage(domChatMessageInput.value);
        }
    });

    // Android-specific - detect newline in input
    if (platformFeatures.os === 'android') {
        domChatMessageInput.addEventListener('input', async (evt) => {
            const value = domChatMessageInput.value;

            // Check if input contains a newline character
            if (value.includes('\n')) {
                // Extract the message BEFORE clearing (remove the newline)
                const messageText = value.replace(/\n/g, '');

                // Send the message with the extracted text
                await sendMessage(messageText);
            }
        });
    }

    // Hook up an 'input' listener on the Message Box for typing indicators
domChatMessageInput.oninput = async () => {
    // Toggle send button active state based on text content
    const hasText = domChatMessageInput.value.trim().length > 0;
    if (hasText) {
        domChatMessageInputSend.classList.add('active');
        // Hide mic button when user starts typing
        domChatMessageInputVoice.style.display = 'none';
    } else {
        domChatMessageInputSend.classList.remove('active');
        // Show mic button when input is empty
        domChatMessageInputVoice.style.display = '';
    }

    // Send a Typing Indicator only when content actually changes and setting is enabled
    if (fSendTypingIndicators && nLastTypingIndicator + 30000 < Date.now()) {
        nLastTypingIndicator = Date.now();
        await invoke("start_typing", { receiver: strOpenChat });
    }
};

    // Hook up the send button click handler
    domChatMessageInputSend.onclick = async () => {
        const messageText = domChatMessageInput.value;
        if (messageText && messageText.trim()) {
            await sendMessage(messageText);
        }
    };

    // Hook up our drag-n-drop listeners
    if (platformFeatures.os !== 'android' && platformFeatures.os !== 'ios') {
        await getCurrentWebview().onDragDropEvent(async (event) => {
            // Only accept File Drops if a chat is open
            if (strOpenChat) {
                if (event.payload.type === 'over') {
                    // TODO: add hover effects
                } else if (event.payload.type === 'drop') {
                    // Reset reply selection while passing a copy of the reference to the backend
                    const strReplyRef = strCurrentReplyReference;
                    cancelReply();
                    await sendFile(strOpenChat, strReplyRef, event.payload.paths[0]);
                } else {
                    // TODO: remove hover effects
                }
            }
        });

        await getCurrentWindow().onFocusChanged(async (event) => {
            if (event.payload && strOpenChat) {
                const currentChat = getDMChat(strOpenChat);
                if (currentChat && currentChat.messages.length > 0) {
                    // Find the last message from the contact (not from current user)
                    let lastContactMsg = null;
                    for (let i = currentChat.messages.length - 1; i >= 0; i--) {
                        if (!currentChat.messages[i].mine) {
                            lastContactMsg = currentChat.messages[i];
                            break;
                        }
                    }
                    if (lastContactMsg) {
                        markAsRead(currentChat, lastContactMsg);
                    }
                }
            }
        });
    }

    // Hook up our voice message recorder listener
    const recorder = new VoiceRecorder(domChatMessageInputVoice);
    recorder.button.addEventListener('click', async () => {
        if (recorder.isRecording) {
            // Stop the recording and retrieve our WAV data
            const wavData = await recorder.stop();

            // Unhide our messaging UI
            if (wavData) {
                // Placeholder
                domChatMessageInput.value = '';
                domChatMessageInput.setAttribute('placeholder', 'Sending...');

                // Send raw bytes to Rust, if the chat is still open
                // Note: since the user could, for some reason, close the chat while recording - we need to check that it's still open
                if (strOpenChat) {
                    try {
                        // Reset reply selection while passing a copy of the reference to the backend
                        const strReplyRef = strCurrentReplyReference;
                        cancelReply();
                        await invoke('voice_message', {
                            receiver: strOpenChat,
                            repliedTo: strReplyRef,
                            bytes: wavData
                        });
                    } catch (e) {
                        // Notify of an attachment send failure
                        popupConfirm(e, '', true, '', 'vector_warning.svg');
                    }

                    nLastTypingIndicator = 0;
                }
            }
        } else {
            // Display our recording status
            domChatMessageInput.value = '';
            domChatMessageInput.setAttribute('placeholder', 'Recording...');

            // Start recording
            if (await recorder.start() === false) {
                // An error likely occured: reset the UI
                cancelReply();
                await recorder.stop();
            }
        }
    });

    // Initialize voice transcription with default model
    window.cTranscriber = new VoiceTranscriptionUI();
    window.voiceSettings = new VoiceSettings();

    // Only load whisper models if transcription is supported
    if (platformFeatures.transcription) {
        await window.voiceSettings.loadWhisperModels();
    }
    
    window.voiceSettings.initVoiceSettings();

    // Initialize settings
    await initSettings();

    // Hook up our "Help Prompts" to give users easy feature explainers in ambiguous or complex contexts
    // Note: since some of these overlap with Checkbox Labels: we prevent event bubbling so that clicking the Info Icon doesn't also trigger other events
    domSettingsWhisperModelInfo.onclick = (e) => {
        popupConfirm('Vector Voice AI Model', 'The Vector Voice AI model <b>determines the Quality of your transcriptions.</b><br><br>A larger model will provide more accurate transcriptions & translations, but require more Disk Space, Memory and CPU power to run.', true);
    };
    domSettingsWhisperAutoTranslateInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Vector Voice Translations', 'Vector Voice AI can <b>automatically detect non-English languages and translate them in to English text for you.</b><br><br>You can decide whether Vector Voice transcribes in to their native spoken language, or instead translates in to English on your behalf.', true);
    };
    domSettingsWhisperAutoTranscribeInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Vector Voice Transcriptions', 'Vector Voice AI can <b>automatically transcribe incoming Voice Messages</b> for immediate reading, without needing to listen.<br><br>You can decide whether Vector Voice transcribes automatically, or if you prefer to transcribe each message explicitly.', true);
    };
    domSettingsPrivacyWebPreviewsInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Web Previews', 'When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>This may expose your IP address if you do not use a VPN.', true);
    };
    domSettingsPrivacyStripTrackingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Strip Tracking Markers', 'When enabled, Vector will <b>automatically remove tracking markers</b> from URLs before displaying or sending them.<br><br>This helps reduce your footprint and enhances your privacy with no loss in functionality, only disable if you know what you\'re doing.', true);
    };
    domSettingsPrivacySendTypingInfo.onclick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        popupConfirm('Send Typing Indicators', 'When enabled, Vector will <b>notify your contacts when you are typing</b> a message to them.<br><br>Disable this if you prefer to type without others knowing you are composing a message.', true);
    };

        // Add npub copy functionality for chat-new section
    document.getElementById('chat-new-npub-copy')?.addEventListener('click', (e) => {
        const npub = document.getElementById('share-npub')?.textContent;
        if (npub) {
            navigator.clipboard.writeText(npub).then(() => {
                const copyBtn = e.target.closest('.profile-npub-copy');
                if (copyBtn) {
                    copyBtn.innerHTML = '<span class="icon icon-check"></span>';
                    setTimeout(() => {
                        copyBtn.innerHTML = '<span class="icon icon-copy"></span>';
                    }, 2000);
                }
            });
        }
    });
});

// Listen for app-wide click interations
document.addEventListener('click', (e) => {
    // If we're clicking the emoji search, don't close it!
    if (e.target === emojiSearch) return;

    // If we're clicking an <a> link, handle it with our openUrl function
    if (e.target.tagName === 'A' && e.target.href) {
        e.preventDefault();
        return openUrl(e.target.href);
    }

    // If we're clicking a Reply button, begin a reply
    if (e.target.classList.contains("reply-btn")) return selectReplyingMessage(e);

    // If we're clicking a File Reveal button, reveal the file with the OS File Explorer
    if (e.target.getAttribute('filepath')) {
        return revealItemInDir(e.target.getAttribute('filepath'));
    }

    // If we're clicking a Reply context, center the referenced message in view
    if (e.target.classList.contains('msg-reply') || e.target.parentElement?.classList.contains('msg-reply')  || e.target.parentElement?.parentElement?.classList.contains('msg-reply')) {
        // Note: The `substring(2)` removes the `r-` prefix
        const strID = e.target.id || e.target.parentElement?.id || e.target.parentElement.parentElement.id;
        const domMsg = document.getElementById(strID.substring(2));
        centerInView(domMsg);

        // Run an animation to bring the user's eye to the message
        const pContainer = domMsg.querySelector('p');
        if (!pContainer.classList.contains('no-background')) {
            domMsg.classList.add('highlight-animation');
            setTimeout(() => domMsg.classList.remove('highlight-animation'), 1500);
        }
        return;
    }

    // If we're clicking a Metadata Preview, open it's URL, if one is attached
    if (e.target.classList.contains("msg-preview-container") || e.target.parentElement?.classList.contains("msg-preview-container")) {
        const strURL = e.target.getAttribute('url') || e.target.parentElement.getAttribute('url');
        if (strURL) openUrl(strURL);
        return;
    }

    // If we're clicking a Payment URI, open it's URL
    if (e.target.getAttribute('pay-uri')) {
        return openUrl(e.target.getAttribute('pay-uri'));
    }

    // If we're clicking a Contact in the main chat list (NOT inside the Create Group panel), open the chat
    const cg = document.getElementById('create-group');
    const inCreateGroup = cg && cg.style.display !== 'none' && cg.contains(e.target);
    if (!inCreateGroup && (e.target.classList.contains("chatlist-contact") || e.target.parentElement?.classList.contains("chatlist-contact") ||  e.target.parentElement?.parentElement?.classList.contains("chatlist-contact"))) {
        const strID = e.target.id || e.target.parentElement?.id || e.target.parentElement.parentElement.id;
        return openChat(strID);
    }

    // If we're clicking an Attachment Download button, request the download
    if (e.target.hasAttribute('download')) {
        return invoke('download_attachment', { npub: e.target.getAttribute('npub'), msgId: e.target.getAttribute('msg'), attachmentId: e.target.id });
    }

    // Run the emoji panel open/close logic
    openEmojiPanel(e);
});

/**
 * Resize certain tricky components (i.e: the Chat Box) on window resizes.
 * 
 * This can also be re-called when some components are spawned, since they can
 * affect the height and width of other components, too.
 */
function adjustSize() {
    // Chat List: resize the list to fit within the screen after the upper Account area
    // Note: no idea why the `- 50px` is needed below, magic numbers, I guess.
    const nNewChatBtnHeight = domChatNewDM?.getBoundingClientRect().height || 0;
    const nNavbarHeight = domNavbar.getBoundingClientRect().height;
    domChatList.style.maxHeight = (window.innerHeight - (domChatList.offsetTop + nNewChatBtnHeight + nNavbarHeight)) + 50 + 'px';

    // Chat Box: resize the chat to fill the remaining space after the upper Contact area (name)
    const rectContact = domChatContact.getBoundingClientRect();
    domChat.style.height = (window.innerHeight - rectContact.height) + `px`;

    // If the chat is open, and the fade-out exists, then position it correctly
    if (strOpenChat) {
        domChatMessagesFade.style.top = domChatMessages.offsetTop + 'px';
    }

    // If the chat is open, and they've not significantly scrolled up: auto-scroll down to correct against container resizes
    softChatScroll();
}

/**
 * Scrolls the chat to the bottom if the user has not already scrolled upwards substantially.
 * 
 * This is used to correct against container resizes, i.e: if an image loads, or a message is received.
 */
function softChatScroll() {
    if (!strOpenChat) return;

    // If the chat is open, and they've not significantly scrolled up: auto-scroll down to correct against container resizes
    const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
    if (pxFromBottom < 1000) {
        scrollToBottom(domChatMessages, false);
    }
}

window.onresize = adjustSize;

// ===== Create Group: state and helpers =====
/**
 * Selected members (npubs) for the group being created.
 * Keep this decoupled from arrChats.
 */
let arrSelectedGroupMembers = [];
/**
 * Tracks whether the user attempted to create the group.
 * Used to only show inline validation after an explicit attempt.
 */
let fCreateGroupAttempt = false;


/**
 * Render the filterable, scrollable contact list with checkboxes.
 * Reuses arrProfiles as the source of truth.
 */
function renderCreateGroupList(filterText = '') {
    if (!domCreateGroupList) return;
    domCreateGroupList.innerHTML = '';

    const f = (filterText || '').trim().toLowerCase();

    // Exclude our own profile from selection
    const mine = arrProfiles.find(p => p.mine)?.id;

    // Build a fragment for performance
    const frag = document.createDocumentFragment();

    for (const p of arrProfiles) {
        if (!p || !p.id) continue;
        if (p.id === mine) continue;

        // Filter by nickname/name/npub
        const name = p.nickname || p.name || '';
        const hay = (name + ' ' + p.id).toLowerCase();
        if (f && !hay.includes(f)) continue;

        // Row container (reuse existing styling conventions)
        const row = document.createElement('div');
        row.classList.add('chatlist-contact');
        row.id = `cg-${p.id}`;

        // Avatar
        const avatarContainer = document.createElement('div');
        avatarContainer.style.position = 'relative';
        avatarContainer.style.zIndex = '-1';
        if (p.avatar) {
            const img = document.createElement('img');
            img.src = p.avatar;
            avatarContainer.appendChild(img);
        } else {
            avatarContainer.appendChild(pubkeyToAvatar(p.id, name, 50));
        }
        row.appendChild(avatarContainer);

        // Title and subtitle
        const preview = document.createElement('div');
        preview.classList.add('chatlist-contact-preview');

        const title = document.createElement('h4');
        title.classList.add('cutoff');
        title.textContent = name || p.id;
        if (name) twemojify(title);
        preview.appendChild(title);

        const subtitle = document.createElement('p');
        subtitle.classList.add('cutoff');
        subtitle.style.opacity = '0.7';
        subtitle.textContent = p.id;
        preview.appendChild(subtitle);
        preview.style.zIndex = '-1';
        row.appendChild(preview);

        // Checkbox
        const chk = document.createElement('input');
        chk.type = 'checkbox';
        chk.style.marginLeft = 'auto';
        chk.style.marginRight = 'auto';
        chk.style.width = '18px';
        chk.style.height = '18px';
        chk.style.marginTop = 'auto';
        chk.style.marginBottom = 'auto';
        chk.checked = arrSelectedGroupMembers.includes(p.id);
        chk.setAttribute('aria-label', `Select ${name || p.id}`);
        chk.onchange = () => {
            if (chk.checked) {
                if (!arrSelectedGroupMembers.includes(p.id)) {
                    arrSelectedGroupMembers.push(p.id);
                }
            } else {
                arrSelectedGroupMembers = arrSelectedGroupMembers.filter(n => n !== p.id);
            }
            updateCreateGroupValidation(true);
        };
        row.appendChild(chk);

        // Row click toggles checkbox for better UX (avoid toggling when clicking the checkbox itself)
        // Important: stop propagation to avoid the global document click handler opening the DM chat.
        row.onclick = null;
        row.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (e.target === chk) return;
            chk.click();
        });
        // Also stop propagation when the checkbox itself is clicked
        chk.addEventListener('click', (e) => {
            e.stopPropagation();
        });

        frag.appendChild(row);
    }

    // If no matches
    if (!frag.childElementCount) {
        const empty = document.createElement('p');
        empty.style.textAlign = 'center';
        empty.style.opacity = '0.7';
        empty.textContent = f ? 'No matches' : 'No contacts found';
        domCreateGroupList.appendChild(empty);
    } else {
        domCreateGroupList.appendChild(frag);
    }
}

/**
 * Enable/disable Create button and show inline hint
 */
function updateCreateGroupValidation(showInline = false) {
    if (!domCreateGroupCreateBtn) return;
    const nameOk = !!domCreateGroupName?.value.trim();
    const membersOk = arrSelectedGroupMembers.length > 0;

    const enabled = nameOk && membersOk;

    // Toggle both property and attribute to avoid any CSS/UA inconsistencies
    domCreateGroupCreateBtn.disabled = !enabled;
    if (enabled) {
        domCreateGroupCreateBtn.removeAttribute('disabled');
    } else {
        domCreateGroupCreateBtn.setAttribute('disabled', '');
    }

    // Only show status after an explicit attempt, or when forced via parameter
    const shouldShow = showInline || fCreateGroupAttempt;

    if (domCreateGroupStatus) {
        if (shouldShow && (!nameOk || !membersOk)) {
            domCreateGroupStatus.style.display = '';
            domCreateGroupStatus.textContent = !nameOk
                ? 'Group name is required'
                : 'Select at least one contact';
        } else {
            domCreateGroupStatus.style.display = 'none';
            domCreateGroupStatus.textContent = '';
        }
    }
}

/**
 * Open Create Group tab
 */
function openCreateGroup() {
    // Show panel
    domCreateGroup.style.display = '';
    // Hide others
    domChats.style.display = 'none';
    domChat.style.display = 'none';
    domNavbar.style.display = 'none';

    // Reset state
    arrSelectedGroupMembers = [];
    fCreateGroupAttempt = false;
    if (domCreateGroupName) domCreateGroupName.value = '';
    if (domCreateGroupFilter) domCreateGroupFilter.value = '';
    if (domCreateGroupStatus) {
        domCreateGroupStatus.style.display = 'none';
        domCreateGroupStatus.textContent = '';
    }

    // Render list
    renderCreateGroupList('');
    updateCreateGroupValidation(false);

    // Focus name
    domCreateGroupName?.focus();
}

/**
 * Close Create Group tab and go back to Chat list
 */
async function closeCreateGroup() {
    domCreateGroup.style.display = 'none';
    fCreateGroupAttempt = false;

    // Restore navbar to follow the same flow as "Start New Chat" close (see closeChat())
    domNavbar.style.display = '';

    // Navigate back to chat list
    await openChatlist();

    // Adjust layout after UI visibility changes
    adjustSize();
}

/**
 * Wire up Create Group UI events
 */
/*
Create Group UI wiring
- Validation: Create button disabled until non-empty group name and at least one member selected. See updateCreateGroupValidation() for state sync.
- Loading states:
  • Button text toggles to 'Creating...' and disabled during IPC.
  • Inline status text shows 'Preparing devices...' then 'Finalizing...' on success.
- IPC flow:
  • invoke('create_group_chat', { groupName, memberIds })
    - Backend validates inputs and refreshes each member's device KeyPackage.
    - If any member fails refresh/fetch, backend returns Err with a user-facing string. We surface that string directly via popupConfirm and status label.
  • On success:
    - loadMLSGroups() ensures immediate discoverability in chat list.
    - openChat(newGroupId) navigates to the newly created group.
- Error handling:
  • popupConfirm('Group creation failed', errorString, ...) shows a clear toast/modal.
  • domCreateGroupStatus also mirrors the exact error string for inline context.
  • We do not partially create groups: any device refresh failure aborts.
- Notes:
  • Tauri expects camelCase params from JS for Rust snake_case args.
  • Backend emits 'mls_group_initial_sync' on success; the chat list also refreshes via loadMLSGroups().
*/
(function wireCreateGroupUI() {
    if (!domCreateGroup) return;

    domCreateGroupBackBtn.onclick = closeCreateGroup;
    domCreateGroupCancelBtn.onclick = closeCreateGroup;

    domCreateGroupName.oninput = () => updateCreateGroupValidation(true);
    domCreateGroupFilter.oninput = (e) => renderCreateGroupList(e.target.value || '');

    domCreateGroupCreateBtn.onclick = async () => {
        const groupName = (domCreateGroupName?.value || '').trim();
        const memberIds = [...arrSelectedGroupMembers];

        // Mark that the user attempted to create a group
        fCreateGroupAttempt = true;

        if (!groupName || memberIds.length === 0) {
            updateCreateGroupValidation(true);
            return;
        }

        // Loading state
        const prevTxt = domCreateGroupCreateBtn.textContent;
        domCreateGroupCreateBtn.textContent = 'Creating...';
        domCreateGroupCreateBtn.disabled = true;

        if (domCreateGroupStatus) {
            domCreateGroupStatus.style.display = '';
            domCreateGroupStatus.textContent = 'Preparing devices...';
        }

        try {
            // Backend orchestration: refresh keypackages per member, create group, persist
            // Note: Tauri expects camelCase arg keys for Rust snake_case params.
            const newGroupId = await invoke('create_group_chat', {
                groupName: groupName,
                memberIds: memberIds
            });

            // On success: refresh groups, open the new group chat, and close panel
            if (domCreateGroupStatus) {
                domCreateGroupStatus.textContent = 'Finalizing...';
            }

            // Ensure groups list is current and visible in UI immediately
            await loadMLSGroups();

            // Navigate to the new group
            openChat(newGroupId);

            // Hide panel
            domCreateGroup.style.display = 'none';
        } catch (e) {
            const raw = (e || '').toString();
            // Map backend "no device keypackages" errors to a friendlier UX message
            let friendly = raw;
            let isHtml = false;
            try {
                const m = raw.match(/no device keypackag(?:e|es) found for (\S+)/i);
                if (m && m[1]) {
                    const npub = m[1];
                    const prof = arrProfiles.find(p => p.id === npub);
                    const display = prof?.nickname || prof?.name || 'This user';
                    friendly = `${display} is using an older Vector version!<br>Please ask them to upgrade before inviting them to a Group Chat.`;
                    isHtml = true;
                }
            } catch (_) {}
            popupConfirm('Group creation failed', friendly, true, '', 'vector_warning.svg');
            if (domCreateGroupStatus) {
                domCreateGroupStatus.style.display = '';
                if (isHtml) domCreateGroupStatus.innerHTML = friendly;
                else domCreateGroupStatus.textContent = friendly;
            }
        } finally {
            domCreateGroupCreateBtn.textContent = prevTxt || 'Create';
            updateCreateGroupValidation();
        }
    };
})();


