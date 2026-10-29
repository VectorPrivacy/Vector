//! Reading a Community from a public invite link alone: no account, no database, nothing
//! published. The bundle is opened and the Control Plane folded exactly as a join's preview
//! does, then every address the link's keys can name is listed.

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

#[derive(Debug, Clone, Serialize)]
pub struct Inspection {
    pub community_id: String,
    pub name: String,
    pub owner: String,
    pub root_epoch: u64,
    pub relays: Vec<String>,
    pub link_signer: String,
    /// The owner's genesis was found under the link's root: the fold, and so the channel list,
    /// is the Community's own. Unverified, everything comes from the bundle alone.
    pub verified: bool,
    pub channels: Vec<InspectedChannel>,
    pub planes: Vec<InspectedPlane>,
    /// What the link's keys can't reach.
    pub gaps: Vec<String>,
}

/// Open the public invite at `url` and name every plane its keys address. `relays` replaces both
/// the link's bootstrap relays and the Community's own, so the read touches only those.
pub async fn inspect_public_link<T: Transport + ?Sized>(transport: &T, url: &str, relays: Option<&[String]>) -> Result<Inspection, String> {
    let parsed = invite::parse_invite_link(url).map_err(|e| e.to_string())?;
    let bundle = match relays {
        Some(r) => fetch_bundle_on(transport, &parsed, r).await?,
        None => fetch_public_bundle(transport, url).await?,
    };
    let mut community = CommunityV2::from_bundle(&bundle, 0)?;
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

    Ok(Inspection {
        community_id: folded.id().to_hex(),
        name: folded.name.clone(),
        owner: crate::simd::hex::bytes_to_hex_32(&folded.identity.owner_xonly),
        root_epoch: folded.root_epoch.0,
        relays: folded.relays.clone(),
        link_signer: parsed.link_signer.to_hex(),
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
        planes: planes(&folded, &channels, &ledgers, &parsed.link_signer),
        gaps,
    })
}

/// Every address a holder of `community`'s keys can name: the Control Plane (and the legacy
/// one a split Community began on), the Guestbook, the dissolution plane, the rotation planes,
/// each Channel's chat, access, rekey and voice planes, each writer address, and the link.
fn planes(community: &CommunityV2, channels: &[ChannelV2], ledgers: &[(ChannelId, Ledger)], link_signer: &PublicKey) -> Vec<InspectedPlane> {
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
    push(*link_signer, "this invite link".into(), &[super::super::kind::INVITE_BUNDLE]);
    out
}
