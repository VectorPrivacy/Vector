//! An AI chatbot. Replies to every message with an LLM completion, showing a
//! "typing…" indicator while it thinks and threading its answer to your message.
//! Keeps a short rolling history per conversation, so it follows context.
//!
//! Works in DMs and Community channels alike — the SDK hides the difference, so
//! the entire "make it an AI" part is just the `ask_llm` call below.
//!
//! Point it at any OpenAI-compatible chat-completions endpoint:
//! ```sh
//! OPENAI_API_KEY=sk-...  \
//! VECTOR_NSEC=nsec1...   \
//! cargo run -p vector-sdk --example ai_bot
//! # optional: OPENAI_BASE_URL (default https://api.openai.com/v1), OPENAI_MODEL (default gpt-4o-mini)
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use vector_sdk::vector_core::net::build_http_client;
use vector_sdk::VectorBot;

const SYSTEM_PROMPT: &str =
    "You are a friendly, concise assistant chatting inside Vector, a private messenger. Keep replies short.";
const HISTORY_LIMIT: usize = 16; // recent user/assistant messages kept per conversation

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let nsec = std::env::var("VECTOR_NSEC").expect("set VECTOR_NSEC to your bot's nsec");
    std::env::var("OPENAI_API_KEY").expect("set OPENAI_API_KEY for the LLM endpoint");

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("AI bot online as {}", bot.npub());

    // Per-conversation rolling history, shared across handler invocations.
    let history: Arc<Mutex<HashMap<String, Vec<Value>>>> = Arc::new(Mutex::new(HashMap::new()));

    bot.on_message(move |_bot, msg| {
        let history = history.clone();
        async move {
            if msg.is_mine() || msg.text().trim().is_empty() {
                return;
            }

            // Let the user see we're working while the model generates.
            let _ = msg.channel().typing().await;

            // Prompt = system + recent history for this conversation + the new turn.
            let key = msg.chat_id.clone();
            let mut messages = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];
            if let Some(prior) = history.lock().unwrap().get(&key) {
                messages.extend(prior.iter().cloned());
            }
            messages.push(json!({ "role": "user", "content": msg.text() }));

            match ask_llm(&messages).await {
                Ok(answer) => {
                    let _ = msg.reply(&answer).await;
                    // Remember both sides, trimmed to the last HISTORY_LIMIT messages.
                    let mut store = history.lock().unwrap();
                    let convo = store.entry(key).or_default();
                    convo.push(json!({ "role": "user", "content": msg.text() }));
                    convo.push(json!({ "role": "assistant", "content": answer }));
                    let overflow = convo.len().saturating_sub(HISTORY_LIMIT);
                    convo.drain(0..overflow);
                }
                Err(e) => {
                    let _ = msg.reply(&format!("(LLM error: {e})")).await;
                }
            }
        }
    })
    .await?;

    Ok(())
}

/// Call an OpenAI-compatible `/chat/completions` endpoint and return the reply text.
async fn ask_llm(messages: &[Value]) -> Result<String, String> {
    let base =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    let key = std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY not set".to_string())?;

    // The SDK hands you a hardened HTTP client — same one Vector uses everywhere — so even your
    // LLM calls inherit the Tor failsafe and SSRF guards. No need to add `reqwest` yourself.
    let body: Value = build_http_client(Duration::from_secs(60))?
        .post(format!("{base}/chat/completions"))
        .bearer_auth(key)
        .json(&json!({ "model": model, "messages": messages }))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    body["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "no completion in response".to_string())
}
