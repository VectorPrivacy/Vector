use std::sync::Mutex;

use nostr_sdk::prelude::*;
use once_cell::sync::OnceCell;
use lazy_static::lazy_static;

static NOSTR_CLIENT: OnceCell<Client> = OnceCell::new();

#[derive(serde::Serialize, Clone, Debug)]
struct Message {
    id: String,
    content: String,
    contact: String,
    at: u64,
    mine: bool,
}

#[derive(serde::Serialize, Clone, Debug)]
struct Profile {
    id: String,
    name: String,
    avatar: String,
}

struct ChatState {
    messages: Vec<Message>,
    profiles: Vec<Profile>,
}

impl ChatState {
    fn new() -> Self {
        Self { messages: Vec::new(), profiles: Vec::new() }
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

lazy_static! {
    static ref STATE: Mutex<ChatState> = Mutex::new(ChatState::new());
}

#[tauri::command]
async fn fetch_messages() -> Result<Vec<Message>, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // If we're not connected to any relays - connect and immediately retrieve our historical messages
    if client.relays().await.len() == 0 {
        connect().await;

        // Grab our pubkey
        let signer = client.signer().await.unwrap();
        let pubkey = signer.get_public_key().await.unwrap();

        // Fetch GiftWraps related to us
        let filter = Filter::new().pubkey(pubkey).kind(Kind::GiftWrap);
        let events = client
            .fetch_events(vec![filter], std::time::Duration::from_secs(10))
            .await.unwrap();

        // Decrypt every GiftWrap and return their contents + senders
        for maybe_dm in events.iter() {
            if maybe_dm.kind == Kind::GiftWrap {
                match client.unwrap_gift_wrap(&maybe_dm).await {
                    Ok(UnwrappedGift { rumor, sender }) => {
                        // Found a NIP-17 message!
                        if rumor.kind == Kind::PrivateDirectMessage {
                            let is_mine = sender == pubkey;
                            // TODO: simplify and un-nest-ify this ugly section
                            match rumor.tags.first() {
                                Some(tag) => {
                                    match tag.content() {
                                        Some(content) => {
                                            match PublicKey::from_hex(content) {
                                                Ok(pub_key) => {
                                                    match pub_key.to_bech32() {
                                                        Ok(bech32) => {
                                                            let msg = Message{ id: rumor.id.unwrap().to_bech32().unwrap(), content: rumor.content.to_string(), contact: if sender == pubkey { bech32 } else { sender.to_bech32().unwrap() }, at: rumor.created_at.as_u64(), mine: is_mine };
                                                            let mut state = STATE.lock().unwrap();
                                                            state.add_message(msg);
                                                        },
                                                        Err(_) => eprintln!("Failed to convert public key to bech32"),
                                                    }
                                                },
                                                Err(_) => eprintln!("Failed to create public key from hex"),
                                            }
                                        },
                                        None => eprintln!("No tag content"),
                                    }
                                },
                                None => eprintln!("No tags found"),
                            }                            
                        }
                    }
                    Err(_e) => (),
                }
            }
        }
    }

    let state = STATE.lock().unwrap();
    let msgs = state.messages.clone();

    Ok(msgs)
}

#[tauri::command]
async fn message(receiver: String, content: String) -> Result<bool, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Convert the Bech32 String in to a PublicKey
    let receiver_pubkey = PublicKey::from_bech32(receiver.as_str()).unwrap();

    // Attempt to send the message
    match client.send_private_msg(receiver_pubkey, content.clone(), []).await {
        Ok(response) => {
            let msg = Message{ id: response.id().to_bech32().unwrap(), content: content, contact: receiver, at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(), mine: true };
            let mut state = STATE.lock().unwrap();
            state.add_message(msg);
            return Ok(true);
        },
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Ok(false);
        },
    }
}

#[tauri::command]
async fn load_profile(npub: String) -> Result<Profile, ()> {
    let client = NOSTR_CLIENT.get().expect("Nostr client not initialized");

    // Convert the Bech32 String in to a PublicKey
    let profile_pubkey = PublicKey::from_bech32(npub.as_str()).unwrap();

    // Attempt to fetch their Metadata profile
    match client.fetch_metadata(profile_pubkey, std::time::Duration::from_secs(10)).await {
        Ok(response) => {
            let profile = Profile{ id: npub, name: response.name.unwrap_or_default(), avatar: response.picture.unwrap_or_default() };
            let mut state = STATE.lock().unwrap();
            state.add_profile(profile.clone());
            return Ok(profile);
        },
        Err(e) => {
            eprintln!("Error: {:?}", e);
            return Err(());
        },
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
                            if rumor.kind == Kind::PrivateDirectMessage {
                                let msg = Message{ id: rumor.id.unwrap().to_bech32().unwrap(), content: rumor.content.to_string(), contact: sender.to_bech32().unwrap().to_string(), at: rumor.created_at.as_u64(), mine: pubkey == rumor.pubkey };
                                let mut state = STATE.lock().unwrap();
                                state.add_message(msg);
                            }
                        }
                        Err(_e) => (),
                    }
                }
            }
            Ok(false)
        })
        .await.unwrap();
}

#[tauri::command]
async fn login(import_key: String) -> Result<bool, ()> {
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
    Ok(true)
}

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
        .invoke_handler(tauri::generate_handler![fetch_messages, message, login, notifs, load_profile])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}