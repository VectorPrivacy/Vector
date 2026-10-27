//! The compact message store against plain `Message`s: memory, inserts and lookups.

use std::time::Instant;
use vector_core::{CompactMessage, CompactMessageVec, Message, NpubInterner};

/// Memory, inserts and id lookups for 10,000 messages.
pub fn messages() {

    const NUM_MESSAGES: usize = 10_000;
    const NUM_UNIQUE_USERS: usize = 50; // Realistic chat scenario

    println!("\n========================================");
    println!("  COMPACT MESSAGE BENCHMARK");
    println!("  {} messages, {} unique users", NUM_MESSAGES, NUM_UNIQUE_USERS);
    println!("========================================\n");

    // Generate test data
    let users: Vec<String> = (0..NUM_UNIQUE_USERS)
        .map(|i| format!("npub1{:0>62}", i))
        .collect();

    // Create regular Messages
    let messages: Vec<Message> = (0..NUM_MESSAGES)
        .map(|i| {
            let user_idx = i % NUM_UNIQUE_USERS;
            Message {
                expiration: None,
                id: format!("{:0>64x}", i),
                at: 1700000000000 + (i as u64 * 1000),
                mine: user_idx == 0,
                pending: false,
                failed: false,
                edited: false,
                npub: Some(users[user_idx].clone()),
                replied_to: if i > 0 && i % 5 == 0 {
                    format!("{:0>64x}", i - 1)
                } else {
                    String::new()
                },
                replied_to_content: if i > 0 && i % 5 == 0 {
                    Some("Previous message content".to_string())
                } else {
                    None
                },
                replied_to_emoji_tags: None,
                replied_to_npub: if i > 0 && i % 5 == 0 {
                    Some(users[(i - 1) % NUM_UNIQUE_USERS].clone())
                } else {
                    None
                },
                replied_to_has_attachment: None,
                replied_to_attachment_extension: None,
                wrapper_event_id: Some(format!("{:0>64x}", i + 1000000)),
                content: format!("This is message number {} with some typical content length.", i),
                attachments: vec![],
                reactions: vec![],
                edit_history: None,
                preview_metadata: None,
                emoji_tags: Vec::new(),
                addressed_bots: Vec::new(),
                text_spans: Vec::new(),
                role_mentions: Vec::new(),
                writer: false,
                unauthorized: false,
            }
        })
        .collect();

    // ===== MEMORY COMPARISON =====
    println!("--- STRUCT SIZES ---");
    println!("  Message struct:        {} bytes", std::mem::size_of::<Message>());
    println!("  CompactMessage struct: {} bytes", std::mem::size_of::<CompactMessage>());
    println!("  Savings per struct:    {} bytes ({:.1}%)",
        std::mem::size_of::<Message>().saturating_sub(std::mem::size_of::<CompactMessage>()),
        (1.0 - std::mem::size_of::<CompactMessage>() as f64 / std::mem::size_of::<Message>() as f64) * 100.0
    );
    println!();

    // Measure Message storage (simulating Vec<Message>)
    let msg_heap_estimate: usize = messages.iter().map(|m| {
        m.id.capacity()
            + m.npub.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + m.replied_to.capacity()
            + m.replied_to_content.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + m.replied_to_npub.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + m.wrapper_event_id.as_ref().map(|s| s.capacity()).unwrap_or(0)
            + m.content.capacity()
    }).sum();
    let msg_total = messages.len() * std::mem::size_of::<Message>() + msg_heap_estimate;

    // ===== CONVERSION + INSERT BENCHMARK =====
    println!("--- INSERT BENCHMARK ---");

    // Test 1: Sequential inserts (simulates real-time message arrival)
    let mut interner = NpubInterner::with_capacity(NUM_UNIQUE_USERS);
    let mut compact_vec = CompactMessageVec::with_capacity(NUM_MESSAGES);

    let insert_start = Instant::now();
    for msg in &messages {
        let compact = CompactMessage::from_message(msg, &mut interner);
        compact_vec.insert(compact);
    }
    let insert_elapsed = insert_start.elapsed();

    println!("  Sequential insert (optimized append path):");
    println!("    {} messages in {:?}", NUM_MESSAGES, insert_elapsed);
    println!("    Rate: {:.0} msgs/sec", NUM_MESSAGES as f64 / insert_elapsed.as_secs_f64());
    println!("    Per message: {:.3} us ({} ns)",
        insert_elapsed.as_micros() as f64 / NUM_MESSAGES as f64,
        insert_elapsed.as_nanos() / NUM_MESSAGES as u128);
    println!();

    // Test 2: Batch insert (simulates pagination/history loading)
    let mut interner2 = NpubInterner::with_capacity(NUM_UNIQUE_USERS);
    let mut compact_vec2 = CompactMessageVec::with_capacity(NUM_MESSAGES);

    let batch_start = Instant::now();
    let compact_messages: Vec<_> = messages.iter()
        .map(|msg| CompactMessage::from_message(msg, &mut interner2))
        .collect();
    let batch_added = compact_vec2.insert_batch(compact_messages);
    let batch_elapsed = batch_start.elapsed();

    println!("  Batch insert (pagination/history load):");
    println!("    {} messages in {:?}", batch_added, batch_elapsed);
    println!("    Rate: {:.0} msgs/sec", NUM_MESSAGES as f64 / batch_elapsed.as_secs_f64());
    println!("    Per message: {:.3} us ({} ns)",
        batch_elapsed.as_micros() as f64 / NUM_MESSAGES as f64,
        batch_elapsed.as_nanos() / NUM_MESSAGES as u128);
    println!();

    // ===== COMPACT MEMORY USAGE =====
    println!("--- MEMORY COMPARISON ---");
    let compact_heap_estimate: usize = compact_vec.iter()
        .map(|m| vector_core::stats::DeepSize::deep_size(m) - std::mem::size_of::<CompactMessage>())
        .sum();
    let compact_struct_mem = compact_vec.len() * std::mem::size_of::<CompactMessage>();
    let compact_index_mem = compact_vec.len() * std::mem::size_of::<u32>();
    let interner_mem = interner.memory_usage();
    let compact_total = compact_struct_mem + compact_heap_estimate + compact_index_mem + interner_mem;

    println!("  Regular Message storage:");
    println!("    Struct memory:     {:>10} bytes", messages.len() * std::mem::size_of::<Message>());
    println!("    Heap (strings):    {:>10} bytes", msg_heap_estimate);
    println!("    TOTAL:             {:>10} bytes ({:.2} MB)", msg_total, msg_total as f64 / 1_000_000.0);
    println!();
    println!("  CompactMessage storage:");
    println!("    Struct memory:     {:>10} bytes", compact_struct_mem);
    println!("    Heap (strings):    {:>10} bytes", compact_heap_estimate);
    println!("    ID index:          {:>10} bytes", compact_index_mem);
    println!("    Interner:          {:>10} bytes ({} unique npubs)", interner_mem, interner.len());
    println!("    TOTAL:             {:>10} bytes ({:.2} MB)", compact_total, compact_total as f64 / 1_000_000.0);
    println!();
    println!("  SAVINGS: {} bytes ({:.1}%)",
        msg_total.saturating_sub(compact_total),
        (1.0 - compact_total as f64 / msg_total as f64) * 100.0
    );
    println!("  Per message: {} -> {} bytes (avg)",
        msg_total / NUM_MESSAGES,
        compact_total / NUM_MESSAGES
    );
    println!();

    // ===== LOOKUP BENCHMARK =====
    println!("--- LOOKUP BENCHMARK ---");

    // Generate random lookup IDs (mix of existing and non-existing)
    let lookup_ids: Vec<String> = (0..1000)
        .map(|i| format!("{:0>64x}", i * 10)) // Every 10th message
        .collect();

    // Benchmark binary search lookup (CompactMessageVec)
    const ROUNDS: usize = 100;
    let lookup_start = Instant::now();
    let mut found_count = 0;
    for _ in 0..ROUNDS {
        for id in &lookup_ids {
            if compact_vec.find_by_hex_id(id).is_some() {
                found_count += 1;
            }
        }
    }
    let lookup_elapsed = lookup_start.elapsed();
    let total_lookups = ROUNDS * lookup_ids.len();

    println!("  Binary search (CompactMessageVec):");
    println!("    {} lookups in {:?}", total_lookups, lookup_elapsed);
    println!("    Rate: {:.0} lookups/sec", total_lookups as f64 / lookup_elapsed.as_secs_f64());
    println!("    Per lookup: {:.2} us", lookup_elapsed.as_micros() as f64 / total_lookups as f64);
    println!("    Found: {} / {}", found_count, total_lookups);
    println!();

    // Benchmark linear search (simulating Vec<Message>)
    let linear_start = Instant::now();
    let mut linear_found = 0;
    for _ in 0..ROUNDS {
        for id in &lookup_ids {
            if messages.iter().find(|m| &m.id == id).is_some() {
                linear_found += 1;
            }
        }
    }
    let linear_elapsed = linear_start.elapsed();

    println!("  Linear search (Vec<Message>):");
    println!("    {} lookups in {:?}", total_lookups, linear_elapsed);
    println!("    Rate: {:.0} lookups/sec", total_lookups as f64 / linear_elapsed.as_secs_f64());
    println!("    Per lookup: {:.2} us", linear_elapsed.as_micros() as f64 / total_lookups as f64);
    println!();

    let speedup = linear_elapsed.as_nanos() as f64 / lookup_elapsed.as_nanos() as f64;
    println!("  SPEEDUP: {:.1}x faster with binary search!", speedup);
    println!();

    // ===== INTERNER EFFICIENCY =====
    println!("--- INTERNER EFFICIENCY ---");
    let npub_string_size = 63 + 1; // "npub1" + 58 chars + null
    let naive_npub_mem = NUM_MESSAGES * npub_string_size * 2; // npub + replied_to_npub
    let actual_npub_mem = interner_mem;
    println!("  Naive (every msg stores npubs): {} bytes", naive_npub_mem);
    println!("  Interned ({} unique):           {} bytes", interner.len(), actual_npub_mem);
    println!("  SAVINGS: {} bytes ({:.1}%)",
        naive_npub_mem.saturating_sub(actual_npub_mem),
        (1.0 - actual_npub_mem as f64 / naive_npub_mem as f64) * 100.0
    );
    println!();

    println!("========================================");
    println!("  BENCHMARK COMPLETE");
    println!("========================================\n");

    // Verify correctness
    assert_eq!(compact_vec.len(), NUM_MESSAGES);
    assert_eq!(interner.len(), NUM_UNIQUE_USERS);
    assert_eq!(found_count, linear_found);
}

/// Benchmark: Profile lookup -- linear scan vs string binary search vs handle binary search
///
/// Compares three approaches for finding a Profile in a Vec:
/// 1. Linear scan with string equality (old -- O(n) x 63-byte strcmp, Profile.id was String)
/// 2. Binary search by npub string (intermediate -- O(log n) x 63-byte strcmp)
/// 3. Direct u16 handle binary search (current -- O(log n) x 2-byte int cmp, Profile.id is u16)
pub fn profile_lookup() {
    use std::hint::black_box;

    const NUM_PROFILES: usize = 60;
    const NUM_LOOKUPS: usize = 100_000;

    println!("\n========================================");
    println!("  PROFILE LOOKUP BENCHMARK");
    println!("  {} profiles, {} lookups each method", NUM_PROFILES, NUM_LOOKUPS);
    println!("========================================\n");

    // Generate realistic npubs (63 chars each: "npub1" + 58 hex-like chars)
    let npubs: Vec<String> = (0..NUM_PROFILES)
        .map(|i| format!("npub1{:0>58}", format!("{:x}", i * 7919 + 1000))) // spread out values
        .collect();

    // --- Setup: Method 1 - Linear scan (old approach, simulating id: String) ---
    let old_ids: Vec<String> = npubs.iter().rev().cloned().collect(); // reversed = worst case

    // --- Setup: Method 2 - String binary search (intermediate approach) ---
    let mut sorted_ids: Vec<String> = npubs.clone();
    sorted_ids.sort();

    // --- Setup: Method 3 - Direct u16 handle lookup (current approach, id: u16) ---
    let mut interner = NpubInterner::new();
    let mut profiles: Vec<vector_core::profile::Profile> = npubs.iter().map(|npub| {
        let mut p = vector_core::profile::Profile::new();
        p.id = interner.intern(npub);
        p
    }).collect();
    profiles.sort_by_key(|p| p.id);

    // Build lookup targets: cycle through all profiles
    let lookup_targets: Vec<&str> = (0..NUM_LOOKUPS)
        .map(|i| npubs[i % NUM_PROFILES].as_str())
        .collect();

    // Pre-resolve handles for method 3
    let handle_targets: Vec<u16> = lookup_targets.iter()
        .map(|&npub| interner.lookup(npub).unwrap())
        .collect();

    // ===== BENCHMARK 1: Linear scan (old -- id: String) =====
    let start = Instant::now();
    let mut found = 0u64;
    for &target in &lookup_targets {
        if old_ids.iter().any(|id| id == target) {
            found += 1;
        }
    }
    let linear_elapsed = start.elapsed();
    assert_eq!(found, NUM_LOOKUPS as u64);

    // ===== BENCHMARK 2: String binary search (intermediate) =====
    let start = Instant::now();
    found = 0;
    for &target in &lookup_targets {
        if sorted_ids.binary_search_by(|id| id.as_str().cmp(target)).is_ok() {
            found += 1;
        }
    }
    let string_bs_elapsed = start.elapsed();
    assert_eq!(found, NUM_LOOKUPS as u64);

    // ===== BENCHMARK 3: Direct u16 handle lookup (current -- id: u16) =====
    let start = Instant::now();
    found = 0;
    for &handle in &handle_targets {
        if profiles.binary_search_by(|p| p.id.cmp(black_box(&handle))).is_ok() {
            found += 1;
        }
    }
    let direct_elapsed = start.elapsed();
    assert_eq!(found, NUM_LOOKUPS as u64);

    // ===== RESULTS =====
    println!("--- LOOKUP METHODS ---");
    println!("  1. Linear scan (old id: String):");
    println!("     {:?} total, {:.0} ns/lookup",
        linear_elapsed,
        linear_elapsed.as_nanos() as f64 / NUM_LOOKUPS as f64);
    println!();
    println!("  2. String binary search (intermediate):");
    println!("     {:?} total, {:.0} ns/lookup",
        string_bs_elapsed,
        string_bs_elapsed.as_nanos() as f64 / NUM_LOOKUPS as f64);
    println!("     vs linear: {:.1}x faster",
        linear_elapsed.as_nanos() as f64 / string_bs_elapsed.as_nanos() as f64);
    println!();
    println!("  3. Direct u16 handle lookup (current id: u16):");
    println!("     {:?} total, {:.0} ns/lookup",
        direct_elapsed,
        direct_elapsed.as_nanos() as f64 / NUM_LOOKUPS as f64);
    println!("     vs linear: {:.1}x faster",
        linear_elapsed.as_nanos() as f64 / direct_elapsed.as_nanos() as f64);
    println!("     vs string BS: {:.1}x faster",
        string_bs_elapsed.as_nanos() as f64 / direct_elapsed.as_nanos() as f64);
    println!();

    // ===== MEMORY COMPARISON =====
    println!("--- MEMORY PER PROFILE ---");
    println!("  Old (id: String):    ~87 bytes (24 String header + ~63 heap)");
    println!("  Current (id: u16):     2 bytes (inline)");
    println!("  Savings: ~85 bytes/profile, ~{} bytes for {} profiles",
        85 * NUM_PROFILES, NUM_PROFILES);
    println!("  Interner (shared):   {} bytes (shared with message system)",
        interner.memory_usage());
    println!();

    println!("========================================");
    println!("  BENCHMARK COMPLETE");
    println!("========================================\n");

    // Correctness: ensure all methods find the same profiles
    for npub in &npubs {
        assert!(old_ids.iter().any(|id| id == npub));
        assert!(sorted_ids.binary_search_by(|id| id.as_str().cmp(npub.as_str())).is_ok());
        let h = interner.lookup(npub).unwrap();
        assert!(profiles.binary_search_by(|p| p.id.cmp(&h)).is_ok());
    }
}
