//! A bot that OWNS a Concord v2 community. On startup (with `VECTOR_CREATE=1`)
//! it creates the community, prints a shareable invite link, optionally
//! Direct-Invites an npub, then greets and echoes in its channels.
//!
//! Run:
//! ```sh
//! VECTOR_NSEC=nsec1... VECTOR_CREATE=1 cargo run -p vector-sdk --example v2_community_bot
//! # optionally also: VECTOR_INVITE_NPUB=npub1...
//! ```
//!
//! The community and its invites are on the modern protocol, byte-compatible
//! with Soapbox/Armada — the exact same discord.js-style API as a v1 bot; the
//! SDK routes to v2 under the hood.

use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("v2 community bot online as {}", bot.npub());

    // Create-and-invite is opt-in so a restart doesn't spawn a fresh community
    // each time (a real bot persists its community id and reuses it).
    if std::env::var("VECTOR_CREATE").is_ok() {
        // Creation is one hop through the core facade (the ergonomic surface stays at
        // the stable published API). A fresh community is created on the modern protocol.
        let summary = bot.core().create_community_v2("Vector v2 Demo").await?;
        let id = summary.get("community_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let community = bot.community(id);
        println!("created v2 community {}", community.id());

        let url = community.create_invite().await?;
        println!("shareable invite link: {url}");

        if let Ok(guest) = std::env::var("VECTOR_INVITE_NPUB") {
            community.invite(&guest).await?;
            println!("direct-invited {guest}");
        }
    }

    // One handler serves DMs and every Community channel — the SDK hides the
    // transport (and the protocol) difference.
    bot.on_message(|_bot, msg| async move {
        if msg.is_mine() {
            return; // never reply to our own messages (no echo loop)
        }
        let _ = msg.channel().typing().await;
        let _ = msg.reply(&format!("v2 heard: {}", msg.text())).await;
    })
    .await?;

    Ok(())
}
