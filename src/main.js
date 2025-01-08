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
const domChatList = document.getElementById('chat-list');

const domChat = document.getElementById('chat');
const domChatBackBtn = document.getElementById('chat-back-btn');
const domChatContact = document.getElementById('chat-contact');
const domChatMessages = document.getElementById('chat-messages');
const domChatMessageBox = document.getElementById('chat-box');
const domChatMessageInput = document.getElementById('chat-input');

const domChatNew = document.getElementById('chat-new');
const domChatNewBackBtn = document.getElementById('chat-new-back-btn');
const domChatNewInput = document.getElementById('chat-new-input');
const domChatNewStartBtn = document.getElementById('chat-new-btn');

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

    // Render the chats
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

        // The avatar, if one exists
        if (cProfile?.avatar) {
            const imgAvatar = document.createElement('img');
            imgAvatar.src = cProfile.avatar;
            divContact.appendChild(imgAvatar);
        }

        // The name (or, if missing metadata, their npub instead)
        const h3ContactName = document.createElement('h3');
        h3ContactName.textContent = cProfile?.name || chat.contact;

        // Slap it all together
        divContact.appendChild(h3ContactName);
        domChatList.appendChild(divContact);
    }

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
    const fLoggedIn = await invoke("login", { importKey: domLoginInput.value.trim() });
    if (fLoggedIn) {
        // Hide the login UI
        domLoginInput.value = "";
        domLogin.style.display = 'none';

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

        // Render their messages
        const arrMessages = cContact.contents;
        domChatMessages.innerHTML = ``;
        for (const msg of arrMessages) {
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
            // Render their text content
            pMessage.textContent = msg.content;
            // Add it to the chat!
            divMessage.appendChild(pMessage);
            domChatMessages.appendChild(divMessage);
        }

        // Auto-scroll on new messages (not a great implementation)
        if (arrMessages.length) {
            const cLastMsg = arrMessages[arrMessages.length - 1];
            if (strLastMsgID !== cLastMsg.id) {
                domChatMessages.scrollTo(0, domChatMessages.scrollHeight);
                strLastMsgID = cLastMsg.id;
            }
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

    // Hook up an 'Enter' listener on the Message Box for sending them
    domChatMessageInput.onkeydown = async (evt) => {
        if (evt.code === 'Enter' && domChatMessageInput.value.trim().length) {
            await message(strOpenChat, domChatMessageInput.value);
            domChatMessageInput.value = '';
        }
    }
});
