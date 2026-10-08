//! An echo bot that lives on I2P. Every connection goes through the I2P router already running
//! on this machine (i2pd, or Java I2P with its SAM bridge on); relays outside I2P are reached
//! through an outproxy, or not at all with `I2P_ONLY=1`.
//!
//! Run with:
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector_sdk --features i2p --example i2p_bot
//! # another SAM port, or I2P relays only:
//! SAM_PORT=7657 I2P_ONLY=1 VECTOR_NSEC=nsec1... cargo run -p vector_sdk --features i2p --example i2p_bot
//! ```

use vector_sdk::{I2pOptions, VectorBot};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");
    let opts = I2pOptions {
        sam_port: std::env::var("SAM_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7656),
        i2p_only: std::env::var("I2P_ONLY").is_ok_and(|v| v == "1"),
        ..I2pOptions::default()
    };

    // Waits for the router's tunnels before anything connects; fails with the router's reason.
    let bot = VectorBot::builder().nsec(nsec).i2p_with(opts).build().await?;
    println!("I2P bot online as {}", bot.npub());

    bot.on_message(|_bot, msg| async move {
        if msg.is_mine() {
            return;
        }
        let _ = msg.reply(&format!("Echo over I2P: {}", msg.text())).await;
    })
    .await?;

    Ok(())
}
