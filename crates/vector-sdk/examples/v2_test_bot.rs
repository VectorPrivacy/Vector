//! A universal Concord **v2 test bot**: join a community by its invite LINK, log
//! the entire inbound event stream, and expose a command console that fires every
//! SDK send-path so a human on the other side (Armada / Vector) can verify each
//! feature round-trips live.
//!
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example v2_test_bot -- "https://vectorapp.io/invite/naddr1...#token"
//! # or:  VECTOR_INVITE="https://…" VECTOR_NSEC=nsec1... cargo run -p vector-sdk --example v2_test_bot
//! ```
//!
//! Once it's in the channel, message it any of:
//!   !help  !ping  !reply  !react  !edit  !delete  !typing  !file
//!   !members  !channels  !caps  !roles  !info  !whoami
//! Non-command messages are logged but not replied to. Every reaction / edit /
//! delete / join / leave / typing / invite / kick it *receives* is printed to
//! stdout, so you watch both directions.

use nostr_sdk::prelude::{Keys, ToBech32};
use std::time::{SystemTime, UNIX_EPOCH};
use vector_sdk::{BotEvent, VectorBot};

const HELP: &str = "\
Concord v2 test bot — commands:
  !help      — this menu
  !ping      — pong (send round-trip)
  !reply     — a threaded reply to your message (reply context)
  !react     — react 🔥 to your message
  !edit      — send a message, then edit it
  !delete    — send a message, then delete it
  !typing    — emit a typing indicator
  !file      — send a small text attachment (encrypt → Blossom → imeta)
  !members   — the folded member list
  !channels  — the channels I can see
  !caps      — my capabilities here (roles engine)
  !roles     — the community roster
  !info      — community id, protocol version, owner, channel count
  !whoami    — my npub + this channel id
  (non-command messages are ignored)";

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    // Reuse VECTOR_NSEC if set, else mint a throwaway identity and print it.
    let nsec = match std::env::var("VECTOR_NSEC") {
        Ok(n) => n,
        Err(_) => {
            let nsec = Keys::generate().secret_key().to_bech32().expect("bech32");
            println!("── no VECTOR_NSEC set; minted a throwaway identity (export this to reuse it):");
            println!("   VECTOR_NSEC={nsec}");
            nsec
        }
    };
    let invite = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("VECTOR_INVITE").ok())
        .expect("pass the invite link as the first arg or set VECTOR_INVITE");

    let bot = VectorBot::builder().nsec(nsec).build().await?;
    println!("── v2 test bot online as {}", bot.npub());

    // Join the community from its LINK (naddr#fragment). join_community routes v2
    // links through accept_public_link under the hood.
    println!("── joining via link…");
    match bot.core().join_community(&invite).await {
        Ok(summary) => {
            let id = summary.get("community_id").or_else(|| summary.get("id")).and_then(|v| v.as_str()).unwrap_or("?");
            let ver = summary.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
            let name = summary.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            println!("── joined \"{name}\"  id={id}  protocol=v{ver}");
        }
        Err(e) => {
            eprintln!("!! join failed: {e}");
            return Err(e);
        }
    }

    // Show what we can see right after joining (proves the fold landed). The join
    // snapshot folds only owner-authored channels; admin-created ones arrive on the
    // first control follow, so re-print once that has had time to land.
    print_channels(&bot, "channels visible (join snapshot)").await;
    {
        let bot = bot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(25)).await;
            print_channels(&bot, "channels visible (after control follow)").await;
        });
    }
    println!("── listening. Message me `!help` from your client.\n");

    bot.on_event(|bot, event| async move {
        match event {
            BotEvent::Message(msg) => {
                if msg.is_mine() {
                    return; // never react to our own echoes
                }
                let author = short(msg.message.npub.as_deref().unwrap_or("?"));
                let text = msg.text().trim().to_string();
                println!("[MSG]    {author}: {text}");

                // Command console — each arm fires a distinct SDK send-path.
                let ch = msg.channel();
                match text.as_str() {
                    "!help" => reply(&msg, HELP).await,
                    "!ping" => reply(&msg, &format!("pong 🏓 ({} ms)", now_ms())).await,
                    "!reply" => reply(&msg, "this is a threaded reply ✅ (I quoted your message)").await,
                    "!react" => log_err("react", msg.react("🔥").await.map(|_| String::new())),
                    "!typing" => log_err("typing", ch.typing().await.map(|_| String::new())),
                    "!edit" => {
                        if let Ok(id) = ch.send("editing this in one second…").await {
                            log_err("edit", ch.edit(&id, "edited ✏️ (this text was changed)").await.map(|_| String::new()));
                        }
                    }
                    "!delete" => {
                        if let Ok(id) = ch.send("…this message will self-destruct").await {
                            log_err("delete", ch.delete(&id).await.map(|_| String::new()));
                        }
                    }
                    "!file" => send_test_file(&ch).await,
                    "!members" => {
                        if let Some(community) = msg.community() {
                            let members = community.members().await;
                            let list: Vec<String> = members.iter().map(|m| short(m.npub())).collect();
                            reply(&msg, &format!("{} member(s): {}", members.len(), list.join(", "))).await;
                        } else {
                            reply(&msg, "not in a community here").await;
                        }
                    }
                    "!channels" | "!info" | "!caps" | "!roles" | "!whoami" => diagnostics(&bot, &msg, &text).await,
                    other if other.starts_with('!') => reply(&msg, &format!("unknown command `{other}` — try !help")).await,
                    // Non-command chatter is logged above but never replied to, so
                    // the bot can sit in a busy community without spamming echoes.
                    _ => {}
                }
            }
            BotEvent::MessageUpdate { message, .. } => {
                println!("[UPDATE] {} → \"{}\"  ({} reaction(s))", short(message.npub.as_deref().unwrap_or("?")), message.content, message.reactions.len());
            }
            BotEvent::Delete { message_id, .. } => println!("[DELETE] message {}", short(&message_id)),
            BotEvent::MemberJoin { npub, .. } => println!("[JOIN]   {}", short(&npub)),
            BotEvent::MemberLeave { npub, .. } => println!("[LEAVE]  {}", short(&npub)),
            BotEvent::Typing { npub, .. } => println!("[TYPING] {}", short(&npub)),
            BotEvent::Invite { community_id } => println!("[INVITE] for community {}", short(&community_id)),
            BotEvent::Removed { community_id } => println!("[REMOVED] from community {} — I was kicked/banned", short(&community_id)),
        }
    })
    .await?;

    Ok(())
}

/// Reply (threaded) to the triggering message, logging any failure.
async fn reply(msg: &vector_sdk::IncomingMessage, text: &str) {
    if let Err(e) = msg.reply(text).await {
        eprintln!("!! reply failed: {e}");
    }
}

/// Print every v2 community's channel names under `label`.
async fn print_channels(bot: &VectorBot, label: &str) {
    for c in bot.core().list_communities().await {
        if c.get("version").and_then(|v| v.as_u64()) == Some(2) {
            let chans: Vec<String> = c
                .get("channels")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|ch| ch.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            println!("── {label}: {chans:?}");
        }
    }
}

/// Report richer diagnostics. `!caps`/`!roles`/`!whoami` come off the OO Community;
/// `!channels`/`!info` come off the core facade summary (channels + protocol version).
async fn diagnostics(bot: &VectorBot, msg: &vector_sdk::IncomingMessage, which: &str) {
    let Some(community) = msg.community() else {
        reply(msg, "not in a community here").await;
        return;
    };
    let cid = community.id().to_string();
    let out = match which {
        "!whoami" => format!("me: {}  ·  this channel/community: {}", bot.npub(), cid),
        "!caps" => community.capabilities().map(|v| v.to_string()).unwrap_or_else(|e| format!("caps error: {e}")),
        "!roles" => community.roles().map(|v| v.to_string()).unwrap_or_else(|e| format!("roles error: {e}")),
        _ /* !channels / !info */ => {
            let mut line = format!("community {cid}: (not found in list)");
            for c in bot.core().list_communities().await {
                let id = c.get("id").or_else(|| c.get("community_id")).and_then(|v| v.as_str()).unwrap_or("");
                if id != cid {
                    continue;
                }
                let ver = c.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                let owner = c.get("is_owner").and_then(|v| v.as_bool()).unwrap_or(false);
                let chans: Vec<String> = c.get("channels").and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|ch| ch.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                    .unwrap_or_default();
                line = format!("protocol=v{ver}  owner={owner}  channels={chans:?}");
            }
            line
        }
    };
    reply(msg, &out).await;
}

/// Write a tiny file and send it as an encrypted attachment.
async fn send_test_file(ch: &vector_sdk::Channel) {
    let path = std::env::temp_dir().join(format!("v2_test_bot_{}.txt", now_ms()));
    if let Err(e) = std::fs::write(&path, format!("Hello from the Concord v2 test bot!\nsent at {} ms\n", now_ms())) {
        eprintln!("!! temp file write failed: {e}");
        return;
    }
    log_err("send_file", ch.send_file(&path).await.map(|_| String::new()));
    let _ = std::fs::remove_file(&path);
}

fn log_err(what: &str, r: vector_sdk::Result<String>) {
    if let Err(e) = r {
        eprintln!("!! {what} failed: {e}");
    }
}

/// A short npub/id for readable logs.
fn short(s: &str) -> String {
    if s.len() > 14 {
        format!("{}…{}", &s[..10], &s[s.len() - 4..])
    } else {
        s.to_string()
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
