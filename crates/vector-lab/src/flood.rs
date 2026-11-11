//! Several bots posting into one Channel at once: every message reaches every other bot once,
//! and how long it took.

use crate::{now_ms, Checks, Lab};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use vector_sdk::*;

pub async fn run(role: &str, lab: Lab) {
    match role {
        "owner" => owner(lab).await,
        bot => poster(bot, lab).await,
    }
}

async fn owner(lab: Lab) {
    let mut checks = Checks::new(&lab, "owner");
    let (bot, _seen) = crate::bot(&lab, "owner").await;
    let c = bot.create_community(&format!("vector-lab flood {}", now_ms() / 1000), lab.community("flood")).await.expect("a Community");
    let ch = c.primary_channel().expect("its first Channel");
    lab.put("state.json", &json!({ "invite": c.create_invite().await.expect("an invite"), "channel": ch.id() }).to_string());
    for i in 0..lab.opts.bots {
        lab.wait(&format!("done-bot{i}"), 800).await;
    }
    let gone = c.dissolve().await;
    checks.check("dissolve", gone.is_ok(), crate::outcome(&gone));
}

async fn poster(who: &str, lab: Lab) {
    let mut checks = Checks::new(&lab, who);
    let (bots, per) = (lab.opts.bots, lab.opts.messages);
    let (bot, seen) = crate::bot(&lab, who).await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 300).await.expect("the owner's Community")).unwrap();
    checks.check("join", bot.join(state["invite"].as_str().unwrap()).await.is_ok(), "by link");
    lab.put(&format!("joined-{who}"), "1");
    for i in 0..bots {
        lab.wait(&format!("joined-bot{i}"), 400).await;
    }
    // Everyone's live subscription settles before the first post.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let ch = bot.channel(state["channel"].as_str().unwrap());
    let t = Instant::now();
    let mut failed = 0;
    for k in 0..per {
        if ch.send(&format!("flood {who} {k}")).await.is_err() {
            failed += 1;
        }
    }
    checks.check("sent", failed == 0, format!("{per} in {:.1}s, {failed} failed", t.elapsed().as_secs_f32()));

    let want = (bots - 1) * per;
    let others = |e: &BotEvent| matches!(e, BotEvent::Message(m) if m.text().starts_with("flood ") && !m.text().starts_with(&format!("flood {who} ")));
    let until = Instant::now() + Duration::from_secs(120);
    while seen.count(others) < want && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut texts: Vec<String> = Vec::new();
    let mut latency: Vec<u64> = Vec::new();
    for (at, e) in seen.all() {
        if let BotEvent::Message(m) = &e {
            if others(&e) {
                texts.push(m.text().to_string());
                latency.push(at.saturating_sub(m.message.at));
            }
        }
    }
    let total = texts.len();
    texts.sort();
    texts.dedup();
    checks.check("every other bot's message, once", texts.len() == want && total == want, format!("{} of {want} distinct, {total} received", texts.len()));
    latency.sort_unstable();
    let pct = |p: usize| latency.get((latency.len() * p / 100).min(latency.len().saturating_sub(1))).copied().unwrap_or(0);
    checks.note("latency", format!("p50 {} ms, p99 {} ms", pct(50), pct(99)));
    lab.put(&format!("done-{who}"), "1");
    tokio::time::sleep(Duration::from_secs(3)).await;
    bot.logout().await;
}
