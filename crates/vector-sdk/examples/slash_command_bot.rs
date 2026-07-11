//! A slash-command bot on the Bot Interface manifest system (Phase 1).
//!
//! Commands are registered with the builder, so the kind-33304 manifest
//! publishes automatically at listen start and clients render a `/` picker
//! with typed argument hints. Invocations arrive as plain message content —
//! any client can type them.
//!
//! ```sh
//! # First run prints the bot's npub. Send it a direct invite from Vector
//! # (public invite policy: it auto-accepts), or pass an invite link as arg 1.
//! BOT_NAME=Dicey VECTOR_DATA_DIR=~/bots/dicey \
//!   cargo run -p vector_sdk --example slash_command_bot
//! ```
//!
//! `VECTOR_DATA_DIR` keeps this bot's identity/DB separate from other bots on
//! the same machine (the default data dir is shared); `VECTOR_NSEC` overrides
//! the persisted identity.

use std::time::{SystemTime, UNIX_EPOCH};
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let name = std::env::var("BOT_NAME").unwrap_or_else(|_| "Dicey".to_string());
    let about = "A dice-flavoured slash-command bot built with vector_sdk. Type / for commands.";

    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    if let Ok(dir) = std::env::var("VECTOR_DATA_DIR") {
        builder = builder.data_dir(dir);
    }
    let bot = builder.build().await?;
    println!("── {name} online as {}", bot.npub());

    // Bot-flagged kind-0 so client pickers gate us in (`bot: true` is what a
    // `/` picker looks for). Deferred until listen has connected the relays.
    {
        let bot = bot.clone();
        let name = name.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let ok = bot.core().update_bot_profile(&name, "", "", about).await;
            println!("── profile publish {}", if ok { "✅" } else { "FAILED" });
        });
    }

    // Optional invite link on first run; later runs come up with memberships.
    if let Some(invite) = std::env::args().nth(1) {
        println!("── joining via link…");
        let summary = bot.core().join_community(&invite).await?;
        let cname = summary.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        println!("── joined \"{cname}\"");
    }

    bot.command("echo", "Repeat your text back")
        .string("text", "What to repeat", true)
        .run(|ctx| async move {
            let text = ctx.str("text").unwrap_or_default().to_string();
            let _ = ctx.reply(text).await;
        });
    // Deliberately the same name as other bots' /roll: the picker's recipient
    // tag routes the invocation to exactly the bot the user chose.
    bot.command("roll", "Roll a die")
        .int("sides", "How many sides (default 6)", false)
        .run(|ctx| async move {
            let sides = ctx.int("sides").unwrap_or(6).clamp(2, 1_000_000);
            let roll = (now_ms() as i64 % sides) + 1;
            let _ = ctx.reply(format!("🎲 rolled {roll} on a d{sides}")).await;
        });
    bot.command("flip", "Flip a coin").run(|ctx| async move {
        let side = if now_ms() % 2 == 0 { "heads" } else { "tails" };
        let _ = ctx.reply(format!("🪙 {side}!")).await;
    });
    bot.command("eightball", "Ask the magic 8-ball")
        .string("question", "Your question", true)
        .run(|ctx| async move {
            const ANSWERS: &[&str] = &[
                "It is certain.", "Without a doubt.", "Ask again later.",
                "Better not tell you now.", "Don't count on it.", "Very doubtful.",
            ];
            let answer = ANSWERS[(now_ms() as usize) % ANSWERS.len()];
            let _ = ctx.reply(format!("🎱 {answer}")).await;
        });
    bot.command("stress", "UI stress test: maximum-length choices")
        .choice(
            "pick",
            "Choose one (two are wire-max 32 chars)",
            ["abcdefghijklmnopqrstuvwxyz-12345", "this-is-a-very-long-choice-name!", "short"],
            true,
        )
        .run(|ctx| async move {
            let _ = ctx.reply(format!("you picked: {}", ctx.str("pick").unwrap_or_default())).await;
        });
    bot.command("about", "What am I?").run(move |ctx| async move {
        let _ = ctx.reply("I'm a slash-command bot built with vector_sdk. My commands come from my published manifest.").await;
    });

    println!("── listening. Type / in a chat with me.\n");
    bot.on_message(|_bot, _msg| async move {
        // Matched commands are consumed before this; ordinary chatter is ignored.
    })
    .await?;

    Ok(())
}

/// Milliseconds since the Unix epoch — a throwaway entropy source for the toys.
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
