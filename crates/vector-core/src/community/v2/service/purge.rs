//! A deleted Channel leaves every device that sees it go (CORD-03 §2): what it holds of the
//! Chat Plane, the files it downloaded there, the files its account uploaded there that nothing
//! else uses, and in time its account's own wraps on the relays. Its keys stay, as the spec asks.

use super::*;

/// What a purge took away.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Purged {
    /// My uploads in it that nothing else uses, asked off their servers.
    pub uploads: Vec<String>,
    /// My events in it queued to leave the relays.
    pub scrubs: usize,
}

/// Drop everything this device holds of `channel_id`, whose deletion `community`'s Control
/// Plane verifiably carries. A second run finds nothing, so it may run on every fold.
pub(crate) async fn purge_deleted_channel(community: &CommunityV2, channel_id: &ChannelId) -> Option<Purged> {
    let (cid_hex, ch_hex) = (crate::simd::hex::bytes_to_hex_32(&community.id().0), crate::simd::hex::bytes_to_hex_32(&channel_id.0));
    let events = match crate::db::community::channel_events(&ch_hex) {
        Ok(events) => events,
        Err(e) => {
            crate::log_warn!("[Community] reading deleted channel {}: {e}", &ch_hex[..8]);
            return None;
        }
    };
    // Mine are the ones whose wraps I kept keys for; read before the rows go.
    let mine: Vec<String> = events
        .iter()
        .map(|(id, _)| id)
        .filter(|id| matches!(crate::db::community::has_wrap_owner(id), Ok(true)) || matches!(crate::db::community::has_message_key(id), Ok(true)))
        .cloned()
        .collect();
    let mut files = Vec::new();
    for (id, _) in events.iter().filter(|(_, has_files)| *has_files) {
        if let Ok(Some((_, msg))) = crate::db::events::get_message_by_id(id).await {
            files.push((id.clone(), msg.mine, msg.attachments));
        }
    }

    let in_memory = {
        let mut st = crate::state::STATE.lock().await;
        let before = st.chats.len();
        st.chats.retain(|c| c.id != ch_hex);
        st.chats.len() != before
    };
    let held = match crate::db::community::forget_channel_chat(&cid_hex, &ch_hex) {
        Ok(held) => held,
        Err(e) => {
            crate::log_warn!("[Community] forgetting deleted channel {}: {e}", &ch_hex[..8]);
            return None;
        }
    };
    if !held && !in_memory {
        return None;
    }

    // With the Channel's own rows gone, a file is unused once nothing outside it holds it.
    let mut uploads = Vec::new();
    for (id, mine, attachments) in files {
        if mine {
            uploads.extend(attachments.iter().flat_map(|a| a.all_urls().map(str::to_string)));
        }
        crate::deletion::delete_cached_attachment_files_pub(&crate::deletion::filter_unreferenced_attachments(&id, attachments).await);
    }
    let uploads = crate::db::attachments::urls_unreferenced_elsewhere(&uploads, "").unwrap_or_default();
    if !uploads.is_empty() {
        if let Ok(signer) = crate::signer::active_signer() {
            crate::blossom::delete_blobs_best_effort(signer, uploads.clone());
        }
    }
    crate::community::message_delete::queue_scrubs(&cid_hex, &mine);
    crate::traits::emit_event("channel_purged", &serde_json::json!({ "community_id": cid_hex, "channel_id": ch_hex }));
    Some(Purged { uploads, scrubs: mine.len() })
}
