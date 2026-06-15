//! A minimal echo bot. Replies to every inbound DM with `Echo: <text>`.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example echo_bot
//! ```

use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("Echo bot online as {}", bot.npub());

    // Blocks, processing inbound messages until the client disconnects. The SAME handler and
    // reply work for DMs and Community channels — the SDK hides the transport difference.
    bot.on_message(|_bot, msg| async move {
        if msg.is_mine() {
            return; // don't echo our own messages
        }
        let _ = msg.channel().typing().await;
        let _ = msg.reply(&format!("Echo: {}", msg.text())).await;
    })
    .await?;

    Ok(())
}
