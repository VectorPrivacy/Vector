use tokio::sync::Mutex;

use lazy_static::lazy_static;
use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;

use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();
static TAURI_APP: OnceCell<AppHandle> = OnceCell::new();

#[derive(serde::Serialize, Clone, Debug)]
struct Message {
    id: String,
    content: String,
    contact: String,
    reactions: Vec<Reaction>,
    at: u64,
    mine: bool,
}

#[derive(serde::Serialize, Clone, Debug)]
struct Reaction {
    id: String,
    /** The HEX Event ID of the message being reacted to */
    reference_id: String,
    /** The HEX ID of the author */
    author_id: String,
    /** The emoji of the reaction */
    emoji: String,
}

#[derive(serde::Serialize, Clone, Debug)]
struct Profile {
    id: String,
    name: String,
    avatar: String,
    status: Status,
    mine: bool,
}

#[derive(serde::Serialize, Clone, Debug)]
struct Status {
    title: String,
    purpose: String,
    url: String,
}

struct ChatState {
    messages: Vec<Message>,
    profiles: Vec<Profile>,
    // Used, particularly, for detecting Message + Profile changes and rendering them
    has_state_changed: bool,
}

impl ChatState {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            profiles: Vec::new(),
            has_state_changed: true,
        }
    }

    fn add_message(&mut self, message: Message) {
        // Make sure we don't add the same message twice
        if !self.messages.iter().any(|m| m.id == message.id) {
            self.messages.push(message);
        }
    }

    fn add_profile(&mut self, profile: Profile) {
        // Make sure we don't add the same profile twice
        if !self.profiles.iter().any(|m| m.id == profile.id) {
            self.profiles.push(profile);
        }
    }
}

impl Message {
    fn add_reaction(&mut self, reaction: Reaction) {
        // Make sure we don't add the same reaction twice
        if !self.reactions.iter().any(|r| r.id == reaction.id) {
            self.reactions.push(reaction);
        }
    }
}

lazy_static! {
    static ref STATE: Mutex<ChatState> = Mutex::new(ChatState::new());
}

#[tauri::command]
async fn fetch_messages() -> Result<Vec<Message>, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // If we don't have any messages - keep trying to find 'em
    let mut state = STATE.lock().await;
    if state.messages.is_empty() {
        // Grab our pubkey
        let signer = client.signer().await.unwrap();
        let my_public_key = signer.get_public_key().await.unwrap();

        // Fetch GiftWraps related to us
        let filter = Filter::new().pubkey(my_public_key).kind(Kind::GiftWrap);
        let events = client
            .fetch_events(vec![filter], std::time::Duration::from_secs(10))
            .await
            .unwrap();

        // Decrypt every GiftWrap and return their contents + senders
        for maybe_dm in events.into_iter().filter(|e| e.kind == Kind::GiftWrap) {
            // Unwrap the gift wrap
            match client.unwrap_gift_wrap(&maybe_dm).await {
                Ok(UnwrappedGift { rumor, sender }) => {
                    // Found a NIP-17 message!
                    if rumor.kind == Kind::PrivateDirectMessage {
                        // Check if it's mine
                        let is_mine = sender == my_public_key;

                        // Get contact public key (bech32)
                        let contact: String = if is_mine {
                            // Get first public key from tags
                            match rumor.tags.public_keys().next() {
                                Some(pub_key) => match pub_key.to_bech32() {
                                    Ok(p_tag_pubkey_bech32) => p_tag_pubkey_bech32,
                                    Err(..) => {
                                        eprintln!("Failed to convert public key to bech32");
                                        continue;
                                    }
                                },
                                None => {
                                    eprintln!("Public key tag found");
                                    continue;
                                }
                            }
                        } else {
                            sender.to_bech32().unwrap()
                        };

                        let msg = Message {
                            id: rumor.id.unwrap().to_hex(),
                            content: rumor.content,
                            contact,
                            at: rumor.created_at.as_u64(),
                            reactions: Vec::new(),
                            mine: is_mine,
                        };
                        state.add_message(msg);
                    }
                    // GiftWrapped Emoji Reaction (compatible with 0xchat implementation)
                    else if rumor.kind == Kind::Reaction {
                        match rumor.tags.find(TagKind::e()) {
                            Some(react_reference_tag) => {
                                // The message ID being 'reacted' to
                                let reference_id = react_reference_tag.content().unwrap();
                                // Now we search our message cache for the referred message
                                let mut found_message: bool = state.has_state_changed;
                                for msg in state.messages.iter_mut() {
                                    // Found it!
                                    if msg.id == reference_id.to_string() {
                                        // Create the Reaction
                                        let reaction = Reaction {
                                            id: rumor.id.unwrap().to_hex(),
                                            reference_id: reference_id.to_string(),
                                            author_id: sender.to_hex(),
                                            emoji: rumor.content.clone(),
                                        };
                                        // Append it to the message
                                        msg.add_reaction(reaction);
                                        found_message = true;
                                    }
                                }

                                // If we found the relevent message: mark the state as changed!
                                state.has_state_changed = found_message;
                            }
                            None => println!("No referenced message for reaction"),
                        }
                    }
                }
                Err(_e) => (),
            }
        }
    }

    let msgs = state.messages.clone();

    Ok(msgs)
}

#[tauri::command]
async fn message(receiver: String, content: String) -> Result<bool, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Convert the Bech32 String in to a PublicKey
    let receiver_pubkey = PublicKey::from_bech32(receiver.as_str()).unwrap();

    // Build the NIP-17 rumor
    let rumor = EventBuilder::private_msg_rumor(receiver_pubkey, content.clone());

    // Send message to the real receiver
    client
        .gift_wrap(&receiver_pubkey, rumor.clone(), [])
        .await
        .unwrap();

    // Send message to our own public key, to allow for message recovering
    match client.gift_wrap(&my_public_key, rumor, []).await {
        Ok(response) => {
            let msg = Message {
                id: response.id().to_hex(),
                content: content,
                contact: receiver,
                at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                reactions: Vec::new(),
                mine: true,
            };
            let mut state = STATE.lock().await;
            state.has_state_changed = true;
            state.add_message(msg);
            return Ok(true);
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Ok(false);
        }
    }
}

#[tauri::command]
async fn react(reference_id: String, chat_pubkey: String, emoji: String) -> Result<bool, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab the message we're reacting to
    let mut state = STATE.lock().await;
    if let Some(msg) = state.messages.iter_mut().find(|msg| msg.id == reference_id) {
        // Format the reference ID in to an EventID
        let reference_event = EventId::from_hex(reference_id.as_str()).unwrap();

        // Format the chat pubkey (which is, currently, the single user we're talking to)
        let receiver_pubkey = PublicKey::from_bech32(chat_pubkey.as_str()).unwrap();

        // Grab our pubkey
        let signer = client.signer().await.unwrap();
        let my_public_key = signer.get_public_key().await.unwrap();

        // Build our NIP-25 Reaction rumor
        let rumor = EventBuilder::reaction_extended(
            reference_event,
            receiver_pubkey,
            Some(Kind::PrivateDirectMessage),
            emoji.clone(),
        );

        // Send reaction to the real receiver
        client
            .gift_wrap(&receiver_pubkey, rumor.clone(), [])
            .await
            .unwrap();

        // Send reaction to our own public key, to allow for recovering
        match client.gift_wrap(&my_public_key, rumor, []).await {
            Ok(response) => {
                // Add the reaction locally
                let reaction = Reaction {
                    id: response.id().to_hex(),
                    reference_id,
                    author_id: my_public_key.to_hex(),
                    emoji,
                };
                // Append it to the message
                msg.add_reaction(reaction);
                state.has_state_changed = true;
                return Ok(true);
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
                return Ok(false);
            }
        }
    } else {
        //  No reference message, what do!?
        return Ok(false);
    }
}

#[tauri::command]
async fn load_profile(npub: String) -> Result<Profile, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Convert the Bech32 String in to a PublicKey
    let profile_pubkey = PublicKey::from_bech32(npub.as_str()).unwrap();

    // Grab our pubkey to check for profiles belonging to us
    let signer = client.signer().await.unwrap();
    let my_public_key = signer.get_public_key().await.unwrap();

    // Attempt to fetch their status, if one exists
    let status_filter = Filter::new()
        .author(profile_pubkey)
        .kind(Kind::from_u16(30315))
        .limit(1);
    let status = match client
        .fetch_events(vec![status_filter], std::time::Duration::from_secs(10))
        .await
    {
        Ok(res) => {
            // Make sure they have a status available
            if !res.is_empty() {
                let status_event = res.first().unwrap();
                // Simple status recognition: last, general-only, no URLs, Metadata or Expiry considered
                // TODO: comply with expiries, accept more "d" types, allow URLs
                Status {
                    title: status_event.content.clone(),
                    purpose: status_event
                        .tags
                        .first()
                        .unwrap()
                        .content()
                        .unwrap()
                        .to_string(),
                    url: String::from(""),
                }
            } else {
                // No status
                Status {
                    title: String::from(""),
                    purpose: String::from(""),
                    url: String::from(""),
                }
            }
        }
        Err(_e) => Status {
            title: String::from(""),
            purpose: String::from(""),
            url: String::from(""),
        },
    };

    // Attempt to fetch their Metadata profile
    match client
        .fetch_metadata(profile_pubkey, std::time::Duration::from_secs(10))
        .await
    {
        Ok(response) => {
            let mine = my_public_key == profile_pubkey;
            let profile = Profile {
                mine,
                id: npub,
                name: response.name.unwrap_or_default(),
                avatar: response.picture.unwrap_or_default(),
                status,
            };
            let mut state = STATE.lock().await;
            state.has_state_changed = true;
            state.add_profile(profile.clone());
            return Ok(profile);
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Err(());
        }
    }
}

#[tauri::command]
async fn notifs() {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Grab our pubkey
    let signer = client.signer().await.unwrap();
    let pubkey = signer.get_public_key().await.unwrap();

    // Listen for GiftWraps related to us
    let filter = Filter::new().pubkey(pubkey).kind(Kind::GiftWrap).limit(0);

    // Subscribe to the filter and begin handling incoming events
    client.subscribe(vec![filter], None).await.unwrap();
    client
        .handle_notifications(|notification| async {
            if let RelayPoolNotification::Event { event, .. } = notification {
                if event.kind == Kind::GiftWrap {
                    match client.unwrap_gift_wrap(&event).await {
                        Ok(UnwrappedGift { rumor, sender }) => {
                            // NIP-17 Private Direct Message
                            if rumor.kind == Kind::PrivateDirectMessage {
                                let msg = Message {
                                    id: rumor.id.unwrap().to_hex(),
                                    content: rumor.content.to_string(),
                                    contact: sender.to_bech32().unwrap().to_string(),
                                    at: rumor.created_at.as_u64(),
                                    reactions: Vec::new(),
                                    mine: pubkey == rumor.pubkey,
                                };
                                let mut state = STATE.lock().await;

                                // Send an OS notification for incoming messages
                                if !msg.mine {
                                    // Find the name of the sender, if we have it
                                    if let Some(profile) = state
                                        .profiles
                                        .iter()
                                        .find(|profile| profile.id == msg.contact)
                                    {
                                        show_notification(
                                            profile.name.clone(),
                                            msg.content.clone(),
                                        );
                                    } else {
                                        show_notification(
                                            String::from("New Message"),
                                            msg.content.clone(),
                                        );
                                    }
                                }

                                // Push the message to our state
                                state.has_state_changed = true;
                                state.add_message(msg);
                            }
                            // GiftWrapped Emoji Reaction (compatible with 0xchat implementation)
                            else if rumor.kind == Kind::Reaction {
                                match rumor.tags.find(TagKind::e()) {
                                    Some(react_reference_tag) => {
                                        // The message ID being 'reacted' to
                                        let reference_id = react_reference_tag.content().unwrap();
                                        // Now we search our message cache for the referred message
                                        let mut state = STATE.lock().await;
                                        let mut found_message: bool = state.has_state_changed;
                                        for msg in state.messages.iter_mut() {
                                            // Found it!
                                            if msg.id == reference_id.to_string() {
                                                // Create the Reaction
                                                let reaction = Reaction {
                                                    id: rumor.id.unwrap().to_hex(),
                                                    reference_id: reference_id.to_string(),
                                                    author_id: sender.to_hex(),
                                                    emoji: rumor.content.clone(),
                                                };
                                                // Append it to the message
                                                msg.add_reaction(reaction);
                                                found_message = true;
                                            }
                                        }

                                        // If we found the relevent message: mark the state as changed!
                                        state.has_state_changed = found_message;
                                    }
                                    None => println!("No referenced message for reaction"),
                                }
                            }
                        }
                        Err(_e) => (),
                    }
                }
            }
            Ok(false)
        })
        .await
        .unwrap();
}

#[tauri::command]
fn show_notification(title: String, content: String) {
    let app_handle = TAURI_APP.get().unwrap().clone();
    // Only send notifications if the app is not focused
    // TODO: generalise this assumption - it's only used for Message Notifications at the moment
    if !app_handle
        .webview_windows()
        .iter()
        .next()
        .unwrap()
        .1
        .is_focused()
        .unwrap()
    {
        app_handle
            .notification()
            .builder()
            .title(title)
            .body(content)
            .show()
            .unwrap_or_else(|e| eprintln!("Failed to send notification: {}", e));
    }
}

#[tauri::command]
async fn login(import_key: String) -> Result<String, ()> {
    let keys: Keys;
    // TODO: add validation, error handling, etc

    // If it's an nsec, import that
    if import_key.starts_with("nsec") {
        keys = Keys::parse(&import_key).unwrap();
    } else {
        // Otherwise, we'll try importing it as a mnemonic seed phrase (BIP-39)
        keys = Keys::from_mnemonic(import_key, Some(String::new())).unwrap();
    }

    // Initialise the Nostr client
    let client = Client::builder()
        .signer(keys.clone())
        .opts(Options::new().gossip(false))
        .build();
    NOSTR_CLIENT.set(client).unwrap();

    // Return our npub to the frontend client
    Ok(keys.public_key.to_bech32().unwrap())
}

#[tauri::command]
async fn has_state_changed() -> Result<bool, ()> {
    let state = STATE.lock().await;
    Ok(state.has_state_changed)
}

#[tauri::command]
async fn acknowledge_state_change() {
    let mut state = STATE.lock().await;
    state.has_state_changed = false;
}

#[tauri::command]
async fn connect() {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Add a couple common relays, especially with explicit NIP-17 support (thanks 0xchat and myself!)
    client.add_relay("wss://jskitty.cat/nostr").await.unwrap();
    client.add_relay("wss://relay.0xchat.com").await.unwrap();
    client.add_relay("wss://auth.nostr1.com").await.unwrap();
    client.add_relay("wss://relay.damus.io").await.unwrap();

    // Connect!
    client.connect().await;
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let app_handle = app.app_handle().clone();
            // Set as our accessible static app handle
            TAURI_APP.set(app_handle).unwrap();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            fetch_messages,
            message,
            react,
            login,
            notifs,
            load_profile,
            connect,
            has_state_changed,
            acknowledge_state_change
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
