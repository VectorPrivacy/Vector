//! Tic-tac-toe against a bot: the bot ships the app, referees, and plays O.
//!
//! Say `!tictactoe` in a DM with the bot, or in a Community channel it's in,
//! and it sends the game. The first person to move plays X; everyone else who
//! opens it watches the same board.
//!
//! The app (`xdc_tictactoe_2d/`) and the bot speak JSON tagged by `t`:
//!
//! - app → bot  `hello`: show me the board
//! - app → bot  `move {cell}`: X plays cell 0..8 (left to right, top to bottom)
//! - app → bot  `new`: start over
//! - bot → app  `state {board, turn, winner, x}`: the whole game, after every change
//!
//! Run:  cargo run --example xdc_tictactoe_2d_bot --features xdc

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use vector_sdk::{VectorBot, XdcEvent};

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    let bot = VectorBot::builder().data_dir("./xdc-tictactoe-2d-data").public().build().await?;
    let app = package_app("./xdc-tictactoe-2d-data/tictactoe.xdc")?;
    println!("Tic-tac-toe bot online as {}", bot.npub());

    // One game per shared copy of the app, so closing and reopening it resumes.
    let games: Arc<Mutex<HashMap<String, Game>>> = Arc::default();

    bot.xdc("vector-tictactoe-2d").run(move |_bot, mut session| {
        let games = games.clone();
        async move {
            let copy = session.app().message_id().to_string();
            while let Some(event) = session.next().await {
                let changed = match event {
                    XdcEvent::PeerJoined(_) => true,
                    XdcEvent::Data(frame) => {
                        let Some(msg) = frame.json::<Value>() else { continue };
                        let mut games = games.lock().unwrap();
                        let game = games.entry(copy.clone()).or_default();
                        match msg["t"].as_str() {
                            Some("hello") => true,
                            Some("new") => {
                                *game = Game::default();
                                true
                            }
                            // Seats go by verified sender, so nobody can move for someone else.
                            Some("move") => match (frame.verified_sender(), msg["cell"].as_u64()) {
                                (Some(player), Some(cell)) => game.play_x(player, cell as usize),
                                _ => false,
                            },
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if !changed {
                    continue;
                }
                let state = games.lock().unwrap().entry(copy.clone()).or_default().state();
                let _ = session.send_json(&state).await;

                // The bot's reply, after a moment so it reads as a move.
                if state["turn"] == "O" && state["winner"].is_null() {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    let state = {
                        let mut games = games.lock().unwrap();
                        let game = games.entry(copy.clone()).or_default();
                        game.play_o();
                        game.state()
                    };
                    let _ = session.send_json(&state).await;
                }
            }
        }
    });

    bot.on_message(move |_bot, msg| {
        let app = app.clone();
        async move {
            if !msg.is_mine() && msg.text().trim().eq_ignore_ascii_case("!tictactoe") {
                if let Err(e) = msg.channel().send_xdc(&app).await {
                    eprintln!("could not send the app: {e}");
                }
            }
        }
    })
    .await
}

const LINES: [[usize; 3]; 8] = [[0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 3, 6], [1, 4, 7], [2, 5, 8], [0, 4, 8], [2, 4, 6]];

struct Game {
    board: [u8; 9],
    /// Who plays X: whoever moved first.
    x: Option<String>,
}

impl Default for Game {
    fn default() -> Self {
        Game { board: [b'.'; 9], x: None }
    }
}

impl Game {
    /// X's move, if it's X's turn, the cell is free and this player holds (or takes) X.
    fn play_x(&mut self, player: &str, cell: usize) -> bool {
        let free = self.board.get(cell) == Some(&b'.');
        let theirs = self.x.as_deref().map_or(true, |x| x == player);
        if !free || !theirs || self.turn() != b'X' || self.winner().is_some() {
            return false;
        }
        self.x = Some(player.to_string());
        self.board[cell] = b'X';
        true
    }

    /// Win if it can, block if it must, else the best free cell.
    fn play_o(&mut self) {
        let free = |i: &usize| self.board[*i] == b'.';
        let completes = |mark: u8| {
            LINES.iter().find_map(|line| {
                let marks = line.iter().filter(|&&i| self.board[i] == mark).count();
                let empty: Vec<usize> = line.iter().copied().filter(free).collect();
                (marks == 2 && empty.len() == 1).then(|| empty[0])
            })
        };
        let cell = completes(b'O')
            .or_else(|| completes(b'X'))
            .or_else(|| [4, 0, 2, 6, 8, 1, 3, 5, 7].into_iter().find(free));
        if let Some(cell) = cell {
            self.board[cell] = b'O';
        }
    }

    fn turn(&self) -> u8 {
        let xs = self.board.iter().filter(|&&c| c == b'X').count();
        let os = self.board.iter().filter(|&&c| c == b'O').count();
        if xs > os { b'O' } else { b'X' }
    }

    fn winner(&self) -> Option<&'static str> {
        for [a, b, c] in LINES {
            if self.board[a] != b'.' && self.board[a] == self.board[b] && self.board[b] == self.board[c] {
                return Some(if self.board[a] == b'X' { "X" } else { "O" });
            }
        }
        (!self.board.contains(&b'.')).then_some("draw")
    }

    fn state(&self) -> Value {
        json!({
            "t": "state",
            "board": String::from_utf8_lossy(&self.board),
            "turn": if self.turn() == b'X' { "X" } else { "O" },
            "winner": self.winner(),
            "x": self.x,
        })
    }
}

/// Zip the app's files (beside this example) into the `.xdc` the bot sends.
fn package_app(path: &str) -> vector_sdk::Result<std::path::PathBuf> {
    let files: [(&str, &[u8]); 2] = [
        ("index.html", include_bytes!("xdc_tictactoe_2d/index.html")),
        ("manifest.toml", include_bytes!("xdc_tictactoe_2d/manifest.toml")),
    ];
    let path = std::path::PathBuf::from(path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        zip.start_file(name, opts).map_err(|e| vector_sdk::Error::Other(e.to_string()))?;
        zip.write_all(bytes)?;
    }
    zip.finish().map_err(|e| vector_sdk::Error::Other(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bot_wins_when_it_can_and_blocks_when_it_must() {
        let mut g = Game::default();
        g.board = *b"OO.XX....";
        g.play_o();
        assert_eq!(&g.board, b"OOOXX....");
        let mut g = Game::default();
        g.board = *b"XX..O....";
        g.play_o();
        assert_eq!(g.board[2], b'O');
    }

    #[test]
    fn only_the_x_player_moves_and_only_in_turn() {
        let mut g = Game::default();
        assert!(g.play_x("alice", 0));
        assert!(!g.play_x("alice", 1), "O to move");
        g.play_o();
        assert!(!g.play_x("bob", 2), "alice holds X");
        assert!(g.play_x("alice", 8));
    }
}
