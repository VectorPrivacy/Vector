//! Sends a single file to a recipient, then exits. Demonstrates the send side of
//! the encrypted file-attachment pipeline; see `save_files_bot` for the receive
//! side (download + decrypt).
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1... \
//! VECTOR_TARGET=npub1... \
//! VECTOR_FILE=/path/to/image.png \
//! cargo run -p vector-sdk --example file_bot
//! ```

use std::time::Duration;
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");
    let target = std::env::var("VECTOR_TARGET").expect("set VECTOR_TARGET to a recipient npub");
    let file = std::env::var("VECTOR_FILE").expect("set VECTOR_FILE to a file path");

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("Sending {} as {}", file, bot.npub());

    // Give the relay connections a moment to come up before the first send.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let event_id = bot.channel(&target).send_file(&file).await?;
    println!("Sent — event id: {}", event_id);

    Ok(())
}
