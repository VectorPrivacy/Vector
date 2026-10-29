//! Reading a Community from a public invite link or an invite bundle alone: no account, no
//! database, nothing published. The bundle is opened and the Control Plane folded exactly as a
//! join's preview does, then every address its keys can name is listed.

use super::*;
use super::super::derive;
use super::super::writers::Ledger;
use serde::Serialize;

/// One address a Community's events are published under (their `pubkey`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedPlane {
    /// Hex x-only pubkey.
    pub pk: String,
    /// What is published there, for a reader.
    pub role: String,
    /// The kinds found there.
    pub kinds: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedChannel {
    pub id: String,
    pub name: String,
    pub private: bool,
    /// A Private Channel the link grants no key for: its posts can't be addressed.
    pub keyless: bool,
    pub epoch: u64,
    /// Gone from the folded Control Plane but named by the bundle; its history still stands.
    pub deleted: bool,
}

/// What to read of the Channels' posts: the newest `count` across `channels`, by name or id,
/// or across every Channel the keys can read when it names none.
#[derive(Debug, Clone, Default)]
pub struct MessageRead {
    pub count: usize,
    pub channels: Vec<String>,
}

/// A post as its author last left it: their own edits applied, their own deletes taken away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedMessage {
    pub id: String,
    pub channel: String,
    /// The author's npub.
    pub author: String,
    pub at_ms: u64,
    pub content: String,
    /// The post it answers, by id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inspection {
    pub community_id: String,
    pub name: String,
    pub owner: String,
    pub root_epoch: u64,
    pub relays: Vec<String>,
    /// The public link's own signer; a bundle read directly has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_signer: Option<String>,
    /// The owner's genesis was found under the link's root: the fold, and so the channel list,
    /// is the Community's own. Unverified, everything comes from the bundle alone.
    pub verified: bool,
    pub channels: Vec<InspectedChannel>,
    pub planes: Vec<InspectedPlane>,
    /// What the link's keys can't reach.
    pub gaps: Vec<String>,
    /// The newest posts, oldest first, when a [`MessageRead`] asked for them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<InspectedMessage>,
}

/// Open the public invite at `url` and name every plane its keys address. `relays` replaces both
/// the link's bootstrap relays and the Community's own, so the read touches only those.
pub async fn inspect_public_link<T: Transport + ?Sized>(transport: &T, url: &str, relays: Option<&[String]>, read: Option<&MessageRead>) -> Result<Inspection, String> {
    let parsed = invite::parse_invite_link(url).map_err(|e| e.to_string())?;
    let bundle = match relays {
        Some(r) => fetch_bundle_on(transport, &parsed, r).await?,
        None => fetch_public_bundle(transport, url).await?,
    };
    inspect(transport, &bundle, relays, Some(&parsed.link_signer), read).await
}

/// [`inspect_public_link`] for an invite bundle in hand (CORD-05 §1), as a Direct Invite
/// delivers it. `relays` replaces the bundle's.
pub async fn inspect_bundle<T: Transport + ?Sized>(transport: &T, bundle: &CommunityInvite, relays: Option<&[String]>, read: Option<&MessageRead>) -> Result<Inspection, String> {
    inspect(transport, bundle, relays, None, read).await
}

async fn inspect<T: Transport + ?Sized>(
    transport: &T,
    bundle: &CommunityInvite,
    relays: Option<&[String]>,
    link_signer: Option<&PublicKey>,
    read: Option<&MessageRead>,
) -> Result<Inspection, String> {
    let mut community = CommunityV2::from_bundle(bundle, 0)?;
    if let Some(r) = relays {
        community.relays = r.to_vec();
    }
    let from_bundle = community.channels.clone();
    let (folded, writers, verified) = match verify_owner_root_and_reconcile(transport, community.clone()).await {
        Ok(vj) => (vj.community, vj.writers, true),
        Err(_) => (community, Vec::new(), false),
    };

    let mut channels = folded.channels.clone();
    let deleted: Vec<ChannelV2> = from_bundle.into_iter().filter(|b| !channels.iter().any(|c| c.id == b.id)).collect();
    channels.extend(deleted.iter().cloned());
    let ledgers: Vec<(ChannelId, Ledger)> = writers
        .iter()
        .filter_map(|(ch_hex, content, _, _)| {
            let id = ChannelId(crate::simd::hex::hex_to_bytes_32_checked(ch_hex)?);
            Some((id, Ledger::parse(content)?))
        })
        .collect();

    let mut gaps = Vec::new();
    if folded.root_epoch.0 > 0 {
        gaps.push(format!("history before root epoch {} sits under earlier roots this link doesn't hold", folded.root_epoch.0));
    }
    for ch in channels.iter().filter(|c| c.private && c.key.is_some() && c.epoch.0 > 0) {
        gaps.push(format!("#{}'s posts before its epoch {} sit under keys this link doesn't hold", ch.name, ch.epoch.0));
    }
    for ch in channels.iter().filter(|c| c.private && c.key.is_none()) {
        gaps.push(format!("#{} is private and this link grants no key for it", ch.name));
    }

    let messages = match read {
        Some(read) => {
            let mut readable = folded.clone();
            readable.channels = channels.clone();
            read_messages(transport, &readable, read, &mut gaps).await?
        }
        None => Vec::new(),
    };

    Ok(Inspection {
        community_id: folded.id().to_hex(),
        name: folded.name.clone(),
        owner: crate::simd::hex::bytes_to_hex_32(&folded.identity.owner_xonly),
        root_epoch: folded.root_epoch.0,
        relays: folded.relays.clone(),
        link_signer: link_signer.map(PublicKey::to_hex),
        verified,
        channels: channels
            .iter()
            .map(|c| InspectedChannel {
                id: crate::simd::hex::bytes_to_hex_32(&c.id.0),
                name: c.name.clone(),
                private: c.private,
                keyless: c.private && c.key.is_none(),
                epoch: c.epoch.0,
                deleted: deleted.iter().any(|d| d.id == c.id),
            })
            .collect(),
        planes: planes(&folded, &channels, &ledgers, link_signer),
        gaps,
        messages,
    })
}

/// The newest `read.count` posts across the Channels it names, oldest first. A Channel that can't
/// be read is a gap, not a failure; one the keys can't address, or no such Channel, is an error.
async fn read_messages<T: Transport + ?Sized>(transport: &T, community: &CommunityV2, read: &MessageRead, gaps: &mut Vec<String>) -> Result<Vec<InspectedMessage>, String> {
    let named = |want: &str| {
        let want = want.trim_start_matches('#');
        community.channels.iter().find(|c| crate::simd::hex::bytes_to_hex_32(&c.id.0) == want.to_ascii_lowercase() || c.name.eq_ignore_ascii_case(want))
    };
    let picked: Vec<&ChannelV2> = if read.channels.is_empty() {
        community.channels.iter().filter(|c| !(c.private && c.key.is_none())).collect()
    } else {
        let mut picked = Vec::new();
        for want in &read.channels {
            let ch = named(want).ok_or_else(|| format!("no channel {want}; it has {}", community.channels.iter().map(|c| format!("#{}", c.name)).collect::<Vec<_>>().join(" ")))?;
            if ch.private && ch.key.is_none() {
                return Err(format!("#{} is private and these keys don't read it", ch.name));
            }
            picked.push(ch);
        }
        picked
    };
    let mut out = Vec::new();
    for ch in picked {
        let mut posts = 0usize;
        let fetched = fetch_channel_history(
            transport,
            community,
            &ch.id,
            read.count.clamp(20, 200),
            10,
            None,
            None,
            crate::community::transport::Evidence::Quorum,
            |page, _| {
                posts += page.iter().filter(|f| matches!(f.event, chat::ChatEvent::Message { .. })).count();
                posts < read.count
            },
            |_| Default::default(),
        )
        .await;
        match fetched {
            Ok(events) => out.extend(as_left(&ch.name, &events)),
            Err(e) => gaps.push(format!("#{}'s posts couldn't be read: {e}", ch.name)),
        }
    }
    out.sort_by_key(|m| m.at_ms);
    let skip = out.len().saturating_sub(read.count);
    Ok(out.split_off(skip))
}

/// A Channel's posts as their authors left them: an author's latest edit of their own post
/// replaces its text, and their own delete takes it away. Moderation is the fold's to judge.
fn as_left(channel: &str, events: &[FetchedEvent]) -> Vec<InspectedMessage> {
    use nostr_sdk::prelude::ToBech32;
    let mut posts: Vec<InspectedMessage> = Vec::new();
    let mut authors: Vec<PublicKey> = Vec::new();
    for f in events {
        if let chat::ChatEvent::Message { opened, reply_to, .. } = &f.event {
            posts.push(InspectedMessage {
                id: opened.rumor_id.to_hex(),
                channel: channel.to_string(),
                author: opened.author.to_bech32().unwrap_or_else(|_| opened.author.to_hex()),
                at_ms: opened.at_ms,
                content: opened.rumor.content.clone(),
                reply_to: reply_to.as_ref().map(|r| crate::simd::hex::bytes_to_hex_32(&r.id)),
                edited: false,
            });
            authors.push(opened.author);
        }
    }
    let at = |target: &[u8; 32], by: &PublicKey| {
        let hex = crate::simd::hex::bytes_to_hex_32(target);
        posts.iter().position(|p| p.id == hex).filter(|&i| authors[i] == *by)
    };
    let mut edits: Vec<(usize, u64, String)> = Vec::new();
    let mut gone: Vec<usize> = Vec::new();
    for f in events {
        match &f.event {
            chat::ChatEvent::Edit { opened, target, new_content, .. } => {
                if let Some(i) = at(target, &opened.author) {
                    edits.push((i, opened.at_ms, new_content.clone()));
                }
            }
            chat::ChatEvent::Delete { opened, target, .. } => gone.extend(at(target, &opened.author)),
            _ => {}
        }
    }
    edits.sort_by_key(|(_, at_ms, _)| *at_ms);
    for (i, _, text) in edits {
        posts[i].content = text;
        posts[i].edited = true;
    }
    posts.into_iter().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, p)| p).collect()
}

/// Every address a holder of `community`'s keys can name: the Control Plane (and the legacy
/// one a split Community began on), the Guestbook, the dissolution plane, the rotation planes,
/// each Channel's chat, access, rekey and voice planes, each writer address, and the link.
fn planes(community: &CommunityV2, channels: &[ChannelV2], ledgers: &[(ChannelId, Ledger)], link_signer: Option<&PublicKey>) -> Vec<InspectedPlane> {
    let id = community.id();
    let (root, epoch) = (&community.community_root, community.root_epoch);
    let next = |e: Epoch| Epoch(e.0.saturating_add(1));
    let mut out: Vec<InspectedPlane> = Vec::new();
    let mut push = |pk: PublicKey, role: String, kinds: &[u16]| {
        let pk = pk.to_hex();
        if !out.iter().any(|p| p.pk == pk) {
            out.push(InspectedPlane { pk, role, kinds: kinds.to_vec() });
        }
    };
    let wrap = &[stream::KIND_WRAP];

    push(control::ControlPlane::of(community).pk(), "control".into(), wrap);
    push(derive::control_group_key(root, id, epoch).pk(), "control (legacy address)".into(), wrap);
    push(derive::guestbook_group_key(root, id, epoch).pk(), "guestbook".into(), wrap);
    push(base_rekey_group_key(root, id, next(epoch)).pk(), format!("root rotation to epoch {}", next(epoch).0), wrap);
    push(derive::dissolved_group_key(id).pk(), "dissolution".into(), wrap);
    for ch in channels {
        let name = &ch.name;
        if !(ch.private && ch.key.is_none()) {
            let (secret, ch_epoch) = community.channel_secret(ch);
            push(channel_group_key(&secret, &ch.id, ch_epoch).pk(), format!("#{name} posts"), &[stream::KIND_WRAP, stream::KIND_WRAP_EPHEMERAL]);
            if ch.voice == Some(true) {
                push(derive::voice_group_key(&secret, &ch.id, ch_epoch).pk(), format!("#{name} voice"), &[stream::KIND_WRAP_EPHEMERAL]);
            }
        }
        if ch.private {
            push(derive::channel_access_group_key(root, &ch.id, epoch).pk(), format!("#{name} access"), wrap);
            for e in 1..=ch.epoch.0.saturating_add(1) {
                push(channel_rekey_group_key(root, &ch.id, Epoch(e)).pk(), format!("#{name} rotation to epoch {e}"), wrap);
            }
        }
        for (_, ledger) in ledgers.iter().filter(|(c, _)| *c == ch.id) {
            for (w_epoch, writer, _) in ledger.writers() {
                if let Ok(pk) = PublicKey::from_slice(&writer) {
                    push(pk, format!("#{name} writer address, epoch {}", w_epoch.0), wrap);
                }
            }
        }
    }
    if let Some(signer) = link_signer {
        push(*signer, "this invite link".into(), &[super::super::kind::INVITE_BUNDLE]);
    }
    out
}
