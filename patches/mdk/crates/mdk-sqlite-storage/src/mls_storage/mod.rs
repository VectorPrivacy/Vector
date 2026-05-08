//! OpenMLS StorageProvider implementation for SQLite.
//!
//! This module implements the `StorageProvider<1>` trait from `openmls_traits`
//! directly on `MdkSqliteStorage`, enabling unified storage for both MLS
//! cryptographic state and MDK-specific data within a single database connection.

use mdk_storage_traits::MdkStorageError;
pub use mdk_storage_traits::mls_codec::{GroupDataType, MlsCodec};
use openmls_traits::storage::{Entity, Key};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The storage provider version matching OpenMLS's CURRENT_VERSION.
pub const STORAGE_PROVIDER_VERSION: u16 = 1;

// ============================================================================
// Helper functions for serialization
// ============================================================================

/// Serialize a key to bytes for database storage.
fn serialize_key<K>(key: &K) -> Result<Vec<u8>, MdkStorageError>
where
    K: Serialize,
{
    MlsCodec::serialize(key)
}

/// Serialize an entity to bytes for database storage.
fn serialize_entity<E>(entity: &E) -> Result<Vec<u8>, MdkStorageError>
where
    E: Serialize,
{
    MlsCodec::serialize(entity)
}

/// Deserialize an entity from bytes.
fn deserialize_entity<E>(bytes: &[u8]) -> Result<E, MdkStorageError>
where
    E: DeserializeOwned,
{
    MlsCodec::deserialize(bytes)
}

// ============================================================================
// Group Data Operations
// ============================================================================

/// Write group data to the database.
pub(crate) fn write_group_data<GroupId, GroupData>(
    conn: &Connection,
    group_id: &GroupId,
    data_type: GroupDataType,
    data: &GroupData,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    GroupData: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let data_bytes = serialize_entity(data)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_group_data (group_id, data_type, group_data, provider_version)
         VALUES (?, ?, ?, ?)",
        params![group_id_bytes, data_type.as_str(), data_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read group data from the database.
pub(crate) fn read_group_data<GroupId, GroupData>(
    conn: &Connection,
    group_id: &GroupId,
    data_type: GroupDataType,
) -> Result<Option<GroupData>, MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    GroupData: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    let result: Option<Vec<u8>> = conn
        .query_row(
            "SELECT group_data FROM openmls_group_data
             WHERE group_id = ? AND data_type = ? AND provider_version = ?",
            params![group_id_bytes, data_type.as_str(), STORAGE_PROVIDER_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    match result {
        Some(bytes) => Ok(Some(deserialize_entity(&bytes)?)),
        None => Ok(None),
    }
}

/// Delete group data from the database.
pub(crate) fn delete_group_data<GroupId>(
    conn: &Connection,
    group_id: &GroupId,
    data_type: GroupDataType,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    conn.execute(
        "DELETE FROM openmls_group_data
         WHERE group_id = ? AND data_type = ? AND provider_version = ?",
        params![group_id_bytes, data_type.as_str(), STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Own Leaf Nodes Operations
// ============================================================================

/// Append an own leaf node for a group.
pub(crate) fn append_own_leaf_node<GroupId, LeafNode>(
    conn: &Connection,
    group_id: &GroupId,
    leaf_node: &LeafNode,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    LeafNode: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let leaf_node_bytes = serialize_entity(leaf_node)?;

    conn.execute(
        "INSERT INTO openmls_own_leaf_nodes (group_id, leaf_node, provider_version)
         VALUES (?, ?, ?)",
        params![group_id_bytes, leaf_node_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read all own leaf nodes for a group.
pub(crate) fn read_own_leaf_nodes<GroupId, LeafNode>(
    conn: &Connection,
    group_id: &GroupId,
) -> Result<Vec<LeafNode>, MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    LeafNode: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    let mut stmt = conn
        .prepare(
            "SELECT leaf_node FROM openmls_own_leaf_nodes
             WHERE group_id = ? AND provider_version = ?
             ORDER BY id ASC",
        )
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![group_id_bytes, STORAGE_PROVIDER_VERSION], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    let mut leaf_nodes = Vec::new();
    for row in rows {
        let bytes = row.map_err(|e| MdkStorageError::Database(e.to_string()))?;
        leaf_nodes.push(deserialize_entity(&bytes)?);
    }

    Ok(leaf_nodes)
}

/// Delete all own leaf nodes for a group.
pub(crate) fn delete_own_leaf_nodes<GroupId>(
    conn: &Connection,
    group_id: &GroupId,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    conn.execute(
        "DELETE FROM openmls_own_leaf_nodes
         WHERE group_id = ? AND provider_version = ?",
        params![group_id_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Proposals Operations
// ============================================================================

/// Queue a proposal for a group.
pub(crate) fn queue_proposal<GroupId, ProposalRef, QueuedProposal>(
    conn: &Connection,
    group_id: &GroupId,
    proposal_ref: &ProposalRef,
    proposal: &QueuedProposal,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    ProposalRef: Key<STORAGE_PROVIDER_VERSION> + Entity<STORAGE_PROVIDER_VERSION>,
    QueuedProposal: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let proposal_ref_bytes = serialize_key(proposal_ref)?;
    let proposal_bytes = serialize_entity(proposal)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_proposals (group_id, proposal_ref, proposal, provider_version)
         VALUES (?, ?, ?, ?)",
        params![group_id_bytes, proposal_ref_bytes, proposal_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read all queued proposal refs for a group.
pub(crate) fn read_queued_proposal_refs<GroupId, ProposalRef>(
    conn: &Connection,
    group_id: &GroupId,
) -> Result<Vec<ProposalRef>, MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    ProposalRef: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    let mut stmt = conn
        .prepare(
            "SELECT proposal_ref FROM openmls_proposals
             WHERE group_id = ? AND provider_version = ?",
        )
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![group_id_bytes, STORAGE_PROVIDER_VERSION], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    let mut refs = Vec::new();
    for row in rows {
        let bytes = row.map_err(|e| MdkStorageError::Database(e.to_string()))?;
        refs.push(deserialize_entity(&bytes)?);
    }

    Ok(refs)
}

/// Read all queued proposals for a group.
pub(crate) fn read_queued_proposals<GroupId, ProposalRef, QueuedProposal>(
    conn: &Connection,
    group_id: &GroupId,
) -> Result<Vec<(ProposalRef, QueuedProposal)>, MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    ProposalRef: Entity<STORAGE_PROVIDER_VERSION>,
    QueuedProposal: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    let mut stmt = conn
        .prepare(
            "SELECT proposal_ref, proposal FROM openmls_proposals
             WHERE group_id = ? AND provider_version = ?",
        )
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    let rows = stmt
        .query_map(params![group_id_bytes, STORAGE_PROVIDER_VERSION], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    let mut proposals = Vec::new();
    for row in rows {
        let (ref_bytes, proposal_bytes) =
            row.map_err(|e| MdkStorageError::Database(e.to_string()))?;
        let proposal_ref: ProposalRef = deserialize_entity(&ref_bytes)?;
        let proposal: QueuedProposal = deserialize_entity(&proposal_bytes)?;
        proposals.push((proposal_ref, proposal));
    }

    Ok(proposals)
}

/// Remove a single proposal from a group's queue.
pub(crate) fn remove_proposal<GroupId, ProposalRef>(
    conn: &Connection,
    group_id: &GroupId,
    proposal_ref: &ProposalRef,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    ProposalRef: Key<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let proposal_ref_bytes = serialize_key(proposal_ref)?;

    conn.execute(
        "DELETE FROM openmls_proposals
         WHERE group_id = ? AND proposal_ref = ? AND provider_version = ?",
        params![group_id_bytes, proposal_ref_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Clear all proposals for a group.
pub(crate) fn clear_proposal_queue<GroupId>(
    conn: &Connection,
    group_id: &GroupId,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;

    conn.execute(
        "DELETE FROM openmls_proposals
         WHERE group_id = ? AND provider_version = ?",
        params![group_id_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Key Packages Operations
// ============================================================================

/// Write a key package.
pub(crate) fn write_key_package<HashReference, KeyPackage>(
    conn: &Connection,
    hash_ref: &HashReference,
    key_package: &KeyPackage,
) -> Result<(), MdkStorageError>
where
    HashReference: Key<STORAGE_PROVIDER_VERSION>,
    KeyPackage: Entity<STORAGE_PROVIDER_VERSION>,
{
    let hash_ref_bytes = serialize_key(hash_ref)?;
    let key_package_bytes = serialize_entity(key_package)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_key_packages (key_package_ref, key_package, provider_version)
         VALUES (?, ?, ?)",
        params![hash_ref_bytes, key_package_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read a key package.
pub(crate) fn read_key_package<HashReference, KeyPackage>(
    conn: &Connection,
    hash_ref: &HashReference,
) -> Result<Option<KeyPackage>, MdkStorageError>
where
    HashReference: Key<STORAGE_PROVIDER_VERSION>,
    KeyPackage: Entity<STORAGE_PROVIDER_VERSION>,
{
    let hash_ref_bytes = serialize_key(hash_ref)?;

    let result: Option<Vec<u8>> = conn
        .query_row(
            "SELECT key_package FROM openmls_key_packages
             WHERE key_package_ref = ? AND provider_version = ?",
            params![hash_ref_bytes, STORAGE_PROVIDER_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    match result {
        Some(bytes) => Ok(Some(deserialize_entity(&bytes)?)),
        None => Ok(None),
    }
}

/// Delete a key package.
pub(crate) fn delete_key_package<HashReference>(
    conn: &Connection,
    hash_ref: &HashReference,
) -> Result<(), MdkStorageError>
where
    HashReference: Key<STORAGE_PROVIDER_VERSION>,
{
    let hash_ref_bytes = serialize_key(hash_ref)?;

    conn.execute(
        "DELETE FROM openmls_key_packages
         WHERE key_package_ref = ? AND provider_version = ?",
        params![hash_ref_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Signature Keys Operations
// ============================================================================

/// Write a signature key pair.
pub(crate) fn write_signature_key_pair<SignaturePublicKey, SignatureKeyPair>(
    conn: &Connection,
    public_key: &SignaturePublicKey,
    signature_key_pair: &SignatureKeyPair,
) -> Result<(), MdkStorageError>
where
    SignaturePublicKey: Key<STORAGE_PROVIDER_VERSION>,
    SignatureKeyPair: Entity<STORAGE_PROVIDER_VERSION>,
{
    let public_key_bytes = serialize_key(public_key)?;
    let key_pair_bytes = serialize_entity(signature_key_pair)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_signature_keys (public_key, signature_key, provider_version)
         VALUES (?, ?, ?)",
        params![public_key_bytes, key_pair_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read a signature key pair.
pub(crate) fn read_signature_key_pair<SignaturePublicKey, SignatureKeyPair>(
    conn: &Connection,
    public_key: &SignaturePublicKey,
) -> Result<Option<SignatureKeyPair>, MdkStorageError>
where
    SignaturePublicKey: Key<STORAGE_PROVIDER_VERSION>,
    SignatureKeyPair: Entity<STORAGE_PROVIDER_VERSION>,
{
    let public_key_bytes = serialize_key(public_key)?;

    let result: Option<Vec<u8>> = conn
        .query_row(
            "SELECT signature_key FROM openmls_signature_keys
             WHERE public_key = ? AND provider_version = ?",
            params![public_key_bytes, STORAGE_PROVIDER_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    match result {
        Some(bytes) => Ok(Some(deserialize_entity(&bytes)?)),
        None => Ok(None),
    }
}

/// Delete a signature key pair.
pub(crate) fn delete_signature_key_pair<SignaturePublicKey>(
    conn: &Connection,
    public_key: &SignaturePublicKey,
) -> Result<(), MdkStorageError>
where
    SignaturePublicKey: Key<STORAGE_PROVIDER_VERSION>,
{
    let public_key_bytes = serialize_key(public_key)?;

    conn.execute(
        "DELETE FROM openmls_signature_keys
         WHERE public_key = ? AND provider_version = ?",
        params![public_key_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Encryption Keys Operations
// ============================================================================

/// Write an encryption key pair.
pub(crate) fn write_encryption_key_pair<EncryptionKey, HpkeKeyPair>(
    conn: &Connection,
    public_key: &EncryptionKey,
    key_pair: &HpkeKeyPair,
) -> Result<(), MdkStorageError>
where
    EncryptionKey: Key<STORAGE_PROVIDER_VERSION>,
    HpkeKeyPair: Entity<STORAGE_PROVIDER_VERSION>,
{
    let public_key_bytes = serialize_key(public_key)?;
    let key_pair_bytes = serialize_entity(key_pair)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_encryption_keys (public_key, key_pair, provider_version)
         VALUES (?, ?, ?)",
        params![public_key_bytes, key_pair_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read an encryption key pair.
pub(crate) fn read_encryption_key_pair<EncryptionKey, HpkeKeyPair>(
    conn: &Connection,
    public_key: &EncryptionKey,
) -> Result<Option<HpkeKeyPair>, MdkStorageError>
where
    EncryptionKey: Key<STORAGE_PROVIDER_VERSION>,
    HpkeKeyPair: Entity<STORAGE_PROVIDER_VERSION>,
{
    let public_key_bytes = serialize_key(public_key)?;

    let result: Option<Vec<u8>> = conn
        .query_row(
            "SELECT key_pair FROM openmls_encryption_keys
             WHERE public_key = ? AND provider_version = ?",
            params![public_key_bytes, STORAGE_PROVIDER_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    match result {
        Some(bytes) => Ok(Some(deserialize_entity(&bytes)?)),
        None => Ok(None),
    }
}

/// Delete an encryption key pair.
pub(crate) fn delete_encryption_key_pair<EncryptionKey>(
    conn: &Connection,
    public_key: &EncryptionKey,
) -> Result<(), MdkStorageError>
where
    EncryptionKey: Key<STORAGE_PROVIDER_VERSION>,
{
    let public_key_bytes = serialize_key(public_key)?;

    conn.execute(
        "DELETE FROM openmls_encryption_keys
         WHERE public_key = ? AND provider_version = ?",
        params![public_key_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// Epoch Key Pairs Operations
// ============================================================================

/// Write epoch encryption key pairs.
pub(crate) fn write_encryption_epoch_key_pairs<GroupId, EpochKey, HpkeKeyPair>(
    conn: &Connection,
    group_id: &GroupId,
    epoch: &EpochKey,
    leaf_index: u32,
    key_pairs: &[HpkeKeyPair],
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    EpochKey: Key<STORAGE_PROVIDER_VERSION>,
    HpkeKeyPair: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let epoch_bytes = serialize_key(epoch)?;
    let key_pairs_bytes = serialize_entity(&key_pairs)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_epoch_key_pairs (group_id, epoch_id, leaf_index, key_pairs, provider_version)
         VALUES (?, ?, ?, ?, ?)",
        params![group_id_bytes, epoch_bytes, leaf_index, key_pairs_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read epoch encryption key pairs.
pub(crate) fn read_encryption_epoch_key_pairs<GroupId, EpochKey, HpkeKeyPair>(
    conn: &Connection,
    group_id: &GroupId,
    epoch: &EpochKey,
    leaf_index: u32,
) -> Result<Vec<HpkeKeyPair>, MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    EpochKey: Key<STORAGE_PROVIDER_VERSION>,
    HpkeKeyPair: Entity<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let epoch_bytes = serialize_key(epoch)?;

    let result: Option<Vec<u8>> = conn
        .query_row(
            "SELECT key_pairs FROM openmls_epoch_key_pairs
             WHERE group_id = ? AND epoch_id = ? AND leaf_index = ? AND provider_version = ?",
            params![
                group_id_bytes,
                epoch_bytes,
                leaf_index,
                STORAGE_PROVIDER_VERSION
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    match result {
        Some(bytes) => deserialize_entity(&bytes),
        None => Ok(Vec::new()),
    }
}

/// Delete epoch encryption key pairs.
pub(crate) fn delete_encryption_epoch_key_pairs<GroupId, EpochKey>(
    conn: &Connection,
    group_id: &GroupId,
    epoch: &EpochKey,
    leaf_index: u32,
) -> Result<(), MdkStorageError>
where
    GroupId: Key<STORAGE_PROVIDER_VERSION>,
    EpochKey: Key<STORAGE_PROVIDER_VERSION>,
{
    let group_id_bytes = serialize_key(group_id)?;
    let epoch_bytes = serialize_key(epoch)?;

    conn.execute(
        "DELETE FROM openmls_epoch_key_pairs
         WHERE group_id = ? AND epoch_id = ? AND leaf_index = ? AND provider_version = ?",
        params![
            group_id_bytes,
            epoch_bytes,
            leaf_index,
            STORAGE_PROVIDER_VERSION
        ],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

// ============================================================================
// PSK Operations
// ============================================================================

/// Write a PSK bundle.
pub(crate) fn write_psk<PskId, PskBundle>(
    conn: &Connection,
    psk_id: &PskId,
    psk: &PskBundle,
) -> Result<(), MdkStorageError>
where
    PskId: Key<STORAGE_PROVIDER_VERSION>,
    PskBundle: Entity<STORAGE_PROVIDER_VERSION>,
{
    let psk_id_bytes = serialize_key(psk_id)?;
    let psk_bytes = serialize_entity(psk)?;

    conn.execute(
        "INSERT OR REPLACE INTO openmls_psks (psk_id, psk_bundle, provider_version)
         VALUES (?, ?, ?)",
        params![psk_id_bytes, psk_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

/// Read a PSK bundle.
pub(crate) fn read_psk<PskId, PskBundle>(
    conn: &Connection,
    psk_id: &PskId,
) -> Result<Option<PskBundle>, MdkStorageError>
where
    PskId: Key<STORAGE_PROVIDER_VERSION>,
    PskBundle: Entity<STORAGE_PROVIDER_VERSION>,
{
    let psk_id_bytes = serialize_key(psk_id)?;

    let result: Option<Vec<u8>> = conn
        .query_row(
            "SELECT psk_bundle FROM openmls_psks
             WHERE psk_id = ? AND provider_version = ?",
            params![psk_id_bytes, STORAGE_PROVIDER_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    match result {
        Some(bytes) => Ok(Some(deserialize_entity(&bytes)?)),
        None => Ok(None),
    }
}

/// Delete a PSK bundle.
pub(crate) fn delete_psk<PskId>(conn: &Connection, psk_id: &PskId) -> Result<(), MdkStorageError>
where
    PskId: Key<STORAGE_PROVIDER_VERSION>,
{
    let psk_id_bytes = serialize_key(psk_id)?;

    conn.execute(
        "DELETE FROM openmls_psks
         WHERE psk_id = ? AND provider_version = ?",
        params![psk_id_bytes, STORAGE_PROVIDER_VERSION],
    )
    .map_err(|e| MdkStorageError::Database(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::MdkSqliteStorage;

    /// Test data structure for MLS storage tests.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestData {
        id: u32,
        name: String,
        bytes: Vec<u8>,
    }

    /// Helper to create a test storage and return the connection.
    fn with_test_storage<F, R>(f: F) -> R
    where
        F: FnOnce(&Connection) -> R,
    {
        let storage = MdkSqliteStorage::new_in_memory().unwrap();
        storage.with_connection(f)
    }

    // ========================================
    // Serialization Helper Tests
    // ========================================

    #[test]
    fn test_serialize_key() {
        let key = vec![1u8, 2, 3, 4];
        let result = serialize_key(&key);
        assert!(result.is_ok());

        let bytes = result.unwrap();
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_serialize_entity() {
        let entity = TestData {
            id: 42,
            name: "test".to_string(),
            bytes: vec![1, 2, 3],
        };
        let result = serialize_entity(&entity);
        assert!(result.is_ok());
    }

    #[test]
    fn test_deserialize_entity() {
        let original = TestData {
            id: 42,
            name: "test".to_string(),
            bytes: vec![1, 2, 3],
        };
        let serialized = serialize_entity(&original).unwrap();

        let result: TestData = deserialize_entity(&serialized).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_deserialize_invalid_data() {
        let invalid = b"not valid data";
        let result: Result<TestData, _> = deserialize_entity(invalid);
        assert!(result.is_err());
    }

    // ========================================
    // Direct SQL Tests for MLS Tables
    // These tests verify the schema and SQL operations work correctly
    // without going through the generic helper functions that have
    // OpenMLS-specific trait bounds.
    // ========================================

    #[test]
    fn test_openmls_group_data_table_operations() {
        with_test_storage(|conn| {
            let group_id_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let data_bytes = MlsCodec::serialize(&"test data".to_string()).unwrap();
            let data_type = GroupDataType::Tree.as_str();

            // Insert
            conn.execute(
                "INSERT OR REPLACE INTO openmls_group_data (group_id, data_type, group_data, provider_version)
                 VALUES (?, ?, ?, ?)",
                params![group_id_bytes, data_type, data_bytes, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Read
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT group_data FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ? AND provider_version = ?",
                    params![group_id_bytes, data_type, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_some());
            let retrieved: String = MlsCodec::deserialize(&result.unwrap()).unwrap();
            assert_eq!(retrieved, "test data");

            // Delete
            conn.execute(
                "DELETE FROM openmls_group_data
                 WHERE group_id = ? AND data_type = ? AND provider_version = ?",
                params![group_id_bytes, data_type, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            // Verify deleted
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT group_data FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ? AND provider_version = ?",
                    params![group_id_bytes, data_type, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_none());
        });
    }

    #[test]
    fn test_openmls_own_leaf_nodes_table_operations() {
        with_test_storage(|conn| {
            let group_id_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();

            // Insert multiple leaf nodes
            for i in 0..3 {
                let leaf_bytes = MlsCodec::serialize(&format!("leaf_{}", i)).unwrap();
                conn.execute(
                    "INSERT INTO openmls_own_leaf_nodes (group_id, leaf_node, provider_version)
                     VALUES (?, ?, ?)",
                    params![group_id_bytes, leaf_bytes, STORAGE_PROVIDER_VERSION],
                )
                .unwrap();
            }

            // Read all leaf nodes
            let mut stmt = conn
                .prepare(
                    "SELECT leaf_node FROM openmls_own_leaf_nodes
                     WHERE group_id = ? AND provider_version = ?
                     ORDER BY id ASC",
                )
                .unwrap();

            let leaf_nodes: Vec<String> = stmt
                .query_map(params![group_id_bytes, STORAGE_PROVIDER_VERSION], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .unwrap()
                .map(|r| MlsCodec::deserialize(&r.unwrap()).unwrap())
                .collect();

            assert_eq!(leaf_nodes, vec!["leaf_0", "leaf_1", "leaf_2"]);

            // Delete all
            conn.execute(
                "DELETE FROM openmls_own_leaf_nodes
                 WHERE group_id = ? AND provider_version = ?",
                params![group_id_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            // Verify deleted
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM openmls_own_leaf_nodes
                     WHERE group_id = ? AND provider_version = ?",
                    params![group_id_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .unwrap();

            assert_eq!(count, 0);
        });
    }

    #[test]
    fn test_openmls_proposals_table_operations() {
        with_test_storage(|conn| {
            let group_id_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let proposal_ref_bytes = MlsCodec::serialize(&vec![10u8, 20, 30]).unwrap();
            let proposal_bytes = MlsCodec::serialize(&"test proposal".to_string()).unwrap();

            // Insert proposal
            conn.execute(
                "INSERT OR REPLACE INTO openmls_proposals (group_id, proposal_ref, proposal, provider_version)
                 VALUES (?, ?, ?, ?)",
                params![group_id_bytes, proposal_ref_bytes, proposal_bytes, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Read proposals
            let mut stmt = conn
                .prepare(
                    "SELECT proposal_ref, proposal FROM openmls_proposals
                     WHERE group_id = ? AND provider_version = ?",
                )
                .unwrap();

            let proposals: Vec<(Vec<u8>, String)> = stmt
                .query_map(params![group_id_bytes, STORAGE_PROVIDER_VERSION], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .unwrap()
                .map(|r| {
                    let (ref_bytes, prop_bytes) = r.unwrap();
                    let prop: String = MlsCodec::deserialize(&prop_bytes).unwrap();
                    (ref_bytes, prop)
                })
                .collect();

            assert_eq!(proposals.len(), 1);
            assert_eq!(proposals[0].1, "test proposal");

            // Remove proposal
            conn.execute(
                "DELETE FROM openmls_proposals
                 WHERE group_id = ? AND proposal_ref = ? AND provider_version = ?",
                params![group_id_bytes, proposal_ref_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            // Verify removed
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM openmls_proposals
                     WHERE group_id = ? AND provider_version = ?",
                    params![group_id_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .unwrap();

            assert_eq!(count, 0);
        });
    }

    #[test]
    fn test_openmls_key_packages_table_operations() {
        with_test_storage(|conn| {
            let hash_ref_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let key_package_bytes = MlsCodec::serialize(&"key package data".to_string()).unwrap();

            // Insert
            conn.execute(
                "INSERT OR REPLACE INTO openmls_key_packages (key_package_ref, key_package, provider_version)
                 VALUES (?, ?, ?)",
                params![hash_ref_bytes, key_package_bytes, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Read
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT key_package FROM openmls_key_packages
                     WHERE key_package_ref = ? AND provider_version = ?",
                    params![hash_ref_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_some());
            let retrieved: String = MlsCodec::deserialize(&result.unwrap()).unwrap();
            assert_eq!(retrieved, "key package data");

            // Delete
            conn.execute(
                "DELETE FROM openmls_key_packages
                 WHERE key_package_ref = ? AND provider_version = ?",
                params![hash_ref_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            // Verify deleted
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT key_package FROM openmls_key_packages
                     WHERE key_package_ref = ? AND provider_version = ?",
                    params![hash_ref_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_none());
        });
    }

    #[test]
    fn test_openmls_signature_keys_table_operations() {
        with_test_storage(|conn| {
            let public_key_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let key_pair_bytes = MlsCodec::serialize(&"signature key pair".to_string()).unwrap();

            // Insert
            conn.execute(
                "INSERT OR REPLACE INTO openmls_signature_keys (public_key, signature_key, provider_version)
                 VALUES (?, ?, ?)",
                params![public_key_bytes, key_pair_bytes, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Read
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT signature_key FROM openmls_signature_keys
                     WHERE public_key = ? AND provider_version = ?",
                    params![public_key_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_some());

            // Delete
            conn.execute(
                "DELETE FROM openmls_signature_keys
                 WHERE public_key = ? AND provider_version = ?",
                params![public_key_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT signature_key FROM openmls_signature_keys
                     WHERE public_key = ? AND provider_version = ?",
                    params![public_key_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_none());
        });
    }

    #[test]
    fn test_openmls_encryption_keys_table_operations() {
        with_test_storage(|conn| {
            let public_key_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let key_pair_bytes = MlsCodec::serialize(&"encryption key pair".to_string()).unwrap();

            // Insert
            conn.execute(
                "INSERT OR REPLACE INTO openmls_encryption_keys (public_key, key_pair, provider_version)
                 VALUES (?, ?, ?)",
                params![public_key_bytes, key_pair_bytes, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Read
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT key_pair FROM openmls_encryption_keys
                     WHERE public_key = ? AND provider_version = ?",
                    params![public_key_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_some());

            // Delete
            conn.execute(
                "DELETE FROM openmls_encryption_keys
                 WHERE public_key = ? AND provider_version = ?",
                params![public_key_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT key_pair FROM openmls_encryption_keys
                     WHERE public_key = ? AND provider_version = ?",
                    params![public_key_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_none());
        });
    }

    #[test]
    fn test_openmls_epoch_key_pairs_table_operations() {
        with_test_storage(|conn| {
            let group_id_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let epoch_bytes = MlsCodec::serialize(&5u64).unwrap();
            let leaf_index = 0u32;
            let key_pairs_bytes =
                MlsCodec::serialize(&vec!["key1".to_string(), "key2".to_string()]).unwrap();

            // Insert
            conn.execute(
                "INSERT OR REPLACE INTO openmls_epoch_key_pairs (group_id, epoch_id, leaf_index, key_pairs, provider_version)
                 VALUES (?, ?, ?, ?, ?)",
                params![group_id_bytes, epoch_bytes, leaf_index, key_pairs_bytes, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Read
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT key_pairs FROM openmls_epoch_key_pairs
                     WHERE group_id = ? AND epoch_id = ? AND leaf_index = ? AND provider_version = ?",
                    params![group_id_bytes, epoch_bytes, leaf_index, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_some());
            let retrieved: Vec<String> = MlsCodec::deserialize(&result.unwrap()).unwrap();
            assert_eq!(retrieved, vec!["key1", "key2"]);

            // Delete
            conn.execute(
                "DELETE FROM openmls_epoch_key_pairs
                 WHERE group_id = ? AND epoch_id = ? AND leaf_index = ? AND provider_version = ?",
                params![
                    group_id_bytes,
                    epoch_bytes,
                    leaf_index,
                    STORAGE_PROVIDER_VERSION
                ],
            )
            .unwrap();

            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT key_pairs FROM openmls_epoch_key_pairs
                     WHERE group_id = ? AND epoch_id = ? AND leaf_index = ? AND provider_version = ?",
                    params![group_id_bytes, epoch_bytes, leaf_index, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_none());
        });
    }

    #[test]
    fn test_openmls_psks_table_operations() {
        with_test_storage(|conn| {
            let psk_id_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let psk_bundle_bytes = MlsCodec::serialize(&"psk bundle data".to_string()).unwrap();

            // Insert
            conn.execute(
                "INSERT OR REPLACE INTO openmls_psks (psk_id, psk_bundle, provider_version)
                 VALUES (?, ?, ?)",
                params![psk_id_bytes, psk_bundle_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            // Read
            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT psk_bundle FROM openmls_psks
                     WHERE psk_id = ? AND provider_version = ?",
                    params![psk_id_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_some());
            let retrieved: String = MlsCodec::deserialize(&result.unwrap()).unwrap();
            assert_eq!(retrieved, "psk bundle data");

            // Delete
            conn.execute(
                "DELETE FROM openmls_psks
                 WHERE psk_id = ? AND provider_version = ?",
                params![psk_id_bytes, STORAGE_PROVIDER_VERSION],
            )
            .unwrap();

            let result: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT psk_bundle FROM openmls_psks
                     WHERE psk_id = ? AND provider_version = ?",
                    params![psk_id_bytes, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();

            assert!(result.is_none());
        });
    }

    #[test]
    fn test_group_data_type_as_str_coverage() {
        // Cover all GroupDataType variants
        assert_eq!(GroupDataType::JoinGroupConfig.as_str(), "join_group_config");
        assert_eq!(GroupDataType::Tree.as_str(), "tree");
        assert_eq!(
            GroupDataType::InterimTranscriptHash.as_str(),
            "interim_transcript_hash"
        );
        assert_eq!(GroupDataType::Context.as_str(), "context");
        assert_eq!(GroupDataType::ConfirmationTag.as_str(), "confirmation_tag");
        assert_eq!(GroupDataType::GroupState.as_str(), "group_state");
        assert_eq!(GroupDataType::MessageSecrets.as_str(), "message_secrets");
        assert_eq!(
            GroupDataType::ResumptionPskStore.as_str(),
            "resumption_psk_store"
        );
        assert_eq!(GroupDataType::OwnLeafIndex.as_str(), "own_leaf_index");
        assert_eq!(
            GroupDataType::GroupEpochSecrets.as_str(),
            "group_epoch_secrets"
        );
    }

    #[test]
    fn test_insert_or_replace_behavior() {
        with_test_storage(|conn| {
            let group_id_bytes = MlsCodec::serialize(&vec![1u8, 2, 3, 4]).unwrap();
            let data_type = GroupDataType::Tree.as_str();

            // Insert first value
            let data1 = MlsCodec::serialize(&"first".to_string()).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO openmls_group_data (group_id, data_type, group_data, provider_version)
                 VALUES (?, ?, ?, ?)",
                params![group_id_bytes, data_type, data1, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Replace with second value
            let data2 = MlsCodec::serialize(&"second".to_string()).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO openmls_group_data (group_id, data_type, group_data, provider_version)
                 VALUES (?, ?, ?, ?)",
                params![group_id_bytes, data_type, data2, STORAGE_PROVIDER_VERSION],
            ).unwrap();

            // Verify only one row and it has the second value
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ? AND provider_version = ?",
                    params![group_id_bytes, data_type, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);

            let result: Vec<u8> = conn
                .query_row(
                    "SELECT group_data FROM openmls_group_data
                     WHERE group_id = ? AND data_type = ? AND provider_version = ?",
                    params![group_id_bytes, data_type, STORAGE_PROVIDER_VERSION],
                    |row| row.get(0),
                )
                .unwrap();

            let retrieved: String = MlsCodec::deserialize(&result).unwrap();
            assert_eq!(retrieved, "second");
        });
    }

    // ========================================
    // Function Tests
    // These tests verify the helper functions work correctly.
    // ========================================

    #[test]
    fn test_group_data_functions() {
        use openmls::group::GroupId;
        with_test_storage(|conn| {
            let group_id = GroupId::from_slice(&[1u8, 2, 3]);
            let data = b"test_data".to_vec();
            let data_type = GroupDataType::Tree;

            // Write
            write_group_data(conn, &group_id, data_type, &data).unwrap();

            // Read
            let read = read_group_data::<_, Vec<u8>>(conn, &group_id, data_type).unwrap();
            assert_eq!(read, Some(data.clone()));

            // Delete
            delete_group_data(conn, &group_id, data_type).unwrap();

            // Verify
            let read_after_delete =
                read_group_data::<_, Vec<u8>>(conn, &group_id, data_type).unwrap();
            assert_eq!(read_after_delete, None);
        });
    }

    #[test]
    fn test_own_leaf_node_functions() {
        use openmls::group::GroupId;
        with_test_storage(|conn| {
            let group_id = GroupId::from_slice(&[1u8, 2, 3]);
            let leaf1 = b"leaf1".to_vec();
            let leaf2 = b"leaf2".to_vec();

            // Append
            append_own_leaf_node(conn, &group_id, &leaf1).unwrap();
            append_own_leaf_node(conn, &group_id, &leaf2).unwrap();

            // Read
            let leaves = read_own_leaf_nodes::<_, Vec<u8>>(conn, &group_id).unwrap();
            assert_eq!(leaves, vec![leaf1, leaf2]);

            // Delete
            delete_own_leaf_nodes(conn, &group_id).unwrap();

            // Verify
            let leaves_after_delete = read_own_leaf_nodes::<_, Vec<u8>>(conn, &group_id).unwrap();
            assert!(leaves_after_delete.is_empty());
        });
    }

    #[test]
    fn test_proposal_functions() {
        use openmls::group::GroupId;
        use openmls_traits::storage::{Entity, Key};
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        struct TestRef(Vec<u8>);
        impl Key<STORAGE_PROVIDER_VERSION> for TestRef {}
        impl Entity<STORAGE_PROVIDER_VERSION> for TestRef {}

        with_test_storage(|conn| {
            let group_id = GroupId::from_slice(&[1u8, 2, 3]);
            let ref1 = TestRef(vec![10u8]);
            let prop1 = b"prop1".to_vec();
            let ref2 = TestRef(vec![20u8]);
            let prop2 = b"prop2".to_vec();

            // Queue
            queue_proposal(conn, &group_id, &ref1, &prop1).unwrap();
            queue_proposal(conn, &group_id, &ref2, &prop2).unwrap();

            // Read refs
            let refs = read_queued_proposal_refs::<_, TestRef>(conn, &group_id).unwrap();
            assert_eq!(refs.len(), 2);
            assert!(refs.contains(&ref1));
            assert!(refs.contains(&ref2));

            // Read proposals
            let props = read_queued_proposals::<_, TestRef, Vec<u8>>(conn, &group_id).unwrap();
            assert_eq!(props.len(), 2);
            // Verify content
            for (r, p) in props {
                if r == ref1 {
                    assert_eq!(p, prop1);
                } else if r == ref2 {
                    assert_eq!(p, prop2);
                } else {
                    panic!("Unexpected ref");
                }
            }

            // Remove single
            remove_proposal(conn, &group_id, &ref1).unwrap();
            let refs_after_remove =
                read_queued_proposal_refs::<_, TestRef>(conn, &group_id).unwrap();
            assert_eq!(refs_after_remove.len(), 1);
            assert_eq!(refs_after_remove[0], ref2);

            // Clear all
            queue_proposal(conn, &group_id, &ref1, &prop1).unwrap(); // Re-add
            clear_proposal_queue(conn, &group_id).unwrap();
            let refs_after_clear =
                read_queued_proposal_refs::<_, TestRef>(conn, &group_id).unwrap();
            assert!(refs_after_clear.is_empty());
        });
    }

    #[test]
    fn test_key_package_functions() {
        use openmls::group::GroupId;
        // Using GroupId as KeyPackageRef (needs Key)
        with_test_storage(|conn| {
            let hash_ref = GroupId::from_slice(&[1u8, 2, 3]);
            let kp = b"key_package".to_vec();

            write_key_package(conn, &hash_ref, &kp).unwrap();

            let read = read_key_package::<_, Vec<u8>>(conn, &hash_ref).unwrap();
            assert_eq!(read, Some(kp));

            delete_key_package(conn, &hash_ref).unwrap();
            let read_after = read_key_package::<_, Vec<u8>>(conn, &hash_ref).unwrap();
            assert_eq!(read_after, None);
        });
    }

    #[test]
    fn test_signature_key_functions() {
        use openmls::group::GroupId;
        with_test_storage(|conn| {
            let pub_key = GroupId::from_slice(&[1u8, 2, 3]);
            let key_pair = b"sig_key_pair".to_vec();

            write_signature_key_pair(conn, &pub_key, &key_pair).unwrap();

            let read = read_signature_key_pair::<_, Vec<u8>>(conn, &pub_key).unwrap();
            assert_eq!(read, Some(key_pair));

            delete_signature_key_pair(conn, &pub_key).unwrap();
            let read_after = read_signature_key_pair::<_, Vec<u8>>(conn, &pub_key).unwrap();
            assert_eq!(read_after, None);
        });
    }

    #[test]
    fn test_encryption_key_functions() {
        use openmls::group::GroupId;
        with_test_storage(|conn| {
            let pub_key = GroupId::from_slice(&[1u8, 2, 3]);
            let key_pair = b"enc_key_pair".to_vec();

            write_encryption_key_pair(conn, &pub_key, &key_pair).unwrap();

            let read = read_encryption_key_pair::<_, Vec<u8>>(conn, &pub_key).unwrap();
            assert_eq!(read, Some(key_pair));

            delete_encryption_key_pair(conn, &pub_key).unwrap();
            let read_after = read_encryption_key_pair::<_, Vec<u8>>(conn, &pub_key).unwrap();
            assert_eq!(read_after, None);
        });
    }

    #[test]
    fn test_epoch_key_pairs_functions() {
        use openmls::group::GroupId;
        // Using GroupId for EpochKey as well (needs Key)
        with_test_storage(|conn| {
            let group_id = GroupId::from_slice(&[1u8]);
            let epoch = GroupId::from_slice(&[10u8]);
            let leaf_index = 5;
            let keys = vec![b"k1".to_vec(), b"k2".to_vec()];

            write_encryption_epoch_key_pairs(conn, &group_id, &epoch, leaf_index, &keys).unwrap();

            let read = read_encryption_epoch_key_pairs::<_, _, Vec<u8>>(
                conn, &group_id, &epoch, leaf_index,
            )
            .unwrap();
            assert_eq!(read, keys);

            delete_encryption_epoch_key_pairs(conn, &group_id, &epoch, leaf_index).unwrap();
            let read_after = read_encryption_epoch_key_pairs::<_, _, Vec<u8>>(
                conn, &group_id, &epoch, leaf_index,
            )
            .unwrap();
            assert!(read_after.is_empty());
        });
    }

    #[test]
    fn test_psk_functions() {
        use openmls::group::GroupId;
        with_test_storage(|conn| {
            let psk_id = GroupId::from_slice(&[1u8, 2, 3]);
            let psk = b"psk_data".to_vec();

            write_psk(conn, &psk_id, &psk).unwrap();

            let read = read_psk::<_, Vec<u8>>(conn, &psk_id).unwrap();
            assert_eq!(read, Some(psk));

            delete_psk(conn, &psk_id).unwrap();
            let read_after = read_psk::<_, Vec<u8>>(conn, &psk_id).unwrap();
            assert_eq!(read_after, None);
        });
    }
}
