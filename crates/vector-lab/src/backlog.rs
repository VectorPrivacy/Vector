//! An overnight gap, made on demand: a Community with many channels and some history, then,
//! once the client under test has joined and quit, thousands of messages, reactions, edits and
//! DMs posted while it is away. The roles wait on files the tester leaves in the lab directory:
//! With `--long` the channels are private. `target` (the test account's npub), `go`
//! (it has joined, synced after `ready`, and quit), `finish` (it has booted).

use crate::{Checks, Lab};
use serde_json::{json, Value};
use vector_sdk::*;

const CHANNELS: usize = 12;
const HISTORY: usize = 30;

pub async fn run(role: &str, lab: Lab) {
    match role {
        "owner" => owner(lab).await,
        bot => poster(bot, lab).await,
    }
}

async fn owner(lab: Lab) {
    let mut checks = Checks::new(&lab, "owner");
    let (bot, _seen) = crate::bot(&lab, "owner").await;
    let c = bot.create_community("vector-lab backlog", lab.community("backlog")).await.expect("a Community");
    let private = lab.opts.long;
    let mut channels = vec![c.primary_channel().expect("its first Channel").id().to_string()];
    for i in 1..CHANNELS {
        let made = if private { c.create_private_channel(&format!("lab-{i}")).await } else { c.create_channel(&format!("lab-{i}")).await };
        channels.push(made.expect("a channel"));
    }
    let invite = c.create_invite().await.expect("an invite");
    lab.put("state.json", &json!({ "invite": invite, "channels": channels }).to_string());
    println!("invite: {invite}");
    if private {
        // Every member reads every private channel: the bots, then the account under test.
        let mut members: Vec<String> = Vec::new();
        for i in 0..lab.opts.bots {
            members.push(lab.wait(&format!("npub-bot{i}"), 600).await.expect("a bot joined"));
        }
        members.push(lab.wait("target", 3600).await.expect("the test account").trim().to_string());
        for ch in &channels[1..] {
            for m in &members {
                let _ = c.grant_access(ch, m).await;
            }
        }
    }
    for ch in &channels {
        for k in 0..HISTORY {
            let _ = bot.channel(ch.clone()).send(&format!("history {k}")).await;
        }
    }
    lab.put("ready", "1");
    lab.wait("go", 3600).await;
    post(&lab, &bot, "owner", &channels).await;
    lab.put("posted-owner", "1");
    for i in 0..lab.opts.bots {
        lab.wait(&format!("posted-bot{i}"), 3600).await;
    }
    lab.put("backlog-done", "1");
    lab.wait("finish", 3600).await;
    let gone = c.dissolve().await;
    checks.check("dissolve", gone.is_ok(), crate::outcome(&gone));
}

async fn poster(who: &str, lab: Lab) {
    let mut checks = Checks::new(&lab, who);
    let (bot, _seen) = crate::bot(&lab, who).await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 1200).await.expect("the owner's Community")).unwrap();
    checks.check("join", bot.join(state["invite"].as_str().unwrap()).await.is_ok(), "by link");
    lab.put(&format!("npub-{who}"), &bot.npub());
    let channels: Vec<String> = state["channels"].as_array().unwrap().iter().filter_map(|c| c.as_str().map(String::from)).collect();
    lab.wait("go", 3600).await;
    let sent = post(&lab, &bot, who, &channels).await;
    checks.check("posted", sent > 0, format!("{sent} messages"));
    lab.put(&format!("posted-{who}"), "1");
}

/// This role's share of the gap: `--messages` per channel, every tenth reacted to, every
/// twentieth edited, and a few DMs to the test account. How many were sent.
async fn post(lab: &Lab, bot: &VectorBot, who: &str, channels: &[String]) -> usize {
    let target = lab.wait("target", 5).await.map(|s| s.trim().to_string());
    let mut sent = 0;
    for k in 0..lab.opts.messages {
        for ch in channels {
            let ch = bot.channel(ch.clone());
            let Ok(id) = ch.send(&format!("gap {who} {k}: while you were away")).await else { continue };
            sent += 1;
            if k % 10 == 0 {
                let _ = ch.react(&id, "👍").await;
            }
            if k % 20 == 0 {
                let _ = ch.edit(&id, &format!("gap {who} {k}: while you were away (edited)")).await;
            }
        }
        if let Some(t) = target.as_deref().filter(|_| k % 25 == 0) {
            let _ = bot.dm(t).send(&format!("dm {who} {k}")).await;
        }
    }
    sent
}
