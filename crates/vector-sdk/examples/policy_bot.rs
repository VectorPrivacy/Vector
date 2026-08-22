//! A moderation bot, end to end.
//!
//! Writes a rulebook, previews it against real history before trusting it,
//! then watches and reports. It removes nobody: the autopilot rehearses until
//! someone calls `.arm()`, which is the one line that turns a bot that flags
//! into a bot that acts.
//!
//! ```sh
//! VECTOR_NSEC=nsec1… COMMUNITY=fe4a… cargo run -p vector_sdk --example policy_bot
//! ```
//!
//! Set `ARM=1` to let it kick — read the dry-run output first.

use std::time::Duration;

use vector_sdk::policy::{Action, Policy, PolicyRule, Preset, Seriousness};
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let bot = VectorBot::builder().nsec(std::env::var("VECTOR_NSEC").expect("VECTOR_NSEC")).build().await?;
    let community = bot.community(std::env::var("COMMUNITY").expect("COMMUNITY"));

    // ── 1. Write a rulebook ─────────────────────────────────────────────────
    // Start from a template, then add what this community cares about. No
    // weights, no thresholds: seriousness is the only judgement, and the
    // library picks numbers that match it.
    let policy = Policy::preset(Preset::ScamLinks)?
        .rule(PolicyRule::words("house-words", ["freeairdrop", "*claimnow*"]).seriousness(Seriousness::Major))
        .rule(PolicyRule::repetition("copypaste"))
        .rule(PolicyRule::cohort("raid"))
        .rule(PolicyRule::join_burst("wave", 600, 5).only_after("raid", Some(3)))
        .window(168, 4000);

    // ── 2. Preview before trusting it ───────────────────────────────────────
    // Against real history, storing nothing. The number that matters is not
    // how many it flagged but how many REGULARS it would have flagged had
    // their standing not saved them: a short list can hide a rule that catches
    // ordinary conversation.
    let preview = community.policies().preview(&policy).await?;
    println!("would flag {} member(s), citing {} message(s)", preview.flagged.len(), preview.messages_cited);
    for row in preview.flagged.iter().take(10) {
        println!("  {} — {} ({})", &row.npub[..12], row.reasons.first().map(String::as_str).unwrap_or(""), row.score);
    }
    if !preview.shielded_matches.is_empty() {
        println!(
            "  ⚠ {} trusted member(s) also matched and were spared only by their standing — \
             if this is meant for raiders, it is catching ordinary conversation",
            preview.shielded_matches.len()
        );
        return Ok(()); // refuse to ship a rule that catches regulars
    }

    // ── 3. Put it to work ───────────────────────────────────────────────────
    community.policies().set("house-rules", policy).await?;

    // ── 4. Watch, and act only on proof ─────────────────────────────────────
    let pilot = {
        let p = community.autopilot(Action::Kick);
        if std::env::var("ARM").is_ok() { p.arm() } else { p }
    };

    let mut watch = community.watch_policies().await?;
    loop {
        let Some(verdicts) = watch.next().await else { break };

        // Provable and grave: the bot's own business.
        let run = pilot.run_once().await?;
        for npub in &run.acted {
            println!("{} {}", if run.dry_run { "would kick" } else { "kicked" }, &npub[..12]);
        }
        for (npub, why) in &run.skipped {
            println!("skipped {}: {}", &npub[..12], why);
        }

        // Convicted on inference — a raid looks exactly like this. The engine is
        // convinced and nobody could replay it, so it goes to a second judge.
        for v in verdicts.unproven() {
            println!("for review: {} — {} (confidence {}, proven {})", v.name(), v.why(), v.confidence(), v.proven);
        }
        if verdicts.raid_detected() {
            println!("RAID SUSPECTED — hiding is safe, removing is a human's call");
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(())
}
