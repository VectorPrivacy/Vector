//! Files and voice notes into a Community channel (`send_community_files`): every attachment
//! rides one message, each in its own imeta, sealed under a fresh key only members can open.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::ToBech32;
use vector_core::community::transport::LiveTransport;
use vector_core::community::CommunityId;
use vector_core::sending::{FileSource, Sealed, SendCallback};
use vector_core::Message;

use super::messaging::Callback;
use crate::message::types::CachedCompressedImage;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "ico"];

struct Prepared {
    attachment: vector_core::types::Attachment,
    plain: FileSource,
    mime: String,
}

fn id32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", hex.len()));
    }
    Ok(vector_core::simd::hex::hex_to_bytes_32(hex))
}

/// Send `path` (named `name_override` when not empty) to `channel` with an optional caption.
pub async fn send_file(
    channel: String,
    content: String,
    path: PathBuf,
    name_override: String,
    compress: bool,
    keep_metadata: bool,
    replied_to: String,
    precompressed: Option<CachedCompressedImage>,
) -> Result<Option<String>, String> {
    let prepared = prepare(&path, &name_override, compress, keep_metadata, precompressed).await?;
    dispatch(channel, content, replied_to, vec![prepared]).await
}

/// A voice note: a nameless WAV, which members draw as a waveform.
pub async fn send_voice(channel: String, wav: Vec<u8>, replied_to: String) -> Result<(), String> {
    let mut prepared = prepare_bytes(wav, "voice-message.wav", false, false, None).await?;
    prepared.attachment.name = String::new();
    dispatch(channel, String::new(), replied_to, vec![prepared]).await.map(|_| ())
}

async fn prepare(
    path: &Path,
    name_override: &str,
    compress: bool,
    keep_metadata: bool,
    precompressed: Option<CachedCompressedImage>,
) -> Result<Prepared, String> {
    let name = if name_override.is_empty() {
        path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()
    } else {
        vector_core::crypto::sanitize_filename(name_override)
    };
    let extension = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !IMAGE_EXTS.contains(&extension.as_str()) {
        return prepare_path(path.to_path_buf(), name).await;
    }
    let source = path.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || {
        vector_core::vcache::read(&source).map(|b| b.to_vec())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("read attachment: {e}"))?;
    prepare_bytes(bytes, &name, compress, keep_metadata, precompressed).await
}

/// A file that needs no processing: hashed where it lies and sealed file to file.
async fn prepare_path(path: PathBuf, name: String) -> Result<Prepared, String> {
    let extension = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();
    let source = path.clone();
    let (hash, len) = tokio::task::spawn_blocking(move || vector_core::crypto::stream::hash_file(&source))
        .await
        .map_err(|e| e.to_string())??;
    if len == 0 {
        return Err("Empty attachment".into());
    }
    Ok(stage(FileSource::Path(path), len, hash, extension, name, None))
}

async fn prepare_bytes(
    bytes: Vec<u8>,
    file_name: &str,
    compress: bool,
    keep_metadata: bool,
    precompressed: Option<CachedCompressedImage>,
) -> Result<Prepared, String> {
    if bytes.is_empty() {
        return Err("Empty attachment".into());
    }
    let sent_as = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();
    let mut extension = sent_as.clone();
    let is_image = IMAGE_EXTS.contains(&extension.as_str());
    let (bytes, img_meta, extension, hash) = tokio::task::spawn_blocking(move || {
        let (bytes, img_meta) = if is_image {
            let processed = crate::message::compression::process_image_for_send(
                Arc::new(bytes),
                &extension,
                compress,
                keep_metadata,
                precompressed,
            )?;
            crate::message::compression::forget_fitted();
            extension = processed.extension;
            (Arc::try_unwrap(processed.bytes).unwrap_or_else(|a| (*a).clone()), processed.img_meta)
        } else {
            (bytes, None)
        };
        let hash = vector_core::crypto::sha256_hex(&bytes);
        Ok::<_, String>((bytes, img_meta, extension, hash))
    })
    .await
    .map_err(|e| e.to_string())??;
    let len = bytes.len() as u64;
    // A compressed picture's name follows its new format, so the receiver opens it as one.
    let name = if extension == sent_as {
        file_name.to_string()
    } else {
        crate::message::video_compression::renamed(file_name, &extension)
    };
    Ok(stage(FileSource::Bytes(Arc::new(bytes)), len, hash, extension, name, img_meta))
}

/// The optimistic attachment; nothing is sealed or uploaded until the bubble is up.
fn stage(
    plain: FileSource,
    plain_len: u64,
    hash_hex: String,
    extension: String,
    name: String,
    img_meta: Option<vector_core::types::ImageMetadata>,
) -> Prepared {
    let params = match &plain {
        FileSource::Sealed { key, nonce, .. } => vector_core::crypto::EncryptionParams {
            key: key.clone(),
            nonce: nonce.clone(),
        },
        _ => vector_core::crypto::generate_encryption_params(),
    };
    let mime = vector_core::crypto::mime_from_extension(&extension).to_string();
    let hash = vector_core::simd::hex::hex_to_bytes_32(&hash_hex);
    match &plain {
        FileSource::Bytes(bytes) => {
            let _ = vector_core::media::ram::insert(hash, bytes.to_vec(), &mime);
        }
        FileSource::Path(path) | FileSource::Saved(path) => {
            vector_core::media::pending::plain(hash, path.clone(), &mime)
        }
        FileSource::Sealed { path, key, nonce, .. } => {
            vector_core::media::pending::sealed(hash, path.clone(), key, nonce, &mime)
        }
    }
    // Minted at send so every member joins one realtime topic; it rides the imeta.
    let webxdc_topic = extension.eq_ignore_ascii_case("xdc").then(|| {
        let sender = vector_core::state::my_public_key()
            .map(|pk| pk.to_hex())
            .unwrap_or_default();
        vector_core::webxdc::mint_topic_id(&hash_hex, &sender)
    });
    let attachment = vector_core::types::Attachment {
        id: hash_hex.clone(),
        key: params.key,
        nonce: params.nonce,
        extension,
        name,
        url: String::new(),
        path: String::new(),
        // AES-GCM's 16-byte tag: the sealed size is known before sealing.
        size: plain_len + 16,
        img_meta,
        downloading: false,
        downloaded: false,
        verified: true,
        resident: vector_core::media::resident(&hash),
        webxdc_topic,
        group_id: None,
        original_hash: Some(hash_hex),
        fallback_urls: Vec::new(),
    };
    Prepared { attachment, plain, mime }
}

async fn mark_failed(channel: &str, pending_id: &str) {
    let failed = {
        let mut state = vector_core::state::STATE.lock().await;
        state.update_message(pending_id, |m| {
            m.set_failed(true);
            m.set_pending(false);
        })
    };
    if let Some((_, msg)) = failed {
        Callback.on_failed(channel, pending_id, &msg);
    }
}

/// The bubble first, then each upload with progress and cancel, then one sealed message;
/// the echo replaces the bubble. Returns a sent Mini App's realtime topic.
async fn dispatch(
    channel: String,
    content: String,
    replied_to: String,
    prepared: Vec<Prepared>,
) -> Result<Option<String>, String> {
    vector_core::db::scoped(async move {
        let reply = (!replied_to.is_empty()).then_some(replied_to);
        let author = vector_core::my_public_key().ok_or("Public key not set")?;
        let my_npub = author.to_bech32().ok();
        let community_id = vector_core::db::community::community_id_for_channel(&channel)?
            .ok_or("Unknown Community channel")?;
        let (content, _) = vector_core::community::mentions::resolve_in(&community_id, &content);
        let (content, text_spans) = vector_core::text_spans::extract_own(&content);
        let community = vector_core::db::community::load_community_v2(&CommunityId(id32(&community_id)?))?
            .ok_or("Community not found")?;
        let ch = vector_core::community::ChannelId(id32(&channel)?);
        if community.channel(&ch).is_none() {
            return Err("Channel not found in Community".to_string());
        }

        let ms = vector_core::clock::now_ms();
        // device-clock: a local id, never compared with a time.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pending_id = format!("pending-{nanos}");
        let emoji_tags = vector_core::emoji_packs::resolve_outbound_emoji_tags(&content);
        let self_destruct = vector_core::self_destruct::chat_duration_secs(&channel);
        let mut pending = Message {
            id: pending_id.clone(),
            content: content.clone(),
            at: ms,
            pending: true,
            mine: true,
            npub: my_npub,
            replied_to: reply.clone().unwrap_or_default(),
            emoji_tags: emoji_tags.clone(),
            text_spans: text_spans.clone(),
            attachments: prepared.iter().map(|p| p.attachment.clone()).collect(),
            expiration: None,
            ..Default::default()
        };
        let _ = vector_core::db::events::populate_reply_context(&mut pending).await;
        vector_core::state::STATE.lock().await.add_message_to_chat(&channel, &pending);
        Callback.on_pending(&channel, &pending);
        let cancel = Callback.cancel_token(&pending_id);

        let signer = vector_core::signer::active_signer().map_err(|e| format!("Signer unavailable: {e}"))?;
        let servers = vector_core::state::get_blossom_servers();
        if servers.is_empty() {
            mark_failed(&channel, &pending_id).await;
            return Err("No Blossom servers configured.".to_string());
        }

        let mut uploaded = Vec::with_capacity(prepared.len());
        for Prepared { mut attachment, plain, mime } in prepared {
            let sealed = match vector_core::sending::seal_or_reuse(
                plain,
                &attachment.id,
                &attachment.key,
                &attachment.nonce,
                &pending_id,
                Arc::new(Callback),
                cancel.clone(),
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    mark_failed(&channel, &pending_id).await;
                    return Err(e);
                }
            };
            let (encrypted, sealed_file) = match sealed {
                // A live blob already holds it: no upload, only a background mirror if it's someone else's.
                Sealed::Reused(r) => {
                    let foreign = !r.mine;
                    (attachment.key, attachment.nonce, attachment.url, attachment.size) =
                        (r.key, r.nonce, r.url, r.size);
                    {
                        let adopted = vector_core::compact::CompactAttachment::from_attachment(&attachment);
                        let mut state = vector_core::state::STATE.lock().await;
                        state.update_attachment(&channel, &pending_id, &attachment.id, |a| *a = adopted.clone());
                    }
                    Callback.on_upload_complete(&channel, &pending_id, &attachment.id, &attachment.url);
                    if foreign {
                        let (signer, url) = (signer.clone(), attachment.url.clone());
                        vector_core::db::spawn_bound(async move {
                            let _ = vector_core::blossom::mirror_blob_to_servers(
                                signer,
                                &url,
                                vector_core::state::get_blossom_servers(),
                                1,
                                Duration::from_secs(8),
                                &[],
                            )
                            .await;
                        });
                    }
                    uploaded.push(attachment);
                    continue;
                }
                Sealed::Body(body, guard) => (body, guard),
            };
            let pid = pending_id.clone();
            let progress: vector_core::blossom::ProgressCallback = Arc::new(move |pct, bytes| {
                Callback.on_upload_progress(&pid, pct.unwrap_or(0), bytes.unwrap_or(0))
            });
            let body = encrypted.clone();
            let accepted = match vector_core::blossom::upload_body_with_progress_and_failover(
                signer.clone(),
                servers.clone(),
                encrypted,
                Some(mime.as_str()),
                true,
                progress,
                Some(3),
                Some(Duration::from_secs(2)),
                cancel.clone(),
            )
            .await
            {
                Ok(a) => a,
                Err(e) => {
                    if !cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                        vector_core::sending::keep_for_retry(
                            &body,
                            sealed_file,
                            &attachment.id,
                            &attachment.key,
                            &attachment.nonce,
                            &pending_id,
                            &mime,
                        );
                    }
                    mark_failed(&channel, &pending_id).await;
                    return Err(format!("Upload failed: {e}"));
                }
            };
            if vector_core::media::keep_media()
                && vector_core::sending::keep_sent(
                    &body,
                    &attachment.id,
                    &attachment.key,
                    &attachment.nonce,
                    attachment.size.saturating_sub(16),
                    &mime,
                )
            {
                attachment.downloaded = true;
            }
            attachment.url = accepted.url.clone();
            {
                let mut state = vector_core::state::STATE.lock().await;
                let url = accepted.url.clone();
                state.update_attachment(&channel, &pending_id, &attachment.id, |a| {
                    a.url = url.clone().into_boxed_str();
                });
            }
            Callback.on_upload_complete(&channel, &pending_id, &attachment.id, &accepted.url);
            // BUD-04 mirrors ride the imeta as fallbacks; none never fails the send.
            let mirrors = vector_core::blossom::mirror_blob_to_servers(
                signer.clone(),
                &accepted.url,
                servers.clone(),
                2,
                Duration::from_secs(5),
                std::slice::from_ref(&accepted.server),
            )
            .await;
            if !mirrors.is_empty() {
                attachment.fallback_urls = mirrors.clone();
                let mut state = vector_core::state::STATE.lock().await;
                state.update_attachment(&channel, &pending_id, &attachment.id, |a| {
                    a.set_fallback_urls(mirrors.iter().map(|s| s.as_str().into()));
                });
            }
            uploaded.push(attachment);
        }

        let mut tags: Vec<_> = uploaded
            .iter()
            .map(vector_core::community::attachments::attachment_to_imeta)
            .collect();
        tags.extend(vector_core::text_spans::to_nostr_tags(&text_spans));
        let quote = match reply.as_deref() {
            Some(parent) => {
                let author = {
                    let st = vector_core::state::STATE.lock().await;
                    st.find_message(parent)
                        .and_then(|(_, m)| m.npub.as_deref().and_then(|n| nostr_sdk::prelude::PublicKey::parse(n).ok()))
                        .map(|pk| pk.to_hex())
                        .unwrap_or_default()
                };
                Some((parent.to_string(), author))
            }
            None => None,
        };
        let quote_ref = quote.as_ref().map(|(id, a)| (id.as_str(), a.as_str()));
        let emoji: Vec<(&str, &str)> = emoji_tags.iter().map(|t| (t.shortcode.as_str(), t.url.as_str())).collect();
        if let Some(exp) = self_destruct.and_then(vector_core::self_destruct::expiry_after) {
            tags.push(nostr_sdk::prelude::Tag::expiration(nostr_sdk::prelude::Timestamp::from_secs(exp)));
        }
        let transport = LiveTransport::with_timeout(Duration::from_secs(12));
        let sent = vector_core::community::v2::service::send_chat_message(
            &transport, &community, &ch, &content, quote_ref, &emoji, tags,
        )
        .await;
        match sent {
            Ok(real_id) => {
                let echoed = {
                    let mut state = vector_core::state::STATE.lock().await;
                    state.remove_message(&pending_id);
                    // Our own plaintexts are on hand: the echo must not fetch them back.
                    for local in &uploaded {
                        state.update_attachment(&channel, &real_id, &local.id, |a| {
                            a.set_verified(local.verified);
                            a.set_downloading(false);
                            if local.downloaded {
                                a.set_downloaded(true);
                                a.set_path(local.path.clone());
                            }
                        });
                    }
                    state.find_message(&real_id).map(|(_, m)| m.clone())
                };
                if let Some(msg) = echoed {
                    Callback.on_sent(&channel, &pending_id, &msg);
                    Callback.on_persist(&channel, &msg);
                }
                Ok(uploaded.iter().find_map(|a| a.webxdc_topic.clone()))
            }
            Err(e) => {
                mark_failed(&channel, &pending_id).await;
                Err(e)
            }
        }
    })
    .await
}
