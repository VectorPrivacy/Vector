// Copyright (c) 2024-2025 MDK Developers
// Distributed under the MIT software license

//! Group Inspection Example
//!
//! This example demonstrates how to create and inspect MLS groups for Nostr.
//! It creates a group with members, displays the group metadata and extensions,
//! and then inspects the internal OpenMLS MlsGroup object.
//!
//! ## Running this example
//!
//! This example requires the `debug-examples` feature flag to access internal
//! inspection methods. Run it with:
//!
//! ```bash
//! cargo run --example group_inspection --features debug-examples
//! ```

#![cfg(feature = "debug-examples")]

use mdk_core::Error;
use mdk_core::prelude::*;
use mdk_memory_storage::MdkMemoryStorage;
use nostr::{Keys, RelayUrl};
use openmls::prelude::*;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Set up logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    println!("\n=== MLS Group Inspection Example ===\n");

    // Initialize MDK with in-memory storage
    let mdk = MDK::new(MdkMemoryStorage::default());

    // ====================================
    // Step 1: Set up identities
    // ====================================
    println!("=== Setting Up Identities ===\n");

    let creator_keys = Keys::generate();
    let member1_keys = Keys::generate();
    let member2_keys = Keys::generate();

    println!("Generated identities:");
    println!("  Creator:  {}", creator_keys.public_key());
    println!("  Member 1: {}", member1_keys.public_key());
    println!("  Member 2: {}", member2_keys.public_key());
    println!();

    // ====================================
    // Step 2: Create key packages for members
    // ====================================
    println!("=== Creating Key Packages ===\n");

    let relay_url = RelayUrl::parse("wss://relay.example.com").unwrap();

    // Create key packages for members (not creator)
    let (member1_kp_encoded, member1_tags, _) =
        mdk.create_key_package_for_event(&member1_keys.public_key(), [relay_url.clone()])?;

    let member1_event =
        nostr::event::builder::EventBuilder::new(nostr::Kind::MlsKeyPackage, member1_kp_encoded)
            .tags(member1_tags)
            .build(member1_keys.public_key())
            .sign(&member1_keys)
            .await?;

    let (member2_kp_encoded, member2_tags, _) =
        mdk.create_key_package_for_event(&member2_keys.public_key(), [relay_url.clone()])?;

    let member2_event =
        nostr::event::builder::EventBuilder::new(nostr::Kind::MlsKeyPackage, member2_kp_encoded)
            .tags(member2_tags)
            .build(member2_keys.public_key())
            .sign(&member2_keys)
            .await?;

    println!("Created key packages for {} members", 2);
    println!();

    // ====================================
    // Step 3: Create the group
    // ====================================
    println!("=== Creating Group ===\n");

    let group_config = mdk_core::groups::NostrGroupConfigData {
        name: "Example Group".to_string(),
        description: "A group for demonstrating MLS inspection".to_string(),
        image_hash: None,
        image_key: None,
        image_nonce: None,
        relays: vec![relay_url.clone()],
        admins: vec![creator_keys.public_key()], // Creator is admin
    };

    let group_result = mdk.create_group(
        &creator_keys.public_key(),
        vec![member1_event.clone(), member2_event.clone()],
        group_config,
    )?;

    println!("Group Created Successfully!");
    println!("  Name: {}", group_result.group.name);
    println!("  Description: {}", group_result.group.description);
    println!("  MLS Group ID: {:?}", group_result.group.mls_group_id);
    println!(
        "  Nostr Group ID: {}",
        hex::encode(group_result.group.nostr_group_id)
    );
    println!("  Epoch: {}", group_result.group.epoch);
    println!("  State: {:?}", group_result.group.state);
    println!(
        "  Welcome events created: {}",
        group_result.welcome_rumors.len()
    );
    println!();

    // ====================================
    // Step 4: Inspect group membership
    // ====================================
    println!("=== Inspecting Group Membership ===\n");

    let group_id = &group_result.group.mls_group_id;
    let members = mdk.get_members(group_id)?;

    println!("Group Members ({}): ", members.len());
    for (i, member) in members.iter().enumerate() {
        let is_creator = *member == creator_keys.public_key();
        let is_admin = group_result.group.admin_pubkeys.contains(member);
        println!(
            "  {}. {} {}{}",
            i + 1,
            member,
            if is_creator { "[Creator] " } else { "" },
            if is_admin { "[Admin]" } else { "" }
        );
    }
    println!();

    // ====================================
    // Step 5: Inspect group relays
    // ====================================
    println!("=== Inspecting Group Relays ===\n");

    let relays = mdk.get_relays(group_id)?;
    println!("Group Relays ({}):", relays.len());
    for (i, relay) in relays.iter().enumerate() {
        println!("  {}. {}", i + 1, relay);
    }
    println!();

    // ====================================
    // Step 6: Inspect stored group metadata
    // ====================================
    println!("=== Inspecting Stored Group Metadata ===\n");

    let stored_group = mdk.get_group(group_id)?.expect("Group should exist");

    println!("Stored Group Metadata:");
    println!("  Name: {}", stored_group.name);
    println!("  Description: {}", stored_group.description);
    println!("  Epoch: {}", stored_group.epoch);
    println!("  State: {:?}", stored_group.state);
    println!(
        "  Nostr Group ID: {}",
        hex::encode(stored_group.nostr_group_id)
    );
    println!("  MLS Group ID: {:?}", stored_group.mls_group_id);
    println!();

    // ====================================
    // Step 7: Inspect admin permissions
    // ====================================
    println!("=== Inspecting Admin Permissions ===\n");

    println!("Group Admins ({}):", stored_group.admin_pubkeys.len());
    for (i, admin) in stored_group.admin_pubkeys.iter().enumerate() {
        let is_creator = *admin == creator_keys.public_key();
        println!(
            "  {}. {} {}",
            i + 1,
            admin,
            if is_creator { "[Creator]" } else { "" }
        );
    }
    println!();

    // ====================================
    // Step 8: Inspect group images (if any)
    // ====================================
    println!("=== Inspecting Group Images ===\n");

    if let Some(image_hash) = stored_group.image_hash {
        println!("Group has an image:");
        println!("  Image Hash: {}", hex::encode(image_hash));

        if let Some(image_key) = stored_group.image_key {
            println!("  Image Key: {}", hex::encode(*image_key));
        }

        if let Some(image_nonce) = stored_group.image_nonce {
            println!("  Image Nonce: {}", hex::encode(*image_nonce));
        }
    } else {
        println!("  No group image configured");
    }
    println!();

    // ====================================
    // Step 9: Load and inspect the MLS group internals
    // ====================================
    println!("=== Inspecting MLS Group Internals ===\n");

    let mls_group = mdk.load_mls_group(group_id)?.expect("Group should exist");

    println!("MLS Group Details:");
    println!("  Group ID: {:?}", mls_group.group_id());
    println!("  Epoch: {}", mls_group.epoch().as_u64());
    println!("  Ciphersuite: {:?}", mls_group.ciphersuite());
    println!("  Own leaf index: {:?}", mls_group.own_leaf_index());
    println!();

    // ====================================
    // Step 10: Inspect the own leaf node
    // ====================================
    println!("=== Inspecting Own Leaf Node ===\n");

    let own_leaf = mls_group.own_leaf().expect("Should have own leaf");
    println!("Leaf Node Information:");
    println!("  Encryption Key: {:?}", own_leaf.encryption_key());
    println!("  Signature Key: {:?}", own_leaf.signature_key());
    println!();

    // Inspect credential
    println!("Credential:");
    let credential = own_leaf.credential();
    println!("  Type: {:?}", credential.credential_type());
    if let Ok(basic_cred) = BasicCredential::try_from(credential.clone()) {
        let identity = basic_cred.identity();
        println!("  Identity (hex): {}", hex::encode(identity));
    }
    println!();

    // Inspect capabilities
    println!("Capabilities:");
    let capabilities = own_leaf.capabilities();
    println!("  Versions: {:?}", capabilities.versions());
    println!("  Ciphersuites: {:?}", capabilities.ciphersuites());
    println!("  Extensions: {:?}", capabilities.extensions());
    println!("  Proposals: {:?}", capabilities.proposals());
    println!("  Credentials: {:?}", capabilities.credentials());
    println!();

    // Inspect leaf node extensions
    println!("Leaf Node Extensions:");
    let leaf_extensions = own_leaf.extensions();
    let mut leaf_ext_count = 0;

    for ext in leaf_extensions.iter() {
        leaf_ext_count += 1;
        println!("  Extension {}:", leaf_ext_count);
        println!("    Type: {:?}", ext.extension_type());
        match ext {
            Extension::LastResort(_) => {
                println!("    Data: LastResort (marks this as a last resort key package)");
            }
            Extension::ApplicationId(app_id) => {
                println!(
                    "    Data: ApplicationId = {}",
                    hex::encode(app_id.as_slice())
                );
            }
            _ => {
                println!("    Data: Other extension");
            }
        }
        println!();
    }

    if leaf_ext_count == 0 {
        println!("  (No extensions in leaf node)");
        println!();
    }

    // ====================================
    // Step 11: Inspect group context extensions
    // ====================================
    println!("=== Inspecting Group Context Extensions ===\n");

    let extensions = mls_group.extensions();
    let mut extension_count = 0;

    for ext in extensions.iter() {
        extension_count += 1;
        println!("Extension {}:", extension_count);
        println!("  Type: {:?}", ext.extension_type());

        match ext {
            Extension::Unknown(ext_type, data) => {
                println!("  Extension Type Code: {:?}", ext_type);
                println!("  Data length: {} bytes", data.0.len());
                println!(
                    "  Data (first 64 bytes): {}",
                    hex::encode(&data.0[..data.0.len().min(64)])
                );
            }
            Extension::RequiredCapabilities(req_caps) => {
                println!("  Data: RequiredCapabilities");
                println!("    Extension types: {:?}", req_caps.extension_types());
                println!("    Proposal types: {:?}", req_caps.proposal_types());
                println!("    Credential types: {:?}", req_caps.credential_types());
            }
            Extension::RatchetTree(_) => {
                println!("  Data: RatchetTree (contains full tree structure)");
            }
            _ => {
                println!("  Data: Other extension type");
            }
        }
        println!();
    }

    if extension_count == 0 {
        println!("  (No extensions found in group context)");
        println!();
    }

    // ====================================
    // Step 11b: Parse NostrGroupDataExtension
    // ====================================
    println!("=== Parsing NostrGroupDataExtension ===\n");

    if let Ok(group_data) = mdk_core::extension::NostrGroupDataExtension::from_group(&mls_group) {
        println!("✓ Successfully parsed NostrGroupDataExtension:");
        println!("  Version: {}", group_data.version);
        println!("  Name: {}", group_data.name);
        println!("  Description: {}", group_data.description);
        println!(
            "  Nostr Group ID: {}",
            hex::encode(group_data.nostr_group_id)
        );
        println!("  Admins ({}):", group_data.admins.len());
        for (i, admin) in group_data.admins.iter().enumerate() {
            println!("    {}. {}", i + 1, admin);
        }
        println!("  Relays ({}):", group_data.relays.len());
        for (i, relay) in group_data.relays.iter().enumerate() {
            println!("    {}. {}", i + 1, relay);
        }
        if let Some(hash) = group_data.image_hash {
            println!("  Image Hash: {}", hex::encode(hash));
        }
        if let Some(key) = group_data.image_key {
            println!("  Image Key: {}", hex::encode(key));
        }
        if let Some(nonce) = group_data.image_nonce {
            println!("  Image Nonce: {}", hex::encode(nonce));
        }
    } else {
        println!("✗ Failed to parse NostrGroupDataExtension");
    }
    println!();

    // ====================================
    // Step 12: Inspect all members in the ratchet tree
    // ====================================
    println!("=== Inspecting All Members in Ratchet Tree ===\n");

    let member_list: Vec<_> = mls_group.members().collect();
    println!("Members in MLS tree ({}):", member_list.len());

    for (idx, member) in member_list.iter().enumerate() {
        println!("  Member {}:", idx + 1);
        println!("    Leaf Index: {}", member.index);
        println!(
            "    Credential type: {:?}",
            member.credential.credential_type()
        );

        // Try to extract public key
        if let Ok(basic_cred) = BasicCredential::try_from(member.credential.clone()) {
            let identity = basic_cred.identity();
            println!("    Identity length: {} bytes", identity.len());
            if identity.len() == 32 {
                let pubkey_hex = hex::encode(identity);
                println!("    Public key: {}", pubkey_hex);

                // Check if this is the creator or an admin
                let is_creator = pubkey_hex == creator_keys.public_key().to_hex();
                let is_admin = stored_group
                    .admin_pubkeys
                    .iter()
                    .any(|pk| pk.to_hex() == pubkey_hex);

                if is_creator {
                    println!("    Role: Creator, Admin");
                } else if is_admin {
                    println!("    Role: Admin");
                } else {
                    println!("    Role: Member");
                }
            }
        }
        println!();
    }

    // ====================================
    // Step 13: Inspect welcome event structure
    // ====================================
    println!("=== Inspecting Welcome Event Structure ===\n");

    println!(
        "Welcome Events Created: {}",
        group_result.welcome_rumors.len()
    );
    println!("Note: One welcome event (rumor) is created for each member added to the group");
    println!();

    for (idx, welcome_rumor) in group_result.welcome_rumors.iter().enumerate() {
        println!("Welcome Event {}:", idx + 1);
        println!("  Kind: {:?}", welcome_rumor.kind);
        println!("  Public Key: {}", welcome_rumor.pubkey);
        println!("  Created At: {:?}", welcome_rumor.created_at);
        println!(
            "  Content length: {} bytes (hex-encoded MLS Welcome message)",
            welcome_rumor.content.len()
        );
        println!(
            "  Content preview: {}...",
            welcome_rumor.content.chars().take(64).collect::<String>()
        );

        println!("  Tags ({}):", welcome_rumor.tags.len());
        for (tag_idx, tag) in welcome_rumor.tags.iter().enumerate() {
            println!("    Tag {}: {:?}", tag_idx + 1, tag);
        }
        println!();
    }

    println!("Welcome Event Structure:");
    println!("  - Kind: Kind::MlsWelcome (444)");
    println!("  - Content: Hex-encoded serialized MLS Welcome message");
    println!("  - Tags:");
    println!("    * 'relays' tag: Group relay URLs");
    println!("    * 'e' tag: References the recipient's key package event");
    println!("    * 'client' tag: MDK version identifier");
    println!("  - Created At: Timestamp when the group was created/updated");
    println!("  - Public Key: The committer's public key (group creator or updater)");
    println!();

    // ====================================
    // Step 14: Inspect pending proposals
    // ====================================
    println!("=== Inspecting Pending Proposals ===\n");

    let pending_proposals = mls_group.pending_proposals();
    let proposal_count = pending_proposals.count();
    println!("Pending Proposals: {}", proposal_count);
    if proposal_count == 0 {
        println!("  (No pending proposals in this group)");
    }
    println!();

    // ====================================
    // Summary
    // ====================================
    println!("=== Example Complete ===\n");
    println!("This example demonstrated:");
    println!("  1. Setting up multiple Nostr identities");
    println!("  2. Creating key packages for group members");
    println!("  3. Creating a new MLS group with initial members");
    println!("  4. Inspecting group metadata:");
    println!("     - Name, description, and state");
    println!("     - MLS Group ID and Nostr Group ID");
    println!("     - Current epoch");
    println!("  5. Examining group membership (who's in the group)");
    println!("  6. Inspecting group relays (where messages are published)");
    println!("  7. Viewing admin permissions");
    println!("  8. Checking for group images and encryption metadata");
    println!("  9. Deep inspection of MLS group internals:");
    println!("     - Group ID, epoch, and ciphersuite");
    println!("     - Own leaf node details (encryption/signature keys)");
    println!("     - Credentials and capabilities");
    println!("     - Leaf node extensions");
    println!(" 10. Inspecting group context extensions:");
    println!("     - NostrGroupDataExtension (custom protocol extension)");
    println!("     - RequiredCapabilities extension");
    println!("     - RatchetTree extension");
    println!(" 11. Examining all members in the ratchet tree");
    println!(" 12. Inspecting welcome event structure:");
    println!("     - Kind, public key, timestamp");
    println!("     - Content (hex-encoded MLS Welcome message)");
    println!("     - Tags (relays, event references, client info)");
    println!(" 13. Checking for pending proposals");
    println!();

    Ok(())
}
