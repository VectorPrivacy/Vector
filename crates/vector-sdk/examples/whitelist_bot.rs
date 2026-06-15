//! A *private* bot: it auto-joins a community ONLY when invited by a trusted
//! npub, ignoring invites from everyone else. This is what makes a bot safe to
//! publish — it can't be spammed into random communities.
//!
//! Set the trusted inviters (comma-separated npubs) in `VECTOR_WHITELIST`.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1...                       \
//! VECTOR_WHITELIST=npub1aaa...,npub1bbb...    \
//! cargo run -p vector-sdk --example whitelist_bot
//! ```

use vector_sdk::{BotEvent, VectorBot};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");
    let whitelist: Vec<String> = std::env::var("VECTOR_WHITELIST")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Only invites from these npubs are auto-accepted; all others stay parked.
    let bot = VectorBot::builder().nsec(nsec).whitelist(whitelist).build().await?;

    println!("Private bot online as {}", bot.npub());
    for c in bot.communities().await {
        println!("  already a member of community {}", c.id());
    }

    bot.on_event(|bot, event| async move {
        match event {
            // An invite arrived. A whitelisted inviter means it's already being joined;
            // anyone else leaves it parked for you to review via bot.pending_invites().
            BotEvent::Invite { community_id } => {
                println!("invite to {community_id} (auto-join attempted per whitelist)");
            }
            // Best-effort hello once we land in a community we accepted.
            BotEvent::MemberJoin { channel_id, npub } if npub == bot.npub() => {
                let _ = bot.channel(channel_id).send("Hello! Reporting for duty 🫡").await;
            }
            BotEvent::Message(msg) if !msg.is_mine() => {
                let _ = msg.reply("At your service. 🛡️").await;
            }
            _ => {}
        }
    })
    .await?;

    Ok(())
}
