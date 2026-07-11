//! Per-relay ephemeral-forwarding probe: subscribe to EACH community relay
//! individually for kind-21059 wraps on a channel's chat plane, trigger the
//! resident test bot's `!typing`, and record which relay forwards whose typing.
//! Isolates "relay accepted but silently dropped" (OK-true lie) from client bugs.
//!
//! Run from the SAME data dir as a bot that already joined the community:
//! ```sh
//! cargo run -p vector_sdk --example v2_typing_probe -- "<channel name>"
//! ```

use nostr_sdk::prelude::*;
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let channel_name = std::env::args().nth(1).unwrap_or_else(|| "spam and testing".into());

    let data_dir = std::env::temp_dir().join("v2_send_once_data");
    let bot = VectorBot::builder().data_dir(&data_dir).build().await?;
    println!("── probe online as {}", bot.npub());

    // Find the joined v2 community + the target channel id.
    let (mut community_hex, mut channel_hex) = (None::<String>, None::<String>);
    for c in bot.core().list_communities().await {
        if c.get("version").and_then(|v| v.as_u64()) != Some(2) {
            continue;
        }
        let cid = c.get("community_id").and_then(|v| v.as_str()).map(String::from);
        for ch in c.get("channels").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            if ch.get("name").and_then(|n| n.as_str()) == Some(channel_name.as_str()) {
                community_hex = cid.clone();
                channel_hex = ch.get("channel_id").and_then(|i| i.as_str()).map(String::from);
            }
        }
    }
    let (community_hex, channel_hex) = match (community_hex, channel_hex) {
        (Some(c), Some(ch)) => (c, ch),
        _ => {
            eprintln!("!! channel \"{channel_name}\" not held locally — join first (run v2_send_once once)");
            std::process::exit(1);
        }
    };

    // Load the community row for relays + root, and derive the channel plane pk
    // exactly as the send/subscribe paths do.
    let cid = vector_core::community::CommunityId(vector_core::simd::hex::hex_to_bytes_32(&community_hex));
    let community = vector_core::db::community::load_community_v2(&cid)
        .map_err(vector_core::VectorError::Other)?
        .ok_or(vector_core::VectorError::Other("community row missing".into()))?;
    let chid = vector_core::community::ChannelId(vector_core::simd::hex::hex_to_bytes_32(&channel_hex));
    let ch = community.channel(&chid).ok_or(vector_core::VectorError::Other("channel missing".into()))?;
    let (secret, epoch) = community.channel_secret(ch);
    let plane_pk = vector_core::community::v2::derive::channel_group_key(&secret, &chid, epoch).pk();
    println!("── channel \"{channel_name}\" plane pk = {plane_pk}");
    println!("── community relays: {:?}", community.relays);

    // One RAW single-relay client per community relay, each with its own live
    // typing subscription — per-relay forwarding becomes directly observable.
    // Every arriving wrap is opened with the plane key and its rumor dumped
    // verbatim: exactly what Armada's `openWrap` + freshness check would see.
    let group = vector_core::community::v2::derive::channel_group_key(&secret, &chid, epoch);
    for relay in community.relays.clone() {
        let plane_pk = plane_pk;
        let group = group.clone();
        tokio::spawn(async move {
            let client = Client::default();
            if client.add_relay(&relay).await.is_err() {
                println!("[{relay}] add failed");
                return;
            }
            client.connect().await;
            let filter = Filter::new().kind(Kind::Custom(21059)).author(plane_pk);
            if let Err(e) = client.subscribe(filter, None).await {
                println!("[{relay}] subscribe failed: {e}");
                return;
            }
            println!("[{relay}] subscribed");
            let mut notifications = client.notifications();
            while let Ok(n) = notifications.recv().await {
                if let RelayPoolNotification::Event { event, .. } = n {
                    println!(
                        "[{relay}] ← 21059 wrap id={} wrap_created_at={} size={}B",
                        &event.id.to_hex()[..12],
                        event.created_at,
                        event.content.len()
                    );
                    match vector_core::community::v2::stream::open_wrap(&event, &group) {
                        Ok(opened) => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let age = now as i128 - opened.at_ms as i128;
                            println!(
                                "    rumor: kind={} author={} created_at={} content={:?}",
                                opened.rumor.kind,
                                &opened.author.to_hex()[..12],
                                opened.rumor.created_at,
                                opened.rumor.content
                            );
                            println!("    tags:  {:?}", opened.rumor.tags.iter().map(|t| t.as_slice().to_vec()).collect::<Vec<_>>());
                            println!("    seal:  kind={} created_at={}", opened.seal.kind, opened.seal.created_at);
                            println!("    at_ms={} (age {age} ms → Armada freshness gate {} 8000ms)", opened.at_ms, if age <= 8000 { "PASSES ≤" } else { "FAILS >" });
                        }
                        Err(e) => println!("    !! open failed: {e}"),
                    }
                }
            }
        });
    }
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    // Trigger the resident bot: a normal chat message it answers with ch.typing().
    println!("── sending !typing into \"{channel_name}\"…");
    match bot.channel(&channel_hex).send("!typing").await {
        Ok(_) => println!("── trigger sent; watching 25s for the bot's typing wrap…"),
        Err(e) => eprintln!("!! trigger send failed: {e}"),
    }
    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
    println!("── probe done");
    Ok(())
}
