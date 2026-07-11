//! **Concordia** — a multi-purpose Concord (v2) community bot.
//!
//! Joins communities by invite link, answers a command console covering every
//! SDK send-path (message, threaded reply, reaction, edit, delete, typing,
//! encrypted file), and reports community diagnostics (members, channels,
//! roles, capabilities). Non-command chatter is logged but never replied to,
//! so it can sit quietly in busy channels.
//!
//! ```sh
//! # First run: join a community and (optionally) set the profile in one go.
//! CONCORDIA_AVATAR=~/Downloads/concord-icon.webp \
//!   cargo run -p vector_sdk --example concordia -- "https://…/invite/naddr1…#token"
//!
//! # Later runs: identity + memberships persist, no arguments needed.
//! cargo run -p vector_sdk --example concordia
//! ```
//!
//! Identity lives at `<data_dir>/identity.nsec` (created on first run; back it
//! up — that file IS the bot). `VECTOR_NSEC` overrides it. Set
//! `CONCORDIA_AVATAR=<image path>` on any run to (re)publish the bot profile
//! with that avatar.

use std::time::{SystemTime, UNIX_EPOCH};
use vector_sdk::{BotEvent, VectorBot};

const NAME: &str = "Concordia";
const ABOUT: &str = "A multi-purpose Concord community bot. Type / (or !help) for commands.";

const HELP: &str = "\
Concordia — a multi-purpose Concord bot (type / in a modern client for the picker):
  /help              — this menu
  /ping              — pong (send round-trip)
  /roll [sides]      — roll a die
  /announce <t> <b>  — format a two-part announcement
  /reply             — a threaded reply to your message (reply context)
  /react [emoji]     — react to your message
  /edit              — send a message, then edit it
  /delete            — send a message, then delete it
  /typing            — emit a typing indicator
  /file              — send a small text attachment (encrypt → Blossom → imeta)
  /members           — the folded member list
  /channels          — the channels I can see
  /caps              — my capabilities here (roles engine)
  /roles             — the community roster
  /info              — community id, protocol version, owner, channel count
  /whoami            — my npub + this channel id
  /reconnect         — bounce my relay sockets (reconnect drill)
  (legacy !commands still work; non-command messages are ignored)";

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    // Identity: VECTOR_NSEC override, else the persisted <data_dir>/identity.nsec
    // (created on first run by the builder). Public invite policy: any member can
    // pull the bot into their community with a direct invite (v1 or v2).
    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    let bot = builder.build().await?;
    println!("── {NAME} online as {}", bot.npub());

    // Optional one-shot profile publish: upload the avatar plainly (avatars are
    // public) and publish the kind-0 with the bot flag. Existing fields are
    // carried forward by the profile pipeline, so this never clobbers. Deferred
    // until after listen() has connected the community relays — a kind-0 that
    // only reaches the login relays is invisible to community peers' clients.
    if let Ok(avatar_path) = std::env::var("CONCORDIA_AVATAR") {
        let bot = bot.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let avatar_path = shellexpand_home(&avatar_path);
            println!("── uploading avatar {avatar_path}…");
            match bot.core().upload_public_image(&avatar_path).await {
                Ok(url) => {
                    let ok = bot.core().update_bot_profile(NAME, &url, "", ABOUT).await;
                    println!("── profile publish {}  avatar={url}", if ok { "✅" } else { "FAILED" });
                    if ok {
                        push_profile_to_communities().await;
                    }
                }
                Err(e) => eprintln!("!! avatar upload failed: {e}"),
            }
        });
    }

    // Optional invite link: join on first run; later runs come up with the
    // persisted memberships and need no arguments.
    if let Some(invite) = std::env::args().nth(1).or_else(|| std::env::var("VECTOR_INVITE").ok()) {
        println!("── joining via link…");
        match bot.core().join_community(&invite).await {
            Ok(summary) => {
                let name = summary.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let ver = summary.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("── joined \"{name}\"  protocol=v{ver}");
            }
            Err(e) => {
                eprintln!("!! join failed: {e}");
                return Err(e);
            }
        }
    }

    // Slash commands (Bot Interface Phase 1): registered here, published as the
    // kind-33304 manifest at listen start, invoked by PLAIN TEXT from any client.
    // Vector's `/` picker renders exactly this registry with argument hints; a
    // matched invocation is consumed before the legacy !bang console below.
    bot.command("help", "List everything Concordia can do").run(|ctx| async move {
        let _ = ctx.reply(HELP).await;
    });
    bot.command("ping", "Round-trip latency check").run(|ctx| async move {
        let _ = ctx.reply(format!("pong 🏓 ({} ms)", now_ms())).await;
    });
    bot.command("roll", "Roll a die")
        .int("sides", "How many sides (default 6)", false)
        .run(|ctx| async move {
            let sides = ctx.int("sides").unwrap_or(6).clamp(2, 1_000_000);
            let roll = (now_ms() as i64 % sides) + 1;
            let _ = ctx.reply(format!("🎲 rolled {roll} on a d{sides}")).await;
        });
    bot.command("announce", "Format a two-part announcement")
        .string("title", "Announcement title", true)
        .string("body", "Announcement body", true)
        .run(|ctx| async move {
            let title = ctx.str("title").unwrap_or("(untitled)").to_string();
            let body = ctx.str("body").unwrap_or_default().to_string();
            let _ = ctx.reply(format!("📣 {title}\n{body}")).await;
        });
    bot.command("reply", "Get a threaded reply to your message").run(|ctx| async move {
        let _ = ctx.reply("this is a threaded reply ✅ (I quoted your message)").await;
    });
    bot.command("react", "React to your message")
        .choice("emoji", "Which reaction", ["🔥", "👍", "❤️", "😂", "🫡"], false)
        .run(|ctx| async move {
            let emoji = ctx.str("emoji").unwrap_or("🔥").to_string();
            log_err("react", ctx.msg.react(&emoji).await.map(|_| String::new()));
        });
    bot.command("edit", "Watch me send then edit a message").run(|ctx| async move {
        let ch = ctx.msg.channel();
        if let Ok(id) = ch.send("editing this in one second…").await {
            log_err("edit", ch.edit(&id, "edited ✏️ (this text was changed)").await.map(|_| String::new()));
        }
    });
    bot.command("delete", "Watch a message self-destruct").run(|ctx| async move {
        let ch = ctx.msg.channel();
        if let Ok(id) = ch.send("…this message will self-destruct").await {
            log_err("delete", ch.delete(&id).await.map(|_| String::new()));
        }
    });
    bot.command("typing", "Emit a typing indicator").run(|ctx| async move {
        log_err("typing", ctx.msg.channel().typing().await.map(|_| String::new()));
    });
    bot.command("file", "Receive a small encrypted attachment").run(|ctx| async move {
        send_test_file(&ctx.msg.channel()).await;
    });
    bot.command("members", "The folded member list").run(|ctx| async move {
        if let Some(community) = ctx.msg.community() {
            let members = community.members().await;
            let list: Vec<String> = members.iter().map(|m| short(m.npub())).collect();
            let _ = ctx.reply(format!("{} member(s): {}", members.len(), list.join(", "))).await;
        } else {
            let _ = ctx.reply("not in a community here").await;
        }
    });
    bot.command("channels", "The channels I can see here").run(|ctx| async move {
        diagnostics(&ctx.bot, &ctx.msg, "!channels").await;
    });
    bot.command("caps", "My capabilities here (roles engine)").run(|ctx| async move {
        diagnostics(&ctx.bot, &ctx.msg, "!caps").await;
    });
    bot.command("roles", "The community roster").run(|ctx| async move {
        diagnostics(&ctx.bot, &ctx.msg, "!roles").await;
    });
    bot.command("info", "Community protocol + ownership summary").run(|ctx| async move {
        diagnostics(&ctx.bot, &ctx.msg, "!info").await;
    });
    bot.command("whoami", "My npub and this channel id").run(|ctx| async move {
        diagnostics(&ctx.bot, &ctx.msg, "!whoami").await;
    });
    bot.command("reconnect", "Bounce my relay sockets (reconnect drill)").run(|ctx| async move {
        let bounced = bounce_community_relays().await;
        let _ = ctx.reply(format!("bounced {bounced} relay socket(s) — /ping to test the post-reconnect subscription")).await;
    });

    // The join snapshot folds only owner-authored channels; admin-created ones
    // arrive on the first control follow, so re-print once that has landed.
    print_channels(&bot, "channels visible (startup)").await;
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
                    return; // never react to our own sends
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
                    "!reconnect" => {
                        // Ops/repro tool: bounce this bot's community relay sockets (a
                        // relay restart or idle drop in miniature) — then a !ping proves
                        // whether the subscription survived the fresh AUTH gate.
                        let bounced = bounce_community_relays().await;
                        reply(&msg, &format!("bounced {bounced} relay socket(s) — send !ping to test the post-reconnect subscription")).await;
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

/// Public relays that index profile metadata for the whole network — the
/// fallback clients use to resolve an author they've never seen. Ditto-family
/// community relays silently drop a stranger's kind-0 (accepted, never stored),
/// so for a bot these indexers are the RELIABLE path to a rendered name+avatar.
const PROFILE_INDEXERS: &[&str] = &["wss://purplepag.es", "wss://relay.nostr.band", "wss://relay.damus.io", "wss://nos.lol"];

/// Community relays are pool-isolated from profile ops by design (the GOSSIP
/// flag keeps pool-wide DM/profile publishes off them), so the kind-0 above
/// only reached the login relays — which community peers' clients never read.
/// Re-target the freshly published metadata at every held community's relays
/// (best-effort; Ditto drops it) plus the profile indexers.
async fn push_profile_to_communities() {
    use nostr_sdk::prelude::{Filter, Kind};
    let Some(client) = vector_core::state::nostr_client() else { return };
    let Some(me) = vector_core::state::my_public_key() else { return };
    let filter = Filter::new().kind(Kind::Metadata).author(me).limit(1);
    let Ok(evs) = client.fetch_events(filter, std::time::Duration::from_secs(8)).await else {
        eprintln!("!! could not fetch own kind-0 back for the community push");
        return;
    };
    let Some(ev) = evs.into_iter().next() else { return };
    let mut targets: Vec<String> = PROFILE_INDEXERS.iter().map(|s| s.to_string()).collect();
    for id in vector_core::db::community::list_community_ids().unwrap_or_default() {
        if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) {
            targets.extend(c.relays.clone());
        }
    }
    for t in &targets {
        let _ = client.add_relay(t).await;
    }
    client.connect().await;
    match client.send_event_to(targets, &ev).await {
        Ok(out) => println!("── profile pushed: stored on {} relay(s), refused by {}", out.success.len(), out.failed.len()),
        Err(e) => eprintln!("!! profile push failed: {e}"),
    }
}

/// Print every v2 community's channel names under `label`.
async fn print_channels(bot: &VectorBot, label: &str) {
    for c in bot.core().list_communities().await {
        if c.get("version").and_then(|v| v.as_u64()) == Some(2) {
            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let chans: Vec<String> = c
                .get("channels")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|ch| ch.get("name").and_then(|n| n.as_str()).map(String::from)).collect())
                .unwrap_or_default();
            println!("── {label} in \"{name}\": {chans:?}");
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

/// Disconnect every held community's relay sockets, then reconnect the pool —
/// the miniature of a relay restart / idle drop. Returns how many were bounced.
async fn bounce_community_relays() -> usize {
    let Some(client) = vector_core::state::nostr_client() else { return 0 };
    let mut bounced = 0;
    for id in vector_core::db::community::list_community_ids().unwrap_or_default() {
        if let Ok(Some(c)) = vector_core::db::community::load_community_v2(&id) {
            for r in &c.relays {
                if let Ok(url) = nostr_sdk::prelude::RelayUrl::parse(r) {
                    if let Ok(relay) = client.pool().relay(url).await {
                        relay.disconnect();
                        bounced += 1;
                    }
                }
            }
        }
    }
    client.connect().await;
    bounced
}

/// Write a tiny file and send it as an encrypted attachment.
async fn send_test_file(ch: &vector_sdk::Channel) {
    let path = std::env::temp_dir().join(format!("concordia_{}.txt", now_ms()));
    if let Err(e) = std::fs::write(&path, format!("Hello from {NAME}!\nsent at {} ms\n", now_ms())) {
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

/// Expand a leading `~/` so a pasted `~/Downloads/…` path works as expected.
fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
