//! Learn a Mini App's protocol: join every app opened in a chat with the bot
//! and print each frame its players send, as text when it is text.
//!
//! The first step in writing a bot for an app you didn't write. Share the app
//! with the bot, open it, play a little, and read off the messages.
//! It is a visible participant: players see it in the app's lobby.
//!
//! Run:  cargo run --example xdc_spy --features xdc

use std::time::Instant;

use vector_sdk::{VectorBot, XdcEvent};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let bot = VectorBot::builder().data_dir("./xdc-spy-data").build().await?;
    println!("xdc_spy online as {}", bot.npub());

    bot.xdc("*").run(|_bot, mut session| async move {
        let app = session.app().clone();
        let name = app.manifest().await.map(|m| m.name).unwrap_or_else(|_| app.file_name().to_string());
        let t0 = Instant::now();
        println!("== {name} in {} (topic {})", app.chat_id(), app.topic().unwrap_or("-"));
        while let Some(event) = session.next().await {
            let at = t0.elapsed().as_secs_f32();
            match event {
                XdcEvent::Data(f) => {
                    let who = f.from.npub.as_deref().map(|n| &n[..16]).unwrap_or("?");
                    let mark = if f.direct { "" } else { " (relayed)" };
                    match f.text() {
                        Some(t) => println!("{at:>7.1}s {who}{mark}: {t}"),
                        None => println!("{at:>7.1}s {who}{mark}: {} binary bytes", f.payload.len()),
                    }
                }
                XdcEvent::PeerJoined(p) => println!("{at:>7.1}s + {}", p.npub.unwrap_or_else(|| "unknown node".into())),
                XdcEvent::PeerLeft(p) => println!("{at:>7.1}s - {}", p.npub.unwrap_or_else(|| "unknown node".into())),
                XdcEvent::Lagged => println!("{at:>7.1}s (frames dropped)"),
            }
        }
        println!("== {name} ended");
    });

    bot.on_message(|_, _| async {}).await
}
