//! **voice_echo_bot**: joins a voice Channel and says back whatever you say, a beat after you
//! stop. Speak, pause, and hear yourself.
//!
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector_sdk --features voice --example voice_echo_bot -- <voice channel id>
//! ```
//!
//! Without a channel id it joins the first voice Channel it finds in a Community it's in.

use vector_sdk::voice::Utterances;
use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    let bot = builder.build().await?;
    println!("echo bot online as {}", bot.npub());

    // Listening is how the call finds who's in it, and how invites arrive.
    let listener = bot.clone();
    tokio::spawn(async move { listener.run(|_, _| async {}).await });
    let mut channel_id = voice_channel(&bot).await;
    loop {
        let call = join(&bot, &channel_id).await;
        println!("in the call in {channel_id}: speak, then pause");
        let mut said = Utterances::new();
        while let Some(speech) = call.listen().await {
            if let Some(utterance) = said.push(speech) {
                println!("echoing {:.1}s", utterance.duration().as_secs_f32());
                call.say(&utterance.pcm);
            }
        }
        // The call ended: a moderator moved the bot (follow), or disconnected it (stop).
        match call.moved_to() {
            Some(to) => channel_id = to,
            None => break,
        }
    }
    println!("disconnected");
    Ok(())
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
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

/// Join the call, retrying until the bot is in the Community the Channel belongs to.
async fn join(bot: &VectorBot, channel_id: &str) -> vector_sdk::VoiceCall {
    loop {
        match bot.channel(channel_id).join_voice().await {
            Ok(call) => return call,
            Err(e) => {
                println!("can't join {channel_id} yet: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}
