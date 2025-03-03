const { invoke, convertFileSrc } = window.__TAURI__.core;
const { getVersion } = window.__TAURI__.app;
const { getCurrentWebview } = window.__TAURI__.webview;
const { listen } = window.__TAURI__.event;
const { readImage } = window.__TAURI__.clipboardManager;
const { openUrl } = window.__TAURI__.opener;

// Display the current version
getVersion().then(v => {
    // TODO: re-add this somewhere, settings?
});

const domTheme = document.getElementById('theme');

const domLogin = document.getElementById('login-form');
const domLoginImport = document.getElementById('login-import');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');

const domLoginEncrypt = document.getElementById('login-encrypt');
const domLoginEncryptTitle = document.getElementById('login-encrypt-title');
const domLoginEncryptPinRow = document.getElementById('login-encrypt-pins');

const domChats = document.getElementById('chats');
const domAccount = document.getElementById('account');
const domSyncStatusContainer = document.getElementById('sync-status-container');
const domSyncStatus = document.getElementById('sync-status');
const domChatList = document.getElementById('chat-list');

const domChat = document.getElementById('chat');
const domChatBackBtn = document.getElementById('chat-back-btn');
const domChatContact = document.getElementById('chat-contact');
const domChatContactStatus = document.getElementById('chat-contact-status');
const domChatMessages = document.getElementById('chat-messages');
const domChatMessageBox = document.getElementById('chat-box');
const domChatMessagesScrollReturnBtn = document.getElementById('chat-scroll-return');
const domChatMessageInput = document.getElementById('chat-input');
const domChatMessageInputFile = document.getElementById('chat-input-file');
const domChatMessageInputCancel = document.getElementById('chat-input-cancel');
const domChatMessageInputEmoji = document.getElementById('chat-input-emoji');
const domChatMessageInputVoice = document.getElementById('chat-input-voice');

const domChatNew = document.getElementById('chat-new');
const domChatNewBackBtn = document.getElementById('chat-new-back-btn');
const domShareNpub = document.getElementById('share-npub');
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

const domSettings = document.getElementById('settings');
const domSettingsBtn = document.getElementById('settings-btn');
const domSettingsBackBtn = document.getElementById('settings-back-btn');
const domSettingsThemeSelect = document.getElementById('theme-select');
const domSettingsLogout = document.getElementById('logout-btn');

const domApp = document.getElementById('popup-container');
const domPopup = document.getElementById('popup');
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

        // Setup the picker UI
        /** @type {DOMRect} */
        const rect = (isDefaultPanel ? domChatContact : e.target).getBoundingClientRect();

        // Display and stick it to the right side
        picker.style.display = `block`;
        picker.style.right = `0px`;

        // Compute it's position based on the element calling it (i.e: reactions are a floaty panel)
        const pickerRect = picker.getBoundingClientRect();
        if (isDefaultPanel) {
            // Note: No idea why the 5px extra height is needed, but this prevents the picker from overlapping too much with the chat box
            picker.style.top = `${document.body.clientHeight - pickerRect.height - rect.height - 5}px`
            picker.classList.add('emoji-picker-message-type');
            // Change the emoji button to a wink while the panel is open (removed on close)
            domChatMessageInputEmoji.innerHTML = `<span class="icon icon-wink-face"></span>`;
        } else {
            picker.classList.remove('emoji-picker-message-type');
            const fLargeMessage = rect.y < rect.height;
            const yAxisTarget = fLargeMessage ? rect.y : rect.y - rect.height;
            const yAxisCorrection = fLargeMessage ? 0 : pickerRect.height / 2;
            picker.style.top = `${yAxisTarget + yAxisCorrection}px`;
            // TODO: this could be more intelligent (aim for the 'e.target' location)
            // ... however, you need to compute when the picker will overflow the app
            // ... and prevent it, so, I'm just glue-ing it to the right for now with
            // ... some 'groundwork' code that shouldn't be too hard to modify.
            //picker.style.left = `${document.body.clientWidth - pickerRect.width}px`
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
                spanEmoji.id = 'first-emoji';
                spanEmoji.style.opacity = 1;
            } else {
                spanEmoji.style.opacity = 0.75;
            }
        }
        emojiResults.appendChild(spanEmoji);
        nDisplayedEmojis++;
    }

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
        const domFirstEmoji = document.getElementById('first-emoji');
        const cEmoji = arrEmojis.find(a => a.emoji === domFirstEmoji.textContent);
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
                spanReaction.style.left = `-2px`;
                spanReaction.style.bottom = `-2px`;
                spanReaction.textContent = `${cEmoji.emoji} 1`;

                // Remove the Reaction button
                const divMessage = document.getElementById(cMsg.id);

                // Note: this is basically a shoddy flow to access the `add-reaction` element.
                // DOM Tree: msg-(them/me) -> msg-extras -> add-reaction
                divMessage.lastElementChild.firstElementChild.replaceWith(spanReaction);

                // Send the Reaction to the network
                invoke('react', { referenceId: strCurrentReactionReference, npub: strReceiverPubkey, emoji: cEmoji.emoji });
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
    if (e.target.tagName === 'SPAN') {
        // Register the click in the emoji-dex
        const cEmoji = arrEmojis.find(a => a.emoji === e.target.textContent);
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
                spanReaction.style.left = `-2px`;
                spanReaction.style.bottom = `-2px`;
                spanReaction.textContent = `${cEmoji.emoji} 1`;

                // Remove the Reaction button
                const divMessage = document.getElementById(cMsg.id);
                divMessage.lastElementChild.remove();

                // Append the Decoy Reaction
                divMessage.appendChild(spanReaction);

                // Send the Reaction to the network
                invoke('react', { referenceId: strCurrentReactionReference, npub: strReceiverPubkey, emoji: cEmoji.emoji });
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
 * @property {Message[]} messages - An array of messages associated with the profile.
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
 * @property {Reaction[]} reactions - An array of reactions to this message.
 * @property {number} at - Timestamp when the message was sent.
 * @property {boolean} mine - Indicates if this message was sent by the current user.
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
 * A cache of all chats with linear chronological history
 * @type {Profile[]}
 */
let arrChats = [];

/**
 * The current open chat (by npub)
 */
let strOpenChat = "";

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
}

/**
 * A "thread" function dedicated to refreshing Profile data in the background
 */
async function fetchProfiles() {
    // Poll for changes in profiles
    for (const chat of arrChats) {
        await invoke("load_profile", { npub: chat.id });
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
        if (chat.messages.length === 0) continue;

        // If the chat order changed; append to fragment instead of directly to the DOM for full list re-render efficiency
        const divContact = renderContact(chat);
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
    fragment.lastElementChild.style.marginBottom = `50px`;

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
        domChatList.appendChild(divFade);
    }
}

function renderContact(chat) {
    // The Contact container (The ID is the Contact's npub)
    const divContact = document.createElement('div');
    divContact.classList.add('chatlist-contact');
    divContact.id = chat.id;

    // The Username + Message Preview container
    const divPreviewContainer = document.createElement('div');
    divPreviewContainer.classList.add('chatlist-contact-preview');

    // The avatar, if one exists
    if (chat?.avatar) {
        const imgAvatar = document.createElement('img');
        imgAvatar.src = chat?.avatar;
        divContact.appendChild(imgAvatar);
    } else {
        // Otherwise, generate a Gradient Avatar
        divContact.appendChild(pubkeyToAvatar(chat.id, chat?.name));
    }

    // Add the name (or, if missing metadata, their npub instead) to the chat preview
    const h4ContactName = document.createElement('h4');
    h4ContactName.textContent = chat?.name || chat.id;
    h4ContactName.classList.add('cutoff')
    divPreviewContainer.appendChild(h4ContactName);

    // Display either their Last Message or Typing Indicator
    const cLastMsg = chat.messages[chat.messages.length - 1];
    const pChatPreview = document.createElement('p');
    pChatPreview.classList.add('cutoff');
    const fIsTyping = chat?.typing_until ? chat.typing_until > Date.now() / 1000 : false;
    pChatPreview.classList.toggle('text-gradient', fIsTyping);
    if (fIsTyping) {
        // Typing; display the glowy indicator!
        pChatPreview.textContent = `Typing...`;
    } else if (!cLastMsg.content) {
        // Not typing, and no text; display as an attachment
        pChatPreview.textContent = (cLastMsg.mine ? 'You: ' : '') + 'Sent an attachment';
    } else {
        // Not typing; display their last message
        pChatPreview.textContent = (cLastMsg.mine ? 'You: ' : '') + cLastMsg.content;
    }
    divPreviewContainer.appendChild(pChatPreview);

    // Add the Chat Preview to the contact UI
    // Note: as a hacky trick to make `divContact` receive all clicks, we set the z-index lower on it's children
    divPreviewContainer.style.zIndex = `-1`;
    divContact.appendChild(divPreviewContainer);

    return divContact;
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
 * Send a file via NIP-96 server to a Nostr user
 * @param {string} pubkey - The user's pubkey
 * @param {string?} replied_to - The reference of the message, if any
 * @param {string} filepath - The absolute file path
 */
async function sendFile(pubkey, replied_to, filepath) {
    domChatMessageInput.setAttribute('placeholder', 'Uploading...');
    try {
        // Send the attachment file
        await invoke("file_message", { receiver: pubkey, repliedTo: replied_to, filePath: filepath });
    } catch (e) {
        // Notify of an attachment send failure
        popupConfirm(e, '', true);
    }

    // Reset the placeholder and typing indicator timestamp
    cancelReply();
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
    }
}

/**
 * Setup our Rust Event listeners, used for relaying the majority of backend changes
 */
async function setupRustListeners() {
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

    // Listen for profile updates
    await listen('profile_update', (evt) => {
        // Check if the frontend is already aware
        const nProfileIdx = arrChats.findIndex(p => p.id === evt.payload.id);
        if (nProfileIdx >= 0) {
            // Update our frontend memory
            arrChats[nProfileIdx] = evt.payload;

            // If this is our profile, make sure to render it's changes
            if (arrChats[nProfileIdx].mine) {
                renderCurrentProfile(arrChats[nProfileIdx]);
            }
        } else {
            // Add the new profile
            arrChats.unshift(evt.payload);

            // Sort by last-message-time in case of backwards incremental sync
            arrChats.sort((a, b) => b?.messages[b.messages.length - 1]?.at - a?.messages[a?.messages.length - 1]?.at);
        }
        // If this user has an open chat, then soft-update the chat too
        if (strOpenChat === evt.payload.id) {
            updateChat(arrChats[nProfileIdx], []);
        }
        // Render the Chat List
        renderChatlist();
    });

    // Listen for incoming messages
    await listen('message_new', (evt) => {
        // Grab our profile index (a profile should be guarenteed before it's first message event)
        const nProfileIdx = arrChats.findIndex(p => p.id === evt.payload.chat_id);

        // Double-check we haven't received this twice (unless this is their first message)
        const cFirstMsg = arrChats[nProfileIdx].messages[0];
        if (arrChats[nProfileIdx].messages.length === 1 && cFirstMsg.id === evt.payload.message.id && !cFirstMsg.mine) return;

        // Reset their typing status
        arrChats[nProfileIdx].typing_until = 0;

        // Append new messages and prepend older messages
        if (cFirstMsg.at < evt.payload.message.at) {
            // New message
            arrChats[nProfileIdx].messages.push(evt.payload.message);
            // Move the chat to the top of our chatlist
            if (nProfileIdx > 0) {
                // Remove the profile at index and get it
                const [profile] = arrChats.splice(nProfileIdx, 1);
                // Add it to the beginning
                arrChats.unshift(profile);
            }
        } else {
            // Old message
            arrChats[nProfileIdx].messages.unshift(evt.payload.message);
        }

        // If this user has an open chat, then soft-update the chat too
        if (strOpenChat === evt.payload.chat_id) {
            updateChat(arrChats[0], [evt.payload.message]);
        }

        // Render the Chat List
        renderChatlist();
    });

    // Listen for existing message updates
    await listen('message_update', (evt) => {
        // Find the message we're updating
        const cProfile = arrChats.find(p => p.id === evt.payload.chat_id);
        const nMsgIdx = cProfile.messages.findIndex(m => m.id === evt.payload.old_id);

        // Update it
        cProfile.messages[nMsgIdx] = evt.payload.message;

        // If this user has an open chat, then update the rendered message
        if (strOpenChat === evt.payload.chat_id) {
            // TODO: is there a slight possibility of a race condition here? i.e: `message_update` calls before `message_new` and thus domMsg isn't found?
            const domMsg = document.getElementById(evt.payload.old_id);
            domMsg?.replaceWith(renderMessage(evt.payload.message, cProfile));

            // If the old ID was a pending ID (our message), make sure to update and scroll accordingly
            if (evt.payload.old_id.startsWith('pending')) {
                strLastMsgID = evt.payload.message.id;
                softChatScroll();
            }
        }

        // Render the Chat List
        renderChatlist();
    });
}

/**
 * A flag that indicates when Vector is still in it's initiation sequence
 */
let fInit = true;

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

        // Setup a Rust Listener for the backend's init finish
        await listen('init_finished', (evt) => {
            // Set our full Chat State
            arrChats = evt.payload;

            // Fadeout the login and encryption UI
            domLogin.classList.add('fadeout-anim');
            domLogin.addEventListener('animationend', async () => {
                domLogin.classList.remove('fadeout-anim');
                domLoginInput.value = "";
                domLogin.style.display = 'none';
                domLoginEncrypt.style.display = 'none';

                // Fade-in the settings button
                domSettingsBtn.style.display = '';
                domSettingsBtn.classList.add('fadein-anim');
                domSettingsBtn.addEventListener('animationend', () => domSettingsBtn.classList.remove('fadein-anim'), { once: true });

                // Render our profile with an intro animation
                const cProfile = arrChats.find(p => p.mine);
                renderCurrentProfile(cProfile);
                domAccount.classList.add('fadein-anim');
                domAccount.addEventListener('animationend', () => domAccount.classList.remove('fadein-anim'), { once: true });

                const domProfileDivider = document.getElementById('profile-divider');
                domProfileDivider.classList.add('intro-anim-widen');
                domProfileDivider.addEventListener('animationend', () => domProfileDivider.classList.remove('intro-anim-widen'), { once: true });

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

                // Append and fade-in a "Start New Chat" button
                const btnStartChat = document.createElement('button');
                btnStartChat.classList.add('corner-float', 'visible');
                btnStartChat.style.bottom = `15px`;
                btnStartChat.style.borderRadius = `100%`;
                btnStartChat.style.height = `50px`;
                btnStartChat.style.width = `50px`;
                btnStartChat.innerHTML = '<span class="icon icon-new-msg"></span>';
                btnStartChat.onclick = openNewChat;
                btnStartChat.classList.add('fadein-anim');
                btnStartChat.addEventListener('animationend', () => btnStartChat.classList.remove('fadein-anim'), { once: true });
                domChats.appendChild(btnStartChat);
                adjustSize();

                // Setup a subscription for new websocket messages
                invoke("notifs");
            }, { once: true });
        });

        // Load and Decrypt our database; fetching the full chat state from disk for immediate bootup
        domLoginEncryptTitle.textContent = `Decrypting Database...`;

        // Note: this also begins the Rust backend's iterative sync, thus, init should ONLY be called once, to initiate it
        init();
    }
}

/**
 * Renders the user's own profile UI
 * @param {object} cProfile 
 */
function renderCurrentProfile(cProfile) {
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
        domAvatar = pubkeyToAvatar(strPubkey, cProfile?.name)
    }
    domAvatar.classList.add('btn');
    domAvatar.onclick = askForAvatar;
    divRow.appendChild(domAvatar);

    // Render our username and npub
    const h3Username = document.createElement('h3');
    h3Username.textContent = cProfile?.name || strPubkey.substring(0, 10) + '…';
    h3Username.classList.add('btn', 'cutoff');
    h3Username.onclick = askForUsername;
    divRow.appendChild(h3Username);

    // Add the username row
    domAccount.appendChild(divRow);

    // Render our status
    const iStatus = document.createElement('i');
    iStatus.textContent = cProfile?.status?.title || 'Set a Status';
    iStatus.classList.add('btn', 'cutoff');
    iStatus.onclick = askForStatus;
    domAccount.appendChild(iStatus);

    // Then add a divider to seperate it all visually from the Chatlist
    const divDivider = document.createElement('div');
    divDivider.classList.add('divider');
    divDivider.id = `profile-divider`;
    domAccount.appendChild(divDivider);

    // Render our Share npub
    domShareNpub.textContent = strPubkey;
}

/**
 * Display the Encryption/Decryption flow, depending on the passed options
 * @param {string} pkey - A private key to encrypt
 * @param {boolean} fUnlock - Whether we're unlocking an existing key, or encrypting the given one
 */
function openEncryptionFlow(pkey, fUnlock = false) {
    domLoginImport.style.display = 'none';
    domLoginEncrypt.style.display = '';

    // Track our pin entries ('Current' is what the user has currently typed)
    // ... while 'Last' holds a previous pin in memory for typo-checking purposes.
    let strPinLast = [];
    let strPinCurrent = Array(6).fill('-');

    // If we're unlocking - display that
    if (fUnlock) domLoginEncryptTitle.textContent = `Enter your Decryption Pin`;

    // Track our pin inputs
    const arrPinDOMs = document.querySelectorAll(`.pin-row input`);
    arrPinDOMs.item(0).focus();
    for (const domPin of arrPinDOMs) {
        domPin.addEventListener('input', async function (e) {
            this.value = this.value.replace(/[^0-9]/g, '');
            if (this.value) {
                // Find the index of this pin entry
                const nIndex = Number(this.id.slice(-1));

                // Focus the next entry
                const domNextEntry = arrPinDOMs.item(nIndex + 1);
                if (domNextEntry) domNextEntry.focus();
                else arrPinDOMs.item(0).focus();

                // Set the current digit entry
                strPinCurrent[nIndex] = this.value;

                // Figure out which Pin Array we're working with
                if (strPinLast.length === 0) {
                    // There's no set pin, so we're still setting the initial one
                    // Check if we've filled this pin entry
                    if (!strPinCurrent.includes(`-`)) {
                        if (fUnlock) {
                            // Attempt to decrypt our key with the pin
                            domLoginEncryptTitle.textContent = `Decrypting your keys...`;
                            domLoginEncryptTitle.classList.add('text-gradient');
                            domLoginEncryptPinRow.style.display = `none`;
                            try {
                                const decryptedPkey = await loadAndDecryptPrivateKey(strPinCurrent.join(''));
                                const { public, _private } = await invoke("login", { importKey: decryptedPkey });
                                strPubkey = public;
                                login();
                            } catch (e) {
                                // Decrypt failed - let's re-try
                                domLoginEncryptPinRow.style.display = ``;
                                domLoginEncryptTitle.textContent = `Incorrect pin, try again`;
                            }
                        } else {
                            // No more empty entries - let's reset for typo checking!
                            strPinLast = [...strPinCurrent];
                            domLoginEncryptTitle.textContent = `Re-enter your Pin`;
                        }

                        // Wipe the current digits
                        for (const domPinToReset of arrPinDOMs) domPinToReset.value = ``;
                        strPinCurrent = Array(6).fill('-');
                    }
                } else {
                    // There's a pin set - let's make sure the re-type matches
                    if (!strPinCurrent.includes(`-`)) {
                        // Do they match?
                        const fMatching = strPinLast.every((char, idx) => char === strPinCurrent[idx]);
                        if (fMatching) {
                            // Encrypt and proceed
                            domLoginEncryptTitle.textContent = `Encrypting your keys...`;
                            domLoginEncryptTitle.classList.add('text-gradient');
                            domLoginEncryptPinRow.style.display = `none`;
                            await saveAndEncryptPrivateKey(pkey, strPinLast.join(''));
                            login();
                        } else {
                            // Wrong pin! Let's start again
                            domLoginEncryptTitle.textContent = `Pin doesn't match, re-try`;
                            strPinCurrent = Array(6).fill(`-`);
                            strPinLast = [];
                        }
                        // Reset the pin inputs
                        for (const domPinToReset of arrPinDOMs) domPinToReset.value = ``;
                    }
                }
            }
        });
    }
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
 * @param {Profile} profile 
 * @param {Array<Message>} arrMessages - The messages to efficiently append/prepend to the chat
 * @param {boolean} fClicked - Whether the chat was opened manually or not
 */
async function updateChat(profile, arrMessages = [], fClicked = false) {
    if (profile?.messages.length || arrMessages.length) {
        // Prefer displaying their name, otherwise, npub
        domChatContact.textContent = profile?.name || strOpenChat.substring(0, 10) + '…';

        // Display either their Status or Typing Indicator
        const fIsTyping = profile?.typing_until ? profile.typing_until > Date.now() / 1000 : false;
        domChatContactStatus.textContent = fIsTyping ? `${profile?.name || 'User'} is typing...` : profile?.status?.title || '';
        domChatContactStatus.classList.toggle('text-gradient', fIsTyping);

        // Adjust our Contact Name class to manage space according to Status visibility
        domChatContact.classList.toggle('chat-contact', !domChatContactStatus.textContent);
        domChatContact.classList.toggle('chat-contact-with-status', !!domChatContactStatus.textContent);

        if (!arrMessages.length) return;

        // If it doesn't already exist: add the fadeout to the top of the message list
        if (!document.getElementById('msg-top-fade')) {
            const divFade = document.createElement('div');
            divFade.id = `msg-top-fade`;
            divFade.classList.add(`fadeout-top-msgs`);
            divFade.style.top = domChatMessages.offsetTop + 'px';
            domChatMessages.appendChild(divFade);
        }

        // Efficiently append or prepend messages based on their time relative to the chat
        let cLastMsg = arrMessages.length > 1 ? arrMessages[0] : profile.messages.find(m => m.id === domChatMessages?.lastElementChild?.id);
        let nLastMsgTime = cLastMsg?.at || Date.now() / 1000;
        for (const msg of arrMessages) {
            // If the last message was over 10 minutes ago, add an inline timestamp
            if (msg.at - nLastMsgTime > 600) {
                nLastMsgTime = msg.at;
                const pTimestamp = document.createElement('p');
                pTimestamp.classList.add('msg-inline-timestamp');
                const messageDate = new Date(msg.at * 1000);

                // Render the time contextually
                if (isToday(messageDate)) {
                    pTimestamp.textContent = messageDate.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true });
                } else if (isYesterday(messageDate)) {
                    pTimestamp.textContent = `Yesterday, ${messageDate.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true })}`;
                } else {
                    pTimestamp.textContent = messageDate.toLocaleString();
                }
                domChatMessages.appendChild(pTimestamp);
            }

            const domMsg = renderMessage(msg, profile);
            if (!cLastMsg || cLastMsg.at < msg.at) {
                // If the message is newer than the last, append it
                // Note: if it's an incoming single new message, then we animate it too
                if (!msg.mine && arrMessages.length === 1) domMsg.classList.add('new-anim');
                domChatMessages.appendChild(domMsg);
            } else {
                // Otherwise, these are older messages, prepend them
                domChatMessages.prepend(domMsg);
            }
        }

        // Auto-scroll on new messages (if the user hasn't scrolled up, or on manual chat open)
        const pxFromBottom = domChatMessages.scrollHeight - domChatMessages.scrollTop - domChatMessages.clientHeight;
        if (pxFromBottom < 500 || fClicked) {
            const cLastMsg = profile.messages[profile.messages.length - 1];
            if (strLastMsgID !== cLastMsg.id || fClicked) {
                strLastMsgID = cLastMsg.id;
                adjustSize();
                // Force an auto-scroll, given soft-scrolling won't accurately work when the entire list has just rendered
                scrollToBottom(domChatMessages, false);
            }
        }
    } else {
        // Probably a 'New Chat', as such, we'll mostly render an empty chat
        domChatContact.textContent = profile?.name || strOpenChat.substring(0, 10) + '…';

        // Force wipe the 'Status' and it's styling
        domChatContactStatus.textContent = '';
        domChatContact.classList.add('chat-contact');
        domChatContact.classList.remove('chat-contact-with-status');
    }

    adjustSize();
}

/**
 * Convert a Message in to a rendered HTML Element
 * @param {Message} msg - the Message to be converted
 * @param {Profile} sender - the Profile of the message sender
 */
function renderMessage(msg, sender) {
    // Construct the message container (the DOM ID is the HEX Nostr Event ID)
    const divMessage = document.createElement('div');
    divMessage.id = msg.id;
    // Render it appropriately depending on who sent it
    divMessage.classList.add('msg-' + (msg.mine ? 'me' : 'them'));
    // Render their avatar, if they have one
    if (!msg.mine && sender?.avatar) {
        const imgAvatar = document.createElement('img');
        imgAvatar.classList.add('avatar');
        imgAvatar.src = sender.avatar;
        divMessage.appendChild(imgAvatar);
    }

    // Prepare the message container
    const pMessage = document.createElement('p');

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
        // Try to find the referenced message
        const cMsg = sender.messages.find(m => m.id === msg.replied_to);
        if (cMsg) {
            // Render the reply in a quote-like fashion
            const divRef = document.createElement('div');
            divRef.classList.add('msg-reply', 'btn');
            divRef.id = `r-${cMsg.id}`;

            // Name + Message
            const spanName = document.createElement('span');
            spanName.style.color = `rgba(255, 255, 255, 0.7)`;
            const spanRef = document.createElement('span');
            spanRef.style.color = `rgba(255, 255, 255, 0.45)`;

            // Name
            const cSenderProfile = !cMsg.mine ? sender : arrChats.find(a => a.mine);
            spanName.textContent = cSenderProfile.name ? cSenderProfile.name : cSenderProfile.id.substring(0, 10) + '…';

            // Replied-to content (Text or Attachment)
            if (cMsg.content)
                spanRef.textContent = cMsg.content.length < 50 ? cMsg.content : cMsg.content.substring(0, 50) + '…';
            else if (cMsg.attachments.length) spanRef.textContent = `Attachment`;

            divRef.appendChild(spanName);
            divRef.appendChild(document.createElement('br'));
            divRef.appendChild(spanRef);
            pMessage.appendChild(divRef);
        }
    }

    // Render the text - if it's emoji-only and/or file-only, and less than four emojis, format them nicely
    const spanMessage = document.createElement('span');
    if (fEmojiOnly) {
        // Strip out unnecessary whitespace
        spanMessage.textContent = strEmojiCleaned;
        // Add an emoji-only CSS format
        pMessage.classList.add('emoji-only');
        spanMessage.classList.add('emoji-only-content');
        // Align the emoji depending on who sent it
        spanMessage.style.textAlign = msg.mine ? 'right' : 'left';
    } else {
        // Render their text content (using our custom Markdown renderer)
        // NOTE: the input IS HTML-sanitised, however, heavy auditing of the sanitisation method should be done, it is a bit sketchy
        spanMessage.innerHTML = parseMarkdown(msg.content.trim());
    }

    // Append the message contents
    pMessage.appendChild(spanMessage);

    // Append attachments
    if (msg.attachments.length) {
        // Float the content depending on who's it is
        pMessage.style.float = msg.mine ? 'right' : 'left';
        // Remove any message bubbles
        pMessage.classList.add('no-background');
    }
    for (const cAttachment of msg.attachments) {
        if (cAttachment.downloaded) {
            // Convert the absolute file path to a Tauri asset
            const assetUrl = convertFileSrc(cAttachment.path);

            // Render the attachment appropriately for it's type
            if (['png', 'jpeg', 'jpg', 'gif', 'webp'].includes(cAttachment.extension)) {
                // Images
                const imgPreview = document.createElement('img');
                imgPreview.style.width = `100%`;
                imgPreview.style.height = `auto`;
                imgPreview.style.borderRadius = `0`;
                imgPreview.src = assetUrl;
                pMessage.appendChild(imgPreview);
            } else if (['wav', 'mp3'].includes(cAttachment.extension)) {
                // Audio
                const audPreview = document.createElement('audio');
                audPreview.setAttribute('controlsList', 'nodownload');
                audPreview.controls = true;
                audPreview.preload = 'metadata';
                audPreview.src = assetUrl;
                // When the metadata loads, we run some maintenance tasks
                audPreview.addEventListener('loadedmetadata', () => {
                    // Auto-scroll to correct against the longer container
                    softChatScroll();
                }, { once: true });
                pMessage.appendChild(audPreview);
            } else if (['mp4', 'mov', 'webm'].includes(cAttachment.extension)) {
                // Videos
                const vidPreview = document.createElement('video');
                vidPreview.setAttribute('controlsList', 'nodownload');
                vidPreview.controls = true;
                vidPreview.style.width = `100%`;
                vidPreview.style.height = `auto`;
                vidPreview.style.borderRadius = `0`;
                vidPreview.style.cursor = `pointer`;
                vidPreview.preload = "metadata";
                vidPreview.playsInline = true;
                vidPreview.src = assetUrl;
                // When the metadata loads, we run some maintenance tasks
                vidPreview.addEventListener('loadedmetadata', () => {
                    // Seek a tiny amount to force the frame 'poster' to load, without loading the entire video
                    vidPreview.currentTime = 0.1;
                    // Auto-scroll to correct against the longer container
                    softChatScroll();
                }, { once: true });
                pMessage.appendChild(vidPreview);
            } else {
                // Unknown attachment
                const iUnknown = document.createElement('i');
                iUnknown.classList.add('text-gradient');
                iUnknown.textContent = `Previews not supported for "${cAttachment.extension}" files yet`;
                pMessage.appendChild(iUnknown);
            }
        } else {
            // Display download prompt UI
        }
    }

    // Append Payment Shortcuts (i.e: Bitcoin Payment URIs, etc)
    const cAddress = detectCryptoAddress(msg.content);
    if (cAddress) {
        // Render the Payment UI
        pMessage.appendChild(renderCryptoAddress(cAddress));
    }

    // Append Metadata Previews (i.e: OpenGraph data from URLs, etc)
    if (!msg.pending && !msg.failed) {
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
                invoke("fetch_msg_metadata", { npub: sender.id, msgId: msg.id });
            }
        }
    }

    // If the message is pending or failed, let's adjust it
    if (msg.pending) {
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
    } else if (!msg.mine) {
        // No reaction on the contact's message, so let's display the 'Add Reaction' UI
        spanReaction = document.createElement('span');
        spanReaction.textContent = `☻`;
        spanReaction.classList.add('add-reaction', 'hideable');
    }

    // Construct our "extras" (reactions, reply button, etc)
    // TODO: placeholder style, looks awful, but works!
    const divExtras = document.createElement('div');
    divExtras.classList.add('msg-extras');

    // These can ONLY be shown on fully sent messages (inherently does not apply to received msgs)
    if (!msg.pending && !msg.failed) {
        // Reactions
        if (spanReaction) {
            if (msg.mine) {
                // My message: reactions on the left
                spanReaction.style.left = '5px';
            } else {
                // Their message: reactions on the right
                spanReaction.style.left = '-2px';
                spanReaction.style.bottom = '-2px';
            }
            divExtras.append(spanReaction);
        } else {
            // No reactions: just render the message
            divMessage.appendChild(pMessage);
        }

        // Reply Icon (if we're not already replying!)
        if (!fReplying) {
            const spanReply = document.createElement('span');
            spanReply.classList.add('reply-btn', 'hideable');
            spanReply.textContent = `R`;
            divExtras.append(spanReply);
        }
    }

    // Depending on who it is: render the extras appropriately
    if (msg.mine) divMessage.append(divExtras, pMessage);
    else divMessage.append(pMessage, divExtras);

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
        document.getElementById(strCurrentReplyReference).querySelector('p').style.borderColor = ``;
    }

    // Remove the reply ID
    strCurrentReplyReference = '';
}

/**
 * Open a chat with a particular contact
 * @param {string} contact 
 */
function openChat(contact) {
    // Display the Chat UI
    domChatNew.style.display = 'none';
    domChats.style.display = 'none';
    domChat.style.display = '';
    domSettingsBtn.style.display = 'none';

    // Render the current contact's messages
    const cProfile = arrChats.find(p => p.id === contact);
    strOpenChat = contact;

    // TODO: enable procedural rendering when the user scrolls up, this is a temp renderer optimisation
    updateChat(cProfile, (cProfile?.messages || []).slice(-50), true);
}

/**
 * Open the dialog for starting a new chat
 */
function openNewChat() {
    // Display the UI
    domChatNew.style.display = '';
    domSettingsBtn.style.display = 'none';
    domChats.style.display = 'none';
    domChat.style.display = 'none';
}

/**
 * Closes the current chat, taking the user back to the chat list
 */
function closeChat() {
    // Attempt to completely release memory (force garbage collection...) of in-chat media
    while (domChatMessages.firstElementChild) {
        const domChild = domChatMessages.firstElementChild;

        // For media (images, audio, video); we ensure they're fully unloaded
        const domMedias = domChild?.querySelectorAll('img, audio, video');
        for (const domMedia of domMedias) {
            // Streamable media (audio + video) should be paused, then force-unloaded
            if (domMedia instanceof HTMLMediaElement) {
                domMedia.pause();
                domMedia.src = ``;
                domMedia.load();
            }
            // Static media (images) should simply be unloaded
            if (domMedia instanceof HTMLImageElement) {
                domMedia.src = ``;
            }
        }

        // Now we explicitly drop them
        domChild.remove();
    }

    // Reset the chat UI
    domChats.style.display = '';
    domSettingsBtn.style.display = '';
    domChatNew.style.display = 'none';
    domChat.style.display = 'none';
    strOpenChat = "";
    nLastTypingIndicator = 0;

    // Cancel any ongoing replies or selections
    strCurrentReactionReference = "";
    strCurrentReplyReference = "";
    cancelReply();

    // Ensure the chat list re-adjusts to fit
    adjustSize();
}

function openSettings() {
    domSettings.style.display = '';
    domSettingsBtn.style.display = 'none';

    // Close the Chat UI
    domChats.style.display = 'none';
}

function closeSettings() {
    domSettings.style.display = 'none';
    domSettingsBtn.style.display = '';

    // Open the Chat UI
    domChats.style.display = '';
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

    // Immediately load and apply theme settings
    const strTheme = await invoke('get_theme');
    if (strTheme) {
        await setTheme(strTheme);
    }

    // If a local encrypted key exists, boot up the decryption UI
    if (await hasKey()) {
        // Check the DB is at least Version 1: otherwise, it's using old & inferior encryption
        // TODO: nuke this by v1.0? Very few users are affected by the earliest VectorDB changes
        if (await invoke('get_db_version') < 1) {
            // Nuke old Private Key
            await invoke('remove_setting', { key: 'pkey' });
            // Alert user
            await popupConfirm('Sorry! 👉👈', `I upgraded the DB with Profile + Message Storage and a 6-pin system.<br><br>You'll have to login again, but this should be the last time! (No promises)<br>- JSKitty`, true);
        } else {
            // Private Key is available and we have a good DB version, login screen!
            openEncryptionFlow(null, true);
        }
    }

    // By this point, it should be safe to set our DB version
    await invoke('set_db_version', { version: 1 });

    // Hook up our static buttons
    domSettingsBtn.onclick = openSettings;
    domSettingsBackBtn.onclick = closeSettings;
    domLoginBtn.onclick = async () => {
        // Import and derive our keys
        try {
            const { public, private } = await invoke("login", { importKey: domLoginInput.value.trim() });
            strPubkey = public;
            // Open the Encryption Flow
            openEncryptionFlow(private);
        } catch (e) {
            // Display the backend error
            popupConfirm(e, '', true);
        }
    }
    domChatBackBtn.onclick = closeChat;
    domChatNewBackBtn.onclick = closeChat;
    domChatNewStartBtn.onclick = () => {
        openChat(domChatNewInput.value.trim());
        domChatNewInput.value = ``;
    };
    domChatMessageInputCancel.onclick = cancelReply;

    // Hook up a scroll handler in the chat to display UI elements at certain scroll depths
    createScrollHandler(domChatMessages, domChatMessagesScrollReturnBtn, { threshold: 500 })

    // Hook up an in-chat File Upload listener
    domChatMessageInputFile.onclick = async () => {
        let filepath = await selectFile();
        if (filepath) {
            await sendFile(strOpenChat, strCurrentReplyReference, filepath);
        }
    };

    // Hook up an in-chat File Paste listener
    document.onpaste = async (evt) => {
        if (strOpenChat) {
            for (const item of evt.clipboardData.items) {
                // Check if the pasted content is an image
                if (item.type.startsWith('image/')) {
                    const blob = await readImage();
                    if (blob) {
                        // Placeholder
                        domChatMessageInput.value = '';
                        domChatMessageInput.setAttribute('placeholder', 'Sending...');

                        // Convert Tauri Clipboard blob to image components
                        const rgba = await blob.rgba();
                        const size = await blob.size();

                        // Send raw bytes to Rust
                        await invoke('paste_message', {
                            receiver: strOpenChat,
                            repliedTo: strCurrentReplyReference,
                            pixels: rgba,
                            width: size.width,
                            height: size.height
                        });

                        // Reset placeholder
                        cancelReply();
                        nLastTypingIndicator = 0;
                    }
                }
            }
        }
    }

    // Hook up an 'Enter' listener on the Message Box for sending messages
    domChatMessageInput.onkeydown = async (evt) => {
        // Allow 'Shift + Enter' to create linebreaks, while only 'Enter' sends a message
        if ((evt.code === 'Enter' || evt.code === 'NumpadEnter') && !evt.shiftKey) {
            evt.preventDefault();
            if (domChatMessageInput.value.trim().length) {
                // Cache the message and previous Input Placeholder
                const strMessage = domChatMessageInput.value;

                // Send the message, and display "Sending..." as the placeholder
                domChatMessageInput.value = '';
                domChatMessageInput.setAttribute('placeholder', 'Sending...');
                try {
                    await message(strOpenChat, strMessage, strCurrentReplyReference, "");
                } catch(_) {}

                // Reset the placeholder and typing indicator timestamp
                cancelReply();
                nLastTypingIndicator = 0;
            }
        } else {
            // Send a Typing Indicator
            if (nLastTypingIndicator + 30000 < Date.now()) {
                nLastTypingIndicator = Date.now();
                await invoke("start_typing", { receiver: strOpenChat });
            }
        }
    };

    // Hook up our drag-n-drop listeners
    await getCurrentWebview().onDragDropEvent(async (event) => {
        // Only accept File Drops if a chat is open
        if (strOpenChat) {
            if (event.payload.type === 'over') {
                // TODO: add hover effects
            } else if (event.payload.type === 'drop') {
                await sendFile(strOpenChat, strCurrentReplyReference, event.payload.paths[0]);
            } else {
                // TODO: remove hover effects
            }
        }
    });

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
                        await invoke('voice_message', {
                            receiver: strOpenChat,
                            repliedTo: strCurrentReplyReference,
                            bytes: wavData
                        });
                    } catch (e) {
                        // Notify of an attachment send failure
                        popupConfirm(e, '', true);
                    }

                    // Reset placeholder
                    cancelReply();
                    nLastTypingIndicator = 0;
                }
            }
        } else {
            // Display our recording status
            domChatMessageInput.value = '';
            domChatMessageInput.setAttribute('placeholder', 'Recording...');

            // Start recording
            await recorder.start();
        }
    });
});

// Listen for app-wide click interations
document.addEventListener('click', (e) => {
    // If we're clicking the emoji search, don't close it!
    if (e.target === emojiSearch) return;

    // If we're clicking a Reply button, begin a reply
    if (e.target.classList.contains("reply-btn")) return selectReplyingMessage(e);

    // If we're clicking a Reply context, center the referenced message in view
    if (e.target.classList.contains('msg-reply') || e.target.parentElement?.classList.contains('msg-reply')) {
        // Note: The `substring(2)` removes the `r-` prefix
        const strID = e.target.id || e.target.parentElement.id;
        const domMsg = document.getElementById(strID.substring(2));
        centerInView(domMsg);

        // Run an animation to bring the user's eye to the message
        domMsg.classList.add('highlight-animation');
        return setTimeout(() => domMsg.classList.remove('highlight-animation'), 1500);
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

    // If we're clicking a Contact, open the chat with the embedded npub (ID)
    if (e.target.classList.contains("chatlist-contact")) return openChat(e.target.id);

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
    // Note: no idea why the `- 30px` is needed below, magic numbers, I guess.
    const rectAccount = domAccount.getBoundingClientRect();
    const rectSyncStatus = domSyncStatusContainer.getBoundingClientRect();
    domChatList.style.maxHeight = (window.innerHeight - (rectAccount.height + rectSyncStatus.height)) - 30 + `px`;

    // Chat Box: resize the chat to fill the remaining space after the upper Contact area (name)
    const rectContact = domChatContact.getBoundingClientRect();
    domChat.style.height = (window.innerHeight - rectContact.height) + `px`;

    // If the chat is open, and the fade-out exists, then position it correctly
    if (strOpenChat) {
        const divFade = document.getElementById('msg-top-fade');
        if (divFade) {
            divFade.style.top = domChatMessages.offsetTop + 'px';
        }
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