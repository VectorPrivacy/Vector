// Copyright (c) 2024-2025 MDK Developers
// Distributed under the MIT software license

//! Key Package Inspection Example
//!
//! This example demonstrates how to create and inspect MLS key packages for Nostr.
//! It creates a key package, displays the tags that would be used in a Nostr event,
//! and then parses the key package back to inspect the internal OpenMLS KeyPackage object.

use mdk_core::Error;
use mdk_core::prelude::*;
use mdk_memory_storage::MdkMemoryStorage;
use nostr::event::builder::EventBuilder;
use nostr::{Keys, Kind, RelayUrl};
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

    println!("\n=== MLS Key Package Inspection Example ===\n");

    // Initialize MDK with in-memory storage
    let mdk = MDK::new(MdkMemoryStorage::default());
    let keys = Keys::generate();
    let relay_url = RelayUrl::parse("wss://relay.example.com").unwrap();

    println!("Generated Nostr identity:");
    println!("  Public Key: {}", keys.public_key());
    println!();

    // ====================================
    // Step 1: Create a key package
    // ====================================
    println!("=== Creating Key Package ===\n");

    // Create key package with protected=true to demonstrate NIP-70 tag
    let (key_package_encoded, tags, _hash_ref) = mdk.create_key_package_for_event_with_options(
        &keys.public_key(),
        [relay_url.clone()],
        true,
    )?;

    println!("Key Package Created Successfully!");
    println!("  Encoded length: {} bytes", key_package_encoded.len());
    println!("  First 64 chars: {}...", &key_package_encoded[..64]);
    println!();

    // ====================================
    // Step 2: Inspect the tags
    // ====================================
    println!("=== Inspecting Tags ===\n");
    println!("Tags that would be added to the Nostr event:\n");

    for (i, tag) in tags.iter().enumerate() {
        println!("Tag {}:", i + 1);
        println!("  Kind: {:?}", tag.kind());
        let tag_parts: Vec<String> = tag
            .as_slice()
            .iter()
            .map(|s| format!("\"{}\"", s))
            .collect();
        println!("  Full tag: [{}]", tag_parts.join(", "));
        println!();
    }

    // ====================================
    // Step 3: Create the actual Nostr event
    // ====================================
    println!("=== Creating Nostr Event ===\n");

    let key_package_event = EventBuilder::new(Kind::MlsKeyPackage, key_package_encoded.clone())
        .tags(tags)
        .build(keys.public_key())
        .sign(&keys)
        .await?;

    println!("Event Details:");
    println!("  Event ID: {}", key_package_event.id);
    println!("  Kind: {:?}", key_package_event.kind);
    println!("  Author: {}", key_package_event.pubkey);
    println!("  Created at: {}", key_package_event.created_at);
    println!("  Number of tags: {}", key_package_event.tags.len());
    println!();

    // ====================================
    // Step 4: Parse and inspect the KeyPackage
    // ====================================
    println!("=== Parsing and Inspecting KeyPackage ===\n");

    let key_package = mdk.parse_key_package(&key_package_event)?;

    println!("✓ Key package parsed and validated successfully!\n");

    // Inspect the key package internals
    println!("KeyPackage Details:");
    println!("  Ciphersuite: {:?}", key_package.ciphersuite());
    println!();

    // Inspect the leaf node
    println!("Leaf Node Information:");
    let leaf_node = key_package.leaf_node();
    println!("  Encryption Key: {:?}", leaf_node.encryption_key());
    println!();

    // Inspect credential
    println!("Credential Information:");
    let credential = leaf_node.credential();
    println!("  Credential Type: {:?}", credential.credential_type());
    println!();

    // Inspect capabilities
    println!("Capabilities:");
    let capabilities = leaf_node.capabilities();
    println!("  Versions: {:?}", capabilities.versions());
    println!("  Ciphersuites: {:?}", capabilities.ciphersuites());
    println!("  Extensions: {:?}", capabilities.extensions());
    println!("  Proposals: {:?}", capabilities.proposals());
    println!("  Credentials: {:?}", capabilities.credentials());
    println!();

    // Inspect leaf node extensions
    println!("Leaf Node Extensions:");
    let leaf_extensions = leaf_node.extensions();
    let mut has_last_resort = false;
    let mut extension_count = 0;

    for ext in leaf_extensions.iter() {
        extension_count += 1;
        println!("  Extension {}:", extension_count);
        println!("    Type: {:?}", ext.extension_type());
        match ext {
            Extension::LastResort(_) => {
                has_last_resort = true;
                println!("    Data: LastResort (no additional data)");
            }
            Extension::ApplicationId(app_id) => {
                println!(
                    "    Data: ApplicationId = {}",
                    hex::encode(app_id.as_slice())
                );
            }
            Extension::RequiredCapabilities(req_caps) => {
                println!("    Data: RequiredCapabilities");
                println!("      Extension types: {:?}", req_caps.extension_types());
                println!("      Proposal types: {:?}", req_caps.proposal_types());
                println!("      Credential types: {:?}", req_caps.credential_types());
            }
            _ => {
                println!("    Data: Other extension type");
            }
        }
        println!();
    }

    if extension_count == 0 {
        println!("  (No extensions found in leaf node)");
        println!();
    }

    // Check key package extensions (separate from leaf node extensions)
    println!("Key Package Extensions:");
    let kp_extensions = key_package.extensions();
    let mut kp_extension_count = 0;
    let mut kp_has_last_resort = false;

    for ext in kp_extensions.iter() {
        kp_extension_count += 1;
        println!("  Extension {}:", kp_extension_count);
        println!("    Type: {:?}", ext.extension_type());
        match ext {
            Extension::LastResort(_) => {
                kp_has_last_resort = true;
                println!("    Data: LastResort (marks this as a last resort key package)");
            }
            Extension::ApplicationId(app_id) => {
                println!(
                    "    Data: ApplicationId = {}",
                    hex::encode(app_id.as_slice())
                );
            }
            Extension::RequiredCapabilities(req_caps) => {
                println!("    Data: RequiredCapabilities");
                println!("      Extension types: {:?}", req_caps.extension_types());
                println!("      Proposal types: {:?}", req_caps.proposal_types());
                println!("      Credential types: {:?}", req_caps.credential_types());
            }
            _ => {
                println!("    Data: Other extension type");
            }
        }
        println!();
    }

    if kp_extension_count == 0 {
        println!("  (No extensions found in key package)");
        println!();
    }

    // Check if it's a last resort key package
    println!("Special Properties:");
    println!("  Is Last Resort (from leaf node): {}", has_last_resort);
    println!(
        "  Is Last Resort (from key package): {}",
        kp_has_last_resort
    );
    println!();

    // ====================================
    // Step 5: Raw serialized data inspection
    // ====================================
    println!("=== Raw Serialized Data ===\n");

    let key_package_bytes = hex::decode(&key_package_encoded)?;
    println!("Serialized Key Package:");
    println!("  Total bytes: {}", key_package_bytes.len());
    println!(
        "  First 32 bytes (hex): {}",
        hex::encode(&key_package_bytes[..32.min(key_package_bytes.len())])
    );
    println!(
        "  Last 32 bytes (hex): {}",
        hex::encode(&key_package_bytes[key_package_bytes.len().saturating_sub(32)..])
    );
    println!();

    // ====================================
    // Step 6: Verify tag validation
    // ====================================
    println!("=== Verifying Tag Validation ===\n");

    // The parse_key_package method already validates tags internally
    println!("✓ All tag validations passed during parsing!");
    println!("  - mls_protocol_version tag present and valid");
    println!("  - mls_ciphersuite tag present and valid (0x0001)");
    println!("  - mls_extensions tag present and valid");
    println!("  - relay tag present");
    println!();

    // ====================================
    // Step 7: Verify protected tag
    // ====================================
    println!("=== Verifying Protected Tag ===\n");

    // Verify that the protected tag is present in the event
    let protected_tag = key_package_event
        .tags
        .iter()
        .find(|tag| matches!(tag.kind(), nostr::TagKind::Protected));

    match protected_tag {
        Some(tag) => {
            println!("✓ Protected tag found!");
            println!("  Tag kind: {:?}", tag.kind());
            println!("  Tag content: {:?}", tag.as_slice());
            println!();
            println!("The protected tag marks this event as protected per NIP-70.");
            println!("This ensures the event content cannot be modified by relays.");
        }
        None => {
            println!("✗ WARNING: Protected tag not found!");
            println!("  This key package event should have a protected tag.");
        }
    }
    println!();

    println!("=== Example Complete ===\n");
    println!("This example demonstrated:");
    println!("  1. Creating a key package with create_key_package_for_event()");
    println!("  2. Inspecting the tags generated for the Nostr event");
    println!("  3. Creating and signing a complete Nostr event");
    println!("  4. Parsing and validating the key package from the event");
    println!("  5. Inspecting the internal OpenMLS KeyPackage structure");
    println!("     - Ciphersuite and capabilities");
    println!("     - Leaf node details (encryption key, credential)");
    println!("     - Extensions (both leaf node and key package level)");
    println!("     - Last resort status");
    println!("  6. Raw serialized data inspection");
    println!("  7. Tag validation verification (protocol version, ciphersuite, extensions)");
    println!("  8. Protected tag verification (NIP-70 compliance)");
    println!();

    Ok(())
}
