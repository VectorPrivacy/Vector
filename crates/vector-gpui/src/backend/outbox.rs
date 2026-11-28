//! Words and any number of files as one message. A DM carries them as a kind 14 with an
//! `imeta` per file (as Armada does) and each URL on its own line after the words, so a
//! client that reads no `imeta` still shows links; a Community uses its own message.

use std::path::PathBuf;
use std::sync::Arc;

use nostr_sdk::prelude::*;
use vector_core::event_ext::FinalizeUnsignedWithId as _;
use vector_core::sending::SendCallback;
use vector_core::Message;

use super::community_files::{self, Prepared};
use super::messaging::Callback;
use crate::message::types::CachedCompressedImage;

/// One file staged in the composer, as it will travel.
pub struct Outgoing {
    pub path: PathBuf,
    /// The name it travels under; empty keeps the file's own.
    pub name: String,
    pub compress: bool,
    pub keep_metadata: bool,
    pub precompressed: Option<CachedCompressedImage>,
}

/// Send `content` with `files` to a DM or a channel.
pub async fn send(receiver: String, content: String, replied_to: String, files: Vec<Outgoing>) -> Result<(), String> {
    let dm = receiver.starts_with("npub1");
    // A lone file with no words keeps the kind 15 every NIP-17 client reads.
    if dm && files.len() == 1 && content.trim().is_empty() {
        let f = files.into_iter().next().expect("one file");
        return super::messaging::send_file(receiver, replied_to, f.path, f.name, f.compress, f.keep_metadata, f.precompressed).await;
    }
    let prepared = futures::future::try_join_all(
        files
            .into_iter()
            .map(|f| async move { community_files::prepare(&f.path, &f.name, f.compress, f.keep_metadata, f.precompressed).await }),
    )
    .await?;
    if dm {
        dispatch_dm(receiver, content, replied_to, prepared).await
    } else {
        community_files::dispatch(receiver, content, replied_to, prepared).await.map(|_| ())
    }
}

/// A failed DM whose wrap never left: its files again from what this device holds of them.
pub async fn retry_dm(receiver: String, msg: Message) -> Result<(), String> {
    let mut prepared = Vec::with_capacity(msg.attachments.len());
    for att in &msg.attachments {
        let source = vector_core::sending::retry_source(att).ok_or(community_files::RETRY_FILE_MISSING)?;
        let plain_len = att.size.saturating_sub(16);
        prepared.push(community_files::stage(source, plain_len, att.id.clone(), att.extension.clone(), att.name.clone(), att.img_meta.clone()));
    }
    super::actions::remove_failed(&msg.id, false).await?;
    if prepared.len() == 1 && msg.content.trim().is_empty() {
        let Prepared { attachment, plain, .. } = prepared.remove(0);
        let reply = (!msg.replied_to.is_empty()).then_some(msg.replied_to.as_str());
        let config = super::messaging::file_send_config(&receiver);
        return vector_core::sending::send_file_dm_from(
            &receiver,
            plain,
            &attachment.name,
            &attachment.extension,
            attachment.img_meta,
            None,
            reply,
            &config,
            Arc::new(Callback),
        )
        .await
        .map(|_| ());
    }
    dispatch_dm(receiver, msg.content, msg.replied_to, prepared).await
}

/// The bubble first, then each upload under it, then one gift-wrapped kind 14.
async fn dispatch_dm(receiver: String, content: String, replied_to: String, prepared: Vec<Prepared>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        let me = vector_core::my_public_key().ok_or("Public key not set")?; // i18n-exempt: developer error
        let to = PublicKey::from_bech32(&receiver).map_err(|e| format!("Invalid npub: {e}"))?; // i18n-exempt: developer error
        let reply = (!replied_to.is_empty()).then_some(replied_to);
        let (content, text_spans) = vector_core::text_spans::extract_own(content.trim());
        let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);
        let config = super::messaging::file_send_config(&receiver);
        let ms = vector_core::clock::now_ms();
        // device-clock: a local id, never compared with a time.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let pending_id = format!("pending-{nanos}");
        let mut pending = Message {
            id: pending_id.clone(),
            content: content.clone(),
            replied_to: reply.clone().unwrap_or_default(),
            at: ms,
            pending: true,
            mine: true,
            npub: me.to_bech32().ok(),
            emoji_tags: emoji_tags.clone(),
            text_spans: text_spans.clone(),
            attachments: prepared.iter().map(|p| p.attachment.clone()).collect(),
            // A lifespan is stamped after the uploads; the bubble shows no clock until then.
            expiration: if config.self_destruct_secs.is_some() { None } else { config.expiration },
            ..Default::default()
        };
        let _ = vector_core::db::events::populate_reply_context(&mut pending).await;
        vector_core::sending::add_pending(&receiver, &pending).await;
        Callback.on_pending(&receiver, &pending);
        let cancel = Callback.cancel_token(&pending_id);

        let uploaded = community_files::upload_all(&receiver, &pending_id, prepared, cancel.clone()).await?;
        if cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err("Upload cancelled".to_string()); // i18n-exempt: matched by the UI
        }

        let urls = uploaded.iter().map(|a| a.url.as_str());
        let wire = std::iter::once(content.as_str()).filter(|c| !c.is_empty()).chain(urls).collect::<Vec<_>>().join("\n");
        let now = std::time::Duration::from_millis(ms);
        let mut rumor = EventBuilder::new(Kind::PrivateDirectMessage, wire)
            .custom_created_at(Timestamp::from_secs(now.as_secs()))
            .tag(Tag::public_key(to));
        if let Some(id) = &reply {
            rumor = rumor.tag(Tag::custom("e", [id.clone(), String::new(), "reply".to_string()]));
        }
        rumor = rumor.tag(Tag::custom("ms", [(now.as_millis() % 1000).to_string()]));
        for et in &emoji_tags {
            rumor = rumor.tag(Tag::custom("emoji", [et.shortcode.clone(), et.url.clone()]));
        }
        rumor = rumor.tags(vector_core::text_spans::to_nostr_tags(&text_spans));
        rumor = rumor.tags(uploaded.iter().map(vector_core::community::attachments::attachment_to_imeta));
        let expiration = config.self_destruct_secs.and_then(vector_core::self_destruct::expiry_after).or(config.expiration);
        if let Some(exp) = expiration {
            rumor = rumor.tag(Tag::expiration(Timestamp::from_secs(exp)));
            let mut state = vector_core::state::STATE.lock().await;
            state.update_message(&pending_id, |m| m.expiration_secs = exp as u32);
        }
        let rumor = rumor.finalize_unsigned_with_id(me);
        vector_core::sending::send_rumor_dm(&receiver, &pending_id, rumor, &config, Arc::new(Callback))
            .await
            .map(|_| ())
    })
    .await
}
