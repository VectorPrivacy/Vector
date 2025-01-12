const { invoke } = window.__TAURI__.core;
const { getVersion } = window.__TAURI__.app;

const domVersion = document.getElementById('version');

// Display the current version
getVersion().then(v => {
    domVersion.textContent += `v${v}`;
});

const domLogin = document.getElementById('login-form');
const domLoginInput = document.getElementById('login-input');
const domLoginBtn = document.getElementById('login-btn');

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
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

document.addEventListener('DOMContentLoaded', () => {
    const picker = document.querySelector('.emoji-picker');
    /** @type {HTMLInputElement} */
    const emojiSearch = document.getElementById('emoji-search-input');
    const emojiResults = document.getElementById('emoji-results');

    // Listen for Emoji Picker interactions
    document.addEventListener('click', (e) => {
        if (e.target === domChatMessageInputEmoji && !picker.classList.contains('active')) {
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
            const rect = domChatMessageBox.getBoundingClientRect();
            picker.style.right = `0px`;

            // TODO: find out why the `-5px` is needed here... hidden margin? padding?
            picker.style.bottom = `${rect.height - 5}px`;
            picker.classList.add('active');

            // Focus on the emoji search box for easy searching
            emojiSearch.focus();
        } else {
            // Hide and reset the UI
            emojiSearch.value = '';
            picker.classList.remove('active');
        }
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

            // Add it to the message input
            domChatMessageInput.value += cEmoji.emoji;

            // Reset the UI state
            emojiSearch.value = '';
            picker.classList.remove('active');

            // Bring the focus back to the chat
            domChatMessageInput.focus();
        } else if (e.code === 'Escape') {
            // Close the dialog
            emojiSearch.value = '';
            picker.classList.remove('active');

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

            // Add it to the message input
            domChatMessageInput.value += cEmoji.emoji;

            // Reset the UI state
            emojiSearch.value = '';
            picker.classList.remove('active');

            // Bring the focus back to the chat
            domChatMessageInput.focus();
        }
    });
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

    // If a chat is open, update it's messages
    if (strOpenChat) {
        updateChat(strOpenChat);
    }

    // Render the chats (if the backend signals a state change)
    const fStateChanged = await invoke('has_state_changed');
    if (!fStateChanged) return;

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
    const strPubkey = await invoke("login", { importKey: domLoginInput.value.trim() });
    if (strPubkey) {
        // Hide the login UI
        domLoginInput.value = "";
        domLogin.style.display = 'none';

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

        // Render our username (or npub)
        const h3Username = document.createElement('h3');
        h3Username.textContent = cProfile?.name || strPubkey.substring(0, 10) + '…';
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

        // Render their messages if a new one has been added
        // TODO: this needs rewriting in the future to be event-based, i.e: new message added (append), message edited (modify one message in the DOM), etc.
        const arrMessages = cContact.contents;
        const cLastMsg = arrMessages[arrMessages.length - 1];
        const isNewMsg = strLastMsgID !== cLastMsg.id;
        if (isNewMsg) {
            // Update the last message ID
            strLastMsgID = cLastMsg.id

            // Wipe and re-render the HTML
            domChatMessages.innerHTML = ``;
            let nLastMsgTime = arrMessages[0]?.at || 0;
            for (const msg of arrMessages) {
                // If the last message was over 10 minutes ago, add an inline timestamp
                if (msg.at - nLastMsgTime > 600) {
                    nLastMsgTime = msg.at;
                    const pTimestamp = document.createElement('p');
                    pTimestamp.classList.add('msg-inline-timestamp');
                    pTimestamp.textContent = (new Date(msg.at * 1000)).toLocaleString();
                    domChatMessages.appendChild(pTimestamp);
                }
                // Construct the message container
                const divMessage = document.createElement('div');
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
                // Add it to the chat!
                divMessage.appendChild(pMessage);
                domChatMessages.appendChild(divMessage);
            }

            // Auto-scroll on new messages
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

window.addEventListener("DOMContentLoaded", () => {
    // Hook up our static buttons
    domLoginBtn.onclick = login;
    domChatBackBtn.onclick = closeChat;
    domChatNewBackBtn.onclick = closeChat;
    domChatNewStartBtn.onclick = () => {
        openChat(domChatNewInput.value.trim());
        domChatNewInput.value = ``;
    };

    // Hook up an 'Enter' listener on the Message Box for sending messages
    domChatMessageInput.onkeydown = async (evt) => {
        // Allow 'Shift + Enter' to create linebreaks, while only 'Enter' sends a message
        if (evt.code === 'Enter' && !evt.shiftKey) {
            evt.preventDefault();
            if (domChatMessageInput.value.trim().length) {
                // Cache the message and previous Input Placeholder
                const strMessage = domChatMessageInput.value;
                const strPlaceholder = domChatMessageInput.getAttribute('placeholder');

                // Send the message, and display "Sending..." as the placeholder
                domChatMessageInput.value = '';
                domChatMessageInput.setAttribute('placeholder', 'Sending...');
                await message(strOpenChat, strMessage);

                // Sent! Reset the placeholder
                domChatMessageInput.setAttribute('placeholder', strPlaceholder);
            }
        }
    };
});
