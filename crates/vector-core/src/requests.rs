//! DM requests: a GUI shields its user from someone who wrote first until they answer or
//! accept. Off unless a GUI turns it on, so SDK clients and bots take every DM and invite
//! as they always have.

use std::sync::atomic::{AtomicBool, Ordering};

static SHIELD: AtomicBool = AtomicBool::new(false);

/// Called once at startup by a client with a person behind it.
pub fn enable_shield() {
    SHIELD.store(true, Ordering::Relaxed);
}

/// Whether a stranger's wallpaper, attachment probes and blocked invites are held back.
pub fn shielded() -> bool {
    SHIELD.load(Ordering::Relaxed)
}

/// Of these people, the ones a Community we hold counts as a member.
pub fn in_our_communities<'a>(npubs: impl IntoIterator<Item = &'a str>) -> std::collections::HashSet<String> {
    let mut wanted: std::collections::HashMap<nostr_sdk::prelude::PublicKey, &str> =
        npubs.into_iter().filter_map(|npub| Some((nostr_sdk::prelude::PublicKey::parse(npub).ok()?, npub))).collect();
    let mut found = std::collections::HashSet::new();
    if wanted.is_empty() {
        return found;
    }
    for community in crate::community::v2::realtime::load_held_v2_cached().communities() {
        for member in crate::community::v2::service::stored_memberlist(community).unwrap_or_default() {
            if let Some(npub) = wanted.remove(&member) {
                found.insert(npub.to_string());
            }
        }
        if wanted.is_empty() {
            break;
        }
    }
    found
}

/// Whether this person's pictures wait on their DM request: we never answered or accepted it,
/// and no Community we hold counts them, where we see them anyway.
pub async fn pictures_held(npub: &str) -> bool {
    let request = crate::state::STATE.lock().await.sent_request(npub);
    request && in_our_communities([npub]).is_empty()
}

/// Everyone whose pictures wait on their DM request.
pub async fn held_pictures() -> std::collections::HashSet<String> {
    let senders: Vec<String> = crate::state::STATE.lock().await.request_senders().into_iter().map(str::to_string).collect();
    let mutual = in_our_communities(senders.iter().map(String::as_str));
    senders.into_iter().filter(|npub| !mutual.contains(npub)).collect()
}

/// The DM requests whose senders show their pictures anyway: a Community we hold counts them.
pub async fn request_mutuals() -> Vec<String> {
    let senders: Vec<String> = crate::state::STATE.lock().await.request_senders().into_iter().map(str::to_string).collect();
    in_our_communities(senders.iter().map(String::as_str)).into_iter().collect()
}
