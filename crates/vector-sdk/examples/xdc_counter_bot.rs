//! The smallest bot-backed Mini App: a number everyone in a chat shares.
//!
//! Say `!counter` in a DM with the bot, or in a Community channel it's in, and
//! it sends the Counter app. Whoever opens it sees the same number and can
//! change it; the bot keeps the count for each shared copy of the app.
//!
//! The app (`xdc_counter/`) and the bot speak three messages, JSON tagged by `t`:
//!
//! - app → bot  `hello`: what's the count?
//! - app → bot  `add {by}`: change it by +1 or -1
//! - bot → app  `count {value}`: here it is
//!
//! Run:  cargo run --example xdc_counter_bot --features xdc

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vector_sdk::{VectorBot, XdcEvent};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    // `public()` accepts Community invites, so anyone can add the bot to theirs.
    let bot = VectorBot::builder().data_dir("./xdc-counter-data").public().build().await?;
    let app = package_app("./xdc-counter-data/counter.xdc")?;
    println!("Counter bot online as {}", bot.npub());

    // One count per shared copy of the app, kept while the bot runs.
    let counts: Arc<Mutex<HashMap<String, i64>>> = Arc::default();

    // Runs each time someone opens the app, for as long as they have it open.
    bot.xdc("vector-counter").run(move |_bot, mut session| {
        let counts = counts.clone();
        async move {
            let copy = session.app().message_id().to_string();
            while let Some(event) = session.next().await {
                match event {
                    // Someone arrived: show them the count (their own `hello` may have
                    // gone out before the bot was connected).
                    XdcEvent::PeerJoined(_) => {}
                    XdcEvent::Data(frame) => {
                        let Some(msg) = frame.json::<Value>() else { continue };
                        match msg["t"].as_str() {
                            Some("hello") => {}
                            Some("add") => {
                                let by = msg["by"].as_i64().unwrap_or(0).clamp(-1, 1);
                                *counts.lock().unwrap().entry(copy.clone()).or_default() += by;
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
                }
                let value = counts.lock().unwrap().get(&copy).copied().unwrap_or(0);
                let _ = session.send_json(&json!({ "t": "count", "value": value })).await;
            }
        }
    });

    // `!counter` anywhere the bot can read hands out the app.
    bot.on_message(move |_bot, msg| {
        let app = app.clone();
        async move {
            if !msg.is_mine() && msg.text().trim().eq_ignore_ascii_case("!counter") {
                if let Err(e) = msg.channel().send_xdc(&app).await {
                    eprintln!("could not send the app: {e}");
                }
            }
        }
    })
    .await
}

/// Zip the app's files (beside this example) into the `.xdc` the bot sends.
fn package_app(path: &str) -> vector_sdk::Result<std::path::PathBuf> {
    let files: [(&str, &[u8]); 2] = [
        ("index.html", include_bytes!("xdc_counter/index.html")),
        ("manifest.toml", include_bytes!("xdc_counter/manifest.toml")),
    ];
    let path = std::path::PathBuf::from(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        zip.start_file(name, opts).map_err(|e| vector_sdk::Error::Other(e.to_string()))?;
        zip.write_all(bytes)?;
    }
    zip.finish().map_err(|e| vector_sdk::Error::Other(e.to_string()))?;
    Ok(path)
}
