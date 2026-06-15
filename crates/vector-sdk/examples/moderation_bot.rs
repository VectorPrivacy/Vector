//! A moderation bot for Communities. Welcomes new members and auto-bans anyone
//! who posts a banned word — admins and the bot itself are never moderated.
//!
//! Demonstrates `on_event` (the full inbound stream) and the discord.js-style
//! "actor in context" model: `msg.member()` is the sender as a [`Member`] you can
//! act on directly.
//!
//! The bot must be a member of the community first — it builds `.public()` so you
//! can invite it from the Vector app.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example moderation_bot
//! ```
//!
//! [`Member`]: vector_sdk::Member

use vector_sdk::{BotEvent, VectorBot};

const BANNED_WORDS: &[&str] = &["spamword", "scamlink", "verboten"];

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");

    // `.public()` lets anyone invite the bot into their community to moderate it.
    let bot = VectorBot::builder().nsec(nsec).public().build().await?;
    println!("Moderation bot online as {}", bot.npub());

    bot.on_event(|bot, event| async move {
        match event {
            // Greet new arrivals in the channel they joined.
            BotEvent::MemberJoin { channel_id, npub } => {
                let who = &npub[..npub.len().min(12)];
                let _ = bot.channel(channel_id).send(&format!("👋 Welcome, {who}!")).await;
            }

            // Screen community messages; ban posters of banned words (never admins or ourselves).
            BotEvent::Message(msg) if msg.is_group && !msg.is_mine() => {
                let lower = msg.text().to_lowercase();
                if !BANNED_WORDS.iter().any(|w| lower.contains(w)) {
                    return;
                }
                if let Some(member) = msg.member() {
                    if member.is_admin() {
                        return; // never moderate admins or the owner
                    }
                    match member.ban().await {
                        Ok(()) => println!("Banned {} for a banned word", member.npub()),
                        Err(e) => eprintln!("Couldn't ban {}: {e}", member.npub()),
                    }
                }
            }

            _ => {}
        }
    })
    .await?;

    Ok(())
}
