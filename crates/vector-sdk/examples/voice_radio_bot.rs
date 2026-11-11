//! **voice_radio_bot**: plays an audio file on a loop in a voice Channel, in as many Channels
//! as you name: one process, many calls.
//!
//! ```sh
//! VECTOR_NSEC=nsec1... cargo run -p vector_sdk --features voice --example voice_radio_bot -- song.mp3 <voice channel id> [<another>…]
//! ```
//!
//! Any format `ffmpeg` reads works; it must be on the PATH.

use vector_sdk::VectorBot;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let mut args = std::env::args().skip(1);
    let file = args.next().expect("usage: voice_radio_bot <audio file> <voice channel id>…");
    let channels: Vec<String> = args.collect();
    assert!(!channels.is_empty(), "name at least one voice channel id");

    let mut builder = VectorBot::builder().public();
    if let Ok(nsec) = std::env::var("VECTOR_NSEC") {
        builder = builder.nsec(nsec);
    }
    let bot = builder.build().await?;
    let track = vector_sdk::voice::decode_with_ffmpeg(&file).await?;
    println!("radio online as {}: {:.0}s of {file}", bot.npub(), track.len() as f32 / vector_sdk::voice::SAMPLE_RATE as f32);

    // Listening is how each call finds who's in it.
    let listener = bot.clone();
    tokio::spawn(async move { listener.run(|_, _| async {}).await });

    let mut stations = Vec::new();
    for id in &channels {
        let mut call = bot.channel(id).join_voice().await?;
        println!("on air in {id}");
        let (bot, track) = (bot.clone(), track.clone());
        stations.push(tokio::spawn(async move {
            loop {
                while call.say_and_wait(&track).await {}
                // A moderator moved the bot, or disconnected it: follow a move, stop otherwise.
                let Some(to) = call.moved_to() else {
                    println!("off air in {}", call.channel_id());
                    return;
                };
                match bot.channel(&to).join_voice().await {
                    Ok(next) => {
                        println!("moved to {to}");
                        call = next;
                    }
                    Err(e) => return println!("couldn't follow the move to {to}: {e}"),
                }
            }
        }));
    }
    tokio::signal::ctrl_c().await.ok();
    for s in stations {
        s.abort();
    }
    bot.logout().await;
    Ok(())
}
