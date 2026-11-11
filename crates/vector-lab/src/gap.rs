//! A bot restarting while a Channel is busy: everything sent from its restart on arrives once,
//! whether before or after it's ready, and so does a Community change made meanwhile. With
//! `--long`, it stays away past the clock allowance first, and what was sent then stays history.

use crate::{now_ms, Checks, Lab};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use vector_sdk::*;

pub async fn run(role: &str, lab: Lab) {
    match role {
        "owner" => owner(lab).await,
        "away" => away(lab).await,
        _ => back(lab).await,
    }
}

async fn owner(lab: Lab) {
    let mut checks = Checks::new(&lab, "owner");
    let (bot, _seen) = crate::bot(&lab, "owner").await;
    let c = bot.create_community(&format!("vector-lab gap {}", now_ms() / 1000), lab.community("gap")).await.expect("a Community");
    let ch = c.primary_channel().expect("its first Channel");
    lab.put("state.json", &json!({ "invite": c.create_invite().await.expect("an invite"), "channel": ch.id() }).to_string());
    lab.wait("member-down", 300).await.expect("the member went away");
    let _ = ch.send("sent while it was away").await;
    let _ = c.create_role(RoleSpec::new("Made while away")).await;
    lab.put("posted-away", "1");

    lab.wait("member-starting", 900).await.expect("the member restarting");
    // From its restart on: a message every 700 ms for 20 s, a Role, a reaction and an edit.
    let (t, mut n, mut first) = (Instant::now(), 0usize, None);
    while t.elapsed() < Duration::from_secs(20) {
        n += 1;
        let Ok(id) = ch.send(&format!("restart #{n}")).await else { continue };
        first.get_or_insert(id.clone());
        match n {
            3 => drop(c.create_role(RoleSpec::new("Made at restart")).await),
            5 => drop(ch.react(first.as_ref().unwrap(), "🔥").await),
            7 => drop(ch.edit(&id, "restart #7, edited").await),
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    lab.put("sent", &n.to_string());
    checks.note("sent from the restart on", format!("{n} messages"));
    lab.wait("member-done", 300).await;
    let gone = c.dissolve().await;
    checks.check("dissolve", gone.is_ok(), crate::outcome(&gone));
}

/// The member's first run: it joins, then goes away.
async fn away(lab: Lab) {
    let mut checks = Checks::new(&lab, "away");
    let (bot, _seen) = crate::bot(&lab, "member").await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 300).await.expect("the owner's Community")).unwrap();
    checks.check("join", bot.join(state["invite"].as_str().unwrap()).await.is_ok(), "by link");
    tokio::time::sleep(Duration::from_secs(6)).await;
    bot.logout().await;
    lab.put("member-down", "1");
}

/// The member back, as the same identity, while the owner posts.
async fn back(lab: Lab) {
    let mut checks = Checks::new(&lab, "back");
    lab.wait("posted-away", 600).await.expect("the owner posted");
    if lab.opts.long {
        // Away longer than the clock allowance: what was sent meanwhile is history.
        tokio::time::sleep(Duration::from_millis(vector_sdk::vector_core::news::ALLOWANCE_MS + 30_000)).await;
    }

    lab.put("member-starting", "1");
    let started = Instant::now();
    let (bot, seen) = crate::bot(&lab, "member").await;
    let ready_after = started.elapsed();
    let sent: usize = lab.wait("sent", 300).await.and_then(|s| s.parse().ok()).unwrap_or(0);
    tokio::time::sleep(Duration::from_secs(15)).await;

    let texts: Vec<String> = seen.all().into_iter().filter_map(|(_, e)| if let BotEvent::Message(m) = e { Some(m.text().to_string()) } else { None }).collect();
    let (mut missing, mut twice) = (Vec::new(), Vec::new());
    for n in (1..=sent).filter(|&n| n != 7) {
        match texts.iter().filter(|t| **t == format!("restart #{n}")).count() {
            0 => missing.push(n),
            1 => {}
            _ => twice.push(n),
        }
    }
    checks.note("ready", format!("{:.1}s after restarting", ready_after.as_secs_f32()));
    checks.check("every message from the restart on, once", sent > 0 && missing.is_empty() && twice.is_empty(), format!("{sent} sent, missing {missing:?}, twice {twice:?}"));
    let role = |name: &'static str| seen.count(move |e| matches!(e, BotEvent::CommunityChange { change, .. } if matches!(&change.kind, ChangeKind::RoleCreated { role } if role.name == name)));
    checks.once("the Role made at restart", role("Made at restart"));
    checks.once("the reaction", seen.count(|e| matches!(e, BotEvent::Reaction { reaction, .. } if reaction.emoji == "🔥")));
    checks.once("the edit", seen.count(|e| matches!(e, BotEvent::Edit { message, .. } if message.content == "restart #7, edited")));
    if lab.opts.long {
        checks.check("what was sent while away stays history", texts.iter().all(|t| t != "sent while it was away"), "no Message");
        checks.check("the Role made while away is part of the start", role("Made while away") == 0, "no change");
    }
    lab.put("member-done", "1");
    bot.logout().await;
}
