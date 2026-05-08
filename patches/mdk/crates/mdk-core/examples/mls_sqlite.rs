// Copyright (c) 2024-2025 MDK Developers
// Distributed under the MIT software license

use mdk_core::prelude::*;
use mdk_core::{Error, messages::MessageProcessingResult};
use mdk_sqlite_storage::MdkSqliteStorage;
use mdk_storage_traits::test_utils::crypto_utils::generate_random_bytes;
use nostr::event::builder::EventBuilder;
use nostr::{EventId, Keys, Kind, RelayUrl, TagKind};
use openmls::key_packages::KeyPackage;
use tempfile::TempDir;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;

/// Generate a new identity and return the keys, MDK instance, and temp directory
/// We use a different temp directory for each identity because OpenMLS doesn't have a concept of partitioning storage for different identities.
/// Because of this, we need to create diffrent databases for each identity.
fn generate_identity() -> (Keys, MDK<MdkSqliteStorage>, TempDir) {
    let keys = Keys::generate();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let db_path = temp_dir.path().join("mls.db");
    let mdk = MDK::new(MdkSqliteStorage::new_unencrypted(db_path).unwrap());
    (keys, mdk, temp_dir)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let relay_url = RelayUrl::parse("ws://localhost:8080").unwrap();

    let (alice_keys, alice_mdk, alice_temp_dir) = generate_identity();
    tracing::info!("Alice identity generated");
    let (bob_keys, bob_mdk, bob_temp_dir) = generate_identity();
    tracing::info!("Bob identity generated");

    // Create key package for Bob
    // This would be published to the Nostr network for other users to find
    let (bob_key_package_encoded, tags, _) =
        bob_mdk.create_key_package_for_event(&bob_keys.public_key(), [relay_url.clone()])?;

    let bob_key_package_event = EventBuilder::new(Kind::MlsKeyPackage, bob_key_package_encoded)
        .tags(tags)
        .build(bob_keys.public_key())
        .sign(&bob_keys)
        .await?;

    // ================================
    // We're now acting as Alice
    // ================================

    // To create a group, Alice fetches Bob's key package from the Nostr network and parses it
    let _bob_key_package: KeyPackage = alice_mdk.parse_key_package(&bob_key_package_event)?;

    let image_hash: [u8; 32] = generate_random_bytes(32).try_into().unwrap();
    let image_key = generate_random_bytes(32).try_into().unwrap();
    let image_nonce = generate_random_bytes(12).try_into().unwrap();
    let name = "Bob & Alice".to_owned();
    let description = "A secret chat between Bob and Alice".to_owned();

    let config = NostrGroupConfigData::new(
        name,
        description,
        Some(image_hash),
        Some(image_key),
        Some(image_nonce),
        vec![relay_url.clone()],
        vec![alice_keys.public_key(), bob_keys.public_key()],
    );

    // Alice creates the group, adding Bob.
    let group_create_result = alice_mdk.create_group(
        &alice_keys.public_key(),
        vec![bob_key_package_event.clone()],
        config,
    )?;

    tracing::info!("Group created");

    // The group is created, and the welcome messages are in welcome_rumors.
    // We also have the Nostr group data, which we can use to show info about the group.
    let alice_group = group_create_result.group;
    let welcome_rumors = group_create_result.welcome_rumors;

    // Alice now creates a Kind: 444 event that is Gift-wrapped to just Bob with the welcome event in the rumor event.
    // If you added multiple users to the group, you'd create a separate gift-wrapped welcome event for each user.
    let welcome_rumor = welcome_rumors
        .first()
        .expect("Should have at least one welcome rumor");

    // Now, let's also try sending a message to the group (using an unsigned Kind: 9 event)
    // We don't have to wait for Bob to join the group before we send our first message.
    let rumor = EventBuilder::new(Kind::Custom(9), "Hi Bob!").build(alice_keys.public_key());
    let message_event = alice_mdk.create_message(&alice_group.mls_group_id, rumor.clone())?;
    // Alice would now publish the message_event to the Nostr network.
    tracing::info!("Message inner event created: {:?}", rumor);
    tracing::debug!("Message wrapper event created: {:?}", message_event);

    // ================================
    // We're now acting as Bob
    // ================================

    // First Bob recieves the Gift-wrapped welcome message from Alice, decrypts it, and processes it.
    // The first param is the gift-wrap event id (which we set as all zeros for this example)
    bob_mdk.process_welcome(&EventId::all_zeros(), welcome_rumor)?;
    // Bob can now preview the welcome message to see what group he might be joining
    let welcomes = bob_mdk
        .get_pending_welcomes(None)
        .expect("Error getting pending welcomes");
    let welcome = welcomes.first().unwrap();

    tracing::debug!("Welcome for Bob: {:?}", welcome);

    assert_eq!(
        welcome.member_count as usize,
        alice_mdk
            .get_members(&alice_group.mls_group_id)
            .unwrap()
            .len(),
        "Welcome message group member count should match the group member count"
    );
    assert_eq!(
        welcome.group_name, "Bob & Alice",
        "Welcome message group name should be Bob & Alice"
    );

    // Bob can now join the group
    bob_mdk.accept_welcome(welcome)?;
    let bobs_group = bob_mdk.get_groups()?.first().unwrap().clone();
    let bob_mls_group_id = &bobs_group.mls_group_id;

    tracing::info!("Bob joined group");

    assert_eq!(
        bob_mdk
            .get_groups()
            .unwrap()
            .first()
            .unwrap()
            .nostr_group_id,
        alice_group.nostr_group_id,
        "Bob's group should have the same Nostr group ID as Alice's group"
    );

    assert_eq!(
        hex::encode(
            bob_mdk
                .get_groups()
                .unwrap()
                .first()
                .unwrap()
                .nostr_group_id
        ),
        message_event
            .tags
            .iter()
            .find(|tag| tag.kind() == TagKind::h())
            .unwrap()
            .content()
            .unwrap(),
        "Bob's group should have the same Nostr group ID as Alice's message wrapper event"
    );
    // Bob and Alice now have synced state for the group.
    assert_eq!(
        bob_mdk.get_members(bob_mls_group_id).unwrap().len(),
        alice_mdk
            .get_members(&alice_group.mls_group_id)
            .unwrap()
            .len(),
        "Groups should have 2 members"
    );
    assert_eq!(
        bobs_group.name, "Bob & Alice",
        "Group name should be Bob & Alice"
    );

    tracing::info!("Bob about to process message");

    // The resulting serialized message is the MLS encrypted message that Bob sent
    // Now Bob can process the MLS message content and do what's needed with it
    bob_mdk.process_message(&message_event)?;

    tracing::info!("Bob processed message");
    let messages = bob_mdk
        .get_messages(bob_mls_group_id, None)
        .map_err(|e| Error::Message(e.to_string()))?;
    tracing::info!("Bob got messages: {:?}", messages);
    let message = messages.first().unwrap();
    tracing::info!("Bob processed message: {:?}", message);

    assert_eq!(
        message.kind,
        Kind::Custom(9),
        "Message event kind should be Custom(9)"
    );
    assert_eq!(
        message.pubkey,
        alice_keys.public_key(),
        "Message event pubkey should be Alice's pubkey"
    );
    assert_eq!(
        message.content, "Hi Bob!",
        "Message event content should be Hi Bob!"
    );

    assert_eq!(
        alice_mdk.get_groups().unwrap().len(),
        1,
        "Alice should have 1 group"
    );

    assert_eq!(
        alice_mdk
            .get_messages(&alice_group.mls_group_id, None)
            .unwrap()
            .len(),
        1,
        "Alice should have 1 message"
    );

    assert_eq!(
        bob_mdk.get_groups().unwrap().len(),
        1,
        "Bob should have 1 group"
    );

    assert_eq!(
        bob_mdk
            .get_messages(&bobs_group.mls_group_id, None)
            .unwrap()
            .len(),
        1,
        "Bob should have 1 message"
    );

    tracing::info!("Alice about to process message");
    alice_mdk.process_message(&message_event)?;

    let messages = alice_mdk
        .get_messages(&alice_group.mls_group_id, None)
        .unwrap();
    let message = messages.first().unwrap();
    tracing::info!("Alice processed message: {:?}", message);

    // ================================
    // Extended functionality: Adding Charlie
    // ================================

    let (charlie_keys, charlie_mdk, charlie_temp_dir) = generate_identity();
    tracing::info!("Charlie identity generated");

    // Create key package for Charlie
    let (charlie_key_package_encoded, charlie_tags, _) = charlie_mdk
        .create_key_package_for_event(&charlie_keys.public_key(), [relay_url.clone()])?;

    let charlie_key_package_event =
        EventBuilder::new(Kind::MlsKeyPackage, charlie_key_package_encoded)
            .tags(charlie_tags)
            .build(charlie_keys.public_key())
            .sign(&charlie_keys)
            .await?;

    // Alice adds Charlie to the group
    tracing::info!("Alice adding Charlie to the group");
    let add_charlie_result = alice_mdk.add_members(
        &alice_group.mls_group_id,
        std::slice::from_ref(&charlie_key_package_event),
    )?;

    // Alice publishes the add commit message and Bob processes it
    tracing::info!("Bob processing Charlie addition commit");
    let add_commit_result = bob_mdk.process_message(&add_charlie_result.evolution_event);
    tracing::info!("Add commit processing result: {:?}", add_commit_result);

    // Alice merges the pending commit for adding Charlie
    alice_mdk.merge_pending_commit(&alice_group.mls_group_id)?;

    // Charlie processes the welcome message
    if let Some(welcome_rumors) = add_charlie_result.welcome_rumors {
        let charlie_welcome_rumor = welcome_rumors
            .first()
            .expect("Should have welcome rumor for Charlie");
        charlie_mdk.process_welcome(&EventId::all_zeros(), charlie_welcome_rumor)?;

        let charlie_welcomes = charlie_mdk
            .get_pending_welcomes(None)
            .expect("Error getting Charlie's pending welcomes");
        let charlie_welcome = charlie_welcomes.first().unwrap();
        charlie_mdk.accept_welcome(charlie_welcome)?;

        tracing::info!("Charlie joined the group");

        // Verify Charlie is in the group
        let group_members = alice_mdk.get_members(&alice_group.mls_group_id)?;
        assert_eq!(group_members.len(), 3, "Group should now have 3 members");
        assert!(
            group_members.contains(&charlie_keys.public_key()),
            "Charlie should be in the group"
        );
    }

    // ================================
    // Removing Charlie from the group
    // ================================

    tracing::info!("Alice removing Charlie from the group");
    let remove_charlie_result =
        alice_mdk.remove_members(&alice_group.mls_group_id, &[charlie_keys.public_key()])?;

    // Bob processes the remove commit message
    tracing::info!("Bob processing Charlie removal commit");
    let remove_commit_result = bob_mdk.process_message(&remove_charlie_result.evolution_event);
    tracing::info!(
        "Remove commit processing result: {:?}",
        remove_commit_result
    );

    // Alice merges the pending commit for removing Charlie
    alice_mdk.merge_pending_commit(&alice_group.mls_group_id)?;

    // Verify Charlie is no longer in the group
    let group_members_after_removal = alice_mdk.get_members(&alice_group.mls_group_id)?;
    assert_eq!(
        group_members_after_removal.len(),
        2,
        "Group should now have 2 members"
    );
    assert!(
        !group_members_after_removal.contains(&charlie_keys.public_key()),
        "Charlie should not be in the group"
    );

    // ================================
    // Bob leaving the group
    // ================================

    tracing::info!("Bob leaving the group");
    let bob_leave_result = bob_mdk.leave_group(&bobs_group.mls_group_id)?;

    // Alice processes Bob's leave proposal
    tracing::info!("Alice processing Bob's leave proposal");
    let leave_proposal_result = alice_mdk.process_message(&bob_leave_result.evolution_event);
    tracing::info!(
        "Leave proposal processing result: {:?}",
        leave_proposal_result
    );

    // The leave creates a proposal that needs to be committed by an admin (Alice)
    // Alice should create a commit to finalize Bob's removal
    // Note: In a real application, Alice would need to detect the proposal and create a commit
    // For now, we'll verify the proposal was processed correctly

    match leave_proposal_result {
        Ok(MessageProcessingResult::Proposal(_)) => {
            // Admin receiver auto-committed the proposal
            tracing::info!(
                "Bob's leave proposal was successfully processed and committed by Alice (admin)"
            );
        }
        Ok(MessageProcessingResult::PendingProposal { .. }) => {
            // Non-admin receiver stored proposal as pending
            tracing::info!("Bob's leave proposal was stored as pending (receiver is not admin)");
        }
        _ => {
            tracing::warn!("Unexpected result from processing Bob's leave proposal");
        }
    }

    tracing::info!("MLS group operations completed successfully!");

    cleanup(alice_temp_dir, bob_temp_dir, charlie_temp_dir);

    Ok(())
}

fn cleanup(alice_temp_dir: TempDir, bob_temp_dir: TempDir, charlie_temp_dir: TempDir) {
    alice_temp_dir
        .close()
        .expect("Failed to close temp directory");
    bob_temp_dir
        .close()
        .expect("Failed to close temp directory");
    charlie_temp_dir
        .close()
        .expect("Failed to close temp directory");
}
