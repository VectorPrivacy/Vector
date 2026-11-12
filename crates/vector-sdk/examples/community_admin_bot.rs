//! **community_admin_bot** — runs a Community the way a Discord admin bot would.
//!
//! On `/setup` it builds a Mods role, a Support category and a private `#tickets` channel only
//! Mods can read. Mods get `/timeout`, `/role` and `/pin`; anyone can read `/audit`. Every
//! change to the Community (by anyone, through any client) is logged with who made it.
//!
//! The bot needs Manage Roles, Manage Channels, Moderate Members and Pin Messages: make it an
//! admin, or the owner, of the Community you invite it to.
//!
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector_sdk --example community_admin_bot
//! ```

use std::time::Duration;
use vector_sdk::{BotEvent, ChangeKind, NewChannel, Overwrite, Permissions, RoleSpec, VectorBot};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    let bot = builder.build().await?;
    println!("admin bot online as {}", bot.npub());

    bot.command("setup", "Create the Mods role and a private #tickets channel").run(|ctx| async move {
        let Some(community) = ctx.msg.community() else { return };
        let built = async {
            let mods = match community.role_named("Mods")? {
                Some(role) => role,
                None => community.create_role(RoleSpec::new("Mods").color(0x59fcb3).permissions(Permissions::KICK | Permissions::MODERATE_MEMBERS | Permissions::PIN_MESSAGES).shown()).await?,
            };
            if community.channel_infos()?.iter().any(|c| c.name == "tickets") {
                return vector_sdk::Result::Ok(());
            }
            let support = match community.categories()?.into_iter().find(|k| k.name == "Support") {
                Some(k) => community.category(k.category_id),
                None => community.create_category("Support").await?,
            };
            let tickets = community.create_channel_with("tickets", NewChannel::default().private().in_category(support.id()).topic("Ask the Mods")).await?;
            tickets.set_overwrite(Overwrite::role(&mods).allow(Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES)).await?;
            vector_sdk::Result::Ok(())
        };
        let _ = ctx.reply(match built.await {
            Ok(()) => "Set up: Mods role, Support category, private #tickets.".to_string(),
            Err(e) => format!("Setup failed: {e}"),
        }).await;
    });

    bot.command("timeout", "Silence a member for a while")
        .user("member", "Who", true)
        .int("minutes", "How long", true)
        .run(|ctx| async move {
            let (Some(community), Some(author)) = (ctx.msg.community(), ctx.msg.author()) else { return };
            if !community.member(&author).can(Permissions::MODERATE_MEMBERS) {
                let _ = ctx.reply("Only Mods can do that.").await;
                return;
            }
            let who = ctx.str("member").unwrap_or_default().to_string();
            let minutes = ctx.int("minutes").unwrap_or(10).clamp(1, 28 * 24 * 60) as u64;
            let done = community.member(&who).timeout(Duration::from_secs(minutes * 60)).await;
            let _ = ctx.reply(match done {
                Ok(()) => format!("Timed out for {minutes} minutes."),
                Err(e) => format!("Couldn't: {e}"),
            }).await;
        });

    bot.command("role", "Give or take a role")
        .user("member", "Who", true)
        .string("role", "Which role", true)
        .choice("action", "Give or take", ["give", "take"], true)
        .run(|ctx| async move {
            let (Some(community), Some(author)) = (ctx.msg.community(), ctx.msg.author()) else { return };
            if !community.member(&author).can(Permissions::MANAGE_ROLES) {
                let _ = ctx.reply("You can't manage roles here.").await;
                return;
            }
            let Ok(Some(role)) = community.role_named(ctx.str("role").unwrap_or_default()) else {
                let _ = ctx.reply("No role by that name.").await;
                return;
            };
            let member = community.member(ctx.str("member").unwrap_or_default());
            let done = if ctx.str("action") == Some("take") { member.remove_role(&role).await } else { member.add_role(&role).await };
            let _ = ctx.reply(match done {
                Ok(()) => "Done.".to_string(),
                Err(e) => format!("Couldn't: {e}"),
            }).await;
        });

    bot.command("pin", "Pin the message you reply to").run(|ctx| async move {
        let Some(parent) = Some(ctx.msg.message.replied_to.clone()).filter(|p| !p.is_empty()) else {
            let _ = ctx.reply("Reply to the message you want pinned.").await;
            return;
        };
        let done = ctx.msg.channel().pin(&parent).await;
        let _ = ctx.reply(if done.is_ok() { "Pinned." } else { "Couldn't pin it." }).await;
    });

    bot.command("audit", "The last ten things that changed here").run(|ctx| async move {
        let Some(community) = ctx.msg.community() else { return };
        let text = match community.audit_log().await {
            Ok(log) => log
                .entries
                .iter()
                .take(10)
                .map(|e| format!("{:?} by {}", e.action, e.actor.as_deref().map_or("?", |a| &a[..16.min(a.len())])))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("Couldn't read the log: {e}"),
        };
        let _ = ctx.reply(text).await;
    });

    bot.run(|_bot, event| async move {
        if let BotEvent::CommunityChange { change, .. } = event {
            let by = change.by.as_deref().unwrap_or("someone");
            match &change.kind {
                ChangeKind::MemberBanned { member } => println!("{by} banned {member}"),
                ChangeKind::TimedOut { member, until } => println!("{by} timed out {member} until {until}"),
                ChangeKind::RoleCreated { role } => println!("{by} made the role {}", role.name),
                other => println!("{by}: {other:?}"),
            }
        }
    })
    .await
}
