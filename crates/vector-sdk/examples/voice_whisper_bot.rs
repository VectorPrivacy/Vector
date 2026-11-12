//! **voice_whisper_bot**: a voice assistant for a voice Channel. It writes down what each person
//! says in the Channel's chat, then answers out loud: ask it the time, ask it to roll a die, or
//! say anything and hear it back.
//!
//! ```sh
//! WHISPER_MODEL=ggml-base.en.bin VECTOR_NSEC=nsec1... \
//!   cargo run -p vector_sdk --features whisper --example voice_whisper_bot -- <voice channel id>
//! ```
//!
//! Models are whisper.cpp's ggml files (<https://huggingface.co/ggerganov/whisper.cpp>);
//! `ggml-base.en.bin` is a good start. It speaks with the system's voice: `say` on macOS,
//! `espeak-ng` on Linux. Without either it only writes. Building needs `cmake`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vector_sdk::voice::{Transcriber, Utterances};
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let model = std::env::var("WHISPER_MODEL").map_err(|_| vector_sdk::Error::Other("set WHISPER_MODEL to a ggml model file".into()))?;
    let whisper = Transcriber::load(&model)?.language("en");

    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    let bot = builder.build().await?;
    println!("voice assistant online as {}", bot.npub());

    // Listening is how the call finds who's in it, and how invites arrive.
    let listener = bot.clone();
    tokio::spawn(async move { listener.run(|_, _| async {}).await });
    let mut channel = bot.channel(voice_channel(&bot).await);
    loop {
        let call = join(&bot, channel.id()).await;
        println!("listening in {}", channel.id());
        serve(&bot, &channel, &call, &whisper).await;
        // The call ended: a moderator moved the bot (follow), or disconnected it (stop).
        match call.moved_to() {
            Some(to) => channel = bot.channel(to),
            None => break,
        }
    }
    println!("disconnected");
    Ok(())
}

/// Write down what each person says in the call, and answer out loud, until the call ends.
async fn serve(bot: &VectorBot, channel: &vector_sdk::Channel, call: &vector_sdk::VoiceCall, whisper: &Transcriber) {
    let mut said = Utterances::new();
    while let Some(speech) = call.listen().await {
        let Some(utterance) = said.push(speech) else { continue };
        if utterance.duration() < Duration::from_millis(400) {
            continue; // a cough, a click
        }
        let text = match whisper.transcribe(&utterance.pcm).await {
            Ok(text) => text,
            Err(e) => {
                eprintln!("transcription failed: {e}");
                continue;
            }
        };
        if text.is_empty() || text.starts_with('[') {
            continue; // Whisper marks silence and noise as [BLANK_AUDIO] and the like
        }
        let who = match utterance.npub.as_deref() {
            Some(npub) => bot.cached_profile(npub).await.map(|p| p.name.to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| npub[..12].to_string()),
            None => "someone".to_string(),
        };
        println!("{who}: {text}");
        let _ = channel.send(&format!("🎙 **{who}**: {text}")).await;
        if let Some(voice) = speak(&answer(&text)).await {
            call.say_and_wait(&voice).await;
        }
    }
}

/// The voice Channel named on the command line, or the first one the bot can find, waiting for
/// an invite to one if there's none yet.
async fn voice_channel(bot: &VectorBot) -> String {
    if let Some(id) = std::env::args().nth(1) {
        return id;
    }
    loop {
        for community in bot.communities().await {
            if let Some(ch) = community.channel_infos().unwrap_or_default().into_iter().find(|c| c.view == "voice") {
                return ch.channel_id;
            }
        }
        println!("waiting to be invited to a Community with a voice Channel");
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// Join the call, retrying until the bot is in the Community the Channel belongs to.
async fn join(bot: &VectorBot, channel_id: &str) -> vector_sdk::VoiceCall {
    loop {
        match bot.channel(channel_id).join_voice().await {
            Ok(call) => return call,
            Err(e) => {
                println!("can't join {channel_id} yet: {e}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

fn answer(text: &str) -> String {
    let asked = text.to_lowercase();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    if asked.contains("time") {
        let mins = now.as_secs() / 60;
        format!("It's {:02}:{:02} UTC.", mins / 60 % 24, mins % 60)
    } else if asked.contains("roll") || asked.contains("dice") || asked.contains(" die") {
        format!("You rolled a {}.", now.subsec_nanos() % 6 + 1)
    } else {
        format!("You said: {text}")
    }
}

/// `text` in the system's voice, as 48 kHz mono PCM.
async fn speak(text: &str) -> Option<Vec<i16>> {
    // A private directory, gone with whatever the voice wrote, however it went.
    let dir = tempfile::tempdir().ok()?;
    let (aiff, wav) = (dir.path().join("say.aiff"), dir.path().join("say.wav"));
    let made = match tokio::process::Command::new("say").arg("-o").arg(&aiff).arg(text).status().await {
        Ok(s) if s.success() => aiff,
        _ => match tokio::process::Command::new("espeak-ng").arg("-w").arg(&wav).arg(text).status().await {
            Ok(s) if s.success() => wav,
            _ => return None,
        },
    };
    vector_sdk::voice::decode_with_ffmpeg(&made).await.ok()
}
