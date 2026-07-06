//! CORD-06: Rekeys and Refoundings — asynchronous key rotation for
//! post-removal secrecy.
//!
//! A rotation delivers the fresh key as per-recipient blobs (120 per kind
//! 3303 event) at an address derived from the *prior* secret. A removed
//! member receives no blob — that absence is the entire secrecy mechanism;
//! `prevcommit` is a convergence check keeping honest members on one shared
//! chain, and authority is verified by the seal (holding a key is never
//! authority).

use nostr_sdk::nips::nip44::v2::{decrypt_to_bytes, encrypt_to_bytes, ConversationKey};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use super::control::ControlFold;
use super::derive::{epoch_key_commitment, recipient_locator, RekeyScope};
use super::edition::Citation;
use super::stream::TAG_MS;
use super::{kind, perm, split_ms, Epoch, REKEY_RECIPIENTS_PER_EVENT, ZERO_ID};

/// One located, wrapped key inside a rotation event's content array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RekeyBlob {
    /// Where its recipient finds it (`derive::recipient_locator`, hex).
    pub locator: String,
    /// NIP-44 ciphertext under the Rotator↔recipient pairwise key, base64.
    pub wrapped: String,
}

/// One parsed kind 3303 chunk. A rotation to many recipients spans several,
/// correlated by the Rotator at one `new_epoch` and `prevcommit`.
#[derive(Debug, Clone)]
pub struct RotationChunk {
    /// The seal-verified Rotator.
    pub rotator: PublicKey,
    pub scope: RekeyScope,
    pub new_epoch: Epoch,
    pub prev_epoch: Epoch,
    pub prevcommit: [u8; 32],
    /// (i, n), 1-based.
    pub chunk: (u32, u32),
    pub blobs: Vec<RekeyBlob>,
    pub citation: Option<Citation>,
}

/// The wrapped plaintext is fixed-width, 72 bytes: `scope_id ‖ epoch_be ‖
/// new_key`. Scope and epoch live *inside* the ciphertext so a blob minted
/// for one channel can never be replayed against another.
const WRAPPED_LEN: usize = 32 + 8 + 32;

#[derive(Debug)]
pub enum RekeyError {
    Malformed(String),
    Crypto(String),
}

impl std::fmt::Display for RekeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RekeyError::Malformed(e) => write!(f, "malformed rekey: {e}"),
            RekeyError::Crypto(e) => write!(f, "rekey crypto: {e}"),
        }
    }
}

impl std::error::Error for RekeyError {}

/// Build a rotation as its chunk rumors: every recipient's blob, 120 per
/// event, all chunks carrying identical continuity fields. The state being
/// rotated is acquired in full before the first publish, so a mid-flight
/// failure never leaves half a rotation as the only copy.
#[allow(clippy::too_many_arguments)]
pub fn build_rotation(
    rotator: &Keys,
    scope: RekeyScope,
    prev_epoch: Epoch,
    prev_key: &[u8; 32],
    new_epoch: Epoch,
    new_key: &[u8; 32],
    recipients: &[PublicKey],
    citation: Option<&Citation>,
    unix_ms: u64,
) -> Result<Vec<UnsignedEvent>, RekeyError> {
    let prevcommit = epoch_key_commitment(prev_epoch, prev_key);
    let scope_id = scope.id32();

    let mut plaintext = [0u8; WRAPPED_LEN];
    plaintext[..32].copy_from_slice(&scope_id);
    plaintext[32..40].copy_from_slice(&new_epoch.0.to_be_bytes());
    plaintext[40..].copy_from_slice(new_key);

    let mut blobs = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        let conv = ConversationKey::derive(rotator.secret_key(), recipient)
            .map_err(|e| RekeyError::Crypto(e.to_string()))?;
        let ct = encrypt_to_bytes(&conv, &plaintext).map_err(|e| RekeyError::Crypto(e.to_string()))?;
        let locator = recipient_locator(
            &rotator.public_key().to_bytes(),
            &recipient.to_bytes(),
            scope,
            new_epoch,
        );
        blobs.push(RekeyBlob {
            locator: crate::simd::hex::bytes_to_hex_32(&locator),
            wrapped: base64_simd::STANDARD.encode_to_string(&ct),
        });
    }

    let chunks: Vec<&[RekeyBlob]> = if blobs.is_empty() {
        vec![&[]]
    } else {
        blobs.chunks(REKEY_RECIPIENTS_PER_EVENT).collect()
    };
    let n = chunks.len();
    let (secs, remainder) = split_ms(unix_ms);

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let content = serde_json::to_string(chunk).map_err(|e| RekeyError::Malformed(e.to_string()))?;
            let mut tags = vec![
                Tag::custom(TagKind::Custom(TAG_MS.into()), [remainder.to_string()]),
                Tag::custom(TagKind::Custom("scope".into()), [crate::simd::hex::bytes_to_hex_32(&scope_id)]),
                Tag::custom(TagKind::Custom("newepoch".into()), [new_epoch.0.to_string()]),
                Tag::custom(TagKind::Custom("prevepoch".into()), [prev_epoch.0.to_string()]),
                Tag::custom(TagKind::Custom("prevcommit".into()), [crate::simd::hex::bytes_to_hex_32(&prevcommit)]),
                Tag::custom(TagKind::Custom("chunk".into()), [(i + 1).to_string(), n.to_string()]),
            ];
            if let Some(c) = citation {
                tags.push(Tag::custom(
                    TagKind::Custom(super::edition::TAG_VAC.into()),
                    [
                        crate::simd::hex::bytes_to_hex_32(&c.grant_eid),
                        c.grant_version.to_string(),
                        crate::simd::hex::bytes_to_hex_32(&c.grant_hash),
                    ],
                ));
            }
            let mut rumor = EventBuilder::new(Kind::Custom(kind::REKEY), content)
                .tags(tags)
                .custom_created_at(Timestamp::from_secs(secs))
                .build(rotator.public_key());
            rumor.ensure_id();
            Ok(rumor)
        })
        .collect()
}

fn tag_parts<'a>(tags: &'a Tags, name: &str) -> Option<Vec<&'a str>> {
    tags.iter()
        .find(|t| t.kind() == TagKind::Custom(name.into()))
        .map(|t| t.as_slice().iter().skip(1).map(|s| s.as_str()).collect())
}

fn tag_value<'a>(tags: &'a Tags, name: &str) -> Option<&'a str> {
    tag_parts(tags, name).and_then(|p| p.first().copied())
}

/// Parse a seal-verified kind 3303 rumor into a [`RotationChunk`].
pub fn parse_rotation(rumor: &UnsignedEvent) -> Result<RotationChunk, RekeyError> {
    if rumor.kind.as_u16() != kind::REKEY {
        return Err(RekeyError::Malformed(format!("kind {} is not a rekey", rumor.kind)));
    }
    let scope_hex = tag_value(&rumor.tags, "scope").ok_or_else(|| RekeyError::Malformed("no scope".into()))?;
    let scope_id = crate::simd::hex::hex_to_bytes_32_checked(scope_hex)
        .ok_or_else(|| RekeyError::Malformed("scope not 32-byte hex".into()))?;
    let new_epoch: u64 = tag_value(&rumor.tags, "newepoch")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| RekeyError::Malformed("bad newepoch".into()))?;
    let prev_epoch: u64 = tag_value(&rumor.tags, "prevepoch")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| RekeyError::Malformed("bad prevepoch".into()))?;
    let prevcommit = tag_value(&rumor.tags, "prevcommit")
        .and_then(crate::simd::hex::hex_to_bytes_32_checked)
        .ok_or_else(|| RekeyError::Malformed("bad prevcommit".into()))?;
    let chunk_parts = tag_parts(&rumor.tags, "chunk").ok_or_else(|| RekeyError::Malformed("no chunk".into()))?;
    let chunk: (u32, u32) = (
        chunk_parts.first().and_then(|v| v.parse().ok()).ok_or_else(|| RekeyError::Malformed("bad chunk i".into()))?,
        chunk_parts.get(1).and_then(|v| v.parse().ok()).ok_or_else(|| RekeyError::Malformed("bad chunk n".into()))?,
    );
    if chunk.0 == 0 || chunk.1 == 0 || chunk.0 > chunk.1 {
        return Err(RekeyError::Malformed("chunk indices are 1-based i ≤ n".into()));
    }
    let blobs: Vec<RekeyBlob> =
        serde_json::from_str(&rumor.content).map_err(|e| RekeyError::Malformed(e.to_string()))?;
    let citation = tag_parts(&rumor.tags, super::edition::TAG_VAC).and_then(|p| {
        Some(Citation {
            grant_eid: crate::simd::hex::hex_to_bytes_32_checked(p.first()?)?,
            grant_version: p.get(1)?.parse().ok()?,
            grant_hash: crate::simd::hex::hex_to_bytes_32_checked(p.get(2)?)?,
        })
    });
    Ok(RotationChunk {
        rotator: rumor.pubkey,
        scope: RekeyScope::from_id32(scope_id),
        new_epoch: Epoch(new_epoch),
        prev_epoch: Epoch(prev_epoch),
        prevcommit,
        chunk,
        blobs,
        citation,
    })
}

/// Continuity verdict (CORD-06 §2) for the key the receiver currently holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuity {
    /// The rotation extends the very key you hold.
    Extends,
    /// A mismatch with a higher `prevepoch`: you missed a rotation — fetch
    /// the gap first.
    MissedRotation,
    /// Any other mismatch: a fork or garbage. Reject.
    Fork,
}

pub fn verify_continuity(chunk: &RotationChunk, held_epoch: Epoch, held_key: &[u8; 32]) -> Continuity {
    if chunk.prev_epoch == held_epoch && chunk.prevcommit == epoch_key_commitment(held_epoch, held_key) {
        Continuity::Extends
    } else if chunk.prev_epoch > held_epoch {
        Continuity::MissedRotation
    } else {
        Continuity::Fork
    }
}

/// The receiver's verdict over the chunks of one rotation (correlate by
/// Rotator + `new_epoch` + `prevcommit` before calling).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RekeyOutcome {
    /// Your blob was found, decrypted, and its inner scope/epoch verified
    /// against the tags: shift to the new epoch with this key.
    NewKey([u8; 32]),
    /// All `n` chunks are held and none contains your locator: removed.
    Removed,
    /// A missing chunk is never a removal — keep recovering.
    Incomplete,
}

/// Search a rotation's chunks for my blob. Chunks with inconsistent
/// continuity fields are rejected (all chunks of one rotation carry
/// identical ones).
pub fn find_my_key(chunks: &[RotationChunk], my_keys: &Keys) -> Result<RekeyOutcome, RekeyError> {
    let Some(first) = chunks.first() else {
        return Ok(RekeyOutcome::Incomplete);
    };
    let n = first.chunk.1;
    for c in chunks {
        if c.rotator != first.rotator
            || c.new_epoch != first.new_epoch
            || c.prev_epoch != first.prev_epoch
            || c.prevcommit != first.prevcommit
            || c.scope != first.scope
            || c.chunk.1 != n
        {
            return Err(RekeyError::Malformed("mixed chunks from different rotations".into()));
        }
    }

    let my_locator = crate::simd::hex::bytes_to_hex_32(&recipient_locator(
        &first.rotator.to_bytes(),
        &my_keys.public_key().to_bytes(),
        first.scope,
        first.new_epoch,
    ));

    for c in chunks {
        for blob in &c.blobs {
            if blob.locator != my_locator {
                continue;
            }
            // One ECDH either side can compute — a NIP-46 bunker opens its
            // blob with a single nip44_decrypt.
            let conv = ConversationKey::derive(my_keys.secret_key(), &first.rotator)
                .map_err(|e| RekeyError::Crypto(e.to_string()))?;
            let ct = base64_simd::STANDARD
                .decode_to_vec(blob.wrapped.as_bytes())
                .map_err(|e| RekeyError::Malformed(format!("blob base64: {e}")))?;
            let pt = decrypt_to_bytes(&conv, &ct).map_err(|e| RekeyError::Crypto(e.to_string()))?;
            if pt.len() != WRAPPED_LEN {
                return Err(RekeyError::Malformed(format!("wrapped plaintext is {} bytes, not 72", pt.len())));
            }
            // Verify the inner scope and epoch against the tags before
            // accepting the key — what makes a blob unspliceable.
            let inner_scope: [u8; 32] = pt[..32].try_into().expect("sized");
            let inner_epoch = u64::from_be_bytes(pt[32..40].try_into().expect("sized"));
            if inner_scope != first.scope.id32() || inner_epoch != first.new_epoch.0 {
                return Err(RekeyError::Malformed("blob scope/epoch do not match the event".into()));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&pt[40..]);
            return Ok(RekeyOutcome::NewKey(key));
        }
    }

    let held: std::collections::HashSet<u32> = chunks.iter().map(|c| c.chunk.0).collect();
    if (1..=n).all(|i| held.contains(&i)) {
        Ok(RekeyOutcome::Removed)
    } else {
        Ok(RekeyOutcome::Incomplete)
    }
}

/// Rotation authority (CORD-06 §Authority): a single-channel Rekey requires
/// `MANAGE_CHANNELS`, a Refounding `BAN`; the Rotator cites their Grant like
/// any authority action and must strictly outrank every removed target.
/// Holding a key is never authority.
pub fn may_rotate(
    control: &ControlFold,
    rotator: &PublicKey,
    citation: Option<&Citation>,
    scope: RekeyScope,
    removed: &[PublicKey],
) -> bool {
    if control.is_banned(rotator) {
        return false;
    }
    if rotator != control.owner() {
        let Some(c) = citation else { return false };
        match control.accepted_hash(&c.grant_eid, c.grant_version) {
            Some(h) if h == c.grant_hash => {}
            _ => return false,
        }
    }
    let bit = match scope {
        RekeyScope::Channel(_) => perm::MANAGE_CHANNELS,
        RekeyScope::CommunityRoot => perm::BAN,
    };
    if control.roster().permissions(rotator) & bit == 0 {
        return false;
    }
    let rank = control.roster().rank(rotator);
    removed.iter().all(|t| rank.outranks(&control.roster().rank(t)))
}

/// Two rotations racing to the same epoch converge deterministically: among
/// authorized candidates at the same continuity point, the lexicographically
/// lowest new key wins — every client computes the same winner.
pub fn rotation_race_winner(candidate_keys: &[[u8; 32]]) -> Option<[u8; 32]> {
    candidate_keys.iter().min().copied()
}

/// The same-epoch heal is **down-only**: a held epoch re-converges solely to
/// a strictly lower sibling, so a flaky fetch returning only the higher one
/// can never re-fork a settled epoch.
pub fn should_reconverge(held_key: &[u8; 32], sibling_key: &[u8; 32]) -> bool {
    sibling_key < held_key
}

/// The all-zero scope hex — a base rotation's `scope` tag value.
pub fn community_root_scope_hex() -> String {
    crate::simd::hex::bytes_to_hex_32(&ZERO_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concord::v2::control::FoldMode;
    use crate::concord::v2::derive::grant_locator;
    use crate::concord::v2::edition::{build_edition_rumor, parse_edition};
    use crate::concord::v2::roster::{Grant, Role, RoleScope};
    use crate::concord::v2::{vsk, ChannelId, CommunityId, OwnerSalt};

    fn chunk_set(recipients: &[Keys], rotator: &Keys, scope: RekeyScope) -> (Vec<RotationChunk>, [u8; 32]) {
        let new_key = [0x55u8; 32];
        let pks: Vec<PublicKey> = recipients.iter().map(|k| k.public_key()).collect();
        let rumors = build_rotation(
            rotator,
            scope,
            Epoch(2),
            &[0x11; 32],
            Epoch(3),
            &new_key,
            &pks,
            None,
            1_722_500_000_123,
        )
        .unwrap();
        (rumors.iter().map(|r| parse_rotation(r).unwrap()).collect(), new_key)
    }

    #[test]
    fn recipient_finds_and_decrypts_their_blob() {
        let rotator = Keys::generate();
        let members: Vec<Keys> = (0..5).map(|_| Keys::generate()).collect();
        let scope = RekeyScope::Channel(ChannelId([0x77; 32]));
        let (chunks, new_key) = chunk_set(&members, &rotator, scope);
        assert_eq!(chunks.len(), 1);
        for member in &members {
            assert_eq!(find_my_key(&chunks, member).unwrap(), RekeyOutcome::NewKey(new_key));
        }
    }

    #[test]
    fn removed_member_sees_removal_only_with_all_chunks() {
        let rotator = Keys::generate();
        // 130 recipients → 2 chunks.
        let members: Vec<Keys> = (0..130).map(|_| Keys::generate()).collect();
        let removed = Keys::generate();
        let (chunks, _) = chunk_set(&members, &rotator, RekeyScope::CommunityRoot);
        assert_eq!(chunks.len(), 2, "recipients chunk at 120 per event");

        // Holding only one chunk: a missing chunk is never a removal.
        assert_eq!(find_my_key(&chunks[..1], &removed).unwrap(), RekeyOutcome::Incomplete);
        // Holding all n and no locator: removed.
        assert_eq!(find_my_key(&chunks, &removed).unwrap(), RekeyOutcome::Removed);
        // A member found in chunk 2 still resolves.
        assert!(matches!(find_my_key(&chunks, &members[125]).unwrap(), RekeyOutcome::NewKey(_)));
    }

    #[test]
    fn blob_binds_scope_and_epoch_inside_the_ciphertext() {
        let rotator = Keys::generate();
        let member = Keys::generate();
        let scope_a = RekeyScope::Channel(ChannelId([0xAA; 32]));
        let scope_b = RekeyScope::Channel(ChannelId([0xBB; 32]));
        let (chunks_a, _) = chunk_set(&[member.clone()], &rotator, scope_a);

        // Splice chunk A's blobs under channel B's tags (attacker re-tags the
        // event): the locator no longer matches — and even if the locator is
        // recomputed, the inner scope check fails.
        let mut spliced = chunks_a[0].clone();
        spliced.scope = scope_b;
        // Recompute the locator the victim would search under scope B.
        let loc_b = crate::simd::hex::bytes_to_hex_32(&recipient_locator(
            &rotator.public_key().to_bytes(),
            &member.public_key().to_bytes(),
            scope_b,
            spliced.new_epoch,
        ));
        spliced.blobs[0].locator = loc_b;
        assert!(find_my_key(&[spliced], &member).is_err(), "inner scope mismatch is rejected");
    }

    #[test]
    fn continuity_extends_missed_or_fork() {
        let rotator = Keys::generate();
        let member = Keys::generate();
        let (chunks, _) = chunk_set(&[member], &rotator, RekeyScope::CommunityRoot);
        let c = &chunks[0];
        // Holding epoch 2 with the exact key: extends.
        assert_eq!(verify_continuity(c, Epoch(2), &[0x11; 32]), Continuity::Extends);
        // Holding an older epoch: you missed a rotation, fetch the gap.
        assert_eq!(verify_continuity(c, Epoch(1), &[0x10; 32]), Continuity::MissedRotation);
        // Same epoch, different key: a fork — reject.
        assert_eq!(verify_continuity(c, Epoch(2), &[0x99; 32]), Continuity::Fork);
    }

    #[test]
    fn mixed_rotations_are_rejected() {
        let rotator = Keys::generate();
        let member = Keys::generate();
        let (a, _) = chunk_set(&[member.clone()], &rotator, RekeyScope::CommunityRoot);
        let (b, _) = chunk_set(&[member.clone()], &Keys::generate(), RekeyScope::CommunityRoot);
        let mixed = vec![a[0].clone(), b[0].clone()];
        assert!(find_my_key(&mixed, &member).is_err());
    }

    #[test]
    fn race_converges_to_lowest_key_and_heals_down_only() {
        let a = [0x02u8; 32];
        let b = [0x01u8; 32];
        assert_eq!(rotation_race_winner(&[a, b]), Some(b));
        // Holding the loser: re-converge down.
        assert!(should_reconverge(&a, &b));
        // Holding the winner: a higher sibling can never re-fork.
        assert!(!should_reconverge(&b, &a));
        assert!(!should_reconverge(&b, &b));
    }

    #[test]
    fn rotation_authority_is_verified_not_possessed() {
        let owner = Keys::generate();
        let salt = OwnerSalt([0x33; 32]);
        let cid = crate::concord::v2::derive::community_id(&owner.public_key().to_bytes(), &salt);
        let mut fold = ControlFold::new(cid, owner.public_key(), FoldMode::Tracking);

        let admin = Keys::generate();
        let removed_member = Keys::generate();
        let role = serde_json::to_string(&Role {
            role_id: [1; 32],
            name: "mod".into(),
            position: 1,
            permissions: perm::MANAGE_CHANNELS,
            scope: RoleScope::Server,
            color: 0,
        })
        .unwrap();
        let grant_eid = grant_locator(&cid, &admin.public_key().to_bytes());
        let grant = serde_json::to_string(&Grant { member: admin.public_key().to_bytes(), role_ids: vec![[1; 32]] }).unwrap();
        let role_ed = parse_edition(&build_edition_rumor(owner.public_key(), vsk::ROLE, &[1; 32], 1, None, &role, 1, None)).unwrap();
        let grant_ed = parse_edition(&build_edition_rumor(owner.public_key(), vsk::GRANT, &grant_eid, 1, None, &grant, 1, None)).unwrap();
        let vac = Citation { grant_eid, grant_version: 1, grant_hash: grant_ed.hash() };
        fold.ingest([role_ed, grant_ed]);

        let chan = RekeyScope::Channel(ChannelId([0x77; 32]));
        // The admin can rekey a channel against a plain member...
        assert!(may_rotate(&fold, &admin.public_key(), Some(&vac), chan, &[removed_member.public_key()]));
        // ...but not refound (no BAN bit)...
        assert!(!may_rotate(&fold, &admin.public_key(), Some(&vac), RekeyScope::CommunityRoot, &[removed_member.public_key()]));
        // ...never against the owner (rank)...
        assert!(!may_rotate(&fold, &admin.public_key(), Some(&vac), chan, &[owner.public_key()]));
        // ...and never without a resolving citation.
        assert!(!may_rotate(&fold, &admin.public_key(), None, chan, &[removed_member.public_key()]));
        // A removed member holding the prior root constructs a perfect
        // rotation — and every honest member drops it (roleless).
        let squatter = Keys::generate();
        assert!(!may_rotate(&fold, &squatter.public_key(), Some(&vac), chan, &[]));
        // The owner needs no citation, and BAN-refounds freely.
        assert!(may_rotate(&fold, &owner.public_key(), None, RekeyScope::CommunityRoot, &[removed_member.public_key()]));
    }

    #[test]
    fn build_parse_roundtrip_preserves_fields() {
        let rotator = Keys::generate();
        let member = Keys::generate();
        let vac = Citation { grant_eid: [1; 32], grant_version: 2, grant_hash: [3; 32] };
        let rumors = build_rotation(
            &rotator,
            RekeyScope::CommunityRoot,
            Epoch(4),
            &[0x11; 32],
            Epoch(5),
            &[0x22; 32],
            &[member.public_key()],
            Some(&vac),
            1_722_500_000_123,
        )
        .unwrap();
        let c = parse_rotation(&rumors[0]).unwrap();
        assert_eq!(c.rotator, rotator.public_key());
        assert_eq!(c.scope, RekeyScope::CommunityRoot);
        assert_eq!((c.prev_epoch, c.new_epoch), (Epoch(4), Epoch(5)));
        assert_eq!(c.prevcommit, epoch_key_commitment(Epoch(4), &[0x11; 32]));
        assert_eq!(c.chunk, (1, 1));
        assert_eq!(c.citation, Some(vac));
        assert_eq!(c.blobs.len(), 1);
    }
}
