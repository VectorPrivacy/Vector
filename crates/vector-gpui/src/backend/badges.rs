//! Profile badges: our own from the flags the post-sync refresh keeps, anyone else's looked up
//! once per session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use nostr_sdk::prelude::*;
use vector_core::badges;
use vector_core::tags::TagsExt;

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Badges {
    /// Users who accepted this one's Beta invite.
    pub invites: u32,
    pub fawkes: bool,
    /// Bug Hunter tier, 0 for none.
    pub bug: u8,
    pub v2026: bool,
}

struct Seen;
fn seen() -> Arc<Mutex<HashMap<String, Badges>>> {
    vector_core::db::current_session().scoped::<Seen, _>()
}

/// Our own live looks run once per session, while the flags read false.
struct OwnChecked;
fn own_checked() -> Arc<Mutex<bool>> {
    vector_core::db::current_session().scoped::<OwnChecked, _>()
}

/// Every badge `npub` holds. Each lookup that fails reads as not held.
pub async fn of(npub: String, mine: bool) -> Badges {
    if !mine && let Some(b) = seen().lock().unwrap().get(&npub).copied() {
        return b;
    }
    let Ok(pk) = PublicKey::from_bech32(&npub) else {
        return Badges::default();
    };
    let invites = invited(pk).await.unwrap_or(0);
    let mut b = Badges {
        invites,
        ..Default::default()
    };
    if mine {
        b.fawkes = badges::has_vector_badge();
        b.v2026 = badges::has_vector_2026_badge();
        b.bug = badges::bug_hunter_tier();
        let first = !std::mem::replace(&mut *own_checked().lock().unwrap(), true);
        if first {
            if !b.fawkes {
                b.fawkes = badges::has_fawkes_badge(&pk).await.unwrap_or(false);
                badges::note_own_badge_confirmed(&pk, b.fawkes);
            }
            if !b.v2026 {
                b.v2026 = badges::check_v2026(&pk).await.unwrap_or(false);
            }
            if b.bug == 0 {
                b.bug = badges::fetch_bug_hunter_tier(&pk)
                    .await
                    .map(|(t, _)| t)
                    .unwrap_or(0);
            }
        }
        return b;
    }
    b.fawkes = badges::has_fawkes_badge(&pk).await.unwrap_or(false);
    b.v2026 = badges::check_v2026(&pk).await.unwrap_or(false);
    b.bug = badges::fetch_bug_hunter_tier(&pk)
        .await
        .map(|(t, _)| t)
        .unwrap_or(0);
    seen().lock().unwrap().insert(npub, b);
    b
}

/// How many distinct users accepted `inviter`'s invite code, read from the trusted relays.
async fn invited(inviter: PublicKey) -> Result<u32, String> {
    let client = vector_core::state::nostr_client().ok_or("Nostr client not initialized")?;
    let relays = vector_core::state::active_trusted_relays().await;
    let timeout = vector_core::relay_request_timeout(std::time::Duration::from_secs(10));
    let filter = Filter::new()
        .author(inviter)
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, "vector")
        .limit(100);
    let mut events = client
        .stream_events(ReqTarget::manual(
            relays.iter().map(|u| (*u, vec![filter.clone()])),
        ))
        .timeout(timeout)
        .await
        .map_err(|e| e.to_string())?;
    let mut code = None;
    while let Some((_, res)) = events.next().await {
        let Ok(event) = res else { continue };
        if event.content == "vector_invite"
            && let Some(c) = event.tags.find_kind("r").and_then(|t| t.content())
        {
            code = Some(c.to_string());
            break;
        }
    }
    let code = code.ok_or("No invite code found for this user")?;
    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .custom_tag(SingleLetterTag::LOWERCASE_D, code)
        .limit(1000);
    let mut events = client
        .stream_events(ReqTarget::manual(
            relays.iter().map(|u| (*u, vec![filter.clone()])),
        ))
        .timeout(timeout)
        .await
        .map_err(|e| e.to_string())?;
    let mut acceptors = std::collections::HashSet::new();
    while let Some((_, res)) = events.next().await {
        let Ok(event) = res else { continue };
        if event.content == "vector_invite_accepted"
            && event.tags.public_keys().any(|pk| pk == inviter)
        {
            acceptors.insert(event.pubkey);
        }
    }
    Ok(acceptors.len() as u32)
}
