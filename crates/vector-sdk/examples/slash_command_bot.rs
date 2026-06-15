//! A slash-command bot: a tiny command router that works in DMs and Communities.
//!
//! Message it `/help`, `/ping`, `/echo hello`, `/roll`, or `/roll 20`.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example slash_command_bot
//! ```

use std::time::{SystemTime, UNIX_EPOCH};
use vector_sdk::VectorBot;

const HELP: &str = "\
Commands:
  /help        — show this message
  /ping        — pong, with a timestamp
  /echo <text> — repeat your text back
  /roll [N]    — roll a d[N] (default d6)
  /about       — what am I?";

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("Slash-command bot online as {}", bot.npub());

    // One handler, every conversation — DMs and Community channels alike.
    bot.on_message(|_bot, msg| async move {
        if msg.is_mine() {
            return;
        }
        // Ignore anything that isn't a /command.
        let Some(rest) = msg.text().trim().strip_prefix('/') else { return };
        let mut parts = rest.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or("").to_lowercase();
        let args = parts.next().unwrap_or("").trim();

        let response = match command.as_str() {
            "help" => HELP.to_string(),
            "ping" => format!("pong 🏓 ({} ms since epoch)", now_millis()),
            "echo" if !args.is_empty() => args.to_string(),
            "echo" => "usage: /echo <text>".to_string(),
            "roll" => {
                let sides = args.parse::<u64>().unwrap_or(6).max(1);
                let value = now_millis() % sides + 1;
                format!("🎲 d{sides} → {value}")
            }
            "about" => "I'm a Vector bot built with the vector-sdk. Try /help.".to_string(),
            other => format!("unknown command `/{other}` — try /help"),
        };

        // `reply` threads the response to the triggering message, in the same place it arrived.
        let _ = msg.reply(&response).await;
    })
    .await?;

    Ok(())
}

/// Milliseconds since the Unix epoch — doubles as a throwaway source of entropy for `/roll`.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
