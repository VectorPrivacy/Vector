//! A gate bot as communities run one: a member with Manage Roles and Manage Channels, not the
//! owner, hands a "Verified" Role to each newcomer. The Role opens a private room with its
//! history, a private room without it, and the right to post in a public Channel nobody else may
//! post in. Each newcomer times how long every one of those takes to reach them, and the last
//! leaves and comes back to time a return.

use crate::{bot, now_ms, Checks, Lab};
use serde_json::{json, Value};
use std::time::Duration;
use vector_sdk::*;

/// How long a newcomer sits before asking in, as someone solving a captcha would.
const SETTLE_SECS: u64 = 30;
const EARLIER: &str = "before anyone was let in";

pub async fn run(role: &str, lab: Lab) {
    match role {
        "owner" => owner(lab).await,
        "gate" => gate(lab).await,
        who => member(who, lab).await,
    }
}

fn members(lab: &Lab) -> Vec<String> {
    (0..lab.opts.bots).map(|i| format!("m{i}")).collect()
}

/// Poll `f` every 100 ms for up to `secs`; when it first held, in unix ms.
async fn until(secs: u64, mut f: impl AsyncFnMut() -> bool) -> Option<u64> {
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < secs {
        if f().await {
            return Some(now_ms());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

async fn holds(c: &Community, rooms: &[&str]) -> bool {
    let held = c.channels().await;
    rooms.iter().all(|r| held.iter().any(|ch| ch.id() == *r && ch.is_readable()))
}

/// Keep trying to post until it goes out: `(message id, when it did)`.
async fn post_when_able(ch: &Channel, text: &str, secs: u64) -> Option<(String, u64)> {
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < secs {
        if let Ok(id) = ch.send(text).await {
            return Some((id, now_ms()));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

async fn owner(lab: Lab) {
    let mut checks = Checks::new(&lab, "owner");
    let (bot, _seen) = bot(&lab, "owner").await;
    let c = bot.create_community(&format!("vector-lab gate {}", now_ms() / 1000), lab.community("gate")).await.expect("a Community");
    let gate = c.create_role(RoleSpec::new("Gate").permissions(Permissions::MANAGE_ROLES | Permissions::MANAGE_CHANNELS)).await.expect("the gate's Role");
    let verified = c.create_role(RoleSpec::new("Verified")).await.expect("a Role");
    let ordered = c.reorder_roles(&[&gate, &verified]).await;
    checks.check("the gate's Role sits above Verified", ordered.is_ok(), crate::outcome(&ordered));
    let (view, send, history) = (Permissions::VIEW_CHANNEL, Permissions::SEND_MESSAGES, Permissions::READ_MESSAGE_HISTORY);

    let with_history = c.create_channel_with("with-history", NewChannel::default().private()).await.expect("a private Channel");
    with_history.set_overwrite(Overwrite::role(&verified).allow(view | send | history)).await.expect("an overwrite");
    let no_history = c.create_channel_with("no-history", NewChannel::default().private()).await.expect("a private Channel");
    no_history.set_overwrite(Overwrite::role(&verified).allow(view | send).deny(history)).await.expect("an overwrite");
    let verified_only = c.create_channel_with("verified-only", NewChannel::default()).await.expect("a Channel");
    verified_only.set_overwrite(Overwrite::everyone().deny(send)).await.expect("an overwrite");
    verified_only.set_overwrite(Overwrite::role(&verified).allow(send)).await.expect("an overwrite");
    for room in [&with_history, &no_history] {
        room.set_overwrite(Overwrite::role(&gate).allow(view | send | history)).await.expect("the gate's overwrite");
        room.send(EARLIER).await.expect("a post");
    }
    let invite = c.create_invite().await.expect("an invite");
    lab.put(
        "state.json",
        &json!({ "invite": invite, "with_history": with_history.id(), "no_history": no_history.id(), "verified_only": verified_only.id(), "verified": verified.id() }).to_string(),
    );

    let gate_npub = lab.wait("joined-gate", 300).await.expect("the gate bot joined");
    let t = std::time::Instant::now();
    while !c.members().await.iter().any(|m| m.npub() == gate_npub) && t.elapsed().as_secs() < 120 {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let start = now_ms();
    let granted = c.member(&gate_npub).add_role(&gate).await;
    checks.check("make the gate bot a gate", granted.is_ok(), format!("{} in {} ms", crate::outcome(&granted), now_ms() - start));

    for who in members(&lab) {
        lab.wait(&format!("done-{who}"), 900).await;
    }
    lab.wait("done-gate", 60).await;
    let gone = c.dissolve().await;
    checks.check("dissolve", gone.is_ok(), crate::outcome(&gone));
}

async fn gate(lab: Lab) {
    let mut checks = Checks::new(&lab, "gate");
    let (bot, seen) = bot(&lab, "gate").await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 300).await.expect("the owner's Community")).unwrap();
    let field = |k: &str| state[k].as_str().unwrap().to_string();
    let joined = bot.join(&field("invite")).await;
    checks.check("join by link", joined.is_ok(), crate::outcome(&joined));
    let Ok(c) = joined else { return };
    let joined_at = now_ms();
    lab.put("joined-gate", bot.npub());
    let (with_history, no_history) = (field("with_history"), field("no_history"));
    let me = c.member(bot.npub());
    let ready = until(180, async || holds(&c, &[&with_history, &no_history]).await && me.can(Permissions::MANAGE_ROLES) && me.can(Permissions::MANAGE_CHANNELS)).await;
    checks.check("holds both rooms and may grant", ready.is_some(), format!("{:?} ms after joining", ready.map(|r| r - joined_at)));

    // Grant each newcomer as they ask, the way a solved captcha is answered.
    let verified = c.role(field("verified"));
    for who in members(&lab) {
        let npub = lab.wait(&format!("joined-{who}"), 300).await.expect("a newcomer joined");
        let t = std::time::Instant::now();
        while !c.members().await.iter().any(|m| m.npub() == npub) && t.elapsed().as_secs() < 120 {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let start = now_ms();
        let granted = c.member(&npub).add_role(&verified).await;
        let end = now_ms();
        checks.check(&format!("grant {who}"), granted.is_ok(), format!("{} in {} ms", crate::outcome(&granted), end - start));
        lab.put(&format!("granted-{who}"), &json!({ "start": start, "end": end }).to_string());
    }

    for who in members(&lab) {
        let Some(report) = lab.wait(&format!("posted-{who}"), 400).await else {
            checks.check(&format!("{who}'s posts"), false, "never posted");
            continue;
        };
        let v: Value = serde_json::from_str(&report).unwrap();
        for room in ["with_history", "no_history", "verified_only"] {
            let (id, at) = (v[room]["id"].as_str().unwrap_or_default().to_string(), v[room]["at"].as_u64().unwrap_or_default());
            let heard = seen.when(60, |e| matches!(e, BotEvent::Message(m) if m.message.id == id)).await;
            checks.check(&format!("hears {who} in {room}"), heard.is_some(), format!("{:?} ms after it was sent", heard.map(|h| h as i64 - at as i64)));
        }
    }
    for who in members(&lab) {
        lab.wait(&format!("done-{who}"), 900).await;
    }
    lab.put("done-gate", "1");
    tokio::time::sleep(Duration::from_secs(4)).await;
    bot.logout().await;
}

async fn member(who: &str, lab: Lab) {
    let mut checks = Checks::new(&lab, who);
    let (bot, seen) = bot(&lab, who).await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 300).await.expect("the owner's Community")).unwrap();
    let field = |k: &str| state[k].as_str().unwrap().to_string();
    let (with_history, no_history, verified_only) = (field("with_history"), field("no_history"), field("verified_only"));
    let joined = bot.join(&field("invite")).await;
    checks.check("join by link", joined.is_ok(), crate::outcome(&joined));
    let Ok(c) = joined else { return };
    // The first asks in at once, the rest once their join has settled.
    if who != "m0" {
        tokio::time::sleep(Duration::from_secs(SETTLE_SECS)).await;
    }
    let refused = bot.channel(&verified_only).send("let me in").await;
    checks.check("can't post in the verified-only Channel yet", refused.is_err(), crate::outcome(&refused));
    lab.put(&format!("joined-{who}"), bot.npub());

    // Watched from before the grant, each on its own: one waiting on another, or on the grant
    // call returning, would hide which came first.
    let granted = async { serde_json::from_str::<Value>(&lab.wait(&format!("granted-{who}"), 600).await.expect("granted")).unwrap() };
    let me = bot.npub().to_string();
    let told = seen.when(300, |e| matches!(e, BotEvent::CommunityChange { change, .. } if matches!(&change.kind, ChangeKind::MemberRolesChanged { member, .. } if *member == me)));
    let keyed = |room: &String| {
        let (c, room) = (&c, room.clone());
        async move { until(300, async || holds(c, &[&room]).await).await }
    };
    let (ch_with, ch_without, ch_posting) = (bot.channel(&with_history), bot.channel(&no_history), bot.channel(&verified_only));
    let read = until(75, async || ch_with.history(20).await.iter().any(|m| m.content == EARLIER));
    let text = format!("{who} is in");
    let (granted, told, keyed_with, keyed_without, read, post_with, post_without, post_gated) = tokio::join!(
        granted,
        told,
        keyed(&with_history),
        keyed(&no_history),
        read,
        post_when_able(&ch_with, &text, 300),
        post_when_able(&ch_without, &text, 300),
        post_when_able(&ch_posting, &text, 300),
    );
    let (start, end) = (granted["start"].as_u64().unwrap(), granted["end"].as_u64().unwrap());
    let since = |t: Option<u64>| t.map_or("never".to_string(), |t| format!("{} ms after the grant began, {} ms after it returned", t as i64 - start as i64, t as i64 - end as i64));
    checks.check("told of the Role", told.is_some(), since(told));
    checks.check("holds the room with history", keyed_with.is_some(), since(keyed_with));
    checks.check("reads what was said there before", read.is_some(), since(read));
    checks.check("holds the room without history", keyed_without.is_some(), since(keyed_without));
    checks.check("posts in the room with history", post_with.is_some(), since(post_with.as_ref().map(|p| p.1)));
    checks.check("posts in the room without history", post_without.is_some(), since(post_without.as_ref().map(|p| p.1)));
    checks.check("posts in the verified-only Channel", post_gated.is_some(), since(post_gated.as_ref().map(|p| p.1)));
    let hidden = !ch_without.history(50).await.iter().any(|m| m.content == EARLIER);
    checks.check("the room without history shows none of it", hidden, "what was said before stays unread");
    let sent = |p: &Option<(String, u64)>| json!({ "id": p.as_ref().map(|p| p.0.clone()), "at": p.as_ref().map(|p| p.1) });
    lab.put(&format!("posted-{who}"), &json!({ "with_history": sent(&post_with), "no_history": sent(&post_without), "verified_only": sent(&post_gated) }).to_string());

    // The last one leaves and comes back, still Verified: how long until it is let back in.
    if members(&lab).last().map(String::as_str) == Some(who) {
        tokio::time::sleep(Duration::from_secs(8)).await;
        let left = c.leave().await;
        checks.check("leave", left.is_ok(), crate::outcome(&left));
        tokio::time::sleep(Duration::from_secs(5)).await;
        let back = bot.join(&field("invite")).await;
        checks.check("come back by the same link", back.is_ok(), crate::outcome(&back));
        if let Ok(c) = back {
            let returned = now_ms();
            let after = |t: Option<u64>| t.map_or("never".to_string(), |t| format!("{} ms after coming back", t - returned));
            let text = format!("{who} is back");
            let (rooms, gated) = tokio::join!(until(240, async || holds(&c, &[&with_history, &no_history]).await), post_when_able(&ch_posting, &text, 240));
            checks.check("holds both rooms again", rooms.is_some(), after(rooms));
            checks.check("posts in the verified-only Channel again", gated.is_some(), after(gated.map(|p| p.1)));
        }
    }
    lab.put(&format!("done-{who}"), "1");
    tokio::time::sleep(Duration::from_secs(4)).await;
    bot.logout().await;
}
