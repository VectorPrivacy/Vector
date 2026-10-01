//! A bot that plays 3D Tic-Tac-Toe (the `3d-tic-tac-toe` Mini App) against
//! whoever opens it.
//!
//! Share the app in a DM with the bot (or a Community it's in) and open it: the
//! bot joins the game's realtime channel and takes the other seat. It speaks
//! the app's own protocol, JSON frames tagged by `t`:
//!
//! - `hello {name, at}`: a player arrived; the two earliest `at`s hold X and O
//! - `state {board, scores, gameOver}`: the host's (X's) authoritative snapshot
//! - `move {cell, player, board}`: the guest's move, carrying the whole board
//! - `new`: start a fresh board
//!
//! Every frame carries `by` = the sender's `selfAddr` (its npub).
//!
//! Run:  cargo run --example xdc_tictactoe_bot --features xdc

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use vector_sdk::{VectorBot, XdcEvent, XdcSession};

const EMPTY: u8 = 0;
const X: u8 = 1;
const O: u8 = 2;

#[tokio::main]
async fn main() -> vector_sdk::Result<()> {
    // Public: anyone may invite it into a Community to play there.
    let bot = VectorBot::builder().data_dir("./xdc-tictactoe-data").public().build().await?;
    println!("TicTacBot online as {}", bot.npub());

    bot.xdc("3d-tic-tac-toe").run(|_bot, session| async move {
        Game::new(session).play().await;
    });

    // Anything else said to the bot gets pointed at the game.
    bot.on_message(|_bot, msg| async move {
        if !msg.is_mine() && msg.xdc().is_none() {
            let _ = msg.reply("Send me 3D Tic-Tac-Toe and open it: I'll take the other seat.").await;
        }
    })
    .await
}

struct Game {
    session: XdcSession,
    me: String,
    my_at: u64,
    /// Everyone who said hello: (addr, opened-at).
    peers: Vec<(String, u64)>,
    board: [u8; 27],
    scores: Value,
    game_over: bool,
    seat_x: String,
    seat_o: Option<String>,
}

impl Game {
    fn new(session: XdcSession) -> Self {
        let me = session.self_addr().to_string();
        Self {
            // Joined after the player opened it, so the player keeps X and hosts.
            my_at: now_ms(),
            seat_x: me.clone(),
            me,
            session,
            peers: Vec::new(),
            board: [EMPTY; 27],
            scores: json!({ "x": 0, "o": 0, "draw": 0 }),
            game_over: false,
            seat_o: None,
        }
    }

    async fn play(mut self) {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(4));
        self.hello().await;
        loop {
            tokio::select! {
                event = self.session.next() => match event {
                    Some(XdcEvent::Data(frame)) => {
                        if let Some(m) = frame.json::<Value>() {
                            self.on_frame(m).await;
                        }
                    }
                    // A player connected: introduce ourselves so seats settle.
                    Some(XdcEvent::PeerJoined(p)) => {
                        println!("connected: {}", p.npub.as_deref().unwrap_or("unknown node"));
                        self.hello().await;
                    }
                    Some(XdcEvent::PeerLeft(p)) => println!("disconnected: {}", p.npub.as_deref().unwrap_or("unknown node")),
                    Some(_) => {}
                    None => break,
                },
                _ = heartbeat.tick() => {
                    if self.am_host() && !self.peers.is_empty() {
                        self.assert_state().await;
                    }
                }
            }
        }
        println!("session over");
    }

    async fn send(&self, mut m: Value) {
        m["by"] = json!(self.me);
        if let Err(e) = self.session.send_json(&m).await {
            eprintln!("send failed: {e}");
        }
    }

    async fn hello(&self) {
        self.send(json!({ "t": "hello", "name": "TicTacBot", "at": self.my_at })).await;
    }

    async fn assert_state(&self) {
        self.send(json!({ "t": "state", "board": self.board.to_vec(), "scores": self.scores, "gameOver": self.game_over }))
            .await;
    }

    fn am_host(&self) -> bool {
        self.seat_x == self.me
    }

    fn my_seat(&self) -> u8 {
        if self.seat_x == self.me {
            X
        } else if self.seat_o.as_deref() == Some(self.me.as_str()) {
            O
        } else {
            EMPTY
        }
    }

    fn in_progress(&self) -> bool {
        self.board.iter().any(|&c| c != EMPTY) || self.game_over
    }

    /// The app's seating: two earliest openers play, npub breaks ties; frozen mid-game.
    fn recompute_seats(&mut self) {
        if self.in_progress() {
            return;
        }
        let mut list: Vec<(String, u64)> = self.peers.clone();
        list.push((self.me.clone(), self.my_at));
        list.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        self.seat_x = list[0].0.clone();
        self.seat_o = list.get(1).map(|p| p.0.clone());
    }

    async fn on_frame(&mut self, m: Value) {
        let Some(by) = m["by"].as_str().map(str::to_string) else { return };
        if by == self.me {
            return;
        }
        match m["t"].as_str() {
            Some("hello") => {
                let at = m["at"].as_u64().unwrap_or(0);
                let known = self.peers.iter().position(|p| p.0 == by);
                // The bot outlives an app window: the same player with a new `at`
                // reopened the app, which starts with an empty board and no memory
                // of us. Treat it as a new arrival, and a seated player's return as
                // a new game.
                let reopened = known.is_some_and(|i| self.peers[i].1 != at);
                if let Some(i) = known.filter(|_| reopened) {
                    self.peers.remove(i);
                    if by == self.seat_x || self.seat_o.as_deref() == Some(by.as_str()) {
                        self.board = [EMPTY; 27];
                        self.game_over = false;
                    }
                }
                if known.is_none() || reopened {
                    self.peers.push((by.clone(), at));
                    println!("{} joined", m["name"].as_str().unwrap_or("someone"));
                    self.hello().await;
                }
                self.recompute_seats();
                if self.am_host() && self.in_progress() {
                    self.assert_state().await;
                }
            }
            Some("state") if by == self.seat_x && !self.am_host() => {
                // A snapshot older than our own last move is in flight from before
                // the host saw it; a fresh board (0 filled) is a new game.
                if let Some(board) = parse_board(&m["board"]).filter(|b| filled(b) >= filled(&self.board) || filled(b) == 0) {
                    self.board = board;
                    self.scores = m["scores"].clone();
                    self.game_over = m["gameOver"].as_bool().unwrap_or(false);
                }
            }
            Some("move") if !self.game_over => {
                if let Some(board) = parse_board(&m["board"]) {
                    if filled(&board) > filled(&self.board) {
                        self.board = board;
                        if self.am_host() {
                            self.settle().await;
                            self.assert_state().await;
                        }
                    }
                }
            }
            Some("new") => {
                self.board = [EMPTY; 27];
                self.game_over = false;
            }
            _ => {}
        }
        self.maybe_move().await;
    }

    /// Host only: tally a finished game, as the app's host does.
    async fn settle(&mut self) {
        if let Some(w) = winner(&self.board) {
            self.game_over = true;
            let key = match w {
                X => "x",
                O => "o",
                _ => "draw",
            };
            self.scores[key] = json!(self.scores[key].as_u64().unwrap_or(0) + 1);
        }
    }

    async fn maybe_move(&mut self) {
        let seat = self.my_seat();
        if seat == EMPTY || self.game_over || self.seat_o.is_none() || turn(&self.board) != seat {
            return;
        }
        // A beat, so the move reads as a move and not a reflex.
        tokio::time::sleep(Duration::from_millis(700)).await;
        let Some(cell) = choose(&self.board, seat) else { return };
        self.board[cell] = seat;
        if self.am_host() {
            self.settle().await;
            self.assert_state().await;
        } else {
            self.send(json!({ "t": "move", "cell": cell, "player": seat, "board": self.board.to_vec() })).await;
        }
        if let Some(w) = winner(&self.board) {
            self.game_over = true;
            let line = if w == seat { "GG! Press NEW GAME for a rematch." } else { "Nice one." };
            let _ = self.session.app().channel().send(line).await;
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn parse_board(v: &Value) -> Option<[u8; 27]> {
    let arr = v.as_array()?;
    if arr.len() != 27 {
        return None;
    }
    let mut b = [EMPTY; 27];
    for (i, c) in arr.iter().enumerate() {
        b[i] = match c.as_u64()? {
            1 => X,
            2 => O,
            _ => EMPTY,
        };
    }
    Some(b)
}

fn filled(b: &[u8; 27]) -> usize {
    b.iter().filter(|&&c| c != EMPTY).count()
}

/// X moves first; equal counts mean it is X's turn.
fn turn(b: &[u8; 27]) -> u8 {
    let xs = b.iter().filter(|&&c| c == X).count();
    let os = b.iter().filter(|&&c| c == O).count();
    if xs == os { X } else { O }
}

fn idx(x: usize, y: usize, z: usize) -> usize {
    x * 9 + y * 3 + z
}

/// The 49 lines of a 3×3×3 cube, in the app's order.
fn lines() -> Vec<[usize; 3]> {
    let mut l = Vec::with_capacity(49);
    for a in 0..3 {
        for b in 0..3 {
            l.push([idx(0, a, b), idx(1, a, b), idx(2, a, b)]);
            l.push([idx(a, 0, b), idx(a, 1, b), idx(a, 2, b)]);
            l.push([idx(a, b, 0), idx(a, b, 1), idx(a, b, 2)]);
        }
    }
    for a in 0..3 {
        l.push([idx(0, 0, a), idx(1, 1, a), idx(2, 2, a)]);
        l.push([idx(2, 0, a), idx(1, 1, a), idx(0, 2, a)]);
        l.push([idx(0, a, 0), idx(1, a, 1), idx(2, a, 2)]);
        l.push([idx(2, a, 0), idx(1, a, 1), idx(0, a, 2)]);
        l.push([idx(a, 0, 0), idx(a, 1, 1), idx(a, 2, 2)]);
        l.push([idx(a, 2, 0), idx(a, 1, 1), idx(a, 0, 2)]);
    }
    l.push([idx(0, 0, 0), idx(1, 1, 1), idx(2, 2, 2)]);
    l.push([idx(2, 0, 0), idx(1, 1, 1), idx(0, 2, 2)]);
    l.push([idx(0, 2, 0), idx(1, 1, 1), idx(2, 0, 2)]);
    l.push([idx(2, 2, 0), idx(1, 1, 1), idx(0, 0, 2)]);
    l
}

/// `X`, `O`, `3` for a draw, or `None` while play continues.
fn winner(b: &[u8; 27]) -> Option<u8> {
    for [a, c, d] in lines() {
        if b[a] != EMPTY && b[a] == b[c] && b[c] == b[d] {
            return Some(b[a]);
        }
    }
    (filled(b) == 27).then_some(3)
}

fn winning_cell(b: &[u8; 27], p: u8) -> Option<usize> {
    lines().into_iter().find_map(|line| {
        let mine = line.iter().filter(|&&i| b[i] == p).count();
        let empty: Vec<usize> = line.iter().copied().filter(|&i| b[i] == EMPTY).collect();
        (mine == 2 && empty.len() == 1).then(|| empty[0])
    })
}

fn fork_cell(b: &[u8; 27], p: u8) -> Option<usize> {
    (0..27).filter(|&i| b[i] == EMPTY).find(|&i| {
        let mut t = *b;
        t[i] = p;
        lines()
            .iter()
            .filter(|line| {
                line.iter().filter(|&&j| t[j] == p).count() == 2 && line.iter().filter(|&&j| t[j] == EMPTY).count() == 1
            })
            .count()
            >= 2
    })
}

/// The app's own "hard" A.I.: win, block, centre, fork, block a fork, then heuristics.
fn choose(b: &[u8; 27], me: u8) -> Option<usize> {
    let opp = if me == X { O } else { X };
    winning_cell(b, me)
        .or_else(|| winning_cell(b, opp))
        .or_else(|| (b[13] == EMPTY).then_some(13))
        .or_else(|| fork_cell(b, me))
        .or_else(|| fork_cell(b, opp))
        .or_else(|| {
            (0..27).filter(|&i| b[i] == EMPTY).max_by_key(|&i| {
                let (x, y, z) = (i / 9, (i % 9) / 3, i % 3);
                let mut score = 0;
                if [x, y, z].iter().all(|v| *v != 1) {
                    score += 3;
                }
                score += [x, y, z].iter().filter(|v| **v == 1).count().min(2);
                for line in lines().iter().filter(|l| l.contains(&i)) {
                    if !line.iter().any(|&j| b[j] == opp) {
                        score += line.iter().filter(|&&j| b[j] == me).count() + 1;
                    }
                }
                score
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_has_49_distinct_lines() {
        let mut l = lines();
        assert_eq!(l.len(), 49);
        for line in &mut l {
            line.sort();
        }
        l.sort();
        l.dedup();
        assert_eq!(l.len(), 49);
    }

    #[test]
    fn it_takes_a_win_then_blocks_one() {
        let mut b = [EMPTY; 27];
        b[0] = O;
        b[1] = O;
        b[9] = X;
        b[18] = X;
        assert_eq!(choose(&b, O), Some(2), "completes its own line first");
        b[0] = EMPTY;
        assert_eq!(choose(&b, O), Some(0), "blocks X's 0-9-18 line");
    }

    #[test]
    fn turns_and_wins_follow_the_board() {
        let mut b = [EMPTY; 27];
        assert_eq!(turn(&b), X);
        b[13] = X;
        assert_eq!(turn(&b), O);
        b[0] = X;
        b[26] = X;
        assert_eq!(winner(&b), Some(X));
    }
}
