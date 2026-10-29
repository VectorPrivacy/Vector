//! Persistence for Community protocol local state (GROUP_PROTOCOL.md).
//!
//! Stores the secrets this account *holds*: the server-root key and per-channel keys
//! (epoch-tagged). The DB is already account-scoped (`account_dir(npub)/vector.db`), so
//! there is no npub column — a row belongs to whichever account's DB it lives in.
//!
//! At-rest encryption: when Local Encryption is on, every secret BLOB and every identifying
//! metadata field (names, relays, roles, banlist, owner attestation, invite material) is wrapped
//! with the account's ENCRYPTION_KEY before it touches disk and unwrapped on read, via the
//! `enc_*`/`dec_*` helpers below. A raw DB then reveals no WHO/WHERE/WHAT. The discriminators
//! (32-byte raw key vs 60-byte ciphertext; `looks_encrypted` for text) let a half-migrated DB
//! read back correctly, so the toggle/PIN-rekey flows and the one-time backfill are safe to re-run.

use nostr_sdk::prelude::{Keys, PublicKey, SecretKey};
use nostr_sdk::prelude::ToBech32;
use rusqlite::{params, OptionalExtension};

use crate::community::{ChannelId, CommunityId, Epoch};

fn now_secs() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn to_32(bytes: &[u8]) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| format!("expected 32-byte key, got {} bytes", bytes.len()))
}

// At-rest wrappers (see module doc). `enc_*` for write binds, `dec_*` for read.
fn enc_key(k: &[u8; 32]) -> Result<Vec<u8>, String> { crate::crypto::maybe_encrypt_blob(k) }
fn dec_key(stored: &[u8]) -> Result<[u8; 32], String> { to_32(&crate::crypto::maybe_decrypt_blob(stored)) }
fn enc_txt(s: &str) -> Result<String, String> { crate::crypto::maybe_encrypt_text(s) }
fn dec_txt(s: &str) -> String { crate::crypto::maybe_decrypt_text(s) }
/// Encrypt an optional text field, preserving NULL.
fn enc_txt_opt(s: &Option<String>) -> Result<Option<String>, String> {
    s.as_deref().map(enc_txt).transpose()
}

/// Decode a 64-char hex id to 32 bytes, REJECTING malformed input. Unlike
/// `simd::hex::hex_to_bytes_32`, this never silently zero-fills or truncates — a
/// corrupted id row must error, not reconstruct a wrong-but-self-consistent id.
pub(crate) fn hex_id_to_32(hex: &str) -> Result<[u8; 32], String> {
    crate::simd::hex::hex_to_bytes_32_checked(hex)
        .ok_or_else(|| format!("corrupt or wrong-length 64-char hex id ({} chars)", hex.len()))
}

// ── Parked Private-Channel key vends (CORD-03 "delivered on grant") ──────────

/// One parked key vend awaiting the control fold that proves its grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChannelKey {
    /// Row id — the discharge handle, since several candidates may name one channel.
    pub id: i64,
    pub channel_id: String,
    pub epoch: u64,
    pub key: [u8; 32],
    /// Seal author — judged as an entitled vendor before the key is accepted.
    pub sender: String,
    pub received_at: i64,
}

/// Most candidate vends retained per channel, and per community overall.
///
/// Parking is reachable by ANY npub that can gift-wrap us, so these bound what a
/// stranger can make us store — and decrypt on every follow pass. Small, because
/// more than a couple of live candidates for one channel is already pathological.
const MAX_PARKED_PER_CHANNEL: usize = 4;
const MAX_PARKED_PER_COMMUNITY: usize = 64;

/// Park a vended channel key as a CANDIDATE.
///
/// Deliberately NOT a single slot keyed on (community, channel): parking is open
/// to any sender, so a slot lets a stranger pre-empt the entitled vendor's key
/// and silently suppress delivery. Every vend is its own row, the judge tries
/// them all, and the caps evict oldest-first so an unprovable flood cannot push
/// out a provable row indefinitely.
///
/// `sender` is encrypted at rest with a per-write nonce, so it can be neither a
/// key nor a dedupe column — hence oldest-first eviction rather than per-sender
/// replacement.
pub fn park_channel_key(
    community_id: &str,
    channel_id: &str,
    epoch: u64,
    key: &[u8; 32],
    sender: &str,
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("park channel key tx: {e}"))?;
    let enc = enc_key(key)?;
    let enc_sender = enc_txt(sender)?;
    tx.execute(
        "INSERT INTO pending_channel_keys
            (community_id, channel_id, epoch, channel_key, sender, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![community_id, channel_id, epoch as i64, &enc[..], enc_sender, now_secs()],
    )
    .map_err(|e| format!("park channel key: {e}"))?;
    // Trim this channel's candidates, then the community's total. `id` is the
    // arrival order (autoincrement), so this is oldest-first without trusting
    // any attacker-supplied field.
    tx.execute(
        "DELETE FROM pending_channel_keys WHERE id IN (
            SELECT id FROM pending_channel_keys
             WHERE community_id = ?1 AND channel_id = ?2
             ORDER BY id DESC LIMIT -1 OFFSET ?3)",
        params![community_id, channel_id, MAX_PARKED_PER_CHANNEL as i64],
    )
    .map_err(|e| format!("trim parked channel keys: {e}"))?;
    tx.execute(
        "DELETE FROM pending_channel_keys WHERE id IN (
            SELECT id FROM pending_channel_keys
             WHERE community_id = ?1
             ORDER BY id DESC LIMIT -1 OFFSET ?2)",
        params![community_id, MAX_PARKED_PER_COMMUNITY as i64],
    )
    .map_err(|e| format!("trim parked community keys: {e}"))?;
    tx.commit().map_err(|e| format!("park channel key commit: {e}"))?;
    Ok(())
}

/// Every parked vend for a community, for the post-fold re-judge.
pub fn get_pending_channel_keys(community_id: &str) -> Result<Vec<PendingChannelKey>, String> {
    let conn = super::get_db_connection_guard_static()?;
    // Newest first: the freshest candidate is the likeliest genuine vend, so a
    // pile of stale squatters costs at most a few failed judgements.
    let mut stmt = conn
        .prepare("SELECT id, channel_id, epoch, channel_key, sender, received_at FROM pending_channel_keys WHERE community_id = ?1 ORDER BY id DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (id, channel_id, epoch, blob, sender, received_at) = row.map_err(|e| e.to_string())?;
        // A key that won't decrypt is unusable — drop it rather than failing the
        // whole re-judge for its sake.
        let Ok(key) = dec_key(&blob) else { continue };
        out.push(PendingChannelKey { id, channel_id, epoch: epoch as u64, key, sender: dec_txt(&sender), received_at });
    }
    Ok(out)
}

/// Seat a channel key on a channel that currently holds NONE, without the
/// monotonic epoch guard.
///
/// [`advance_channel_epoch`] requires `new_epoch > current`, which is right for a
/// rotation but wrong for first delivery: a keyless record parks at epoch 0 (the
/// rekey-scan cursor) and a client that mints born-private channels at epoch 0
/// vends exactly that epoch, so the guard would silently refuse the only key we
/// will ever be offered. A keyless channel has no key to lose, so any authorized
/// delivery is strictly an improvement.
pub fn seat_channel_key(
    community_id: &str,
    channel_id: &str,
    epoch: u64,
    key: &[u8; 32],
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("seat channel key tx: {e}"))?;
    // "Keyless" is the caller's read of a snapshot; re-establish it HERE, inside the
    // transaction that writes. Seating over a real key is a downgrade to whatever a
    // vend offered, and only lock discipline stands between this and that today.
    // A keyless private channel stores the community_root as its placeholder.
    let placeholder: Vec<u8> = tx
        .query_row(
            "SELECT server_root_key FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("seat channel key root: {e}"))?;
    let current: Vec<u8> = tx
        .query_row(
            "SELECT channel_key FROM community_channels WHERE community_id = ?1 AND channel_id = ?2",
            params![community_id, channel_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("seat channel key read: {e}"))?;
    // Ciphertext carries a per-write nonce, so the comparison has to be on plaintext.
    if dec_key(&current)? != dec_key(&placeholder)? {
        return Err("channel already holds a key — seating would downgrade it".to_string());
    }
    store_epoch_key_tx(&tx, community_id, channel_id, epoch, key)?;
    let enc = enc_key(key)?;
    tx.execute(
        "UPDATE community_channels SET epoch = ?1, channel_key = ?2
           WHERE community_id = ?3 AND channel_id = ?4",
        params![epoch as i64, &enc[..], community_id, channel_id],
    )
    .map_err(|e| format!("seat channel key: {e}"))?;
    tx.commit().map_err(|e| format!("seat channel key commit: {e}"))?;
    crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    Ok(())
}

/// Discharge ONE candidate (refused, superseded, expired, or undecodable).
pub fn drop_pending_channel_key(id: i64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM pending_channel_keys WHERE id = ?1", params![id])
        .map_err(|e| format!("drop pending channel key: {e}"))?;
    Ok(())
}

/// Discharge every candidate for a channel — a key landed, so the rest are moot.
pub fn drop_pending_channel_keys_for(community_id: &str, channel_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM pending_channel_keys WHERE community_id = ?1 AND channel_id = ?2",
        params![community_id, channel_id],
    )
    .map_err(|e| format!("drop pending channel keys: {e}"))?;
    Ok(())
}

/// Store one held epoch key in the multi-held archive. `scope_id` is a channel_id hex or
/// [`crate::community::SERVER_ROOT_SCOPE_HEX`]. The `(community, scope, epoch)` PK makes a write for
/// one epoch unable to disturb another epoch's key — so retained history survives a rekey. Uses
/// REPLACE on the exact coordinate so the fork-resolution apply path can commit the *winning*
/// key for a contested epoch over a previously-stored loser (the only legitimate same-coordinate
/// overwrite; an epoch key is otherwise immutable).
pub fn store_epoch_key(community_id: &str, scope_id: &str, epoch: u64, key: &[u8; 32]) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    store_epoch_key_tx(&conn, community_id, scope_id, epoch, key)
}

/// Forget one held epoch key, as a device that never held it.
#[cfg(test)]
pub fn delete_epoch_key(community_id: &str, scope_id: &str, epoch: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM community_epoch_keys WHERE community_id = ?1 AND scope_id = ?2 AND epoch = ?3",
        params![community_id, scope_id, epoch as i64],
    )
    .map_err(|e| format!("delete epoch key: {e}"))?;
    Ok(())
}

/// Shared INSERT body so `save_community` can archive keys inside its own transaction and the
/// standalone [`store_epoch_key`] can run on a borrowed connection. `C: Deref<Target=Connection>`
/// covers both a `Connection` and a `Transaction`.
fn store_epoch_key_tx<C: std::ops::Deref<Target = rusqlite::Connection>>(
    conn: &C,
    community_id: &str,
    scope_id: &str,
    epoch: u64,
    key: &[u8; 32],
) -> Result<(), String> {
    let enc = enc_key(key)?;
    conn.execute(
        "INSERT OR REPLACE INTO community_epoch_keys
            (community_id, scope_id, epoch, key, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        // epoch reinterpreted u64->i64 (lossless); never ORDER BY / range-filter it in SQL (see save).
        params![community_id, scope_id, epoch as i64, &enc[..], now_secs()],
    )
    .map_err(|e| format!("store epoch key: {e}"))?;
    Ok(())
}

/// Apply a received channel rekey's new key — the atomic archive+head dual-write: in ONE transaction,
/// ARCHIVE `(channel, new_epoch) -> new_key` in `community_epoch_keys` AND advance the channel's
/// read-head (`community_channels.epoch` + `channel_key`) iff `new_epoch` exceeds the current head.
/// A caught-up OLDER epoch is archived (its history stays decryptable) but never regresses the head.
/// Atomic so a crash can't leave the archive ahead of the head or the reverse. Returns whether the
/// head advanced. Epoch comparison is done in RUST (the u64-as-i64 ≥2^63 SQL mis-order trap).
pub fn advance_channel_epoch(
    community_id: &str,
    channel_id: &str,
    new_epoch: u64,
    new_key: &[u8; 32],
) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("advance channel epoch tx: {e}"))?;
    // Archive always (PK includes epoch → never clobbers another epoch's key).
    store_epoch_key_tx(&tx, community_id, channel_id, new_epoch, new_key)?;
    // Monotonic head advance, compared in Rust.
    let cur: Option<i64> = tx
        .query_row(
            "SELECT epoch FROM community_channels WHERE community_id = ?1 AND channel_id = ?2",
            params![community_id, channel_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("read channel head: {e}"))?;
    let advanced = matches!(cur, Some(c) if new_epoch > c as u64);
    if advanced {
        let enc = enc_key(new_key)?;
        tx.execute(
            "UPDATE community_channels SET epoch = ?1, channel_key = ?2
               WHERE community_id = ?3 AND channel_id = ?4",
            params![new_epoch as i64, &enc[..], community_id, channel_id],
        )
        .map_err(|e| format!("advance channel head: {e}"))?;
    }
    tx.commit().map_err(|e| format!("advance channel epoch commit: {e}"))?;
    if advanced {
        crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    }
    Ok(advanced)
}

/// Apply a received SERVER-ROOT (base) rekey's new root — the base counterpart to
/// [`advance_channel_epoch`], atomic: in ONE transaction, ARCHIVE `(server-root scope, new_epoch) ->
/// new_root` in `community_epoch_keys` AND advance the base head (`communities.server_root_epoch` +
/// `server_root_key`) iff `new_epoch` exceeds the current base epoch (monotonic, compared in RUST). A
/// caught-up OLDER base epoch is archived (its control/base history stays decryptable) but never
/// regresses the head. Returns whether the head advanced.
/// The community row's CURRENT base epoch — the cheap freshness probe a
/// root-derived write compares its in-hand struct against.
pub fn get_server_root_epoch(community_id: &str) -> Result<Option<u64>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT server_root_epoch FROM communities WHERE community_id = ?1",
        params![community_id],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|v| v.map(|e| e as u64))
    .map_err(|e| format!("get server root epoch: {e}"))
}

pub fn advance_server_root_epoch(community_id: &str, new_epoch: u64, new_root: &[u8; 32]) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("advance server root tx: {e}"))?;
    // Archive always, under the all-zero server-root scope sentinel (PK includes epoch → never clobbers
    // another epoch's root).
    store_epoch_key_tx(&tx, community_id, crate::community::SERVER_ROOT_SCOPE_HEX, new_epoch, new_root)?;
    let cur: Option<i64> = tx
        .query_row(
            "SELECT server_root_epoch FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("read server-root epoch: {e}"))?;
    let advanced = matches!(cur, Some(c) if new_epoch > c as u64);
    if advanced {
        let enc = enc_key(new_root)?;
        tx.execute(
            "UPDATE communities SET server_root_epoch = ?1, server_root_key = ?2 WHERE community_id = ?3",
            params![new_epoch as i64, &enc[..], community_id],
        )
        .map_err(|e| format!("advance server-root head: {e}"))?;
    }
    tx.commit().map_err(|e| format!("advance server root commit: {e}"))?;
    if advanced {
        crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    }
    Ok(advanced)
}

/// SAME-EPOCH convergence for the server root (concurrent re-founding heal): two BAN-holders who
/// re-founded at the same time each sit on their OWN root at the SAME epoch. `advance_server_root_epoch`
/// refuses to switch (its guard is strictly monotonic), so this is the sibling that REPLACES the head root
/// at `epoch` with the deterministic winner (lowest root bytes — the caller decides). Archives the new root +
/// swaps the head, but ONLY while we're still AT `epoch` (a later real rotation must win over a stale
/// converge). Returns whether it switched.
pub fn converge_server_root_epoch(community_id: &str, epoch: u64, new_root: &[u8; 32]) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("converge server root tx: {e}"))?;
    store_epoch_key_tx(&tx, community_id, crate::community::SERVER_ROOT_SCOPE_HEX, epoch, new_root)?;
    let enc = enc_key(new_root)?;
    let switched = tx
        .execute(
            "UPDATE communities SET server_root_key = ?1 WHERE community_id = ?2 AND server_root_epoch = ?3",
            params![&enc[..], community_id, epoch as i64],
        )
        .map_err(|e| format!("converge server-root head: {e}"))?
        > 0;
    tx.commit().map_err(|e| format!("converge server root commit: {e}"))?;
    if switched {
        crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    }
    Ok(switched)
}

/// Every held `(epoch, key)` for a scope, ascending by epoch. The read paths derive a pseudonym per
/// returned epoch (`#z` OR-set) so cross-epoch history isn't stranded. Sorted in Rust (not SQL):
/// epoch is a u64 stored as i64, so a SQL `ORDER BY` would mis-order epochs >= 2^63.
pub fn held_epoch_keys(community_id: &str, scope_id: &str) -> Result<Vec<(Epoch, [u8; 32])>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT epoch, key FROM community_epoch_keys WHERE community_id = ?1 AND scope_id = ?2")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id, scope_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut out: Vec<(Epoch, [u8; 32])> = Vec::new();
    for row in rows {
        let (epoch, key_blob) = row.map_err(|e| e.to_string())?;
        out.push((Epoch(epoch as u64), dec_key(&key_blob)?));
    }
    out.sort_by_key(|(e, _)| e.0);
    Ok(out)
}

/// Archive a past epoch's read view of a private channel (CORD-03 §5): its conversation key and
/// address, under scopes of their own so neither is ever taken for a channel key.
pub fn store_history_view(community_id: &str, channel_id: &str, epoch: u64, view: &crate::community::v2::rekey::HistoryView) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("store history view tx: {e}"))?;
    store_epoch_key_tx(&tx, community_id, &format!("hist:{channel_id}"), epoch, &view.conv_key)?;
    store_epoch_key_tx(&tx, community_id, &format!("hista:{channel_id}"), epoch, &view.address)?;
    tx.commit().map_err(|e| format!("store history view commit: {e}"))
}

/// Every past epoch of a private channel held read-only, oldest first.
pub fn history_views(community_id: &str, channel_id: &str) -> Result<Vec<(Epoch, crate::community::v2::rekey::HistoryView)>, String> {
    let addresses: std::collections::HashMap<u64, [u8; 32]> =
        held_epoch_keys(community_id, &format!("hista:{channel_id}"))?.into_iter().map(|(e, a)| (e.0, a)).collect();
    Ok(held_epoch_keys(community_id, &format!("hist:{channel_id}"))?
        .into_iter()
        .filter_map(|(e, conv_key)| addresses.get(&e.0).map(|address| (e, crate::community::v2::rekey::HistoryView { address: *address, conv_key })))
        .collect())
}

/// The held key for one specific `(scope, epoch)`, or `None` if not held. The open path uses this to
/// select the decryption key by the inbound event's `epoch` tag.
pub fn held_epoch_key(community_id: &str, scope_id: &str, epoch: u64) -> Result<Option<[u8; 32]>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let blob: Option<Vec<u8>> = conn
        .query_row(
            "SELECT key FROM community_epoch_keys WHERE community_id = ?1 AND scope_id = ?2 AND epoch = ?3",
            params![community_id, scope_id, epoch as i64],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("held epoch key: {e}"))?;
    blob.map(|b| dec_key(&b)).transpose()
}

/// Retain the ephemeral signing key of a message I published, so I can later
/// NIP-09-delete it. `relays` is where the deletion must be sent.
pub fn store_message_key(
    message_id: &str,
    outer_event_id: &str,
    ephemeral: &Keys,
    relays: &[String],
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let relays_json = serde_json::to_string(relays).map_err(|e| e.to_string())?;
    let sk_bytes = to_32(ephemeral.secret_key().as_secret_bytes())?;
    let enc_secret = enc_key(&sk_bytes)?;
    let enc_relays = enc_txt(&relays_json)?;
    conn.execute(
        "INSERT OR REPLACE INTO community_message_keys
            (outer_event_id, message_id, ephemeral_secret, relays, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            outer_event_id,
            message_id,
            &enc_secret[..],
            enc_relays,
            now_secs(),
        ],
    )
    .map_err(|e| format!("store message key: {e}"))?;
    Ok(())
}

/// Read (WITHOUT removing) the retained key for a message by its INNER message id (what
/// the UI holds). Returns the ephemeral signing `Keys`, the OUTER event id to
/// NIP-09-delete, and the relay set — or `None` if not retained (someone else's message,
/// or already deleted). Peek-only so the key survives a failed deletion publish; the
/// caller removes it with [`delete_message_key`] only after the publish succeeds.
pub fn get_message_key(message_id: &str) -> Result<Option<(Keys, String, Vec<String>)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let row = conn
        .query_row(
            "SELECT ephemeral_secret, outer_event_id, relays
               FROM community_message_keys WHERE message_id = ?1",
            params![message_id],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()
        .map_err(|e| format!("get message key: {e}"))?;
    let (secret_blob, outer_event_id, relays_json) = match row {
        Some(t) => t,
        None => return Ok(None),
    };
    let secret = SecretKey::from_slice(&dec_key(&secret_blob)?).map_err(|e| format!("ephemeral secret: {e}"))?;
    let relays: Vec<String> = serde_json::from_str(&dec_txt(&relays_json)).map_err(|e| e.to_string())?;
    Ok(Some((Keys::new(secret), outer_event_id, relays)))
}

/// Whether the plane key of a wrap of `message_id` is kept.
pub fn has_message_key(message_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM community_message_keys WHERE message_id = ?1)")
        .and_then(|mut stmt| stmt.query_row(params![message_id], |r| r.get(0)))
        .map_err(|e| format!("has message key: {e}"))
}

/// Remove a retained message key (after a successful deletion publish).
pub fn delete_message_key(message_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM community_message_keys WHERE message_id = ?1",
        params![message_id],
    )
    .map_err(|e| format!("remove message key: {e}"))?;
    Ok(())
}

/// Peek + remove in one call. Prefer [`get_message_key`] + [`delete_message_key`] when a
/// fallible step sits between, so a failure doesn't strand the key.
pub fn take_message_key(message_id: &str) -> Result<Option<(Keys, String, Vec<String>)>, String> {
    let r = get_message_key(message_id)?;
    if r.is_some() {
        delete_message_key(message_id)?;
    }
    Ok(r)
}

/// Keep the `p` key of a wrap I published or met: its holder may delete the wrap (CORD-01
/// §Deletions) on every relay that refuses the plane key's deletions.
pub fn store_wrap_owner(message_id: &str, outer_event_id: &str, owner: &Keys, relays: &[String]) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let relays_json = serde_json::to_string(relays).map_err(|e| e.to_string())?;
    let enc_secret = enc_key(&to_32(owner.secret_key().as_secret_bytes())?)?;
    let enc_relays = enc_txt(&relays_json)?;
    conn.execute(
        "INSERT OR REPLACE INTO community_wrap_owners (outer_event_id, message_id, owner_secret, relays, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![outer_event_id, message_id, &enc_secret[..], enc_relays, now_secs()],
    )
    .map_err(|e| format!("store wrap owner: {e}"))?;
    Ok(())
}

/// Every wrap of `message_id` whose `p` key is kept: the key, the wrap's id, and the relays
/// it was sent to.
pub fn wrap_owners(message_id: &str) -> Result<Vec<(Keys, String, Vec<String>)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare_cached("SELECT owner_secret, outer_event_id, relays FROM community_wrap_owners WHERE message_id = ?1")
        .map_err(|e| format!("wrap owners: {e}"))?;
    let rows = stmt
        .query_map(params![message_id], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| format!("wrap owners: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let (secret_blob, outer_event_id, relays_json) = row.map_err(|e| format!("wrap owners: {e}"))?;
        let secret = SecretKey::from_slice(&dec_key(&secret_blob)?).map_err(|e| format!("wrap owner secret: {e}"))?;
        let relays: Vec<String> = serde_json::from_str(&dec_txt(&relays_json)).map_err(|e| e.to_string())?;
        out.push((Keys::new(secret), outer_event_id, relays));
    }
    Ok(out)
}

/// Whether a wrap of `message_id` can be deleted by its owner from here.
pub fn has_wrap_owner(message_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.prepare_cached("SELECT EXISTS(SELECT 1 FROM community_wrap_owners WHERE message_id = ?1)")
        .and_then(|mut stmt| stmt.query_row(params![message_id], |r| r.get(0)))
        .map_err(|e| format!("has wrap owner: {e}"))
}

/// Drop the kept `p` keys of `message_id`'s wraps, once their deletions went out.
pub fn forget_wrap_owners(message_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM community_wrap_owners WHERE message_id = ?1", params![message_id])
        .map_err(|e| format!("forget wrap owners: {e}"))?;
    Ok(())
}

/// Queue my own `message_ids` of `community_id` to leave the relays, a wrap at a time.
pub fn queue_wrap_scrubs(community_id: &str, message_ids: &[String]) -> Result<(), String> {
    let mut conn = super::get_write_connection_guard_static()?;
    let tx = conn.transaction().map_err(|e| format!("queue wrap scrubs: {e}"))?;
    for id in message_ids {
        tx.execute(
            "INSERT OR IGNORE INTO community_wrap_scrubs (message_id, community_id, queued_at) VALUES (?1, ?2, ?3)",
            params![id, community_id, now_secs()],
        )
        .map_err(|e| format!("queue wrap scrubs: {e}"))?;
    }
    tx.commit().map_err(|e| format!("queue wrap scrubs: {e}"))
}

/// The longest-waiting queued scrub: `(message_id, community_id)`.
pub fn next_wrap_scrub() -> Result<Option<(String, String)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT message_id, community_id FROM community_wrap_scrubs ORDER BY queued_at, rowid LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(|e| format!("next wrap scrub: {e}"))
}

/// Settle a queued scrub: `retry` sends it to the back of the queue, otherwise it is done.
pub fn settle_wrap_scrub(message_id: &str, retry: bool) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    let sql = if retry {
        "UPDATE community_wrap_scrubs SET queued_at = ?2 WHERE message_id = ?1"
    } else {
        "DELETE FROM community_wrap_scrubs WHERE message_id = ?1"
    };
    let n = if retry { conn.execute(sql, params![message_id, now_secs()]) } else { conn.execute(sql, params![message_id]) };
    n.map(|_| ()).map_err(|e| format!("settle wrap scrub: {e}"))
}

/// Drop what this device holds of a deleted Channel's Chat Plane: its chat, events and their
/// attachment rows, and wraps held for a later judgment. Its keys stay (CORD-03 §2).
/// Returns whether it held anything.
pub fn forget_channel_chat(community_id: &str, channel_id: &str) -> Result<bool, String> {
    let held = {
        let conn = super::get_write_connection_guard_static()?;
        let files = conn
            .execute(
                "DELETE FROM attachments WHERE event_id IN (SELECT e.id FROM events e JOIN chats c ON c.id = e.chat_id WHERE c.chat_identifier = ?1)",
                params![channel_id],
            )
            .map_err(|e| format!("forget channel attachments: {e}"))?;
        let wraps = conn
            .execute("DELETE FROM community_held_wraps WHERE community_id = ?1 AND channel_id = ?2", params![community_id, channel_id])
            .map_err(|e| format!("forget channel held wraps: {e}"))?;
        let chat: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM chats WHERE chat_identifier = ?1)", params![channel_id], |r| r.get(0))
            .map_err(|e| format!("forget channel chat: {e}"))?;
        files + wraps > 0 || chat
    };
    if held {
        super::chats::delete_chat(channel_id)?;
    }
    Ok(held)
}

/// Every event this device stores in `channel_id`'s chat, and whether it carries files.
pub fn channel_events(channel_id: &str) -> Result<Vec<(String, bool)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT e.id, EXISTS(SELECT 1 FROM attachments a WHERE a.event_id = e.id)
               FROM events e JOIN chats c ON c.id = e.chat_id WHERE c.chat_identifier = ?1",
        )
        .map_err(|e| format!("channel events: {e}"))?;
    let rows = stmt
        .query_map(params![channel_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))
        .map_err(|e| format!("channel events: {e}"))?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("channel events: {e}"))
}

/// The hex id of the Community that owns `channel_id`, if any is stored locally. Used to
/// resolve a channel-addressed chat back to its Community for sending.
// ── Channel → community cache ────────────────────────────────────────────────
// The owning community of a channel is IMMUTABLE (a channel belongs to one community
// for life; channel ids are random-32 and never reused — the save path refuses a
// cross-community id claim), so a live entry is never wrong. It only needs eviction
// when the channel row itself goes away: `delete_community_inner` (all rows) or a
// `save_community_v2` prune (some rows); `save_community` is UPSERT-only and never
// removes a channel, so it needs none. POSITIVES ONLY — a missing channel is never
// cached, so a not-yet-synced channel resolves the moment its row lands and `None`
// keeps meaning "gone" for delete-then-check callers. Per session: what is held is
// the account's own. Keyed by bytes; an id not in canonical hex skips the cache.
struct ChannelCommunityCache;
type ChannelCommunityMap = std::sync::RwLock<std::collections::HashMap<[u8; 32], [u8; 32]>>;

fn channel_community_cache() -> std::sync::Arc<ChannelCommunityMap> {
    crate::db::current_session().scoped::<ChannelCommunityCache, ChannelCommunityMap>()
}

/// Drop every cached channel→community mapping.
pub fn clear_channel_community_cache() {
    *channel_community_cache().write().unwrap_or_else(|e| e.into_inner()) = Default::default();
    crate::notify::invalidate_communities();
}

/// Forget a community's channel mappings — its rows were dropped or rewritten, so any
/// pruned entry must stop resolving. Retained channels refill lazily on next lookup.
fn forget_community_channels(community_id: &str) {
    if let Some(cid) = crate::community::id32_from_hex(community_id) {
        channel_community_cache().write().unwrap_or_else(|e| e.into_inner()).retain(|_, c| *c != cid);
    }
    // A prune here is how a channel stops being openable, which is what decides
    // whether its unread may still badge.
    crate::notify::invalidate_communities();
}

fn query_community_for_channel(channel_id: &str) -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.prepare_cached("SELECT community_id FROM community_channels WHERE channel_id = ?1")
        .and_then(|mut stmt| stmt.query_row(params![channel_id], |r| r.get::<_, String>(0)))
        .optional()
        .map_err(|e| format!("community_id_for_channel: {e}"))
}

/// Whether a stored Channel is private, `None` when it isn't stored.
pub fn channel_is_private(channel_id: &str) -> Result<Option<bool>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.prepare_cached("SELECT private FROM community_channels WHERE channel_id = ?1")
        .and_then(|mut stmt| stmt.query_row(params![channel_id], |r| r.get::<_, i64>(0)))
        .optional()
        .map(|p| p.map(|p| p != 0))
        .map_err(|e| format!("channel_is_private: {e}"))
}

/// The id of the Community that owns `channel`, if one is stored locally.
pub fn community_for_channel(channel: &[u8; 32]) -> Result<Option<[u8; 32]>, String> {
    let cache = channel_community_cache();
    if let Some(cid) = cache.read().unwrap_or_else(|e| e.into_inner()).get(channel) {
        return Ok(Some(*cid));
    }
    let Some(cid_hex) = query_community_for_channel(&crate::simd::hex::bytes_to_hex_32(channel))? else {
        return Ok(None);
    };
    let Some(cid) = crate::community::id32_from_hex(&cid_hex) else {
        return Ok(None);
    };
    cache.write().unwrap_or_else(|e| e.into_inner()).insert(*channel, cid);
    Ok(Some(cid))
}

/// [`community_for_channel`] over hex, returning the stored id verbatim.
pub fn community_id_for_channel(channel_id: &str) -> Result<Option<String>, String> {
    let channel = crate::community::id32_from_hex(channel_id);
    if let Some(ch) = channel {
        if let Some(cid) = channel_community_cache().read().unwrap_or_else(|e| e.into_inner()).get(&ch) {
            return Ok(Some(crate::simd::hex::bytes_to_hex_32(cid)));
        }
    }
    let cid = query_community_for_channel(channel_id)?;
    if let (Some(ch), Some(c)) = (channel, cid.as_deref().and_then(crate::community::id32_from_hex)) {
        channel_community_cache().write().unwrap_or_else(|e| e.into_inner()).insert(ch, c);
    }
    Ok(cid)
}

/// Whether a Community with this id is already stored locally (joined). Cheaper than
/// `load_community` when only existence matters (e.g. inbound-invite dedup).
pub fn community_exists(id: &CommunityId) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM communities WHERE community_id = ?1",
            params![id.to_hex()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("community_exists: {e}"))?;
    Ok(found.is_some())
}

/// A parked invite awaiting the user's accept/decline decision.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingCommunityInvite {
    pub community_id: String,
    pub bundle_json: String,
    pub inviter_npub: String,
    pub received_at: i64,
    /// Sender-declared NIP-40 expiry (unix secs); 0 = none declared, so permanent.
    pub expires_at: i64,
}

/// Park an inbound invite bundle for explicit user consent (the carrier never
/// auto-joins). First-invite-wins: `INSERT OR IGNORE` means a later invite for the
/// same `community_id` can't silently rewrite a parked bundle. Returns whether a new
/// row was inserted (`false` = already pending, caller should not re-notify).
pub fn save_pending_invite(
    community_id: &str,
    bundle_json: &str,
    inviter_npub: &str,
    expires_at: i64,
) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    /// Cap on parked invites. Each row is one gift-wrapped invite from an arbitrary sender, so an
    /// attacker fabricating unbounded community_ids could otherwise grow this table without limit
    /// (#298). Newest-wins: a stale months-old park is the safe thing to shed.
    const MAX_PENDING_INVITES: usize = 100;

    let conn = super::get_write_connection_guard_static()?;
    let enc_bundle = enc_txt(bundle_json)?;
    let enc_inviter = enc_txt(inviter_npub)?;
    // First-wins: a parked invite is never silently overwritten by a later different
    // bundle (that would let an attacker replace a genuine parked invite). For v2, a
    // pre-planted forged-root bundle sharing a real community_id is instead cleared on
    // a failed accept (see `accept_pending_invite`), so a genuine re-invite can re-park.
    let changed = conn
        .execute(
            "INSERT OR IGNORE INTO pending_community_invites
                (community_id, bundle_json, inviter_npub, received_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![community_id, enc_bundle, enc_inviter, now_secs(), expires_at],
        )
        .map_err(|e| format!("save pending invite: {e}"))?;
    // Only growth can breach the cap. Evict everything past the newest MAX rows
    // (LIMIT -1 OFFSET cap = "all rows after the first cap"); community_id tie-breaks equal times.
    if changed > 0 {
        let _ = conn.execute(
            "DELETE FROM pending_community_invites
               WHERE community_id IN (
                 SELECT community_id FROM pending_community_invites
                 ORDER BY received_at DESC, community_id DESC
                 LIMIT -1 OFFSET ?1
               )",
            params![MAX_PENDING_INVITES],
        );
    }
    Ok(changed > 0)
}

/// Drop every parked invite for a community we ALREADY hold — once joined on any device, the
/// invite must never resurface. Ordering-independent: covers the cross-device case where the
/// historical gift-wrapped invites are ingested BEFORE the synced membership list rehydrates
/// those communities (so the ingest-time `community_exists` guard saw nothing yet). Returns the
/// count purged.
pub fn purge_pending_invites_for_held_communities() -> Result<usize, String> {
    let conn = super::get_write_connection_guard_static()?;
    let n = conn
        .execute(
            "DELETE FROM pending_community_invites
               WHERE community_id IN (SELECT community_id FROM communities)",
            [],
        )
        .map_err(|e| format!("purge held pending invites: {e}"))?;
    Ok(n)
}

/// Drop parked invites past their sender-declared NIP-40 expiry OR past the
/// recipient-enforced 24h lifetime (measured from park time — conservative,
/// since parking postdates sending). The lifetime leg is what clears rows
/// parked by builds that predate the ingest-time rule: a machine dormant for
/// a month otherwise boots into a page of fossil invites no other culler can
/// touch (no declared expiry, never held, never tombstoned). Returns the
/// count purged.
pub fn purge_expired_pending_invites() -> Result<usize, String> {
    let conn = super::get_write_connection_guard_static()?;
    let now = now_secs();
    let n = conn
        .execute(
            "DELETE FROM pending_community_invites
              WHERE (expires_at != 0 AND expires_at <= ?1)
                 OR received_at <= ?2",
            params![now, now - crate::event_handler::DIRECT_INVITE_LIFETIME_SECS as i64],
        )
        .map_err(|e| format!("purge expired pending invites: {e}"))?;
    Ok(n)
}

/// All parked invites, newest first.
pub fn list_pending_invites() -> Result<Vec<PendingCommunityInvite>, String> {
    let now = now_secs();
    let raw: Vec<(String, String, String, i64, i64)> = {
        let conn = super::get_db_connection_guard_static()?;
        let mut stmt = conn
            .prepare(
                "SELECT community_id, bundle_json, inviter_npub, received_at, expires_at
                   FROM pending_community_invites
                  WHERE (expires_at = 0 OR expires_at > ?1)
                    AND received_at > ?2
                  ORDER BY received_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![now, now - crate::event_handler::DIRECT_INVITE_LIFETIME_SECS as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    Ok(raw
        .into_iter()
        .map(|(community_id, bundle, inviter, received_at, expires_at)| PendingCommunityInvite {
            community_id,
            bundle_json: dec_txt(&bundle),
            inviter_npub: dec_txt(&inviter),
            received_at,
            expires_at,
        })
        .collect())
}

/// Read a parked invite's bundle WITHOUT removing it. Accept is fallible (caps,
/// owner/authority collision), so the row must survive a rejected accept — peek here,
/// then [`delete_pending_invite`] only after the join succeeds.
pub fn get_pending_invite(community_id: &str) -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT bundle_json FROM pending_community_invites
              WHERE community_id = ?1 AND (expires_at = 0 OR expires_at > ?2)",
            params![community_id, now_secs()],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| format!("get pending invite: {e}"))?;
    Ok(raw.map(|s| dec_txt(&s)))
}

/// Drop a parked invite without joining (the user declined).
pub fn delete_pending_invite(community_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM pending_community_invites WHERE community_id = ?1",
        params![community_id],
    )
    .map_err(|e| format!("delete pending invite: {e}"))?;
    Ok(())
}

/// Drop every invite this person parked (they were blocked); the count dropped. The
/// inviter is sealed at rest, so the rows are read to match it.
pub fn delete_pending_invites_from(inviter_npub: &str) -> Result<usize, String> {
    let mut dropped = 0;
    for invite in list_pending_invites()? {
        if invite.inviter_npub == inviter_npub {
            delete_pending_invite(&invite.community_id)?;
            dropped += 1;
        }
    }
    Ok(dropped)
}

/// Whether an invite for this id is already parked (inbound dedup).
/// When a parked invite ARRIVED (unix secs), or `None` if none is parked. The
/// supersession key for the purge: an invite that arrived AFTER a removal is a genuine
/// re-invite, not residue of the leave.
pub fn pending_invite_received_at(community_id: &str) -> Result<Option<i64>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT received_at FROM pending_community_invites WHERE community_id = ?1",
        params![community_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| format!("pending_invite_received_at: {e}"))
}

pub fn pending_invite_exists(community_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pending_community_invites WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("pending_invite_exists: {e}"))?;
    Ok(found.is_some())
}

/// A minted public-invite link the owner retains (to list + revoke).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicInviteRecord {
    /// Hex token (the link's whole secret; lives only in the local account DB).
    pub token: String,
    pub community_id: String,
    pub url: String,
    pub expires_at: Option<i64>,
    pub created_at: i64,
    /// Optional human label set at mint time (e.g. "Twitter", "Discord"). None if unset.
    pub label: Option<String>,
    /// Distinct members who joined via this link (by label attribution). 0 if none/unknown.
    #[serde(default)]
    pub join_count: u64,
}

/// Retain a minted public-invite token so the owner can later list + revoke it.
pub fn save_public_invite(
    token: &str,
    community_id: &str,
    url: &str,
    expires_at: Option<i64>,
    label: Option<&str>,
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    // token + url are the link's secret; encrypted, the token PK becomes per-write-unique (random
    // nonce) so this is effectively an INSERT — fine, mints generate a fresh token each time.
    let enc_token = enc_txt(token)?;
    let enc_url = enc_txt(url)?;
    // Encrypt the label at rest like the url; NULL when no label was set.
    let enc_label = label.map(enc_txt).transpose()?;
    conn.execute(
        "INSERT OR REPLACE INTO community_public_invites
            (token, community_id, url, expires_at, created_at, label)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![enc_token, community_id, enc_url, expires_at, now_secs(), enc_label],
    )
    .map_err(|e| format!("save public invite: {e}"))?;
    Ok(())
}

/// All minted public-invite links for a Community, newest first.
pub fn list_public_invites(community_id: &str) -> Result<Vec<PublicInviteRecord>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT token, community_id, url, expires_at, created_at, label
               FROM community_public_invites WHERE community_id = ?1 ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| {
            Ok(PublicInviteRecord {
                token: dec_txt(&r.get::<_, String>(0)?),
                community_id: r.get(1)?,
                url: dec_txt(&r.get::<_, String>(2)?),
                expires_at: r.get(3)?,
                created_at: r.get(4)?,
                label: r.get::<_, Option<String>>(5)?.map(|s| dec_txt(&s)),
                join_count: 0,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    // Fill per-link join counts (distinct joiners via each label, attributed to me).
    if let Some(me) = crate::state::my_public_key() {
        if let Ok(counts) = community_invite_join_counts(community_id, &me) {
            // A v2 link minted without a label is joined as "": that bucket is only
            // its own while it is the one unlabeled link.
            let unlabeled = out.iter().filter(|r| r.label.as_deref().is_none_or(str::is_empty)).count();
            for rec in &mut out {
                match rec.label.as_deref() {
                    Some(l) if !l.is_empty() => rec.join_count = counts.get(l).copied().unwrap_or(0),
                    _ if unlabeled == 1 => rec.join_count = counts.get("").copied().unwrap_or(0),
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// Forget a minted public-invite token (after revoking it on relays).
pub fn delete_public_invite(token: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // Stored tokens are encrypted (random nonce), so an equality DELETE can't match — scan,
    // decrypt, and delete the row whose plaintext token matches (by rowid). Few rows, owner-only.
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT rowid, token FROM community_public_invites")
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        mapped.filter_map(|r| r.ok()).collect()
    };
    for (rowid, stored) in rows {
        if dec_txt(&stored) == token {
            conn.execute("DELETE FROM community_public_invites WHERE rowid = ?1", params![rowid])
                .map_err(|e| format!("delete public invite: {e}"))?;
        }
    }
    Ok(())
}

/// All minted public-invite links across ALL communities (backfill source for the synced Invite List).
pub fn list_all_public_invites() -> Result<Vec<PublicInviteRecord>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT token, community_id, url, expires_at, created_at, label
               FROM community_public_invites ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PublicInviteRecord {
                token: dec_txt(&r.get::<_, String>(0)?),
                community_id: r.get(1)?,
                url: dec_txt(&r.get::<_, String>(2)?),
                expires_at: r.get(3)?,
                created_at: r.get(4)?,
                label: r.get::<_, Option<String>>(5)?.map(|s| dec_txt(&s)),
                join_count: 0,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Remove a Community and all its local state (channels, retained message keys, parked
/// invites, minted public-invite tokens). Used when the user leaves a Community — there
/// is no protocol "leave" (membership is key possession), so leaving is purely local:
/// drop the keys + stop subscribing.
pub fn delete_community(community_id: &str) -> Result<(), String> {
    let out = delete_community_inner(community_id, false);
    crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    out
}

/// self-removal teardown: drop all local community state EXCEPT the held epoch keys
/// (`community_epoch_keys`). Read access to future epochs is already gone (the post-removal
/// keys are never delivered); retaining the OLD keys only preserves the ability to author a
/// `3305` self-delete of one's own past messages, each sealed under the epoch key it was sent
/// at. Used by every self-removal trigger (voluntary leave, kick of me, ban-rekey exclusion).
pub fn delete_community_retain_keys(community_id: &str) -> Result<(), String> {
    let out = delete_community_inner(community_id, true);
    crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    out
}

fn delete_community_inner(community_id: &str, retain_keys: bool) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // Atomic: a crash/error mid-delete must not orphan channel/invite rows under a
    // now-missing parent (community_id_for_channel would still resolve them).
    let tx = conn.unchecked_transaction().map_err(|e| format!("delete community tx: {e}"))?;
    for sql in [
        Some("DELETE FROM communities WHERE community_id = ?1"),
        Some("DELETE FROM community_channels WHERE community_id = ?1"),
        // Multi-held epoch keys (base + per-channel, all epochs). RETAINED on a self-removal so a
        // later self-scrub of own past messages stays possible; dropped on an explicit delete/re-join reset
        // (else a re-join inherits stale rotated keys).
        (!retain_keys).then_some("DELETE FROM community_epoch_keys WHERE community_id = ?1"),
        // Writer and certifier secrets let their holder post and vouch; a self-scrub needs neither.
        Some("DELETE FROM community_epoch_keys WHERE community_id = ?1 AND (scope_id LIKE 'wr:%' OR scope_id LIKE 'wrmint:%' OR scope_id LIKE 'wrcert:%')"),
        Some("DELETE FROM community_public_invites WHERE community_id = ?1"),
        Some("DELETE FROM community_invite_link_sets WHERE community_id = ?1"),
        Some("DELETE FROM pending_community_invites WHERE community_id = ?1"),
        // Parked key vends. Dropped even under `retain_keys`: those are OUR held
        // epoch keys kept for a later self-scrub, whereas a parked vend is
        // undelivered key material for a community we no longer hold — nothing
        // re-judges it once the community is gone, and it would resurrect on
        // re-join to seat a stale epoch.
        Some("DELETE FROM pending_channel_keys WHERE community_id = ?1"),
        // Per-entity edition heads (keyless model) — else stale refuse-downgrade floors + self_hash
        // anchors survive a leave/re-join and reject a legitimately reset chain.
        Some("DELETE FROM community_edition_heads WHERE community_id = ?1"),
        Some("DELETE FROM community_bans WHERE community_id = ?1"),
        Some("DELETE FROM community_writers WHERE community_id = ?1"),
        Some("DELETE FROM community_held_wraps WHERE community_id = ?1"),
        Some("DELETE FROM community_audit_local WHERE community_id = ?1"),
        Some("DELETE FROM settings WHERE key LIKE 'writer_postable:' || ?1 || ':%'"),
        Some("DELETE FROM settings WHERE key LIKE 'writer_cert:' || ?1 || ':%'"),
        Some("DELETE FROM settings WHERE key LIKE 'writer_retract:' || ?1 || ':%'"),
        Some("DELETE FROM settings WHERE key LIKE 'writer_retract_by:' || ?1 || ':%'"),
        Some("DELETE FROM settings WHERE key = 'departures_seen:' || ?1"),
    ]
    .into_iter()
    .flatten()
    {
        tx.execute(sql, params![community_id])
            .map_err(|e| format!("delete community: {e}"))?;
    }
    tx.commit().map_err(|e| format!("delete community commit: {e}"))?;
    if let Some(cid) = crate::community::id32_from_hex(community_id) {
        banlist_cache().write().unwrap_or_else(|e| e.into_inner()).remove(&cid);
    }
    forget_community_channels(community_id);
    // `community_message_keys` and `community_wrap_owners` are INTENTIONALLY left intact: those are our OWN keys for
    // NIP-09-deleting our own messages. The right to erase our own content from relays outlives membership
    // — even after a ban or leave we must keep the ability to purge what we sent — so they survive a
    // community delete. (Keyed by message_id, no community_id; there is nothing community-scoped to drop.)
    Ok(())
}

/// Observed participants: the best-effort member list of a Community, newest-active first.
/// Membership is NOT authoritative (a lurker who never posts and never announced won't appear).
/// A member is included when they have real activity — a posted message/reaction/edit, OR a
/// join presence (kind 3306) — UNLESS that is superseded by a more-recent leave, OR they are
/// banned. So a "leave" actually removes a member, and a leave-then-rejoin/post re-adds them.
/// `created_at` is in seconds. Result is capped (anti-flood); see [`COMMUNITY_MEMBER_CAP`].
pub fn community_member_activity(community_id: &str) -> Result<Vec<(String, u64)>, String> {
    community_member_activity_capped(community_id, true)
}

/// [`community_member_activity`] with the anti-flood display cap OPTIONAL. A roster seed
/// passes `capped = false` so EVERY member seeds — a silent truncation there would permanently
/// strand the dropped members (absent from the snapshot → absent from `memberlist()` → excluded
/// from every future rotation). Every other caller keeps the cap.
pub fn community_member_activity_capped(community_id: &str, capped: bool) -> Result<Vec<(String, u64)>, String> {
    member_activity(community_id, capped, false)
}

/// [`community_member_activity`] for rules where leaving counts: a Grant holder is present only at
/// time 0, so any Leave or Kick of theirs outranks a Role that lingers. Uncapped.
pub fn community_member_activity_strict(community_id: &str) -> Result<Vec<(String, u64)>, String> {
    member_activity(community_id, false, true)
}

/// `strict`: Grants stamped 0 rather than now, and a post at a writer address no sign of
/// presence, since a writer who left still holds a key to post there.
fn member_activity(community_id: &str, capped: bool, strict: bool) -> Result<Vec<(String, u64)>, String> {
    let grant_stamp = if strict { 0 } else { now_secs() as u64 };
    /// Cap on rendered members — bounds a presence-flood (fresh-identity 3306 spam) from
    /// growing the list / profile-fetch fan-out without limit.
    const COMMUNITY_MEMBER_CAP: usize = 500;
    use std::collections::HashMap;

    // The owner is seeded by `fold_members`, which derives it from the community id.
    if !community_held(&CommunityId(hex_id_to_32(community_id)?))? {
        return Ok(Vec::new());
    }

    let chat_ints = community_chat_row_ids(community_id)?;

    // APPLICATION_SPECIFIC (30078) is the kind for presence/system events; everything else in a
    // community channel is real message activity. Inlined as a constant integer (no injection).
    let sys = crate::stored_event::event_kind::APPLICATION_SPECIFIC;

    // active_at[npub] = newest real-activity time (any non-presence event), folded with joins below. The
    // proven owner + roster grant-holders are NOT seeded here — they're re-asserted AFTER the leave/ban
    // filter (else a stale message would overwrite the seed and a later `left` would wrongly cut a current
    // admin — the retain-set inversion). See the re-assert block below.
    let mut active: HashMap<String, u64> = HashMap::new();
    let mut left: HashMap<String, u64> = HashMap::new();
    // No channel has any events yet (e.g. fresh community) → skip the activity queries; the owner + roster
    // are still surfaced by the post-filter re-assert below.
    if !chat_ints.is_empty() {
    let conn = super::get_db_connection_guard_static()?;
    let placeholders = chat_ints.iter().map(|_| "?").collect::<Vec<_>>().join(",");

    {
        let sql = format!(
            "SELECT npub, MAX(created_at) FROM events \
             WHERE chat_id IN ({placeholders}) AND kind != {sys} AND npub IS NOT NULL AND npub != '' \
             GROUP BY npub"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (npub, at) = row.map_err(|e| e.to_string())?;
            active.insert(npub, at);
        }
    }

    // A post its Channel's rules held, kept marked on a manager's device, is no sign of membership;
    // for the writer rules, neither is one at a writer address.
    {
        let excluded = if strict { "(instr(tags, '[\"unauthorized\"]') > 0 OR instr(tags, '[\"writer\"]') > 0)" } else { "instr(tags, '[\"unauthorized\"]') > 0" };
        let sql = format!("SELECT DISTINCT npub FROM events WHERE chat_id IN ({placeholders}) AND kind != {sys} AND npub IS NOT NULL AND {excluded}");
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let flagged: Vec<String> = stmt.query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| r.get(0)).map_err(|e| e.to_string())?.flatten().collect();
        let sql = format!("SELECT MAX(created_at) FROM events WHERE chat_id IN ({placeholders}) AND kind != {sys} AND npub = ? AND NOT {excluded}");
        for npub in flagged {
            let mut args: Vec<rusqlite::types::Value> = chat_ints.iter().map(|i| rusqlite::types::Value::Integer(*i)).collect();
            args.push(npub.clone().into());
            let newest: Option<i64> = conn.query_row(&sql, rusqlite::params_from_iter(args), |r| r.get(0)).map_err(|e| e.to_string())?;
            match newest {
                Some(at) => active.insert(npub, at.max(0) as u64),
                None => active.remove(&npub),
            };
        }
    }

    // Fold presence: a join (event-type "1") is activity; a leave (event-type "0") may remove.
    {
        let sql = format!(
            "SELECT npub, created_at, tags FROM events \
             WHERE chat_id IN ({placeholders}) AND kind = {sys} AND npub IS NOT NULL AND npub != ''"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64, r.get::<_, String>(2)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        // The reader goes back before every presence event's tags are parsed.
        drop(stmt);
        drop(conn);
        for (npub, at, tags_json) in rows {
            // SystemEventType: 1 = MemberJoined, 0 = MemberLeft (carried in an ["event-type", n] tag).
            let etype = serde_json::from_str::<Vec<Vec<String>>>(&tags_json)
                .ok()
                .and_then(|tags| {
                    tags.into_iter()
                        .find(|t| t.first().map(|s| s == "event-type").unwrap_or(false))
                        .and_then(|t| t.into_iter().nth(1))
                });
            match etype.as_deref() {
                Some("1") => {
                    let e = active.entry(npub).or_insert(0);
                    if at > *e { *e = at; }
                }
                Some("0") => {
                    let e = left.entry(npub).or_insert(0);
                    if at > *e { *e = at; }
                }
                _ => {}
            }
        }
    }
    }

    // Events store bech32, so compare on bech32.
    let banned: std::collections::HashSet<String> = banned_set(community_id)
        .iter()
        .filter_map(|b| PublicKey::from_slice(b).ok())
        .filter_map(|pk| pk.to_bech32().ok())
        .collect();

    // Member iff active, not banned, and last activity is at-or-after the last leave.
    let mut out: Vec<(String, u64)> = active
        .into_iter()
        .filter(|(npub, at)| !banned.contains(npub) && left.get(npub).is_none_or(|l| at >= l))
        .collect();

    // RE-ASSERT authorized members AFTER the activity/leave filter: the proven owner + every
    // non-empty-grant roster holder is a member regardless of stale activity or a `left` — a privatize/ban
    // retain set must NEVER silently shed an authorized member (a leave or an old message must not drop a
    // current admin; that read-cut would lock a sitting admin out of their own community). Banned is the
    // only exclusion (a ban revokes the role anyway). Stamped `now_secs()` so they sort to the top and
    // survive the cap. Computed POST-filter so neither the leave filter nor a stale overwrite can cut them.
    {
        let mut present: std::collections::HashSet<String> = out.iter().map(|(n, _)| n.clone()).collect();
        let mut reassert = |npub: String| {
            if !banned.contains(&npub) && present.insert(npub.clone()) {
                out.push((npub, grant_stamp));
            }
        };
        if let Ok(roles) = get_community_roles(community_id) {
            for g in &roles.grants {
                if g.role_ids.is_empty() {
                    continue; // an empty grant is a revoked role, not a member
                }
                if let Some(b32) = PublicKey::from_hex(&g.member).ok().and_then(|pk| pk.to_bech32().ok()) {
                    reassert(b32);
                }
            }
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.1));
    if capped {
        out.truncate(COMMUNITY_MEMBER_CAP);
    }
    Ok(out)
}

/// Per-link join counts for the owner's public invites: `label -> distinct joiners` who joined
/// via a link minted by `inviter`. Distinct by joiner so a rejoin isn't double-counted. Labels are
/// unique per creator (random fallback ensures it), so (inviter, label) keys a single link.
///
/// Reads the attribution each joiner echoes into their Guestbook Join.
pub fn community_invite_join_counts(
    community_id: &str,
    inviter: &PublicKey,
) -> Result<std::collections::HashMap<String, u64>, String> {
    use std::collections::{HashMap, HashSet};
    hex_id_to_32(community_id)?;
    // Bundles name their creator in hex; older ones in bech32.
    let (hex, npub) = (inviter.to_hex(), inviter.to_bech32().unwrap_or_default());
    let (events, _) = get_guestbook(community_id)?;
    let mut per_label: HashMap<String, HashSet<PublicKey>> = HashMap::new();
    for e in &events {
        let crate::community::v2::guestbook::GuestbookEntry::Join { member, invited_by: Some((by, label)), .. } = &e.entry else {
            continue;
        };
        if by.eq_ignore_ascii_case(&hex) || *by == npub {
            per_label.entry(label.clone()).or_default().insert(*member);
        }
    }
    Ok(per_label.into_iter().map(|(k, v)| (k, v.len() as u64)).collect())
}

/// One author's posting footprint in a Community, from the local store: how much
/// they have said and the window they said it in. Feeds the moderation panel's
/// tenure and volume signals ([`crate::community::raid`]). Times are seconds.
#[derive(Debug, Clone)]
pub struct AuthorFootprint {
    pub npub: String,
    pub messages: u64,
    pub first_secs: u64,
    pub last_secs: u64,
}

/// The `chats` row ids of a Community's channels. Deliberately not [`load_community`]:
/// that decrypts every channel key and every retained epoch key, and all this needs is
/// the ids. A channel with no events yet has no row and is simply absent.
pub fn community_chat_row_ids(community_id: &str) -> Result<Vec<i64>, String> {
    let channel_ids = community_channel_ids(community_id)?;
    Ok(channel_ids.iter().filter_map(|c| super::id_cache::get_chat_id_by_identifier(c).ok()).collect())
}

/// A community's channel ids, read from the rows alone (no key opened).
pub fn community_channel_ids(community_id: &str) -> Result<Vec<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT channel_id FROM community_channels WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// A stored policy: the exact bytes a community declared, plus what the engine
/// needs to use them.
#[derive(Debug, Clone)]
pub struct StoredPolicy {
    /// Stable local id. Once policies ride the control plane this becomes the
    /// edition's entity id.
    pub policy_id: String,
    /// The EXACT bytes, never a re-serialization — `policy_hash` is over these,
    /// and re-encoding them would change the only identity the wire has.
    pub bytes: String,
    pub hash: String,
    pub enabled: bool,
    pub updated_at: u64,
}

/// Every policy a community holds, enabled or not, ordered by id so two clients
/// read them in the same order.
pub fn get_community_policies(community_id: &str) -> Result<Vec<StoredPolicy>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT policy_id, bytes, hash, enabled, updated_at FROM community_policies \
             WHERE community_id = ?1 ORDER BY policy_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| {
            Ok(StoredPolicy {
                policy_id: r.get::<_, String>(0)?,
                bytes: dec_txt(&r.get::<_, String>(1)?),
                hash: r.get::<_, String>(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                updated_at: r.get::<_, i64>(4)?.max(0) as u64,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// Store one policy verbatim. The caller validates BEFORE calling: an invalid
/// policy is a rejected edit, not a stored one that evaluates to nothing.
pub fn set_community_policy(
    community_id: &str,
    policy_id: &str,
    bytes: &str,
    hash: &str,
    enabled: bool,
    updated_at: u64,
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT INTO community_policies (community_id, policy_id, bytes, hash, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(community_id, policy_id) DO UPDATE SET bytes=?3, hash=?4, enabled=?5, updated_at=?6",
        params![community_id, policy_id, enc_txt(bytes)?, hash, enabled as i64, updated_at as i64],
    )
    .map_err(|e| format!("save policy: {e}"))?;
    Ok(())
}

pub fn delete_community_policy(community_id: &str, policy_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM community_policies WHERE community_id = ?1 AND policy_id = ?2",
        params![community_id, policy_id],
    )
    .map_err(|e| format!("delete policy: {e}"))?;
    Ok(())
}

/// One message as the policy engine reads it: the identity, author, channel and
/// text a rule needs to cite what it convicted on.
#[derive(Debug, Clone)]
pub struct PolicyMessage {
    /// The inner rumor id, hex — a chunk-set wire event carries many messages,
    /// so only this names one.
    pub id: String,
    pub npub: String,
    /// Channel eid hex, so an exempt channel can be excluded by id.
    pub channel_id: String,
    /// Milliseconds: the engine clamps and orders on inner time.
    pub at_ms: u64,
    pub text: String,
    /// p-tags on the message — the only thing that counts as a mention.
    pub mentions: u32,
}

/// The evaluation corpus: the newest `limit` messages a Community's channels
/// hold, with the identity and channel `community_recent_texts` discards.
/// Ordered oldest-first so the caller can clamp from the front.
pub fn community_policy_messages(community_id: &str, limit: usize) -> Result<Vec<PolicyMessage>, String> {
    let channel_ids: Vec<String> = {
        let conn = super::get_db_connection_guard_static()?;
        let mut stmt = conn
            .prepare("SELECT channel_id FROM community_channels WHERE community_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![community_id], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    if channel_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    // chat row id -> channel eid, so each message reports the channel it lives in.
    let mut chat_to_channel: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    for c in &channel_ids {
        if let Ok(row) = super::id_cache::get_chat_id_by_identifier(c) {
            chat_to_channel.insert(row, c.clone());
        }
    }
    if chat_to_channel.is_empty() {
        return Ok(Vec::new());
    }
    let text = crate::stored_event::event_kind::PRIVATE_DIRECT_MESSAGE;
    let chat_ints: Vec<i64> = chat_to_channel.keys().copied().collect();
    let rows: Vec<(String, String, i64, i64, String)> = {
        let conn = super::get_db_connection_guard_static()?;
        let placeholders = chat_ints.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, npub, chat_id, created_at, content FROM events \
             WHERE chat_id IN ({placeholders}) AND kind = {text} \
               AND npub IS NOT NULL AND npub != '' \
             ORDER BY created_at DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    let mut out: Vec<PolicyMessage> = crate::crypto::open_batch(|at_rest| rows
        .into_iter()
        .filter_map(|(id, npub, chat_id, created_at, content)| {
            let channel_id = chat_to_channel.get(&chat_id)?.clone();
            let content = at_rest.open_or_keep(content);
            // Vector has no mention tag: a mention is an npub in the body, which
            // the renderer turns into a pill. Counting `p` tags counted the reply
            // parent and the reaction target instead — 1 on an ordinary reply, 0
            // on a message that named fifteen people.
            let mentions = crate::community::policy::matchers::count_mentions(&content);
            Some(PolicyMessage {
                id,
                npub,
                channel_id,
                at_ms: (created_at.max(0) as u64).saturating_mul(1000),
                text: content,
                mentions,
            })
        })
        .collect());
    out.reverse(); // oldest-first
    Ok(out)
}

/// Per-author message aggregates across a Community's channels. Counts only what a
/// person actually SAID — kinds 14/15 — so a reaction spammer doesn't read as a
/// conversationalist and an edit doesn't double-count its original.
pub fn community_author_footprints(community_id: &str) -> Result<Vec<AuthorFootprint>, String> {
    let chat_ints = community_chat_row_ids(community_id)?;
    if chat_ints.is_empty() {
        return Ok(Vec::new());
    }
    let text = crate::stored_event::event_kind::PRIVATE_DIRECT_MESSAGE;
    let file = crate::stored_event::event_kind::FILE_ATTACHMENT;
    let conn = super::get_db_connection_guard_static()?;
    let placeholders = chat_ints.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT npub, COUNT(*), MIN(created_at), MAX(created_at) FROM events \
         WHERE chat_id IN ({placeholders}) AND kind IN ({text}, {file}) \
           AND npub IS NOT NULL AND npub != '' \
         GROUP BY npub"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| {
            Ok(AuthorFootprint {
                npub: r.get::<_, String>(0)?,
                messages: r.get::<_, i64>(1)?.max(0) as u64,
                first_secs: r.get::<_, i64>(2)?.max(0) as u64,
                last_secs: r.get::<_, i64>(3)?.max(0) as u64,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// The newest `limit` message texts across a Community's channels, decrypted, as
/// `(npub, created_at_secs, text)`. Newest-first is the right window for raid work:
/// a wave is by definition recent, and reading the whole history would pay a vault
/// decrypt per row for messages that predate the attack.
pub fn community_recent_texts(community_id: &str, limit: usize) -> Result<Vec<(String, u64, String)>, String> {
    let chat_ints = community_chat_row_ids(community_id)?;
    if chat_ints.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let text = crate::stored_event::event_kind::PRIVATE_DIRECT_MESSAGE;
    let rows: Vec<(String, u64, String)> = {
        let conn = super::get_db_connection_guard_static()?;
        let placeholders = chat_ints.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT npub, created_at, content FROM events \
             WHERE chat_id IN ({placeholders}) AND kind = {text} \
               AND npub IS NOT NULL AND npub != '' \
             ORDER BY created_at DESC LIMIT {limit}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64, r.get::<_, String>(2)?))
            })
            .map_err(|e| e.to_string())?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    Ok(crate::crypto::open_batch(|at_rest| {
        rows.into_iter().map(|(npub, at, content)| (npub, at, at_rest.open_or_keep(content))).collect()
    }))
}

/// Replace a Community's stored banlist (JSON array of hex pubkeys) + the `created_at` (secs) of
/// the edition it came from. `at` is the version: the owner's own ban/unban writes its freshly
/// built event time, and `fetch_and_apply_banlist` only calls this with a strictly-newer edition,
/// so the stored banlist can never roll backwards.
// ── Banlist cache ────────────────────────────────────────────────────────────
// The inbound ban check runs per community event; uncached, each call costs a DB
// fetch + ChaCha20 vault decrypt + JSON parse, even though the banlist only changes
// on a fold. Cache the banned pubkeys as raw bytes, keyed by community id bytes —
// kept coherent by write-through in `set_community_banlist` and eviction in
// `delete_community_inner`, and held on the session, so a swap starts empty.
// Absent entry = not yet loaded (lazy-fills from DB on first read). An id not in
// canonical hex skips the cache and reads the DB.
type BanSet = std::sync::Arc<std::collections::HashSet<[u8; 32]>>;
struct BanlistCache;
type BanlistMap = std::sync::RwLock<std::collections::HashMap<[u8; 32], BanSet>>;

fn banlist_cache() -> std::sync::Arc<BanlistMap> {
    crate::db::current_session().scoped::<BanlistCache, BanlistMap>()
}

/// Most communities ban nobody; they all share this one empty set.
fn empty_ban_set() -> BanSet {
    static EMPTY: std::sync::LazyLock<BanSet> = std::sync::LazyLock::new(Default::default);
    std::sync::Arc::clone(&EMPTY)
}

fn ban_set_from_hexes(hexes: &[String]) -> BanSet {
    let set: std::collections::HashSet<[u8; 32]> =
        hexes.iter().filter_map(|h| crate::simd::hex::hex_to_bytes_32_checked(h)).collect();
    if set.is_empty() { empty_ban_set() } else { std::sync::Arc::new(set) }
}

/// Drop every cached banlist.
pub fn clear_banlist_cache() {
    *banlist_cache().write().unwrap_or_else(|e| e.into_inner()) = Default::default();
}

/// [`banned_set`] by community id bytes — the inbound hot path's form.
pub fn banned_set_of(community: &[u8; 32]) -> BanSet {
    let cache = banlist_cache();
    if let Some(set) = cache.read().unwrap_or_else(|e| e.into_inner()).get(community) {
        return std::sync::Arc::clone(set);
    }
    let set = ban_set_from_hexes(&get_community_banlist(&crate::simd::hex::bytes_to_hex_32(community)).unwrap_or_default());
    cache.write().unwrap_or_else(|e| e.into_inner()).insert(*community, std::sync::Arc::clone(&set));
    set
}

/// The banned-pubkey set for a community, lazily decrypted+parsed from DB on first
/// use and held (no further DB/decrypt) until a fold or delete invalidates it.
pub fn banned_set(community_id: &str) -> BanSet {
    match crate::community::id32_from_hex(community_id) {
        Some(cid) => banned_set_of(&cid),
        None => ban_set_from_hexes(&get_community_banlist(community_id).unwrap_or_default()),
    }
}

/// Whether `author` is banned in this community. The per-event hot check: a HashSet
/// lookup once warm, no DB / decrypt / parse.
pub fn is_author_banned(community_id: &str, author: &PublicKey) -> bool {
    let set = banned_set(community_id);
    !set.is_empty() && set.contains(author.as_bytes())
}

/// [`is_author_banned`] by community id bytes.
pub fn is_author_banned_in(community: &[u8; 32], author: &PublicKey) -> bool {
    let set = banned_set_of(community);
    !set.is_empty() && set.contains(author.as_bytes())
}

/// Drop the moderation console's memoised verdict for this community. Called from
/// every write that changes who the console reports on, so an answer adopted from
/// ANOTHER admin's action invalidates it exactly like a local one: with a bot doing
/// the moderating, the stale console is the human admin's, and it is the only view
/// of the raid they have.
fn forget_console_verdict(community_id: &str) {
    crate::VectorCore::invalidate_raid_report(community_id);
}

pub fn set_community_banlist(community_id: &str, banned_hex: &[String], at: i64) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    // MONOTONIC in the edition version. Un-banning is the fail-OPEN direction — the
    // un-banned party's rotations and messages start being honored again — so an
    // older edition must never overwrite a newer one, however it reached us. The
    // fold's floors already refuse a downgrade; this makes the storage layer refuse
    // it too, so no future folding change can reintroduce the hole. Checked before
    // writing so a refused downgrade touches neither the row nor the hot cache.
    let current_at: Option<i64> = conn
        .query_row("SELECT banlist_at FROM communities WHERE community_id = ?1", params![community_id], |r| r.get(0))
        .optional()
        .map_err(|e| format!("read banlist_at: {e}"))?
        .flatten();
    if current_at.is_some_and(|have| have > at) {
        crate::log_warn!(
            "[Banlist] refused a stale edition (v{at} behind held v{}) — un-banning is the fail-open direction",
            current_at.unwrap_or(0)
        );
        return Ok(());
    }
    // The banlist lives in `community_bans`, not as a blob: SQL has to compare it
    // against `events.npub` to drop a banned author's rows BEFORE `LIMIT`, and a
    // filter applied after `LIMIT` returns short pages. Whole-set replace in one
    // transaction — the fold hands us the complete list every time, so there is no
    // partial state to reconcile and no second copy to drift.
    let tx = conn.unchecked_transaction().map_err(|e| format!("banlist tx: {e}"))?;
    tx.execute("DELETE FROM community_bans WHERE community_id = ?1", params![community_id])
        .map_err(|e| format!("clear banlist: {e}"))?;
    {
        let mut ins = tx
            .prepare("INSERT INTO community_bans (community_id, npub) VALUES (?1, ?2)")
            .map_err(|e| format!("prepare banlist insert: {e}"))?;
        // Stored as bech32, what `events.npub` holds, and sealed at rest: SQL is handed the
        // opened list (see `banned_npubs_json`). An unparseable entry is kept verbatim and
        // matches nothing. Sealed values never collide, so duplicates are dropped here.
        let joinable: std::collections::BTreeSet<String> = banned_hex
            .iter()
            .map(|hex| {
                PublicKey::from_hex(hex)
                    .ok()
                    .and_then(|pk| pk.to_bech32().ok())
                    .unwrap_or_else(|| hex.clone())
            })
            .collect();
        crate::crypto::seal_batch(|seal| {
            for npub in &joinable {
                ins.execute(params![community_id, seal.seal(npub)?])
                    .map_err(|e| format!("insert ban: {e}"))?;
            }
            Ok::<_, String>(())
        })?;
    }
    tx.execute(
        "UPDATE communities SET banlist_at = ?1 WHERE community_id = ?2",
        params![at, community_id],
    )
    .map_err(|e| format!("set banlist_at: {e}"))?;
    tx.commit().map_err(|e| format!("commit banlist: {e}"))?;
    forget_console_verdict(community_id);
    // Write-through: the hot-path cache must not lag a fold — a stale ban would wrongly
    // vanish a now-unbanned author's messages (fail-closed).
    if let Some(cid) = crate::community::id32_from_hex(community_id) {
        banlist_cache().write().unwrap_or_else(|e| e.into_inner()).insert(cid, ban_set_from_hexes(banned_hex));
    }
    Ok(())
}

/// Per-npub ban marks: lowercase-hex npub → `created_at` (secs) of the newest AUTHORIZED
/// banlist edition that named them. Retained past an un-ban on purpose — it is what stops a
/// pre-ban Join resurrecting a phantom member (CORD-02 §5 counts observation forward of the
/// latest Leave, Kick **or Ban**).
pub fn get_community_ban_marks(community_id: &str) -> Result<std::collections::BTreeMap<String, u64>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let json: Option<String> = conn
        .query_row(
            "SELECT banlist_marks FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get ban marks: {e}"))?;
    Ok(json.and_then(|j| serde_json::from_str(&dec_txt(&j)).ok()).unwrap_or_default())
}

/// MERGE fresh ban marks into the stored set, keeping the LATER time per npub and never
/// dropping an npub. A fold only sees the editions still in its window, so replacing
/// wholesale would forget every ban that has since aged out — precisely the history the
/// suppression depends on.
pub fn merge_community_ban_marks(community_id: &str, marks: &std::collections::BTreeMap<String, u64>) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    if marks.is_empty() {
        return Ok(false);
    }
    let mut stored = get_community_ban_marks(community_id)?;
    let mut changed = false;
    for (npub, at) in marks {
        let slot = stored.entry(npub.clone()).or_insert(0);
        if *at > *slot {
            *slot = *at;
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    let json = enc_txt(&serde_json::to_string(&stored).map_err(|e| e.to_string())?)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET banlist_marks = ?1 WHERE community_id = ?2",
        params![json, community_id],
    )
    .map_err(|e| format!("set ban marks: {e}"))?;
    Ok(true)
}

/// The `created_at` (secs) of the banlist edition currently stored, or 0 if none. The version
/// floor the rollback guard compares against.
pub fn get_community_banlist_at(community_id: &str) -> Result<i64, String> {
    let conn = super::get_db_connection_guard_static()?;
    let at: Option<i64> = conn
        .query_row(
            "SELECT banlist_at FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get banlist_at: {e}"))?;
    Ok(at.unwrap_or(0))
}

/// Replace a Community's cached role graph (the aggregated `CommunityRoles`) + the `created_at`
/// (secs) of the newest per-entity edition it was built from. `at` is the version floor: the
/// fetch path only calls this with a strictly-newer aggregate, so the role graph can't roll
/// backwards (same guard as the banlist).
pub fn set_community_roles(
    community_id: &str,
    roles: &crate::community::roles::CommunityRoles,
    at: i64,
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    forget_console_verdict(community_id);
    let json = enc_txt(&serde_json::to_string(roles).map_err(|e| e.to_string())?)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET roles = ?1, roles_at = ?2 WHERE community_id = ?3",
        params![json, at, community_id],
    )
    .map_err(|e| format!("set roles: {e}"))?;
    bump_membership_generation();
    Ok(())
}

/// A Community's cached role graph. Empty (default) for an unknown community or none stored.
pub fn get_community_roles(
    community_id: &str,
) -> Result<crate::community::roles::CommunityRoles, String> {
    let conn = super::get_db_connection_guard_static()?;
    let json = stored_roles(&conn, community_id).map_err(|e| format!("get roles: {e}"))?;
    Ok(parse_roles(json, dec_txt))
}

/// A community's roles JSON as stored (sealed when Local Encryption is on).
fn stored_roles(conn: &rusqlite::Connection, community_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT roles FROM communities WHERE community_id = ?1", params![community_id], |r| r.get(0))
        .optional()
}

/// Open and parse stored roles; anything missing or unreadable is the empty roster.
fn parse_roles(json: Option<String>, open: impl FnOnce(&str) -> String) -> crate::community::roles::CommunityRoles {
    json.and_then(|j| serde_json::from_str(&open(&j)).ok()).unwrap_or_default()
}

/// The `created_at` (secs) of the role-graph edition currently stored, or 0 if none.
pub fn get_community_roles_at(community_id: &str) -> Result<i64, String> {
    let conn = super::get_db_connection_guard_static()?;
    let at: Option<i64> = conn
        .query_row(
            "SELECT roles_at FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get roles_at: {e}"))?;
    Ok(at.unwrap_or(0))
}

/// Record the current head (version + self_hash) of a control entity's edition chain (keyless model).
/// The send side reads this to emit the next edition as `version+1` citing `self_hash` as `prev_hash`;
/// the fold uses it as the per-entity refuse-downgrade floor + anchor. Upserts per (community, entity).
/// `inner_id` and `author` are the head edition's fork tiebreak key (see [`converge_edition_head_at_epoch`]).
pub fn set_edition_head(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32]) -> Result<(), String> {
    set_edition_head_inner(community_id, entity_id, version, self_hash, None, None, None)
}

/// As [`set_edition_head`], but also records the head edition's `inner_id` (the deterministic tiebreak
/// key), so a later same-version convergence can rank against it. A plain advance carries it through.
pub fn set_edition_head_with_id(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32]) -> Result<(), String> {
    set_edition_head_inner(community_id, entity_id, version, self_hash, Some(inner_id), None, None)
}

/// As [`set_edition_head_with_id`], stamping an EXPLICIT epoch — the epoch the caller's fold actually
/// ran under — instead of reading the community row at write time. Closes the TOCTOU where a
/// concurrent re-founding bumps `server_root_epoch` between a fold and its head persist, which would
/// stamp an old-plane version as the new epoch's floor and wedge the new epoch's genuine head.
/// Forget one entity's floor.
///
/// A floor refuses to fold an edition older than the one we last saw, which is
/// what stops a relay rolling our authority backwards. But a floor for an entity
/// the plane will never serve again — its author was demoted, so their editions
/// stopped counting; or it aged out entirely — is a veto nothing can ever satisfy,
/// and it blocks the WHOLE roster cache behind it. Dropping such a floor is only
/// safe once a coherent, quorum-backed read has failed to find the entity, which
/// is the one caller's job to establish.
pub fn delete_edition_head(community_id: &str, entity_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "DELETE FROM community_edition_heads WHERE community_id = ?1 AND entity_id = ?2",
        params![community_id, entity_id],
    )
    .map_err(|e| format!("delete edition head: {e}"))?;
    Ok(())
}

pub fn set_edition_head_at_epoch(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32], author: &[u8; 32], epoch: u64) -> Result<(), String> {
    set_edition_head_inner(community_id, entity_id, version, self_hash, Some(inner_id), Some(author), Some(epoch))
}

/// [`set_edition_head_at_epoch`] for a head of `family` ([`FLOOR_AUTHORITY`] or
/// [`FLOOR_CONTROL`]). An authority head takes a coordinate a control row holds; a control
/// head never takes one an authority row holds.
#[allow(clippy::too_many_arguments)]
pub fn set_family_head_at_epoch(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32], author: &[u8; 32], epoch: u64, family: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT INTO community_edition_heads (community_id, entity_id, version, self_hash, inner_id, epoch, author, family)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(community_id, entity_id) DO UPDATE SET
            version = excluded.version,
            self_hash = excluded.self_hash,
            inner_id = excluded.inner_id,
            epoch = excluded.epoch,
            author = excluded.author,
            family = excluded.family
         WHERE NOT (community_edition_heads.family = 'a' AND excluded.family IS NOT 'a')
           AND (excluded.epoch > community_edition_heads.epoch
            OR (excluded.epoch = community_edition_heads.epoch AND excluded.version > community_edition_heads.version)
            OR (excluded.family = 'a' AND community_edition_heads.family IS NOT NULL AND community_edition_heads.family IS NOT 'a'))",
        params![community_id, entity_id, version as i64, self_hash.as_slice(), inner_id.as_slice(), epoch as i64, author.as_slice(), family],
    )
    .map_err(|e| format!("set edition head: {e}"))?;
    Ok(())
}

/// A floor of the authority fold: Roles, Grants, the Banlist, Tenures, overwrites, syncs.
pub const FLOOR_AUTHORITY: &str = "a";
/// A floor of the control fold: metadata, Channels, Categories, pins, the invite registry.
pub const FLOOR_CONTROL: &str = "c";

/// Entity → family, for the rows that record one.
pub fn edition_head_families(community_id: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT entity_id, family FROM community_edition_heads WHERE community_id = ?1 AND family IS NOT NULL")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn set_edition_head_inner(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: Option<&[u8; 32]>, author: Option<&[u8; 32]>, epoch: Option<u64>) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // MONOTONIC, EPOCH-PRIMARY: the head IS the refuse-downgrade floor. The recorded `epoch` is
    // the fold's epoch when given explicitly, else the community's current server-root epoch
    // (re-founding bumps it + resets versions to 1). A higher epoch ALWAYS supersedes (so a
    // re-founding's v1 lands over a held v21); within an epoch, version still only advances. So a
    // stale/hostile rollback can lower neither the epoch nor the in-epoch version.
    conn.execute(
        "INSERT INTO community_edition_heads (community_id, entity_id, version, self_hash, inner_id, epoch, author)
         VALUES (?1, ?2, ?3, ?4, ?5, COALESCE(?6, (SELECT server_root_epoch FROM communities WHERE community_id = ?1), 0), ?7)
         ON CONFLICT(community_id, entity_id) DO UPDATE SET
            version = excluded.version,
            self_hash = excluded.self_hash,
            inner_id = excluded.inner_id,
            epoch = excluded.epoch,
            author = excluded.author
         WHERE excluded.epoch > community_edition_heads.epoch
            OR (excluded.epoch = community_edition_heads.epoch AND excluded.version > community_edition_heads.version)",
        params![community_id, entity_id, version as i64, self_hash.as_slice(), inner_id.map(|i| i.as_slice()), epoch.map(|e| e as i64), author.map(|a| a.as_slice())],
    )
    .map_err(|e| format!("set edition head: {e}"))?;
    Ok(())
}

/// Replace the held head with a same-version fork winner (concurrent-edit resolution). Two
/// authorized editors working from one base both produce `version`, and every client must land on
/// the same edition. The winner is decided by the CALLER, which alone can rank authors against its
/// roster (CORD-04 §1: authority first, then the lower inner id) and which compares against the
/// held key from [`get_all_edition_heads_full`]. This only refuses what the caller cannot judge:
/// another version, or another epoch (a Refounding is the advance path's job).
pub fn converge_edition_head_at_epoch(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32], author: &[u8; 32], epoch: u64) -> Result<(), String> {
    converge_family_head_at_epoch(community_id, entity_id, version, self_hash, inner_id, author, epoch, None)
}

/// [`converge_edition_head_at_epoch`], touching only a row of `family` (or one that records none).
#[allow(clippy::too_many_arguments)]
pub fn converge_family_head_at_epoch(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32], author: &[u8; 32], epoch: u64, family: Option<&str>) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE community_edition_heads
            SET self_hash = ?4, inner_id = ?5, author = ?7
          WHERE community_id = ?1 AND entity_id = ?2
            AND version = ?3
            AND epoch = ?6
            AND (?8 IS NULL OR family IS NULL OR family = ?8)
            AND (self_hash IS NOT ?4 OR inner_id IS NOT ?5 OR author IS NOT ?7)",
        params![community_id, entity_id, version as i64, self_hash.as_slice(), inner_id.as_slice(), epoch as i64, author.as_slice(), family],
    )
    .map_err(|e| format!("converge edition head: {e}"))?;
    Ok(())
}

/// Move a floor DOWN to the newest edition the Roster still admits, within one epoch. The
/// caller has proven the held head is served yet refused, on a complete authority fold
/// (refuse-downgrade guards the newest authorized edition, CORD-04 §1); nothing else may
/// lower a floor, and a different epoch is never touched.
/// Only a row of the head's own `family`, or one that records none, moves.
#[allow(clippy::too_many_arguments)]
pub fn rebase_edition_head_at_epoch(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32], author: &[u8; 32], epoch: u64, family: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE community_edition_heads
            SET version = ?3, self_hash = ?4, inner_id = ?5, author = ?7, family = COALESCE(family, ?8)
          WHERE community_id = ?1 AND entity_id = ?2 AND epoch = ?6 AND (family IS NULL OR family = ?8)",
        params![community_id, entity_id, version as i64, self_hash.as_slice(), inner_id.as_slice(), epoch as i64, author.as_slice(), family],
    )
    .map_err(|e| format!("rebase edition head: {e}"))?;
    Ok(())
}

/// The held head's inner id, or `None` if unheld or pre-migration (NULL).
pub fn get_edition_head_inner_id(community_id: &str, entity_id: &str) -> Result<Option<[u8; 32]>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let row: Option<Option<Vec<u8>>> = conn
        .query_row(
            "SELECT inner_id FROM community_edition_heads WHERE community_id = ?1 AND entity_id = ?2",
            params![community_id, entity_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get edition head inner_id: {e}"))?;
    match row.flatten() {
        Some(blob) if blob.len() == 32 => {
            let mut h = [0u8; 32];
            h.copy_from_slice(&blob);
            Ok(Some(h))
        }
        _ => Ok(None),
    }
}

/// The current head `(version, self_hash)` of a control entity's edition chain, or `None` if no
/// edition is held yet (so the next edition is the genesis, version 1, no prev_hash).
pub fn get_edition_head(community_id: &str, entity_id: &str) -> Result<Option<(u64, [u8; 32])>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let row: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT version, self_hash FROM community_edition_heads WHERE community_id = ?1 AND entity_id = ?2",
            params![community_id, entity_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("get edition head: {e}"))?;
    match row {
        Some((v, hash)) if hash.len() == 32 => {
            let mut h = [0u8; 32];
            h.copy_from_slice(&hash);
            Ok(Some((v as u64, h)))
        }
        _ => Ok(None),
    }
}

/// The set of control-entity ids (hex) this account tracks a head for. A base rotation gates its
/// head-advance on re-anchoring covering EVERY one of these (not just a matching count), so a relay
/// that withholds one entity's editions while over-serving another's can't slip a thinned control
/// plane past the rotator.
pub fn edition_head_entity_ids(community_id: &str) -> Result<std::collections::HashSet<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT entity_id FROM community_edition_heads WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = std::collections::HashSet::new();
    for row in rows {
        out.insert(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}


/// Every tracked head as `entity_hex → (epoch, version, self_hash)` — the epoch-primary floor.
/// The caller seeds the fold with ONLY the entities at the community's CURRENT epoch (a head recorded
/// at a PRIOR epoch belongs to a superseded founding, so its entity folds fresh from the new epoch's v1
/// genesis). This is what lets a re-founding's compacted v1 plane land without a version-only downgrade.
/// Every tracked head as `entity_hex → (epoch, version, self_hash, inner_id)` — the epoch-primary
/// floor INCLUDING the deterministic tiebreak key, so a fold can resolve a same-version fork at the
/// floor (converge to the fork winner) instead of wedging on it.
/// `(epoch, version, self_hash, inner_id, author)`.
type EditionHeadFull = (u64, u64, [u8; 32], Option<[u8; 32]>, Option<[u8; 32]>);
/// `(epoch, version, self_hash)`.
type EditionHead = (u64, u64, [u8; 32]);

pub fn get_all_edition_heads_full(community_id: &str) -> Result<std::collections::HashMap<String, EditionHeadFull>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT entity_id, epoch, version, self_hash, inner_id, author FROM community_edition_heads WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
                r.get::<_, Option<Vec<u8>>>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let blob32 = |b: Option<Vec<u8>>| {
        b.and_then(|b| {
            (b.len() == 32).then(|| {
                let mut i = [0u8; 32];
                i.copy_from_slice(&b);
                i
            })
        })
    };
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (entity, epoch, version, hash, inner, author) = row.map_err(|e| e.to_string())?;
        if hash.len() == 32 {
            let mut h = [0u8; 32];
            h.copy_from_slice(&hash);
            out.insert(entity, (epoch as u64, version as u64, h, blob32(inner), blob32(author)));
        }
    }
    Ok(out)
}

pub fn get_all_edition_heads_epoched(community_id: &str) -> Result<std::collections::HashMap<String, EditionHead>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT entity_id, epoch, version, self_hash FROM community_edition_heads WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Vec<u8>>(3)?))
        })
        .map_err(|e| e.to_string())?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (entity, epoch, version, hash) = row.map_err(|e| e.to_string())?;
        if hash.len() == 32 {
            let mut h = [0u8; 32];
            h.copy_from_slice(&hash);
            out.insert(entity, (epoch as u64, version as u64, h));
        }
    }
    Ok(out)
}

/// A Community's current banlist (hex pubkeys). Empty for an unknown community or empty list.
pub fn get_community_banlist(community_id: &str) -> Result<Vec<String>, String> {
    // A ticket, because the first read may adopt the legacy blob, which seals.
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT npub FROM community_bans WHERE community_id = ?1")
        .map_err(|e| format!("prepare get banlist: {e}"))?;
    let rows = stmt
        .query_map(params![community_id], |r| r.get::<_, String>(0))
        .map_err(|e| format!("get banlist: {e}"))?;
    // Back to hex for the protocol; the table holds the sealed bech32.
    let mut out: Vec<String> = crate::crypto::open_batch(|at_rest| {
        rows.flatten()
            .map(|b| at_rest.open_or_keep(b))
            .map(|b| PublicKey::parse(&b).map(|pk| pk.to_hex()).unwrap_or(b))
            .collect()
    });
    if !out.is_empty() {
        return Ok(out);
    }
    // Empty may mean "nobody is banned" or "this row predates migration 90". The
    // legacy blob is encrypted, so the migration itself could not read it — the key
    // is not available at schema time. Adopt it on first read instead, where it is,
    // and clear it so this runs once.
    drop(stmt);
    let legacy: Option<String> = conn
        .query_row(
            "SELECT banlist FROM communities WHERE community_id = ?1 AND banlist IS NOT NULL AND banlist != ''",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("read legacy banlist: {e}"))?
        .flatten();
    let Some(blob) = legacy else { return Ok(out) };
    let decoded = dec_txt(&blob);
    let Ok(list) = serde_json::from_str::<Vec<String>>(&decoded) else { return Ok(out) };
    if list.is_empty() {
        return Ok(out);
    }
    let tx = conn.unchecked_transaction().map_err(|e| format!("adopt tx: {e}"))?;
    {
        let mut ins = tx
            .prepare("INSERT INTO community_bans (community_id, npub) VALUES (?1, ?2)")
            .map_err(|e| format!("prepare adopt: {e}"))?;
        let joinable: std::collections::BTreeSet<String> = list
            .iter()
            .map(|hex| {
                PublicKey::from_hex(hex)
                    .ok()
                    .and_then(|pk| pk.to_bech32().ok())
                    .unwrap_or_else(|| hex.clone())
            })
            .collect();
        crate::crypto::seal_batch(|seal| {
            for npub in &joinable {
                ins.execute(params![community_id, seal.seal(npub)?]).map_err(|e| format!("adopt ban: {e}"))?;
            }
            Ok::<_, String>(())
        })?;
    }
    // The column is NOT NULL; an empty list retires it just as well and reads as "adopted".
    tx.execute("UPDATE communities SET banlist = '[]' WHERE community_id = ?1", params![community_id])
        .map_err(|e| format!("retire legacy banlist: {e}"))?;
    tx.commit().map_err(|e| format!("commit adopt: {e}"))?;
    crate::log_info!("[Banlist] adopted {} legacy ban(s) into community_bans", list.len());
    out = list;
    Ok(out)
}

/// Replace a Community's cached invite-link registry (active link locators, hex), folded from the
/// owner-signed vsk=5 edition. Empty = Private. The version floor lives in `community_edition_heads`
/// (the registry's own entity), so this is just the content cache (mirrors `set_community_banlist`).
pub fn set_community_invite_registry(community_id: &str, link_locators: &[String]) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let json = enc_txt(&serde_json::to_string(link_locators).map_err(|e| e.to_string())?)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET invite_registry = ?1 WHERE community_id = ?2",
        params![json, community_id],
    )
    .map_err(|e| format!("set invite registry: {e}"))?;
    // A first link or a last revoke turns a group chat into a community or back.
    crate::notify::invalidate_communities();
    Ok(())
}

/// Every Concord group that is a community rather than a group chat: more than one
/// channel we hold, or a live invite link. See [`crate::notify::is_community`].
pub fn community_ids_beyond_group_chat() -> Result<std::collections::HashSet<[u8; 32]>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT c.community_id, c.invite_registry,
                    (SELECT COUNT(*) FROM community_channels ch WHERE ch.community_id = c.community_id)
               FROM communities c",
        )
        .map_err(|e| format!("prepare community kinds: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
        .map_err(|e| format!("query community kinds: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read community kinds: {e}"))?;
    // The reader goes back before every registry is decrypted and parsed.
    drop(stmt);
    drop(conn);
    let mut out = std::collections::HashSet::new();
    for (id, registry, channels) in rows {
        let links: Vec<String> = serde_json::from_str(&dec_txt(&registry)).unwrap_or_default();
        if channels > 1 || !links.is_empty() {
            out.extend(crate::community::id32_from_hex(&id));
        }
    }
    Ok(out)
}

/// A Community's current invite-link registry (active link locators, hex). Empty for an unknown
/// community or a Private one. `is_public` = this is non-empty (computed mode).
pub fn get_community_invite_registry(community_id: &str) -> Result<Vec<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let json: Option<String> = conn
        .query_row(
            "SELECT invite_registry FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get invite registry: {e}"))?;
    Ok(json.and_then(|j| serde_json::from_str(&dec_txt(&j)).ok()).unwrap_or_default())
}

/// A folded per-creator public-invite-link set: the creator's pubkey (hex) and their active link
/// locators. Used to surface "X has N active invite links" in the UI.
pub struct InviteLinkSetRow {
    pub creator_hex: String,
    pub locators: Vec<String>,
}

/// The creators the folded invite Registry currently lists for this community —
/// the cheap local answer to "might I hold a live public link here?".
pub fn invite_link_set_creators(community_id: &str) -> Result<Vec<String>, String> {
    let conn = super::get_write_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT creator FROM community_invite_link_sets WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .map(|stored| dec_txt(&stored))
        .collect();
    Ok(rows)
}

/// Replace ALL of a Community's per-creator invite-link sets with the freshly-folded set (latest-wins).
/// Replacing wholesale (not upserting) drops a creator who has revoked every link, so the per-creator
/// view stays in lockstep with the flat registry computed in the same fold.
pub fn replace_invite_link_sets(community_id: &str, sets: &[InviteLinkSetRow]) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let mut conn = super::get_write_connection_guard_static()?;
    let tx = conn.transaction().map_err(|e| format!("invite-link-sets tx: {e}"))?;
    tx.execute("DELETE FROM community_invite_link_sets WHERE community_id = ?1", params![community_id])
        .map_err(|e| format!("clear invite-link-sets: {e}"))?;
    for s in sets {
        if s.locators.is_empty() {
            continue; // a creator with no active links is just absent (count 0)
        }
        let enc_creator = enc_txt(&s.creator_hex)?;
        let enc_locators = enc_txt(&serde_json::to_string(&s.locators).map_err(|e| e.to_string())?)?;
        // Plain INSERT: the DELETE above cleared the community's rows and `sets` has distinct creators
        // (an encrypted creator can't act as a dedup key anyway — random nonce per write).
        tx.execute(
            "INSERT INTO community_invite_link_sets (community_id, creator, locators) VALUES (?1, ?2, ?3)",
            params![community_id, enc_creator, enc_locators],
        )
        .map_err(|e| format!("insert invite-link-set: {e}"))?;
    }
    tx.commit().map_err(|e| format!("commit invite-link-sets: {e}"))?;
    Ok(())
}

/// Every creator's active invite-link set for a Community (creator hex + locators). Empty for a Private
/// community (or one not yet re-folded since this table was added).
pub fn get_invite_link_sets(community_id: &str) -> Result<Vec<InviteLinkSetRow>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT creator, locators FROM community_invite_link_sets WHERE community_id = ?1")
        .map_err(|e| format!("prepare invite-link-sets: {e}"))?;
    let rows = stmt
        .query_map(params![community_id], |r| {
            let creator_hex: String = r.get(0)?;
            let json: String = r.get(1)?;
            Ok((creator_hex, json))
        })
        .map_err(|e| format!("query invite-link-sets: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let (creator_hex, json) = row.map_err(|e| format!("row invite-link-sets: {e}"))?;
        let locators: Vec<String> = serde_json::from_str(&dec_txt(&json)).unwrap_or_default();
        out.push(InviteLinkSetRow { creator_hex: dec_txt(&creator_hex), locators });
    }
    Ok(out)
}

/// Set the owner-dissolution SEAL on a community — PERMANENT + irreversible (no clear path; there
/// is no un-dissolve). Idempotent: re-setting an already-dissolved community is a harmless no-op. Once
/// set, the control fold stops advancing and the inbound path drops every subsequent event.
/// Seal a community as dissolved. Returns whether this call TRANSITIONED it (a
/// live→dissolved flip) so the caller can fire the one-time death notification
/// exactly once — a re-wrapped tombstone (fresh outer id, same owner seal) then
/// can't spam the handler.
pub fn set_community_dissolved(community_id: &str) -> Result<bool, String> {
    let conn = super::get_write_connection_guard_static()?;
    let changed = conn
        .execute(
            "UPDATE communities SET dissolved = 1 WHERE community_id = ?1 AND dissolved = 0",
            params![community_id],
        )
        .map_err(|e| format!("set dissolved: {e}"))?;
    crate::community::v2::realtime::invalidate_held_v2_community(community_id);
    Ok(changed > 0)
}

/// Whether a community has been sealed by a folded + owner-verified GroupDissolved tombstone.
/// `false` for an unknown community.
pub fn get_community_dissolved(community_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT dissolved FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get dissolved: {e}"))?;
    Ok(v.unwrap_or(0) != 0)
}

/// Ids of every locally-stored Community.
pub fn list_community_ids() -> Result<Vec<CommunityId>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT community_id FROM communities ORDER BY created_at")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(CommunityId(hex_id_to_32(&row.map_err(|e| e.to_string())?)?));
    }
    Ok(ids)
}

/// Every channel id held under a community we still hold.
pub fn held_channel_ids() -> Result<std::collections::HashSet<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT ch.channel_id FROM community_channels ch JOIN communities c ON c.community_id = ch.community_id")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// Every relay a held community lists, without opening any key.
pub fn held_community_relays() -> Result<std::collections::HashSet<String>, String> {
    let sealed: Vec<String> = {
        let conn = super::get_db_connection_guard_static()?;
        let mut stmt = conn.prepare("SELECT relays FROM communities").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    Ok(crate::crypto::open_batch(|o| {
        sealed
            .into_iter()
            .filter_map(|s| serde_json::from_str::<Vec<String>>(&o.open_or_keep(s)).ok())
            .flatten()
            .collect()
    }))
}

/// A community's icon or banner, stored as [`CommunityImage`](crate::community::CommunityImage) JSON.
pub fn community_image(community_id: &str, banner: bool) -> Result<Option<crate::community::CommunityImage>, String> {
    let sql = if banner {
        "SELECT banner FROM communities WHERE community_id = ?1"
    } else {
        "SELECT icon FROM communities WHERE community_id = ?1"
    };
    let sealed: Option<String> = {
        let conn = super::get_db_connection_guard_static()?;
        conn.query_row(sql, params![community_id], |r| r.get::<_, Option<String>>(0))
            .optional()
            .map_err(|e| e.to_string())?
            .flatten()
    };
    let Some(sealed) = sealed else { return Ok(None) };
    let json = crate::crypto::open_batch(|o| o.open_or_keep(sealed));
    serde_json::from_str(&json).map(Some).map_err(|e| format!("community image json: {e}"))
}

// ── Concord v2 storage ───────────────────────────────────────────────────────
//
// v2 communities reuse the shared community tables (migration 65 added the
// `protocol`/`owner_pubkey`/`owner_salt`/`private` columns). The base access key
// rides `server_root_key`/`server_root_epoch`.
// A public channel stores the community_root in `channel_key` as a placeholder
// (its real secret is derived from the root); a private channel stores its own
// key. At-rest encryption reuses the same `enc_*`/`dec_*` helpers.

/// Whether the community is held locally.
pub fn community_held(id: &CommunityId) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row("SELECT 1 FROM communities WHERE community_id = ?1", params![id.to_hex()], |_| Ok(()))
        .optional()
        .map(|row| row.is_some())
        .map_err(|e| e.to_string())
}

/// The persisted CORD-02 §6 stash riding a community row: the vsk-0 fields
/// Vector doesn't drive but must republish verbatim on its own edits.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct CommunityMetaStash {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    extra: serde_json::Map<String, serde_json::Value>,
    /// The folded Categories ride here: a fold product like the channels, too small for a
    /// table of their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    categories: Vec<CategoryStash>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CategoryStash {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// As [`CommunityMetaStash`], for a channel row (vsk-2: + the voice flag).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ChannelMetaStash {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    voice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    custom: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    extra: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed: Option<crate::community::v2::community::SealedMeta>,
}

/// Persist a v2 community + its channels atomically. UPSERT so a metadata
/// re-save preserves banlist/roles (managed by the fold, not here).
pub fn save_community_v2(c: &crate::community::v2::community::CommunityV2) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let conn = super::get_write_connection_guard_static()?;
    let id_hex = crate::simd::hex::bytes_to_hex_32(&c.identity.community_id.0);
    let relays_json = serde_json::to_string(&c.relays).map_err(|e| e.to_string())?;
    let created = (c.created_at_ms / 1000) as i64;

    let enc_root = enc_key(&c.community_root)?;
    let enc_name = enc_txt(&c.name)?;
    let enc_relays = enc_txt(&relays_json)?;
    let enc_desc = enc_txt_opt(&c.description)?;
    let enc_owner_pk = enc_txt(&crate::simd::hex::bytes_to_hex_32(&c.identity.owner_xonly))?;
    let enc_owner_salt = enc_txt(&crate::simd::hex::bytes_to_hex_32(&c.identity.owner_salt))?;
    // ImageRef serializes to the CommunityImage JSON shape (`ext` rides the
    // flattened `extra`), so cache_community_image reads a v2 row's images unchanged.
    let icon_json = c.icon.as_ref().map(|i| serde_json::to_string(i).map_err(|e| e.to_string())).transpose()?;
    let banner_json = c.banner.as_ref().map(|b| serde_json::to_string(b).map_err(|e| e.to_string())).transpose()?;
    let enc_icon = enc_txt_opt(&icon_json)?;
    let enc_banner = enc_txt_opt(&banner_json)?;
    let categories: Vec<CategoryStash> = c
        .categories
        .iter()
        .map(|k| CategoryStash { id: crate::simd::hex::bytes_to_hex_32(&k.id), name: k.name.clone(), position: k.position.clone(), extra: k.extra.clone() })
        .collect();
    let stash_json = (c.meta_custom.is_some() || !c.meta_extra.is_empty() || !categories.is_empty())
        .then(|| serde_json::to_string(&CommunityMetaStash { custom: c.meta_custom.clone(), extra: c.meta_extra.clone(), categories }).map_err(|e| e.to_string()))
        .transpose()?;
    let enc_stash = enc_txt_opt(&stash_json)?;
    // The split pair (CORD-02 §2): the address as encrypted hex (owner_pubkey's
    // treatment), the secret as an encrypted blob (server_root_key's).
    let enc_control_pk = enc_txt_opt(&c.control_pk.map(|p| p.to_hex()))?;
    let enc_control_root = c.control_root.as_ref().map(enc_key).transpose()?;

    let tx = conn.unchecked_transaction().map_err(|e| format!("save v2 community tx: {e}"))?;
    tx.execute(
        "INSERT INTO communities
            (community_id, server_root_key, name, relays, created_at, description,
             server_root_epoch, dissolved, protocol, owner_pubkey, owner_salt, icon, banner, meta_extra,
             control_pk, control_root)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 2, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(community_id) DO UPDATE SET
            server_root_key=?2, name=?3, relays=?4, description=?6,
            server_root_epoch=?7, dissolved=?8, protocol=2, owner_pubkey=?9, owner_salt=?10,
            icon=?11, banner=?12, meta_extra=?13, control_pk=?14, control_root=?15",
        params![
            id_hex, enc_root, enc_name, enc_relays, created, enc_desc,
            c.root_epoch.0 as i64, c.dissolved as i64, enc_owner_pk, enc_owner_salt,
            enc_icon, enc_banner, enc_stash, enc_control_pk, enc_control_root,
        ],
    )
    .map_err(|e| format!("save v2 community: {e}"))?;

    for ch in &c.channels {
        let ch_hex = crate::simd::hex::bytes_to_hex_32(&ch.id.0);
        // channel_id is the sole PRIMARY KEY, so an UPSERT keyed on it alone would
        // let a bundle reusing ANOTHER community's channel_id overwrite that row's
        // key/epoch/private in place (a chat-plane hijack). Channel ids are random-32
        // (a genuine cross-community collision is negligible), so refuse rather than
        // clobber a foreign community's row.
        let owner_of: Option<String> = tx
            .query_row("SELECT community_id FROM community_channels WHERE channel_id=?1", params![ch_hex], |r| r.get(0))
            .optional()
            .map_err(|e| format!("channel ownership check: {e}"))?;
        if let Some(existing) = owner_of.filter(|existing| *existing != id_hex) {
            // SKIP the foreign-owned channel rather than fail the whole save: a
            // single replayed phantom (a same-owner cross-community vsk-2 edition)
            // would otherwise wedge ALL of this community's control-plane persistence
            // on every fold. The foreign row stays untouched; this community just
            // never acquires a row for that id. Loud on purpose: a community that hits
            // this on every fold is otherwise deaf with no trace.
            crate::log_warn!(
                "[v2:{}] channel {} not saved: its row belongs to community {} (unflipped migration, or a hijack attempt)",
                &id_hex[..8],
                &ch_hex[..8],
                &existing[..8.min(existing.len())]
            );
            continue;
        }
        // A public channel has no independent key; store the community_root as a
        // placeholder so the NOT NULL column is satisfied (the real secret is
        // derived from the root at read time via `channel_secret`).
        let stored_key = ch.key.unwrap_or(c.community_root);
        let enc_ch_key = enc_key(&stored_key)?;
        let enc_ch_name = enc_txt(&ch.name)?;
        let ch_stash_json = (ch.voice.is_some() || ch.meta_custom.is_some() || !ch.meta_extra.is_empty() || ch.sealed.is_some())
            .then(|| {
                serde_json::to_string(&ChannelMetaStash { voice: ch.voice, custom: ch.meta_custom.clone(), extra: ch.meta_extra.clone(), sealed: ch.sealed.clone() })
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        let enc_ch_stash = enc_txt_opt(&ch_stash_json)?;
        tx.execute(
            "INSERT INTO community_channels
                (channel_id, community_id, channel_key, epoch, name, created_at, private, meta_extra)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(channel_id) DO UPDATE SET
                channel_key=?3, epoch=?4, name=?5, private=?7, meta_extra=?8",
            params![ch_hex, id_hex, enc_ch_key, ch.epoch.0 as i64, enc_ch_name, created, ch.private as i64, enc_ch_stash],
        )
        .map_err(|e| format!("save v2 channel: {e}"))?;
    }

    // Prune channels no longer in the in-memory set — the persisted set is
    // authoritative, so a control-follow delete or a rekey removal doesn't
    // resurrect (with a stale key) on the next reload. No FK references
    // community_channels, so this cascades to nothing.
    let keep: Vec<String> = c.channels.iter().map(|ch| crate::simd::hex::bytes_to_hex_32(&ch.id.0)).collect();
    if keep.is_empty() {
        tx.execute("DELETE FROM community_channels WHERE community_id=?1", params![id_hex])
            .map_err(|e| format!("prune v2 channels: {e}"))?;
    } else {
        let placeholders = std::iter::repeat_n("?", keep.len()).collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM community_channels WHERE community_id=? AND channel_id NOT IN ({placeholders})");
        let mut binds: Vec<String> = Vec::with_capacity(keep.len() + 1);
        binds.push(id_hex.clone());
        binds.extend(keep);
        tx.execute(&sql, rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| format!("prune v2 channels: {e}"))?;
    }

    tx.commit().map_err(|e| format!("commit v2 community: {e}"))?;
    crate::community::v2::realtime::invalidate_held_v2_community(&id_hex);
    // The save may have pruned channels (DELETE ... NOT IN the new set); evict so a
    // pruned channel stops resolving to this community.
    forget_community_channels(&id_hex);
    Ok(())
}

/// Load a v2 community by id, or `None` if absent / not a v2 community.
pub fn load_community_v2(id: &CommunityId) -> Result<Option<crate::community::v2::community::CommunityV2>, String> {
    use crate::community::v2::community::{ChannelV2, CommunityV2};
    use crate::community::v2::control::CommunityIdentity;
    let conn = super::get_db_connection_guard_static()?;
    let id_hex = id.to_hex();

    let row = conn
        .query_row(
            "SELECT server_root_key, name, relays, created_at, description,
                    server_root_epoch, dissolved, owner_pubkey, owner_salt,
                    icon, banner, meta_extra, control_pk, control_root
             FROM communities WHERE community_id = ?1",
            params![id_hex],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<String>>(10)?,
                    r.get::<_, Option<String>>(11)?,
                    r.get::<_, Option<String>>(12)?,
                    r.get::<_, Option<Vec<u8>>>(13)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((root_blob, name_e, relays_e, created, desc_e, root_epoch, dissolved, owner_pk_e, owner_salt_e, icon_e, banner_e, stash_e, control_pk_e, control_root_b)) = row
    else {
        return Ok(None);
    };
    let (Some(owner_pk_e), Some(owner_salt_e)) = (owner_pk_e, owner_salt_e) else {
        return Err("v2 community row is missing its owner commitment".to_string());
    };

    // One vault read for the whole row set.
    let dk = |o: &mut crate::crypto::AtRestOpener, b: &[u8]| to_32(&o.open_blob_or_keep(b));
    let dt = |o: &mut crate::crypto::AtRestOpener, s: &str| o.open_or_keep(s.to_string());
    crate::crypto::open_batch(|o| {
        let community_root = dk(o, &root_blob)?;
        let owner_xonly = parse_hex32(&dt(o, &owner_pk_e))?;
        let owner_salt = parse_hex32(&dt(o, &owner_salt_e))?;
        let identity = CommunityIdentity { community_id: *id, owner_xonly, owner_salt };
        let relays: Vec<String> = serde_json::from_str(&dt(o, &relays_e)).unwrap_or_default();

        let mut channels = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT channel_id, channel_key, epoch, name, private, meta_extra
                     FROM community_channels WHERE community_id = ?1 ORDER BY created_at",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![id_hex], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (ch_hex, key_blob, epoch, name_e, private, ch_stash_e) = row.map_err(|e| e.to_string())?;
                let private = private != 0;
                let key = dk(o, &key_blob)?;
                // Unparseable stash degrades to empty — the fold re-persists the
                // authoritative value on its next pass.
                let ch_stash: ChannelMetaStash = ch_stash_e
                    .map(|s| dt(o, &s))
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default();
                channels.push(ChannelV2 {
                    id: ChannelId(hex_id_to_32(&ch_hex)?),
                    name: dt(o, &name_e),
                    private,
                    // A public channel derives from the root — drop the placeholder. A
                    // PRIVATE channel stored with the root value is the KEYLESS placeholder
                    // (key not yet delivered over the rekey plane): a real private key is
                    // independently random (CORD-03 §1), never the root, so reconstruct
                    // None and keep every read/send path behind the keyless guards instead
                    // of silently addressing the public plane.
                    key: (private && key != community_root).then_some(key),
                    epoch: Epoch(epoch as u64),
                    voice: ch_stash.voice,
                    meta_custom: ch_stash.custom,
                    meta_extra: ch_stash.extra,
                    writers: None,
                    sealed: ch_stash.sealed,
                });
            }
        }
        {
            let mut stmt = conn
                .prepare("SELECT channel_id, content FROM community_writers WHERE community_id = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![id_hex], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (ch_hex, content) = row.map_err(|e| e.to_string())?;
                let Some(ch) = channels.iter_mut().find(|c| crate::simd::hex::bytes_to_hex_32(&c.id.0) == ch_hex) else { continue };
                ch.writers = crate::community::v2::writers::Ledger::parse(&dt(o, &content)).filter(|l| l.channel == ch.id.0);
            }
        }
        // The held set keeps these resident; push-growth leaves up to half unused.
        channels.shrink_to_fit();

        // Unparseable image JSON degrades to no-image rather than failing the load —
        // the fold re-persists the authoritative value on its next pass.
        let icon = icon_e
            .map(|s| dt(o, &s))
            .and_then(|j| serde_json::from_str::<crate::community::v2::control::ImageRef>(&j).ok());
        let banner = banner_e
            .map(|s| dt(o, &s))
            .and_then(|j| serde_json::from_str::<crate::community::v2::control::ImageRef>(&j).ok());
        let stash: CommunityMetaStash = stash_e
            .map(|s| dt(o, &s))
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();

        // The split pair (CORD-02 §2). Fail closed on corruption: an unparseable
        // address degrades to the legacy view, and a secret that no longer derives
        // to the held address is dropped to read-only rather than signing at an
        // address nobody reads.
        let control_pk = control_pk_e
            .as_deref()
            .and_then(|e| nostr_sdk::prelude::PublicKey::from_hex(&dt(o, e)).ok());
        let control_root = match (control_pk, control_root_b) {
            (Some(pk), Some(blob)) => dk(o, &blob).ok().filter(|cr| {
                crate::community::v2::derive::control_signer_group_key(cr, id, Epoch(root_epoch as u64)).pk() == pk
            }),
            _ => None,
        };

        Ok(Some(CommunityV2 {
            identity,
            community_root,
            root_epoch: Epoch(root_epoch as u64),
            control_pk,
            control_root,
            name: dt(o, &name_e),
            description: desc_e.map(|d| dt(o, &d)),
            icon,
            banner,
            meta_custom: stash.custom,
            meta_extra: stash.extra,
            relays,
            channels,
            categories: stash
                .categories
                .into_iter()
                .filter_map(|k| {
                    let id = crate::simd::hex::hex_to_bytes_32_checked(&k.id)?;
                    Some(crate::community::v2::community::CategoryV2 { id, name: k.name, position: k.position, extra: k.extra })
                })
                .collect(),
            dissolved: dissolved != 0,
            created_at_ms: (created as u64).saturating_mul(1000),
        }))
    })
}
fn parse_hex32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("stored value is not 32-byte hex".to_string());
    }
    Ok(crate::simd::hex::hex_to_bytes_32(hex))
}

/// Load a community's persisted Guestbook: the raw membership events + the
/// newest-seen cursor (relay seconds). `([], 0)` when never synced; unparseable
/// stored JSON degrades the same way (the next sync re-seeds from zero).
pub fn get_guestbook(community_id: &str) -> Result<(Vec<crate::community::v2::guestbook::GuestbookEvent>, u64), String> {
    // The reader goes back before the decrypt and parse, which outlast the read by far.
    let row = super::get_db_connection_guard_static()?
        .query_row(
            "SELECT events, cursor_secs FROM community_guestbook WHERE community_id = ?1",
            params![community_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|e| format!("load guestbook: {e}"))?;
    let Some((events_e, cursor)) = row else {
        return Ok((Vec::new(), 0));
    };
    let events = serde_json::from_str(&dec_txt(&events_e)).unwrap_or_default();
    Ok((events, cursor.max(0) as u64))
}

struct StandingCache;
/// Per community: the Guestbook write generation and, once read, the Timeouts and Nicknames at it.
type StandingMap = std::sync::RwLock<std::collections::HashMap<String, (u64, Option<std::sync::Arc<Vec<crate::community::v2::guestbook::GuestbookEvent>>>)>>;

fn standing_cache() -> std::sync::Arc<StandingMap> {
    crate::db::current_session().scoped::<StandingCache, StandingMap>()
}

/// The Guestbook's Timeout and Nickname entries, held so every send and paint doesn't reparse
/// the whole book.
pub fn get_guestbook_standing(community_id: &str) -> Result<std::sync::Arc<Vec<crate::community::v2::guestbook::GuestbookEvent>>, String> {
    let generation = match standing_cache().read().unwrap_or_else(|e| e.into_inner()).get(community_id) {
        Some((_, Some(held))) => return Ok(held.clone()),
        Some((generation, None)) => *generation,
        None => 0,
    };
    let (events, _) = get_guestbook(community_id)?;
    use crate::community::v2::guestbook::GuestbookEntry;
    let standing: std::sync::Arc<Vec<_>> = std::sync::Arc::new(
        events.into_iter().filter(|e| matches!(e.entry, GuestbookEntry::Timeout { .. } | GuestbookEntry::Nickname { .. })).collect(),
    );
    // A write that landed during the read moved the generation on: keep what it left.
    let map = standing_cache();
    let mut cache = map.write().unwrap_or_else(|e| e.into_inner());
    let entry = cache.entry(community_id.to_string()).or_insert((0, None));
    if entry.0 == generation {
        entry.1 = Some(standing.clone());
    }
    Ok(standing)
}

/// Persist a community's Guestbook events + cursor (encrypted at rest, like the
/// community row itself). The caller owns dedup/merge — this is a plain replace.
struct MembershipGeneration;

/// Moves whenever what a Guestbook fold reads may have: a store write here, or a Control fold.
pub fn membership_generation() -> u64 {
    super::current_session().scoped::<MembershipGeneration, std::sync::atomic::AtomicU64>().load(std::sync::atomic::Ordering::Acquire)
}

pub fn bump_membership_generation() {
    super::current_session().scoped::<MembershipGeneration, std::sync::atomic::AtomicU64>().fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

pub fn set_guestbook(
    community_id: &str,
    events: &[crate::community::v2::guestbook::GuestbookEvent],
    cursor_secs: u64,
) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    forget_console_verdict(community_id);
    let conn = super::get_write_connection_guard_static()?;
    let json = serde_json::to_string(events).map_err(|e| e.to_string())?;
    let enc = enc_txt(&json)?;
    conn.execute(
        "INSERT INTO community_guestbook (community_id, events, cursor_secs)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(community_id) DO UPDATE SET events=?2, cursor_secs=?3",
        params![community_id, enc, cursor_secs as i64],
    )
    .map_err(|e| format!("save guestbook: {e}"))?;
    let map = standing_cache();
    let mut cache = map.write().unwrap_or_else(|e| e.into_inner());
    let entry = cache.entry(community_id.to_string()).or_insert((0, None));
    *entry = (entry.0 + 1, None);
    bump_membership_generation();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// A unique, syntactically-valid test npub per call (bech32 charset, correct
    /// length). Uniqueness isolates each test's account DB so state can't bleed.
    fn make_test_npub(n: u32) -> String {
        const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
        let mut payload = vec![b'q'; 58];
        let mut x = n as u64;
        let mut i = 58;
        while x > 0 && i > 0 {
            i -= 1;
            payload[i] = BECH32[(x as usize) % 32];
            x /= 32;
        }
        format!("npub1{}", std::str::from_utf8(&payload).unwrap())
    }

    fn init_test_db() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        crate::db::close_database();
        // Per-account row-id caches survive close_database; clear them so a stale entry from a prior
        // test's DB can't point into this fresh account's DB and FK-fail an insert.
        crate::db::clear_id_caches();
        let tmp = tempfile::tempdir().unwrap();
        let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let account = make_test_npub(n);
        std::fs::create_dir_all(tmp.path().join(&account)).unwrap();
        crate::db::set_app_data_dir(crate::db::shared_test_data_dir().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
    }

    #[test]
    fn a_channel_holds_its_newest_dated_wraps_up_to_the_cap() {
        let (_tmp, _guard) = init_test_db();
        let (cid, ch) = ("c".repeat(64), "d".repeat(64));
        for i in 0..MAX_HELD_WRAPS + 2 {
            hold_wrap(&cid, &ch, &format!("{i:064x}"), &format!("wrap {i}"), "", 1_000 + i as u64).unwrap();
        }
        hold_wrap(&cid, &"e".repeat(64), &"f".repeat(64), "another channel's", "", 1).unwrap();
        let held = held_wraps(&cid).unwrap();
        assert_eq!(held.len(), MAX_HELD_WRAPS + 1, "each channel caps on its own");
        assert!(!held.iter().any(|(id, ..)| *id == format!("{:064x}", 0) || *id == format!("{:064x}", 1)), "the oldest-dated go first");
        assert!(held.iter().any(|(_, _, wrap)| wrap == "wrap 501"), "opened as it was held");
        hold_wrap(&cid, &ch, &format!("{:064x}", 501), "again", "", 1).unwrap();
        assert_eq!(held_wraps(&cid).unwrap().iter().filter(|(id, ..)| *id == format!("{:064x}", 501)).count(), 1, "once");
        release_held_wrap(&format!("{:064x}", 501)).unwrap();
        assert!(held_wrap_ids(&[format!("{:064x}", 501), format!("{:064x}", 500)]) == std::collections::HashSet::from([format!("{:064x}", 500)]));
    }

    /// An unsaved v2 community fresh from genesis, holding one Public `#general` channel.
    fn v2_fixture(name: &str, relays: &[&str]) -> crate::community::v2::community::CommunityV2 {
        let owner = Keys::generate();
        let g = crate::community::v2::control::genesis(
            &owner,
            crate::community::v2::control::CommunityMetadata { name: name.into(), ..Default::default() },
            1_000,
        )
        .unwrap();
        let relays = relays.iter().map(|r| r.to_string()).collect();
        crate::community::v2::community::CommunityV2::from_genesis(&g, name, None, relays, 1_000)
    }

    /// A saved v2 community with the hex ids of it and its `#general` channel.
    /// A bundle this build can join, named `name`.
    fn v2_bundle(name: &str) -> String {
        let bundle = crate::community::v2::service::bundle_of(
            &v2_fixture(name, &["wss://r"]),
            crate::community::v2::service::BundleAudience::Link,
            None,
            None,
            None,
        );
        serde_json::to_string(&bundle).unwrap()
    }

    fn saved_v2(name: &str) -> (crate::community::v2::community::CommunityV2, String, String) {
        let c = v2_fixture(name, &["wss://r"]);
        save_community_v2(&c).unwrap();
        let (cid, ch) = (c.id().to_hex(), c.channels[0].id.to_hex());
        (c, cid, ch)
    }

    /// A Private channel holding its own key, to sit beside `#general`.
    fn private_channel(id: u8, key: u8) -> crate::community::v2::community::ChannelV2 {
        crate::community::v2::community::ChannelV2 {
            id: ChannelId([id; 32]),
            name: "staff".into(),
            private: true,
            key: Some([key; 32]),
            epoch: Epoch(0),
            voice: None,
            meta_custom: None,
            meta_extra: Default::default(),
            writers: None,
            sealed: None,
        }
    }

    /// A saved v2 community with its root and a Private channel's key archived, as the
    /// service leaves one. Returns it with the hex ids of it and the Private channel.
    fn archived_v2() -> (crate::community::v2::community::CommunityV2, String, String) {
        let mut c = v2_fixture("HQ", &["r1"]);
        c.channels.push(private_channel(0x61, 0x62));
        save_community_v2(&c).unwrap();
        let (cid, private) = (c.id().to_hex(), c.channels[1].id.to_hex());
        store_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 0, &c.community_root).unwrap();
        store_epoch_key(&cid, &private, 0, &[0x62; 32]).unwrap();
        (c, cid, private)
    }

    fn encrypted_community_with_messages(n: usize) -> String {
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let (_, cid, ch) = saved_v2("HQ");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for i in 0..n {
            let msg = crate::types::Message {
                id: format!("{i:064x}"),
                content: format!("message number {i}, a few words long"),
                at: 1_700_000_000_000 + i as u64 * 1000,
                npub: Some(make_test_npub(i as u32 % 7)),
                ..Default::default()
            };
            rt.block_on(crate::db::events::save_message(&ch, &msg)).unwrap();
        }
        cid
    }

    #[test]
    fn the_policy_window_and_recent_texts_open_sealed_rows() {
        let (_tmp, _guard) = init_test_db();
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::set_encryption_enabled(false);
                crate::state::ENCRYPTION_KEY.clear(&[]);
            }
        }
        let _vault = Vault;
        let cid = encrypted_community_with_messages(5);
        let stored: String = crate::db::get_db_connection_guard_static()
            .unwrap()
            .query_row("SELECT content FROM events WHERE content != '' LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert!(crate::crypto::looks_encrypted(&stored), "the rows read back are sealed on disk");
        let window = community_policy_messages(&cid, 100).unwrap();
        assert_eq!(window.len(), 5);
        assert_eq!(window[0].text, "message number 0, a few words long");
        let recent = community_recent_texts(&cid, 100).unwrap();
        assert_eq!(recent[0].2, "message number 4, a few words long");
    }

    #[test]
    fn an_authority_floor_takes_a_squatted_coordinate_and_keeps_it() {
        let (_tmp, _guard) = init_test_db();
        let (cid, entity) = ("f".repeat(64), "a".repeat(64));
        let head = |v: u64, b: u8, family: &str| set_family_head_at_epoch(&cid, &entity, v, &[b; 32], &[b; 32], &[b; 32], 0, family).unwrap();
        let held = || (get_edition_head(&cid, &entity).unwrap().unwrap(), edition_head_families(&cid).unwrap().get(&entity).cloned());
        head(50, 0xC0, FLOOR_CONTROL);
        head(1, 0xA1, FLOOR_AUTHORITY);
        assert_eq!(held(), ((1, [0xA1; 32]), Some("a".into())), "an authority head replaces a control row at any version");
        head(99, 0xC1, FLOOR_CONTROL);
        assert_eq!(held().0, (1, [0xA1; 32]), "a control head never takes it back");
        converge_family_head_at_epoch(&cid, &entity, 1, &[0xC2; 32], &[0xC2; 32], &[0xC2; 32], 0, Some(FLOOR_CONTROL)).unwrap();
        assert_eq!(held().0, (1, [0xA1; 32]), "nor converges it");
        rebase_edition_head_at_epoch(&cid, &entity, 0, &[0xC3; 32], &[0xC3; 32], &[0xC3; 32], 0, FLOOR_CONTROL).unwrap();
        assert_eq!(held().0, (1, [0xA1; 32]), "nor moves it down");
        head(1, 0xA0, FLOOR_AUTHORITY);
        assert_eq!(held().0, (1, [0xA1; 32]), "within the family the version still only advances");
        head(2, 0xA2, FLOOR_AUTHORITY);
        assert_eq!(held().0, (2, [0xA2; 32]));
    }

    #[test]
    fn edition_head_round_trips_and_upserts() {
        let (_tmp, _guard) = init_test_db();
        let cid = "f".repeat(64);
        let entity = "a".repeat(64);

        // No head yet → None (the next edition is genesis v1).
        assert_eq!(get_edition_head(&cid, &entity).unwrap(), None);

        // Set v1, read it back exactly.
        let h1 = [0x11u8; 32];
        set_edition_head(&cid, &entity, 1, &h1).unwrap();
        assert_eq!(get_edition_head(&cid, &entity).unwrap(), Some((1, h1)));

        // Upsert to v2 — the head advances in place (one row per (community, entity)).
        let h2 = [0x22u8; 32];
        set_edition_head(&cid, &entity, 2, &h2).unwrap();
        assert_eq!(get_edition_head(&cid, &entity).unwrap(), Some((2, h2)));

        // MONOTONIC: a lower-or-equal version write is a no-op — the refuse-downgrade floor never
        // rolls back, even against a stale or hostile rollback attempt.
        set_edition_head(&cid, &entity, 1, &[0xEEu8; 32]).unwrap();
        assert_eq!(get_edition_head(&cid, &entity).unwrap(), Some((2, h2)), "rollback to v1 ignored");
        set_edition_head(&cid, &entity, 2, &[0xEEu8; 32]).unwrap();
        assert_eq!(get_edition_head(&cid, &entity).unwrap(), Some((2, h2)), "equal version is a no-op too");

        // A different entity is tracked independently.
        let other = "b".repeat(64);
        assert_eq!(get_edition_head(&cid, &other).unwrap(), None);
    }

    #[test]
    fn a_floor_keeps_the_author_of_the_head_it_holds() {
        let (_tmp, _guard) = init_test_db();
        let cid = "c".repeat(64);
        let entity = "e".repeat(64);
        let author = |b: u8| [b; 32];
        let held = |cid: &str| get_all_edition_heads_full(cid).unwrap()[&entity];
        set_edition_head_at_epoch(&cid, &entity, 1, &[0x11; 32], &[0x01; 32], &author(0xA1), 0).unwrap();
        set_edition_head_at_epoch(&cid, &entity, 2, &[0x22; 32], &[0x02; 32], &author(0xB2), 0).unwrap();
        assert_eq!(held(&cid), (0, 2, [0x22; 32], Some([0x02; 32]), Some(author(0xB2))), "an advance records its own author");

        // Convergence is the caller's verdict: it replaces the held edition whatever the ids,
        // but only at the same version and epoch.
        converge_edition_head_at_epoch(&cid, &entity, 2, &[0x33; 32], &[0xFF; 32], &author(0xC3), 0).unwrap();
        assert_eq!(held(&cid), (0, 2, [0x33; 32], Some([0xFF; 32]), Some(author(0xC3))), "a higher id can win on authority");
        converge_edition_head_at_epoch(&cid, &entity, 3, &[0x44; 32], &[0x00; 32], &author(0xD4), 0).unwrap();
        converge_edition_head_at_epoch(&cid, &entity, 2, &[0x55; 32], &[0x00; 32], &author(0xE5), 1).unwrap();
        assert_eq!(held(&cid).2, [0x33; 32], "another version or epoch is never converged onto");

        // A Refounding's advance into a new epoch takes the new head's author too.
        set_edition_head_at_epoch(&cid, &entity, 1, &[0x66; 32], &[0x06; 32], &author(0xF6), 1).unwrap();
        assert_eq!(held(&cid), (1, 1, [0x66; 32], Some([0x06; 32]), Some(author(0xF6))));
    }

    #[test]
    fn guestbook_round_trips_events_and_cursor() {
        let (_tmp, _guard) = init_test_db();
        let member = nostr_sdk::prelude::Keys::generate();
        let ev = crate::community::v2::guestbook::GuestbookEvent { epoch: None, observed_at: None,
            rumor_id: [7u8; 32],
            entry: crate::community::v2::guestbook::GuestbookEntry::Join {
                member: member.public_key(),
                invited_by: Some(("creator".into(), "label".into())),
                at_ms: 1_000,
            },
        };
        let cid = "d".repeat(64);
        assert_eq!(get_guestbook(&cid).unwrap(), (Vec::new(), 0), "absent reads as empty at cursor 0");
        set_guestbook(&cid, std::slice::from_ref(&ev), 42).unwrap();
        let (events, cursor) = get_guestbook(&cid).unwrap();
        assert_eq!(events, vec![ev], "events round-trip through the encrypted blob");
        assert_eq!(cursor, 42);
    }

    #[test]
    fn v2_images_round_trip_and_read_as_community_images() {
        let (_tmp, _guard) = init_test_db();
        let owner = nostr_sdk::prelude::Keys::generate();
        let g = crate::community::v2::control::genesis(
            &owner,
            crate::community::v2::control::CommunityMetadata { name: "Icons".into(), ..Default::default() },
            1_000,
        )
        .unwrap();
        let mut c = crate::community::v2::community::CommunityV2::from_genesis(&g, "Icons", None, vec!["wss://r".into()], 1_000);
        let mut extra = serde_json::Map::new();
        extra.insert("ext".into(), serde_json::Value::String("webp".into()));
        c.icon = Some(crate::community::v2::control::ImageRef {
            url: "https://blossom.example/abc".into(),
            key: "0".repeat(64),
            nonce: "1".repeat(32),
            hash: "a".repeat(64),
            extra,
        });
        c.meta_custom = Some({
            let mut m = serde_json::Map::new();
            m.insert("k".into(), serde_json::Value::from("v"));
            m
        });
        c.channels[0].voice = Some(true);
        c.channels[0].meta_extra.insert("vnd".into(), serde_json::Value::from(7));
        save_community_v2(&c).unwrap();

        // The v2 loader round-trips the ImageRef exactly (extra included).
        let re = load_community_v2(c.id()).unwrap().unwrap();
        assert_eq!(re.icon, c.icon);
        assert_eq!(re.banner, None);
        // The CORD-02 §6 stash survives the encrypted envelope columns.
        assert_eq!(re.meta_custom, c.meta_custom);
        assert_eq!(re.channels[0].voice, Some(true));
        assert_eq!(re.channels[0].meta_extra.get("vnd"), Some(&serde_json::Value::from(7)));

        // The stored JSON parses as a CommunityImage (`ext` from the flattened extra),
        // so `community_image` serves it from the column alone.
        let cid = crate::simd::hex::bytes_to_hex_32(&c.id().0);
        let img = community_image(&cid, false).unwrap().expect("the v2 icon reads as a CommunityImage");
        assert_eq!(img.url, "https://blossom.example/abc");
        assert_eq!(img.ext, "webp");
        assert_eq!(img.hash, "a".repeat(64));
        assert_eq!(community_image(&cid, true).unwrap(), None);
        assert_eq!(community_image(&"cd".repeat(32), false).unwrap(), None);
    }

    #[test]
    fn column_reads_and_the_owner_check_serve_a_v2_community() {
        use crate::community::roles::{CommunityRoles, MemberGrant, Permissions, Role, RoleScope};
        use crate::community::moderation::is_owner_or_admin;
        let (_tmp, _guard) = init_test_db();
        let owner = nostr_sdk::prelude::Keys::generate();
        let g = crate::community::v2::control::genesis(
            &owner,
            crate::community::v2::control::CommunityMetadata { name: "Two".into(), ..Default::default() },
            1_000,
        )
        .unwrap();
        let v2 = crate::community::v2::community::CommunityV2::from_genesis(&g, "Two", None, vec!["wss://two".into()], 1_000);
        save_community_v2(&v2).unwrap();
        let mut other = v2_fixture("One", &["wss://one"]);
        other.channels.push(private_channel(0x71, 0x72));
        save_community_v2(&other).unwrap();

        crate::db::get_write_connection_guard_static()
            .unwrap()
            .execute(
                "INSERT INTO community_channels (channel_id, community_id, channel_key, epoch, name, created_at)
                 VALUES (?1, ?2, x'00', 0, 'c', 1)",
                params!["ee".repeat(32), "ff".repeat(32)],
            )
            .unwrap();
        let channels = held_channel_ids().unwrap();
        assert!(!channels.contains(&"ee".repeat(32)), "a channel whose community is gone is not held");
        assert!(channels.contains(&crate::simd::hex::bytes_to_hex_32(&v2.channels[0].id.0)));
        assert!(other.channels.iter().all(|ch| channels.contains(&ch.id.to_hex())));
        assert_eq!(channels.len(), other.channels.len() + v2.channels.len());
        let relays = held_community_relays().unwrap();
        assert!(relays.contains("wss://two") && relays.contains("wss://one"));

        let cid = crate::simd::hex::bytes_to_hex_32(&v2.id().0);
        let (admin, social, stranger) = (Keys::generate().public_key(), Keys::generate().public_key(), Keys::generate().public_key());
        let role = |id: &str, perms: u64| Role {
            role_id: id.repeat(32), name: id.into(), position: 1, permissions: Permissions(perms),
            scope: RoleScope::Server, color: 0, rank: None, display: false, mentionable: false, extra: Default::default(),
        };
        let roles = CommunityRoles {
            roles: vec![role("aa", Permissions::ADMIN_ALL), role("bb", 0)],
            grants: vec![
                MemberGrant { member: admin.to_hex(), role_ids: vec!["aa".repeat(32)] },
                MemberGrant { member: social.to_hex(), role_ids: vec!["bb".repeat(32)] },
            ],
            everyone: None,
            overwrites: Vec::new(), syncs: Default::default(), legacy_channels: None, creators: Default::default(),
        };
        set_community_roles(&cid, &roles, 1).unwrap();
        assert!(is_owner_or_admin(&cid, &owner.public_key().to_hex()), "a v2 owner proves itself from the id");
        assert!(is_owner_or_admin(&cid, &admin.to_hex()));
        assert!(!is_owner_or_admin(&cid, &social.to_hex()), "a role without management is not admin");
        assert!(!is_owner_or_admin(&cid, &stranger.to_hex()));
        assert!(!is_owner_or_admin(&"cd".repeat(32), &owner.public_key().to_hex()));
    }

    #[test]
    fn epoch_key_archive_retains_every_epoch() {
        // a member who lived through a rotation must keep OLD epoch keys. Storing a new
        // epoch's key must NOT clobber a prior one (the data-loss bug the archive fixes).
        let (_tmp, _guard) = init_test_db();
        let cid = "f".repeat(64);
        let scope = "a".repeat(64);

        store_epoch_key(&cid, &scope, 0, &[0xA0u8; 32]).unwrap();
        store_epoch_key(&cid, &scope, 1, &[0xA1u8; 32]).unwrap();
        store_epoch_key(&cid, &scope, 2, &[0xA2u8; 32]).unwrap();

        let held = held_epoch_keys(&cid, &scope).unwrap();
        assert_eq!(held.len(), 3, "all three epoch keys retained");
        assert_eq!(held[0], (Epoch(0), [0xA0u8; 32]));
        assert_eq!(held[1], (Epoch(1), [0xA1u8; 32]));
        assert_eq!(held[2], (Epoch(2), [0xA2u8; 32]));

        // Point lookup by epoch (what the open path uses to select a decryption key).
        assert_eq!(held_epoch_key(&cid, &scope, 1).unwrap(), Some([0xA1u8; 32]));
        assert_eq!(held_epoch_key(&cid, &scope, 9).unwrap(), None, "unheld epoch is None");

        // Same coordinate REPLACE = fork-resolution committing a winning key (only legit overwrite).
        store_epoch_key(&cid, &scope, 1, &[0xBBu8; 32]).unwrap();
        assert_eq!(held_epoch_key(&cid, &scope, 1).unwrap(), Some([0xBBu8; 32]));
        assert_eq!(held_epoch_keys(&cid, &scope).unwrap().len(), 3, "replace didn't add a row");

        // A different scope is isolated (server-root vs a channel share the table, never collide) —
        // at both the list AND the point-lookup level.
        assert!(held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty());
        assert_eq!(
            held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 1).unwrap(),
            None,
            "epoch 1 under a different scope is not the channel's key"
        );
    }

    #[test]
    fn at_rest_encryption_wraps_keys_and_metadata_on_disk() {
        let (_tmp, _guard) = init_test_db();
        // Local Encryption ON with a known vault key (the db-test guard serializes, so toggling these
        // globals is safe; reset at the end). `others: &[]` — the slice only allocates a vault lane.
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);

        let mut c = v2_fixture("Secret HQ", &["wss://relay.example"]);
        c.community_root = [0x42u8; 32];
        c.description = Some("top secret".into());
        c.channels.push(private_channel(0x61, 0x62));
        save_community_v2(&c).unwrap();
        let cid = c.id().to_hex();
        store_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 0, &c.community_root).unwrap();
        set_community_banlist(&cid, &["deadbeef".repeat(8)], 1).unwrap();

        // On disk: secrets are 60-byte ciphertext (12 nonce + 32 + 16 tag), NOT raw 32-byte keys;
        // identifying text is hex ciphertext, never the plaintext.
        {
            let conn = crate::db::get_db_connection_guard_static().unwrap();
            let (root_len, control_root_len, name, relays, description): (i64, i64, String, String, String) = conn
                .query_row(
                    "SELECT length(server_root_key), length(control_root), name, relays, description
                     FROM communities WHERE community_id = ?1",
                    params![cid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .unwrap();
            assert_eq!(root_len, 60, "server_root_key must be ciphertext, not a raw 32-byte key");
            assert_eq!(control_root_len, 60, "control_root must be ciphertext, not a raw 32-byte key");
            for (column, stored, plain) in [
                ("name", &name, "Secret HQ"),
                ("relays", &relays, r#"["wss://relay.example"]"#),
                ("description", &description, "top secret"),
            ] {
                assert_ne!(stored, plain, "{column} must not be plaintext on disk");
                assert!(crate::crypto::looks_encrypted(stored), "{column} column is ciphertext");
            }
            let (owner_pk, owner_salt, control_pk): (String, String, String) = conn
                .query_row(
                    "SELECT owner_pubkey, owner_salt, control_pk FROM communities WHERE community_id = ?1",
                    params![cid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            for (column, stored, plain) in [
                ("owner_pubkey", &owner_pk, crate::simd::hex::bytes_to_hex_32(&c.identity.owner_xonly)),
                ("owner_salt", &owner_salt, crate::simd::hex::bytes_to_hex_32(&c.identity.owner_salt)),
                ("control_pk", &control_pk, c.control_pk.unwrap().to_hex()),
            ] {
                assert_ne!(*stored, plain, "{column} must not be plaintext on disk");
                assert!(crate::crypto::looks_encrypted(stored), "{column} column is ciphertext");
            }
            let mut stmt = conn
                .prepare("SELECT length(channel_key), name FROM community_channels WHERE community_id = ?1")
                .unwrap();
            let channels: Vec<(i64, String)> =
                stmt.query_map(params![cid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
            assert_eq!(channels.len(), 2);
            for (key_len, name) in &channels {
                assert_eq!(*key_len, 60, "channel_key must be ciphertext, Public placeholder and Private key alike");
                assert!(crate::crypto::looks_encrypted(name) && name != "general" && name != "staff", "channel name is ciphertext");
            }
            // The banlist lives in `community_bans`, sealed; SQL filters are handed the opened list.
            let banned_npub: String = conn
                .query_row("SELECT npub FROM community_bans WHERE community_id = ?1 LIMIT 1", params![cid], |r| r.get(0))
                .unwrap();
            assert!(crate::crypto::looks_encrypted(&banned_npub), "who is banned is sealed on disk, got {banned_npub}");
            let opened = crate::crypto::maybe_decrypt_text(&banned_npub);
            assert!(opened.starts_with("npub1"), "and opens to the form `events.npub` uses");
            let key_len: i64 = conn
                .query_row(
                    "SELECT length(key) FROM community_epoch_keys WHERE community_id = ?1 LIMIT 1",
                    params![cid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(key_len, 60, "epoch-archive key must be ciphertext");
        }

        // In memory: load decrypts everything back to the originals.
        let loaded = load_community_v2(c.id()).unwrap().unwrap();
        assert_eq!(loaded.name, "Secret HQ");
        assert_eq!(loaded.description.as_deref(), Some("top secret"));
        assert_eq!(loaded.community_root, [0x42u8; 32]);
        assert_eq!(loaded.control_root, c.control_root);
        assert_eq!(loaded.control_pk, c.control_pk);
        assert_eq!(loaded.identity.owner_xonly, c.identity.owner_xonly);
        assert_eq!(loaded.identity.owner_salt, c.identity.owner_salt);
        assert_eq!(loaded.relays, vec!["wss://relay.example".to_string()]);
        assert_eq!(loaded.channels[0].name, "general");
        assert_eq!(loaded.channels[1].key, Some([0x62u8; 32]));
        assert_eq!(held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 0).unwrap(), Some([0x42u8; 32]));
        assert_eq!(get_community_banlist(&cid).unwrap(), vec!["deadbeef".repeat(8)]);

        crate::state::set_encryption_enabled(false);
        crate::state::ENCRYPTION_KEY.clear(&[]);
    }

    #[test]
    fn an_encrypted_v2_community_loads_back_whole() {
        let (_tmp, _guard) = init_test_db();
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::set_encryption_enabled(false);
                crate::state::ENCRYPTION_KEY.clear(&[]);
            }
        }
        let _vault = Vault;
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);

        let owner = nostr_sdk::prelude::Keys::generate();
        let g = crate::community::v2::control::genesis(
            &owner,
            crate::community::v2::control::CommunityMetadata { name: "Sealed".into(), ..Default::default() },
            1_000,
        )
        .unwrap();
        let mut v2 = crate::community::v2::community::CommunityV2::from_genesis(&g, "Sealed", Some("about".into()), vec!["wss://r".into()], 1_000);
        let mut private = v2.channels[0].clone();
        private.id = ChannelId([0x61; 32]);
        private.name = "staff".into();
        private.private = true;
        private.key = Some([0x62; 32]);
        v2.channels.push(private);
        save_community_v2(&v2).unwrap();

        let name: String = crate::db::get_db_connection_guard_static()
            .unwrap()
            .query_row("SELECT name FROM communities WHERE community_id = ?1", params![v2.id().to_hex()], |r| r.get(0))
            .unwrap();
        assert!(crate::crypto::looks_encrypted(&name), "sealed on disk");
        let loaded = load_community_v2(v2.id()).unwrap().unwrap();
        assert_eq!(format!("{loaded:?}"), format!("{v2:?}"), "every field opens back to what was saved");
    }

    #[test]
    fn at_rest_decrypt_tolerates_a_pre_migration_plaintext_row() {
        // A row written BEFORE the at-rest pass (raw key + plaintext text) must still read back once
        // encryption is on — the 32-vs-60 byte + `looks_encrypted` discriminators handle the mixed DB.
        let (_tmp, _guard) = init_test_db();
        crate::state::set_encryption_enabled(false);
        let mut c = v2_fixture("Legacy HQ", &[]);
        c.community_root = [0x42u8; 32];
        c.channels.push(private_channel(0x61, 0x62));
        save_community_v2(&c).unwrap();

        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let loaded = load_community_v2(c.id()).unwrap().unwrap();
        assert_eq!(loaded.name, "Legacy HQ", "plaintext name reads through");
        assert_eq!(loaded.community_root, [0x42u8; 32], "raw 32-byte key reads through");
        assert_eq!(loaded.control_root, c.control_root, "raw 32-byte control root reads through");
        assert_eq!(loaded.channels[1].key, Some([0x62u8; 32]), "raw 32-byte channel key reads through");

        crate::state::set_encryption_enabled(false);
        crate::state::ENCRYPTION_KEY.clear(&[]);
    }

    #[test]
    fn large_epoch_round_trips_losslessly() {
        // Epoch >= 2^63 stored as i64 then reinterpreted as u64 must be exact.
        let (_tmp, _guard) = init_test_db();
        let mut c = v2_fixture("HQ", &[]);
        c.root_epoch = Epoch(u64::MAX - 3);
        c.channels[0].epoch = Epoch(u64::MAX - 7);
        save_community_v2(&c).unwrap();
        let loaded = load_community_v2(c.id()).unwrap().unwrap();
        assert_eq!(loaded.root_epoch, Epoch(u64::MAX - 3));
        assert_eq!(loaded.channels[0].epoch, Epoch(u64::MAX - 7));
    }

    #[test]
    fn malformed_channel_id_row_errors_not_corrupts() {
        // A corrupted (short/non-hex) channel_id must error on load, not silently
        // reconstruct a wrong-but-self-consistent id.
        let (_tmp, _guard) = init_test_db();
        let (c, cid, _) = saved_v2("HQ");
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO community_channels
                    (channel_id, community_id, channel_key, epoch, name, created_at)
                 VALUES (?1, ?2, ?3, 0, 'bad', 0)",
                rusqlite::params!["zz_not_hex", cid, &[0u8; 32][..]],
            )
            .unwrap();
        }
        assert!(load_community_v2(c.id()).is_err(), "malformed id must error, not corrupt");
    }

    #[test]
    fn message_key_store_take_round_trip() {
        let (_tmp, _guard) = init_test_db();
        let eph = Keys::generate();
        let relays = vec!["wss://r.one".to_string()];
        // Keyed by INNER message id; resolves to the OUTER event id + key + relays.
        store_message_key("inner_msg_id", "outer_evid", &eph, &relays).unwrap();

        let (loaded, outer, r) = take_message_key("inner_msg_id").unwrap().expect("present");
        assert_eq!(
            loaded.secret_key().as_secret_bytes(),
            eph.secret_key().as_secret_bytes()
        );
        assert_eq!(outer, "outer_evid");
        assert_eq!(r, relays);
        // `take` is single-use: the row is removed.
        assert!(take_message_key("inner_msg_id").unwrap().is_none());
    }

    #[test]
    fn missing_community_is_none() {
        let (_tmp, _guard) = init_test_db();
        let absent = CommunityId([0x33u8; 32]);
        assert!(load_community_v2(&absent).unwrap().is_none());
    }

    #[test]
    fn list_ids_reflects_saved() {
        let (_tmp, _guard) = init_test_db();
        let (a, _, _) = saved_v2("A");
        let (b, _, _) = saved_v2("B");
        let ids = list_community_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(a.id()) && ids.contains(b.id()));
    }

    #[test]
    fn delete_community_clears_all_local_state() {
        let (_tmp, _guard) = init_test_db();
        let (c, cid, private) = archived_v2();
        save_public_invite(&"ab".repeat(32), &cid, "url", None, None).unwrap();
        save_pending_invite(&"cd".repeat(32), "{}", "npub1x", 0).unwrap();
        set_edition_head(&cid, &"a".repeat(64), 3, &[0x11u8; 32]).unwrap();
        assert!(!held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty());
        assert!(!held_epoch_keys(&cid, &private).unwrap().is_empty());

        delete_community(&cid).unwrap();
        assert!(!community_exists(c.id()).unwrap());
        assert!(community_id_for_channel(&c.channels[0].id.to_hex()).unwrap().is_none());
        assert!(community_id_for_channel(&private).unwrap().is_none());
        assert!(list_public_invites(&cid).unwrap().is_empty());
        assert_eq!(get_edition_head(&cid, &"a".repeat(64)).unwrap(), None, "edition heads cleared on delete");
        assert!(held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty(), "epoch keys cleared on delete");
        assert!(held_epoch_keys(&cid, &private).unwrap().is_empty(), "channel epoch keys cleared on delete");
    }

    #[test]
    fn delete_community_retain_keys_drops_state_but_keeps_epoch_keys() {
        // self-removal teardown: drop chat/membership/control state but KEEP the held epoch keys so a
        // later self-scrub of own past messages stays possible.
        let (_tmp, _guard) = init_test_db();
        let (c, cid, private) = archived_v2();
        save_public_invite(&"ab".repeat(32), &cid, "url", None, None).unwrap();
        set_edition_head(&cid, &"a".repeat(64), 3, &[0x11u8; 32]).unwrap();

        let base_before = held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap();
        let chan_before = held_epoch_keys(&cid, &private).unwrap();
        assert!(!base_before.is_empty() && !chan_before.is_empty(), "base + channel keys archived");

        delete_community_retain_keys(&cid).unwrap();

        // State is gone.
        assert!(!community_exists(c.id()).unwrap());
        assert!(community_id_for_channel(&c.channels[0].id.to_hex()).unwrap().is_none());
        assert!(community_id_for_channel(&private).unwrap().is_none());
        assert!(list_public_invites(&cid).unwrap().is_empty());
        assert_eq!(get_edition_head(&cid, &"a".repeat(64)).unwrap(), None);
        // Epoch keys (base + channel, every epoch) survive intact.
        assert_eq!(held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap(), base_before,
            "base epoch keys retained for self-scrub");
        assert_eq!(held_epoch_keys(&cid, &private).unwrap(), chan_before,
            "channel epoch keys retained for self-scrub");
    }

    #[test]
    fn channel_resolves_to_owning_community() {
        let (_tmp, _guard) = init_test_db();
        let (_, cid, chan) = saved_v2("HQ");
        assert_eq!(community_id_for_channel(&chan).unwrap().as_deref(), Some(cid.as_str()));
        assert!(community_id_for_channel(&"ff".repeat(32)).unwrap().is_none());
    }

    #[test]
    fn the_channel_cache_answers_in_bytes_and_hex_alike() {
        let (_tmp, _guard) = init_test_db();
        let (c, cid, chan) = saved_v2("Bytes");
        let ch = c.channels[0].id.0;
        // A miss fills the cache; the hit after it answers the same, in either form.
        assert_eq!(community_for_channel(&ch).unwrap(), Some(c.id().0));
        assert_eq!(channel_community_cache().read().unwrap().get(&ch), Some(&c.id().0), "the miss filled the cache");
        assert_eq!(community_for_channel(&ch).unwrap(), Some(c.id().0));
        assert_eq!(community_id_for_channel(&chan).unwrap().as_deref(), Some(cid.as_str()));
        // Only the exact stored spelling resolves, cached or not.
        assert_eq!(community_id_for_channel(&chan.to_uppercase()).unwrap(), None);
        assert_eq!(community_for_channel(&[0xFF; 32]).unwrap(), None);
        assert!(channel_community_cache().read().unwrap().get(&[0xFF; 32]).is_none(), "a miss is never cached");
        // Deleting the community stops both forms resolving.
        delete_community_retain_keys(&cid).unwrap();
        assert_eq!(community_for_channel(&ch).unwrap(), None);
        assert_eq!(community_id_for_channel(&chan).unwrap(), None);
    }

    #[test]
    fn communities_that_ban_nobody_share_one_empty_set() {
        let (_tmp, _guard) = init_test_db();
        let (a, a_hex, _) = saved_v2("Calm");
        let (b, b_hex, _) = saved_v2("Quiet");
        let (ea, eb) = (banned_set_of(&a.id().0), banned_set(&b_hex));
        assert!(ea.is_empty() && std::sync::Arc::ptr_eq(&ea, &eb), "every empty banlist is the one shared set");

        let villain = Keys::generate().public_key();
        set_community_banlist(&a_hex, &[villain.to_hex()], 10).unwrap();
        assert!(is_author_banned(&a_hex, &villain) && is_author_banned_in(&a.id().0, &villain));
        assert!(!is_author_banned_in(&b.id().0, &villain), "a ban stays in its own community");
        assert!(!std::sync::Arc::ptr_eq(&banned_set_of(&a.id().0), &empty_ban_set()));

        set_community_banlist(&a_hex, &[], 11).unwrap();
        assert!(std::sync::Arc::ptr_eq(&banned_set_of(&a.id().0), &empty_ban_set()), "an emptied banlist returns to the shared set");
        assert!(!is_author_banned_in(&a.id().0, &villain));
    }

    #[test]
    fn community_exists_reflects_saved() {
        let (_tmp, _guard) = init_test_db();
        let c = v2_fixture("A", &[]);
        assert!(!community_exists(c.id()).unwrap());
        save_community_v2(&c).unwrap();
        assert!(community_exists(c.id()).unwrap());
    }

    #[test]
    fn a_block_drops_only_that_persons_parked_invites() {
        let (_tmp, _guard) = init_test_db();
        let (spam, friend) = ("cd".repeat(32), "ef".repeat(32));
        save_pending_invite(&spam, &v2_bundle("Spam"), "npub1spammer", 0).unwrap();
        save_pending_invite(&friend, &v2_bundle("Friends"), "npub1friend", 0).unwrap();
        assert_eq!(delete_pending_invites_from("npub1spammer").unwrap(), 1);
        let left: Vec<String> = list_pending_invites().unwrap().into_iter().map(|i| i.community_id).collect();
        assert_eq!(left, vec![friend], "the friend's invite stays");
    }

    #[test]
    fn pending_invite_first_wins_and_round_trips() {
        let (_tmp, _guard) = init_test_db();
        let cid = "ab".repeat(32);
        let (first, second) = (v2_bundle("First"), v2_bundle("Second"));
        // First park inserts; a re-invite for the same id is IGNORED (first-wins, so a
        // hostile re-send can't rewrite a parked bundle or re-notify).
        assert!(save_pending_invite(&cid, &first, "npub1inviter", 0).unwrap());
        assert!(!save_pending_invite(&cid, &second, "npub1other", 0).unwrap());
        assert!(pending_invite_exists(&cid).unwrap());

        let listed = list_pending_invites().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].community_id, cid);
        assert_eq!(listed[0].bundle_json, first, "original bundle preserved");
        assert_eq!(listed[0].inviter_npub, "npub1inviter");

        // get is non-destructive; delete then removes it.
        assert_eq!(get_pending_invite(&cid).unwrap(), Some(first.clone()));
        assert!(pending_invite_exists(&cid).unwrap(), "get must not delete");
        delete_pending_invite(&cid).unwrap();
        assert!(!pending_invite_exists(&cid).unwrap());
        assert!(get_pending_invite(&cid).unwrap().is_none());
    }

    #[test]
    fn purge_drops_invites_for_held_communities_only() {
        let (_tmp, _guard) = init_test_db();
        // A community we hold + a parked invite for it (the cross-device race: invite landed
        // before the membership list rehydrated the community).
        let (_, held_hex, _) = saved_v2("Held");
        save_pending_invite(&held_hex, "{\"bundle\":1}", "npub1inviter", 0).unwrap();
        // An invite for a community we do NOT hold must survive the purge.
        let stranger = "ab".repeat(32);
        save_pending_invite(&stranger, "{\"bundle\":2}", "npub1inviter", 0).unwrap();

        let n = purge_pending_invites_for_held_communities().unwrap();
        assert_eq!(n, 1, "only the held community's invite is purged");
        assert!(!pending_invite_exists(&held_hex).unwrap(), "held → invite gone");
        assert!(pending_invite_exists(&stranger).unwrap(), "unknown community → invite kept");
    }

    #[test]
    fn decline_drops_pending_invite() {
        let (_tmp, _guard) = init_test_db();
        let cid = "cd".repeat(32);
        save_pending_invite(&cid, "{}", "npub1x", 0).unwrap();
        delete_pending_invite(&cid).unwrap();
        assert!(!pending_invite_exists(&cid).unwrap());
    }

    #[test]
    fn pending_invites_are_capped_keeping_the_newest() {
        let (_tmp, _guard) = init_test_db();
        // 150 distinct invites with strictly increasing received_at (the helper stamps now_secs(),
        // so vary the id and rely on insertion order; to make ordering deterministic we bump the
        // stored time directly after each insert isn't needed — received_at ties break on id DESC).
        // Insert 150; the table must cap at 100.
        let bundle = v2_bundle("Flood");
        for i in 0..150u32 {
            let cid = format!("{:064x}", i);
            save_pending_invite(&cid, &bundle, "npub1x", 0).unwrap();
        }
        let all = list_pending_invites().unwrap();
        assert_eq!(all.len(), 100, "table capped at MAX_PENDING_INVITES");
        // A spam flood can't grow it past the cap regardless of how many arrive.
        for i in 150..400u32 {
            let cid = format!("{:064x}", i);
            save_pending_invite(&cid, &bundle, "npub1x", 0).unwrap();
        }
        assert_eq!(list_pending_invites().unwrap().len(), 100, "cap holds under flood");
    }

    /// A parked invite past its sender-declared NIP-40 deadline is invisible to BOTH reads:
    /// the list (so it stops cluttering) and the peek the accept path uses (so it can't be
    /// redeemed). Relays only stop delivering an expired invite; the already-parked row is
    /// ours to enforce.
    #[test]
    fn expired_parked_invites_are_hidden_from_list_and_accept() {
        let (_tmp, _guard) = init_test_db();
        let now = now_secs();
        let live = "aa".repeat(32);
        let expired = "bb".repeat(32);
        let permanent = "cc".repeat(32);

        save_pending_invite(&live, &v2_bundle("Live"), "npub1x", now + 3600).unwrap();
        save_pending_invite(&expired, &v2_bundle("Dead"), "npub1x", now - 1).unwrap();
        // 0 = the sender declared no deadline (a pre-expiry client): stays permanent, because
        // an invite whose sender never promised a deadline isn't ours to revoke.
        save_pending_invite(&permanent, &v2_bundle("Forever"), "npub1x", 0).unwrap();

        let listed: Vec<String> = list_pending_invites().unwrap().into_iter().map(|i| i.community_id).collect();
        assert!(listed.contains(&live), "an unexpired invite still lists");
        assert!(listed.contains(&permanent), "a no-deadline invite still lists");
        assert!(!listed.contains(&expired), "an expired invite is hidden from the list");

        assert!(get_pending_invite(&live).unwrap().is_some());
        assert!(get_pending_invite(&permanent).unwrap().is_some());
        assert!(
            get_pending_invite(&expired).unwrap().is_none(),
            "an expired invite must not be redeemable"
        );

        // The row still exists until swept, and the sweep reclaims exactly the expired one.
        assert!(pending_invite_exists(&expired).unwrap(), "hidden, not yet deleted");
        assert_eq!(purge_expired_pending_invites().unwrap(), 1);
        assert!(!pending_invite_exists(&expired).unwrap());
        assert!(pending_invite_exists(&live).unwrap(), "the sweep spares live invites");
        assert!(pending_invite_exists(&permanent).unwrap(), "and no-deadline ones");
    }

    /// The expiry is per-invite from the SENDER's tag, not derived from local receive time,
    /// so a short-fuse invite expires on the sender's schedule even if it was just received.
    #[test]
    fn expiry_follows_the_senders_deadline_not_receipt_time() {
        let (_tmp, _guard) = init_test_db();
        let cid = "de".repeat(32);
        // Received right now, but the sender's deadline already passed (a stale wrap that a
        // NIP-40-ignoring relay still served).
        save_pending_invite(&cid, "{}", "npub1x", now_secs() - 10).unwrap();
        assert!(list_pending_invites().unwrap().is_empty(), "receipt time does not extend the deadline");
        assert!(get_pending_invite(&cid).unwrap().is_none());
    }

    /// A legacy banlist blob is adopted into `community_bans` on first read, and the blob is retired
    /// without tripping the column's NOT NULL: a failed adoption read as "nobody is banned".
    #[test]
    fn a_legacy_banlist_is_adopted_on_first_read() {
        let (_tmp, _guard) = init_test_db();
        let (_, cid, _) = saved_v2("Legacy bans");
        let banned = "ab".repeat(32);
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute("DELETE FROM community_bans WHERE community_id = ?1", params![cid]).unwrap();
            conn.execute("UPDATE communities SET banlist = ?1 WHERE community_id = ?2", params![format!("[\"{banned}\"]"), cid]).unwrap();
        }
        assert_eq!(get_community_banlist(&cid).unwrap(), vec![banned.clone()]);
        assert_eq!(get_community_banlist(&cid).unwrap(), vec![banned], "and stays adopted");
    }

    /// A task of a swapped-out account can't seal into its own store: not as plaintext right after
    /// the swap, and not under the next account's key once that one has unlocked.
    #[test]
    fn a_task_of_a_swapped_out_account_cannot_seal() {
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        const KA: [u8; 32] = [0x61; 32];
        const KB: [u8; 32] = [0x62; 32];
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        let account_a = crate::db::get_current_account().unwrap();
        crate::db::set_sql_setting("encryption_enabled".to_string(), "true".to_string()).unwrap();
        crate::state::ENCRYPTION_KEY.set(KA, &[]);
        crate::state::set_encryption_enabled(true);
        let (_, cid, ch) = saved_v2("A");

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let late_write = |label: &'static str| {
            let (go, wait) = tokio::sync::oneshot::channel::<()>();
            let (cid, ch) = (cid.clone(), ch.clone());
            let task = {
                let _rt = rt.enter();
                crate::db::spawn_bound(async move {
                    let _ = wait.await;
                    set_community_pins(&cid, &ch, &format!("{{\"{label}\":1}}"), 1)
                })
            };
            (go, task)
        };
        let (go_plain, plain) = late_write("plain");
        let (go_foreign, foreign) = late_write("foreign");

        // The swap: the vault and the flag drop first, then account B opens and unlocks.
        let account_b = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        crate::state::ENCRYPTION_KEY.clear(&[]);
        crate::state::set_encryption_enabled(false);
        crate::db::set_current_account(account_b.clone()).unwrap();
        crate::db::init_database(&account_b).unwrap();
        go_plain.send(()).unwrap();
        assert!(rt.block_on(plain).unwrap().is_err(), "no plaintext into an encrypted store");
        crate::state::ENCRYPTION_KEY.set(KB, &[]);
        crate::state::set_encryption_enabled(true);
        go_foreign.send(()).unwrap();
        assert!(rt.block_on(foreign).unwrap().is_err(), "no seal under another account's key");

        crate::db::set_current_account(account_a.clone()).unwrap();
        crate::db::init_database(&account_a).unwrap();
        crate::state::ENCRYPTION_KEY.set(KA, &[]);
        assert_eq!(get_community_pins(&cid, &ch).unwrap(), None, "account A's store is untouched");
    }

    /// A legacy-salted account moves onto its own salt at its first unlock: everything reads back,
    /// nothing anywhere still opens under the legacy key, and the derivation is recorded. A wrong
    /// key or a biometric account changes nothing.
    #[test]
    fn a_legacy_account_moves_onto_its_own_salt_at_unlock() {
        use crate::crypto::{derive_key, Kdf};
        use crate::db::at_rest;
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let pin = "123456";
        let legacy = rt.block_on(derive_key(pin, &Kdf::Legacy));
        crate::state::ENCRYPTION_KEY.set(legacy, &[]);
        crate::state::set_encryption_enabled(true);
        crate::db::set_sql_setting("encryption_enabled".to_string(), "true".to_string()).unwrap();
        crate::db::set_sql_setting("security_type".to_string(), "pin".to_string()).unwrap();
        crate::db::set_sql_setting("last_vacuum".to_string(), "1".to_string()).unwrap();
        crate::db::set_pkey(&crate::crypto::encrypt_with_key("nsec1legacyaccount", &legacy).unwrap()).unwrap();
        let mut community = v2_fixture("Old", &["wss://r"]);
        community.channels.push(private_channel(0x61, 0x62));
        save_community_v2(&community).unwrap();
        let (cid, ch) = (community.id().to_hex(), community.channels[0].id.to_hex());
        set_community_pins(&cid, &ch, r#"{"pins":["old"]}"#, 1).unwrap();
        let dm_chat = make_test_npub(900_002);
        let dm = crate::types::Message { id: "e1".repeat(32), content: "said long ago".into(), at: 1_600_000_000_000, mine: true, ..Default::default() };
        rt.block_on(crate::db::events::save_message(&dm_chat, &dm)).unwrap();
        let dm_int = crate::db::id_cache::get_or_create_chat_id(&dm_chat).unwrap();
        let read = || {
            (
                get_community_pins(&cid, &ch).unwrap(),
                load_community_v2(community.id()).unwrap().map(|c| format!("{c:?}")),
                rt.block_on(crate::db::events::get_message_views(dm_int, 10, 0)).unwrap().into_iter().map(|m| m.content).collect::<Vec<_>>(),
            )
        };
        let before = read();
        assert_eq!(before.1, Some(format!("{community:?}")));
        assert_eq!(before.2, vec!["said long ago".to_string()]);

        assert!(!rt.block_on(at_rest::upgrade_after_unlock(pin, &[0x77; 32])).unwrap(), "a key that proves nothing moves nothing");
        assert_eq!(Kdf::of_account().unwrap(), Kdf::Legacy);
        crate::db::set_sql_setting("security_type".to_string(), "biometric".to_string()).unwrap();
        assert!(!rt.block_on(at_rest::upgrade_after_unlock(pin, &legacy)).unwrap(), "a biometric account stays put");
        crate::db::set_sql_setting("security_type".to_string(), "pin".to_string()).unwrap();
        assert_eq!(read(), before);

        // A failed upgrade leaves everything as it was and waits a day before the next attempt.
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute_batch("CREATE TABLE unaddressable (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;").unwrap();
            conn.execute("INSERT INTO unaddressable VALUES ('a', ?1)", [crate::crypto::encrypt_with_key("x", &legacy).unwrap()]).unwrap();
        }
        assert!(rt.block_on(at_rest::upgrade_after_unlock(pin, &legacy)).is_err());
        assert_eq!((Kdf::of_account().unwrap(), read()), (Kdf::Legacy, before.clone()), "nothing moved");
        assert_eq!(crate::state::ENCRYPTION_KEY.get(), Some(legacy));
        assert!(!rt.block_on(at_rest::upgrade_after_unlock(pin, &legacy)).unwrap(), "backed off");
        crate::db::get_write_connection_guard_static().unwrap().execute_batch("DROP TABLE unaddressable;").unwrap();
        crate::db::settings::remove_setting("kdf_upgrade_attempt").unwrap();

        assert!(rt.block_on(at_rest::upgrade_after_unlock(pin, &legacy)).unwrap());
        let kdf = Kdf::of_account().unwrap();
        assert!(!kdf.is_legacy(), "the salt is recorded");
        let salted = rt.block_on(derive_key(pin, &kdf));
        assert_eq!(crate::state::ENCRYPTION_KEY.get(), Some(salted), "the session carries on under the new key");
        assert_eq!(read(), before, "everything reads back");
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            assert_eq!(at_rest::sweep_residue_in_tx(&tx, &legacy, None).unwrap(), 0, "nothing opens under the legacy key");
        }
        assert_eq!(crate::db::get_sql_setting("last_vacuum".to_string()).unwrap(), None, "a vacuum is due");
        assert!(!rt.block_on(at_rest::upgrade_after_unlock(pin, &salted)).unwrap(), "the next unlock has nothing to do");

        // A second unlock that derived under the legacy salt before this upgrade committed can't
        // put that key back, and can't run an upgrade of its own.
        assert!(crate::crypto::install_unlocked_key(&legacy, &Kdf::Legacy).is_err());
        assert_eq!(crate::state::ENCRYPTION_KEY.get(), Some(salted));
        let again = Kdf::fresh();
        let again_key = rt.block_on(derive_key(pin, &again));
        assert!(!at_rest::upgrade_legacy_derivation(&legacy, &again_key, &again, &|_| {}).unwrap());
        assert_eq!(Kdf::of_account().unwrap(), kdf);
    }

    const CRASH_PIN: &str = "424242";

    /// Child half of `the_upgrade_survives_a_kill_at_any_moment`: open the account, unlock with
    /// the legacy key and run the upgrade until killed. Inert unless the parent set it up.
    #[test]
    #[ignore = "driven by the_upgrade_survives_a_kill_at_any_moment"]
    fn upgrade_crash_child() {
        use std::io::Write;
        let (Ok(root), Ok(npub)) = (std::env::var("VECTOR_CRASH_ROOT"), std::env::var("VECTOR_CRASH_NPUB")) else { return };
        crate::db::set_app_data_dir(root.into());
        crate::db::set_current_account(npub.clone()).unwrap();
        crate::db::init_database(&npub).unwrap();
        crate::state::init_encryption_enabled();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let legacy = rt.block_on(crate::crypto::derive_key(CRASH_PIN, &crate::crypto::Kdf::Legacy));
        crate::state::ENCRYPTION_KEY.set(legacy, &[]);
        // Derived before READY, so the parent's kills fall across the transaction alone.
        let kdf = crate::crypto::Kdf::fresh();
        let salted = rt.block_on(crate::crypto::derive_key(CRASH_PIN, &kdf));
        println!("CHILD READY");
        std::io::stdout().flush().unwrap();
        assert!(crate::db::at_rest::upgrade_legacy_derivation(&legacy, &salted, &kdf, &|_| {}).unwrap());
        println!("CHILD DONE");
        std::io::stdout().flush().unwrap();
    }

    /// The upgrade killed at moments swept across a whole run, in a separate process: every
    /// database left behind opens whole under exactly the key its recorded derivation names,
    /// and nothing is left under the other.
    #[test]
    #[ignore = "spawns and kills child processes; run with --ignored"]
    fn the_upgrade_survives_a_kill_at_any_moment() {
        use crate::crypto::{derive_key, Kdf};
        use std::io::BufRead;
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let legacy = rt.block_on(derive_key(CRASH_PIN, &Kdf::Legacy));

        // A legacy account big enough that the transaction takes real time.
        let template = crate::db::get_current_account().unwrap();
        crate::state::ENCRYPTION_KEY.set(legacy, &[]);
        crate::state::set_encryption_enabled(true);
        crate::db::set_sql_setting("encryption_enabled".to_string(), "true".to_string()).unwrap();
        crate::db::set_sql_setting("security_type".to_string(), "pin".to_string()).unwrap();
        crate::db::set_pkey(&crate::crypto::encrypt_with_key("nsec1crashaccount", &legacy).unwrap()).unwrap();
        let (_, cid, ch) = saved_v2("Crash");
        set_community_pins(&cid, &ch, r#"{"pins":["kept"]}"#, 1).unwrap();
        let chat = crate::db::id_cache::get_or_create_chat_id(&make_test_npub(900_003)).unwrap();
        {
            let mut conn = crate::db::get_write_connection_guard_static().unwrap();
            let tx = conn.transaction().unwrap();
            for i in 0..20_000u32 {
                tx.execute(
                    "INSERT INTO events (id, kind, chat_id, content, created_at, received_at) VALUES (?1, 14, ?2, ?3, ?4, ?4)",
                    rusqlite::params![format!("{i:064x}"), chat, crate::crypto::encrypt_with_key(&format!("message {i}"), &legacy).unwrap(), i],
                ).unwrap();
            }
            tx.commit().unwrap();
        }
        crate::db::close_database();
        let root = crate::db::shared_test_data_dir().to_path_buf();

        let copy_of_template = || -> String {
            let npub = make_test_npub(TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            std::fs::create_dir_all(root.join(&npub)).unwrap();
            for entry in std::fs::read_dir(root.join(&template)).unwrap().flatten() {
                std::fs::copy(entry.path(), root.join(&npub).join(entry.file_name())).unwrap();
            }
            npub
        };
        // One child run, killed `after` READY, or left to finish when `None`.
        let run_child = |npub: &str, after: Option<std::time::Duration>| -> std::time::Duration {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "db::community::tests::upgrade_crash_child", "--ignored", "--nocapture", "--test-threads=1"])
                .env("VECTOR_CRASH_ROOT", &root)
                .env("VECTOR_CRASH_NPUB", npub)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
            while let Some(Ok(line)) = lines.next() {
                if line.contains("CHILD READY") {
                    break;
                }
            }
            let ready = std::time::Instant::now();
            match after {
                Some(delay) => {
                    std::thread::sleep(delay);
                    let _ = child.kill();
                }
                None => {
                    while let Some(Ok(line)) = lines.next() {
                        if line.contains("CHILD DONE") {
                            break;
                        }
                    }
                }
            }
            let _ = child.wait();
            ready.elapsed()
        };
        let verify = |npub: &str| -> bool {
            crate::db::set_current_account(npub.to_string()).unwrap();
            crate::db::init_database(npub).unwrap();
            let kdf = Kdf::of_account().unwrap();
            let key = rt.block_on(derive_key(CRASH_PIN, &kdf));
            crate::state::ENCRYPTION_KEY.set(key, &[]);
            let pkey = crate::db::get_pkey().unwrap().unwrap();
            assert_eq!(crate::crypto::decrypt_with_key(&pkey, &key).unwrap(), "nsec1crashaccount", "{npub}: the stored key opens");
            assert_eq!(get_community_pins(&cid, &ch).unwrap().map(|p| p.0).as_deref(), Some(r#"{"pins":["kept"]}"#));
            let conn = crate::db::get_db_connection_guard_static().unwrap();
            let mut stmt = conn.prepare("SELECT content FROM events WHERE chat_id = ?1").unwrap();
            let unopened = stmt
                .query_map([chat], |r| r.get::<_, String>(0)).unwrap()
                .flatten()
                .filter(|c| crate::crypto::decrypt_with_key(c, &key).is_err())
                .count();
            assert_eq!(unopened, 0, "{npub}: every message opens under the recorded derivation");
            !kdf.is_legacy()
        };

        // Measure one whole run, then kill at moments spread across it and a little past.
        let first = copy_of_template();
        let whole = run_child(&first, None);
        assert!(verify(&first), "an uninterrupted run upgrades");
        let mut outcomes = (0, 0);
        let mut trace = String::new();
        for step in 0..24u32 {
            let npub = copy_of_template();
            run_child(&npub, Some(whole * step / 21));
            let upgraded = verify(&npub);
            trace.push(if upgraded { 'U' } else { 'L' });
            if upgraded { outcomes.1 += 1 } else { outcomes.0 += 1 }
        }
        println!("kill outcomes in time order: {trace}");
        println!("crash sweep over a {whole:?} transaction: {} left legacy, {} upgraded", outcomes.0, outcomes.1);
        assert!(outcomes.0 >= 10, "most kills land inside the transaction and roll it back");
        assert!(outcomes.1 > 0, "and the sweep reaches past the commit");
    }

    /// The stored derivation always describes the key the data is under: setup records it, enable
    /// and a PIN change replace it in their own transaction, disable drops it, and a refused PIN
    /// change leaves it alone.
    #[test]
    fn the_key_derivation_moves_with_every_migration() {
        use crate::crypto::Kdf;
        use crate::db::at_rest;
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        assert_eq!(Kdf::of_account().unwrap(), Kdf::Legacy, "no row reads as the legacy salt");
        save_community_v2(&v2_fixture("K", &[])).unwrap();

        let (k1, key1) = (Kdf::fresh(), [0x51; 32]);
        at_rest::enable(&key1, &k1, "pin", None, None, &|_| {}).unwrap();
        assert_eq!(Kdf::of_account().unwrap(), k1);

        let (k2, key2) = (Kdf::fresh(), [0x52; 32]);
        assert!(at_rest::rekey(&[0x5F; 32], &key2, &k2, "pin", None, &|_| {}).is_err());
        assert_eq!(Kdf::of_account().unwrap(), k1, "a refused change keeps the derivation");
        at_rest::rekey(&key1, &key2, &k2, "pin", None, &|_| {}).unwrap();
        assert_eq!(Kdf::of_account().unwrap(), k2);

        at_rest::disable(&key2, &|_| {}).unwrap();
        assert_eq!(Kdf::of_account().unwrap(), Kdf::Legacy, "a plaintext store keeps no derivation");

        let k3 = Kdf::fresh();
        let sealed = crate::crypto::encrypt_with_key("nsec1setup", &[0x53; 32]).unwrap();
        crate::db::settings::commit_account_setup(&sealed, true, Some("pin"), None, None, k3.descriptor().as_deref()).unwrap();
        assert_eq!(Kdf::of_account().unwrap(), k3, "setup records the salt with the key it sealed");
    }

    #[test]
    fn the_identity_opens_under_the_users_proof_and_nothing_else() {
        use crate::crypto::{derive_key, Kdf};
        use crate::db::at_rest::{self, open_identity_secrets, CREDENTIAL_INCORRECT};
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        crate::db::set_pkey("nsec1identity").unwrap();
        crate::db::set_seed("abandon ability able").unwrap();
        let plain = open_identity_secrets(None).unwrap();
        assert_eq!((plain.0.as_str(), plain.1.as_deref().map(|s| s.as_str())), ("nsec1identity", Some("abandon ability able")));

        let kdf = Kdf::fresh();
        let key = rt.block_on(derive_key("424242", &kdf));
        at_rest::enable(&key, &kdf, "pin", None, None, &|_| {}).unwrap();
        assert!(crate::state::ENCRYPTION_KEY.get().is_some(), "the session holds the key");
        assert!(open_identity_secrets(None).is_err(), "a sealed store opens nothing on the session alone");
        assert_eq!(rt.block_on(at_rest::prove_credential("000000")).unwrap_err(), CREDENTIAL_INCORRECT);
        assert_eq!(open_identity_secrets(Some(&[0x09; 32])).unwrap_err(), CREDENTIAL_INCORRECT);

        let proven = rt.block_on(at_rest::prove_credential("424242")).unwrap();
        let (nsec, seed) = open_identity_secrets(Some(&proven)).unwrap();
        assert_eq!((nsec.as_str(), seed.as_deref().map(|s| s.as_str())), ("nsec1identity", Some("abandon ability able")));

        // Misses past the free ones lock even the right PIN out for a while, and a success clears them.
        for _ in 0..4 {
            assert_eq!(rt.block_on(at_rest::prove_credential("000000")).unwrap_err(), CREDENTIAL_INCORRECT);
        }
        assert_eq!(rt.block_on(at_rest::prove_credential("000000")).unwrap_err(), CREDENTIAL_INCORRECT);
        let locked = rt.block_on(at_rest::prove_credential("424242")).unwrap_err();
        assert!(locked.starts_with("CREDENTIAL_LOCKED:"), "{locked}");
        crate::db::remove_setting("credential_retry_at").unwrap();
        rt.block_on(at_rest::prove_credential("424242")).unwrap();
        assert!(crate::db::get_sql_setting("credential_failures".to_string()).unwrap().is_none());

        // A clock set back far can't stretch the wait past the longest one a miss earns.
        let far = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap().as_secs() + 86_400 * 30;
        crate::db::set_sql_setting("credential_retry_at".to_string(), far.to_string()).unwrap();
        let locked = rt.block_on(at_rest::prove_credential("424242")).unwrap_err();
        let secs: u64 = locked.strip_prefix("CREDENTIAL_LOCKED:").unwrap().parse().unwrap();
        assert!(secs <= 15 * 60, "{secs}");
        crate::db::remove_setting("credential_retry_at").unwrap();

        // A seed left in plaintext from before encryption still exports; one that won't open stays behind.
        crate::db::set_seed("abandon ability able").unwrap();
        assert_eq!(open_identity_secrets(Some(&proven)).unwrap().1.as_deref().map(|s| s.as_str()), Some("abandon ability able"));
        crate::db::set_seed("deadbeef").unwrap();
        let (nsec, seed) = open_identity_secrets(Some(&proven)).unwrap();
        assert!(nsec.starts_with("nsec1") && seed.is_none());
    }

    /// Reads racing a stream of PIN changes always open: a row read just before a commit and
    /// opened just after, or read just after and opened before the vault moved, never comes
    /// back as ciphertext.
    #[test]
    fn reads_racing_pin_changes_always_open() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        let keys: Vec<[u8; 32]> = (0u8..31).map(|i| [0x90 + i; 32]).collect();
        crate::state::ENCRYPTION_KEY.set(keys[0], &[]);
        crate::state::set_encryption_enabled(true);
        let (_, cid, ch) = saved_v2("Read");
        let pinned = r#"{"pins":["kept"]}"#;
        set_community_pins(&cid, &ch, pinned, 1).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let (reads, garbled) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let reader = {
            let (stop, reads, garbled, cid, ch) = (stop.clone(), reads.clone(), garbled.clone(), cid.clone(), ch.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let got = get_community_pins(&cid, &ch).unwrap().map(|(c, _)| c);
                    reads.fetch_add(1, Ordering::SeqCst);
                    if got.as_deref() != Some(pinned) {
                        garbled.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        };
        for pair in keys.windows(2) {
            crate::db::at_rest::rekey(&pair[0], &pair[1], &crate::crypto::Kdf::fresh(), "pin", None, &|_| {}).unwrap();
            // The grace covers a read straddling ONE change; real PIN changes are seconds apart,
            // so let a read that began after this one finish before the next.
            let seen = reads.load(Ordering::SeqCst);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while reads.load(Ordering::SeqCst) < seen + 2 {
                assert!(std::time::Instant::now() < deadline, "reads stopped completing across a PIN change");
                std::thread::yield_now();
            }
        }
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
        assert!(reads.load(Ordering::SeqCst) > 0);
        assert_eq!(garbled.load(Ordering::SeqCst), 0, "no read across a PIN change came back unopened");
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(!crate::state::PREVIOUS_AT_REST_KEY.has_key(), "the replaced key is wiped after its grace");
    }

    /// Writes racing a stream of PIN changes all land on the key the store ends on, and a
    /// read-modify-write never loses what it merged into.
    #[test]
    fn writes_racing_pin_changes_land_on_the_final_key() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        let keys: Vec<[u8; 32]> = (0u8..21).map(|i| [0x40 + i; 32]).collect();
        crate::state::ENCRYPTION_KEY.set(keys[0], &[]);
        crate::state::set_encryption_enabled(true);
        let (_, cid, ch) = saved_v2("Race");

        let stop = Arc::new(AtomicBool::new(false));
        let last = Arc::new(AtomicU64::new(0));
        let writer = {
            let (stop, last, cid, ch) = (stop.clone(), last.clone(), cid.clone(), ch.clone());
            std::thread::spawn(move || {
                let mut i = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    i += 1;
                    set_community_pins(&cid, &ch, &format!("{{\"v\":{i}}}"), i as i64).unwrap();
                    merge_community_ban_marks(&cid, &[(format!("npub{i}"), i)].into_iter().collect()).unwrap();
                    last.store(i, Ordering::SeqCst);
                }
            })
        };
        while last.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        for pair in keys.windows(2) {
            crate::db::at_rest::rekey(&pair[0], &pair[1], &crate::crypto::Kdf::fresh(), "pin", None, &|_| {}).unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();

        let n = last.load(Ordering::SeqCst);
        assert_eq!(get_community_pins(&cid, &ch).unwrap(), Some((format!("{{\"v\":{n}}}"), n as i64)));
        let marks = get_community_ban_marks(&cid).unwrap();
        assert_eq!(marks.len() as u64, n, "every merged mark survives the key changes");
    }

    /// Every encrypting writer, on the real schema, through a PIN change and a disable. The
    /// sweep list alone must leave nothing on the old key, so a writer whose column it misses
    /// fails here. A new writer that encrypts a column belongs in this test.
    #[test]
    fn a_v2_link_counts_the_members_who_joined_through_it() {
        use crate::community::v2::guestbook::{GuestbookEntry, GuestbookEvent};
        let (_tmp, _guard) = init_test_db();
        let owner = nostr_sdk::prelude::Keys::generate();
        let g = crate::community::v2::control::genesis(
            &owner,
            crate::community::v2::control::CommunityMetadata { name: "Hub".into(), ..Default::default() },
            1_000,
        )
        .unwrap();
        let v2 = crate::community::v2::community::CommunityV2::from_genesis(&g, "Hub", None, vec!["wss://r".into()], 1_000);
        save_community_v2(&v2).unwrap();
        let cid = v2.id().to_hex();

        let me = owner.public_key();
        let someone_else = nostr_sdk::prelude::Keys::generate().public_key();
        let (a, b, c) = (Keys::generate().public_key(), Keys::generate().public_key(), Keys::generate().public_key());
        let join = |n: u8, member: PublicKey, by: Option<(String, &str)>| GuestbookEvent {
            epoch: None,
            observed_at: None,
            rumor_id: [n; 32],
            entry: GuestbookEntry::Join { member, invited_by: by.map(|(c, l)| (c, l.to_string())), at_ms: n as u64 },
        };
        let events = vec![
            join(1, a, Some((me.to_hex(), "hub"))),
            join(2, b, Some((me.to_bech32().unwrap(), "hub"))),
            join(3, a, Some((me.to_hex(), "hub"))),
            join(4, c, Some((me.to_hex(), ""))),
            join(5, c, Some((someone_else.to_hex(), "hub"))),
            join(6, b, None),
        ];
        set_guestbook(&cid, &events, 9).unwrap();

        let counts = community_invite_join_counts(&cid, &me).unwrap();
        assert_eq!(counts.get("hub"), Some(&2), "a rejoin counts once, either spelling of the creator counts");
        assert_eq!(counts.get(""), Some(&1), "an unlabeled link's joins are kept apart");
        assert_eq!(counts.len(), 2, "another creator's link and a bare join count for nobody here");
    }

    #[test]
    fn a_group_chat_becomes_a_community_with_a_second_channel_or_an_invite_link() {
        let (_tmp, _guard) = init_test_db();
        let is = |c: &crate::community::v2::community::CommunityV2| crate::notify::is_community(&c.id().to_hex());

        let mut chat = v2_fixture("Friends", &["wss://r"]);
        save_community_v2(&chat).unwrap();
        assert!(!is(&chat), "one channel, no link: a group chat");
        assert_eq!(crate::notify::default_level(Some(&chat.id().to_hex())), crate::notify::NotifyLevel::All);

        let mut second = chat.channels[0].clone();
        second.id = crate::community::ChannelId([0x42; 32]);
        second.name = "random".into();
        chat.channels.push(second);
        save_community_v2(&chat).unwrap();
        assert!(is(&chat), "a second channel makes it a community");
        assert_eq!(crate::notify::default_level(Some(&chat.id().to_hex())), crate::notify::NotifyLevel::Mentions);

        let open = v2_fixture("Open", &["wss://r"]);
        save_community_v2(&open).unwrap();
        assert!(!is(&open));
        set_community_invite_registry(&open.id().to_hex(), &["loc1".into()]).unwrap();
        assert!(is(&open), "an invite link makes it a community");
        set_community_invite_registry(&open.id().to_hex(), &[]).unwrap();
        assert!(!is(&open), "revoking the last link makes it a group chat again");

        assert_eq!(crate::notify::default_level(None), crate::notify::NotifyLevel::All, "a DM rings for everything");
    }

    #[test]
    fn every_encrypting_writer_is_covered_by_the_at_rest_sweep() {
        use crate::db::at_rest;
        const K: [u8; 32] = [0x31; 32];
        const K2: [u8; 32] = [0x32; 32];
        const WRONG: [u8; 32] = [0x39; 32];
        struct Vault;
        impl Drop for Vault {
            fn drop(&mut self) {
                crate::state::ENCRYPTION_KEY.clear(&[]);
                crate::state::set_encryption_enabled(false);
            }
        }
        let (_tmp, _guard) = init_test_db();
        let _vault = Vault;
        crate::state::ENCRYPTION_KEY.set(K, &[]);
        crate::state::set_encryption_enabled(true);

        let mut v2 = v2_fixture("HQ", &["wss://r"]);
        v2.description = Some("about".into());
        let image = |url: &str| crate::community::v2::control::ImageRef {
            url: url.into(), key: "0".repeat(64), nonce: "1".repeat(32), hash: "a".repeat(64), extra: Default::default(),
        };
        v2.icon = Some(image("https://blossom.example/icon"));
        v2.banner = Some(image("https://blossom.example/banner"));
        v2.meta_custom = Some([("k".to_string(), serde_json::Value::from("v"))].into_iter().collect());
        v2.channels[0].voice = Some(true);
        v2.channels[0].meta_extra.insert("vnd".into(), serde_json::Value::from(7));
        v2.channels.push(private_channel(0x61, 0x62));
        save_community_v2(&v2).unwrap();
        let cid = v2.id().to_hex();
        let ch = v2.channels[0].id.to_hex();
        let private = v2.channels[1].id.to_hex();
        store_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 0, &v2.community_root).unwrap();
        store_epoch_key(&cid, &private, 0, &[0x62; 32]).unwrap();
        park_channel_key(&cid, &private, 3, &[0x33; 32], &"ab".repeat(32)).unwrap();
        store_message_key("m1", "o1", &nostr_sdk::prelude::Keys::generate(), &["wss://r".into()]).unwrap();
        let owner = nostr_sdk::prelude::Keys::generate();
        store_wrap_owner("m1", "o1", &owner, &["wss://r".into()]).unwrap();
        save_pending_invite(&"de".repeat(32), r#"{"bundle":1}"#, "npub1inviter", now_secs() + 3600).unwrap();
        save_public_invite(&"ab".repeat(32), &cid, "https://vectorapp.io/invite#x", None, Some("Label")).unwrap();
        set_community_policy(&cid, "p1", r#"{"format":1}"#, "hh", true, 0).unwrap();
        merge_community_ban_marks(&cid, &[("npub1banned".to_string(), 5u64)].into_iter().collect()).unwrap();
        set_community_roles(&cid, &crate::community::roles::CommunityRoles::default(), 1).unwrap();
        set_community_invite_registry(&cid, &["loc1".into()]).unwrap();
        replace_invite_link_sets(&cid, &[InviteLinkSetRow { creator_hex: "cd".repeat(32), locators: vec!["l1".into()] }]).unwrap();
        set_guestbook(&cid, &[], 9).unwrap();
        set_community_pins(&cid, &ch, r#"{"pins":["x"]}"#, 1).unwrap();
        record_audit_local(&cid, "e1", 5, r#"{"action":"message_delete"}"#).unwrap();

        // A DM with a link preview and a file, and an unconfirmed send's retained rumor.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let dm_chat = make_test_npub(900_000);
        let dm = crate::types::Message {
            id: "a1".repeat(32),
            content: "the message itself".into(),
            at: 1_700_000_000_000,
            mine: true,
            preview_metadata: Some(crate::types::SiteMetadata {
                domain: "example.com".into(), og_title: Some("A title".into()), og_description: None,
                og_image: None, og_url: Some("https://example.com/a".into()), og_type: None,
                title: None, description: None, favicon: None,
            }),
            attachments: vec![crate::types::Attachment {
                id: "b2".repeat(32), key: "c3".repeat(32), nonce: "d4".repeat(16), extension: "jpg".into(),
                name: "passport.jpg".into(), url: "https://blossom.example/x.jpg".into(), size: 42,
                img_meta: Some(crate::types::ImageMetadata { thumbhash: "thumb".into(), width: 4, height: 3 }),
                fallback_urls: vec!["https://mirror.example/x.jpg".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        rt.block_on(crate::db::events::save_message(&dm_chat, &dm)).unwrap();
        let dm_chat_int = crate::db::id_cache::get_or_create_chat_id(&dm_chat).unwrap();
        let (wrap, rumor) = {
            use crate::event_ext::FinalizeUnsignedWithId;
            use nostr_sdk::prelude::*;
            let ephemeral = Keys::generate();
            let wrap = EventBuilder::new(Kind::TextNote, "wrap").finalize(&ephemeral).unwrap();
            let rumor = EventBuilder::new(Kind::PrivateDirectMessage, "still sending")
                .finalize_unsigned_with_id(Keys::generate().public_key());
            crate::db::nip17_keys::store_wrap_key(
                &wrap.id, &rumor.id.unwrap(), &Keys::generate().public_key(),
                crate::db::nip17_keys::WrapRole::Recipient, ephemeral.secret_key(), &[],
            ).unwrap();
            (wrap, rumor)
        };
        crate::db::nip17_keys::stash_resend_payload(&wrap.id, "pending-1", &wrap, &rumor).unwrap();
        // A stranger's wallpaper, held until their request is accepted.
        crate::db::chats::hold_wallpaper(&dm_chat, 5, r#"{"url":"https://blossom.example/held-wallpaper"}"#).unwrap();
        // A kept file's key.
        let kept = crate::db::media_store::StoredMedia {
            hash: [0x5b; 32], file: "f".into(), key: "e5".repeat(32), nonce: "f6".repeat(16), blob_sha256: "aa".repeat(32),
            pt_len: 9, mime: "image/png".into(), origin: crate::db::media_store::Origin::Received,
        };
        crate::db::media_store::insert(&kept).unwrap();

        // A ban set while sealed still hides its author from the channel and its unread count.
        let (banned, allowed) = {
            use nostr_sdk::prelude::ToBech32;
            (nostr_sdk::prelude::Keys::generate().public_key(), nostr_sdk::prelude::Keys::generate().public_key().to_bech32().unwrap())
        };
        set_community_banlist(&cid, &[banned.to_hex()], 1).unwrap();
        for (id, author) in [("c7", { use nostr_sdk::prelude::ToBech32; banned.to_bech32().unwrap() }), ("d8", allowed)] {
            let msg = crate::types::Message { id: id.repeat(32), content: "in the channel".into(), at: 1_700_000_000_000, npub: Some(author), ..Default::default() };
            rt.block_on(crate::db::events::save_message(&ch, &msg)).unwrap();
        }
        let ch_int = crate::db::id_cache::get_or_create_chat_id(&ch).unwrap();

        let read = || {
            (
                get_pending_channel_keys(&cid).unwrap().iter().map(|k| (k.key, k.sender.clone())).collect::<Vec<_>>(),
                get_community_ban_marks(&cid).unwrap(),
                get_community_pins(&cid, &ch).unwrap(),
                load_community_v2(v2.id()).unwrap().map(|c| format!("{c:?}")),
                (
                    held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap(),
                    held_epoch_keys(&cid, &private).unwrap(),
                    get_audit_local(&cid).unwrap(),
                ),
                rt.block_on(crate::db::events::get_message_views(dm_chat_int, 10, 0)).unwrap()
                    .into_iter().map(|m| (m.content, m.preview_metadata, m.attachments)).collect::<Vec<_>>(),
                crate::db::nip17_keys::get_resend_payload_by_pending("pending-1").unwrap().map(|p| p.rumor.content),
                (
                    get_community_banlist(&cid).unwrap(),
                    rt.block_on(crate::db::events::get_message_views(ch_int, 10, 0)).unwrap().into_iter().map(|m| m.id).collect::<Vec<_>>(),
                    rt.block_on(crate::db::events::unread_count_for_chat(&ch)).unwrap(),
                    rt.block_on(crate::db::events::unread_counts()).unwrap().get(&ch).copied(),
                ),
                wrap_owners("m1").unwrap().into_iter().map(|(k, outer, relays)| (k.public_key(), outer, relays)).collect::<Vec<_>>(),
                crate::db::media_store::get(&[0x5b; 32]).unwrap(),
            )
        };
        let before = read();
        assert_eq!(before.8, vec![(owner.public_key(), "o1".to_string(), vec!["wss://r".to_string()])]);
        assert_eq!(before.9.as_ref(), Some(&kept));
        assert_eq!(before.0, vec![([0x33; 32], "ab".repeat(32))]);
        assert_eq!(before.2, Some((r#"{"pins":["x"]}"#.to_string(), 1)));
        assert_eq!(before.3, Some(format!("{v2:?}")));
        assert_eq!(
            before.4,
            (vec![(Epoch(0), v2.community_root)], vec![(Epoch(0), [0x62; 32])], vec![("e1".to_string(), 5, r#"{"action":"message_delete"}"#.to_string())])
        );
        // File captions are stored but not shown again yet; the view still carries the rest.
        assert_eq!(before.5, vec![(String::new(), dm.preview_metadata.clone(), dm.attachments.clone())]);
        assert_eq!(before.6.as_deref(), Some("still sending"));
        assert_eq!(before.7, (vec![banned.to_hex()], vec!["d8".repeat(32)], 1, Some(1)), "the banned author stays hidden");
        let sealed: (String, String, String) = crate::db::get_db_connection_guard_static().unwrap().query_row(
            "SELECT a.name || e.preview_metadata || e.content, w.rumor_json, a.key FROM attachments a, events e, nip17_wrap_keys w
             WHERE e.id = a.event_id AND w.pending_id = 'pending-1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap();
        for plain in ["passport", "A title", "the message itself"] {
            assert!(!sealed.0.contains(plain), "{plain} is sealed on disk");
        }
        assert!(!sealed.1.contains("still sending") && sealed.2 != "c3".repeat(32), "unconfirmed sends and file keys are sealed");
        let held: String = crate::db::get_db_connection_guard_static().unwrap()
            .query_row("SELECT payload FROM dm_held_wallpapers", [], |r| r.get(0)).unwrap();
        assert!(!held.contains("held-wallpaper"), "a held wallpaper is sealed on disk");

        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            at_rest::rekey_listed_in_tx(&tx, &K, &K2, &|_| {}).unwrap();
            assert_eq!(
                at_rest::sweep_residue_in_tx(&tx, &K, Some(&K2)).unwrap(),
                0,
                "the sweep list must name every column these writers encrypt"
            );
        }

        assert!(at_rest::rekey(&WRONG, &K2, &crate::crypto::Kdf::fresh(), "pin", None, &|_| {}).is_err(), "a key the store does not open is refused");
        at_rest::rekey(&K, &K2, &crate::crypto::Kdf::fresh(), "pin", None, &|_| {}).unwrap();
        crate::state::ENCRYPTION_KEY.set(K2, &[]);
        assert_eq!(read(), before, "every value reads back after a PIN change");
        assert_eq!(
            crate::db::chats::take_held_wallpaper(&dm_chat).unwrap().as_deref(),
            Some(r#"{"url":"https://blossom.example/held-wallpaper"}"#),
            "a held wallpaper moves with the key"
        );

        let mode = || crate::db::get_sql_setting("security_type".to_string()).unwrap();
        at_rest::rekey(&K2, &K2, &crate::crypto::Kdf::of_account().unwrap(), "password", None, &|_| {}).unwrap();
        assert_eq!((read(), mode().as_deref()), (before.clone(), Some("password")), "the same key on both sides only switches the mode");

        // Rows from before this build: a plaintext caption and a DM whose plaintext looks sealed.
        let legacy_chat = crate::db::id_cache::get_or_create_chat_id(&make_test_npub(900_001)).unwrap();
        let hexish = "ab".repeat(40);
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            for (id, kind, content) in [("e5", 15, "old caption"), ("f6", 14, hexish.as_str())] {
                conn.execute(
                    "INSERT INTO events (id, kind, chat_id, content, created_at, received_at) VALUES (?1, ?2, ?3, ?4, 1, 1)",
                    rusqlite::params![id.repeat(32), kind, legacy_chat, content],
                ).unwrap();
            }
        }
        let flag = || crate::db::get_sql_setting("community_at_rest_encrypted".to_string()).unwrap();
        crate::db::set_sql_setting("community_at_rest_encrypted".to_string(), "1".to_string()).unwrap();
        crate::state::ENCRYPTION_KEY.set(WRONG, &[]);
        assert!(at_rest::backfill_at_rest().is_err(), "the backfill never wraps under a key it cannot prove");
        assert_eq!(flag().as_deref(), Some("1"));
        crate::state::ENCRYPTION_KEY.set(K2, &[]);
        at_rest::backfill_at_rest().unwrap();
        assert_eq!((read(), flag().as_deref()), (before.clone(), Some("8")));
        let stored = |id: &str| -> String {
            crate::db::get_db_connection_guard_static().unwrap()
                .query_row("SELECT content FROM events WHERE id = ?1", [id.repeat(32)], |r| r.get(0)).unwrap()
        };
        assert!(stored("e5") != "old caption" && stored("f6") != hexish, "the backfill seals legacy plaintext messages");
        let legacy = rt.block_on(crate::db::events::get_message_views(legacy_chat, 10, 0)).unwrap();
        assert!(legacy.iter().any(|m| m.content == hexish), "and they still read back");

        assert!(at_rest::disable(&WRONG, &|_| {}).is_err(), "a wrong key never turns encryption off");
        assert_eq!(read(), before);
        at_rest::disable(&K2, &|_| {}).unwrap();
        crate::state::ENCRYPTION_KEY.clear(&[]);
        assert_eq!(read(), before, "every value reads back after Local Encryption is turned off");
    }
}

// ── Pin lists (CORD-04 §7) ───────────────────────────────────────────────────

/// Persist a channel's folded Pin List head — the RAW carried content (either
/// self-describing form), never a re-serialization: republishing must carry the
/// exact bytes, and the byte cap judges what the wire carried. Encrypted at
/// rest like the banlist — a public-form list carries disclosed message keys.
///
/// Monotonic IN THE STATEMENT: fold persists and publish echoes race (the fold
/// reads relay windows while an echo lands), and a read-check-then-write let a
/// stale fold clobber a newer echo. Equal versions still write — a same-version
/// fork's converged winner must be adoptable. Returns whether a row changed.
pub fn set_community_pins(community_id: &str, channel_id: &str, content: &str, version: i64) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    let enc = enc_txt(content)?;
    let conn = super::get_write_connection_guard_static()?;
    let changed = conn
        .execute(
            "INSERT INTO community_pins (community_id, channel_id, content, version) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(community_id, channel_id) DO UPDATE SET
                 content = excluded.content, version = excluded.version
             WHERE excluded.version >= community_pins.version",
            params![community_id, channel_id, enc, version],
        )
        .map_err(|e| format!("set pins: {e}"))?;
    Ok(changed > 0)
}

/// Persist a channel's folded Writers ledger head (CORD-03 §1), monotonic on version.
pub fn set_community_writers(community_id: &str, channel_id: &str, content: &str, version: i64, replace: bool) -> Result<bool, String> {
    let _seal = crate::crypto::gate::sealing();
    let enc = enc_txt(content)?;
    let conn = super::get_write_connection_guard_static()?;
    let old: Option<String> = conn
        .query_row("SELECT content FROM community_writers WHERE community_id = ?1 AND channel_id = ?2", params![community_id, channel_id], |r| r.get(0))
        .optional()
        .map_err(|e| format!("get writers: {e}"))?
        .map(|c: String| dec_txt(&c));
    let changed = conn
        .execute(
            "INSERT INTO community_writers (community_id, channel_id, content, version) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(community_id, channel_id) DO UPDATE SET
                 content = excluded.content, version = excluded.version
             WHERE ?5 OR excluded.version >= community_writers.version",
            params![community_id, channel_id, enc, version, replace],
        )
        .map_err(|e| format!("set writers: {e}"))?;
    // Whichever fold stored it, what a new writer or stop entry undoes is owed a retraction.
    if changed > 0 {
        crate::community::v2::realtime::invalidate_held_v2_community(community_id);
        if let Some(since) = retraction_since(old.as_deref(), content) {
            owe_retraction(&conn, community_id, channel_id, since)?;
        }
        for (writer, since) in stop_owes(old.as_deref(), content) {
            owe_author_retraction_on(&conn, community_id, channel_id, &crate::simd::hex::bytes_to_hex_32(&writer), since, None)?;
        }
    }
    Ok(changed > 0)
}

/// From when a move from `old` to `new` can change everyone's verdicts: the earliest writer entry
/// it added, or entry a fork switch dropped. An `open` added takes nothing back, an entry the cap
/// trimmed from the front moves no window, and a stop is its writer's alone ([`stop_owes`]).
pub(crate) fn retraction_since(old: Option<&str>, new: &str) -> Option<u64> {
    use crate::community::v2::writers::{Ledger, Spell};
    let new = Ledger::parse(new)?;
    let old = old.and_then(Ledger::parse);
    let added = new.entries.iter().filter(|e| matches!(e.spell, Spell::Writer { .. })).filter(|e| !old.as_ref().is_some_and(|o| o.entries.contains(e)));
    let front = new.entries.first().map_or(u64::MAX, |e| e.at);
    let dropped = old.iter().flat_map(|o| o.entries.iter()).filter(|e| !matches!(e.spell, Spell::Other) && e.at >= front && !new.entries.contains(e));
    added.chain(dropped).map(|e| e.effective()).min()
}

/// The stops a move from `old` to `new` added: whose posts each undoes, and from when.
pub(crate) fn stop_owes(old: Option<&str>, new: &str) -> Vec<([u8; 32], u64)> {
    use crate::community::v2::writers::{Ledger, Spell};
    let Some(new) = Ledger::parse(new) else { return Vec::new() };
    let old = old.and_then(Ledger::parse);
    new.entries
        .iter()
        .filter(|e| !old.as_ref().is_some_and(|o| o.entries.contains(e)))
        .filter_map(|e| match e.spell {
            Spell::Stop { writer } => Some((writer, e.effective())),
            _ => None,
        })
        .collect()
}

fn retraction_key(community_id: &str, channel_id: &str) -> String {
    format!("writer_retract:{community_id}:{channel_id}")
}

/// Stored as `since:generation`: a drain clears only the value it read, so what is owed while it
/// runs survives it.
fn parse_owed(value: &str) -> Option<(u64, u64)> {
    let mut parts = value.split(':');
    Some((parts.next()?.parse().ok()?, parts.next().and_then(|g| g.parse().ok()).unwrap_or(0)))
}

fn owe_retraction(conn: &rusqlite::Connection, community_id: &str, channel_id: &str, since: u64) -> Result<(), String> {
    let key = retraction_key(community_id, channel_id);
    let held: Option<String> = conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional().map_err(|e| format!("owed retraction: {e}"))?;
    let (since, generation) = held.as_deref().and_then(parse_owed).map_or((since, 1), |(s, g)| (s.min(since), g + 1));
    conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![key, format!("{since}:{generation}")]).map_err(|e| format!("owe retraction: {e}"))?;
    Ok(())
}

/// Owe `channel_id` a retraction of what it shows dated from `since_ms`.
pub fn owe_writer_retraction(community_id: &str, channel_id: &str, since_ms: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    owe_retraction(&conn, community_id, channel_id, since_ms)
}

/// The Channels of `community_id` owed a retraction: `(channel, since_ms, generation)`.
pub fn owed_retractions(community_id: &str) -> Result<Vec<(String, u64, u64)>, String> {
    let prefix = retraction_key(community_id, "");
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT key, value FROM settings WHERE substr(key, 1, length(?1)) = ?1").map_err(|e| format!("owed retractions: {e}"))?;
    let rows = stmt.query_map(params![prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| format!("owed retractions: {e}"))?;
    Ok(rows.flatten().filter_map(|(k, v)| parse_owed(&v).map(|(since, generation)| (k[prefix.len()..].to_string(), since, generation))).collect())
}

fn author_retraction_key(community_id: &str, channel_id: &str, author: &str) -> String {
    format!("writer_retract_by:{community_id}:{channel_id}:{author}")
}

/// Owe `channel_id` a retraction of what `author` (hex) posted there from `since_ms`, before
/// `until_ms` when given.
pub fn owe_author_retraction(community_id: &str, channel_id: &str, author: &str, since_ms: u64, until_ms: Option<u64>) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    owe_author_retraction_on(&conn, community_id, channel_id, author, since_ms, until_ms)
}

/// Stored as `since:generation:until`, `until` empty when open.
fn parse_author_owed(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let mut parts = value.split(':');
    let since = parts.next()?.parse().ok()?;
    let generation = parts.next().and_then(|g| g.parse().ok()).unwrap_or(0);
    let until = parts.next().and_then(|u| u.parse().ok());
    Some((since, generation, until))
}

fn author_owed_value(since: u64, generation: u64, until: Option<u64>) -> String {
    format!("{since}:{generation}:{}", until.map_or(String::new(), |u| u.to_string()))
}

fn owe_author_retraction_on(conn: &rusqlite::Connection, community_id: &str, channel_id: &str, author: &str, since_ms: u64, until_ms: Option<u64>) -> Result<(), String> {
    let key = author_retraction_key(community_id, channel_id, author);
    let held: Option<String> = conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional().map_err(|e| format!("owed retraction: {e}"))?;
    let (since, generation, until) = match held.as_deref().and_then(parse_author_owed) {
        // The same span closing narrows its own owe; any other range widens it.
        Some((s, g, u)) => (s.min(since_ms), g + 1, match (u, until_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, Some(b)) if s == since_ms => Some(b),
            _ => None,
        }),
        None => (since_ms, 1, until_ms),
    };
    conn.execute("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)", params![key, author_owed_value(since, generation, until)]).map_err(|e| format!("owe retraction: {e}"))?;
    Ok(())
}

/// What `community_id` owes one author's posts: `(channel, author hex, since_ms, until_ms, generation)`.
pub fn owed_author_retractions(community_id: &str) -> Result<Vec<(String, String, u64, Option<u64>, u64)>, String> {
    let prefix = author_retraction_key(community_id, "", "");
    let prefix = &prefix[..prefix.len() - 1];
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT key, value FROM settings WHERE substr(key, 1, length(?1)) = ?1").map_err(|e| format!("owed retractions: {e}"))?;
    let rows = stmt.query_map(params![prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| format!("owed retractions: {e}"))?;
    Ok(rows
        .flatten()
        .filter_map(|(k, v)| {
            let (channel, author) = k[prefix.len()..].split_once(':')?;
            let (since, generation, until) = parse_author_owed(&v)?;
            Some((channel.to_string(), author.to_string(), since, until, generation))
        })
        .collect())
}

/// Clear one author's owed retraction once done, unless more was owed since it was read.
pub fn settle_author_retraction(community_id: &str, channel_id: &str, author: &str, since: u64, until: Option<u64>, generation: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM settings WHERE key = ?1 AND value = ?2", params![author_retraction_key(community_id, channel_id, author), author_owed_value(since, generation, until)])
        .map_err(|e| format!("settle retraction: {e}"))?;
    Ok(())
}

/// The departures this device has already owed take-backs for, by author hex.
pub fn departures_seen(community_id: &str) -> Result<std::collections::BTreeMap<String, Vec<crate::community::v2::guestbook::Span>>, String> {
    Ok(super::settings::get_sql_setting(format!("departures_seen:{community_id}"))?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
}

pub fn set_departures_seen(community_id: &str, seen: &std::collections::BTreeMap<String, Vec<crate::community::v2::guestbook::Span>>) -> Result<(), String> {
    super::settings::set_sql_setting(format!("departures_seen:{community_id}"), serde_json::to_string(seen).map_err(|e| e.to_string())?)
}

/// Clear `channel_id`'s owed retraction once done, unless more was owed since it was read.
pub fn settle_retraction(community_id: &str, channel_id: &str, since: u64, generation: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM settings WHERE key = ?1 AND value = ?2", params![retraction_key(community_id, channel_id), format!("{since}:{generation}")])
        .map_err(|e| format!("settle retraction: {e}"))?;
    Ok(())
}

pub fn get_community_writers(community_id: &str, channel_id: &str) -> Result<Option<(String, i64)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT content, version FROM community_writers WHERE community_id = ?1 AND channel_id = ?2",
            params![community_id, channel_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("get writers: {e}"))?;
    Ok(row.map(|(content, version)| (dec_txt(&content), version)))
}

/// How many held wraps one Channel keeps, how many bytes, how many one author, and how many
/// bytes a Community: past any, those dated furthest from now go first, so a flood dated years
/// ahead or behind, or one author's, evicts only itself.
pub const MAX_HELD_WRAPS: usize = 500;
pub const MAX_HELD_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_HELD_PER_AUTHOR: usize = MAX_HELD_WRAPS / 10;
pub const MAX_HELD_COMMUNITY_BYTES: usize = 16 * 1024 * 1024;

/// How far past this device's clock a post may be dated and still be held (CORD-02 §5's horizon):
/// a later one is left on the relay for a later read.
pub const HELD_HORIZON_MS: u64 = 60 * 60 * 1000;

/// Hold `wrap`, a post by `author` (hex) `channel_id`'s rules didn't stand behind on arrival,
/// dated `at_ms`. Whether it is held: a post dated past the horizon isn't, nor one the caps
/// evict at once. A marked copy kept beside an evicted wrap goes with it.
pub fn hold_wrap(community_id: &str, channel_id: &str, wrap_id: &str, wrap: &str, author: &str, at_ms: u64) -> Result<bool, String> {
    let now = now_secs().max(0) as u64 * 1000;
    if at_ms > now.saturating_add(HELD_HORIZON_MS) {
        let held = deferred();
        let mut map = held.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(wrap_id.to_string(), at_ms);
        // Bounded: one dropped costs only a decrypt on the next read.
        if map.len() > MAX_DEFERRED {
            if let Some(furthest) = map.iter().max_by_key(|(_, at)| **at).map(|(id, _)| id.clone()) {
                map.remove(&furthest);
            }
        }
        return Ok(false);
    }
    let _seal = crate::crypto::gate::sealing();
    let enc = enc_txt(wrap)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR IGNORE INTO community_held_wraps (wrap_id, community_id, channel_id, wrap, at_ms, author, bytes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![wrap_id, community_id, channel_id, enc, at_ms as i64, author, enc.len() as i64],
    )
    .map_err(|e| format!("hold wrap: {e}"))?;
    // Nearest now first, across the Community; each row counts against its Channel and author.
    let ranked: Vec<(String, String, String, usize)> = {
        let mut stmt = conn
            .prepare("SELECT wrap_id, channel_id, author, bytes FROM community_held_wraps WHERE community_id = ?1 ORDER BY ABS(at_ms - ?2), wrap_id")
            .map_err(|e| format!("rank held wraps: {e}"))?;
        let rows = stmt
            .query_map(params![community_id, now as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?.max(0) as usize)))
            .map_err(|e| format!("rank held wraps: {e}"))?;
        rows.flatten().collect()
    };
    let (mut channels, mut authors) = (std::collections::HashMap::<&str, (usize, usize)>::new(), std::collections::HashMap::<(&str, &str), usize>::new());
    let mut evicted: Vec<&String> = Vec::new();
    let mut kept: Vec<(&String, &str, usize)> = Vec::new();
    for (id, ch, who, len) in &ranked {
        let c = channels.entry(ch.as_str()).or_default();
        let a = authors.entry((ch.as_str(), who.as_str())).or_default();
        if c.0 >= MAX_HELD_WRAPS || c.1 + len > MAX_HELD_BYTES || (!who.is_empty() && *a >= MAX_HELD_PER_AUTHOR) {
            evicted.push(id);
            continue;
        }
        c.0 += 1;
        c.1 += len;
        *a += 1;
        kept.push((id, ch.as_str(), *len));
    }
    // Past the Community's bytes, the Channel holding the most gives up its furthest-dated first,
    // so one Channel's flood never empties another.
    let mut total: usize = kept.iter().map(|k| k.2).sum();
    while total > MAX_HELD_COMMUNITY_BYTES {
        let most = channels.values().map(|c| c.1).max().unwrap_or(0);
        let Some(i) = kept.iter().rposition(|k| channels.get(k.1).is_some_and(|c| c.1 == most)) else { break };
        let (id, ch, len) = kept.remove(i);
        if let Some(c) = channels.get_mut(ch) {
            c.1 -= len;
        }
        total -= len;
        evicted.push(id);
    }
    for id in &evicted {
        conn.execute("DELETE FROM community_held_wraps WHERE wrap_id = ?1", params![id]).map_err(|e| format!("trim held wraps: {e}"))?;
        conn.execute("DELETE FROM events WHERE wrapper_event_id = ?1 AND instr(tags, '[\"unauthorized\"]') > 0", params![id]).map_err(|e| format!("trim held copy: {e}"))?;
    }
    Ok(!evicted.iter().any(|id| *id == wrap_id))
}

/// Note an Audit Log act no Control edition records (a moderator's delete, a voice directive),
/// keyed by the event that carried it so a re-delivery notes nothing twice.
pub fn record_audit_local(community_id: &str, id: &str, at_ms: u64, entry: &str) -> Result<(), String> {
    let _seal = crate::crypto::gate::sealing();
    let enc = enc_txt(entry)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "INSERT OR IGNORE INTO community_audit_local (id, community_id, at_ms, entry) VALUES (?1, ?2, ?3, ?4)",
        params![id, community_id, at_ms as i64, enc],
    )
    .map_err(|e| format!("record audit: {e}"))?;
    Ok(())
}

/// Every act [`record_audit_local`] noted for `community_id`: `(id, at_ms, entry)`.
pub fn get_audit_local(community_id: &str) -> Result<Vec<(String, u64, String)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT id, at_ms, entry FROM community_audit_local WHERE community_id = ?1 ORDER BY at_ms")
        .map_err(|e| format!("read audit: {e}"))?;
    let rows = stmt
        .query_map(params![community_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64, r.get::<_, String>(2)?)))
        .map_err(|e| format!("read audit: {e}"))?;
    Ok(rows.flatten().map(|(id, at, entry)| (id, at, dec_txt(&entry))).collect())
}

/// The Channels of `community_id` holding wraps.
pub fn held_wrap_channels(community_id: &str) -> Result<Vec<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT DISTINCT channel_id FROM community_held_wraps WHERE community_id = ?1").map_err(|e| format!("held channels: {e}"))?;
    let rows = stmt.query_map(params![community_id], |r| r.get(0)).map_err(|e| format!("held channels: {e}"))?;
    Ok(rows.flatten().collect())
}

/// The ids of the wraps held for one Channel.
pub fn held_wrap_ids_in(community_id: &str, channel_id: &str) -> Result<Vec<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn.prepare("SELECT wrap_id FROM community_held_wraps WHERE community_id = ?1 AND channel_id = ?2").map_err(|e| format!("held ids: {e}"))?;
    let rows = stmt.query_map(params![community_id, channel_id], |r| r.get(0)).map_err(|e| format!("held ids: {e}"))?;
    Ok(rows.flatten().collect())
}

/// The wraps held for one Channel, as `(wrap_id, wrap)`, oldest-dated first.
pub fn held_wraps_in(community_id: &str, channel_id: &str) -> Result<Vec<(String, String)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT wrap_id, wrap FROM community_held_wraps WHERE community_id = ?1 AND channel_id = ?2 ORDER BY at_ms")
        .map_err(|e| format!("held wraps: {e}"))?;
    let rows = stmt.query_map(params![community_id, channel_id], |r| Ok((r.get(0)?, r.get::<_, String>(1)?))).map_err(|e| format!("held wraps: {e}"))?;
    Ok(rows.flatten().map(|(id, wrap)| (id, dec_txt(&wrap))).collect())
}

/// Every wrap held for `community_id`, as `(wrap_id, channel_id, wrap)`, oldest-dated first.
pub fn held_wraps(community_id: &str) -> Result<Vec<(String, String, String)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT wrap_id, channel_id, wrap FROM community_held_wraps WHERE community_id = ?1 ORDER BY at_ms")
        .map_err(|e| format!("held wraps: {e}"))?;
    let rows = stmt.query_map(params![community_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))).map_err(|e| format!("held wraps: {e}"))?;
    Ok(rows.flatten().map(|(id, ch, wrap)| (id, ch, dec_txt(&wrap))).collect())
}

/// Whether `community_id` holds any wrap.
pub fn holds_wraps(community_id: &str) -> bool {
    super::get_db_connection_guard_static()
        .ok()
        .and_then(|conn| conn.query_row("SELECT 1 FROM community_held_wraps WHERE community_id = ?1 LIMIT 1", params![community_id], |_| Ok(())).optional().ok().flatten())
        .is_some()
}

/// Whether `channel_id` holds a wrap by `author` (hex) sent to `address` (a writer address),
/// waiting for a later judgment.
pub fn holds_wraps_at(community_id: &str, channel_id: &str, author: &str, address: &[u8; 32]) -> bool {
    let wraps: Vec<String> = {
        let Ok(conn) = super::get_db_connection_guard_static() else { return false };
        let Ok(mut stmt) = conn.prepare("SELECT wrap FROM community_held_wraps WHERE community_id = ?1 AND channel_id = ?2 AND author = ?3") else { return false };
        let Ok(rows) = stmt.query_map(params![community_id, channel_id, author], |r| r.get::<_, String>(0)) else { return false };
        rows.flatten().collect()
    };
    wraps.iter().any(|w| crate::event_json::event(&dec_txt(w)).is_ok_and(|e| e.pubkey.to_bytes() == *address))
}

struct DeferredWraps;

pub const MAX_DEFERRED: usize = 4096;

/// Wraps refused for a date past the horizon, by id: a read skips each until its date nears.
fn deferred() -> std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>> {
    super::current_session().scoped::<DeferredWraps, _>()
}

/// Of `wrap_ids`, those held, or deferred while their date is still past the horizon.
pub fn held_wrap_ids(wrap_ids: &[String]) -> std::collections::HashSet<String> {
    let now = now_secs().max(0) as u64 * 1000;
    let waiting: std::collections::HashSet<String> = {
        let held = deferred();
        let mut map = held.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, at| *at > now.saturating_add(HELD_HORIZON_MS));
        wrap_ids.iter().filter(|id| map.contains_key(*id)).cloned().collect()
    };
    let mut out = held_wrap_rows(wrap_ids);
    out.extend(waiting);
    out
}

fn held_wrap_rows(wrap_ids: &[String]) -> std::collections::HashSet<String> {
    let Ok(conn) = super::get_db_connection_guard_static() else { return Default::default() };
    let Ok(json) = serde_json::to_string(wrap_ids) else { return Default::default() };
    let Ok(mut stmt) = conn.prepare("SELECT wrap_id FROM community_held_wraps WHERE wrap_id IN (SELECT value FROM json_each(?1))") else { return Default::default() };
    stmt.query_map(params![json], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// Let `wrap_id` go: it stands now.
pub fn release_held_wrap(wrap_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute("DELETE FROM community_held_wraps WHERE wrap_id = ?1", params![wrap_id]).map_err(|e| format!("release held wrap: {e}"))?;
    Ok(())
}

/// A channel's stored Pin List head: `(raw content, version)`, `None` when no
/// edition has ever folded for it.
pub fn get_community_pins(community_id: &str, channel_id: &str) -> Result<Option<(String, i64)>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT content, version FROM community_pins WHERE community_id = ?1 AND channel_id = ?2",
            params![community_id, channel_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("get pins: {e}"))?;
    Ok(row.map(|(content, version)| (dec_txt(&content), version)))
}
