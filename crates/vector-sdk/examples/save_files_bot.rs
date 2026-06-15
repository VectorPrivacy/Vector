//! Saves every file anyone sends, decrypting it on the way in. This is the
//! receive side of the encrypted-attachment pipeline (`file_bot` shows the send
//! side) — `bot.save_attachment` fetches the blob from its Blossom server and
//! AES-decrypts it with the key + nonce that travelled inside the message.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1...             \
//! VECTOR_DOWNLOAD_DIR=./downloads  \
//! cargo run -p vector-sdk --example save_files_bot
//! ```

use std::path::PathBuf;
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");
    let dir = PathBuf::from(
        std::env::var("VECTOR_DOWNLOAD_DIR").unwrap_or_else(|_| "downloads".into()),
    );
    std::fs::create_dir_all(&dir).ok();

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("Saving incoming files to {} as {}", dir.display(), bot.npub());

    bot.on_message(move |bot, msg| {
        let dir = dir.clone();
        async move {
            if msg.is_mine() || !msg.is_file {
                return;
            }
            for att in &msg.message.attachments {
                // Prefer the sender's original filename; fall back to the attachment id.
                let filename = if att.name.is_empty() {
                    format!("{}.{}", att.id, att.extension)
                } else {
                    att.name.clone()
                };
                match bot.save_attachment(att, dir.join(&filename)).await {
                    Ok(path) => {
                        println!("saved {}", path.display());
                        let _ = msg.reply(&format!("saved {filename} ({} bytes)", att.size)).await;
                    }
                    Err(e) => {
                        let _ = msg.reply(&format!("couldn't save {filename}: {e}")).await;
                    }
                }
            }
        }
    })
    .await?;

    Ok(())
}
