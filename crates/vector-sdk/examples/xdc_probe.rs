//! Test a Mini App bot without a phone: play the human's side of 3D Tic-Tac-Toe.
//!
//! Sends the `.xdc` to the bot by DM, opens it (joins the realtime session),
//! then plays X with the app's own protocol until the game ends, printing every
//! frame. Exits non-zero if the bot never answers.
//!
//! Run:  cargo run --example xdc_probe --features xdc -- <bot npub> <path/to/3d-tic-tac-toe.xdc> [community]
//!
//! With `community`, it founds a Community, invites the bot (which must accept
//! invites), and shares the app in its channel instead of a DM.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use vector_sdk::{VectorBot, XdcEvent};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(bot_npub), Some(app_path)) = (args.next(), args.next()) else {
        eprintln!("usage: xdc_probe <bot npub> <app.xdc>");
        std::process::exit(2);
    };
    let me = VectorBot::builder().data_dir("./xdc-probe-data").build().await?;
    println!("probe {} -> bot {}", me.npub(), bot_npub);

    // Receive loop in the background, so the bot's advertisements land.
    let listener = me.clone();
    tokio::spawn(async move {
        let _ = listener.on_message(|_, msg| async move {
            if !msg.is_mine() {
                println!("[chat] {}", msg.text());
            }
        }).await;
    });
    tokio::time::sleep(Duration::from_secs(3)).await;

    let t0 = Instant::now();
    let channel = if args.next().as_deref() == Some("community") {
        let created = me.core().create_community_v2("XDC Probe").await?;
        let community_id = created["community_id"].as_str().unwrap_or_default().to_string();
        let channel_id = created["primary_channel"].as_str().unwrap_or_default().to_string();
        me.community(&community_id).invite(&bot_npub).await?;
        println!("[{:>5}ms] founded {}, invited the bot; waiting for it to join", t0.elapsed().as_millis(), &community_id[..12]);
        tokio::time::sleep(Duration::from_secs(20)).await;
        me.channel(channel_id)
    } else {
        me.dm(&bot_npub)
    };
    let app = channel.send_xdc(&app_path).await?;
    println!("[{:>5}ms] sent {} topic {}", t0.elapsed().as_millis(), app.file_name(), app.topic().unwrap_or("-"));

    let mut session = app.join().await?;
    println!("[{:>5}ms] joined, advertised", t0.elapsed().as_millis());

    let my_addr = session.self_addr().to_string();
    let my_at = now_ms() - 60_000; // opened first: we are X and host
    let send = |m: Value| {
        let mut m = m;
        m["by"] = json!(my_addr);
        m
    };

    let mut board = [0u8; 27];
    let mut bot_seen = false;
    let mut bot_moves = 0;
    let mut last_hello = Instant::now() - Duration::from_secs(10);
    let deadline = Instant::now() + Duration::from_secs(180);

    while Instant::now() < deadline {
        if !bot_seen && last_hello.elapsed() > Duration::from_secs(3) {
            let _ = session.send_json(&send(json!({ "t": "hello", "name": "Probe", "at": my_at }))).await;
            last_hello = Instant::now();
        }
        let event = match tokio::time::timeout(Duration::from_secs(1), session.next()).await {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => continue,
        };
        let frame = match event {
            XdcEvent::Data(f) => f,
            XdcEvent::PeerJoined(p) => {
                println!("[{:>5}ms] peer joined: node {} npub {:?}", t0.elapsed().as_millis(), hex(&p.node), p.npub);
                continue;
            }
            other => {
                println!("[{:>5}ms] {other:?}", t0.elapsed().as_millis());
                continue;
            }
        };
        let Some(m) = frame.json::<Value>() else { continue };
        println!(
            "[{:>5}ms] <- {} (verified sender: {:?})",
            t0.elapsed().as_millis(),
            m,
            frame.verified_sender().map(|s| &s[..16])
        );
        match m["t"].as_str() {
            Some("hello") if !bot_seen => {
                bot_seen = true;
                let _ = session.send_json(&send(json!({ "t": "hello", "name": "Probe", "at": my_at }))).await;
                // Open with the first empty corner, as host: moves go out as `state`.
                board[0] = 1;
                let _ = session.send_json(&send(json!({ "t": "state", "board": board.to_vec(), "scores": {"x":0,"o":0,"draw":0}, "gameOver": false }))).await;
                println!("[{:>5}ms] -> X at 0", t0.elapsed().as_millis());
            }
            Some("move") => {
                let Some(b) = m["board"].as_array() else { continue };
                let nb: Vec<u8> = b.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect();
                if nb.len() != 27 || nb.iter().filter(|&&c| c != 0).count() <= board.iter().filter(|&&c| c != 0).count() {
                    continue;
                }
                board.copy_from_slice(&nb);
                bot_moves += 1;
                if let Some(w) = winner(&board) {
                    println!("[{:>5}ms] game over, winner {w}", t0.elapsed().as_millis());
                    let _ = session.send_json(&send(json!({ "t": "state", "board": board.to_vec(), "scores": {"x":0,"o":1,"draw":0}, "gameOver": true }))).await;
                    break;
                }
                // Play the first empty cell: a weak X, so the game ends quickly.
                if let Some(cell) = (0..27).find(|&i| board[i] == 0) {
                    board[cell] = 1;
                    let over = winner(&board).is_some();
                    let _ = session.send_json(&send(json!({ "t": "state", "board": board.to_vec(), "scores": {"x":0,"o":0,"draw":0}, "gameOver": over }))).await;
                    println!("[{:>5}ms] -> X at {cell}", t0.elapsed().as_millis());
                    if over {
                        println!("[{:>5}ms] game over, X won", t0.elapsed().as_millis());
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    println!("peers at end: {:?}", session.peers().iter().map(|p| p.npub.clone()).collect::<Vec<_>>());
    session.leave().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    if !bot_seen || bot_moves == 0 {
        eprintln!("FAIL: bot_seen={bot_seen} bot_moves={bot_moves}");
        std::process::exit(1);
    }
    println!("PASS: bot answered and made {bot_moves} move(s)");
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().take(6).map(|x| format!("{x:02x}")).collect()
}

fn winner(b: &[u8; 27]) -> Option<u8> {
    let idx = |x: usize, y: usize, z: usize| x * 9 + y * 3 + z;
    let mut lines = Vec::new();
    for a in 0..3 {
        for c in 0..3 {
            lines.push([idx(0, a, c), idx(1, a, c), idx(2, a, c)]);
            lines.push([idx(a, 0, c), idx(a, 1, c), idx(a, 2, c)]);
            lines.push([idx(a, c, 0), idx(a, c, 1), idx(a, c, 2)]);
        }
        lines.push([idx(0, 0, a), idx(1, 1, a), idx(2, 2, a)]);
        lines.push([idx(2, 0, a), idx(1, 1, a), idx(0, 2, a)]);
        lines.push([idx(0, a, 0), idx(1, a, 1), idx(2, a, 2)]);
        lines.push([idx(2, a, 0), idx(1, a, 1), idx(0, a, 2)]);
        lines.push([idx(a, 0, 0), idx(a, 1, 1), idx(a, 2, 2)]);
        lines.push([idx(a, 2, 0), idx(a, 1, 1), idx(a, 0, 2)]);
    }
    lines.push([0, 13, 26]);
    lines.push([18, 13, 8]);
    lines.push([6, 13, 20]);
    lines.push([24, 13, 2]);
    for [a, c, d] in lines {
        if b[a] != 0 && b[a] == b[c] && b[c] == b[d] {
            return Some(b[a]);
        }
    }
    b.iter().all(|&c| c != 0).then_some(3)
}
