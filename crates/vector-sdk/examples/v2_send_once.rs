//! One-shot v2 sender for live cross-bot verification: join a community from its
//! invite link under an ISOLATED data dir, post one message into a named channel,
//! and exit. Pair it with `v2_test_bot` listening on another identity to prove
//! send + receive round-trip over real relays.
//!
//! ```sh
//! cargo run -p vector-sdk --example v2_send_once -- "<invite-link>" "<channel name>" "<message>"
//! ```

use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let mut args = std::env::args().skip(1);
    let invite = args.next().expect("arg 1: invite link");
    let channel_name = args.next().expect("arg 2: channel name");
    let text = args.next().expect("arg 3: message text");

    let data_dir = std::env::temp_dir().join("v2_send_once_data");
    let bot = VectorBot::builder().data_dir(&data_dir).build().await?;
    println!("── sender online as {}", bot.npub());

    println!("── joining via link…");
    let summary = bot.core().join_community(&invite).await?;
    let name = summary.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    println!("── joined \"{name}\"");

    // The join-time fold is owner-only; admin-created channels arrive on the first
    // control follow. Poll the local list until the target channel appears.
    let mut channel_id: Option<String> = None;
    for attempt in 0..10 {
        for c in bot.core().list_communities().await {
            if c.get("version").and_then(|v| v.as_u64()) != Some(2) {
                continue;
            }
            let Some(chans) = c.get("channels").and_then(|v| v.as_array()) else { continue };
            for ch in chans {
                if ch.get("name").and_then(|n| n.as_str()) == Some(channel_name.as_str()) {
                    channel_id = ch.get("channel_id").and_then(|i| i.as_str()).map(String::from);
                }
            }
        }
        if channel_id.is_some() {
            break;
        }
        if attempt == 0 {
            println!("── \"{channel_name}\" not in the join snapshot; syncing for the control fold…");
        }
        let _ = bot.core().sync_communities().await;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let Some(channel_id) = channel_id else {
        eprintln!("!! channel \"{channel_name}\" never appeared; giving up");
        std::process::exit(1);
    };
    println!("── channel \"{channel_name}\" = {channel_id}");

    match bot.channel(&channel_id).send(&text).await {
        Ok(id) => println!("── sent ✅  message id {id}"),
        Err(e) => {
            eprintln!("!! send failed: {e}");
            std::process::exit(1);
        }
    }
    // Give the gift-wrap publish + persistence a moment to flush before exit.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    Ok(())
}
