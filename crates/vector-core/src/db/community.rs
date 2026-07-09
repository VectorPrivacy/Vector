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
use nostr_sdk::ToBech32;
use rusqlite::{params, OptionalExtension};

use crate::community::{Channel, ChannelId, ChannelKey, Community, CommunityId, Epoch, ServerRootKey};

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
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

/// Persist a Community and all its channels (upsert). Secrets are stored as raw
/// blobs in the account-scoped DB.
pub fn save_community(community: &Community) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    let relays_json = serde_json::to_string(&community.relays).map_err(|e| e.to_string())?;
    let community_id = community.id.to_hex();

    // icon/banner persisted as the CommunityImage JSON ref (None → NULL).
    let icon_json = community
        .icon
        .as_ref()
        .map(|i| serde_json::to_string(i))
        .transpose()
        .map_err(|e| e.to_string())?;
    let banner_json = community
        .banner
        .as_ref()
        .map(|b| serde_json::to_string(b))
        .transpose()
        .map_err(|e| e.to_string())?;
    // Atomic: the community row + all its channel rows commit together, so a crash mid-save
    // can't leave a Community with a partial channel set.
    let tx = conn.unchecked_transaction().map_err(|e| format!("save community tx: {e}"))?;
    // UPSERT (not INSERT OR REPLACE): a metadata re-save must NOT reset `banlist` (managed
    // separately via set_community_banlist) or `created_at` to their defaults — REPLACE deletes
    // the row first, so omitted columns would revert.
    // Wrap secrets + identifying metadata before they touch disk (no-op when encryption is off).
    let enc_root = enc_key(community.server_root_key.as_bytes())?;
    let enc_name = enc_txt(&community.name)?;
    let enc_relays = enc_txt(&relays_json)?;
    let enc_desc = enc_txt_opt(&community.description)?;
    let enc_icon = enc_txt_opt(&icon_json)?;
    let enc_banner = enc_txt_opt(&banner_json)?;
    let enc_owner = enc_txt_opt(&community.owner_attestation)?;
    tx.execute(
        "INSERT INTO communities
            (community_id, server_root_key, name, relays, created_at,
             description, icon, banner, owner_attestation, server_root_epoch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(community_id) DO UPDATE SET
            server_root_key=excluded.server_root_key, name=excluded.name, relays=excluded.relays,
            description=excluded.description, icon=excluded.icon, banner=excluded.banner,
            owner_attestation=excluded.owner_attestation, server_root_epoch=excluded.server_root_epoch",
        params![
            community_id,
            &enc_root[..],
            enc_name,
            enc_relays,
            now_secs(),
            enc_desc,
            enc_icon,
            enc_banner,
            enc_owner,
            community.server_root_epoch.0 as i64,
        ],
    )
    .map_err(|e| format!("save community: {e}"))?;

    // Archive the base key at its epoch so a future rotation can't clobber it (multi-held keys).
    store_epoch_key_tx(&tx, &community_id, crate::community::SERVER_ROOT_SCOPE_HEX,
        community.server_root_epoch.0, community.server_root_key.as_bytes())?;

    for channel in &community.channels {
        let enc_chan_key = enc_key(channel.key.as_bytes())?;
        let enc_chan_name = enc_txt(&channel.name)?;
        // UPSERT (not INSERT OR REPLACE): a re-save must preserve `created_at` (channel ordering) and
        // `rekeyed_at_server_epoch` (read-cut resume progress) — REPLACE would reset both to defaults.
        tx.execute(
            "INSERT INTO community_channels
                (channel_id, community_id, channel_key, epoch, name, created_at, rekeyed_at_server_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(channel_id) DO UPDATE SET
                community_id=excluded.community_id, channel_key=excluded.channel_key,
                epoch=excluded.epoch, name=excluded.name",
            params![
                channel.id.to_hex(),
                community_id,
                &enc_chan_key[..],
                // SQLite INTEGER is i64; reinterpret the u64 epoch bit-for-bit
                // (lossless two's-complement). NOTE: a signed SQL comparison would
                // mis-order epochs >= 2^63 — don't ORDER BY / range-filter `epoch`.
                channel.epoch.0 as i64,
                enc_chan_name,
                now_secs(),
                // A newly-inserted channel is current as of the community's base epoch (no cut owed for it).
                community.server_root_epoch.0 as i64,
            ],
        )
        .map_err(|e| format!("save channel: {e}"))?;
        // Mirror the channel's current-epoch key into the multi-held archive. The
        // `community_channels` row above is just the head pointer (REPLACE clobbers it); the archive
        // (PK includes epoch) retains EVERY epoch key so cross-epoch history stays readable post-rekey.
        store_epoch_key_tx(&tx, &community_id, &channel.id.to_hex(), channel.epoch.0, channel.key.as_bytes())?;
    }
    tx.commit().map_err(|e| format!("save community commit: {e}"))?;
    Ok(())
}

/// Store one held epoch key in the multi-held archive. `scope_id` is a channel_id hex or
/// [`crate::community::SERVER_ROOT_SCOPE_HEX`]. The `(community, scope, epoch)` PK makes a write for
/// one epoch unable to disturb another epoch's key — so retained history survives a rekey. Uses
/// REPLACE on the exact coordinate so the fork-resolution apply path can commit the *winning*
/// key for a contested epoch over a previously-stored loser (the only legitimate same-coordinate
/// overwrite; an epoch key is otherwise immutable).
pub fn store_epoch_key(community_id: &str, scope_id: &str, epoch: u64, key: &[u8; 32]) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    store_epoch_key_tx(&conn, community_id, scope_id, epoch, key)
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
    Ok(advanced)
}

/// Apply a received SERVER-ROOT (base) rekey's new root — the base counterpart to
/// [`advance_channel_epoch`], atomic: in ONE transaction, ARCHIVE `(server-root scope, new_epoch) ->
/// new_root` in `community_epoch_keys` AND advance the base head (`communities.server_root_epoch` +
/// `server_root_key`) iff `new_epoch` exceeds the current base epoch (monotonic, compared in RUST). A
/// caught-up OLDER base epoch is archived (its control/base history stays decryptable) but never
/// regresses the head. Returns whether the head advanced.
pub fn advance_server_root_epoch(community_id: &str, new_epoch: u64, new_root: &[u8; 32]) -> Result<bool, String> {
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
    Ok(advanced)
}

/// SAME-EPOCH convergence for the server root (concurrent re-founding heal): two BAN-holders who
/// re-founded at the same time each sit on their OWN root at the SAME epoch. `advance_server_root_epoch`
/// refuses to switch (its guard is strictly monotonic), so this is the sibling that REPLACES the head root
/// at `epoch` with the deterministic winner (lowest root bytes — the caller decides). Archives the new root
/// + swaps the head, but ONLY while we're still AT `epoch` (a later real rotation must win over a stale
/// converge). Returns whether it switched.
pub fn converge_server_root_epoch(community_id: &str, epoch: u64, new_root: &[u8; 32]) -> Result<bool, String> {
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
    Ok(switched)
}

/// SAME-EPOCH convergence for a channel key (concurrent re-founding heal) — the channel counterpart to
/// [`converge_server_root_epoch`]. Adopts the winning re-founding's channel key at `epoch` (the rekey
/// addressed under the converged server root), replacing the one we minted in our own losing fork. Switches
/// only while the channel is still AT `epoch`. Returns whether it switched.
pub fn converge_channel_epoch(community_id: &str, channel_id: &str, epoch: u64, new_key: &[u8; 32]) -> Result<bool, String> {
    let conn = super::get_write_connection_guard_static()?;
    let tx = conn.unchecked_transaction().map_err(|e| format!("converge channel tx: {e}"))?;
    store_epoch_key_tx(&tx, community_id, channel_id, epoch, new_key)?;
    let enc = enc_key(new_key)?;
    let switched = tx
        .execute(
            "UPDATE community_channels SET channel_key = ?1 WHERE community_id = ?2 AND channel_id = ?3 AND epoch = ?4",
            params![&enc[..], community_id, channel_id, epoch as i64],
        )
        .map_err(|e| format!("converge channel head: {e}"))?
        > 0;
    tx.commit().map_err(|e| format!("converge channel commit: {e}"))?;
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

/// Local first-save time of a community (≈ when this account joined or created it), in ms.
/// `created_at` is set on the first save and preserved across metadata re-saves, so it tracks
/// the join moment. Used to sort a not-yet-active community by join time. `None` if unknown.
pub fn community_created_at_ms(id: &CommunityId) -> Option<u64> {
    let conn = super::get_db_connection_guard_static().ok()?;
    conn.query_row(
        "SELECT created_at FROM communities WHERE community_id = ?1",
        params![id.to_hex()],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .ok()
    .flatten()
    .map(|secs| (secs.max(0) as u64) * 1000)
}

/// Load a Community and its channels by id. Returns `None` if not stored locally.
pub fn load_community(id: &CommunityId) -> Result<Option<Community>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let id_hex = id.to_hex();

    let row = conn
        .query_row(
            "SELECT server_root_key, name, relays,
                    description, icon, banner, banlist, owner_attestation, server_root_epoch, dissolved
               FROM communities WHERE community_id = ?1",
            params![id_hex],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()
        .map_err(|e| format!("load community: {e}"))?;

    let (root_blob, name, relays_json, description, icon_json, banner_json, banlist_json, owner_attestation, server_root_epoch, dissolved_int) =
        match row {
            Some(t) => t,
            None => return Ok(None),
        };
    let dissolved = dissolved_int != 0;

    // Unwrap at-rest encryption before parsing (no-op when off, or for not-yet-wrapped rows).
    let name = dec_txt(&name);
    let relays_json = dec_txt(&relays_json);
    let description = description.map(|s| dec_txt(&s));
    let icon_json = icon_json.map(|s| dec_txt(&s));
    let banner_json = banner_json.map(|s| dec_txt(&s));
    let banlist_json = dec_txt(&banlist_json);
    let owner_attestation = owner_attestation.map(|s| dec_txt(&s));

    // Banlist: stored as a JSON array of hex pubkeys; parse to PublicKeys (skipping any
    // malformed entry) and denormalize onto every channel so the inbound path can drop
    // banned authors. A bad/empty column degrades to "no bans", never an error.
    let banned: Vec<PublicKey> = serde_json::from_str::<Vec<String>>(&banlist_json)
        .unwrap_or_default()
        .iter()
        .filter_map(|h| PublicKey::from_hex(h).ok())
        .collect();

    let icon = icon_json
        .map(|j| serde_json::from_str(&j))
        .transpose()
        .map_err(|e| format!("icon json: {e}"))?;
    let banner = banner_json
        .map(|j| serde_json::from_str(&j))
        .transpose()
        .map_err(|e| format!("banner json: {e}"))?;

    let server_root_key = ServerRootKey(dec_key(&root_blob)?);
    let relays: Vec<String> = serde_json::from_str(&relays_json).map_err(|e| e.to_string())?;

    // hierarchy invariant (apply-time): the OWNER is the uppermost role and can never be
    // effectively banned or hidden — by anyone. Everyone knows the owner from the attestation, so
    // ALL members enforce this (the owner is filtered out of `banned` and protected from hides).
    // Admins are NOT absolutely protected: the owner outranks them and CAN ban/hide an admin.
    // (Admin-vs-admin peer protection — a lower rank can't act on an equal — is a later
    // position-relative refinement gated on the author proof; the owner protection is the invariant.)
    let mut protected: Vec<PublicKey> = Vec::new();
    if let Some(owner) = owner_attestation
        .as_ref()
        .and_then(|att| crate::community::owner::verify_owner_attestation(att, &id_hex))
    {
        protected.push(owner);
    }
    let banned: Vec<PublicKey> = banned.into_iter().filter(|pk| !protected.contains(pk)).collect();

    // Collect the channel head rows FIRST (drops the borrow on `conn`) so we can then query each
    // channel's full epoch-key archive on the same connection without a borrow conflict.
    let raw_channels: Vec<(String, Vec<u8>, i64, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT channel_id, channel_key, epoch, name
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
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };

    // The AUTHORIZED roster (cached by fetch_and_apply_roles, post delegation check), denormalized
    // onto each channel so the inbound delete path can verify a keyless moderation-hide.
    let roster = get_community_roles(&id_hex).unwrap_or_default();

    let mut channels = Vec::new();
    for (cid_hex, key_blob, epoch, cname) in raw_channels {
        // Every retained epoch key for this channel (multi-held archive), so the read path can fetch +
        // decrypt across rekeys. Best-effort: a read hiccup degrades to the head epoch (read_epoch_keys
        // falls back), never an error.
        let epoch_keys: Vec<(Epoch, crate::community::ChannelKey)> = {
            let mut ek_stmt = conn
                .prepare("SELECT epoch, key FROM community_epoch_keys WHERE community_id = ?1 AND scope_id = ?2")
                .map_err(|e| e.to_string())?;
            let rows = ek_stmt
                .query_map(params![id_hex, cid_hex], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                let (e, blob) = row.map_err(|e| e.to_string())?;
                if let Ok(k) = dec_key(&blob) {
                    out.push((Epoch(e as u64), crate::community::ChannelKey(k)));
                }
            }
            out
        };
        channels.push(Channel {
            id: ChannelId(hex_id_to_32(&cid_hex)?),
            key: ChannelKey(dec_key(&key_blob)?),
            // Stored as i64; reinterpreted back to u64 (two's-complement is exact,
            // so the bit pattern round-trips losslessly even for epoch >= 2^63).
            epoch: Epoch(epoch as u64),
            name: dec_txt(&cname),
            banned: banned.clone(),
            protected: protected.clone(),
            roster: roster.clone(),
            epoch_keys,
            dissolved,
        });
    }

    Ok(Some(Community {
        id: *id,
        server_root_key,
        // Stored as i64; reinterpreted to u64 (two's-complement is exact), same as channel epochs.
        server_root_epoch: Epoch(server_root_epoch as u64),
        name,
        description,
        icon,
        banner,
        relays,
        channels,
        owner_attestation,
        dissolved,
    }))
}

/// Retain the ephemeral signing key of a message I published, so I can later
/// NIP-09-delete it. `relays` is where the deletion must be sent.
pub fn store_message_key(
    message_id: &str,
    outer_event_id: &str,
    ephemeral: &Keys,
    relays: &[String],
) -> Result<(), String> {
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

/// The hex id of the Community that owns `channel_id`, if any is stored locally. Used to
/// resolve a channel-addressed chat back to its Community for sending.
pub fn community_id_for_channel(channel_id: &str) -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    conn.query_row(
        "SELECT community_id FROM community_channels WHERE channel_id = ?1",
        params![channel_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| format!("community_id_for_channel: {e}"))
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
}

/// Park an inbound invite bundle for explicit user consent (the carrier never
/// auto-joins). First-invite-wins: `INSERT OR IGNORE` means a later invite for the
/// same `community_id` can't silently rewrite a parked bundle. Returns whether a new
/// row was inserted (`false` = already pending, caller should not re-notify).
pub fn save_pending_invite(
    community_id: &str,
    bundle_json: &str,
    inviter_npub: &str,
) -> Result<bool, String> {
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
                (community_id, bundle_json, inviter_npub, received_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![community_id, enc_bundle, enc_inviter, now_secs()],
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

/// All parked invites, newest first.
pub fn list_pending_invites() -> Result<Vec<PendingCommunityInvite>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare(
            "SELECT community_id, bundle_json, inviter_npub, received_at
               FROM pending_community_invites ORDER BY received_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PendingCommunityInvite {
                community_id: r.get(0)?,
                bundle_json: dec_txt(&r.get::<_, String>(1)?),
                inviter_npub: dec_txt(&r.get::<_, String>(2)?),
                received_at: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Read a parked invite's bundle WITHOUT removing it. Accept is fallible (caps,
/// owner/authority collision), so the row must survive a rejected accept — peek here,
/// then [`delete_pending_invite`] only after the join succeeds.
pub fn get_pending_invite(community_id: &str) -> Result<Option<String>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT bundle_json FROM pending_community_invites WHERE community_id = ?1",
            params![community_id],
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

/// Whether an invite for this id is already parked (inbound dedup).
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
    if let Some(me) = crate::state::my_public_key().and_then(|pk| pk.to_bech32().ok()) {
        if let Ok(counts) = community_invite_join_counts(community_id, &me) {
            for rec in &mut out {
                if let Some(l) = rec.label.as_deref() {
                    rec.join_count = counts.get(l).copied().unwrap_or(0);
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

/// Insert a public-invite row only if its (decrypted) token isn't already present — idempotent hydration
/// from the synced Invite List, PRESERVING the original `created_at` (unlike `save_public_invite`, which
/// stamps now). Returns true if a row was inserted. Tokens are stored encrypted with a random nonce, so SQL
/// equality can't dedup; scan + decrypt (few rows per community, owner-only).
pub fn upsert_public_invite(
    token: &str,
    community_id: &str,
    url: &str,
    expires_at: Option<i64>,
    created_at: i64,
    label: Option<&str>,
) -> Result<bool, String> {
    let conn = super::get_write_connection_guard_static()?;
    let already = {
        let mut stmt = conn
            .prepare("SELECT token FROM community_public_invites WHERE community_id = ?1")
            .map_err(|e| e.to_string())?;
        let stored: Vec<String> = stmt
            .query_map(params![community_id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        stored.iter().any(|s| dec_txt(s) == token)
    };
    if already {
        return Ok(false);
    }
    let enc_token = enc_txt(token)?;
    let enc_url = enc_txt(url)?;
    let enc_label = label.map(enc_txt).transpose()?;
    conn.execute(
        "INSERT INTO community_public_invites
            (token, community_id, url, expires_at, created_at, label)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![enc_token, community_id, enc_url, expires_at, created_at, enc_label],
    )
    .map_err(|e| format!("upsert public invite: {e}"))?;
    Ok(true)
}

/// Remove a Community and all its local state (channels, retained message keys, parked
/// invites, minted public-invite tokens). Used when the user leaves a Community — there
/// is no protocol "leave" (membership is key possession), so leaving is purely local:
/// drop the keys + stop subscribing.
pub fn delete_community(community_id: &str) -> Result<(), String> {
    delete_community_inner(community_id, false)
}

/// self-removal teardown: drop all local community state EXCEPT the held epoch keys
/// (`community_epoch_keys`). Read access to future epochs is already gone (the post-removal
/// keys are never delivered); retaining the OLD keys only preserves the ability to author a
/// `3305` self-delete of one's own past messages, each sealed under the epoch key it was sent
/// at. Used by every self-removal trigger (voluntary leave, kick of me, ban-rekey exclusion).
pub fn delete_community_retain_keys(community_id: &str) -> Result<(), String> {
    delete_community_inner(community_id, true)
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
        Some("DELETE FROM community_public_invites WHERE community_id = ?1"),
        Some("DELETE FROM community_invite_link_sets WHERE community_id = ?1"),
        Some("DELETE FROM pending_community_invites WHERE community_id = ?1"),
        // Per-entity edition heads (keyless model) — else stale refuse-downgrade floors + self_hash
        // anchors survive a leave/re-join and reject a legitimately reset chain.
        Some("DELETE FROM community_edition_heads WHERE community_id = ?1"),
    ]
    .into_iter()
    .flatten()
    {
        tx.execute(sql, params![community_id])
            .map_err(|e| format!("delete community: {e}"))?;
    }
    tx.commit().map_err(|e| format!("delete community commit: {e}"))?;
    // `community_message_keys` is INTENTIONALLY left intact: those are our OWN ephemeral signing keys for
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
    /// Cap on rendered members — bounds a presence-flood (fresh-identity 3306 spam) from
    /// growing the list / profile-fetch fan-out without limit.
    const COMMUNITY_MEMBER_CAP: usize = 500;
    use std::collections::HashMap;

    let community = match load_community(&CommunityId(hex_id_to_32(community_id)?))? {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };
    // The proven owner is ALWAYS a member of their own community. Seed them so a freshly-created
    // community (no message/presence events yet) still shows its creator instead of an empty roster.
    // `now_secs()` is just a presence baseline; real activity below overwrites it, and the UI re-sorts
    // by role tier regardless.
    let owner_b32: Option<String> = community
        .owner_attestation
        .as_deref()
        .and_then(|att| crate::community::owner::verify_owner_attestation(att, community_id))
        .and_then(|pk| pk.to_bech32().ok());

    // Map each channel's hex id → its integer chat row id (skip channels with no events yet).
    let mut chat_ints: Vec<i64> = Vec::new();
    for ch in &community.channels {
        if let Ok(cid) = super::id_cache::get_chat_id_by_identifier(&ch.id.to_hex()) {
            chat_ints.push(cid);
        }
    }

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
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (npub, at, tags_json) = row.map_err(|e| e.to_string())?;
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

    // Exclude banned (banlist is hex; events store bech32 — compare on bech32). Denormalized
    // identically onto every channel at load, so reading channels[0] is sufficient.
    let banned: std::collections::HashSet<String> = community
        .channels
        .first()
        .map(|c| c.banned.iter().filter_map(|pk| pk.to_bech32().ok()).collect())
        .unwrap_or_default();

    // Member iff active, not banned, and last activity is at-or-after the last leave.
    let mut out: Vec<(String, u64)> = active
        .into_iter()
        .filter(|(npub, at)| !banned.contains(npub) && left.get(npub).map_or(true, |l| at >= l))
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
                out.push((npub, now_secs() as u64));
            }
        };
        if let Some(o) = owner_b32 {
            reassert(o);
        }
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
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out.truncate(COMMUNITY_MEMBER_CAP);
    Ok(out)
}

/// Per-link join counts for the owner's public invites: `label -> distinct joiners` who joined
/// via a link minted by `inviter_npub` (bech32). Reads the `invited-by` / `invited-label` tags on
/// MemberJoined system events; distinct by joiner npub so a rejoin isn't double-counted. Labels are
/// unique per creator (random fallback ensures it), so (inviter, label) keys a single link.
pub fn community_invite_join_counts(
    community_id: &str,
    inviter_npub: &str,
) -> Result<std::collections::HashMap<String, u64>, String> {
    use std::collections::{HashMap, HashSet};
    let community = match load_community(&CommunityId(hex_id_to_32(community_id)?))? {
        Some(c) => c,
        None => return Ok(HashMap::new()),
    };
    let mut chat_ints: Vec<i64> = Vec::new();
    for ch in &community.channels {
        if let Ok(cid) = super::id_cache::get_chat_id_by_identifier(&ch.id.to_hex()) {
            chat_ints.push(cid);
        }
    }
    if chat_ints.is_empty() {
        return Ok(HashMap::new());
    }
    let sys = crate::stored_event::event_kind::APPLICATION_SPECIFIC;
    let conn = super::get_db_connection_guard_static()?;
    let placeholders = chat_ints.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT npub, tags FROM events \
         WHERE chat_id IN ({placeholders}) AND kind = {sys} AND npub IS NOT NULL AND npub != ''"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(chat_ints.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    // label -> set of distinct joiner npubs
    let mut per_label: HashMap<String, HashSet<String>> = HashMap::new();
    for row in rows {
        let (joiner, tags_json) = row.map_err(|e| e.to_string())?;
        let tags = match serde_json::from_str::<Vec<Vec<String>>>(&tags_json) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let tag_val = |key: &str| -> Option<String> {
            tags.iter()
                .find(|t| t.first().map(|s| s == key).unwrap_or(false))
                .and_then(|t| t.get(1).cloned())
        };
        // MemberJoined (event-type "1") attributed to THIS owner's link, with a label.
        if tag_val("event-type").as_deref() != Some("1") {
            continue;
        }
        if tag_val("invited-by").as_deref() != Some(inviter_npub) {
            continue;
        }
        if let Some(label) = tag_val("invited-label") {
            per_label.entry(label).or_default().insert(joiner);
        }
    }
    Ok(per_label.into_iter().map(|(k, v)| (k, v.len() as u64)).collect())
}

/// Replace a Community's stored banlist (JSON array of hex pubkeys) + the `created_at` (secs) of
/// the edition it came from. `at` is the version: the owner's own ban/unban writes its freshly
/// built event time, and `fetch_and_apply_banlist` only calls this with a strictly-newer edition,
/// so the stored banlist can never roll backwards.
pub fn set_community_banlist(community_id: &str, banned_hex: &[String], at: i64) -> Result<(), String> {
    let json = enc_txt(&serde_json::to_string(banned_hex).map_err(|e| e.to_string())?)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET banlist = ?1, banlist_at = ?2 WHERE community_id = ?3",
        params![json, at, community_id],
    )
    .map_err(|e| format!("set banlist: {e}"))?;
    Ok(())
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
    let json = enc_txt(&serde_json::to_string(roles).map_err(|e| e.to_string())?)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET roles = ?1, roles_at = ?2 WHERE community_id = ?3",
        params![json, at, community_id],
    )
    .map_err(|e| format!("set roles: {e}"))?;
    Ok(())
}

/// A Community's cached role graph. Empty (default) for an unknown community or none stored.
pub fn get_community_roles(
    community_id: &str,
) -> Result<crate::community::roles::CommunityRoles, String> {
    let conn = super::get_db_connection_guard_static()?;
    let json: Option<String> = conn
        .query_row(
            "SELECT roles FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get roles: {e}"))?;
    Ok(json.and_then(|j| serde_json::from_str(&dec_txt(&j)).ok()).unwrap_or_default())
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
/// `inner_id` is the head edition's deterministic tiebreak key (used only by [`converge_edition_head`]).
pub fn set_edition_head(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32]) -> Result<(), String> {
    set_edition_head_inner(community_id, entity_id, version, self_hash, None)
}

/// As [`set_edition_head`], but also records the head edition's `inner_id` (the deterministic tiebreak
/// key), so a later same-version convergence can rank against it. A plain advance carries it through.
pub fn set_edition_head_with_id(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32]) -> Result<(), String> {
    set_edition_head_inner(community_id, entity_id, version, self_hash, Some(inner_id))
}

fn set_edition_head_inner(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: Option<&[u8; 32]>) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // MONOTONIC, EPOCH-PRIMARY: the head IS the refuse-downgrade floor. The recorded `epoch` is
    // the community's current server-root epoch (re-founding bumps it + resets versions to 1). A higher
    // epoch ALWAYS supersedes (so a re-founding's v1 lands over a held v21); within an epoch, version
    // still only advances. So a stale/hostile rollback can lower neither the epoch nor the in-epoch version.
    conn.execute(
        "INSERT INTO community_edition_heads (community_id, entity_id, version, self_hash, inner_id, epoch)
         VALUES (?1, ?2, ?3, ?4, ?5, COALESCE((SELECT server_root_epoch FROM communities WHERE community_id = ?1), 0))
         ON CONFLICT(community_id, entity_id) DO UPDATE SET
            version = excluded.version,
            self_hash = excluded.self_hash,
            inner_id = excluded.inner_id,
            epoch = excluded.epoch
         WHERE excluded.epoch > community_edition_heads.epoch
            OR (excluded.epoch = community_edition_heads.epoch AND excluded.version > community_edition_heads.version)",
        params![community_id, entity_id, version as i64, self_hash.as_slice(), inner_id.map(|i| i.as_slice())],
    )
    .map_err(|e| format!("set edition head: {e}"))?;
    Ok(())
}

/// Converge the head to a same-version fork winner (concurrent-edit resolution). Unlike
/// [`set_edition_head`] (which only ADVANCES the version), this resolves a fork AT the current version:
/// two authorized editors editing concurrently from the same base both produce `version`, and every
/// client must adopt the SAME one. The winner is the lower deterministic `inner_id`, so this update
/// fires only when the incoming edition ties the stored version AND carries a strictly lower `inner_id`
/// — monotonic toward the global minimum, so it can never flip-flop (a relay can't churn the head by
/// reordering, and a held row with a NULL `inner_id`, pre-migration, is treated as "always replaceable"
/// so it heals to a ranked id). The version-advance path is unchanged and still handled by
/// [`set_edition_head_with_id`]; callers run BOTH (advance covers v+1, converge covers a same-v fork).
pub fn converge_edition_head(community_id: &str, entity_id: &str, version: u64, self_hash: &[u8; 32], inner_id: &[u8; 32]) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // Scoped to the CURRENT epoch's head: a fork is resolved within an epoch, never across one (an epoch
    // bump is a re-founding, handled by the advance path). `epoch` matches the community's current epoch.
    conn.execute(
        "UPDATE community_edition_heads
            SET self_hash = ?4, inner_id = ?5
          WHERE community_id = ?1 AND entity_id = ?2
            AND version = ?3
            AND epoch = COALESCE((SELECT server_root_epoch FROM communities WHERE community_id = ?1), 0)
            AND (inner_id IS NULL OR ?5 < inner_id)",
        params![community_id, entity_id, version as i64, self_hash.as_slice(), inner_id.as_slice()],
    )
    .map_err(|e| format!("converge edition head: {e}"))?;
    Ok(())
}

/// The held head's tiebreak key (`inner_id`), or `None` if unheld or pre-migration (NULL). The consumer
/// uses this to decide a same-version convergence exactly as [`converge_edition_head`]'s SQL does (a
/// NULL/None held id is "always replaceable") — so it never applies a display edit the head write would
/// then refuse.
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


/// Every tracked control entity's persisted head `(entity_id hex → (version, self_hash))`. This is the
/// per-entity refuse-downgrade FLOOR: the fold seeds each entity's chain from its held head, so a
/// withholding relay serving editions BELOW what we already hold can't roll an authority chain back
/// (e.g. resurrecting a since-revoked admin's old grant). An empty map = a bootstrapping joiner (folds
/// from genesis, floor 0).
pub fn get_all_edition_heads(community_id: &str) -> Result<std::collections::HashMap<String, (u64, [u8; 32])>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let mut stmt = conn
        .prepare("SELECT entity_id, version, self_hash FROM community_edition_heads WHERE community_id = ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![community_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?))
        })
        .map_err(|e| e.to_string())?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (entity, version, hash) = row.map_err(|e| e.to_string())?;
        if hash.len() == 32 {
            let mut h = [0u8; 32];
            h.copy_from_slice(&hash);
            out.insert(entity, (version as u64, h));
        }
    }
    Ok(out)
}

/// Every tracked head as `entity_hex → (epoch, version, self_hash)` — the epoch-primary floor.
/// The caller seeds the fold with ONLY the entities at the community's CURRENT epoch (a head recorded
/// at a PRIOR epoch belongs to a superseded founding, so its entity folds fresh from the new epoch's v1
/// genesis). This is what lets a re-founding's compacted v1 plane land without a version-only downgrade.
pub fn get_all_edition_heads_epoched(community_id: &str) -> Result<std::collections::HashMap<String, (u64, u64, [u8; 32])>, String> {
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
    let conn = super::get_db_connection_guard_static()?;
    let json: Option<String> = conn
        .query_row(
            "SELECT banlist FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get banlist: {e}"))?;
    Ok(json.and_then(|j| serde_json::from_str(&dec_txt(&j)).ok()).unwrap_or_default())
}

/// Replace a Community's cached invite-link registry (active link locators, hex), folded from the
/// owner-signed vsk=5 edition. Empty = Private. The version floor lives in `community_edition_heads`
/// (the registry's own entity), so this is just the content cache (mirrors `set_community_banlist`).
pub fn set_community_invite_registry(community_id: &str, link_locators: &[String]) -> Result<(), String> {
    let json = enc_txt(&serde_json::to_string(link_locators).map_err(|e| e.to_string())?)?;
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET invite_registry = ?1 WHERE community_id = ?2",
        params![json, community_id],
    )
    .map_err(|e| format!("set invite registry: {e}"))?;
    Ok(())
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

/// Replace ALL of a Community's per-creator invite-link sets with the freshly-folded set (latest-wins).
/// Replacing wholesale (not upserting) drops a creator who has revoked every link, so the per-creator
/// view stays in lockstep with the flat registry computed in the same fold.
pub fn replace_invite_link_sets(community_id: &str, sets: &[InviteLinkSetRow]) -> Result<(), String> {
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

/// Upsert ONE creator's invite-link set (optimistic local update after the local user mints/revokes their
/// own links, mirroring the flat-registry merge). An empty set removes the row.
pub fn upsert_invite_link_set(community_id: &str, creator_hex: &str, locators: &[String]) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    // `creator` is encrypted (random nonce), so locate any existing row by decrypting + matching.
    let existing_rowid: Option<i64> = {
        let mut stmt = conn
            .prepare("SELECT rowid, creator FROM community_invite_link_sets WHERE community_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![community_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut found = None;
        for row in rows {
            let (rowid, stored) = row.map_err(|e| e.to_string())?;
            if dec_txt(&stored) == creator_hex {
                found = Some(rowid);
                break;
            }
        }
        found
    };
    if locators.is_empty() {
        if let Some(rowid) = existing_rowid {
            conn.execute("DELETE FROM community_invite_link_sets WHERE rowid = ?1", params![rowid])
                .map_err(|e| format!("delete invite-link-set: {e}"))?;
        }
        return Ok(());
    }
    let enc_locators = enc_txt(&serde_json::to_string(locators).map_err(|e| e.to_string())?)?;
    match existing_rowid {
        Some(rowid) => {
            conn.execute(
                "UPDATE community_invite_link_sets SET locators = ?1 WHERE rowid = ?2",
                params![enc_locators, rowid],
            )
            .map_err(|e| format!("upsert invite-link-set: {e}"))?;
        }
        None => {
            let enc_creator = enc_txt(creator_hex)?;
            conn.execute(
                "INSERT INTO community_invite_link_sets (community_id, creator, locators) VALUES (?1, ?2, ?3)",
                params![community_id, enc_creator, enc_locators],
            )
            .map_err(|e| format!("upsert invite-link-set: {e}"))?;
        }
    }
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

/// Mark (or clear) that a PRIVATE-community ban's base re-seal (read-cut) is OUTSTANDING — set when
/// the re-seal is attempted and cleared only when it succeeds, so a transient failure is retried later
/// instead of silently leaving a banned member with read access.
pub fn set_read_cut_pending(community_id: &str, pending: bool) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET read_cut_pending = ?1 WHERE community_id = ?2",
        params![pending as i64, community_id],
    )
    .map_err(|e| format!("set read_cut_pending: {e}"))?;
    Ok(())
}

/// Set the owner-dissolution SEAL on a community — PERMANENT + irreversible (no clear path; there
/// is no un-dissolve). Idempotent: re-setting an already-dissolved community is a harmless no-op. Once
/// set, the control fold stops advancing and the inbound path drops every subsequent event.
pub fn set_community_dissolved(community_id: &str) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET dissolved = 1 WHERE community_id = ?1",
        params![community_id],
    )
    .map_err(|e| format!("set dissolved: {e}"))?;
    Ok(())
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

/// Whether a PRIVATE-community read-cut re-seal is still outstanding (a prior attempt failed). The ban
/// flow retries the re-seal whenever this is set. `false` for an unknown community.
pub fn get_read_cut_pending(community_id: &str) -> Result<bool, String> {
    let conn = super::get_db_connection_guard_static()?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT read_cut_pending FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get read_cut_pending: {e}"))?;
    Ok(v.unwrap_or(0) != 0)
}

/// Set the base epoch a pending read-cut (re-founding) must reach. The re-seal rotates the base only
/// while `server_root_epoch < target`, so a retry never double-rotates a base that already advanced. Set
/// to `server_root_epoch + 1` on a fresh exclusion delta (ban add / privatize); left untouched on a pure
/// resume so the in-flight target is preserved.
pub fn set_read_cut_target_epoch(community_id: &str, target: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE communities SET read_cut_target_epoch = ?1 WHERE community_id = ?2",
        params![target as i64, community_id],
    )
    .map_err(|e| format!("set read_cut_target_epoch: {e}"))?;
    Ok(())
}

/// The base epoch a pending read-cut must reach (see [`set_read_cut_target_epoch`]). `0` for an unknown
/// community. Reinterpreted i64->u64 (lossless) for epochs >= 2^63.
pub fn get_read_cut_target_epoch(community_id: &str) -> Result<u64, String> {
    let conn = super::get_db_connection_guard_static()?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT read_cut_target_epoch FROM communities WHERE community_id = ?1",
            params![community_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get read_cut_target_epoch: {e}"))?;
    Ok(v.unwrap_or(0) as u64)
}

/// The base (server-root) epoch a channel was last rekeyed FOR during a read-cut — the per-channel
/// progress marker that lets a resumed re-founding skip channels already cut. `0` if unknown.
pub fn channel_rekeyed_at_server_epoch(community_id: &str, channel_id: &str) -> Result<u64, String> {
    let conn = super::get_db_connection_guard_static()?;
    let v: Option<i64> = conn
        .query_row(
            "SELECT rekeyed_at_server_epoch FROM community_channels WHERE community_id = ?1 AND channel_id = ?2",
            params![community_id, channel_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("get rekeyed_at_server_epoch: {e}"))?;
    Ok(v.unwrap_or(0) as u64)
}

/// Record that a channel's key has been rotated to cover base epoch `server_epoch` (a read-cut step).
/// Best-effort progress marker: written after the channel rekey lands, so a crash before it just re-rotates
/// the channel on resume (safe, the rekey is monotonic) rather than skipping a channel that needed cutting.
pub fn mark_channel_rekeyed_at_server_epoch(community_id: &str, channel_id: &str, server_epoch: u64) -> Result<(), String> {
    let conn = super::get_write_connection_guard_static()?;
    conn.execute(
        "UPDATE community_channels SET rekeyed_at_server_epoch = ?1 WHERE community_id = ?2 AND channel_id = ?3",
        params![server_epoch as i64, community_id, channel_id],
    )
    .map_err(|e| format!("mark rekeyed_at_server_epoch: {e}"))?;
    Ok(())
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

// ── Concord v2 storage (dual-stack) ──────────────────────────────────────────
//
// v2 communities reuse the shared community tables (migration 65 added the
// `protocol`/`owner_pubkey`/`owner_salt`/`private` columns). The base access key
// rides `server_root_key`/`server_root_epoch` (same role as v1's server root).
// A public channel stores the community_root in `channel_key` as a placeholder
// (its real secret is derived from the root); a private channel stores its own
// key. At-rest encryption reuses the same `enc_*`/`dec_*` helpers.

/// The protocol a stored community runs, or `None` if it isn't held locally.
pub fn community_protocol(id: &CommunityId) -> Result<Option<crate::community::ConcordProtocol>, String> {
    let conn = super::get_db_connection_guard_static()?;
    let n: Option<i64> = conn
        .query_row("SELECT protocol FROM communities WHERE community_id = ?1", params![id.to_hex()], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(n.map(crate::community::ConcordProtocol::from_i64))
}

/// Persist a v2 community + its channels atomically. UPSERT so a metadata
/// re-save preserves banlist/roles (managed by the fold, not here).
pub fn save_community_v2(c: &crate::community::v2::community::CommunityV2) -> Result<(), String> {
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

    let tx = conn.unchecked_transaction().map_err(|e| format!("save v2 community tx: {e}"))?;
    tx.execute(
        "INSERT INTO communities
            (community_id, server_root_key, name, relays, created_at, description,
             server_root_epoch, dissolved, protocol, owner_pubkey, owner_salt)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 2, ?9, ?10)
         ON CONFLICT(community_id) DO UPDATE SET
            server_root_key=?2, name=?3, relays=?4, description=?6,
            server_root_epoch=?7, dissolved=?8, protocol=2, owner_pubkey=?9, owner_salt=?10",
        params![
            id_hex, enc_root, enc_name, enc_relays, created, enc_desc,
            c.root_epoch.0 as i64, c.dissolved as i64, enc_owner_pk, enc_owner_salt,
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
        if owner_of.is_some_and(|existing| existing != id_hex) {
            // SKIP the foreign-owned channel rather than fail the whole save: a
            // single replayed phantom (a same-owner cross-community vsk-2 edition)
            // would otherwise wedge ALL of this community's control-plane persistence
            // on every fold. The foreign row stays untouched; this community just
            // never acquires a row for that id.
            continue;
        }
        // A public channel has no independent key; store the community_root as a
        // placeholder so the NOT NULL column is satisfied (the real secret is
        // derived from the root at read time via `channel_secret`).
        let stored_key = ch.key.unwrap_or(c.community_root);
        let enc_ch_key = enc_key(&stored_key)?;
        let enc_ch_name = enc_txt(&ch.name)?;
        tx.execute(
            "INSERT INTO community_channels
                (channel_id, community_id, channel_key, epoch, name, created_at, private)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(channel_id) DO UPDATE SET
                channel_key=?3, epoch=?4, name=?5, private=?7",
            params![ch_hex, id_hex, enc_ch_key, ch.epoch.0 as i64, enc_ch_name, created, ch.private as i64],
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
        let placeholders = std::iter::repeat("?").take(keep.len()).collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM community_channels WHERE community_id=? AND channel_id NOT IN ({placeholders})");
        let mut binds: Vec<String> = Vec::with_capacity(keep.len() + 1);
        binds.push(id_hex.clone());
        binds.extend(keep);
        tx.execute(&sql, rusqlite::params_from_iter(binds.iter()))
            .map_err(|e| format!("prune v2 channels: {e}"))?;
    }

    tx.commit().map_err(|e| format!("commit v2 community: {e}"))?;
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
                    server_root_epoch, dissolved, protocol, owner_pubkey, owner_salt
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
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((root_blob, name_e, relays_e, created, desc_e, root_epoch, dissolved, protocol, owner_pk_e, owner_salt_e)) = row
    else {
        return Ok(None);
    };
    if crate::community::ConcordProtocol::from_i64(protocol) != crate::community::ConcordProtocol::V2 {
        return Ok(None);
    }
    let (Some(owner_pk_e), Some(owner_salt_e)) = (owner_pk_e, owner_salt_e) else {
        return Err("v2 community row is missing its owner commitment".to_string());
    };

    let community_root = dec_key(&root_blob)?;
    let owner_xonly = parse_hex32(&dec_txt(&owner_pk_e))?;
    let owner_salt = parse_hex32(&dec_txt(&owner_salt_e))?;
    let identity = CommunityIdentity { community_id: *id, owner_xonly, owner_salt };
    let relays: Vec<String> = serde_json::from_str(&dec_txt(&relays_e)).unwrap_or_default();

    let mut channels = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT channel_id, channel_key, epoch, name, private
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
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (ch_hex, key_blob, epoch, name_e, private) = row.map_err(|e| e.to_string())?;
            let private = private != 0;
            let key = dec_key(&key_blob)?;
            channels.push(ChannelV2 {
                id: ChannelId(hex_id_to_32(&ch_hex)?),
                name: dec_txt(&name_e),
                private,
                // A public channel derives from the root — drop the placeholder.
                key: private.then_some(key),
                epoch: Epoch(epoch as u64),
            });
        }
    }

    Ok(Some(CommunityV2 {
        identity,
        community_root,
        root_epoch: Epoch(root_epoch as u64),
        name: dec_txt(&name_e),
        description: desc_e.map(|d| dec_txt(&d)),
        relays,
        channels,
        dissolved: dissolved != 0,
        created_at_ms: (created as u64).saturating_mul(1000),
    }))
}

fn parse_hex32(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("stored value is not 32-byte hex".to_string());
    }
    Ok(crate::simd::hex::hex_to_bytes_32(hex))
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
        crate::db::set_app_data_dir(tmp.path().to_path_buf());
        crate::db::set_current_account(account.clone()).unwrap();
        crate::db::init_database(&account).unwrap();
        (tmp, guard)
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
    fn server_root_epoch_round_trips() {
        // The base read clock survives save/load (default 0; a rotated value preserved exactly).
        let (_tmp, _guard) = init_test_db();
        let mut c = Community::create("HQ", "general", vec![]);
        save_community(&c).unwrap();
        assert_eq!(load_community(&c.id).unwrap().unwrap().server_root_epoch, Epoch(0));

        c.server_root_epoch = Epoch(5);
        c.server_root_key = ServerRootKey([0x42u8; 32]);
        save_community(&c).unwrap();
        let loaded = load_community(&c.id).unwrap().unwrap();
        assert_eq!(loaded.server_root_epoch, Epoch(5));
        assert_eq!(loaded.server_root_key.as_bytes(), &[0x42u8; 32]);
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
    fn save_community_populates_the_epoch_archive() {
        // save_community mirrors the current base + channel keys into the multi-held archive, so the
        // foundation is live without any explicit store_epoch_key call by the caller.
        let (_tmp, _guard) = init_test_db();
        let c = Community::create("HQ", "general", vec![]);
        save_community(&c).unwrap();
        let cid = c.id.to_hex();

        // Base key archived under the server-root sentinel at epoch 0.
        assert_eq!(
            held_epoch_key(&cid, crate::community::SERVER_ROOT_SCOPE_HEX, 0).unwrap().as_ref(),
            Some(c.server_root_key.as_bytes())
        );
        // The default channel's key archived under its channel id at epoch 0.
        let chan = &c.channels[0];
        assert_eq!(
            held_epoch_key(&cid, &chan.id.to_hex(), 0).unwrap().as_ref(),
            Some(chan.key.as_bytes())
        );
    }

    #[test]
    fn at_rest_encryption_wraps_keys_and_metadata_on_disk() {
        let (_tmp, _guard) = init_test_db();
        // Local Encryption ON with a known vault key (the db-test guard serializes, so toggling these
        // globals is safe; reset at the end). `others: &[]` — the slice only allocates a vault lane.
        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);

        let mut c = Community::create("Secret HQ", "general", vec!["wss://relay.example".into()]);
        c.server_root_key = ServerRootKey([0x42u8; 32]);
        c.description = Some("top secret".into());
        save_community(&c).unwrap();
        let cid = c.id.to_hex();
        set_community_banlist(&cid, &["deadbeef".repeat(8)], 1).unwrap();

        // On disk: secrets are 60-byte ciphertext (12 nonce + 32 + 16 tag), NOT raw 32-byte keys;
        // identifying text is hex ciphertext, never the plaintext.
        {
            let conn = crate::db::get_db_connection_guard_static().unwrap();
            let (root_len, name, banlist): (i64, String, String) = conn
                .query_row(
                    "SELECT length(server_root_key), name, banlist FROM communities WHERE community_id = ?1",
                    params![cid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(root_len, 60, "server_root_key must be ciphertext, not a raw 32-byte key");
            assert_ne!(name, "Secret HQ", "name must not be plaintext on disk");
            assert!(crate::crypto::looks_encrypted(&name), "name column is ciphertext");
            assert!(crate::crypto::looks_encrypted(&banlist), "banlist column is ciphertext");
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
        let loaded = load_community(&c.id).unwrap().unwrap();
        assert_eq!(loaded.name, "Secret HQ");
        assert_eq!(loaded.description.as_deref(), Some("top secret"));
        assert_eq!(loaded.server_root_key.as_bytes(), &[0x42u8; 32]);
        assert_eq!(loaded.relays, vec!["wss://relay.example".to_string()]);
        assert_eq!(get_community_banlist(&cid).unwrap(), vec!["deadbeef".repeat(8)]);

        crate::state::set_encryption_enabled(false);
        crate::state::ENCRYPTION_KEY.clear(&[]);
    }

    #[test]
    fn at_rest_decrypt_tolerates_a_pre_migration_plaintext_row() {
        // A row written BEFORE the at-rest pass (raw key + plaintext text) must still read back once
        // encryption is on — the 32-vs-60 byte + `looks_encrypted` discriminators handle the mixed DB.
        let (_tmp, _guard) = init_test_db();
        crate::state::set_encryption_enabled(false);
        let mut c = Community::create("Legacy HQ", "general", vec![]);
        c.server_root_key = ServerRootKey([0x42u8; 32]);
        save_community(&c).unwrap();

        crate::state::ENCRYPTION_KEY.set([0x55u8; 32], &[]);
        crate::state::set_encryption_enabled(true);
        let loaded = load_community(&c.id).unwrap().unwrap();
        assert_eq!(loaded.name, "Legacy HQ", "plaintext name reads through");
        assert_eq!(loaded.server_root_key.as_bytes(), &[0x42u8; 32], "raw 32-byte key reads through");

        crate::state::set_encryption_enabled(false);
        crate::state::ENCRYPTION_KEY.clear(&[]);
    }

    #[test]
    fn save_and_load_round_trip() {
        let (_tmp, _guard) = init_test_db();
        let original = Community::create("Vector HQ", "general", vec!["wss://r.one".into()]);
        save_community(&original).unwrap();

        let loaded = load_community(&original.id).unwrap().expect("present");
        assert_eq!(loaded.id, original.id);
        assert_eq!(loaded.name, "Vector HQ");
        assert_eq!(loaded.relays, original.relays);
        // Secrets survive the round trip byte-for-byte.
        assert_eq!(loaded.server_root_key.as_bytes(), original.server_root_key.as_bytes());
        // Channel survives with its key, epoch, and name.
        assert_eq!(loaded.channels.len(), 1);
        assert_eq!(loaded.channels[0].id, original.channels[0].id);
        assert_eq!(loaded.channels[0].key.as_bytes(), original.channels[0].key.as_bytes());
        assert_eq!(loaded.channels[0].epoch, Epoch(0));
        assert_eq!(loaded.channels[0].name, "general");
    }

    #[test]
    fn owner_is_protected_from_the_banlist_a_member_is_not() {
        use nostr_sdk::JsonUtil;
        let (_tmp, _guard) = init_test_db();
        let mut community = Community::create("HQ", "general", vec!["wss://r".into()]);
        // Give it a proven owner (index 0).
        let owner_id = Keys::new(SecretKey::from_slice(&[7u8; 32]).unwrap());
        community.owner_attestation = Some(
            crate::community::owner::build_owner_attestation_unsigned(
                owner_id.public_key(),
                &community.id.to_hex(),
            )
            .sign_with_keys(&owner_id)
            .unwrap()
            .as_json(),
        );
        save_community(&community).unwrap();

        // A banlist naming BOTH the owner and a regular member.
        let member = Keys::generate();
        set_community_banlist(
            &community.id.to_hex(),
            &[owner_id.public_key().to_hex(), member.public_key().to_hex()],
            1,
        )
        .unwrap();

        let loaded = load_community(&community.id).unwrap().unwrap();
        let ch = &loaded.channels[0];
        // The owner is filtered OUT of the effective banlist (index 0 can't be banned)...
        assert!(!ch.banned.contains(&owner_id.public_key()), "owner is never effectively banned");
        assert!(ch.protected.contains(&owner_id.public_key()), "owner is in the protected set");
        // ...but a regular member's ban stands.
        assert!(ch.banned.contains(&member.public_key()), "a member's ban is honored");
    }

    #[test]
    fn loaded_keys_actually_decrypt() {
        // The reconstructed keys must be usable: seal with the original channel key,
        // open with the loaded one (proves the blob round-trip preserved key bytes).
        let (_tmp, _guard) = init_test_db();
        let original = Community::create("HQ", "general", vec![]);
        save_community(&original).unwrap();
        let loaded = load_community(&original.id).unwrap().unwrap();

        let author = nostr_sdk::prelude::Keys::generate();
        let chan = &original.channels[0];
        let sealed = crate::community::envelope::seal_message(
            &author, &chan.key, &chan.id, chan.epoch, "persisted!", 1,
        )
        .unwrap();
        let opened = crate::community::envelope::open_message(
            &sealed,
            &loaded.channels[0].key,
            &loaded.channels[0].id,
            loaded.channels[0].epoch,
        )
        .unwrap();
        assert_eq!(opened.content, "persisted!");
    }

    #[test]
    fn member_view_round_trips() {
        // A joined member-view Community (keyless) persists + reloads with its
        // server-root + channel keys intact.
        let (_tmp, _guard) = init_test_db();
        let member = Community {
            id: CommunityId([7u8; 32]),
            server_root_key: ServerRootKey([8u8; 32]),
            server_root_epoch: Epoch(0),
            name: "Joined".into(),
            description: None,
            icon: None,
            banner: None,
            relays: vec!["wss://r".into()],
            channels: vec![Channel {
                id: ChannelId([9u8; 32]),
                key: ChannelKey([10u8; 32]),
                epoch: Epoch(0),
                name: "general".into(),
                banned: Vec::new(),
                protected: Vec::new(), roster: Default::default(),
                epoch_keys: Vec::new(),
                dissolved: false,
            }],
            owner_attestation: None,
            dissolved: false,
        };
        save_community(&member).unwrap();
        let loaded = load_community(&member.id).unwrap().expect("present");
        assert_eq!(loaded.server_root_key.as_bytes(), &[8u8; 32]);
        assert_eq!(loaded.channels[0].key.as_bytes(), &[10u8; 32]);
    }

    #[test]
    fn large_epoch_round_trips_losslessly() {
        // Epoch >= 2^63 stored as i64 then reinterpreted as u64 must be exact.
        let (_tmp, _guard) = init_test_db();
        let mut c = Community::create("HQ", "g", vec![]);
        c.channels[0].epoch = Epoch(u64::MAX - 7);
        save_community(&c).unwrap();
        let loaded = load_community(&c.id).unwrap().unwrap();
        assert_eq!(loaded.channels[0].epoch, Epoch(u64::MAX - 7));
    }

    #[test]
    fn malformed_channel_id_row_errors_not_corrupts() {
        // A corrupted (short/non-hex) channel_id must error on load, not silently
        // reconstruct a wrong-but-self-consistent id.
        let (_tmp, _guard) = init_test_db();
        let c = Community::create("HQ", "g", vec![]);
        save_community(&c).unwrap();
        {
            let conn = crate::db::get_write_connection_guard_static().unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO community_channels
                    (channel_id, community_id, channel_key, epoch, name, created_at)
                 VALUES (?1, ?2, ?3, 0, 'bad', 0)",
                rusqlite::params!["zz_not_hex", c.id.to_hex(), &[0u8; 32][..]],
            )
            .unwrap();
        }
        assert!(load_community(&c.id).is_err(), "malformed id must error, not corrupt");
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
        assert!(load_community(&absent).unwrap().is_none());
    }

    #[test]
    fn list_ids_reflects_saved() {
        let (_tmp, _guard) = init_test_db();
        let a = Community::create("A", "g", vec![]);
        let b = Community::create("B", "g", vec![]);
        save_community(&a).unwrap();
        save_community(&b).unwrap();
        let ids = list_community_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn delete_community_clears_all_local_state() {
        let (_tmp, _guard) = init_test_db();
        let c = Community::create("HQ", "general", vec!["r1".into()]);
        save_community(&c).unwrap();
        let cid = c.id.to_hex();
        save_public_invite(&"ab".repeat(32), &cid, "url", None, None).unwrap();
        save_pending_invite(&"cd".repeat(32), "{}", "npub1x").unwrap();
        set_edition_head(&cid, &"a".repeat(64), 3, &[0x11u8; 32]).unwrap();

        // The save above archived the base + channel keys; this proves delete clears them too.
        assert!(!held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty());

        delete_community(&cid).unwrap();
        assert!(!community_exists(&c.id).unwrap());
        assert!(community_id_for_channel(&c.channels[0].id.to_hex()).unwrap().is_none());
        assert!(list_public_invites(&cid).unwrap().is_empty());
        assert_eq!(get_edition_head(&cid, &"a".repeat(64)).unwrap(), None, "edition heads cleared on delete");
        assert!(held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap().is_empty(), "epoch keys cleared on delete");
    }

    #[test]
    fn delete_community_retain_keys_drops_state_but_keeps_epoch_keys() {
        // self-removal teardown: drop chat/membership/control state but KEEP the held epoch keys so a
        // later self-scrub of own past messages stays possible.
        let (_tmp, _guard) = init_test_db();
        let c = Community::create("HQ", "general", vec!["r1".into()]);
        save_community(&c).unwrap();
        let cid = c.id.to_hex();
        save_public_invite(&"ab".repeat(32), &cid, "url", None, None).unwrap();
        set_edition_head(&cid, &"a".repeat(64), 3, &[0x11u8; 32]).unwrap();

        let base_before = held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap();
        let chan_before = held_epoch_keys(&cid, &c.channels[0].id.to_hex()).unwrap();
        assert!(!base_before.is_empty() && !chan_before.is_empty(), "save archived base + channel keys");

        delete_community_retain_keys(&cid).unwrap();

        // State is gone.
        assert!(!community_exists(&c.id).unwrap());
        assert!(community_id_for_channel(&c.channels[0].id.to_hex()).unwrap().is_none());
        assert!(list_public_invites(&cid).unwrap().is_empty());
        assert_eq!(get_edition_head(&cid, &"a".repeat(64)).unwrap(), None);
        // Epoch keys (base + channel, every epoch) survive intact.
        assert_eq!(held_epoch_keys(&cid, crate::community::SERVER_ROOT_SCOPE_HEX).unwrap(), base_before,
            "base epoch keys retained for self-scrub");
        assert_eq!(held_epoch_keys(&cid, &c.channels[0].id.to_hex()).unwrap(), chan_before,
            "channel epoch keys retained for self-scrub");
    }

    #[test]
    fn channel_resolves_to_owning_community() {
        let (_tmp, _guard) = init_test_db();
        let c = Community::create("HQ", "general", vec![]);
        save_community(&c).unwrap();
        let chan = c.channels[0].id.to_hex();
        assert_eq!(community_id_for_channel(&chan).unwrap().as_deref(), Some(c.id.to_hex().as_str()));
        assert!(community_id_for_channel(&"ff".repeat(32)).unwrap().is_none());
    }

    #[test]
    fn community_exists_reflects_saved() {
        let (_tmp, _guard) = init_test_db();
        let c = Community::create("A", "g", vec![]);
        assert!(!community_exists(&c.id).unwrap());
        save_community(&c).unwrap();
        assert!(community_exists(&c.id).unwrap());
    }

    #[test]
    fn pending_invite_first_wins_and_round_trips() {
        let (_tmp, _guard) = init_test_db();
        let cid = "ab".repeat(32);
        // First park inserts; a re-invite for the same id is IGNORED (first-wins, so a
        // hostile re-send can't rewrite a parked bundle or re-notify).
        assert!(save_pending_invite(&cid, "{\"bundle\":1}", "npub1inviter").unwrap());
        assert!(!save_pending_invite(&cid, "{\"bundle\":2}", "npub1other").unwrap());
        assert!(pending_invite_exists(&cid).unwrap());

        let listed = list_pending_invites().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].community_id, cid);
        assert_eq!(listed[0].bundle_json, "{\"bundle\":1}", "original bundle preserved");
        assert_eq!(listed[0].inviter_npub, "npub1inviter");

        // get is non-destructive; delete then removes it.
        assert_eq!(get_pending_invite(&cid).unwrap().as_deref(), Some("{\"bundle\":1}"));
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
        let held = Community::create("Held", "general", vec![]);
        save_community(&held).unwrap();
        let held_hex = held.id.to_hex();
        save_pending_invite(&held_hex, "{\"bundle\":1}", "npub1inviter").unwrap();
        // An invite for a community we do NOT hold must survive the purge.
        let stranger = "ab".repeat(32);
        save_pending_invite(&stranger, "{\"bundle\":2}", "npub1inviter").unwrap();

        let n = purge_pending_invites_for_held_communities().unwrap();
        assert_eq!(n, 1, "only the held community's invite is purged");
        assert!(!pending_invite_exists(&held_hex).unwrap(), "held → invite gone");
        assert!(pending_invite_exists(&stranger).unwrap(), "unknown community → invite kept");
    }

    #[test]
    fn decline_drops_pending_invite() {
        let (_tmp, _guard) = init_test_db();
        let cid = "cd".repeat(32);
        save_pending_invite(&cid, "{}", "npub1x").unwrap();
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
        for i in 0..150u32 {
            let cid = format!("{:064x}", i);
            save_pending_invite(&cid, "{}", "npub1x").unwrap();
        }
        let all = list_pending_invites().unwrap();
        assert_eq!(all.len(), 100, "table capped at MAX_PENDING_INVITES");
        // A spam flood can't grow it past the cap regardless of how many arrive.
        for i in 150..400u32 {
            let cid = format!("{:064x}", i);
            save_pending_invite(&cid, "{}", "npub1x").unwrap();
        }
        assert_eq!(list_pending_invites().unwrap().len(), 100, "cap holds under flood");
    }
}
