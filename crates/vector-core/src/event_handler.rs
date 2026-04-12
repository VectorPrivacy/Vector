//! Event handler — gift wrap receive, unwrap, process, commit pipeline.
//!
//! Two-phase architecture:
//! - **Phase 1** (`prepare_event`): Parallel-safe — dedup, unwrap, process_rumor
//! - **Phase 2** (`commit_prepared_event`): Sequential — save DB, update STATE, emit
//!
//! Platform-specific behavior (notifications, MLS) handled by `InboundEventHandler` trait.

use nostr_sdk::prelude::*;

use crate::rumor::{RumorProcessingResult, RumorEvent, RumorContext, ConversationType, process_rumor};
use crate::types::Message;
use crate::state::WRAPPER_ID_CACHE;

/// Platform-specific callbacks for inbound event processing.
///
/// Same pattern as SendCallback/ProfileSyncHandler — trait with default no-ops.
/// Platforms implement only the hooks they need.
pub trait InboundEventHandler: Send + Sync {
    /// A DM text message was received and committed to STATE + DB.
    fn on_dm_received(&self, _chat_id: &str, _msg: &Message, _is_new: bool) {}

    /// A DM file attachment was received and committed to STATE + DB.
    fn on_file_received(&self, _chat_id: &str, _msg: &Message, _is_new: bool) {}

    /// A reaction was received and applied to a message.
    fn on_reaction_received(&self, _chat_id: &str, _msg: &Message) {}

    /// An MLS Welcome event — platform handles group join flow.
    fn on_mls_welcome(&self, _event: &Event, _rumor: &UnsignedEvent, _sender: &PublicKey) {}

    /// An MLS group message — platform handles decryption via MDK.
    fn on_mls_group_message(&self, _event: &Event) {}
}

/// No-op handler for CLI/tests.
pub struct NoOpEventHandler;
impl InboundEventHandler for NoOpEventHandler {}

/// Result of Phase 1 (prepare_event) — everything needed for sequential commit.
pub enum PreparedEvent {
    /// Fully processed DM rumor — ready for state commit.
    Processed {
        result: RumorProcessingResult,
        contact: String,
        sender: PublicKey,
        is_mine: bool,
        wrapper_event_id: String,
        wrapper_event_id_bytes: [u8; 32],
        wrapper_created_at: u64,
    },
    /// MLS Welcome — platform handles group join.
    MlsWelcome {
        event: Event,
        rumor: UnsignedEvent,
        contact: String,
        sender: PublicKey,
        is_mine: bool,
        wrapper_event_id: String,
        wrapper_event_id_bytes: [u8; 32],
        wrapper_created_at: u64,
    },
    /// Duplicate event — just persist wrapper for negentropy.
    DedupSkip {
        wrapper_id_bytes: [u8; 32],
        wrapper_created_at: u64,
    },
    /// Error during unwrap/processing — persist wrapper for negentropy.
    ErrorSkip {
        wrapper_id_bytes: [u8; 32],
        wrapper_created_at: u64,
    },
}

/// Phase 1: Prepare an event for commit (parallel-safe, no state mutation).
///
/// Performs dedup check, gift wrap decryption, and rumor parsing.
/// Safe to call from multiple tokio tasks concurrently.
pub async fn prepare_event(
    event: Event,
    client: &Client,
    my_public_key: PublicKey,
) -> PreparedEvent {
    let wrapper_created_at = event.created_at.as_secs();
    let wrapper_event_id_bytes: [u8; 32] = event.id.to_bytes();
    let wrapper_event_id = event.id.to_hex();

    // Dedup: in-memory cache first, then DB fallback
    {
        let cache = WRAPPER_ID_CACHE.lock().await;
        if cache.contains(&wrapper_event_id_bytes) {
            return PreparedEvent::DedupSkip { wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at };
        }
    }

    if let Ok(true) = crate::db::events::wrapper_event_exists(&wrapper_event_id) {
        return PreparedEvent::DedupSkip { wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at };
    }

    // Unwrap gift wrap (CPU-bound ECDH + ChaCha20Poly1305)
    let (rumor, sender) = match client.unwrap_gift_wrap(&event).await {
        Ok(UnwrappedGift { rumor, sender }) => (rumor, sender),
        Err(_) => return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        },
    };

    let is_mine = sender == my_public_key;
    let contact = if is_mine {
        rumor.tags.public_keys().next()
            .and_then(|pk| pk.to_bech32().ok())
            .unwrap_or_else(|| sender.to_bech32().unwrap_or_default())
    } else {
        sender.to_bech32().unwrap_or_default()
    };

    // Skip NIP-17 group messages (multiple p-tags) — Vector uses MLS
    if rumor.tags.public_keys().count() > 1 {
        return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        };
    }

    // MLS Welcome — defer to platform
    if rumor.kind == Kind::MlsWelcome {
        return PreparedEvent::MlsWelcome {
            event, rumor, contact, sender, is_mine,
            wrapper_event_id, wrapper_event_id_bytes, wrapper_created_at,
        };
    }

    // Build RumorEvent for processing
    let Some(rumor_id) = rumor.id else {
        return PreparedEvent::ErrorSkip {
            wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
        };
    };

    let rumor_event = RumorEvent {
        id: rumor_id,
        kind: rumor.kind,
        content: rumor.content,
        tags: rumor.tags,
        created_at: rumor.created_at,
        pubkey: rumor.pubkey,
    };
    let rumor_context = RumorContext {
        sender,
        is_mine,
        conversation_id: contact.clone(),
        conversation_type: ConversationType::DirectMessage,
    };

    let download_dir = crate::db::get_download_dir();
    match process_rumor(rumor_event, rumor_context, &download_dir) {
        Ok(result) => PreparedEvent::Processed {
            result, contact, sender, is_mine,
            wrapper_event_id, wrapper_event_id_bytes, wrapper_created_at,
        },
        Err(e) => {
            log_warn!("[EventHandler] Failed to process rumor: {}", e);
            PreparedEvent::ErrorSkip {
                wrapper_id_bytes: wrapper_event_id_bytes, wrapper_created_at,
            }
        }
    }
}
