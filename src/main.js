const { invoke } = window.__TAURI__.core;
const { getVersion } = window.__TAURI__.app;

const domVersion = document.getElementById('version');

// Display the current version
getVersion().then(v => {
    domVersion.textContent += `v${v}`;
});

const domLogin = document.getElementById('login-form');
const domLoginImport = document.getElementById('login-import');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');

const domLoginEncrypt = document.getElementById('login-encrypt');
const domLoginEncryptTitle = document.getElementById('login-encrypt-title');
const domLoginEncryptPinRow = document.getElementById('login-encrypt-pins');

const domChats = document.getElementById('chats');
const domAccount = document.getElementById('account');
const domChatList = document.getElementById('chat-list');

const domChat = document.getElementById('chat');
const domChatBackBtn = document.getElementById('chat-back-btn');
const domChatContact = document.getElementById('chat-contact');
const domChatContactStatus = document.getElementById('chat-contact-status');
const domChatMessages = document.getElementById('chat-messages');
const domChatMessageBox = document.getElementById('chat-box');
const domChatMessageInput = document.getElementById('chat-input');
const domChatMessageInputEmoji = document.getElementById('chat-input-emoji');

const domChatNew = document.getElementById('chat-new');
const domChatNewBackBtn = document.getElementById('chat-new-back-btn');
const domShareNpub = document.getElementById('share-npub');
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

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
    const isDefaultPanel = e.target === domChatMessageInputEmoji;

    // Open or Close the panel depending on it's state
    const fReaction = e.target.hasAttribute('reaction');
    const fClickedInputOrReaction = isDefaultPanel || fReaction;
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
        const rect = (isDefaultPanel ? domChatMessageBox : e.target).getBoundingClientRect();

        // Display and stick it to the right side
        picker.style.display = `block`;
        picker.style.right = `0px`;

        // Compute it's position based on the element calling it (i.e: reactions are a floaty panel)
        const pickerRect = picker.getBoundingClientRect();
        if (isDefaultPanel) {
            picker.style.top = `${document.body.clientHeight - pickerRect.height - rect.height + 5}px`
            picker.classList.add('emoji-picker-message-type');
        } else {
            picker.classList.remove('emoji-picker-message-type');
            picker.style.top = `${rect.y - rect.height + (pickerRect.height / 2) + 10}px`;
            // TODO: this could be more intelligent (aim for the 'e.target' location)
            // ... however, you need to compute when the picker will overflow the app
            // ... and prevent it, so, I'm just glue-ing it to the right for now with
            // ... some 'groundwork' code that shouldn't be too hard to modify.
            //picker.style.left = `${document.body.clientWidth - pickerRect.width}px`
        }

        // If this is a Reaction, let's cache the Reference ID
        if (fReaction) {
            // Message IDs are stored on the parent of the React button
            strCurrentReactionReference = e.target.parentElement.id;
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
    }
}

// Listen for Emoji Picker interactions
document.addEventListener('click', (e) => {
    openEmojiPanel(e);
});

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
    if (e.code === 'Enter') {
        e.preventDefault();

        // Register the selection in the emoji-dex
        const domFirstEmoji = document.getElementById('first-emoji');
        const cEmoji = arrEmojis.find(a => a.emoji === domFirstEmoji.textContent);
        cEmoji.used++;

        // If this is a Reaction - let's send it!
        if (strCurrentReactionReference) {
            // Grab the referred message to find it's chat pubkey
            for (const cChat of arrChats) {
                const cMsg = cChat.contents.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cMsg.contact;

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
                invoke('react', { referenceId: strCurrentReactionReference, chatPubkey: strReceiverPubkey, emoji: cEmoji.emoji });
            }
        } else {
            // Add it to the message input
            domChatMessageInput.value += cEmoji.emoji;
        }

        // Reset the UI state
        emojiSearch.value = '';
        picker.style.display = ``;
        strCurrentReactionReference = '';

        // Bring the focus back to the chat
        domChatMessageInput.focus();
    } else if (e.code === 'Escape') {
        // Close the dialog
        emojiSearch.value = '';
        picker.style.display = ``;
        strCurrentReactionReference = '';

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
                const cMsg = cChat.contents.find(a => a.id === strCurrentReactionReference);
                if (!cMsg) continue;

                // Found the message!
                const strReceiverPubkey = cMsg.contact;

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
                invoke('react', { referenceId: strCurrentReactionReference, chatPubkey: strReceiverPubkey, emoji: cEmoji.emoji });
            }
        } else {
            // Add it to the message input
            domChatMessageInput.value += cEmoji.emoji;
        }

        // Reset the UI state
        emojiSearch.value = '';
        picker.classList.remove('active');
        strCurrentReactionReference = '';

        // Bring the focus back to the chat
        domChatMessageInput.focus();
    }
});

/**
 * @typedef {Object} Message
 * @property {string} content - The content of the message.
 * @property {string} contact - The identifier of the contact.
 * @property {number} at - The timestamp of the message.
 * @property {boolean} mine - Whether the message was sent by us, or them.
 */

/**
 * @typedef {Object} Chat
 * @property {string} contact - The id of the contact.
 * @property {Message[]} contents - Array of messages associated with the contact.
 */

/**
 * Organizes an array of Message objects by contact into an array of Chat objects.
 * Each contact in the Chat array has an array of associated message contents.
 *
 * @param {Message[]} data - The data to be sorted.
 * @returns {Chat} - The organized data.
 */
function sortTocontact(data) {
    // Sort the messages in ascending order of timestamps
    data.sort((a, b) => a.at - b.at);

    // Create an empty object to collect contact data for sorting
    let contactData = {};

    // Iterate through every item in the data array
    data.forEach(item => {

        // If the contact doesn't exist in contactData yet, create a new array for them
        if (!(item.contact in contactData)) {
            contactData[item.contact] = [];
        }

        // Add the message to the chat data
        contactData[item.contact].push(item);
    });

    // Create an array of Chats from contactData
    return Object.entries(contactData).map(([contact, contents]) => ({ contact, contents }));
}

/**
 * A cache of all chats with linear chronological history
 * @type {Chat[]}
 */
let arrChats = [];

/**
 * A cache of all profile metadata for folks we've chat with
 */
let arrProfiles = [];

/**
 * The current open chat (by npub)
 */
let strOpenChat = "";

/**
 * Fetch all messages from the client
 * 
 * **Note:** Setting 'init' simply starts an automatic re-call every half-second
 * to emulate a "live" feed, this could probably be improved later.
 * 
 * **Note:** Only the first call actually calls to the Nostr network, all 
 * consecutive calls utilise cache, which is updated by the event (notify) system.
 * 
 * @param {boolean} init - Whether this is an Init call or not
 */
async function fetchMessages(init = false) {
    if (init) {
        domChatList.textContent = `Loading DMs...`;
    }
    const arrMessages = await invoke("fetch_messages");

    // Sort our linear message history in to Chats
    arrChats = sortTocontact(arrMessages);

    // Now sort our Chat history by descending time since last message
    arrChats.sort((a, b) => b.contents[b.contents.length - 1].at - a.contents[a.contents.length - 1].at);

    // Render the chats (if the backend signals a state change)
    const fStateChanged = await invoke('has_state_changed');
    if (!fStateChanged) return;

    // If a chat is open, update it's messages
    if (strOpenChat) {
        updateChat(strOpenChat);
    }

    domChatList.innerHTML = ``;
    for (const chat of arrChats) {
        // Let's try to load the profile of each chat, too
        let cProfile = arrProfiles.find(a => a.id === chat.contact);
        if (!cProfile) {
            try {
                if (init) {
                    domChatList.textContent = `Loading Contact Profile...`;
                }
                cProfile = await invoke("load_profile", { npub: chat.contact });
                arrProfiles.push(cProfile);
            } catch (e) {
                arrProfiles.push({ id: chat.contact, name: '', avatar: '' });
            }
        }
        // The Contact container
        const divContact = document.createElement('div');
        divContact.classList.add('chatlist-contact');
        divContact.onclick = () => { openChat(chat.contact) };

        // The Username + Message Preview container
        const divPreviewContainer = document.createElement('div');
        divPreviewContainer.classList.add('chatlist-contact-preview');

        // The avatar, if one exists
        if (cProfile?.avatar) {
            const imgAvatar = document.createElement('img');
            imgAvatar.src = cProfile.avatar;
            divContact.appendChild(imgAvatar);
        } else {
            // Otherwise, add some left-margin compensation to keep them aligned
            divPreviewContainer.style.marginLeft = `50px`;
        }

        // Add the name (or, if missing metadata, their npub instead) to the chat preview
        const h4ContactName = document.createElement('h4');
        h4ContactName.textContent = cProfile?.name || chat.contact;
        divPreviewContainer.appendChild(h4ContactName);

        // Add the last message to the chat preview
        const cLastMsg = chat.contents[chat.contents.length - 1];
        const pChatPreview = document.createElement('p');
        pChatPreview.textContent = cLastMsg ? (cLastMsg.mine ? 'You: ' : '') + cLastMsg.content : '...';
        divPreviewContainer.appendChild(pChatPreview);

        // Add the Chat Preview to the contact UI
        divContact.appendChild(divPreviewContainer);

        // Finally, add the full contact to the list
        domChatList.appendChild(divContact);
    }

    // Acknowledge the state change (thus, preventing re-renders when there's nothing new to render)
    if (!init) await invoke('acknowledge_state_change');

    // Start a post-init refresh loop, which will frequently poll cached chats from the client
    if (init) setInterval(fetchMessages, 500);
}

/**
 * Send a NIP-17 message to a Nostr user
 * @param {string} pubkey - The user's pubkey
 * @param {string} content - The content of the message
 */
async function message(pubkey, content) {
    await invoke("message", { receiver: pubkey, content: content });
}

/**
 * Login to the Nostr network
 */
async function login() {
    if (strPubkey) {
        // Hide the login and encryption UI
        domLoginInput.value = "";
        domLogin.style.display = 'none';
        domLoginEncrypt.style.display = 'none';

        // Connect to Nostr
        domChatList.textContent = `Connecting to Nostr...`;
        await invoke("connect");

        // Attempt to sync our profile data
        domChatList.textContent = `Syncing your profile...`;
        let cProfile;
        try {
            cProfile = await invoke("load_profile", { npub: strPubkey });
            arrProfiles.push(cProfile);
        } catch (e) {
            arrProfiles.push({ id: strPubkey, name: '', avatar: '', mine: true });
        }

        // Render our avatar (if we have one)
        if (cProfile?.avatar) {
            const imgAvatar = document.createElement('img');
            imgAvatar.src = cProfile.avatar;
            domAccount.appendChild(imgAvatar);
        }

        // Render our username and npub
        const h3Username = document.createElement('h3');
        h3Username.textContent = cProfile?.name || strPubkey.substring(0, 10) + '…';
        domShareNpub.textContent = strPubkey;
        domAccount.appendChild(h3Username);

        // Connect and fetch historical messages
        await fetchMessages(true);

         // Append a "Start New Chat" button
        const btnStartChat = document.createElement('button');
        btnStartChat.textContent = "Start New Chat";
        btnStartChat.onclick = openNewChat;
        domChats.appendChild(btnStartChat);

        // Setup a subscription for new websocket messages
        invoke("notifs");
    }
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
    let strPinCurrent = Array(5).fill('-');

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
                        strPinCurrent = Array(5).fill('-');
                    }
                } else {
                    // There's a pin set - let's make sure the re-type matches
                    if (!strPinCurrent.includes(`-`)) {
                        // Do they match?
                        const fMatching = strPinLast.every((char, idx) => char === strPinCurrent[idx]);
                        if (fMatching) {
                            // Encrypt and proceed
                            domLoginEncryptTitle.textContent = `Encrypting your keys...`;
                            domLoginEncryptPinRow.style.display = `none`;
                            await saveAndEncryptPrivateKey(pkey, strPinLast.join(''));
                            login();
                        } else {
                            // Wrong pin! Let's start again
                            domLoginEncryptTitle.textContent = `Pin doesn't match, re-try`;
                            strPinCurrent = Array(5).fill(`-`);
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
 * Open a chat with a particular contact
 * @param {string} contact 
 */
function openChat(contact) {
    // Display the Chat UI
    domChatNew.style.display = 'none';
    domChats.style.display = 'none';
    domChat.style.display = '';

    // Render the current contact's messages
    strOpenChat = contact;
    updateChat(contact);
}

/**
 * Open the dialog for starting a new chat
 */
function openNewChat() {
    // Display the UI
    domChatNew.style.display = '';
    domChats.style.display = 'none';
    domChat.style.display = 'none';
}

/**
 * A simple state tracker for the last message ID, if it changes, we auto-scroll
 */
let strLastMsgID = "";

/**
 * Updates the current chat (to display incoming and outgoing messages)
 * @param {string} contact 
 */
function updateChat(contact) {
    const cContact = arrChats.find(a => a.contact === contact);
    const cProfile = arrProfiles.find(a => a.id === contact);
    if (cContact) {
        // Prefer displaying their name, otherwise, npub
        domChatContact.textContent = cProfile?.name || contact.substring(0, 10) + '…';

        // Display their status, if one exists
        const fHasStatus = !!cProfile?.status?.title;
        domChatContactStatus.textContent = cProfile?.status?.title || '';

        // Adjust our Contact Name class to manage space according to Status visibility
        if (fHasStatus) {
            domChatContact.classList.remove('chat-contact');
            domChatContact.classList.add('chat-contact-with-status');
        } else {
            domChatContact.classList.add('chat-contact');
            domChatContact.classList.remove('chat-contact-with-status');
        }

        // Render their messages upon state changes (guided by fetchMessages())
        // TODO: this needs rewriting in the future to be event-based, i.e: new message added (append), message edited (modify one message in the DOM), etc.
        domChatMessages.innerHTML = ``;
        let nLastMsgTime = cContact.contents[0]?.at || 0;
        for (const msg of cContact.contents) {
            // If the last message was over 10 minutes ago, add an inline timestamp
            if (msg.at - nLastMsgTime > 600) {
                nLastMsgTime = msg.at;
                const pTimestamp = document.createElement('p');
                pTimestamp.classList.add('msg-inline-timestamp');
                pTimestamp.textContent = (new Date(msg.at * 1000)).toLocaleString();
                domChatMessages.appendChild(pTimestamp);
            }
            // Construct the message container (the DOM ID is the HEX Nostr Event ID)
            const divMessage = document.createElement('div');
            divMessage.id = msg.id;
            // Render it appropriately depending on who sent it
            divMessage.classList.add('msg-' + (msg.mine ? 'me' : 'them'));
            // Render their avatar, if they have one
            if (!msg.mine && cProfile?.avatar) {
                const imgAvatar = document.createElement('img');
                imgAvatar.src = cProfile.avatar;
                divMessage.appendChild(imgAvatar);
            }
            // Construct the text content
            const pMessage = document.createElement('p');
            // If it's emoji-only, and less than four emojis, format them nicely
            const strEmojiCleaned = msg.content.replace(/\s/g, '');
            if (isEmojiOnly(strEmojiCleaned) && strEmojiCleaned.length <= 6) {
                // Strip out unnecessary whitespace
                pMessage.textContent = strEmojiCleaned
                // Add an emoji-only CSS format
                pMessage.classList.add('emoji-only');
            } else {
                // Render their text content (using our custom Markdown renderer)
                // NOTE: the input IS HTML-sanitised, however, heavy auditing of the sanitisation method should be done, it is a bit sketchy
                pMessage.innerHTML = parseMarkdown(msg.content.trim());
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
                spanReaction.classList.add('add-reaction');
                spanReaction.setAttribute('reaction', true);
            }

            // Decide which side of the msg to render reactions on - if they exist
            if (spanReaction) {
                if (msg.mine) {
                    // My message: reactions on the left
                    spanReaction.style.left = `5px`;
                    divMessage.appendChild(spanReaction);
                    divMessage.appendChild(pMessage);
                } else {
                    // Their message: reactions on the right
                    spanReaction.style.left = `-2px`;
                    spanReaction.style.bottom = `-2px`;
                    divMessage.appendChild(pMessage);
                    divMessage.appendChild(spanReaction);
                }
            } else {
                // No reactions: just render the message
                divMessage.appendChild(pMessage);
            }

            // Add it to the chat!
            domChatMessages.appendChild(divMessage);
        }

        // Auto-scroll on new messages
        const cLastMsg = cContact.contents[cContact.contents.length - 1];
        if (cLastMsg.id !== strLastMsgID) {
            strLastMsgID = cLastMsg.id;
            domChatMessages.scrollTo(0, domChatMessages.scrollHeight);
        }
    } else {
        // Probably a 'New Chat', as such, we'll mostly render an empty chat
        domChatContact.textContent = cProfile?.name || contact.substring(0, 10) + '…';

        // Nuke the message list
        domChatMessages.innerHTML = ``;
    }
}

/**
 * Closes the current chat, taking the user back to the chat list
 */
function closeChat() {
    domChats.style.display = '';
    domChatNew.style.display = 'none';
    domChat.style.display = 'none';
    strOpenChat = "";
}

/**
 * Our Bech32 Nostr Public Key
 */
let strPubkey;

window.addEventListener("DOMContentLoaded", async () => {
    // Hook up our static buttons
    domLoginBtn.onclick = async () => {
        // Import and derive our keys
        try {
            const { public, private } = await invoke("login", { importKey: domLoginInput.value.trim() });
            strPubkey = public;
            // Open the Encryption Flow
            openEncryptionFlow(private);
        } catch (e) { console.error(e) }
    }
    domChatBackBtn.onclick = closeChat;
    domChatNewBackBtn.onclick = closeChat;
    domChatNewStartBtn.onclick = () => {
        openChat(domChatNewInput.value.trim());
        domChatNewInput.value = ``;
    };

    // Load the DB
    store = await load('vector.json', { autoSave: true });

    // If a local encrypted key exists, boot up the decryption UI
    if (await hasKey()) {
        openEncryptionFlow(null, true);
    }

    // Hook up an 'Enter' listener on the Message Box for sending messages
    const strOriginalInputPlaceholder = domChatMessageInput.getAttribute('placeholder');
    domChatMessageInput.onkeydown = async (evt) => {
        // Allow 'Shift + Enter' to create linebreaks, while only 'Enter' sends a message
        if (evt.code === 'Enter' && !evt.shiftKey) {
            evt.preventDefault();
            if (domChatMessageInput.value.trim().length) {
                // Cache the message and previous Input Placeholder
                const strMessage = domChatMessageInput.value;

                // Send the message, and display "Sending..." as the placeholder
                domChatMessageInput.value = '';
                domChatMessageInput.setAttribute('placeholder', 'Sending...');
                try {
                    await message(strOpenChat, strMessage);
                } catch(e) {
                    // TODO: temporary storage of failed messages for later re-try attempts
                }

                // Reset the placeholder
                domChatMessageInput.setAttribute('placeholder', strOriginalInputPlaceholder);
            }
        }
    };
});
