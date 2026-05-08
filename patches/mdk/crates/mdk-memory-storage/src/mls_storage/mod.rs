//! OpenMLS StorageProvider implementation for in-memory storage.
//!
//! This module implements the `StorageProvider<1>` trait from `openmls_traits`
//! directly on `MdkMemoryStorage`, enabling unified storage for both MLS
//! cryptographic state and MDK-specific data within a single storage structure.

// Allow complex types for MLS storage structures - these maps require compound keys
// for proper data organization and the complexity is inherent to the domain.
#![allow(clippy::type_complexity)]

use std::collections::HashMap;

use mdk_storage_traits::MdkStorageError;
pub use mdk_storage_traits::mls_codec::{GroupDataType, MlsCodec};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The storage provider version matching OpenMLS's CURRENT_VERSION.
pub const STORAGE_PROVIDER_VERSION: u16 = 1;

// In-memory data structures now expect external locking via MdkMemoryStorage
// Key: (group_id bytes, data type)
// Value: serialized data bytes
#[derive(Default)]
pub struct MlsGroupData {
    pub(crate) data: HashMap<(Vec<u8>, GroupDataType), Vec<u8>>,
}

impl std::fmt::Debug for MlsGroupData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsGroupData")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsGroupData {
    /// Creates a new empty MLS group data store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write group data.
    pub fn write<GroupId, GroupData>(
        &mut self,
        group_id: &GroupId,
        data_type: GroupDataType,
        data: &GroupData,
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
        GroupData: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let data_bytes = serialize_entity(data)?;
        self.data.insert((group_id_bytes, data_type), data_bytes);
        Ok(())
    }

    /// Read group data.
    pub fn read<GroupId, GroupData>(
        &self,
        group_id: &GroupId,
        data_type: GroupDataType,
    ) -> Result<Option<GroupData>, MdkStorageError>
    where
        GroupId: Serialize,
        GroupData: DeserializeOwned,
    {
        let group_id_bytes = serialize_key(group_id)?;
        match self.data.get(&(group_id_bytes, data_type)) {
            Some(bytes) => Ok(Some(deserialize_entity(bytes)?)),
            None => Ok(None),
        }
    }

    /// Delete group data.
    pub fn delete<GroupId>(
        &mut self,
        group_id: &GroupId,
        data_type: GroupDataType,
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        self.data.remove(&(group_id_bytes, data_type));
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<(Vec<u8>, GroupDataType), Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<(Vec<u8>, GroupDataType), Vec<u8>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS own leaf nodes.
/// Key: group_id bytes
/// Value: list of serialized leaf node bytes (in insertion order)
#[derive(Default)]
pub struct MlsOwnLeafNodes {
    pub(crate) data: HashMap<Vec<u8>, Vec<Vec<u8>>>,
}

impl std::fmt::Debug for MlsOwnLeafNodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsOwnLeafNodes")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsOwnLeafNodes {
    /// Creates a new empty own leaf nodes store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a leaf node for a group.
    pub fn append<GroupId, LeafNode>(
        &mut self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
        LeafNode: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let leaf_node_bytes = serialize_entity(leaf_node)?;
        self.data
            .entry(group_id_bytes)
            .or_default()
            .push(leaf_node_bytes);
        Ok(())
    }

    /// Read all leaf nodes for a group.
    pub fn read<GroupId, LeafNode>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<LeafNode>, MdkStorageError>
    where
        GroupId: Serialize,
        LeafNode: DeserializeOwned,
    {
        let group_id_bytes = serialize_key(group_id)?;
        match self.data.get(&group_id_bytes) {
            Some(leaf_nodes) => {
                let mut result = Vec::with_capacity(leaf_nodes.len());
                for bytes in leaf_nodes {
                    result.push(deserialize_entity(bytes)?);
                }
                Ok(result)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Delete all leaf nodes for a group.
    pub fn delete<GroupId>(&mut self, group_id: &GroupId) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        self.data.remove(&group_id_bytes);
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<Vec<u8>, Vec<Vec<u8>>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<Vec<u8>, Vec<Vec<u8>>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS proposals.
/// Key: (group_id bytes, proposal_ref bytes)
/// Value: serialized proposal bytes
#[derive(Default)]
pub struct MlsProposals {
    pub(crate) data: HashMap<(Vec<u8>, Vec<u8>), Vec<u8>>,
}

impl std::fmt::Debug for MlsProposals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsProposals")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsProposals {
    /// Creates a new empty proposals store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a proposal.
    pub fn queue<GroupId, ProposalRef, QueuedProposal>(
        &mut self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
        proposal: &QueuedProposal,
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
        ProposalRef: Serialize,
        QueuedProposal: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let proposal_ref_bytes = serialize_key(proposal_ref)?;
        let proposal_bytes = serialize_entity(proposal)?;
        self.data
            .insert((group_id_bytes, proposal_ref_bytes), proposal_bytes);
        Ok(())
    }

    /// Read all proposal refs for a group.
    pub fn read_refs<GroupId, ProposalRef>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProposalRef>, MdkStorageError>
    where
        GroupId: Serialize,
        ProposalRef: DeserializeOwned,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let mut refs = Vec::new();
        for (key, _) in self.data.iter() {
            if key.0 == group_id_bytes {
                refs.push(deserialize_entity(&key.1)?);
            }
        }
        Ok(refs)
    }

    /// Read all proposals for a group.
    pub fn read_proposals<GroupId, ProposalRef, QueuedProposal>(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(ProposalRef, QueuedProposal)>, MdkStorageError>
    where
        GroupId: Serialize,
        ProposalRef: DeserializeOwned,
        QueuedProposal: DeserializeOwned,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let mut proposals = Vec::new();
        for ((gid, ref_bytes), proposal_bytes) in self.data.iter() {
            if *gid == group_id_bytes {
                let proposal_ref: ProposalRef = deserialize_entity(ref_bytes)?;
                let proposal: QueuedProposal = deserialize_entity(proposal_bytes)?;
                proposals.push((proposal_ref, proposal));
            }
        }
        Ok(proposals)
    }

    /// Remove a single proposal.
    pub fn remove<GroupId, ProposalRef>(
        &mut self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
        ProposalRef: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let proposal_ref_bytes = serialize_key(proposal_ref)?;
        self.data.remove(&(group_id_bytes, proposal_ref_bytes));
        Ok(())
    }

    /// Clear all proposals for a group.
    pub fn clear<GroupId>(&mut self, group_id: &GroupId) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        self.data.retain(|(gid, _), _| *gid != group_id_bytes);
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<(Vec<u8>, Vec<u8>), Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<(Vec<u8>, Vec<u8>), Vec<u8>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS key packages.
/// Key: hash_ref bytes
/// Value: serialized key package bytes
#[derive(Default)]
pub struct MlsKeyPackages {
    pub(crate) data: HashMap<Vec<u8>, Vec<u8>>,
}

impl std::fmt::Debug for MlsKeyPackages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsKeyPackages")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsKeyPackages {
    /// Creates a new empty key packages store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a key package.
    pub fn write<HashReference, KeyPackage>(
        &mut self,
        hash_ref: &HashReference,
        key_package: &KeyPackage,
    ) -> Result<(), MdkStorageError>
    where
        HashReference: Serialize,
        KeyPackage: Serialize,
    {
        let hash_ref_bytes = serialize_key(hash_ref)?;
        let key_package_bytes = serialize_entity(key_package)?;
        self.data.insert(hash_ref_bytes, key_package_bytes);
        Ok(())
    }

    /// Read a key package.
    pub fn read<HashReference, KeyPackage>(
        &self,
        hash_ref: &HashReference,
    ) -> Result<Option<KeyPackage>, MdkStorageError>
    where
        HashReference: Serialize,
        KeyPackage: DeserializeOwned,
    {
        let hash_ref_bytes = serialize_key(hash_ref)?;
        match self.data.get(&hash_ref_bytes) {
            Some(bytes) => Ok(Some(deserialize_entity(bytes)?)),
            None => Ok(None),
        }
    }

    /// Delete a key package.
    pub fn delete<HashReference>(&mut self, hash_ref: &HashReference) -> Result<(), MdkStorageError>
    where
        HashReference: Serialize,
    {
        let hash_ref_bytes = serialize_key(hash_ref)?;
        self.data.remove(&hash_ref_bytes);
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<Vec<u8>, Vec<u8>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS PSKs.
/// Key: psk_id bytes
/// Value: serialized PSK bundle bytes
#[derive(Default)]
pub struct MlsPsks {
    pub(crate) data: HashMap<Vec<u8>, Vec<u8>>,
}

impl std::fmt::Debug for MlsPsks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsPsks")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsPsks {
    /// Creates a new empty PSKs store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a PSK.
    pub fn write<PskId, PskBundle>(
        &mut self,
        psk_id: &PskId,
        psk: &PskBundle,
    ) -> Result<(), MdkStorageError>
    where
        PskId: Serialize,
        PskBundle: Serialize,
    {
        let psk_id_bytes = serialize_key(psk_id)?;
        let psk_bytes = serialize_entity(psk)?;
        self.data.insert(psk_id_bytes, psk_bytes);
        Ok(())
    }

    /// Read a PSK.
    pub fn read<PskId, PskBundle>(
        &self,
        psk_id: &PskId,
    ) -> Result<Option<PskBundle>, MdkStorageError>
    where
        PskId: Serialize,
        PskBundle: DeserializeOwned,
    {
        let psk_id_bytes = serialize_key(psk_id)?;
        match self.data.get(&psk_id_bytes) {
            Some(bytes) => Ok(Some(deserialize_entity(bytes)?)),
            None => Ok(None),
        }
    }

    /// Delete a PSK.
    pub fn delete<PskId>(&mut self, psk_id: &PskId) -> Result<(), MdkStorageError>
    where
        PskId: Serialize,
    {
        let psk_id_bytes = serialize_key(psk_id)?;
        self.data.remove(&psk_id_bytes);
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<Vec<u8>, Vec<u8>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS signature keys.
/// Key: public_key bytes
/// Value: serialized signature key pair bytes
#[derive(Default)]
pub struct MlsSignatureKeys {
    pub(crate) data: HashMap<Vec<u8>, Vec<u8>>,
}

impl std::fmt::Debug for MlsSignatureKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsSignatureKeys")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsSignatureKeys {
    /// Creates a new empty signature keys store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a signature key pair.
    pub fn write<SignaturePublicKey, SignatureKeyPair>(
        &mut self,
        public_key: &SignaturePublicKey,
        key_pair: &SignatureKeyPair,
    ) -> Result<(), MdkStorageError>
    where
        SignaturePublicKey: Serialize,
        SignatureKeyPair: Serialize,
    {
        let public_key_bytes = serialize_key(public_key)?;
        let key_pair_bytes = serialize_entity(key_pair)?;
        self.data.insert(public_key_bytes, key_pair_bytes);
        Ok(())
    }

    /// Read a signature key pair.
    pub fn read<SignaturePublicKey, SignatureKeyPair>(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<Option<SignatureKeyPair>, MdkStorageError>
    where
        SignaturePublicKey: Serialize,
        SignatureKeyPair: DeserializeOwned,
    {
        let public_key_bytes = serialize_key(public_key)?;
        match self.data.get(&public_key_bytes) {
            Some(bytes) => Ok(Some(deserialize_entity(bytes)?)),
            None => Ok(None),
        }
    }

    /// Delete a signature key pair.
    pub fn delete<SignaturePublicKey>(
        &mut self,
        public_key: &SignaturePublicKey,
    ) -> Result<(), MdkStorageError>
    where
        SignaturePublicKey: Serialize,
    {
        let public_key_bytes = serialize_key(public_key)?;
        self.data.remove(&public_key_bytes);
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<Vec<u8>, Vec<u8>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS encryption keys.
/// Key: public_key bytes
/// Value: serialized HPKE key pair bytes
#[derive(Default)]
pub struct MlsEncryptionKeys {
    pub(crate) data: HashMap<Vec<u8>, Vec<u8>>,
}

impl std::fmt::Debug for MlsEncryptionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsEncryptionKeys")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsEncryptionKeys {
    /// Creates a new empty encryption keys store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write an encryption key pair.
    pub fn write<EncryptionKey, HpkeKeyPair>(
        &mut self,
        public_key: &EncryptionKey,
        key_pair: &HpkeKeyPair,
    ) -> Result<(), MdkStorageError>
    where
        EncryptionKey: Serialize,
        HpkeKeyPair: Serialize,
    {
        let public_key_bytes = serialize_key(public_key)?;
        let key_pair_bytes = serialize_entity(key_pair)?;
        self.data.insert(public_key_bytes, key_pair_bytes);
        Ok(())
    }

    /// Read an encryption key pair.
    pub fn read<EncryptionKey, HpkeKeyPair>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<Option<HpkeKeyPair>, MdkStorageError>
    where
        EncryptionKey: Serialize,
        HpkeKeyPair: DeserializeOwned,
    {
        let public_key_bytes = serialize_key(public_key)?;
        match self.data.get(&public_key_bytes) {
            Some(bytes) => Ok(Some(deserialize_entity(bytes)?)),
            None => Ok(None),
        }
    }

    /// Delete an encryption key pair.
    pub fn delete<EncryptionKey>(
        &mut self,
        public_key: &EncryptionKey,
    ) -> Result<(), MdkStorageError>
    where
        EncryptionKey: Serialize,
    {
        let public_key_bytes = serialize_key(public_key)?;
        self.data.remove(&public_key_bytes);
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<Vec<u8>, Vec<u8>>) {
        self.data = data;
    }
}

/// In-memory storage for MLS epoch key pairs.
/// Key: (group_id bytes, epoch bytes, leaf_index)
/// Value: serialized list of HPKE key pairs
#[derive(Default)]
pub struct MlsEpochKeyPairs {
    pub(crate) data: HashMap<(Vec<u8>, Vec<u8>, u32), Vec<u8>>,
}

impl std::fmt::Debug for MlsEpochKeyPairs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlsEpochKeyPairs")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

impl MlsEpochKeyPairs {
    /// Creates a new empty epoch key pairs store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write epoch encryption key pairs.
    pub fn write<GroupId, EpochKey, HpkeKeyPair>(
        &mut self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
        key_pairs: &[HpkeKeyPair],
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
        EpochKey: Serialize,
        HpkeKeyPair: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let epoch_bytes = serialize_key(epoch)?;
        let key_pairs_bytes = serialize_entity(&key_pairs)?;
        self.data
            .insert((group_id_bytes, epoch_bytes, leaf_index), key_pairs_bytes);
        Ok(())
    }

    /// Read epoch encryption key pairs.
    pub fn read<GroupId, EpochKey, HpkeKeyPair>(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<Vec<HpkeKeyPair>, MdkStorageError>
    where
        GroupId: Serialize,
        EpochKey: Serialize,
        HpkeKeyPair: DeserializeOwned,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let epoch_bytes = serialize_key(epoch)?;
        match self.data.get(&(group_id_bytes, epoch_bytes, leaf_index)) {
            Some(bytes) => deserialize_entity(bytes),
            None => Ok(Vec::new()),
        }
    }

    /// Delete epoch encryption key pairs.
    pub fn delete<GroupId, EpochKey>(
        &mut self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<(), MdkStorageError>
    where
        GroupId: Serialize,
        EpochKey: Serialize,
    {
        let group_id_bytes = serialize_key(group_id)?;
        let epoch_bytes = serialize_key(epoch)?;
        self.data.remove(&(group_id_bytes, epoch_bytes, leaf_index));
        Ok(())
    }

    /// Clone all data for snapshotting.
    pub fn clone_data(&self) -> HashMap<(Vec<u8>, Vec<u8>, u32), Vec<u8>> {
        self.data.clone()
    }

    /// Restore data from a snapshot.
    pub fn restore_data(&mut self, data: HashMap<(Vec<u8>, Vec<u8>, u32), Vec<u8>>) {
        self.data = data;
    }
}

// ============================================================================
// Helper functions for serialization
// ============================================================================

/// Serialize a key to bytes for storage.
fn serialize_key<K>(key: &K) -> Result<Vec<u8>, MdkStorageError>
where
    K: Serialize,
{
    MlsCodec::serialize(key)
}

/// Serialize an entity to bytes for storage.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_data_type_equality() {
        assert_eq!(
            GroupDataType::JoinGroupConfig,
            GroupDataType::JoinGroupConfig
        );
        assert_ne!(GroupDataType::JoinGroupConfig, GroupDataType::Tree);
    }

    #[test]
    fn test_mls_group_data_basic() {
        let mut store = MlsGroupData::new();
        let group_id = vec![1u8, 2, 3, 4];
        let data = "test data".to_string();

        // Write data
        store.write(&group_id, GroupDataType::Tree, &data).unwrap();

        // Read data
        let result: Option<String> = store.read(&group_id, GroupDataType::Tree).unwrap();
        assert_eq!(result, Some("test data".to_string()));

        // Read non-existent data type
        let result: Option<String> = store.read(&group_id, GroupDataType::Context).unwrap();
        assert!(result.is_none());

        // Delete data
        store
            .delete::<Vec<u8>>(&group_id, GroupDataType::Tree)
            .unwrap();
        let result: Option<String> = store.read(&group_id, GroupDataType::Tree).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_mls_key_packages_basic() {
        let mut store = MlsKeyPackages::new();
        let hash_ref = vec![1u8, 2, 3, 4];
        let key_package = "key package data".to_string();

        // Write key package
        store.write(&hash_ref, &key_package).unwrap();

        // Read key package
        let result: Option<String> = store.read(&hash_ref).unwrap();
        assert_eq!(result, Some("key package data".to_string()));

        // Delete key package
        store.delete(&hash_ref).unwrap();
        let result: Option<String> = store.read(&hash_ref).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_mls_own_leaf_nodes_basic() {
        let mut store = MlsOwnLeafNodes::new();
        let group_id = vec![1u8, 2, 3, 4];

        // Append leaf nodes
        store.append(&group_id, &"leaf1".to_string()).unwrap();
        store.append(&group_id, &"leaf2".to_string()).unwrap();

        // Read leaf nodes
        let result: Vec<String> = store.read(&group_id).unwrap();
        assert_eq!(result, vec!["leaf1".to_string(), "leaf2".to_string()]);

        // Delete leaf nodes
        store.delete(&group_id).unwrap();
        let result: Vec<String> = store.read(&group_id).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_snapshot_restore() {
        let mut store = MlsGroupData::new();
        let group_id = vec![1u8, 2, 3, 4];

        // Write initial data
        store
            .write(&group_id, GroupDataType::Tree, &"original".to_string())
            .unwrap();

        // Take snapshot
        let snapshot = store.clone_data();

        // Modify data
        store
            .write(&group_id, GroupDataType::Tree, &"modified".to_string())
            .unwrap();

        // Verify modification
        let result: Option<String> = store.read(&group_id, GroupDataType::Tree).unwrap();
        assert_eq!(result, Some("modified".to_string()));

        // Restore snapshot
        store.restore_data(snapshot);

        // Verify restoration
        let result: Option<String> = store.read(&group_id, GroupDataType::Tree).unwrap();
        assert_eq!(result, Some("original".to_string()));
    }

    // ========================================
    // MlsProposals Tests
    // ========================================

    #[test]
    fn test_proposals_queue_and_read() {
        let mut store = MlsProposals::new();
        let group_id = vec![1u8, 2, 3, 4];
        let proposal_ref = vec![10u8, 20, 30];
        let proposal = "test proposal".to_string();

        // Queue proposal
        store.queue(&group_id, &proposal_ref, &proposal).unwrap();

        // Read proposal refs
        let refs: Vec<Vec<u8>> = store.read_refs(&group_id).unwrap();
        assert_eq!(refs, vec![proposal_ref.clone()]);

        // Read proposals
        let proposals: Vec<(Vec<u8>, String)> = store.read_proposals(&group_id).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].0, proposal_ref);
        assert_eq!(proposals[0].1, proposal);
    }

    #[test]
    fn test_proposals_remove_single() {
        let mut store = MlsProposals::new();
        let group_id = vec![1u8, 2, 3, 4];
        let proposal_ref = vec![10u8, 20, 30];
        let proposal = "test proposal".to_string();

        // Queue and remove
        store.queue(&group_id, &proposal_ref, &proposal).unwrap();
        store.remove(&group_id, &proposal_ref).unwrap();

        // Verify removed
        let proposals: Vec<(Vec<u8>, String)> = store.read_proposals(&group_id).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_proposals_clear() {
        let mut store = MlsProposals::new();
        let group_id = vec![1u8, 2, 3, 4];

        // Queue multiple proposals
        for i in 0..3 {
            let proposal_ref = vec![i as u8; 4];
            store
                .queue(&group_id, &proposal_ref, &format!("proposal_{}", i))
                .unwrap();
        }

        // Clear all
        store.clear(&group_id).unwrap();

        // Verify empty
        let proposals: Vec<(Vec<u8>, String)> = store.read_proposals(&group_id).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_proposals_read_empty() {
        let store = MlsProposals::new();
        let group_id = vec![1u8, 2, 3, 4];

        let refs: Vec<Vec<u8>> = store.read_refs(&group_id).unwrap();
        assert!(refs.is_empty());

        let proposals: Vec<(Vec<u8>, String)> = store.read_proposals(&group_id).unwrap();
        assert!(proposals.is_empty());
    }

    #[test]
    fn test_proposals_snapshot_restore() {
        let mut store = MlsProposals::new();
        let group_id = vec![1u8, 2, 3, 4];
        let proposal_ref = vec![10u8, 20, 30];

        // Queue proposal
        store
            .queue(&group_id, &proposal_ref, &"original".to_string())
            .unwrap();

        // Take snapshot
        let snapshot = store.clone_data();

        // Clear proposals
        store.clear(&group_id).unwrap();

        // Verify cleared
        let proposals: Vec<(Vec<u8>, String)> = store.read_proposals(&group_id).unwrap();
        assert!(proposals.is_empty());

        // Restore snapshot
        store.restore_data(snapshot);

        // Verify restored
        let proposals: Vec<(Vec<u8>, String)> = store.read_proposals(&group_id).unwrap();
        assert_eq!(proposals.len(), 1);
    }

    // ========================================
    // MlsPsks Tests
    // ========================================

    #[test]
    fn test_psks_write_and_read() {
        let mut store = MlsPsks::new();
        let psk_id = vec![1u8, 2, 3, 4];
        let psk_bundle = "psk bundle data".to_string();

        // Write PSK
        store.write(&psk_id, &psk_bundle).unwrap();

        // Read PSK
        let result: Option<String> = store.read(&psk_id).unwrap();
        assert_eq!(result, Some(psk_bundle));
    }

    #[test]
    fn test_psks_read_nonexistent() {
        let store = MlsPsks::new();
        let psk_id = vec![1u8, 2, 3, 4];

        let result: Option<String> = store.read(&psk_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_psks_delete() {
        let mut store = MlsPsks::new();
        let psk_id = vec![1u8, 2, 3, 4];
        let psk_bundle = "psk bundle".to_string();

        // Write and delete
        store.write(&psk_id, &psk_bundle).unwrap();
        store.delete(&psk_id).unwrap();

        // Verify deleted
        let result: Option<String> = store.read(&psk_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_psks_overwrite() {
        let mut store = MlsPsks::new();
        let psk_id = vec![1u8, 2, 3, 4];

        // Write first
        store.write(&psk_id, &"first".to_string()).unwrap();

        // Overwrite
        store.write(&psk_id, &"second".to_string()).unwrap();

        // Verify second
        let result: Option<String> = store.read(&psk_id).unwrap();
        assert_eq!(result, Some("second".to_string()));
    }

    #[test]
    fn test_psks_snapshot_restore() {
        let mut store = MlsPsks::new();
        let psk_id = vec![1u8, 2, 3, 4];

        // Write
        store.write(&psk_id, &"original".to_string()).unwrap();

        // Snapshot
        let snapshot = store.clone_data();

        // Modify
        store.write(&psk_id, &"modified".to_string()).unwrap();

        // Restore
        store.restore_data(snapshot);

        // Verify
        let result: Option<String> = store.read(&psk_id).unwrap();
        assert_eq!(result, Some("original".to_string()));
    }

    // ========================================
    // MlsSignatureKeys Tests
    // ========================================

    #[test]
    fn test_signature_keys_write_and_read() {
        let mut store = MlsSignatureKeys::new();
        let public_key = vec![1u8, 2, 3, 4];
        let key_pair = "signature key pair".to_string();

        // Write
        store.write(&public_key, &key_pair).unwrap();

        // Read
        let result: Option<String> = store.read(&public_key).unwrap();
        assert_eq!(result, Some(key_pair));
    }

    #[test]
    fn test_signature_keys_read_nonexistent() {
        let store = MlsSignatureKeys::new();
        let public_key = vec![1u8, 2, 3, 4];

        let result: Option<String> = store.read(&public_key).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_signature_keys_delete() {
        let mut store = MlsSignatureKeys::new();
        let public_key = vec![1u8, 2, 3, 4];
        let key_pair = "signature key pair".to_string();

        // Write and delete
        store.write(&public_key, &key_pair).unwrap();
        store.delete(&public_key).unwrap();

        // Verify deleted
        let result: Option<String> = store.read(&public_key).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_signature_keys_snapshot_restore() {
        let mut store = MlsSignatureKeys::new();
        let public_key = vec![1u8, 2, 3, 4];

        // Write
        store.write(&public_key, &"original".to_string()).unwrap();

        // Snapshot
        let snapshot = store.clone_data();

        // Modify
        store.write(&public_key, &"modified".to_string()).unwrap();

        // Restore
        store.restore_data(snapshot);

        // Verify
        let result: Option<String> = store.read(&public_key).unwrap();
        assert_eq!(result, Some("original".to_string()));
    }

    // ========================================
    // MlsEncryptionKeys Tests
    // ========================================

    #[test]
    fn test_encryption_keys_write_and_read() {
        let mut store = MlsEncryptionKeys::new();
        let public_key = vec![1u8, 2, 3, 4];
        let key_pair = "encryption key pair".to_string();

        // Write
        store.write(&public_key, &key_pair).unwrap();

        // Read
        let result: Option<String> = store.read(&public_key).unwrap();
        assert_eq!(result, Some(key_pair));
    }

    #[test]
    fn test_encryption_keys_read_nonexistent() {
        let store = MlsEncryptionKeys::new();
        let public_key = vec![1u8, 2, 3, 4];

        let result: Option<String> = store.read(&public_key).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_encryption_keys_delete() {
        let mut store = MlsEncryptionKeys::new();
        let public_key = vec![1u8, 2, 3, 4];
        let key_pair = "encryption key pair".to_string();

        // Write and delete
        store.write(&public_key, &key_pair).unwrap();
        store.delete(&public_key).unwrap();

        // Verify deleted
        let result: Option<String> = store.read(&public_key).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_encryption_keys_snapshot_restore() {
        let mut store = MlsEncryptionKeys::new();
        let public_key = vec![1u8, 2, 3, 4];

        // Write
        store.write(&public_key, &"original".to_string()).unwrap();

        // Snapshot
        let snapshot = store.clone_data();

        // Modify
        store.write(&public_key, &"modified".to_string()).unwrap();

        // Restore
        store.restore_data(snapshot);

        // Verify
        let result: Option<String> = store.read(&public_key).unwrap();
        assert_eq!(result, Some("original".to_string()));
    }

    // ========================================
    // MlsEpochKeyPairs Tests
    // ========================================

    #[test]
    fn test_epoch_key_pairs_write_and_read() {
        let mut store = MlsEpochKeyPairs::new();
        let group_id = vec![1u8, 2, 3, 4];
        let epoch = 5u64;
        let leaf_index = 0u32;
        let key_pairs = vec!["key1".to_string(), "key2".to_string()];

        // Write
        store
            .write(&group_id, &epoch, leaf_index, &key_pairs)
            .unwrap();

        // Read
        let result: Vec<String> = store.read(&group_id, &epoch, leaf_index).unwrap();
        assert_eq!(result, key_pairs);
    }

    #[test]
    fn test_epoch_key_pairs_read_nonexistent() {
        let store = MlsEpochKeyPairs::new();
        let group_id = vec![1u8, 2, 3, 4];
        let epoch = 5u64;
        let leaf_index = 0u32;

        let result: Vec<String> = store.read(&group_id, &epoch, leaf_index).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_epoch_key_pairs_delete() {
        let mut store = MlsEpochKeyPairs::new();
        let group_id = vec![1u8, 2, 3, 4];
        let epoch = 5u64;
        let leaf_index = 0u32;
        let key_pairs = vec!["key".to_string()];

        // Write and delete
        store
            .write(&group_id, &epoch, leaf_index, &key_pairs)
            .unwrap();
        store.delete(&group_id, &epoch, leaf_index).unwrap();

        // Verify deleted (returns empty vec)
        let result: Vec<String> = store.read(&group_id, &epoch, leaf_index).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_epoch_key_pairs_different_epochs() {
        let mut store = MlsEpochKeyPairs::new();
        let group_id = vec![1u8, 2, 3, 4];
        let leaf_index = 0u32;

        let keys_epoch_1 = ["epoch1".to_string()];
        let keys_epoch_2 = ["epoch2".to_string()];

        // Write for different epochs
        store
            .write(&group_id, &1u64, leaf_index, &keys_epoch_1)
            .unwrap();
        store
            .write(&group_id, &2u64, leaf_index, &keys_epoch_2)
            .unwrap();

        // Verify each epoch
        let result1: Vec<String> = store.read(&group_id, &1u64, leaf_index).unwrap();
        let result2: Vec<String> = store.read(&group_id, &2u64, leaf_index).unwrap();

        assert_eq!(result1, vec!["epoch1".to_string()]);
        assert_eq!(result2, vec!["epoch2".to_string()]);
    }

    #[test]
    fn test_epoch_key_pairs_different_leaf_indices() {
        let mut store = MlsEpochKeyPairs::new();
        let group_id = vec![1u8, 2, 3, 4];
        let epoch = 1u64;

        let keys_leaf_0 = ["leaf0".to_string()];
        let keys_leaf_1 = ["leaf1".to_string()];

        // Write for different leaf indices
        store.write(&group_id, &epoch, 0, &keys_leaf_0).unwrap();
        store.write(&group_id, &epoch, 1, &keys_leaf_1).unwrap();

        // Verify each leaf index
        let result0: Vec<String> = store.read(&group_id, &epoch, 0).unwrap();
        let result1: Vec<String> = store.read(&group_id, &epoch, 1).unwrap();

        assert_eq!(result0, vec!["leaf0".to_string()]);
        assert_eq!(result1, vec!["leaf1".to_string()]);
    }

    #[test]
    fn test_epoch_key_pairs_snapshot_restore() {
        let mut store = MlsEpochKeyPairs::new();
        let group_id = vec![1u8, 2, 3, 4];
        let epoch = 1u64;
        let leaf_index = 0u32;

        let original = ["original".to_string()];
        let modified = ["modified".to_string()];

        // Write
        store
            .write(&group_id, &epoch, leaf_index, &original)
            .unwrap();

        // Snapshot
        let snapshot = store.clone_data();

        // Modify
        store
            .write(&group_id, &epoch, leaf_index, &modified)
            .unwrap();

        // Restore
        store.restore_data(snapshot);

        // Verify
        let result: Vec<String> = store.read(&group_id, &epoch, leaf_index).unwrap();
        assert_eq!(result, vec!["original".to_string()]);
    }

    // ========================================
    // Serialization Tests
    // ========================================

    #[test]
    fn test_serialize_key_success() {
        let key = vec![1u8, 2, 3, 4];
        let result = serialize_key(&key);
        assert!(result.is_ok());
    }

    #[test]
    fn test_serialize_entity_success() {
        let entity = "test entity".to_string();
        let result = serialize_entity(&entity);
        assert!(result.is_ok());
    }

    #[test]
    fn test_deserialize_entity_success() {
        let original = "test entity".to_string();
        let serialized = serialize_entity(&original).unwrap();
        let result: String = deserialize_entity(&serialized).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn test_deserialize_entity_invalid_data() {
        let invalid = b"not valid data";
        let result: Result<String, _> = deserialize_entity(invalid);
        assert!(result.is_err());
    }
}
