//! Two voice Channels, one bot speaking a different tone into each and another hearing each call
//! apart, knowing who spoke. A second join to a Channel replaces the first call, which ends.

use crate::{now_ms, Checks, Lab};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use vector_sdk::voice::{level, tone, SAMPLE_RATE};
use vector_sdk::*;

pub async fn run(role: &str, lab: Lab) {
    match role {
        "owner" => owner(lab).await,
        "a" => speaker(lab).await,
        _ => listener(lab).await,
    }
}

/// The dominant frequency, by zero crossings: enough to tell 440 Hz from 660.
fn hz(pcm: &[i16]) -> f32 {
    let crossings = pcm.windows(2).filter(|w| (w[0] >= 0) != (w[1] >= 0)).count();
    crossings as f32 / 2.0 / (pcm.len() as f32 / SAMPLE_RATE as f32)
}

async fn owner(lab: Lab) {
    let mut checks = Checks::new(&lab, "owner");
    let (bot, _seen) = crate::bot(&lab, "owner").await;
    let c = bot.create_community(&format!("vector-lab voice {}", now_ms() / 1000), lab.community("voice")).await.expect("a Community");
    let v1 = c.create_channel_with("stage", NewChannel::default().view("voice")).await.expect("a voice Channel");
    let v2 = c.create_channel_with("lounge", NewChannel::default().view("voice")).await.expect("another");
    lab.put("state.json", &json!({ "invite": c.create_invite().await.expect("an invite"), "v1": v1.id(), "v2": v2.id() }).to_string());
    lab.wait("done-a", 500).await;
    lab.wait("done-b", 500).await;
    let gone = c.dissolve().await;
    checks.check("dissolve", gone.is_ok(), crate::outcome(&gone));
}

async fn joined(lab: &Lab, who: &str, checks: &mut Checks) -> (VectorBot, Channel, Channel) {
    let (bot, _seen) = crate::bot(lab, who).await;
    let state: Value = serde_json::from_str(&lab.wait("state.json", 300).await.expect("the owner's Community")).unwrap();
    checks.check("join the Community", bot.join(state["invite"].as_str().unwrap()).await.is_ok(), "by link");
    lab.put(&format!("joined-{who}"), bot.npub());
    let (v1, v2) = (bot.channel(state["v1"].as_str().unwrap()), bot.channel(state["v2"].as_str().unwrap()));
    (bot, v1, v2)
}

/// What one call heard for 14 s: frames and summed pitch, per speaker.
async fn hear(call: &VoiceCall) -> HashMap<Option<String>, (usize, f32)> {
    let mut heard: HashMap<Option<String>, (usize, f32)> = HashMap::new();
    let until = Instant::now() + Duration::from_secs(14);
    while Instant::now() < until {
        let Ok(Some(s)) = tokio::time::timeout(Duration::from_secs(1), call.listen()).await else { continue };
        if level(&s.pcm) > 0.02 {
            let e = heard.entry(s.npub).or_default();
            e.0 += 1;
            e.1 += hz(&s.pcm);
        }
    }
    heard
}

async fn speaker(lab: Lab) {
    let mut checks = Checks::new(&lab, "a");
    let (bot, v1, v2) = joined(&lab, "a", &mut checks).await;
    let c1 = v1.join_voice().await.expect("into the stage");
    let c2 = v2.join_voice().await.expect("into the lounge as well");
    checks.note("in two calls at once", "stage and lounge");
    lab.wait("b-listening", 300).await.expect("b listening");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut said = true;
    for _ in 0..3 {
        c1.say(&tone(440.0, 1500));
        c2.say(&tone(660.0, 1500));
        let (said1, said2) = tokio::join!(c1.finished(), c2.finished());
        said &= said1 && said2;
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    checks.check("said into both, in real time", said, "three tones each");
    lab.put("a-said", "1");
    lab.wait("b-heard", 120).await;
    c2.leave().await;
    lab.put("a-left-lounge", "1");
    lab.wait("done-b", 300).await;
    c1.leave().await;
    lab.put("done-a", "1");
    bot.logout().await;
}

async fn listener(lab: Lab) {
    let mut checks = Checks::new(&lab, "b");
    let (bot, v1, v2) = joined(&lab, "b", &mut checks).await;
    let a = lab.wait("joined-a", 300).await.expect("a in the Community");
    let c1 = v1.join_voice().await.expect("into the stage");
    let c2 = v2.join_voice().await.expect("into the lounge");
    let t = Instant::now();
    while (c1.connected() < 1 || c2.connected() < 1) && t.elapsed() < Duration::from_secs(90) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    checks.check("linked to a in both calls", c1.connected() >= 1 && c2.connected() >= 1, format!("after {:.1}s", t.elapsed().as_secs_f32()));
    lab.put("b-listening", "1");

    // Each call apart: who spoke, how much, at what pitch.
    let (h1, h2) = tokio::join!(hear(&c1), hear(&c2));
    lab.put("b-heard", "1");
    for (name, heard, want) in [("stage", &h1, 440.0), ("lounge", &h2, 660.0)] {
        let from_a = heard.get(&Some(a.clone())).copied().unwrap_or_default();
        let pitch = if from_a.0 > 0 { from_a.1 / from_a.0 as f32 } else { 0.0 };
        checks.check(&format!("heard a in the {name}"), from_a.0 >= 150 && (pitch - want).abs() < 40.0, format!("{} frames at {pitch:.0} Hz, want ~225 at {want}", from_a.0));
        checks.check(&format!("only a in the {name}"), heard.len() == 1, format!("{} speaker(s)", heard.len()));
    }

    // a leaves the lounge: what it was saying there ends.
    lab.wait("a-left-lounge", 120).await;
    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(s) = c2.listen().await {
            if s.pcm.is_empty() && s.npub.as_deref() == Some(a.as_str()) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    checks.check("a's leaving ends what it said", ended, "an empty frame from a");

    // Joining the stage again replaces the call there: the old one ends.
    let again = v1.join_voice().await;
    checks.check("join the stage again", again.is_ok(), crate::outcome(&again));
    let over = tokio::time::timeout(Duration::from_secs(10), async {
        while c1.listen().await.is_some() {}
    })
    .await
    .is_ok();
    checks.check("the replaced call ends", over && c1.ended() && !c1.say(&tone(300.0, 20)), "listen None, say false");
    if let Ok(again) = again {
        again.leave().await;
    }
    c2.leave().await;
    lab.put("done-b", "1");
    bot.logout().await;
}
