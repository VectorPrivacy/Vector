//! A bot that is the backend of its own Mini App: it sends people "Oracle", a
//! small chat app, and answers whatever they ask in it with an LLM, streaming
//! each answer to everyone who has the app open.
//!
//! The pattern for any app that needs more than a phone can run (a model, a
//! database, a game server): the `.xdc` is the frontend, the bot is the
//! server, the realtime channel is the wire between them. The protocol here
//! is the example's own, JSON tagged by `t`:
//!
//! - app → bot  `hello` / `ask {id, text, name}`
//! - bot → app  `ready {model, transcript}` / `chunk {id, text}` / `done {id, text}`
//!
//! The channel is best-effort, so `done` repeats the whole answer: a client
//! that dropped a chunk still ends up with the right text. And it is
//! ephemeral, so `ready` carries the transcript to whoever opens the app late.
//!
//! ```sh
//! cargo run --example xdc_oracle_bot --features xdc
//! # optional, for real answers: OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL
//! ```
//! Without a key it answers as a (very confident) fortune teller.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vector_sdk::vector_core::net::build_http_client;
use vector_sdk::{VectorBot, XdcEvent, XdcSession};

const DATA_DIR: &str = "./xdc-oracle-data";
const SYSTEM_PROMPT: &str = "You are the Oracle, answering inside a small app in a private group chat. Be brief and vivid.";

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let bot = VectorBot::builder().data_dir(DATA_DIR).build().await?;
    let app = package_app()?;
    println!("Oracle online as {} (app: {})", bot.npub(), app.display());

    bot.xdc("vector-oracle").run(|_bot, session| async move {
        serve(session).await;
    });

    // Say anything to the bot and it hands you the app (once every 10 minutes per chat).
    let sent: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
    bot.on_message(move |_bot, msg| {
        let (sent, app) = (sent.clone(), app.clone());
        async move {
            if msg.is_mine() || msg.xdc().is_some() {
                return;
            }
            {
                let mut sent = sent.lock().unwrap();
                if sent.get(&msg.chat_id).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
                    return;
                }
                sent.insert(msg.chat_id.clone(), Instant::now());
            }
            match msg.channel().send_xdc(&app).await {
                Ok(_) => {
                    let _ = msg.reply("Open the Oracle and ask away.").await;
                }
                Err(e) => eprintln!("could not send the app: {e}"),
            }
        }
    })
    .await
}

/// One open copy of the app: answer its questions until everyone leaves.
async fn serve(mut session: XdcSession) {
    let me = session.self_addr().to_string();
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "fortune teller".into());
    let mut transcript: Vec<Value> = Vec::new();
    while let Some(event) = session.next().await {
        let XdcEvent::Data(frame) = event else { continue };
        let Some(m) = frame.json::<Value>() else { continue };
        match m["t"].as_str() {
            Some("hello") => {
                let _ = session.send_json(&json!({ "t": "ready", "by": me, "model": model, "transcript": transcript })).await;
            }
            Some("ask") => {
                let (Some(id), Some(text)) = (m["id"].as_str(), m["text"].as_str()) else { continue };
                // Everyone open already got the `ask` itself: the channel is a mesh.
                let asker = frame.from.npub.clone().unwrap_or_default();
                let name = m["name"].as_str().unwrap_or("someone").to_string();
                let answer = answer(&session, &me, id, text).await;
                let _ = session.send_json(&json!({ "t": "done", "by": me, "id": id, "text": answer })).await;
                transcript.push(json!({ "id": id, "q": text, "a": answer, "asker": asker, "name": name }));
                transcript.drain(..transcript.len().saturating_sub(20));
            }
            _ => {}
        }
    }
}

/// Stream an answer into the session chunk by chunk, and return all of it.
async fn answer(session: &XdcSession, me: &str, id: &str, question: &str) -> String {
    let mut full = String::new();
    let mut rx = if std::env::var("OPENAI_API_KEY").is_ok() {
        match stream_llm(question).await {
            Ok(rx) => rx,
            Err(e) => return format!("The mists are thick today ({e})."),
        }
    } else {
        fortune(question)
    };
    while let Some(piece) = rx.recv().await {
        let _ = session.send_json(&json!({ "t": "chunk", "by": me, "id": id, "text": piece })).await;
        full.push_str(&piece);
    }
    full
}

/// An OpenAI-compatible streaming completion, as a channel of text pieces.
async fn stream_llm(question: &str) -> Result<tokio::sync::mpsc::Receiver<String>, String> {
    let base = std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    let key = std::env::var("OPENAI_API_KEY").map_err(|e| e.to_string())?;
    let mut resp = build_http_client(Duration::from_secs(120))?
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(&json!({
            "model": model, "stream": true,
            "messages": [{ "role": "system", "content": SYSTEM_PROMPT }, { "role": "user", "content": question }],
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut buf = String::new();
        while let Ok(Some(bytes)) = resp.chunk().await {
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf.drain(..=nl);
                let Some(data) = line.strip_prefix("data: ") else { continue };
                if data == "[DONE]" {
                    return;
                }
                let delta = serde_json::from_str::<Value>(data).ok().and_then(|v| v["choices"][0]["delta"]["content"].as_str().map(str::to_string));
                if let Some(piece) = delta.filter(|p| !p.is_empty()) {
                    if tx.send(piece).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    Ok(rx)
}

/// The keyless stand-in, streamed a word at a time like a model would.
fn fortune(question: &str) -> tokio::sync::mpsc::Receiver<String> {
    const SAYINGS: [&str; 6] = [
        "The stars say yes, though they are notoriously bad at details.",
        "Not today. Ask again when the kettle boils.",
        "Without a doubt, provided you bring snacks.",
        "The answer lies within. Also within: lunch.",
        "Signs point to a long, interesting detour.",
        "Yes, but only on a Tuesday.",
    ];
    let pick = question.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % SAYINGS.len();
    let text = format!("You ask: \"{}\". {}", question.trim(), SAYINGS[pick]);
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        for word in text.split_inclusive(' ') {
            tokio::time::sleep(Duration::from_millis(60)).await;
            if tx.send(word.to_string()).await.is_err() {
                return;
            }
        }
    });
    rx
}

/// Zip the app's files (beside this example) into an `.xdc` the bot can send.
fn package_app() -> vector_sdk::Result<std::path::PathBuf> {
    let files: [(&str, &[u8]); 3] = [
        ("index.html", include_bytes!("xdc_oracle/index.html")),
        ("manifest.toml", include_bytes!("xdc_oracle/manifest.toml")),
        ("icon.svg", include_bytes!("xdc_oracle/icon.svg")),
    ];
    std::fs::create_dir_all(DATA_DIR)?;
    let path = std::path::Path::new(DATA_DIR).join("oracle.xdc");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        zip.start_file(name, opts).map_err(|e| vector_sdk::Error::Other(e.to_string()))?;
        zip.write_all(bytes)?;
    }
    zip.finish().map_err(|e| vector_sdk::Error::Other(e.to_string()))?;
    Ok(path)
}
