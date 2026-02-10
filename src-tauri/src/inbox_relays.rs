//! NIP-17 Kind 10050 (DM Relay List) support.
//!
//! Fetches, caches, and publishes kind 10050 events so that DM gift wraps
//! are delivered to the recipient's preferred inbox relays.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use nostr_sdk::prelude::*;
use once_cell::sync::Lazy;

use crate::NOSTR_CLIENT;

// ============================================================================
// Cache
// ============================================================================

/// How long cached relay lists stay valid before re-fetching.
const CACHE_TTL_SECS: u64 = 3600; // 1 hour

/// Shorter TTL for failed fetches so transient errors don't suppress routing too long.
const CACHE_TTL_ERROR_SECS: u64 = 60; // 1 minute

struct CachedRelays {
    relays: Vec<String>,
    fetched_at: Instant,
    /// Whether the fetch succeeded (true) or failed/timed out (false).
    /// Failed fetches use a shorter cache TTL.
    fetch_ok: bool,
}

static INBOX_RELAY_CACHE: Lazy<Mutex<HashMap<PublicKey, CachedRelays>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

// ============================================================================
// Fetch
// ============================================================================

/// Result of a 10050 fetch: relays found, or whether the fetch itself failed.
struct FetchResult {
    relays: Vec<String>,
    /// `true` if the network request succeeded (even if no events were found).
    fetch_ok: bool,
}

/// Fetch a pubkey's kind 10050 relay list from the network.
async fn fetch_inbox_relays(client: &Client, pubkey: &PublicKey) -> FetchResult {
    let filter = Filter::new()
        .author(*pubkey)
        .kind(Kind::Custom(10050))
        .limit(1);

    let events = match client
        .fetch_events(filter, std::time::Duration::from_secs(5))
        .await
    {
        Ok(events) => events,
        Err(e) => {
            eprintln!("[InboxRelays] Failed to fetch 10050 for {}: {}", pubkey, e);
            return FetchResult { relays: Vec::new(), fetch_ok: false };
        }
    };

    // The SDK returns Events (implements IntoIterator), take the first (most recent).
    let event = match events.into_iter().next() {
        Some(e) => e,
        None => return FetchResult { relays: Vec::new(), fetch_ok: true },
    };

    FetchResult { relays: parse_relay_tags(&event.tags), fetch_ok: true }
}

/// Extract relay URLs from kind 10050 event tags.
/// Looks for `["relay", "wss://..."]` tag entries.
fn parse_relay_tags(tags: &Tags) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| {
            let values: Vec<&str> = tag.as_slice().iter().map(|s| s.as_str()).collect();
            if values.len() >= 2 && values[0] == "relay" {
                Some(values[1].to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Get inbox relays for a pubkey, using cache when available.
async fn get_or_fetch_inbox_relays(client: &Client, pubkey: &PublicKey) -> Vec<String> {
    // Check cache first
    {
        let cache = INBOX_RELAY_CACHE.lock().unwrap();
        if let Some(entry) = cache.get(pubkey) {
            let ttl = if entry.fetch_ok { CACHE_TTL_SECS } else { CACHE_TTL_ERROR_SECS };
            if entry.fetched_at.elapsed().as_secs() < ttl {
                return entry.relays.clone();
            }
        }
    }

    let result = fetch_inbox_relays(client, pubkey).await;

    // Store in cache (even empty/error results to avoid hammering relays)
    {
        let mut cache = INBOX_RELAY_CACHE.lock().unwrap();
        cache.insert(
            *pubkey,
            CachedRelays {
                relays: result.relays.clone(),
                fetched_at: Instant::now(),
                fetch_ok: result.fetch_ok,
            },
        );
    }

    result.relays
}

// ============================================================================
// Send helper
// ============================================================================

/// Send a gift-wrapped rumor to a recipient, routing to their inbox relays
/// (kind 10050) when available. Falls back to pool broadcast if no inbox
/// relays are found or if targeted delivery fails entirely.
pub async fn send_gift_wrap(
    client: &Client,
    recipient: &PublicKey,
    rumor: UnsignedEvent,
    extra_tags: impl IntoIterator<Item = Tag>,
) -> Result<Output<EventId>, Error> {
    let inbox = get_or_fetch_inbox_relays(client, recipient).await;

    if inbox.is_empty() {
        // No 10050 found — broadcast to pool (current behaviour)
        return client.gift_wrap(recipient, rumor, extra_tags).await;
    }

    println!(
        "[InboxRelays] Routing gift-wrap to {} inbox relays for {}",
        inbox.len(),
        recipient
    );

    // Collect tags so they can be reused on fallback
    let tags: Vec<Tag> = extra_tags.into_iter().collect();

    match client
        .gift_wrap_to(inbox, recipient, rumor.clone(), tags.clone())
        .await
    {
        Ok(output) if !output.success.is_empty() => Ok(output),
        Ok(_) => {
            // All inbox relays failed — fall back to pool broadcast
            eprintln!(
                "[InboxRelays] All inbox relays failed for {}, falling back to pool broadcast",
                recipient
            );
            client.gift_wrap(recipient, rumor, tags).await
        }
        Err(e) => {
            eprintln!(
                "[InboxRelays] gift_wrap_to error for {}: {}, falling back to pool broadcast",
                recipient, e
            );
            client.gift_wrap(recipient, rumor, tags).await
        }
    }
}

// ============================================================================
// Publish own inbox relays
// ============================================================================

/// Publish our own kind 10050 event advertising readable relays as DM inboxes.
/// Write-only relays are excluded since senders need to write to them.
/// If no readable relays exist, publishes an empty 10050 to clear any stale list.
pub async fn publish_inbox_relays(client: &Client) -> Result<(), String> {
    // Gather relay URLs that have the READ flag (i.e. relays we read from,
    // which means senders should write to them so we can receive DMs).
    let relays: Vec<String> = client
        .pool()
        .relays()
        .await
        .iter()
        .filter(|(_, relay)| relay.flags().has_read())
        .map(|(url, _)| url.to_string())
        .collect();

    // Build kind 10050 replaceable event with ["relay", url] tags.
    // An empty event (no relay tags) replaces any prior 10050, clearing stale lists.
    let mut builder = EventBuilder::new(Kind::Custom(10050), "");
    for url in &relays {
        builder = builder.tag(Tag::custom(TagKind::custom("relay"), vec![url.clone()]));
    }

    client
        .send_event_builder(builder)
        .await
        .map_err(|e| format!("Failed to publish inbox relays: {}", e))?;

    println!(
        "[InboxRelays] Published kind 10050 with {} relay(s)",
        relays.len()
    );
    Ok(())
}

/// Monotonic generation counter used to debounce republish calls.
/// Only the most recent spawn actually publishes; earlier ones exit early.
static REPUBLISH_GEN: AtomicU64 = AtomicU64::new(0);

/// Republish kind 10050 in the background (debounced).
/// Called after relay config changes (add/remove/toggle/mode update).
/// Rapid successive calls coalesce into a single publish.
pub fn republish_inbox_relays_debounced() {
    let gen = REPUBLISH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    tokio::spawn(async move {
        // Wait for the relay pool to settle; if another call arrives
        // during this window it will bump the generation and we'll exit.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        if REPUBLISH_GEN.load(Ordering::SeqCst) != gen {
            return; // superseded by a newer call
        }
        let client = match NOSTR_CLIENT.get() {
            Some(c) => c,
            None => return,
        };
        if let Err(e) = publish_inbox_relays(client).await {
            eprintln!("[InboxRelays] Failed to republish after config change: {}", e);
        }
    });
}
