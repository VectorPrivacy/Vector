//! Joins, Roles, and a conversation in a Channel and in DMs, each event checked once at the bot
//! it was meant for, with who did it.

use crate::{bot, now_ms, Checks, Lab};
use serde_json::{json, Value};
use vector_sdk::*;

pub async fn run(role: &str, lab: Lab) {
    match role {
        "owner" => owner(lab).await,
        who => member(who, lab).await,
    }
}

async fn owner(lab: Lab) {
    let mut checks = Checks::new(&lab, "owner");
    let started = now_ms();
    let (bot, seen) = bot(&lab, "owner").await;
    let c = bot.create_community(&format!("vector-lab events {}", started / 1000), lab.community("events")).await.expect("a Community");
    let lobby = c.primary_channel().expect("its first Channel");
    let invite = c.create_invite().await.expect("an invite");
    lab.put("state.json", &json!({ "invite": invite, "lobby": lobby.id(), "owner": bot.npub() }).to_string());

    let a = lab.wait("joined-a", 300).await.expect("a joined");
    let b = lab.wait("joined-b", 300).await.expect("b joined");
    for (who, npub) in [("a", &a), ("b", &b)] {
        let joined = seen.wait_for(120, |e| matches!(e, BotEvent::MemberJoin { npub: n, .. } if n == npub).then_some(())).await;
        checks.check(&format!("{who}'s join"), joined.is_some(), "MemberJoin");
    }
    // A Role is granted only to a member whose join this side has folded.
    let t = std::time::Instant::now();
    while !c.members().await.iter().any(|m| m.npub() == a) && t.elapsed().as_secs() < 120 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let mods = c.create_role(RoleSpec::new("Lab Mods").permissions(Permissions::KICK)).await;
    checks.check("create a Role", mods.is_ok(), crate::outcome(&mods));
    if let Ok(mods) = &mods {
        let granted = c.member(&a).add_role(mods).await;
        checks.check("grant it to a", granted.is_ok(), crate::outcome(&granted));
        lab.put("role", mods.id());
    }

    lab.wait("done-a", 400).await;
    lab.wait("done-b", 400).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    for (who, npub) in [("a", &a), ("b", &b)] {
        checks.once(&format!("{who}'s join, once"), seen.count(|e| matches!(e, BotEvent::MemberJoin { npub: n, .. } if n == npub)));
    }
    checks.check("its own Role changes aren't reported back", seen.count(|e| matches!(e, BotEvent::CommunityChange { .. })) == 0, "none");
    let gone = c.dissolve().await;
    checks.check("dissolve", gone.is_ok(), crate::outcome(&gone));
}

async fn member(who: &str, lab: Lab) {
    let mut checks = Checks::new(&lab, who);
    let started = now_ms();
    let (bot, seen) = bot(&lab, who).await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 300).await.expect("the owner's Community")).unwrap();
    let (lobby_id, owner) = (state["lobby"].as_str().unwrap().to_string(), state["owner"].as_str().unwrap().to_string());
    let joined = bot.join(state["invite"].as_str().unwrap()).await;
    checks.check("join by link", joined.is_ok(), crate::outcome(&joined));
    lab.put(&format!("joined-{who}"), bot.npub());
    let lobby = bot.channel(&lobby_id);
    let other = if who == "a" { "b" } else { "a" };
    let peer = lab.wait(&format!("joined-{other}"), 300).await.expect("the other member");

    // The Role the owner made, and the one it granted a: who did it, and when.
    let role_id = lab.wait("role", 300).await.unwrap_or_default();
    let created = seen
        .wait_for(120, |e| match e {
            BotEvent::CommunityChange { change, .. } => match &change.kind {
                ChangeKind::RoleCreated { role } if role.name == "Lab Mods" => Some((change.by.clone(), change.at)),
                _ => None,
            },
            _ => None,
        })
        .await;
    checks.check(
        "a Role created, by the owner, dated",
        created.as_ref().is_some_and(|(by, at)| by.as_deref() == Some(owner.as_str()) && *at + 600_000 >= started && *at <= now_ms() + 60_000),
        format!("{created:?}"),
    );
    if who == "a" {
        let granted = seen
            .wait_for(120, |e| match e {
                BotEvent::CommunityChange { change, .. } => match &change.kind {
                    ChangeKind::MemberRolesChanged { member, added, .. } if *member == bot.npub() && added.contains(&role_id) => Some(change.by.clone()),
                    _ => None,
                },
                _ => None,
            })
            .await;
        checks.check("granted the Role, by the owner", granted.flatten().as_deref() == Some(owner.as_str()), "MemberRolesChanged");
    }

    if who == "a" {
        // a speaks, b reacts, a edits, then deletes; then the same in a DM.
        let hello = lobby.send("hello from a").await.expect("post");
        lab.put("hello", &hello);
        let reacted = seen.wait_for(120, |e| matches!(e, BotEvent::Reaction { message, reaction, .. } if message.id == hello && reaction.emoji == "👍" && reaction.author_id == peer).then_some(())).await;
        checks.check("b's reaction reached a", reacted.is_some(), "Reaction");
        let _ = lobby.edit(&hello, "hello from a, edited").await;
        lab.wait("b-saw-edit", 120).await;
        let deleted = lobby.delete(&hello).await;
        checks.check("delete", deleted.is_ok(), crate::outcome(&deleted));

        let dm = bot.channel(&peer).send("a DM from a").await.expect("a DM");
        lab.put("dm", &dm);
        let fire = seen.wait_for(120, |e| matches!(e, BotEvent::Reaction { message, reaction, .. } if message.id == dm && reaction.emoji == "🔥").then_some(())).await;
        checks.check("b's DM reaction reached a", fire.is_some(), "Reaction");
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        checks.once("the Channel reaction, once", seen.count(|e| matches!(e, BotEvent::Reaction { reaction, .. } if reaction.emoji == "👍")));
        checks.once("the DM reaction, once", seen.count(|e| matches!(e, BotEvent::Reaction { reaction, .. } if reaction.emoji == "🔥")));
    } else {
        let hello = lab.wait("hello", 300).await.expect("a's post");
        let heard = seen.wait_for(120, |e| matches!(e, BotEvent::Message(m) if m.message.id == hello && m.author().as_deref() == Some(peer.as_str())).then_some(())).await;
        checks.check("a's post reached b", heard.is_some(), "Message");
        let _ = lobby.react(&hello, "👍").await;
        let edit = seen.wait_for(120, |e| matches!(e, BotEvent::Edit { message, .. } if message.id == hello && message.content == "hello from a, edited").then_some(())).await;
        checks.check("a's edit reached b", edit.is_some(), "Edit");
        lab.put("b-saw-edit", "1");
        let gone = seen.wait_for(120, |e| match e {
            BotEvent::Delete { message_id, by, .. } if *message_id == hello => Some(by.clone()),
            _ => None,
        })
        .await;
        checks.check("a's delete reached b, by a", gone.flatten().as_deref() == Some(peer.as_str()), "Delete");

        let dm = lab.wait("dm", 300).await.expect("a's DM");
        let got = seen.wait_for(120, |e| matches!(e, BotEvent::Message(m) if m.message.id == dm && !m.is_group).then_some(())).await;
        checks.check("a's DM reached b", got.is_some(), "Message");
        let _ = bot.channel(&peer).react(&dm, "🔥").await;
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        checks.once("a's post, once", seen.count(|e| matches!(e, BotEvent::Message(m) if m.message.id == hello)));
        checks.once("a's edit, once", seen.count(|e| matches!(e, BotEvent::Edit { message, .. } if message.id == hello)));
        checks.once("a's DM, once", seen.count(|e| matches!(e, BotEvent::Message(m) if m.message.id == dm)));
    }
    checks.once("the Role's creation, once", seen.count(|e| matches!(e, BotEvent::CommunityChange { change, .. } if matches!(&change.kind, ChangeKind::RoleCreated { role } if role.name == "Lab Mods"))));
    lab.put(&format!("done-{who}"), "1");
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    bot.logout().await;
}
