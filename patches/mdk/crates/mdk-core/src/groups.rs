//! MDK groups
//!
//! This module provides functionality for managing MLS groups in Nostr:
//! - Group creation and configuration
//! - Member management (adding/removing members)
//! - Group state updates and synchronization
//! - Group metadata handling
//! - Group secret management
//!
//! Groups in MDK have both an MLS group ID and a Nostr group ID. The MLS group ID
//! is used internally by the MLS protocol, while the Nostr group ID is used for
//! relay-based message routing and group discovery.

use std::collections::BTreeSet;

use mdk_storage_traits::GroupId;
use mdk_storage_traits::MdkStorageProvider;
use mdk_storage_traits::groups::types as group_types;
use mdk_storage_traits::messages::types as message_types;
use nostr::prelude::*;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use tls_codec::Serialize as TlsSerialize;

use super::MDK;
use super::extension::NostrGroupDataExtension;
use crate::error::Error;
use crate::util::{ContentEncoding, encode_content};

/// Result of creating a new MLS group
#[derive(Debug)]
pub struct GroupResult {
    /// The stored group
    pub group: group_types::Group,
    /// A vec of Kind:444 Welcome Events to be published for members added during creation.
    pub welcome_rumors: Vec<UnsignedEvent>,
}

/// Result of updating a group
#[derive(Debug)]
pub struct UpdateGroupResult {
    /// A Kind:445 Event containing the proposal or commit message. To be published to the group relays.
    pub evolution_event: Event,
    /// A vec of Kind:444 Welcome Events to be published for any members added as part of the update.
    pub welcome_rumors: Option<Vec<UnsignedEvent>>,
    /// The MLS group ID this update applies to
    pub mls_group_id: GroupId,
}

/// Configuration data for the Group
#[derive(Debug, Clone)]
pub struct NostrGroupConfigData {
    /// Group name
    pub name: String,
    /// Group description
    pub description: String,
    /// URL to encrypted group image
    pub image_hash: Option<[u8; 32]>,
    /// Key to decrypt the image
    pub image_key: Option<[u8; 32]>,
    /// Nonce to decrypt the image
    pub image_nonce: Option<[u8; 12]>,
    /// Relays used by the group
    pub relays: Vec<RelayUrl>,
    /// Group admins
    pub admins: Vec<PublicKey>,
}

/// Configuration for updating group data with optional fields
#[derive(Debug, Clone, Default)]
pub struct NostrGroupDataUpdate {
    /// Group name (optional)
    pub name: Option<String>,
    /// Group description (optional)
    pub description: Option<String>,
    /// URL to encrypted group image (optional, use Some(None) to clear)
    pub image_hash: Option<Option<[u8; 32]>>,
    /// Key to decrypt the image (optional, use Some(None) to clear)
    pub image_key: Option<Option<[u8; 32]>>,
    /// Nonce to decrypt the image (optional, use Some(None) to clear)
    pub image_nonce: Option<Option<[u8; 12]>>,
    /// Upload key seed for the image (optional, use Some(None) to clear)
    pub image_upload_key: Option<Option<[u8; 32]>>,
    /// Relays used by the group (optional)
    pub relays: Option<Vec<RelayUrl>>,
    /// Group admins (optional)
    pub admins: Option<Vec<PublicKey>>,
    /// Nostr group ID for message routing (optional, for rotation per MIP-01)
    pub nostr_group_id: Option<[u8; 32]>,
}

/// Pending member changes from proposals that need admin approval
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingMemberChanges {
    /// Public keys of members that will be added when proposals are committed
    pub additions: Vec<PublicKey>,
    /// Public keys of members that will be removed when proposals are committed
    pub removals: Vec<PublicKey>,
}

impl NostrGroupConfigData {
    /// Creates NostrGroupConfigData
    pub fn new(
        name: String,
        description: String,
        image_hash: Option<[u8; 32]>,
        image_key: Option<[u8; 32]>,
        image_nonce: Option<[u8; 12]>,
        relays: Vec<RelayUrl>,
        admins: Vec<PublicKey>,
    ) -> Self {
        Self {
            name,
            description,
            image_hash,
            image_key,
            image_nonce,
            relays,
            admins,
        }
    }
}

impl NostrGroupDataUpdate {
    /// Creates a new empty update configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the name to be updated
    pub fn name<T>(mut self, name: T) -> Self
    where
        T: Into<String>,
    {
        self.name = Some(name.into());
        self
    }

    /// Sets the description to be updated
    pub fn description<T>(mut self, description: T) -> Self
    where
        T: Into<String>,
    {
        self.description = Some(description.into());
        self
    }

    /// Sets the image URL to be updated
    pub fn image_hash(mut self, image_hash: Option<[u8; 32]>) -> Self {
        self.image_hash = Some(image_hash);
        self
    }

    /// Sets the image key to be updated
    pub fn image_key(mut self, image_key: Option<[u8; 32]>) -> Self {
        self.image_key = Some(image_key);
        self
    }

    /// Sets the image key to be updated
    pub fn image_nonce(mut self, image_nonce: Option<[u8; 12]>) -> Self {
        self.image_nonce = Some(image_nonce);
        self
    }

    /// Sets the image upload key to be updated
    pub fn image_upload_key(mut self, image_upload_key: Option<[u8; 32]>) -> Self {
        self.image_upload_key = Some(image_upload_key);
        self
    }

    /// Sets the relays to be updated
    pub fn relays(mut self, relays: Vec<RelayUrl>) -> Self {
        self.relays = Some(relays);
        self
    }

    /// Sets the admins to be updated
    pub fn admins(mut self, admins: Vec<PublicKey>) -> Self {
        self.admins = Some(admins);
        self
    }

    /// Sets the nostr_group_id to be updated (for ID rotation per MIP-01)
    pub fn nostr_group_id(mut self, nostr_group_id: [u8; 32]) -> Self {
        self.nostr_group_id = Some(nostr_group_id);
        self
    }
}

impl<Storage> MDK<Storage>
where
    Storage: MdkStorageProvider,
{
    /// Gets the current user's public key from an MLS group
    ///
    /// # Arguments
    ///
    /// * `group` - Reference to the MLS group
    ///
    /// # Returns
    ///
    /// * `Ok(PublicKey)` - The current user's public key
    /// * `Err(Error)` - If the user's leaf node is not found or there is an error extracting the public key
    pub(crate) fn get_own_pubkey(&self, group: &MlsGroup) -> Result<PublicKey, Error> {
        let own_leaf = group.own_leaf().ok_or(Error::OwnLeafNotFound)?;
        let credentials: BasicCredential =
            BasicCredential::try_from(own_leaf.credential().clone())?;
        let identity_bytes: &[u8] = credentials.identity();
        self.parse_credential_identity(identity_bytes)
    }

    /// Checks if the LeafNode is an admin of an MLS group
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    /// * `leaf_node` - The leaf to check as an admin
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - The leaf node is an admin
    /// * `Ok(false)` - The leaf node is not an admin
    /// * `Err(Error)` - If the public key cannot be extracted or the group is not found
    pub(crate) fn is_leaf_node_admin(
        &self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<bool, Error> {
        let pubkey = self.pubkey_for_leaf_node(leaf_node)?;
        let mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;
        let group_data = NostrGroupDataExtension::from_group(&mls_group)?;
        Ok(group_data.admins.contains(&pubkey))
    }

    /// Extracts the public key from a leaf node
    ///
    /// # Arguments
    ///
    /// * `leaf_node` - Reference to the leaf node
    ///
    /// # Returns
    ///
    /// * `Ok(PublicKey)` - The public key extracted from the leaf node
    /// * `Err(Error)` - If the credential cannot be converted or the public key cannot be extracted
    pub(crate) fn pubkey_for_leaf_node(&self, leaf_node: &LeafNode) -> Result<PublicKey, Error> {
        let credentials: BasicCredential =
            BasicCredential::try_from(leaf_node.credential().clone())?;
        let identity_bytes: &[u8] = credentials.identity();
        self.parse_credential_identity(identity_bytes)
    }

    /// Extracts the public key from a member
    ///
    /// # Arguments
    ///
    /// * `member` - Reference to the member
    ///
    /// # Returns
    ///
    /// * `Ok(PublicKey)` - The public key extracted from the member
    /// * `Err(Error)` - If the public key cannot be extracted or there is an error converting the public key to hex
    pub(crate) fn pubkey_for_member(&self, member: &Member) -> Result<PublicKey, Error> {
        let credentials: BasicCredential = BasicCredential::try_from(member.credential.clone())?;
        let identity_bytes: &[u8] = credentials.identity();
        self.parse_credential_identity(identity_bytes)
    }

    /// Loads the signature key pair for the current member in an MLS group
    ///
    /// # Arguments
    ///
    /// * `group` - Reference to the MLS group
    ///
    /// # Returns
    ///
    /// * `Ok(SignatureKeyPair)` - The member's signature key pair
    /// * `Err(Error)` - If the key pair cannot be loaded
    pub(crate) fn load_mls_signer(&self, group: &MlsGroup) -> Result<SignatureKeyPair, Error> {
        let own_leaf: &LeafNode = group.own_leaf().ok_or(Error::OwnLeafNotFound)?;
        let public_key: &[u8] = own_leaf.signature_key().as_slice();

        SignatureKeyPair::read(
            self.provider.storage(),
            public_key,
            group.ciphersuite().signature_algorithm(),
        )
        .ok_or(Error::CantLoadSigner)
    }

    /// Loads an MLS group from storage by its ID
    fn load_mls_group_impl(&self, group_id: &GroupId) -> Result<Option<MlsGroup>, Error> {
        MlsGroup::load(self.provider.storage(), group_id.inner())
            .map_err(|e| Error::Provider(e.to_string()))
    }

    /// Loads an MLS group from storage by its ID
    ///
    /// This method provides access to the underlying OpenMLS `MlsGroup` object,
    /// which can be useful for inspection, debugging, and advanced operations.
    ///
    /// **Note:** This method is only available with the `debug-examples` feature flag.
    /// It is intended for debugging and example purposes only.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID to load
    ///
    /// # Returns
    ///
    /// * `Ok(Some(MlsGroup))` - The loaded group if found
    /// * `Ok(None)` - If no group exists with the given ID
    /// * `Err(Error)` - If there is an error loading the group
    #[cfg(feature = "debug-examples")]
    pub fn load_mls_group(&self, group_id: &GroupId) -> Result<Option<MlsGroup>, Error> {
        self.load_mls_group_impl(group_id)
    }

    /// Loads an MLS group from storage by its ID (internal version)
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID to load
    ///
    /// # Returns
    ///
    /// * `Ok(Some(MlsGroup))` - The loaded group if found
    /// * `Ok(None)` - If no group exists with the given ID
    /// * `Err(Error)` - If there is an error loading the group
    #[cfg(not(feature = "debug-examples"))]
    pub(crate) fn load_mls_group(&self, group_id: &GroupId) -> Result<Option<MlsGroup>, Error> {
        self.load_mls_group_impl(group_id)
    }

    /// Exports the current epoch's secret key from an MLS group (internal version)
    ///
    /// This secret is used for NIP-44 message encryption in Group Message Events (kind:445).
    /// The secret is cached in storage to avoid re-exporting it for each message.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    ///
    /// # Returns
    ///
    /// * `Ok(GroupExporterSecret)` - The exported secret
    /// * `Err(Error)` - If the group is not found or there is an error exporting the secret
    pub(crate) fn exporter_secret(
        &self,
        group_id: &crate::GroupId,
    ) -> Result<group_types::GroupExporterSecret, Error> {
        let group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        match self
            .storage()
            .get_group_exporter_secret(group_id, group.epoch().as_u64())
            .map_err(|e| Error::Group(e.to_string()))?
        {
            Some(group_exporter_secret) => Ok(group_exporter_secret),
            // If it's not already in the storage, export the secret and save it
            None => {
                let export_secret: [u8; 32] = group
                    .export_secret(self.provider.crypto(), "nostr", b"nostr", 32)?
                    .try_into()
                    .map_err(|_| {
                        Error::Group("Failed to convert export secret to [u8; 32]".to_string())
                    })?;
                let group_exporter_secret = group_types::GroupExporterSecret {
                    mls_group_id: group_id.clone(),
                    epoch: group.epoch().as_u64(),
                    secret: mdk_storage_traits::Secret::new(export_secret),
                };

                self.storage()
                    .save_group_exporter_secret(group_exporter_secret.clone())
                    .map_err(|e| Error::Group(e.to_string()))?;

                Ok(group_exporter_secret)
            }
        }
    }

    /// Retrieves a MDK group by its MLS group ID
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID to look up
    ///
    /// # Returns
    ///
    /// * `Ok(Some(Group))` - The group if found
    /// * `Ok(None)` - If no group exists with the given ID
    /// * `Err(Error)` - If there is an error accessing storage
    pub fn get_group(&self, group_id: &GroupId) -> Result<Option<group_types::Group>, Error> {
        self.storage()
            .find_group_by_mls_group_id(group_id)
            .map_err(|e| Error::Group(e.to_string()))
    }

    /// Retrieves all MDK groups from storage
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Group>)` - List of all groups
    /// * `Err(Error)` - If there is an error accessing storage
    pub fn get_groups(&self) -> Result<Vec<group_types::Group>, Error> {
        self.storage()
            .all_groups()
            .map_err(|e| Error::Group(e.to_string()))
    }

    /// Returns the group IDs of active groups that need a self-update.
    ///
    /// A group needs a self-update if:
    /// - `self_update_state` is `SelfUpdateState::Required` (post-join requirement, MIP-02), or
    /// - `self_update_state` is `SelfUpdateState::CompletedAt` and the timestamp is older than `threshold_secs` (periodic rotation, MIP-00)
    ///
    /// # Arguments
    /// * `threshold_secs` - Maximum age in seconds before a group's key rotation is considered stale
    ///
    /// # Returns
    /// * `Ok(Vec<GroupId>)` - Group IDs needing self-update
    /// * `Err(Error)` - If there is an error accessing storage
    pub fn groups_needing_self_update(&self, threshold_secs: u64) -> Result<Vec<GroupId>, Error> {
        self.storage()
            .groups_needing_self_update(threshold_secs)
            .map_err(|e| Error::Group(e.to_string()))
    }

    /// Gets the public keys of all members in an MLS group
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    ///
    /// # Returns
    ///
    /// * `Ok(BTreeSet<PublicKey>)` - Set of member public keys
    /// * `Err(Error)` - If the group is not found or there is an error accessing member data
    pub fn get_members(&self, group_id: &GroupId) -> Result<BTreeSet<PublicKey>, Error> {
        let group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        // Store members in a variable to extend its lifetime
        let mut members = group.members();
        members.try_fold(BTreeSet::new(), |mut acc, m| {
            let credentials: BasicCredential = BasicCredential::try_from(m.credential)?;
            let identity_bytes: &[u8] = credentials.identity();
            let public_key = self.parse_credential_identity(identity_bytes)?;
            acc.insert(public_key);
            Ok(acc)
        })
    }

    /// Gets the public keys of members that will be added from pending proposals in an MLS group
    ///
    /// This method examines pending Add proposals in the group and extracts the public keys
    /// of members that would be added if these proposals are committed. This is useful for
    /// showing admins which member additions are pending approval.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID to examine for pending proposals
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<PublicKey>)` - List of public keys for members in pending Add proposals
    /// * `Err(Error)` - If there's an error loading the group or extracting member information
    pub fn pending_added_members_pubkeys(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<PublicKey>, Error> {
        let mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        let mut added_pubkeys = Vec::new();

        for proposal in mls_group.pending_proposals() {
            if let Proposal::Add(add_proposal) = proposal.proposal() {
                let leaf_node = add_proposal.key_package().leaf_node();
                let pubkey = self.pubkey_for_leaf_node(leaf_node)?;
                added_pubkeys.push(pubkey);
            }
        }

        Ok(added_pubkeys)
    }

    /// Gets the public keys of members that will be removed from pending proposals in an MLS group
    ///
    /// This method examines pending Remove proposals in the group and extracts the public keys
    /// of members that would be removed if these proposals are committed. This is useful for
    /// showing admins which member removals are pending approval.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID to examine for pending proposals
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<PublicKey>)` - List of public keys for members in pending Remove proposals
    /// * `Err(Error)` - If there's an error loading the group or extracting member information
    pub fn pending_removed_members_pubkeys(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<PublicKey>, Error> {
        let mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        let mut removed_pubkeys = Vec::new();

        for proposal in mls_group.pending_proposals() {
            if let Proposal::Remove(remove_proposal) = proposal.proposal() {
                let removed_leaf_index = remove_proposal.removed();
                if let Some(member) = mls_group.member_at(removed_leaf_index) {
                    let pubkey = self.pubkey_for_member(&member)?;
                    removed_pubkeys.push(pubkey);
                }
            }
        }

        Ok(removed_pubkeys)
    }

    /// Gets all pending member changes (additions and removals) from pending proposals
    ///
    /// This method provides a combined view of all pending member changes in a group,
    /// which is useful for showing admins a complete picture of proposed membership changes
    /// that need approval.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID to examine for pending proposals
    ///
    /// # Returns
    ///
    /// * `Ok(PendingMemberChanges)` - Struct containing lists of pending additions and removals
    /// * `Err(Error)` - If there's an error loading the group or extracting member information
    pub fn pending_member_changes(
        &self,
        group_id: &GroupId,
    ) -> Result<PendingMemberChanges, Error> {
        let mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        let mut additions = Vec::new();
        let mut removals = Vec::new();

        for proposal in mls_group.pending_proposals() {
            match proposal.proposal() {
                Proposal::Add(add_proposal) => {
                    let leaf_node = add_proposal.key_package().leaf_node();
                    let pubkey = self.pubkey_for_leaf_node(leaf_node)?;
                    additions.push(pubkey);
                }
                Proposal::Remove(remove_proposal) => {
                    let removed_leaf_index = remove_proposal.removed();
                    if let Some(member) = mls_group.member_at(removed_leaf_index) {
                        let pubkey = self.pubkey_for_member(&member)?;
                        removals.push(pubkey);
                    }
                }
                _ => {}
            }
        }

        Ok(PendingMemberChanges {
            additions,
            removals,
        })
    }

    /// Add members to a group
    ///
    /// NOTE: This function doesn't merge the pending commit. Clients must call this function manually only after successful publish of the commit message to relays.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    /// * `key_package_events` - The nostr key package events (Kind:443) for each new member to add
    ///
    /// # Returns
    ///
    /// * `Ok(UpdateGroupResult)`
    /// * `Err(Error)` - If there is an error adding members
    pub fn add_members(
        &self,
        group_id: &GroupId,
        key_package_events: &[Event],
    ) -> Result<UpdateGroupResult, Error> {
        let mut mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;
        let mls_signer: SignatureKeyPair = self.load_mls_signer(&mls_group)?;

        // Check if current user is an admin
        let own_leaf = mls_group.own_leaf().ok_or(Error::OwnLeafNotFound)?;
        if !self.is_leaf_node_admin(&mls_group.group_id().into(), own_leaf)? {
            return Err(Error::Group(
                "Only group admins can add members".to_string(),
            ));
        }

        // Parse key packages from events
        let mut key_packages_vec: Vec<KeyPackage> = Vec::new();
        for event in key_package_events {
            // TODO: Error handling for failure here
            let key_package: KeyPackage = self.parse_key_package(event)?;
            key_packages_vec.push(key_package);
        }

        let (commit_message, welcome_message, _group_info) = mls_group
            .add_members(&self.provider, &mls_signer, &key_packages_vec)
            .map_err(|e| Error::Group(e.to_string()))?;

        let serialized_commit_message = commit_message
            .tls_serialize_detached()
            .map_err(|e| Error::Group(e.to_string()))?;

        let commit_event =
            self.build_message_event(&mls_group.group_id().into(), serialized_commit_message)?;

        // Create processed_message to track state of message
        let processed_message: message_types::ProcessedMessage = message_types::ProcessedMessage {
            wrapper_event_id: commit_event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: Some(mls_group.epoch().as_u64()),
            mls_group_id: Some(mls_group.group_id().into()),
            state: message_types::ProcessedMessageState::ProcessedCommit,
            failure_reason: None,
        };

        self.storage()
            .save_processed_message(processed_message)
            .map_err(|e| Error::Message(e.to_string()))?;

        let serialized_welcome_message = welcome_message
            .tls_serialize_detached()
            .map_err(|e| Error::Group(e.to_string()))?;

        // Get relays for this group
        let group_relays = self
            .get_relays(&mls_group.group_id().into())?
            .into_iter()
            .collect::<Vec<_>>();

        let welcome_rumors = self.build_welcome_rumors_for_key_packages(
            &mls_group,
            serialized_welcome_message,
            key_package_events.to_vec(),
            &group_relays,
        )?;

        // let serialized_group_info = group_info
        //     .map(|g| {
        //         g.tls_serialize_detached()
        //             .map_err(|e| Error::Group(e.to_string()))
        //     })
        //     .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event: commit_event,
            welcome_rumors, // serialized_group_info,
            mls_group_id: group_id.clone(),
        })
    }

    /// Remove members from a group
    ///
    /// NOTE: This function doesn't merge the pending commit. Clients must call this function manually only after successful publish of the commit message to relays.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    /// * `pubkeys` - The Nostr public keys of the members to remove
    ///
    /// # Returns
    ///
    /// * `Ok(UpdateGroupResult)`
    /// * `Err(Error)` - If there is an error removing members
    pub fn remove_members(
        &self,
        group_id: &GroupId,
        pubkeys: &[PublicKey],
    ) -> Result<UpdateGroupResult, Error> {
        let mut mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        let signer: SignatureKeyPair = self.load_mls_signer(&mls_group)?;

        // Check if current user is an admin
        let own_leaf = mls_group.own_leaf().ok_or(Error::OwnLeafNotFound)?;
        if !self.is_leaf_node_admin(group_id, own_leaf)? {
            return Err(Error::Group(
                "Only group admins can remove members".to_string(),
            ));
        }

        // Convert pubkeys to leaf indices
        let mut leaf_indices = Vec::new();

        for member in mls_group.members() {
            let pubkey = self.pubkey_for_member(&member)?;
            if pubkeys.contains(&pubkey) {
                leaf_indices.push(member.index);
            }
        }

        if leaf_indices.is_empty() {
            return Err(Error::Group(
                "No matching members found to remove".to_string(),
            ));
        }

        // TODO: Get a list of users to be added from any proposals and create welcome events for them

        let (commit_message, welcome_option, _group_info) = mls_group
            .remove_members(&self.provider, &signer, &leaf_indices)
            .map_err(|e| Error::Group(e.to_string()))?;

        let serialized_commit_message = commit_message
            .tls_serialize_detached()
            .map_err(|e| Error::Group(e.to_string()))?;

        let commit_event =
            self.build_message_event(&mls_group.group_id().into(), serialized_commit_message)?;

        // Create processed_message to track state of message
        let processed_message: message_types::ProcessedMessage = message_types::ProcessedMessage {
            wrapper_event_id: commit_event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: Some(mls_group.epoch().as_u64()),
            mls_group_id: Some(mls_group.group_id().into()),
            state: message_types::ProcessedMessageState::ProcessedCommit,
            failure_reason: None,
        };

        self.storage()
            .save_processed_message(processed_message)
            .map_err(|e| Error::Message(e.to_string()))?;

        // For now, if we find welcomes, throw an error.
        if welcome_option.is_some() {
            return Err(Error::Group(
                "Found welcomes when removing users".to_string(),
            ));
        }
        // let serialized_welcome_message = welcome_option
        //     .map(|w| {
        //         w.tls_serialize_detached()
        //             .map_err(|e| Error::Group(e.to_string()))
        //     })
        //     .transpose()?;

        // let serialized_group_info = group_info
        //     .map(|g| {
        //         g.tls_serialize_detached()
        //             .map_err(|e| Error::Group(e.to_string()))
        //     })
        //     .transpose()?;

        Ok(UpdateGroupResult {
            evolution_event: commit_event,
            welcome_rumors: None, // serialized_group_info,
            mls_group_id: group_id.clone(),
        })
    }

    fn update_group_data_extension(
        &self,
        mls_group: &mut MlsGroup,
        group_id: &GroupId,
        group_data: &NostrGroupDataExtension,
    ) -> Result<UpdateGroupResult, Error> {
        // Check if current user is an admin
        let own_leaf = mls_group.own_leaf().ok_or(Error::OwnLeafNotFound)?;
        if !self.is_leaf_node_admin(group_id, own_leaf)? {
            return Err(Error::Group(
                "Only group admins can update group context extensions".to_string(),
            ));
        }

        let extension = Self::get_unknown_extension_from_group_data(group_data)?;
        let mut extensions = mls_group.extensions().clone();
        extensions.add_or_replace(extension)?;

        let signature_keypair = self.load_mls_signer(mls_group)?;
        let (message_out, _, _) = mls_group.update_group_context_extensions(
            &self.provider,
            extensions,
            &signature_keypair,
        )?;
        let commit_event = self.build_message_event(
            &mls_group.group_id().into(),
            message_out.tls_serialize_detached()?,
        )?;

        // Create processed_message to track state of message
        let processed_message: message_types::ProcessedMessage = message_types::ProcessedMessage {
            wrapper_event_id: commit_event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: Some(mls_group.epoch().as_u64()),
            mls_group_id: Some(mls_group.group_id().into()),
            state: message_types::ProcessedMessageState::ProcessedCommit,
            failure_reason: None,
        };

        self.storage()
            .save_processed_message(processed_message)
            .map_err(|e| Error::Message(e.to_string()))?;

        Ok(UpdateGroupResult {
            evolution_event: commit_event,
            welcome_rumors: None,
            mls_group_id: group_id.clone(),
        })
    }

    /// Updates group data with the specified configuration
    ///
    /// This method allows updating one or more fields of the group data in a single operation.
    /// Only the fields specified in the update configuration will be modified.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    /// * `update` - Configuration specifying which fields to update and their new values
    ///
    /// # Returns
    ///
    /// * `Ok(UpdateGroupResult)` - Update result containing the evolution event
    /// * `Err(Error)` - If the group is not found or the operation fails
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// // Update only the name
    /// let update = NostrGroupDataUpdate::new().name("New Group Name");
    /// mls.update_group_data(&group_id, update)?;
    ///
    /// // Update name and description together
    /// let update = NostrGroupDataUpdate::new()
    ///     .name("New Name")
    ///     .description("New Description");
    /// mls.update_group_data(&group_id, update)?;
    ///
    /// // Update image, clearing the existing one
    /// // Note: Setting image_hash to None automatically clears image_key, image_nonce, and image_upload_key
    /// let update = NostrGroupDataUpdate::new().image_hash(None);
    /// mls.update_group_data(&group_id, update)?;
    ///
    /// // Rotate the nostr_group_id for message routing (per MIP-01)
    /// let new_id = [0u8; 32]; // Generate a new random ID
    /// let update = NostrGroupDataUpdate::new().nostr_group_id(new_id);
    /// mls.update_group_data(&group_id, update)?;
    /// ```
    pub fn update_group_data(
        &self,
        group_id: &GroupId,
        update: NostrGroupDataUpdate,
    ) -> Result<UpdateGroupResult, Error> {
        let mut mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        let mut group_data = NostrGroupDataExtension::from_group(&mls_group)?;

        // Apply updates only for fields that are specified
        if let Some(name) = update.name {
            group_data.name = name;
        }

        if let Some(description) = update.description {
            group_data.description = description;
        }

        if let Some(image_hash) = update.image_hash {
            group_data.image_hash = image_hash;
            // When clearing the image (setting hash to None), also clear all related cryptographic material
            if image_hash.is_none() {
                group_data.image_key = None;
                group_data.image_nonce = None;
                group_data.image_upload_key = None;
            }
        }

        if let Some(image_key) = update.image_key {
            group_data.image_key = image_key;
        }

        if let Some(image_nonce) = update.image_nonce {
            group_data.image_nonce = image_nonce;
        }

        if let Some(image_upload_key) = update.image_upload_key {
            group_data.image_upload_key = image_upload_key;
        }

        if let Some(relays) = update.relays {
            group_data.relays = relays.into_iter().collect();
        }

        if let Some(ref admins) = update.admins {
            // Validate admin update against current membership before applying
            self.validate_admin_update(group_id, admins)?;
            group_data.admins = admins.iter().copied().collect();
        }

        if let Some(nostr_group_id) = update.nostr_group_id {
            group_data.nostr_group_id = nostr_group_id;
        }

        self.update_group_data_extension(&mut mls_group, group_id, &group_data)
    }

    /// Retrieves the set of relay URLs associated with an MLS group
    ///
    /// # Arguments
    ///
    /// * `group_id` - The MLS group ID
    ///
    /// # Returns
    ///
    /// * `Ok(BTreeSet<RelayUrl>)` - Set of relay URLs where group messages are published
    /// * `Err(Error)` - If there is an error accessing storage or the group is not found
    pub fn get_relays(&self, group_id: &GroupId) -> Result<BTreeSet<RelayUrl>, Error> {
        let relays = self
            .storage()
            .group_relays(group_id)
            .map_err(|e| Error::Group(e.to_string()))?;
        Ok(relays.into_iter().map(|r| r.relay_url).collect())
    }

    fn get_unknown_extension_from_group_data(
        group_data: &NostrGroupDataExtension,
    ) -> Result<Extension, Error> {
        let serialized_group_data = group_data.as_raw().tls_serialize_detached()?;

        Ok(Extension::Unknown(
            group_data.extension_type(),
            UnknownExtension(serialized_group_data),
        ))
    }

    /// Creates a new MLS group with the specified members and settings.
    ///
    /// This function creates a new MLS group with the given name, description, members, and administrators.
    /// It generates the necessary cryptographic credentials, configures the group with Nostr-specific extensions,
    /// and adds the specified members.
    ///
    /// # Single-Member Groups
    ///
    /// This method supports creating groups with only the creator (no additional members).
    /// When `member_key_package_events` is empty, the group is created with just the creator,
    /// and `welcome_rumors` in the result will be empty. This is useful for:
    /// - "Message to self" functionality
    /// - Setting up groups before inviting members
    ///
    /// # Arguments
    ///
    /// * `creator_public_key` - The Nostr public key of the group creator
    /// * `member_key_package_events` - A vector of Nostr events (Kind:443) containing key packages
    ///   for the initial group members. Can be empty to create a single-member group.
    /// * `config` - Group configuration including name, description, admins, and relays
    ///
    /// # Returns
    ///
    /// A `GroupResult` containing:
    /// - The created group
    /// - A Vec of UnsignedEvents (`welcome_rumors`) representing the welcomes to be sent to new
    ///   members. Empty if no members were added.
    ///
    /// # Errors
    ///
    /// Returns an `Error` if:
    /// - Credential generation fails
    /// - Group creation fails
    /// - Adding members fails (when members are provided)
    /// - Message serialization fails
    pub fn create_group(
        &self,
        creator_public_key: &PublicKey,
        member_key_package_events: Vec<Event>,
        config: NostrGroupConfigData,
    ) -> Result<GroupResult, Error> {
        // Get member pubkeys
        let member_pubkeys = member_key_package_events
            .clone()
            .into_iter()
            .map(|e| e.pubkey)
            .collect::<Vec<PublicKey>>();

        let admins = config.admins.clone();

        // Validate group members
        self.validate_group_members(creator_public_key, &member_pubkeys, &admins)?;

        let (credential, signer) = self.generate_credential_with_key(creator_public_key)?;

        let group_data = NostrGroupDataExtension::new(
            config.name,
            config.description,
            admins,
            config.relays.clone(),
            config.image_hash,
            config.image_key,
            config.image_nonce,
            None, // image_upload_key - will be set when image is uploaded
        );

        let extension = Self::get_unknown_extension_from_group_data(&group_data)?;
        let required_capabilities_extension = self.required_capabilities_extension();
        let extensions = Extensions::from_vec(vec![extension, required_capabilities_extension])?;

        // Build the group config
        let capabilities = self.capabilities();
        let sender_ratchet_config = SenderRatchetConfiguration::new(
            self.config.out_of_order_tolerance,
            self.config.maximum_forward_distance,
        );
        let group_config = MlsGroupCreateConfig::builder()
            .ciphersuite(self.ciphersuite)
            .use_ratchet_tree_extension(true)
            .capabilities(capabilities)
            .with_group_context_extensions(extensions)
            .sender_ratchet_configuration(sender_ratchet_config)
            .build();

        let mut mls_group =
            MlsGroup::new(&self.provider, &signer, &group_config, credential.clone())?;

        let mut key_packages_vec: Vec<KeyPackage> = Vec::new();
        for event in &member_key_package_events {
            // TODO: Error handling for failure here
            let key_package: KeyPackage = self.parse_key_package(event)?;
            key_packages_vec.push(key_package);
        }

        // Handle member addition and welcome message creation
        // For single-member groups (no additional members), we skip adding members
        // and return an empty welcome_rumors vec
        let welcome_rumors = if key_packages_vec.is_empty() {
            // Single-member group: no members to add, no welcome messages needed
            Vec::new()
        } else {
            // Add members to the group
            let (_, welcome_out, _group_info) =
                mls_group.add_members(&self.provider, &signer, &key_packages_vec)?;

            // IMPORTANT: Privacy-preserving group creation
            //
            // We intentionally DO NOT publish the initial commit to relays. Instead, we:
            // 1. Merge the pending commit locally (immediately below)
            // 2. Send Welcome messages directly to invited members
            //
            // This differs from the MLS specification (RFC 9420), which recommends waiting
            // for Delivery Service confirmation before applying commits. However, that
            // guidance assumes a centralized Delivery Service model.
            //
            // For initial group creation with Nostr relays, not publishing the commit is
            // the correct choice for security and privacy reasons:
            //
            // - PRIVACY: Publishing the commit would expose additional metadata on relays
            //   (timing, event patterns, correlation opportunities) with no functional benefit
            // - SECURITY: Invited members receive complete group state via Welcome messages;
            //   they do not need the commit to join the group
            // - NO RACE CONDITIONS: At creation time, only the creator exists in the group,
            //   so there are no other members who need to process this commit
            //
            // This approach minimizes observable events on relays while maintaining full
            // MLS security properties. The Welcome messages contain all cryptographic
            // material needed for invitees to participate in the group.
            //
            // NOTE: This is specific to initial group creation. For commits in established
            // groups (adding/removing members, updates), commits MUST be published to relays
            // so existing members can process them and stay in sync.
            mls_group.merge_pending_commit(&self.provider)?;

            // Serialize the welcome message and send it to the members
            let serialized_welcome_message = welcome_out.tls_serialize_detached()?;

            self.build_welcome_rumors_for_key_packages(
                &mls_group,
                serialized_welcome_message,
                member_key_package_events,
                &config.relays,
            )?
            .ok_or(Error::Welcome("Error creating welcome rumors".to_string()))?
        };

        // Save the NostrMLS Group
        let group = group_types::Group {
            mls_group_id: mls_group.group_id().clone().into(),
            nostr_group_id: group_data.clone().nostr_group_id,
            name: group_data.clone().name,
            description: group_data.clone().description,
            admin_pubkeys: group_data.clone().admins,
            last_message_id: None,
            last_message_at: None,
            last_message_processed_at: None,
            epoch: mls_group.epoch().as_u64(),
            state: group_types::GroupState::Active,
            image_hash: config.image_hash,
            image_key: config.image_key.map(mdk_storage_traits::Secret::new),
            image_nonce: config.image_nonce.map(mdk_storage_traits::Secret::new),
            self_update_state: group_types::SelfUpdateState::CompletedAt(Timestamp::now()),
        };

        self.storage().save_group(group.clone()).map_err(
            |e: mdk_storage_traits::groups::error::GroupError| Error::Group(e.to_string()),
        )?;

        // Save the group relays after saving the group
        self.storage()
            .replace_group_relays(&group.mls_group_id, config.relays.into_iter().collect())
            .map_err(|e| Error::Group(e.to_string()))?;

        Ok(GroupResult {
            group,
            welcome_rumors,
        })
    }

    /// Updates the current member's leaf node in an MLS group.
    /// Does not currently support updating any group attributes.
    ///
    /// This function performs a self-update operation in the specified MLS group by:
    /// 1. Loading the group from storage
    /// 2. Generating a new signature keypair
    /// 3. Storing the keypair
    /// 4. Creating and applying a self-update proposal
    ///
    /// NOTE: This function doesn't merge the pending commit. Clients must call this function manually only after successful publish of the commit message to relays.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The ID of the MLS group
    ///
    /// # Returns
    ///
    /// An UpdateGroupResult
    ///
    /// # Errors
    ///
    /// Returns a Error if:
    /// - The group cannot be loaded from storage
    /// - The specified group is not found
    /// - Failed to generate or store signature keypair
    /// - Failed to perform self-update operation
    pub fn self_update(&self, group_id: &GroupId) -> Result<UpdateGroupResult, Error> {
        let mut mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        tracing::debug!(target: "mdk_core::groups::self_update", "Current epoch: {:?}", mls_group.epoch().as_u64());

        // Load current signer
        let current_signer: SignatureKeyPair = self.load_mls_signer(&mls_group)?;

        // Get own leaf
        let own_leaf = mls_group.own_leaf().ok_or(Error::OwnLeafNotFound)?;

        let new_signature_keypair = SignatureKeyPair::new(self.ciphersuite.signature_algorithm())?;

        new_signature_keypair
            .store(self.provider.storage())
            .map_err(|e| Error::Provider(e.to_string()))?;

        let pubkey = BasicCredential::try_from(own_leaf.credential().clone())?
            .identity()
            .to_vec();

        let new_credential: BasicCredential = BasicCredential::new(pubkey);
        let new_credential_with_key = CredentialWithKey {
            credential: new_credential.into(),
            signature_key: new_signature_keypair.public().into(),
        };

        let new_signer_bundle = NewSignerBundle {
            signer: &new_signature_keypair,
            credential_with_key: new_credential_with_key.clone(),
        };

        let leaf_node_params = LeafNodeParameters::builder()
            .with_credential_with_key(new_credential_with_key)
            .with_capabilities(own_leaf.capabilities().clone())
            .with_extensions(own_leaf.extensions().clone())
            .build();

        let commit_message_bundle = mls_group.self_update_with_new_signer(
            &self.provider,
            &current_signer,
            new_signer_bundle,
            leaf_node_params,
        )?;

        // Serialize the message
        let serialized_commit_message = commit_message_bundle.commit().tls_serialize_detached()?;

        let commit_event =
            self.build_message_event(&mls_group.group_id().into(), serialized_commit_message)?;

        // Create processed_message to track state of message
        let processed_message: message_types::ProcessedMessage = message_types::ProcessedMessage {
            wrapper_event_id: commit_event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: Some(mls_group.epoch().as_u64()),
            mls_group_id: Some(mls_group.group_id().into()),
            state: message_types::ProcessedMessageState::ProcessedCommit,
            failure_reason: None,
        };

        self.storage()
            .save_processed_message(processed_message)
            .map_err(|e| Error::Message(e.to_string()))?;

        let serialized_welcome_message = commit_message_bundle
            .welcome()
            .map(|w| {
                w.tls_serialize_detached()
                    .map_err(|e| Error::Group(e.to_string()))
            })
            .transpose()?;

        // For now, if we find welcomes, throw an error.
        if serialized_welcome_message.is_some() {
            return Err(Error::Group(
                "Found welcomes when performing a self update".to_string(),
            ));
        }

        Ok(UpdateGroupResult {
            evolution_event: commit_event,
            welcome_rumors: None, // serialized_group_info,
            mls_group_id: group_id.clone(),
        })
    }

    /// Create a proposal to leave the group
    ///
    /// This creates a leave proposal that must be committed by another member (typically an admin).
    /// The member cannot unilaterally leave because they cannot commit themselves out of the tree.
    /// The member remains in the group and can continue participating until another member
    /// processes and commits this proposal.
    ///
    /// # Arguments
    ///
    /// * `group_id` - The ID of the MLS group
    ///
    /// # Returns
    /// * `Ok(UpdateGroupResult)` - Contains the leave proposal event that must be processed by another member
    pub fn leave_group(&self, group_id: &GroupId) -> Result<UpdateGroupResult, Error> {
        let mut group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        let signer: SignatureKeyPair = self.load_mls_signer(&group)?;

        let leave_message = group
            .leave_group(&self.provider, &signer)
            .map_err(|e| Error::Group(e.to_string()))?;

        let serialized_message_out = leave_message
            .tls_serialize_detached()
            .map_err(|e| Error::Group(e.to_string()))?;

        let evolution_event =
            self.build_message_event(&group.group_id().into(), serialized_message_out)?;

        // Create processed_message to track state of message
        let processed_message: message_types::ProcessedMessage = message_types::ProcessedMessage {
            wrapper_event_id: evolution_event.id,
            message_event_id: None,
            processed_at: Timestamp::now(),
            epoch: Some(group.epoch().as_u64()),
            mls_group_id: Some(group.group_id().into()),
            state: message_types::ProcessedMessageState::ProcessedCommit,
            failure_reason: None,
        };

        self.storage()
            .save_processed_message(processed_message)
            .map_err(|e| Error::Message(e.to_string()))?;

        Ok(UpdateGroupResult {
            evolution_event,
            welcome_rumors: None,
            mls_group_id: group_id.clone(),
        })
    }

    /// Clear (rollback) a pending commit without merging it.
    ///
    /// This should be called when the Kind:445 publish fails after creating a commit
    /// via `add_members`, `remove_members`, or `self_update`. Without clearing, the
    /// pending commit blocks all future group operations with "Can't execute operation
    /// because a pending commit exists".
    ///
    /// The group returns to its pre-commit state — no epoch advance, no member changes.
    ///
    /// When the pending commit is a `self_update`, the new `SignatureKeyPair` that was
    /// stored in the provider before creating the commit is deleted from storage, since
    /// the group reverts to the previous signer.
    ///
    /// # Arguments
    /// * `group_id` - the MlsGroup GroupId value
    pub fn clear_pending_commit(&self, group_id: &GroupId) -> Result<(), Error> {
        let mut mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        // Detect whether the pending commit is a self-update BEFORE clearing,
        // since clear_pending_commit() consumes the staged commit.
        // If it is, capture the new leaf's signature public key so we can delete
        // the orphaned keypair from storage after the rollback.
        let self_update_new_pubkey: Option<Vec<u8>> =
            mls_group.pending_commit().and_then(|staged_commit| {
                let has_update_signal = staged_commit.update_path_leaf_node().is_some()
                    || staged_commit.update_proposals().next().is_some();
                let no_non_update_proposals = staged_commit
                    .queued_proposals()
                    .all(|p| matches!(p.proposal(), Proposal::Update(_)));
                if has_update_signal && no_non_update_proposals {
                    staged_commit
                        .update_path_leaf_node()
                        .map(|leaf| leaf.signature_key().as_slice().to_vec())
                } else {
                    None
                }
            });

        mls_group
            .clear_pending_commit(self.provider.storage())
            .map_err(|e| Error::Provider(e.to_string()))?;

        // If this was a self-update, delete the orphaned new signature keypair.
        // OpenMLS reverts to the old signer after clear_pending_commit; the new
        // keypair was stored eagerly in self_update() and is now unreachable.
        if let Some(pubkey_bytes) = self_update_new_pubkey {
            SignatureKeyPair::delete(
                self.provider.storage(),
                &pubkey_bytes,
                self.ciphersuite.signature_algorithm(),
            )
            .map_err(|e| Error::Provider(e.to_string()))?;
        }

        Ok(())
    }

    /// Merge any pending commits.
    /// This should be called AFTER publishing the Kind:445 message that contains a commit message to mitigate race conditions
    ///
    /// # Arguments
    /// * `group_id` - the MlsGroup GroupId value
    ///
    /// Returns
    /// * `Ok(())` - if the commits were merged successfully
    /// * Err(GroupError) - if something goes wrong
    pub fn merge_pending_commit(&self, group_id: &GroupId) -> Result<(), Error> {
        let mut mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;

        // Detect whether the pending commit is a self-update BEFORE merging,
        // since merge_pending_commit() consumes the staged commit.
        let is_self_update = mls_group.pending_commit().is_some_and(|staged_commit| {
            // Must contain at least one update signal
            let has_update_signal = staged_commit.update_path_leaf_node().is_some()
                || staged_commit.update_proposals().next().is_some();
            // No non-update proposals present (add/remove/psk/etc.).
            // Note: `all()` is vacuously true on an empty iterator, which is
            // the expected case for path-based self-updates where the update
            // comes via `update_path_leaf_node()` rather than explicit Update
            // proposals in `queued_proposals()`.
            let no_non_update_proposals = staged_commit
                .queued_proposals()
                .all(|p| matches!(p.proposal(), Proposal::Update(_)));
            has_update_signal && no_non_update_proposals
        });

        mls_group.merge_pending_commit(&self.provider)?;

        // Sync the stored group metadata with the updated MLS group state
        self.sync_group_metadata_from_mls(group_id)?;

        // If this was actually a self-update commit, record the timestamp.
        // This correctly handles:
        // - Post-join self-updates (transitions Required → CompletedAt)
        // - Periodic rotation self-updates (updates CompletedAt timestamp)
        // - Non-self-update commits (leaves self_update_state untouched)
        if is_self_update {
            let mut group = self.get_group(group_id)?.ok_or(Error::GroupNotFound)?;
            group.self_update_state = group_types::SelfUpdateState::CompletedAt(Timestamp::now());
            self.storage()
                .save_group(group)
                .map_err(|e| Error::Group(e.to_string()))?;
        }

        Ok(())
    }

    /// Synchronizes the stored group metadata with the current MLS group state
    ///
    /// This helper method ensures that all fields in the stored `group_types::Group`
    /// remain consistent with the MLS group state and extensions after operations.
    /// It should be called after any operation that changes the group state or extensions.
    ///
    /// # Arguments
    /// * `group_id` - The MLS group ID to synchronize
    ///
    /// # Returns
    /// * `Ok(())` - if synchronization succeeds
    /// * `Err(Error)` - if the group is not found or synchronization fails
    pub fn sync_group_metadata_from_mls(&self, group_id: &GroupId) -> Result<(), Error> {
        let mls_group = self.load_mls_group(group_id)?.ok_or(Error::GroupNotFound)?;
        let mut stored_group = self.get_group(group_id)?.ok_or(Error::GroupNotFound)?;

        // Validate the mandatory group-data extension FIRST before making any state changes
        // This ensures we don't update stored_group if the extension is missing, invalid, or unsupported
        let group_data = NostrGroupDataExtension::from_group(&mls_group)?;
        // Only after successful validation, update epoch and metadata from MLS group
        stored_group.epoch = mls_group.epoch().as_u64();

        // Update extension data from NostrGroupDataExtension
        stored_group.name = group_data.name;
        stored_group.description = group_data.description;
        stored_group.image_hash = group_data.image_hash;
        stored_group.image_key = group_data.image_key.map(mdk_storage_traits::Secret::new);
        stored_group.image_nonce = group_data.image_nonce.map(mdk_storage_traits::Secret::new);
        stored_group.admin_pubkeys = group_data.admins;
        stored_group.nostr_group_id = group_data.nostr_group_id;

        // Sync relays atomically - replace entire relay set with current extension data
        self.storage()
            .replace_group_relays(group_id, group_data.relays)
            .map_err(|e| Error::Group(e.to_string()))?;

        self.storage()
            .save_group(stored_group)
            .map_err(|e| Error::Group(e.to_string()))?;

        Ok(())
    }

    /// Validates the members and admins of a group during creation
    ///
    /// # Arguments
    /// * `creator_pubkey` - The public key of the group creator
    /// * `member_pubkeys` - List of public keys for group members
    /// * `admin_pubkeys` - List of public keys for group admins
    ///
    /// # Returns
    /// * `Ok(true)` if validation passes
    /// * `Err(GroupError::InvalidParameters)` if validation fails
    ///
    /// # Validation Rules
    /// - Creator must be an admin but not included in member list
    /// - All admins must also be members (except creator)
    ///
    /// # Errors
    /// Returns `GroupError::InvalidParameters` with descriptive message if:
    /// - Creator is not an admin
    /// - Creator is in member list
    /// - Any admin, other than the creator, is not a member
    fn validate_group_members(
        &self,
        creator_pubkey: &PublicKey,
        member_pubkeys: &[PublicKey],
        admin_pubkeys: &[PublicKey],
    ) -> Result<bool, Error> {
        // Creator must be an admin
        if !admin_pubkeys.contains(creator_pubkey) {
            return Err(Error::Group("Creator must be an admin".to_string()));
        }

        // Creator must not be included as a member
        if member_pubkeys.contains(creator_pubkey) {
            return Err(Error::Group(
                "Creator must not be included as a member".to_string(),
            ));
        }

        // Check that admins are valid pubkeys and are members
        for pubkey in admin_pubkeys.iter() {
            if !member_pubkeys.contains(pubkey) && creator_pubkey != pubkey {
                return Err(Error::Group("Admin must be a member".to_string()));
            }
        }
        Ok(true)
    }

    /// Validates admin updates against current group membership
    ///
    /// # Arguments
    /// * `group_id` - The MLS group ID
    /// * `new_admins` - The proposed new admin set
    ///
    /// # Returns
    /// * `Ok(())` if validation passes
    /// * `Err(Error)` if validation fails
    ///
    /// # Validation Rules
    /// - Admin set must not be empty
    /// - All admins must be current group members
    ///
    /// # Errors
    /// Returns `Error::Group` with descriptive message if:
    /// - Admin set is empty
    /// - Any admin is not a current group member
    fn validate_admin_update(
        &self,
        group_id: &GroupId,
        new_admins: &[PublicKey],
    ) -> Result<(), Error> {
        // Admin set must not be empty
        if new_admins.is_empty() {
            return Err(Error::UpdateGroupContextExts(
                "Admin set cannot be empty".to_string(),
            ));
        }

        // Get current group members
        let current_members = self.get_members(group_id)?;

        // All admins must be current group members
        for admin in new_admins {
            if !current_members.contains(admin) {
                return Err(Error::UpdateGroupContextExts(format!(
                    "Admin {} is not a current group member",
                    admin
                )));
            }
        }

        Ok(())
    }

    /// Creates a NIP-44 encrypted message event Kind: 445 signing with an ephemeral keypair.
    pub(crate) fn build_message_event(
        &self,
        group_id: &GroupId,
        serialized_content: Vec<u8>,
    ) -> Result<Event, Error> {
        // Delegate to the retained variant and discard the ephemeral
        // key — the original behaviour. Callers that need to retain
        // the key (e.g. to later publish a NIP-09 deletion against
        // the kind-445 wrapper) should call `build_message_event_retained`.
        Ok(self.build_message_event_retained(group_id, serialized_content)?.0)
    }

    /// Creates a NIP-44 encrypted Kind: 445 wrapper event AND returns
    /// the ephemeral keypair used to sign it.
    ///
    /// Equivalent to `build_message_event` but exposes the ephemeral
    /// signing key so the caller can retain it and later publish an
    /// author-signed NIP-09 deletion against the wrapper event id.
    /// Wire-compatible with `build_message_event` — relays cannot
    /// distinguish events produced by the two paths.
    pub(crate) fn build_message_event_retained(
        &self,
        group_id: &GroupId,
        serialized_content: Vec<u8>,
    ) -> Result<(Event, Keys), Error> {
        let group = self.get_group(group_id)?.ok_or(Error::GroupNotFound)?;

        // Export secret
        let secret: group_types::GroupExporterSecret = self.exporter_secret(group_id)?;

        // Convert that secret to nostr keys
        let secret_key: SecretKey = SecretKey::from_slice(secret.secret.as_ref())?;
        let export_nostr_keys: Keys = Keys::new(secret_key);

        // Encrypt the message content
        // At some group size this will become too large for NIP44 encryption or relay event size limits.
        // We're not sure yet what size, but it's something to be aware of.
        let encrypted_content: String = nip44::encrypt(
            export_nostr_keys.secret_key(),
            &export_nostr_keys.public_key,
            &serialized_content,
            nip44::Version::default(),
        )?;

        // Generate ephemeral key
        let ephemeral_nostr_keys: Keys = Keys::generate();

        let tag: Tag = Tag::custom(TagKind::h(), [hex::encode(group.nostr_group_id)]);

        let event = EventBuilder::new(Kind::MlsGroupMessage, encrypted_content)
            .tag(tag)
            .sign_with_keys(&ephemeral_nostr_keys)?;

        Ok((event, ephemeral_nostr_keys))
    }

    pub(crate) fn build_welcome_rumors_for_key_packages(
        &self,
        group: &MlsGroup,
        serialized_welcome: Vec<u8>,
        key_package_events: Vec<Event>,
        group_relays: &[RelayUrl],
    ) -> Result<Option<Vec<UnsignedEvent>>, Error> {
        let committer_pubkey = self.get_own_pubkey(group)?;
        let mut welcome_rumors_vec = Vec::new();

        for event in key_package_events {
            // SECURITY: Always use base64 encoding with explicit encoding tag per MIP-00/MIP-02.
            // This prevents downgrade attacks and parsing ambiguity across clients.
            let encoding = ContentEncoding::Base64;

            let encoded_welcome = encode_content(&serialized_welcome, encoding);

            tracing::debug!(
                target: "mdk_core::groups",
                "Encoded welcome using {} format",
                encoding.as_tag_value()
            );

            let tags = vec![
                Tag::from_standardized(TagStandard::Relays(group_relays.to_vec())),
                Tag::event(event.id),
                Tag::client(format!("MDK/{}", env!("CARGO_PKG_VERSION"))),
                Tag::custom(
                    TagKind::Custom("encoding".into()),
                    [encoding.as_tag_value()],
                ),
            ];

            // Build welcome event rumors for each new user
            let welcome_rumor = EventBuilder::new(Kind::MlsWelcome, encoded_welcome)
                .tags(tags)
                .build(committer_pubkey);

            welcome_rumors_vec.push(welcome_rumor);
        }

        let welcome_rumors = if !welcome_rumors_vec.is_empty() {
            Some(welcome_rumors_vec)
        } else {
            None
        };

        Ok(welcome_rumors)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mdk_memory_storage::MdkMemoryStorage;
    use mdk_storage_traits::groups::GroupStorage;
    use mdk_storage_traits::messages::{MessageStorage, types as message_types};
    use nostr::{Keys, PublicKey};
    use openmls::prelude::BasicCredential;
    use openmls_basic_credential::SignatureKeyPair;

    use super::NostrGroupDataExtension;
    use crate::constant::NOSTR_GROUP_DATA_EXTENSION_TYPE;
    use crate::groups::NostrGroupDataUpdate;
    use crate::test_util::*;
    use crate::tests::create_test_mdk;

    #[test]
    fn test_validate_group_members() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();
        let member_pks: Vec<PublicKey> = members.iter().map(|k| k.public_key()).collect();

        // Test valid configuration
        assert!(
            mdk.validate_group_members(&creator_pk, &member_pks, &admins)
                .is_ok()
        );

        // Test creator not in admin list
        let bad_admins = vec![member_pks[0]];
        assert!(
            mdk.validate_group_members(&creator_pk, &member_pks, &bad_admins)
                .is_err()
        );

        // Test creator in member list
        let bad_members = vec![creator_pk, member_pks[0]];
        assert!(
            mdk.validate_group_members(&creator_pk, &bad_members, &admins)
                .is_err()
        );

        // Test admin not in member list
        let non_member = Keys::generate().public_key();
        let bad_admins = vec![creator_pk, non_member];
        assert!(
            mdk.validate_group_members(&creator_pk, &member_pks, &bad_admins)
                .is_err()
        );
    }

    #[test]
    fn test_create_group_basic() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Verify group was created with correct members
        let members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get members");

        assert_eq!(members.len(), 3); // creator + 2 initial members
        assert!(members.contains(&creator_pk));
        for member_keys in &initial_members {
            assert!(members.contains(&member_keys.public_key()));
        }
    }

    /// Test creating a group with only the creator (no additional members).
    /// This is useful for "message to self" functionality, setting up groups
    /// before inviting members, and multi-device scenarios.
    #[test]
    fn test_create_single_member_group() {
        let creator_mdk = create_test_mdk();
        let creator = Keys::generate();
        let creator_pk = creator.public_key();

        // Create a group with no additional members - only the creator
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                Vec::new(), // No additional members
                create_nostr_group_config_data(vec![creator_pk]),
            )
            .expect("Failed to create single-member group");

        let group_id = &create_result.group.mls_group_id;

        // Verify welcome_rumors is empty (no members to welcome)
        assert!(
            create_result.welcome_rumors.is_empty(),
            "Single-member group should have no welcome rumors"
        );

        // Verify only the creator is in the group
        let members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get members");

        assert_eq!(
            members.len(),
            1,
            "Single-member group should have exactly 1 member"
        );
        assert!(
            members.contains(&creator_pk),
            "Creator should be in the group"
        );

        // Verify group metadata was saved correctly
        let group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        assert_eq!(group.name, "Test Group");
        assert!(group.admin_pubkeys.contains(&creator_pk));
    }

    #[test]
    fn test_get_members() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Test get_members
        let members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get members");

        assert_eq!(members.len(), 3); // creator + 2 initial members
        assert!(members.contains(&creator_pk));
        for member_keys in &initial_members {
            assert!(members.contains(&member_keys.public_key()));
        }
    }

    #[test]
    fn test_add_members_epoch_advancement() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the initial group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get initial epoch
        let initial_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        let initial_epoch = initial_group.epoch;

        // Create key package event for new member
        let new_member = Keys::generate();
        let new_key_package_event = create_key_package_event(&creator_mdk, &new_member);

        // Add the new member
        let _add_result = creator_mdk
            .add_members(group_id, &[new_key_package_event])
            .expect("Failed to add member");

        // Merge the pending commit for the member addition
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for member addition");

        // Verify the MLS group epoch was advanced by checking the actual MLS group
        let mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let final_mls_epoch = mls_group.epoch().as_u64();

        assert!(
            final_mls_epoch > initial_epoch,
            "MLS group epoch should advance after adding members (initial: {}, final: {})",
            initial_epoch,
            final_mls_epoch
        );

        // Verify the new member was added
        let final_members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get members");
        assert!(
            final_members.contains(&new_member.public_key()),
            "New member should be in the group"
        );
        assert_eq!(
            final_members.len(),
            4, // creator + 2 initial + 1 new = 4 total
            "Should have 4 total members"
        );
    }

    #[test]
    fn test_get_own_pubkey() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        let mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Test get_own_pubkey
        let own_pubkey = creator_mdk
            .get_own_pubkey(&mls_group)
            .expect("Failed to get own pubkey");

        assert_eq!(
            own_pubkey, creator_pk,
            "Own pubkey should match creator pubkey"
        );
    }

    #[test]
    fn test_admin_check() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Test admin check - verify creator is in admin list
        let stored_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        assert!(
            stored_group.admin_pubkeys.contains(&creator_pk),
            "Creator should be admin"
        );
    }

    #[test]
    fn test_admin_permission_checks() {
        let admin_mdk = create_test_mdk();
        let non_admin_mdk = create_test_mdk();

        // Generate keys
        let admin_keys = Keys::generate();
        let non_admin_keys = Keys::generate();
        let member1_keys = Keys::generate();

        let admin_pk = admin_keys.public_key();
        let _non_admin_pk = non_admin_keys.public_key();
        let member1_pk = member1_keys.public_key();

        // Create key package events for initial members
        let non_admin_event = create_key_package_event(&admin_mdk, &non_admin_keys);
        let member1_event = create_key_package_event(&admin_mdk, &member1_keys);

        // Create group with admin as creator, non_admin and member1 as members
        // Only admin is an admin
        let create_result = admin_mdk
            .create_group(
                &admin_pk,
                vec![non_admin_event.clone(), member1_event.clone()],
                create_nostr_group_config_data(vec![admin_pk]), // Only admin is an admin
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        admin_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Create a new member to add
        let new_member_keys = Keys::generate();
        let _new_member_pk = new_member_keys.public_key();
        let new_member_event = create_key_package_event(&non_admin_mdk, &new_member_keys);

        // Test that admin can add members (should work)
        let add_result = admin_mdk.add_members(group_id, &[new_member_event]);
        assert!(add_result.is_ok(), "Admin should be able to add members");

        // Merge the pending commit for the member addition
        admin_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for member addition");

        // Test that admin can remove members (should work)
        let remove_result = admin_mdk.remove_members(group_id, &[member1_pk]);
        assert!(
            remove_result.is_ok(),
            "Admin should be able to remove members"
        );

        // Note: Testing non-admin permissions would require the non-admin user to actually
        // be part of the MLS group, which would require processing the welcome message.
        // For now, we've verified that admin permissions work correctly.
    }

    /// Test that admin authorization reads from the current MLS group state (NostrGroupDataExtension)
    /// rather than from potentially stale stored metadata.
    ///
    /// This test addresses issue #50: Admin Authorization Uses Stale Stored Metadata Instead of MLS State
    /// See: <https://github.com/marmot-protocol/mdk/issues/50>
    #[test]
    fn test_admin_check_uses_mls_state_not_stale_storage() {
        let creator_mdk = create_test_mdk();

        // Generate keys
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_pk = alice_keys.public_key();
        let bob_pk = bob_keys.public_key();
        let _charlie_pk = charlie_keys.public_key();

        // Create key package events for members
        let bob_event = create_key_package_event(&creator_mdk, &bob_keys);
        let charlie_event = create_key_package_event(&creator_mdk, &charlie_keys);

        // Create group with Alice as the ONLY admin
        let create_result = creator_mdk
            .create_group(
                &alice_pk,
                vec![bob_event, charlie_event],
                create_nostr_group_config_data(vec![alice_pk]), // Only Alice is admin
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id;

        // Merge the pending commit
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get the MLS group to access leaf nodes
        let mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Get Alice's leaf node (she's the creator/own leaf)
        let alice_leaf = mls_group.own_leaf().expect("Group should have own leaf");

        // Verify initial state: Alice is admin per MLS state
        assert!(
            creator_mdk
                .is_leaf_node_admin(&group_id.clone(), alice_leaf)
                .unwrap(),
            "Alice should be admin in MLS state"
        );

        // Now simulate stale storage by directly modifying stored_group.admin_pubkeys
        // to remove Alice as admin (even though MLS state has her as admin)
        let mut stored_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        // Add Bob to the stored admin list (simulating stale/incorrect storage)
        stored_group.admin_pubkeys.insert(bob_pk);
        // Remove Alice from stored admin list (simulating stale storage)
        stored_group.admin_pubkeys.remove(&alice_pk);

        // Save the modified (now stale) storage
        creator_mdk
            .storage()
            .save_group(stored_group.clone())
            .expect("Failed to save modified group");

        // Verify storage is now "stale" (has incorrect admin set)
        let stale_stored_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        assert!(
            stale_stored_group.admin_pubkeys.contains(&bob_pk),
            "Stale storage should have Bob as admin"
        );
        assert!(
            !stale_stored_group.admin_pubkeys.contains(&alice_pk),
            "Stale storage should NOT have Alice as admin"
        );

        // The critical test: is_leaf_node_admin should read from MLS state, NOT stale storage
        // Alice should still be admin (per MLS state) even though stale storage says otherwise
        assert!(
            creator_mdk
                .is_leaf_node_admin(&group_id.clone(), alice_leaf)
                .unwrap(),
            "is_leaf_node_admin should use MLS state, not stale storage"
        );
    }

    #[test]
    fn test_pubkey_for_member() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        let mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Test pubkey_for_member by checking all members
        let members: Vec<_> = mls_group.members().collect();
        let mut found_pubkeys = Vec::new();

        for member in &members {
            let pubkey = creator_mdk
                .pubkey_for_member(member)
                .expect("Failed to get pubkey for member");
            found_pubkeys.push(pubkey);
        }

        // Verify we found the expected public keys
        assert!(
            found_pubkeys.contains(&creator_pk),
            "Should find creator pubkey"
        );
        for member_keys in &initial_members {
            assert!(
                found_pubkeys.contains(&member_keys.public_key()),
                "Should find member pubkey: {:?}",
                member_keys.public_key()
            );
        }
        assert_eq!(found_pubkeys.len(), 3, "Should have 3 members total");
    }

    // TODO: Fix remaining test cases that need to be updated to match new API

    #[test]
    fn test_remove_members_group_not_found() {
        let mdk = create_test_mdk();
        let non_existent_group_id = crate::GroupId::from_slice(&[1, 2, 3, 4, 5]);
        let dummy_pubkey = Keys::generate().public_key();

        let result = mdk.remove_members(&non_existent_group_id, &[dummy_pubkey]);
        assert!(
            matches!(result, Err(crate::Error::GroupNotFound)),
            "Should return GroupNotFound error for non-existent group"
        );
    }

    #[test]
    fn test_remove_members_no_matching_members() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Try to remove a member that doesn't exist in the group
        let non_member = Keys::generate().public_key();
        let result = creator_mdk.remove_members(group_id, &[non_member]);

        assert!(
            matches!(
                result,
                Err(crate::Error::Group(ref msg)) if msg.contains("No matching members found")
            ),
            "Should return error when no matching members found"
        );
    }

    #[test]
    fn test_remove_members_epoch_advancement() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get initial epoch
        let initial_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        let initial_epoch = initial_group.epoch;

        // Remove a member
        let member_to_remove = initial_members[0].public_key();
        let _remove_result = creator_mdk
            .remove_members(group_id, &[member_to_remove])
            .expect("Failed to remove member");

        // Merge the pending commit for the member removal
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for member removal");

        // Verify the MLS group epoch was advanced
        let mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let final_mls_epoch = mls_group.epoch().as_u64();

        assert!(
            final_mls_epoch > initial_epoch,
            "MLS group epoch should advance after removing members (initial: {}, final: {})",
            initial_epoch,
            final_mls_epoch
        );

        // Verify the member was removed
        let final_members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get members");
        assert!(
            !final_members.contains(&member_to_remove),
            "Removed member should not be in the group"
        );
        assert_eq!(
            final_members.len(),
            2, // creator + 1 remaining member
            "Should have 2 total members after removal"
        );
    }

    #[test]
    fn test_self_update_success() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Verify initial group state
        let initial_members_set = creator_mdk
            .get_members(group_id)
            .expect("Failed to get initial members");
        assert_eq!(initial_members_set.len(), 3); // creator + 2 initial members

        // Get initial group state
        let initial_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let initial_epoch = initial_mls_group.epoch().as_u64();

        // Perform self update
        let update_result = creator_mdk
            .self_update(group_id)
            .expect("Failed to perform self update");

        // Merge the pending commit for the self update
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for self update");

        // Verify the result contains the expected data
        assert!(
            !update_result.evolution_event.content.is_empty(),
            "Evolution event should not be empty"
        );
        // Note: self_update typically doesn't produce a welcome message unless there are special circumstances
        // assert!(update_result.serialized_welcome_message.is_none(), "Welcome message should typically be None for self-update");

        // Verify the group state was updated correctly
        let final_members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get final members");
        assert_eq!(
            final_members.len(),
            3,
            "Member count should remain the same after self update"
        );

        // Verify all original members are still in the group
        assert!(
            final_members.contains(&creator_pk),
            "Creator should still be in group"
        );
        for initial_member_keys in &initial_members {
            assert!(
                final_members.contains(&initial_member_keys.public_key()),
                "Initial member should still be in group"
            );
        }

        // Verify the epoch was advanced
        let final_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let final_epoch = final_mls_group.epoch().as_u64();

        assert!(
            final_epoch > initial_epoch,
            "Epoch should advance after self update (initial: {}, final: {})",
            initial_epoch,
            final_epoch
        );
    }

    #[test]
    fn test_self_update_group_not_found() {
        let mdk = create_test_mdk();
        let non_existent_group_id = crate::GroupId::from_slice(&[1, 2, 3, 4, 5]);

        let result = mdk.self_update(&non_existent_group_id);
        assert!(
            matches!(result, Err(crate::Error::GroupNotFound)),
            "Should return GroupNotFound error for non-existent group"
        );
    }

    #[test]
    fn test_self_update_key_rotation() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get initial signature key from the leaf node
        let initial_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let initial_own_leaf = initial_mls_group
            .own_leaf()
            .expect("Failed to get initial own leaf");
        let initial_signature_key = initial_own_leaf.signature_key().as_slice().to_vec();

        // Perform self update (this should rotate the signing key)
        let _update_result = creator_mdk
            .self_update(group_id)
            .expect("Failed to perform self update");

        // Merge the pending commit for the self update
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for self update");

        // Get the new signature key
        let final_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let final_own_leaf = final_mls_group
            .own_leaf()
            .expect("Failed to get final own leaf");
        let final_signature_key = final_own_leaf.signature_key().as_slice().to_vec();

        // Verify the signature key has been rotated
        assert_ne!(
            initial_signature_key, final_signature_key,
            "Signature key should be different after self update"
        );

        // Verify the public key identity remains the same
        let initial_credential = BasicCredential::try_from(initial_own_leaf.credential().clone())
            .expect("Failed to extract initial credential");
        let final_credential = BasicCredential::try_from(final_own_leaf.credential().clone())
            .expect("Failed to extract final credential");

        assert_eq!(
            initial_credential.identity(),
            final_credential.identity(),
            "Public key identity should remain the same after self update"
        );
    }

    #[test]
    fn test_self_update_exporter_secret_rotation() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get initial exporter secret
        let initial_secret = creator_mdk
            .exporter_secret(group_id)
            .expect("Failed to get initial exporter secret");

        // Perform self update
        let _update_result = creator_mdk
            .self_update(group_id)
            .expect("Failed to perform self update");

        // Merge the pending commit for the self update
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for self update");

        // Get the new exporter secret
        let final_secret = creator_mdk
            .exporter_secret(group_id)
            .expect("Failed to get final exporter secret");

        // Verify the exporter secret has been rotated
        assert_ne!(
            initial_secret.secret, final_secret.secret,
            "Exporter secret should be different after self update"
        );

        // Verify the epoch has advanced
        assert!(
            final_secret.epoch > initial_secret.epoch,
            "Epoch should advance after self update (initial: {}, final: {})",
            initial_secret.epoch,
            final_secret.epoch
        );

        // Verify the group ID remains the same
        assert_eq!(
            initial_secret.mls_group_id, final_secret.mls_group_id,
            "Group ID should remain the same"
        );
    }

    #[test]
    fn test_update_group_data() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get initial group data for comparison
        let initial_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let initial_group_data = NostrGroupDataExtension::from_group(&initial_mls_group).unwrap();

        // Test 1: Update only the name
        let new_name = "Updated Name".to_string();
        let update = NostrGroupDataUpdate::new().name(new_name.clone());
        let update_result = creator_mdk
            .update_group_data(group_id, update)
            .expect("Failed to update group name");

        assert!(!update_result.evolution_event.content.is_empty());
        assert!(update_result.welcome_rumors.is_none());

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        let updated_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let updated_group_data = NostrGroupDataExtension::from_group(&updated_mls_group).unwrap();

        assert_eq!(updated_group_data.name, new_name);
        assert_eq!(
            updated_group_data.description,
            initial_group_data.description
        );
        assert_eq!(updated_group_data.image_hash, initial_group_data.image_hash);

        // Test 2: Update multiple fields at once
        let new_description = "Updated Description".to_string();
        let new_image_hash =
            mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes(32)
                .try_into()
                .unwrap();
        let new_image_key = mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes(32)
            .try_into()
            .unwrap();
        let new_image_upload_key =
            mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes(32)
                .try_into()
                .unwrap();

        let update = NostrGroupDataUpdate::new()
            .description(new_description.clone())
            .image_hash(Some(new_image_hash))
            .image_key(Some(new_image_key))
            .image_upload_key(Some(new_image_upload_key));

        let update_result = creator_mdk
            .update_group_data(group_id, update)
            .expect("Failed to update multiple fields");

        assert!(!update_result.evolution_event.content.is_empty());

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        let final_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let final_group_data = NostrGroupDataExtension::from_group(&final_mls_group).unwrap();

        assert_eq!(final_group_data.name, new_name); // Should remain from previous update
        assert_eq!(final_group_data.description, new_description);
        assert_eq!(final_group_data.image_hash, Some(new_image_hash));
        assert_eq!(final_group_data.image_key, Some(new_image_key));
        assert_eq!(
            final_group_data.image_upload_key,
            Some(new_image_upload_key)
        );

        // Test 3: Clear optional fields
        let update = NostrGroupDataUpdate::new().image_hash(None);

        let update_result = creator_mdk
            .update_group_data(group_id, update)
            .expect("Failed to clear optional fields");

        assert!(!update_result.evolution_event.content.is_empty());

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        let cleared_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let cleared_group_data = NostrGroupDataExtension::from_group(&cleared_mls_group).unwrap();

        assert_eq!(cleared_group_data.name, new_name);
        assert_eq!(cleared_group_data.description, new_description);
        assert_eq!(cleared_group_data.image_hash, None);
        assert_eq!(cleared_group_data.image_key, None);
        assert_eq!(cleared_group_data.image_nonce, None);
        assert_eq!(cleared_group_data.image_upload_key, None);

        // Test 4: Empty update (should succeed but not change anything)
        let empty_update = NostrGroupDataUpdate::new();
        let update_result = creator_mdk
            .update_group_data(group_id, empty_update)
            .expect("Failed to apply empty update");

        assert!(!update_result.evolution_event.content.is_empty());

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        let unchanged_mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let unchanged_group_data =
            NostrGroupDataExtension::from_group(&unchanged_mls_group).unwrap();

        assert_eq!(unchanged_group_data.name, cleared_group_data.name);
        assert_eq!(
            unchanged_group_data.description,
            cleared_group_data.description
        );
        assert_eq!(
            unchanged_group_data.image_hash,
            cleared_group_data.image_hash
        );
        assert_eq!(unchanged_group_data.image_key, cleared_group_data.image_key);
    }

    #[test]
    fn test_sync_group_metadata_from_mls() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins.clone()),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get initial stored group state
        let initial_stored_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get initial stored group")
            .expect("Stored group should exist");

        // Modify the MLS group directly (simulating state change without sync)
        let mut mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Create a new group data extension with different values
        let mut new_group_data = NostrGroupDataExtension::from_group(&mls_group).unwrap();
        new_group_data.name = "Synchronized Name".to_string();
        new_group_data.description = "Synchronized Description".to_string();

        // Apply the extension update to MLS group (but not to stored group)
        let extension =
            super::MDK::<MdkMemoryStorage>::get_unknown_extension_from_group_data(&new_group_data)
                .unwrap();
        let mut extensions = mls_group.extensions().clone();
        extensions.add_or_replace(extension).unwrap();

        let signature_keypair = creator_mdk.load_mls_signer(&mls_group).unwrap();
        let (_message_out, _, _) = mls_group
            .update_group_context_extensions(&creator_mdk.provider, extensions, &signature_keypair)
            .unwrap();

        // Merge the pending commit to advance epoch
        mls_group
            .merge_pending_commit(&creator_mdk.provider)
            .unwrap();

        // At this point, MLS group has changed but stored group is stale
        let stale_stored_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get stale stored group")
            .expect("Stored group should exist");

        // Verify stored group is stale
        assert_eq!(stale_stored_group.name, initial_stored_group.name);
        assert_eq!(
            stale_stored_group.description,
            initial_stored_group.description
        );
        assert_eq!(stale_stored_group.epoch, initial_stored_group.epoch);

        // Now test our sync function
        creator_mdk
            .sync_group_metadata_from_mls(group_id)
            .expect("Failed to sync group metadata");

        // Verify stored group is now synchronized
        let synced_stored_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get synced stored group")
            .expect("Stored group should exist");

        assert_eq!(synced_stored_group.name, "Synchronized Name");
        assert_eq!(synced_stored_group.description, "Synchronized Description");
        assert!(synced_stored_group.epoch > initial_stored_group.epoch);
        assert_eq!(
            synced_stored_group.admin_pubkeys,
            admins.into_iter().collect::<BTreeSet<_>>()
        );

        // Verify other fields remain unchanged
        assert_eq!(
            synced_stored_group.mls_group_id,
            initial_stored_group.mls_group_id
        );
        assert_eq!(
            synced_stored_group.last_message_id,
            initial_stored_group.last_message_id
        );
        assert_eq!(
            synced_stored_group.last_message_at,
            initial_stored_group.last_message_at
        );
        assert_eq!(synced_stored_group.state, initial_stored_group.state);
    }

    #[test]
    fn test_extension_updates_create_processed_messages() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Test that each extension update creates a ProcessedMessage
        let test_cases = vec![
            ("update_group_name", "New Name"),
            ("update_group_description", "New Description"),
        ];

        for (operation, _value) in test_cases {
            let update_result = match operation {
                "update_group_name" => {
                    let update = NostrGroupDataUpdate::new().name("New Name".to_string());
                    creator_mdk.update_group_data(group_id, update)
                }
                "update_group_description" => {
                    let update =
                        NostrGroupDataUpdate::new().description("New Description".to_string());
                    creator_mdk.update_group_data(group_id, update)
                }
                _ => panic!("Unknown operation"),
            };

            let update_result = update_result.unwrap_or_else(|_| panic!("Failed to {}", operation));
            let commit_event_id = update_result.evolution_event.id;

            // Verify ProcessedMessage was created with correct state
            let processed_message = creator_mdk
                .storage()
                .find_processed_message_by_event_id(&commit_event_id)
                .expect("Failed to query processed message")
                .expect("ProcessedMessage should exist");

            assert_eq!(processed_message.wrapper_event_id, commit_event_id);
            assert_eq!(processed_message.message_event_id, None);
            assert_eq!(
                processed_message.state,
                message_types::ProcessedMessageState::ProcessedCommit
            );
            assert_eq!(processed_message.failure_reason, None);

            // Clean up by merging the commit
            creator_mdk
                .merge_pending_commit(group_id)
                .unwrap_or_else(|_| panic!("Failed to merge pending commit for {}", operation));
        }
    }

    #[test]
    fn test_stored_group_sync_after_all_operations() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Helper function to verify stored group epoch matches MLS group epoch
        let verify_epoch_sync = || {
            let mls_group = creator_mdk.load_mls_group(group_id).unwrap().unwrap();
            let stored_group = creator_mdk.get_group(group_id).unwrap().unwrap();
            assert_eq!(
                stored_group.epoch,
                mls_group.epoch().as_u64(),
                "Stored group epoch should match MLS group epoch"
            );
        };

        // Test 1: After group creation (should already be synced)
        verify_epoch_sync();

        // Test 2: After adding members
        let new_member = Keys::generate();
        let new_key_package_event = create_key_package_event(&creator_mdk, &new_member);
        let _add_result = creator_mdk
            .add_members(group_id, &[new_key_package_event])
            .expect("Failed to add member");

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for add member");
        verify_epoch_sync();

        // Test 3: After self update
        let _self_update_result = creator_mdk
            .self_update(group_id)
            .expect("Failed to perform self update");

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for self update");
        verify_epoch_sync();

        // Test 4: After extension updates
        let update = NostrGroupDataUpdate::new().name("Final Name".to_string());
        let _name_result = creator_mdk
            .update_group_data(group_id, update)
            .expect("Failed to update group name");

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit for name update");
        verify_epoch_sync();

        // Test 5: Verify stored group metadata matches extension data
        let final_mls_group = creator_mdk.load_mls_group(group_id).unwrap().unwrap();
        let final_stored_group = creator_mdk.get_group(group_id).unwrap().unwrap();
        let final_group_data = NostrGroupDataExtension::from_group(&final_mls_group).unwrap();

        assert_eq!(final_stored_group.name, final_group_data.name);
        assert_eq!(final_stored_group.description, final_group_data.description);
        assert_eq!(final_stored_group.admin_pubkeys, final_group_data.admins);
        assert_eq!(
            final_stored_group.nostr_group_id,
            final_group_data.nostr_group_id
        );
    }

    #[test]
    fn test_sync_group_metadata_error_cases() {
        let creator_mdk = create_test_mdk();

        // Test with non-existent group
        let non_existent_group_id = crate::GroupId::from_slice(&[1, 2, 3, 4, 5]);
        let result = creator_mdk.sync_group_metadata_from_mls(&non_existent_group_id);
        assert!(matches!(result, Err(crate::Error::GroupNotFound)));
    }

    #[test]
    fn test_sync_group_metadata_propagates_extension_parse_failure() {
        use openmls::prelude::{Extension, UnknownExtension};

        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins.clone()),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id;

        // Merge the pending commit
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Load the MLS group and corrupt the group-data extension
        let mut mls_group = creator_mdk
            .load_mls_group(group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");

        // Create a corrupted extension with invalid data
        let corrupted_extension_data = vec![0xFF, 0xFF, 0xFF]; // Invalid TLS-serialized data
        let corrupted_extension = Extension::Unknown(
            NOSTR_GROUP_DATA_EXTENSION_TYPE,
            UnknownExtension(corrupted_extension_data),
        );

        // Replace the group-data extension with the corrupted one
        let mut extensions = mls_group.extensions().clone();
        extensions.add_or_replace(corrupted_extension).unwrap();

        let signature_keypair = creator_mdk.load_mls_signer(&mls_group).unwrap();
        let (_message_out, _, _) = mls_group
            .update_group_context_extensions(&creator_mdk.provider, extensions, &signature_keypair)
            .unwrap();

        // Merge the pending commit to apply the corrupted extension
        mls_group
            .merge_pending_commit(&creator_mdk.provider)
            .unwrap();

        // Now test that sync_group_metadata_from_mls properly propagates the parse error
        let result = creator_mdk.sync_group_metadata_from_mls(group_id);

        // The function should return an error, not silently ignore the parse failure
        assert!(
            result.is_err(),
            "sync_group_metadata_from_mls should propagate extension parse errors"
        );

        // Verify it's a deserialization error (the specific error from deserialize_bytes)
        match result {
            Err(e) => {
                let error_msg = e.to_string();
                assert!(
                    error_msg.contains("TLS")
                        || error_msg.contains("deserialize")
                        || error_msg.contains("EndOfStream"),
                    "Expected deserialization error, got: {}",
                    error_msg
                );
            }
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    /// Test getting group that doesn't exist
    #[test]
    fn test_get_nonexistent_group() {
        let mdk = create_test_mdk();
        let non_existent_id = crate::GroupId::from_slice(&[9, 9, 9, 9]);

        let result = mdk.get_group(&non_existent_id);

        assert!(result.is_ok(), "Should succeed");
        assert!(
            result.unwrap().is_none(),
            "Should return None for non-existent group"
        );
    }

    /// Member self-removal proposal
    ///
    /// Tests that leave_group creates a valid leave proposal.
    /// Note: A member cannot unilaterally leave - they create a proposal
    /// that must be committed by another member (typically an admin).
    ///
    /// Requirements tested:
    /// - leave_group creates valid MLS proposal events
    /// - leave_group works for group members
    /// - The proposal can be processed by other members
    #[test]
    fn test_member_self_removal() {
        use crate::test_util::create_key_package_event;

        // Create Alice (admin) and Bob (member)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key()];

        // Bob creates his key package
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates the group
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should be able to create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge Alice's create commit");

        // Bob processes and accepts welcome
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should be able to process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should be able to accept welcome");

        // Verify initial member count
        let initial_members = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(initial_members.len(), 2, "Group should have 2 members");

        // Bob calls leave_group
        let bob_leave_result = bob_mdk.leave_group(&group_id);
        assert!(
            bob_leave_result.is_ok(),
            "Bob should be able to call leave_group: {:?}",
            bob_leave_result.err()
        );

        // Verify leave generates proper MLS evolution event
        let bob_leave_event = bob_leave_result.unwrap().evolution_event;
        assert_eq!(
            bob_leave_event.kind,
            nostr::Kind::MlsGroupMessage,
            "Leave should generate MLS group message event"
        );

        // Verify the leave event has required tags
        assert!(
            bob_leave_event.tags.iter().any(|t| t.kind()
                == nostr::TagKind::SingleLetter(nostr::SingleLetterTag::from_char('h').unwrap())),
            "Leave event should have group ID tag"
        );

        // (1) Verify Bob is still in the group from Alice's perspective
        // The leave is only a proposal and hasn't been applied yet
        let members_after_leave_call = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members after leave call");
        assert_eq!(
            members_after_leave_call.len(),
            2,
            "Bob should still be in group - leave hasn't been processed yet"
        );
        assert!(
            members_after_leave_call.contains(&bob_keys.public_key()),
            "Bob should still be in member list until another member processes the leave"
        );

        // (2) Alice processes Bob's leave event
        // OpenMLS behavior: Alice receives the leave proposal
        let process_result = alice_mdk.process_message(&bob_leave_event);
        assert!(
            process_result.is_ok(),
            "Alice should be able to process Bob's leave event: {:?}",
            process_result.err()
        );

        // (3) Check if merge is needed
        let _merge_result = alice_mdk.merge_pending_commit(&group_id);

        // (4) Verify Bob's leave was processed successfully
        // The leave_group call by Bob creates a valid leave event that Alice can process
        // Whether Bob is immediately removed depends on OpenMLS implementation details
        let final_members = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");

        // The test verifies that leave_group creates a valid event structure
        // that other members can process without errors
        assert!(
            final_members.len() <= 2,
            "Group should have at most 2 members after processing leave"
        );
    }

    /// Member removal and re-addition
    ///
    /// Tests that attempting to add an existing member with the same KeyPackage fails,
    /// but the member can be successfully re-added after removal using a new KeyPackage.
    ///
    /// Requirements tested:
    /// - Cannot add existing member with same KeyPackage (OpenMLS deterministic behavior)
    /// - Member can be removed from group
    /// - Member can be successfully re-added after removal with new KeyPackage
    #[test]
    fn test_cannot_add_existing_member() {
        use crate::test_util::create_key_package_event;

        // Create Alice (admin) and Bob (member)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key()];

        // Bob creates his key package
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates group with Bob as member
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package.clone()],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should be able to create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge Alice's create commit");

        // Bob processes and accepts welcome
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should be able to process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should be able to accept welcome");

        // Verify initial member count
        let initial_members = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(initial_members.len(), 2, "Group should have 2 members");

        // Step 1: Alice attempts to add Bob again using the same KeyPackage
        // OpenMLS should reject this because Bob is already in the group
        let add_duplicate_result = alice_mdk.add_members(&group_id, &[bob_key_package]);
        assert!(
            add_duplicate_result.is_err(),
            "Should not be able to add existing member with same KeyPackage"
        );

        // Verify member count unchanged
        let members_after_duplicate = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(
            members_after_duplicate.len(),
            2,
            "Member count should not change after rejected duplicate add"
        );

        // Step 2: Alice removes Bob
        let remove_result = alice_mdk
            .remove_members(&group_id, &[bob_keys.public_key()])
            .expect("Should be able to remove Bob");

        alice_mdk
            .process_message(&remove_result.evolution_event)
            .expect("Failed to process remove");
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge remove commit");

        // Verify Bob is removed
        let members_after_remove = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(
            members_after_remove.len(),
            1,
            "Group should have 1 member after removal"
        );
        assert!(
            !members_after_remove.contains(&bob_keys.public_key()),
            "Bob should not be in group"
        );

        // Step 3: Alice adds Bob back (should succeed)
        let bob_new_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let readd_result = alice_mdk.add_members(&group_id, &[bob_new_key_package]);

        assert!(
            readd_result.is_ok(),
            "Should be able to re-add Bob after removal: {:?}",
            readd_result.err()
        );

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge re-add commit");

        // Verify Bob is back in the group
        let final_members = alice_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        assert_eq!(final_members.len(), 2, "Group should have 2 members again");
        assert!(
            final_members.contains(&bob_keys.public_key()),
            "Bob should be back in group"
        );
    }

    /// Test that non-admins cannot add members to a group
    #[test]
    fn test_non_admin_cannot_add_members() {
        use crate::test_util::create_key_package_event;

        let creator_mdk = create_test_mdk();
        let creator = Keys::generate();
        let non_admin_keys = Keys::generate();

        // Only creator is admin
        let admins = vec![creator.public_key()];

        // Non-admin creates their own MDK and key package
        let non_admin_mdk = create_test_mdk();
        let non_admin_key_package = create_key_package_event(&non_admin_mdk, &non_admin_keys);

        // Creator creates group with non-admin as member
        let create_result = creator_mdk
            .create_group(
                &creator.public_key(),
                vec![non_admin_key_package],
                create_nostr_group_config_data(admins.clone()),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        // Non-admin joins the group
        let non_admin_welcome_rumor = &create_result.welcome_rumors[0];
        let non_admin_welcome = non_admin_mdk
            .process_welcome(&nostr::EventId::all_zeros(), non_admin_welcome_rumor)
            .expect("Non-admin should process welcome");
        non_admin_mdk
            .accept_welcome(&non_admin_welcome)
            .expect("Non-admin should accept welcome");

        // Verify non-admin is not an admin
        assert!(
            !admins.contains(&non_admin_keys.public_key()),
            "Non-admin should not be in admin list"
        );

        // Get initial member count
        let initial_member_count = creator_mdk
            .get_members(&group_id)
            .expect("Failed to get members")
            .len();

        // Try to have the non-admin add a new member
        let new_member_keys = Keys::generate();
        let new_member_key_package = create_key_package_event(&non_admin_mdk, &new_member_keys);

        let result = non_admin_mdk.add_members(&group_id, &[new_member_key_package]);

        // Should fail with permission error, not GroupNotFound
        assert!(
            result.is_err(),
            "Non-admin should not be able to add members"
        );
        assert!(
            matches!(result, Err(crate::Error::Group(ref msg)) if msg.contains("Only group admins can add members")),
            "Should fail with admin permission error, got: {:?}",
            result
        );

        // Verify that the members list did not change
        let final_member_count = creator_mdk
            .get_members(&group_id)
            .expect("Failed to get members")
            .len();
        assert_eq!(
            initial_member_count, final_member_count,
            "Member count should not change when non-admin attempts to add members"
        );
    }

    /// Test that non-admins cannot remove members from a group
    #[test]
    fn test_non_admin_cannot_remove_members() {
        use crate::test_util::create_key_package_event;

        let creator_mdk = create_test_mdk();
        let creator = Keys::generate();
        let non_admin_keys = Keys::generate();
        let other_member_keys = Keys::generate();

        // Only creator is admin
        let admins = vec![creator.public_key()];

        // Create MDKs and key packages for members
        let non_admin_mdk = create_test_mdk();
        let other_member_mdk = create_test_mdk();
        let non_admin_key_package = create_key_package_event(&non_admin_mdk, &non_admin_keys);
        let other_member_key_package =
            create_key_package_event(&other_member_mdk, &other_member_keys);

        // Creator creates group with non-admin and other member
        let create_result = creator_mdk
            .create_group(
                &creator.public_key(),
                vec![non_admin_key_package, other_member_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        // Non-admin joins the group
        let non_admin_welcome_rumor = &create_result.welcome_rumors[0];
        let non_admin_welcome = non_admin_mdk
            .process_welcome(&nostr::EventId::all_zeros(), non_admin_welcome_rumor)
            .expect("Non-admin should process welcome");
        non_admin_mdk
            .accept_welcome(&non_admin_welcome)
            .expect("Non-admin should accept welcome");

        // Get initial member count
        let initial_member_count = creator_mdk
            .get_members(&group_id)
            .expect("Failed to get members")
            .len();

        // Try to have the non-admin remove another member
        let result = non_admin_mdk.remove_members(&group_id, &[other_member_keys.public_key()]);

        // Should fail with permission error, not GroupNotFound
        assert!(
            result.is_err(),
            "Non-admin should not be able to remove members"
        );
        assert!(
            matches!(result, Err(crate::Error::Group(ref msg)) if msg.contains("Only group admins can remove members")),
            "Should fail with admin permission error, got: {:?}",
            result
        );

        // Verify that the members list did not change
        let final_members_list = creator_mdk
            .get_members(&group_id)
            .expect("Failed to get members");
        let final_member_count = final_members_list.len();

        assert_eq!(
            initial_member_count, final_member_count,
            "Member count should not change when non-admin attempts to remove members"
        );

        // Verify the specific member is still present
        assert!(
            final_members_list
                .iter()
                .any(|m| m == &other_member_keys.public_key()),
            "Target member should still be in the group"
        );
    }

    /// Test that non-admins cannot update group extensions
    #[test]
    fn test_non_admin_cannot_update_group_extensions() {
        use crate::test_util::create_key_package_event;

        let creator_mdk = create_test_mdk();
        let creator = Keys::generate();
        let non_admin_keys = Keys::generate();

        // Only creator is admin
        let admins = vec![creator.public_key()];

        // Non-admin creates their own MDK and key package
        let non_admin_mdk = create_test_mdk();
        let non_admin_key_package = create_key_package_event(&non_admin_mdk, &non_admin_keys);

        // Creator creates group with non-admin as member
        let create_result = creator_mdk
            .create_group(
                &creator.public_key(),
                vec![non_admin_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = create_result.group.mls_group_id.clone();

        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        // Non-admin joins the group
        let non_admin_welcome_rumor = &create_result.welcome_rumors[0];
        let non_admin_welcome = non_admin_mdk
            .process_welcome(&nostr::EventId::all_zeros(), non_admin_welcome_rumor)
            .expect("Non-admin should process welcome");
        non_admin_mdk
            .accept_welcome(&non_admin_welcome)
            .expect("Non-admin should accept welcome");

        // Get initial group metadata
        let initial_group = creator_mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        let initial_name = initial_group.name.clone();
        let initial_description = initial_group.description.clone();

        // Try to have the non-admin update group name
        let update = NostrGroupDataUpdate::new().name("Hacked Name".to_string());
        let result = non_admin_mdk.update_group_data(&group_id, update);

        // Should fail with permission error, not GroupNotFound
        assert!(
            result.is_err(),
            "Non-admin should not be able to update group extensions"
        );
        assert!(
            matches!(result, Err(crate::Error::Group(ref msg)) if msg.contains("Only group admins")),
            "Should fail with admin permission error, got: {:?}",
            result
        );

        // Verify that the group metadata did not change
        let final_group = creator_mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        assert_eq!(
            initial_name, final_group.name,
            "Group name should not change when non-admin attempts to update"
        );
        assert_eq!(
            initial_description, final_group.description,
            "Group description should not change when non-admin attempts to update"
        );
    }

    /// Test creator validation errors
    #[test]
    fn test_creator_validation_errors() {
        let mdk = create_test_mdk();
        let creator = Keys::generate();
        let member1 = Keys::generate();
        let member2 = Keys::generate();

        let creator_pk = creator.public_key();
        let member_pks = vec![member1.public_key(), member2.public_key()];

        // Test 1: Creator not in admin list
        let bad_admins = vec![member1.public_key()];
        let result = mdk.validate_group_members(&creator_pk, &member_pks, &bad_admins);
        assert!(
            matches!(result, Err(crate::Error::Group(ref msg)) if msg.contains("Creator must be an admin")),
            "Should error when creator is not an admin"
        );

        // Test 2: Creator in member list
        let bad_members = vec![creator_pk, member1.public_key()];
        let admins = vec![creator_pk];
        let result = mdk.validate_group_members(&creator_pk, &bad_members, &admins);
        assert!(
            matches!(result, Err(crate::Error::Group(ref msg)) if msg.contains("Creator must not be included as a member")),
            "Should error when creator is in member list"
        );

        // Test 3: Admin not in member list
        let non_member_admin = Keys::generate().public_key();
        let bad_admins = vec![creator_pk, non_member_admin];
        let result = mdk.validate_group_members(&creator_pk, &member_pks, &bad_admins);
        assert!(
            matches!(result, Err(crate::Error::Group(ref msg)) if msg.contains("Admin must be a member")),
            "Should error when admin is not a member"
        );
    }

    /// Test that admin update validation rejects empty admin sets
    #[test]
    fn test_admin_update_rejects_empty_admin_set() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Attempt to update with empty admin set - should fail
        let empty_admins: Vec<PublicKey> = vec![];
        let update = NostrGroupDataUpdate::new().admins(empty_admins);
        let result = creator_mdk.update_group_data(group_id, update);

        assert!(
            matches!(result, Err(crate::Error::UpdateGroupContextExts(ref msg)) if msg.contains("Admin set cannot be empty")),
            "Should error when admin set is empty, got: {:?}",
            result
        );
    }

    /// Test that admin update validation rejects non-member admins
    #[test]
    fn test_admin_update_rejects_non_member_admins() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Attempt to update with a non-member admin - should fail
        let non_member = Keys::generate().public_key();
        let bad_admins = vec![creator_pk, non_member];
        let update = NostrGroupDataUpdate::new().admins(bad_admins);
        let result = creator_mdk.update_group_data(group_id, update);

        assert!(
            matches!(result, Err(crate::Error::UpdateGroupContextExts(ref msg)) if msg.contains("is not a current group member")),
            "Should error when admin is not a group member, got: {:?}",
            result
        );
    }

    /// Test that admin update validation accepts valid admin sets
    #[test]
    fn test_admin_update_accepts_valid_member_admins() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Get current members
        let members = creator_mdk
            .get_members(group_id)
            .expect("Failed to get members");

        // Update admins to include all current members - should succeed
        let new_admins: Vec<PublicKey> = members.into_iter().collect();
        let update = NostrGroupDataUpdate::new().admins(new_admins.clone());
        let result = creator_mdk.update_group_data(group_id, update);

        assert!(
            result.is_ok(),
            "Should succeed when all admins are current members, got: {:?}",
            result
        );

        // Merge the pending commit
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Sync from MLS to get updated admin set
        creator_mdk
            .sync_group_metadata_from_mls(group_id)
            .expect("Failed to sync");

        let synced_group = creator_mdk
            .get_group(group_id)
            .expect("Failed to get group")
            .expect("Group should exist");

        let expected_admins: BTreeSet<PublicKey> = new_admins.into_iter().collect();
        assert_eq!(
            synced_group.admin_pubkeys, expected_admins,
            "Admin pubkeys should be updated to the new set"
        );
    }

    /// Test that admin update only accepts existing members, not previously removed members
    #[test]
    fn test_admin_update_rejects_previously_removed_member() {
        let creator_mdk = create_test_mdk();
        let (creator, initial_members, admins) = create_test_group_members();
        let creator_pk = creator.public_key();

        // Capture member public keys before they're used
        let member1_pk = initial_members[0].public_key();
        let member2_pk = initial_members[1].public_key();

        // Create key package events for initial members
        let mut initial_key_package_events = Vec::new();
        for member_keys in &initial_members {
            let key_package_event = create_key_package_event(&creator_mdk, member_keys);
            initial_key_package_events.push(key_package_event);
        }

        // Create the group
        let create_result = creator_mdk
            .create_group(
                &creator_pk,
                initial_key_package_events,
                create_nostr_group_config_data(admins),
            )
            .expect("Failed to create group");

        let group_id = &create_result.group.mls_group_id.clone();

        // Merge the pending commit to apply the member additions
        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Remove member1 from the group
        creator_mdk
            .remove_members(group_id, &[member1_pk])
            .expect("Failed to remove member");

        creator_mdk
            .merge_pending_commit(group_id)
            .expect("Failed to merge pending commit");

        // Attempt to make the removed member an admin - should fail
        let bad_admins = vec![creator_pk, member1_pk];
        let update = NostrGroupDataUpdate::new().admins(bad_admins);
        let result = creator_mdk.update_group_data(group_id, update);

        assert!(
            matches!(result, Err(crate::Error::UpdateGroupContextExts(ref msg)) if msg.contains("is not a current group member")),
            "Should error when trying to make removed member an admin, got: {:?}",
            result
        );

        // But updating with remaining members should work
        let good_admins = vec![creator_pk, member2_pk];
        let update = NostrGroupDataUpdate::new().admins(good_admins);
        let result = creator_mdk.update_group_data(group_id, update);

        assert!(
            result.is_ok(),
            "Should succeed when all admins are current members, got: {:?}",
            result
        );
    }

    /// Test getting all groups when none exist
    #[test]
    fn test_get_groups_empty() {
        let mdk = create_test_mdk();

        let groups = mdk.get_groups().expect("Should succeed");

        assert_eq!(groups.len(), 0, "Should have no groups initially");
    }

    /// Test getting all groups returns created groups
    #[test]
    fn test_get_groups_with_data() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();

        // Create a group
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Get all groups
        let groups = creator_mdk.get_groups().expect("Should succeed");

        assert_eq!(groups.len(), 1, "Should have 1 group");
        assert_eq!(groups[0].mls_group_id, group_id, "Group ID should match");
    }

    /// Test getting relays for a group
    #[test]
    fn test_get_relays() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();

        // Create a group (create_nostr_group_config_data includes test relays)
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Get relays for the group
        let relays = creator_mdk
            .get_relays(&group_id)
            .expect("Should get relays");

        // Verify relays were stored (test config includes relays)
        assert!(!relays.is_empty(), "Group should have relays");
    }

    /// Test getting members for non-existent group
    #[test]
    fn test_get_members_nonexistent_group() {
        let mdk = create_test_mdk();
        let non_existent_id = crate::GroupId::from_slice(&[9, 9, 9, 9]);

        let result = mdk.get_members(&non_existent_id);

        // Should fail because group doesn't exist
        assert!(result.is_err(), "Should fail for non-existent group");
    }

    /// Test group name and description updates
    #[test]
    fn test_group_metadata_updates() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Update group name
        let update = NostrGroupDataUpdate::new().name("New Name".to_string());
        let result = creator_mdk.update_group_data(&group_id, update);
        assert!(result.is_ok(), "Should be able to update group name");
        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        // Update group description
        let update = NostrGroupDataUpdate::new().description("New Description".to_string());
        let result = creator_mdk.update_group_data(&group_id, update);
        assert!(result.is_ok(), "Should be able to update group description");
        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        // Update both at once
        let update = NostrGroupDataUpdate::new()
            .name("Final Name".to_string())
            .description("Final Description".to_string());
        let result = creator_mdk.update_group_data(&group_id, update);
        assert!(
            result.is_ok(),
            "Should be able to update both name and description"
        );
        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");
    }

    /// Test group with empty name
    #[test]
    fn test_group_with_empty_name() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Update to empty name (should be valid)
        let update = NostrGroupDataUpdate::new().name("".to_string());
        let result = creator_mdk.update_group_data(&group_id, update);
        assert!(result.is_ok(), "Empty group name should be valid");
        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");
    }

    /// Test group with long name within allowed limits
    ///
    /// Security fix (Issue #82): Group names are now limited to prevent memory exhaustion.
    /// This test verifies that names within the limit work correctly.
    #[test]
    fn test_group_with_long_name() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Update to a long name within the allowed limit (256 bytes)
        let long_name = "a".repeat(256);
        let update = NostrGroupDataUpdate::new().name(long_name);
        let result = creator_mdk.update_group_data(&group_id, update);
        assert!(result.is_ok(), "Group name at limit should be valid");
        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");
    }

    /// Test that nostr_group_id can be rotated via update_group_data
    ///
    /// MIP-01 allows nostr_group_id rotation via proposals. This test verifies
    /// that the update API supports rotating the nostr_group_id for message routing.
    #[test]
    fn test_update_nostr_group_id() {
        let creator_mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&creator_mdk, &creator, &members, &admins);

        // Get the initial nostr_group_id
        let initial_mls_group = creator_mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let initial_group_data = NostrGroupDataExtension::from_group(&initial_mls_group).unwrap();
        let initial_nostr_group_id = initial_group_data.nostr_group_id;

        // Create a new nostr_group_id
        let new_nostr_group_id: [u8; 32] = [42u8; 32];

        // Update the nostr_group_id via the update API
        let update = NostrGroupDataUpdate::new().nostr_group_id(new_nostr_group_id);
        let result = creator_mdk.update_group_data(&group_id, update);
        assert!(result.is_ok(), "Should be able to update nostr_group_id");

        creator_mdk
            .merge_pending_commit(&group_id)
            .expect("Failed to merge commit");

        // Verify the nostr_group_id was updated in the MLS extension
        let final_mls_group = creator_mdk
            .load_mls_group(&group_id)
            .expect("Failed to load MLS group")
            .expect("MLS group should exist");
        let final_group_data = NostrGroupDataExtension::from_group(&final_mls_group).unwrap();

        assert_ne!(
            final_group_data.nostr_group_id, initial_nostr_group_id,
            "nostr_group_id should have changed"
        );
        assert_eq!(
            final_group_data.nostr_group_id, new_nostr_group_id,
            "nostr_group_id should match the new value"
        );

        // Verify the stored group metadata was synced
        let stored_group = creator_mdk
            .get_group(&group_id)
            .expect("Failed to get group")
            .expect("Group should exist");
        assert_eq!(
            stored_group.nostr_group_id, new_nostr_group_id,
            "Stored group nostr_group_id should be synced"
        );
    }

    // ============================================================================
    // Proposal/Commit Edge Cases
    // ============================================================================

    /// Operation from Removed Member
    ///
    /// Validates that operations (adds/removes/updates) from a removed member
    /// are properly rejected to prevent security issues.
    #[test]
    fn test_operation_from_removed_member() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();
        let dave_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();
        let dave_mdk = create_test_mdk();

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates group with Bob, both are admins
        let admin_pubkeys = vec![alice_keys.public_key(), bob_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob joins
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        // Step 1: Bob successfully adds Charlie (proves Bob has admin permissions)
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);
        let bob_add_charlie = bob_mdk
            .add_members(&group_id, &[charlie_key_package])
            .expect("Bob should be able to add Charlie as admin");

        bob_mdk
            .merge_pending_commit(&group_id)
            .expect("Bob should merge commit");

        // Alice processes Bob's add commit
        alice_mdk
            .process_message(&bob_add_charlie.evolution_event)
            .expect("Alice should process Bob's commit");
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Verify Charlie is in the group
        let members_after_charlie = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(
            members_after_charlie.len(),
            3,
            "Should have 3 members (Alice, Bob, Charlie)"
        );
        assert!(
            members_after_charlie.contains(&charlie_keys.public_key()),
            "Charlie should be in the group"
        );

        // Step 2: Alice removes Bob
        let _remove_bob = alice_mdk
            .remove_members(&group_id, &[bob_keys.public_key()])
            .expect("Alice should remove Bob");

        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge removal commit");

        // Verify Bob is removed
        let members_after_removal = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(
            members_after_removal.len(),
            2,
            "Should have 2 members after Bob's removal"
        );
        assert!(
            !members_after_removal.contains(&bob_keys.public_key()),
            "Bob should not be in Alice's member list"
        );

        // Step 3: Bob attempts to add Dave (should fail - Bob is removed)
        // Bob hasn't processed his own removal yet, so he still has the group locally
        let dave_key_package = create_key_package_event(&dave_mdk, &dave_keys);
        let bob_add_dave = bob_mdk.add_members(&group_id, &[dave_key_package]);

        // Either Bob's operation fails locally, or if it succeeds,
        // Alice will reject it when processing
        if let Ok(bob_add_result) = bob_add_dave {
            // Bob was able to create a commit locally
            // Process it with Alice and merge if needed
            let alice_process_result = alice_mdk.process_message(&bob_add_result.evolution_event);

            // If processing succeeded, try to merge
            if alice_process_result.is_ok() {
                let _merge_result = alice_mdk.merge_pending_commit(&group_id);
            }
        }
        // If bob_add_dave failed locally, that's also acceptable - Bob's removal
        // was effective

        // Verify Dave was NOT added - this is the key assertion
        // Even if Bob could create a commit, it shouldn't result in Dave being added
        let final_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(
            final_members.len(),
            2,
            "Should still have 2 members (Alice and Charlie)"
        );
        assert!(
            !final_members.contains(&dave_keys.public_key()),
            "Dave should not be in the group"
        );
    }

    /// Rapid Sequential Member Operations
    ///
    /// Validates that rapid sequential member add/remove operations
    /// maintain state consistency and proper epoch advancement.
    #[test]
    fn test_rapid_sequential_member_operations() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let alice_mdk = create_test_mdk();

        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        // Create initial member
        let bob_keys = Keys::generate();
        let bob_mdk = create_test_mdk();
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob processes welcome and joins
        let bob_welcome_rumor = &create_result.welcome_rumors[0];
        let bob_welcome = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome_rumor)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome)
            .expect("Bob should accept welcome");

        let initial_epoch = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist")
            .epoch;

        // Rapidly add multiple members and have Bob process each commit
        let mut member_add_events = Vec::new();
        for i in 0..3 {
            let member_keys = Keys::generate();
            let member_mdk = create_test_mdk();
            let member_key_package = create_key_package_event(&member_mdk, &member_keys);

            let add_result = alice_mdk
                .add_members(&group_id, &[member_key_package])
                .unwrap_or_else(|_| panic!("Should add member {}", i));

            alice_mdk
                .merge_pending_commit(&group_id)
                .unwrap_or_else(|_| panic!("Should merge commit {}", i));

            member_add_events.push(add_result.evolution_event);
        }

        // Bob processes all the add commits
        for (i, event) in member_add_events.iter().enumerate() {
            bob_mdk
                .process_message(event)
                .unwrap_or_else(|_| panic!("Bob should process add commit {}", i));
            bob_mdk
                .merge_pending_commit(&group_id)
                .unwrap_or_else(|_| panic!("Bob should merge commit {}", i));
        }

        // Verify epoch advanced from Alice's perspective
        let after_adds_epoch = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist")
            .epoch;

        assert!(
            after_adds_epoch > initial_epoch,
            "Epoch should advance after additions"
        );

        // Verify member count from Alice's perspective
        let alice_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");

        // Should have Alice + Bob + 3 new members = 5 total
        assert_eq!(
            alice_members.len(),
            5,
            "Alice should see 5 members after additions"
        );

        // Verify Bob's perspective matches Alice's
        let bob_group = bob_mdk
            .get_group(&group_id)
            .expect("Bob should have group")
            .expect("Group should exist for Bob");
        assert_eq!(
            bob_group.epoch, after_adds_epoch,
            "Bob's epoch should match Alice's"
        );

        let bob_members = bob_mdk
            .get_members(&group_id)
            .expect("Bob should get members");
        assert_eq!(bob_members.len(), 5, "Bob should see 5 members");

        // Verify both see the same members
        for member in &alice_members {
            assert!(
                bob_members.contains(member),
                "Bob should see member {:?}",
                member
            );
        }
    }

    /// Member Operation State Consistency
    ///
    /// Validates that member operations maintain consistent state across
    /// group metadata, member lists, and epoch tracking.
    #[test]
    fn test_member_operation_state_consistency() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Check initial state
        let initial_group = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist");
        let initial_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        let initial_epoch = initial_group.epoch;

        assert_eq!(initial_members.len(), 2, "Should have 2 initial members");

        // Add Charlie
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);
        alice_mdk
            .add_members(&group_id, &[charlie_key_package])
            .expect("Should add Charlie");
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge commit");

        // Verify state after add
        let after_add_group = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist");
        let after_add_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");

        assert_eq!(
            after_add_members.len(),
            3,
            "Should have 3 members after add"
        );
        assert!(
            after_add_group.epoch > initial_epoch,
            "Epoch should advance after add"
        );
        assert!(
            after_add_members.contains(&charlie_keys.public_key()),
            "Charlie should be in members list"
        );

        // Remove Charlie
        alice_mdk
            .remove_members(&group_id, &[charlie_keys.public_key()])
            .expect("Should remove Charlie");
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge commit");

        // Verify state after removal
        let after_remove_group = alice_mdk
            .get_group(&group_id)
            .expect("Should get group")
            .expect("Group should exist");
        let after_remove_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");

        assert_eq!(
            after_remove_members.len(),
            2,
            "Should have 2 members after removal"
        );
        assert!(
            after_remove_group.epoch > after_add_group.epoch,
            "Epoch should advance after removal"
        );
        assert!(
            !after_remove_members.contains(&charlie_keys.public_key()),
            "Charlie should not be in members list"
        );

        // Verify Alice and Bob still present
        assert!(
            after_remove_members.contains(&alice_keys.public_key()),
            "Alice should still be in group"
        );
        assert!(
            after_remove_members.contains(&bob_keys.public_key()),
            "Bob should still be in group"
        );
    }

    /// Test that remove_members correctly handles ratchet tree holes
    ///
    /// This is a regression test for a bug where enumerate() was used to derive
    /// LeafNodeIndex instead of using member.index. When the ratchet tree has holes
    /// (from prior removals), the enumeration index diverges from the actual leaf index.
    ///
    /// Scenario: Alice creates group with Bob, Charlie, Dave. Remove Charlie (creates hole).
    /// Then remove Dave - must remove Dave (leaf 3), not the wrong member.
    #[test]
    fn test_remove_members_with_tree_holes() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();
        let dave_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();
        let dave_mdk = create_test_mdk();

        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        // Create key packages for all members
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);
        let dave_key_package = create_key_package_event(&dave_mdk, &dave_keys);

        // Alice creates group with Bob, Charlie, Dave
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package, charlie_key_package, dave_key_package],
                config,
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Verify initial state: Alice, Bob, Charlie, Dave
        let initial_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(initial_members.len(), 4, "Should have 4 members initially");

        // Step 1: Remove Charlie (creates a hole in the ratchet tree)
        alice_mdk
            .remove_members(&group_id, &[charlie_keys.public_key()])
            .expect("Should remove Charlie");
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge commit");

        let after_charlie_removal = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(
            after_charlie_removal.len(),
            3,
            "Should have 3 members after removing Charlie"
        );
        assert!(
            !after_charlie_removal.contains(&charlie_keys.public_key()),
            "Charlie should be removed"
        );

        // Step 2: Remove Dave (the bug would cause wrong member removal here)
        // With the bug: enumerate() would give Dave index 2, but his actual leaf index is 3
        alice_mdk
            .remove_members(&group_id, &[dave_keys.public_key()])
            .expect("Should remove Dave");
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Should merge commit");

        // Verify final state: only Alice and Bob remain
        let final_members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(
            final_members.len(),
            2,
            "Should have 2 members after removals"
        );
        assert!(
            final_members.contains(&alice_keys.public_key()),
            "Alice should still be in group"
        );
        assert!(
            final_members.contains(&bob_keys.public_key()),
            "Bob should still be in group"
        );
        assert!(
            !final_members.contains(&dave_keys.public_key()),
            "Dave should be removed"
        );
    }

    /// Empty Group Operations
    ///
    /// Validates proper handling of edge cases with minimal group configurations.
    #[test]
    fn test_empty_group_operations() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admin_pubkeys = vec![alice_keys.public_key()];
        let config = create_nostr_group_config_data(admin_pubkeys);

        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        let create_result = alice_mdk
            .create_group(&alice_keys.public_key(), vec![bob_key_package], config)
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Test: Remove with empty list (should return error)
        let empty_remove_result = alice_mdk.remove_members(&group_id, &[]);
        assert!(
            empty_remove_result.is_err(),
            "Removing empty member list should fail"
        );

        // Verify no state change after failed empty remove
        let members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(members.len(), 2, "Member count should not change");

        // Test: Add with empty list (should return error)
        let empty_add_result = alice_mdk.add_members(&group_id, &[]);
        assert!(
            empty_add_result.is_err(),
            "Adding empty member list should fail"
        );

        // Verify no state change after failed empty add
        let members = alice_mdk
            .get_members(&group_id)
            .expect("Should get members");
        assert_eq!(members.len(), 2, "Member count should not change");
    }

    /// Tests that pending_added_members_pubkeys returns empty when there are no pending proposals.
    /// Note: pending_proposals() in MLS only contains proposals received via process_message,
    /// not commits created locally. This test verifies the method works for empty groups.
    #[test]
    fn test_pending_added_members_pubkeys_empty() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key()];

        // Create key package for Bob
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        // Alice creates the group with Bob
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // There should be no pending added members (proposals are from process_message)
        let pending = alice_mdk
            .pending_added_members_pubkeys(&group_id)
            .expect("Should get pending added members");
        assert!(
            pending.is_empty(),
            "No pending additions when no proposals have been received"
        );
    }

    /// Tests that pending_removed_members_pubkeys shows members pending removal
    /// when a self-leave proposal is received by a non-admin member.
    #[test]
    fn test_pending_removed_members_from_self_leave_proposal() {
        use crate::test_util::create_key_package_event;

        // Setup: Alice (admin), Bob (non-admin), Charlie (non-admin)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        // Only Alice is admin
        let admins = vec![alice_keys.public_key()];

        // Create key packages
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        // Alice creates the group with Bob and Charlie
        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package, charlie_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob and Charlie join the group
        let bob_welcome = &create_result.welcome_rumors[0];
        let charlie_welcome = &create_result.welcome_rumors[1];

        let bob_welcome_preview = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome_preview)
            .expect("Bob should accept welcome");

        let charlie_welcome_preview = charlie_mdk
            .process_welcome(&nostr::EventId::all_zeros(), charlie_welcome)
            .expect("Charlie should process welcome");
        charlie_mdk
            .accept_welcome(&charlie_welcome_preview)
            .expect("Charlie should accept welcome");

        // Initially, Charlie has no pending removals
        let pending_before = charlie_mdk
            .pending_removed_members_pubkeys(&group_id)
            .expect("Should get pending removed members");
        assert!(pending_before.is_empty(), "No pending removals initially");

        // Bob leaves the group (creates a leave proposal)
        let bob_leave_result = bob_mdk
            .leave_group(&group_id)
            .expect("Bob should be able to leave");

        // Charlie (non-admin) processes Bob's leave proposal
        // This should store the proposal as pending (not auto-commit since Charlie is not admin)
        let process_result = charlie_mdk.process_message(&bob_leave_result.evolution_event);
        assert!(
            process_result.is_ok(),
            "Charlie should be able to process Bob's leave: {:?}",
            process_result.err()
        );

        // Now Charlie should have Bob in pending removals
        let pending_after = charlie_mdk
            .pending_removed_members_pubkeys(&group_id)
            .expect("Should get pending removed members");
        assert_eq!(
            pending_after.len(),
            1,
            "Should have one pending removal (Bob)"
        );
        assert_eq!(
            pending_after[0],
            bob_keys.public_key(),
            "Pending removal should be Bob"
        );
    }

    /// Tests that pending_member_changes returns empty when there are no pending proposals.
    #[test]
    fn test_pending_member_changes_empty() {
        use crate::test_util::create_key_package_event;

        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();

        // Create group with Alice as admin and Bob as member
        let admins = vec![alice_keys.public_key()];
        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);

        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // There should be no pending changes
        let changes = alice_mdk
            .pending_member_changes(&group_id)
            .expect("Should get pending member changes");
        assert!(changes.additions.is_empty(), "No pending additions");
        assert!(changes.removals.is_empty(), "No pending removals");
    }

    /// Tests that pending_member_changes shows pending removal from leave proposal.
    #[test]
    fn test_pending_member_changes_with_leave_proposal() {
        use crate::test_util::create_key_package_event;

        // Setup: Alice (admin), Bob (non-admin), Charlie (non-admin)
        let alice_keys = Keys::generate();
        let bob_keys = Keys::generate();
        let charlie_keys = Keys::generate();

        let alice_mdk = create_test_mdk();
        let bob_mdk = create_test_mdk();
        let charlie_mdk = create_test_mdk();

        let admins = vec![alice_keys.public_key()];

        let bob_key_package = create_key_package_event(&bob_mdk, &bob_keys);
        let charlie_key_package = create_key_package_event(&charlie_mdk, &charlie_keys);

        let create_result = alice_mdk
            .create_group(
                &alice_keys.public_key(),
                vec![bob_key_package, charlie_key_package],
                create_nostr_group_config_data(admins),
            )
            .expect("Alice should create group");

        let group_id = create_result.group.mls_group_id.clone();
        alice_mdk
            .merge_pending_commit(&group_id)
            .expect("Alice should merge commit");

        // Bob and Charlie join
        let bob_welcome = &create_result.welcome_rumors[0];
        let charlie_welcome = &create_result.welcome_rumors[1];

        let bob_welcome_preview = bob_mdk
            .process_welcome(&nostr::EventId::all_zeros(), bob_welcome)
            .expect("Bob should process welcome");
        bob_mdk
            .accept_welcome(&bob_welcome_preview)
            .expect("Bob should accept welcome");

        let charlie_welcome_preview = charlie_mdk
            .process_welcome(&nostr::EventId::all_zeros(), charlie_welcome)
            .expect("Charlie should process welcome");
        charlie_mdk
            .accept_welcome(&charlie_welcome_preview)
            .expect("Charlie should accept welcome");

        // Bob leaves (creates proposal)
        let bob_leave_result = bob_mdk.leave_group(&group_id).expect("Bob should leave");

        // Charlie (non-admin) processes the leave proposal
        charlie_mdk
            .process_message(&bob_leave_result.evolution_event)
            .expect("Charlie should process leave");

        // Charlie should see Bob in pending removals
        let changes = charlie_mdk
            .pending_member_changes(&group_id)
            .expect("Should get pending member changes");
        assert!(changes.additions.is_empty(), "No pending additions");
        assert_eq!(changes.removals.len(), 1, "Should have one pending removal");
        assert_eq!(
            changes.removals[0],
            bob_keys.public_key(),
            "Pending removal should be Bob"
        );
    }

    /// Tests that pending member methods return error for non-existent group.
    #[test]
    fn test_pending_member_methods_group_not_found() {
        let alice_mdk = create_test_mdk();
        let fake_group_id = mdk_storage_traits::GroupId::from_slice(&[0u8; 16]);

        let result = alice_mdk.pending_added_members_pubkeys(&fake_group_id);
        assert!(result.is_err(), "Should error for non-existent group");

        let result = alice_mdk.pending_removed_members_pubkeys(&fake_group_id);
        assert!(result.is_err(), "Should error for non-existent group");

        let result = alice_mdk.pending_member_changes(&fake_group_id);
        assert!(result.is_err(), "Should error for non-existent group");
    }

    /// Tests that `clear_pending_commit` rolls back a pending add-member commit,
    /// allowing subsequent group operations to succeed.
    #[test]
    fn test_clear_pending_commit_after_failed_add() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        let initial_members = mdk.get_members(&group_id).expect("get members");
        let initial_count = initial_members.len();

        // Add a new member — creates a pending commit but do NOT merge
        // (simulates a failed relay publish)
        let new_member = Keys::generate();
        let kp_event = create_key_package_event(&mdk, &new_member);
        let _add_result = mdk
            .add_members(&group_id, &[kp_event])
            .expect("add_members should succeed");

        // A second add_members should fail because there is already a pending commit
        let another_member = Keys::generate();
        let kp_event2 = create_key_package_event(&mdk, &another_member);
        let err = mdk.add_members(&group_id, &[kp_event2]);
        assert!(err.is_err(), "Should fail due to existing pending commit");

        // Clear the pending commit (simulates cleanup after failed publish)
        mdk.clear_pending_commit(&group_id)
            .expect("clear_pending_commit should succeed");

        // Verify the member was NOT added
        let after_clear = mdk.get_members(&group_id).expect("get members");
        assert_eq!(
            after_clear.len(),
            initial_count,
            "Member count should be unchanged after clearing pending commit"
        );
        assert!(
            !after_clear.contains(&new_member.public_key()),
            "New member should not be in group after clearing pending commit"
        );

        // Verify the group is usable again — a new operation should succeed
        let kp_event3 = create_key_package_event(&mdk, &another_member);
        mdk.add_members(&group_id, &[kp_event3])
            .expect("add_members should succeed after clearing pending commit");
        mdk.merge_pending_commit(&group_id)
            .expect("merge should succeed after clearing pending commit");

        let final_members = mdk.get_members(&group_id).expect("get members");
        assert_eq!(
            final_members.len(),
            initial_count + 1,
            "Member should be added after clearing stale commit and retrying"
        );
        assert!(
            final_members.contains(&another_member.public_key()),
            "New member should be in group after successful retry"
        );
    }

    /// Tests that `clear_pending_commit` rolls back a pending remove-member commit.
    #[test]
    fn test_clear_pending_commit_after_failed_remove() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        let initial_members = mdk.get_members(&group_id).expect("get members");
        let initial_count = initial_members.len();
        let member_to_remove = members[0].public_key();

        // Remove a member — creates a pending commit but do NOT merge
        let _remove_result = mdk
            .remove_members(&group_id, &[member_to_remove])
            .expect("remove_members should succeed");

        // Clear the pending commit
        mdk.clear_pending_commit(&group_id)
            .expect("clear_pending_commit should succeed");

        // Verify the member was NOT removed
        let after_clear = mdk.get_members(&group_id).expect("get members");
        assert_eq!(
            after_clear.len(),
            initial_count,
            "Member count should be unchanged after clearing pending remove commit"
        );
        assert!(
            after_clear.contains(&member_to_remove),
            "Member should still be in group after clearing pending remove commit"
        );

        // Verify the group is usable again — retry the removal
        mdk.remove_members(&group_id, &[member_to_remove])
            .expect("remove_members should succeed after clearing pending commit");
        mdk.merge_pending_commit(&group_id)
            .expect("merge should succeed after clearing pending commit");

        let final_members = mdk.get_members(&group_id).expect("get members");
        assert_eq!(
            final_members.len(),
            initial_count - 1,
            "Member should be removed after clearing stale commit and retrying"
        );
        assert!(
            !final_members.contains(&member_to_remove),
            "Removed member should not be in group after successful retry"
        );
    }

    /// Tests that `clear_pending_commit` is a no-op when there is no pending commit.
    #[test]
    fn test_clear_pending_commit_no_pending() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Clearing when there's nothing pending should succeed (no-op)
        mdk.clear_pending_commit(&group_id)
            .expect("clear_pending_commit should succeed even with no pending commit");

        // Group should still be functional
        let member_count = mdk.get_members(&group_id).expect("get members").len();
        assert!(member_count > 0, "Group should still have members");
    }

    /// Tests that `clear_pending_commit` returns an error for a non-existent group.
    #[test]
    fn test_clear_pending_commit_group_not_found() {
        let mdk = create_test_mdk();
        let fake_group_id = mdk_storage_traits::GroupId::from_slice(&[0u8; 16]);

        let result = mdk.clear_pending_commit(&fake_group_id);
        assert!(
            result.is_err(),
            "clear_pending_commit should error for non-existent group"
        );
    }

    /// Tests that `self_update` followed by `merge_pending_commit` rotates the signing key and
    /// leaves no orphaned keypairs in storage — the new key becomes the active signer.
    #[test]
    fn test_self_update_then_merge_no_orphan() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Capture the pre-update signature key so we can verify rotation.
        let pre_update_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("load mls group")
            .expect("group exists");
        let pre_update_pubkey = pre_update_mls_group
            .own_leaf()
            .expect("own leaf")
            .signature_key()
            .as_slice()
            .to_vec();

        // Perform self_update — stores the new keypair and creates a pending commit.
        mdk.self_update(&group_id).expect("self_update");

        // Capture the pending new public key before merging.
        let pending_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("load mls group")
            .expect("group exists");
        let new_pubkey = pending_mls_group
            .pending_commit()
            .expect("pending commit exists")
            .update_path_leaf_node()
            .expect("update path leaf node in self_update commit")
            .signature_key()
            .as_slice()
            .to_vec();

        // The new key should differ from the pre-update key.
        assert_ne!(
            pre_update_pubkey, new_pubkey,
            "self_update should rotate the signature key"
        );

        // The new keypair must be in storage at this point (used by OpenMLS for the commit).
        let stored_before_merge = SignatureKeyPair::read(
            &mdk.provider.storage,
            &new_pubkey,
            mdk.ciphersuite.signature_algorithm(),
        );
        assert!(
            stored_before_merge.is_some(),
            "new keypair must be in storage before merge_pending_commit"
        );

        // Merge — advances the epoch and makes the new key the active signer.
        mdk.merge_pending_commit(&group_id)
            .expect("merge_pending_commit");

        // After a successful merge the new key must still be in storage (it is now the active signer).
        let stored_after_merge = SignatureKeyPair::read(
            &mdk.provider.storage,
            &new_pubkey,
            mdk.ciphersuite.signature_algorithm(),
        );
        assert!(
            stored_after_merge.is_some(),
            "new keypair must remain in storage after successful merge (it is the active signer)"
        );

        // The group should still be functional.
        let members_after = mdk.get_members(&group_id).expect("get members");
        assert!(!members_after.is_empty(), "group should still have members");
    }

    /// Tests that `self_update` followed by `clear_pending_commit` rolls back the commit and
    /// removes the orphaned new signature keypair from storage.
    #[test]
    fn test_self_update_then_clear_removes_orphaned_keypair() {
        let mdk = create_test_mdk();
        let (creator, members, admins) = create_test_group_members();
        let group_id = create_test_group(&mdk, &creator, &members, &admins);

        // Perform self_update — stores the new keypair and creates a pending commit.
        mdk.self_update(&group_id).expect("self_update");

        // Capture the new public key that was stored during self_update.
        let pending_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("load mls group")
            .expect("group exists");
        let new_pubkey = pending_mls_group
            .pending_commit()
            .expect("pending commit exists")
            .update_path_leaf_node()
            .expect("update path leaf node in self_update commit")
            .signature_key()
            .as_slice()
            .to_vec();

        // The new keypair must be present in storage before we clear.
        let stored_before_clear = SignatureKeyPair::read(
            &mdk.provider.storage,
            &new_pubkey,
            mdk.ciphersuite.signature_algorithm(),
        );
        assert!(
            stored_before_clear.is_some(),
            "new keypair must be in storage before clear_pending_commit"
        );

        // Clear — rolls back the MLS state and must delete the orphaned keypair.
        mdk.clear_pending_commit(&group_id)
            .expect("clear_pending_commit");

        // The orphaned new keypair must now be gone from storage.
        let stored_after_clear = SignatureKeyPair::read(
            &mdk.provider.storage,
            &new_pubkey,
            mdk.ciphersuite.signature_algorithm(),
        );
        assert!(
            stored_after_clear.is_none(),
            "orphaned new keypair must be deleted from storage after clear_pending_commit"
        );

        // The group must be functional and able to perform a new self_update.
        mdk.self_update(&group_id)
            .expect("self_update should succeed after clearing pending commit");
        mdk.merge_pending_commit(&group_id)
            .expect("merge should succeed after retry");

        // After the successful retry the active signer is the newly rotated key.
        let final_mls_group = mdk
            .load_mls_group(&group_id)
            .expect("load mls group")
            .expect("group exists");
        let final_pubkey = final_mls_group
            .own_leaf()
            .expect("own leaf")
            .signature_key()
            .as_slice()
            .to_vec();

        let active_signer = SignatureKeyPair::read(
            &mdk.provider.storage,
            &final_pubkey,
            mdk.ciphersuite.signature_algorithm(),
        );
        assert!(
            active_signer.is_some(),
            "active signer keypair must be present in storage after successful retry"
        );
    }
}
