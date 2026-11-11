//! Live end-to-end checks of Vector bots. The parent process runs each scenario by starting its
//! roles as child processes of this same binary (a bot is one identity per process), which meet
//! through files in a shared working directory and each write the checks they made.

mod events;
mod flood;
mod gap;
#[cfg(feature = "voice")]
mod voice;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vector_sdk::{BotEvent, NewCommunity, VectorBot};

/// What a run was asked for, as every role reads it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Opts {
    pub relays: Vec<String>,
    pub long: bool,
    pub bots: usize,
    pub messages: usize,
}

/// One role's working directory, shared with the others in its scenario.
pub struct Lab {
    pub dir: PathBuf,
    pub opts: Opts,
}

impl Lab {
    /// Wait for another role to leave `name` here; its contents.
    pub async fn wait(&self, name: &str, secs: u64) -> Option<String> {
        let until = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < until {
            if let Ok(s) = std::fs::read_to_string(self.dir.join(name)) {
                return Some(s);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        None
    }

    /// Leave `name` for the other roles, whole or not at all.
    pub fn put(&self, name: &str, body: &str) {
        let tmp = self.dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, body).expect("write to the lab directory");
        std::fs::rename(tmp, self.dir.join(name)).expect("write to the lab directory");
    }

    /// A Community for this run, on the relays asked for.
    pub fn community(&self, scenario: &str) -> NewCommunity {
        let mut c = NewCommunity::default().description(format!("vector-lab {scenario}: a test, safe to ignore"));
        if !self.opts.relays.is_empty() {
            c = c.relays(self.opts.relays.clone());
        }
        c
    }
}

/// What a step came to, for a check's detail.
pub fn outcome<T, E: std::fmt::Display>(r: &Result<T, E>) -> String {
    match r {
        Ok(_) => "ok".into(),
        Err(e) => e.to_string(),
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Every event a bot saw, with when.
#[derive(Clone, Default)]
pub struct Seen(Arc<Mutex<Vec<(u64, BotEvent)>>>);

impl Seen {
    pub fn count(&self, f: impl Fn(&BotEvent) -> bool) -> usize {
        self.0.lock().unwrap().iter().filter(|(_, e)| f(e)).count()
    }

    /// The first event `f` picks, waiting up to `secs` for one.
    pub async fn wait_for<T>(&self, secs: u64, f: impl Fn(&BotEvent) -> Option<T>) -> Option<T> {
        let until = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(found) = self.0.lock().unwrap().iter().find_map(|(_, e)| f(e)) {
                return Some(found);
            }
            if Instant::now() > until {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub fn all(&self) -> Vec<(u64, BotEvent)> {
        self.0.lock().unwrap().clone()
    }
}

/// A bot with a fresh identity in its own directory, listening, its events gathered.
pub async fn bot(lab: &Lab, who: &str) -> (VectorBot, Seen) {
    let bot = VectorBot::builder().data_dir(lab.dir.join(who)).public().build().await.expect("start the bot");
    let seen = Seen::default();
    let gathered = seen.clone();
    let listener = bot.clone();
    tokio::spawn(async move {
        let _ = listener
            .run(move |_, event| {
                let gathered = gathered.clone();
                async move { gathered.0.lock().unwrap().push((now_ms(), event)) }
            })
            .await;
    });
    let _ = seen.wait_for(120, |e| matches!(e, BotEvent::Ready { .. }).then_some(())).await;
    (bot, seen)
}

/// One thing a role checked.
#[derive(Debug, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

/// The checks one role makes, written for the parent when the role ends.
pub struct Checks {
    role: String,
    file: PathBuf,
    items: Vec<Check>,
}

impl Checks {
    pub fn new(lab: &Lab, role: &str) -> Self {
        Checks { role: role.to_string(), file: lab.dir.join(format!("checks-{role}.json")), items: Vec::new() }
    }

    pub fn check(&mut self, name: &str, pass: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!("[{}] {} {name}: {detail}", self.role, if pass { "PASS" } else { "FAIL" });
        self.items.push(Check { name: name.to_string(), pass, detail });
    }

    /// Exactly one, as `what`.
    pub fn once(&mut self, name: &str, n: usize) {
        self.check(name, n == 1, format!("seen {n}x"));
    }

    pub fn note(&mut self, name: &str, detail: impl Into<String>) {
        self.check(name, true, detail);
    }
}

impl Drop for Checks {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.file, serde_json::to_string(&self.items).unwrap_or_default());
    }
}

/// The roles each scenario runs, and how long it may take.
fn roles(scenario: &str, opts: &Opts) -> Option<(Vec<String>, u64)> {
    Some(match scenario {
        "events" => (vec!["owner".into(), "a".into(), "b".into()], 600),
        "gap" => (vec!["owner".into(), "away".into(), "back".into()], if opts.long { 1200 } else { 600 }),
        #[cfg(feature = "voice")]
        "voice" => (vec!["owner".into(), "a".into(), "b".into()], 600),
        "flood" => (std::iter::once("owner".to_string()).chain((0..opts.bots).map(|i| format!("bot{i}"))).collect(), 900),
        _ => return None,
    })
}

async fn run_role(scenario: &str, role: &str, lab: Lab) {
    match scenario {
        "events" => events::run(role, lab).await,
        "gap" => gap::run(role, lab).await,
        #[cfg(feature = "voice")]
        "voice" => voice::run(role, lab).await,
        "flood" => flood::run(role, lab).await,
        _ => {}
    }
}

/// Run one scenario's roles to the end; its checks, and whether it ran out of time.
async fn scenario(name: &str, opts: &Opts, keep: bool) -> (Vec<(String, Check)>, bool) {
    let (roles, secs) = roles(name, opts).expect("a known scenario");
    let dir = std::env::temp_dir().join(format!("vector-lab-{name}-{}-{}", std::process::id(), now_ms()));
    std::fs::create_dir_all(&dir).expect("make the lab directory");
    std::fs::write(dir.join("opts.json"), serde_json::to_string(opts).unwrap()).unwrap();
    let exe = std::env::current_exe().expect("this binary");
    let mut children = Vec::new();
    for role in &roles {
        let log = std::fs::File::create(dir.join(format!("{role}.log"))).expect("a log file");
        let child = tokio::process::Command::new(&exe)
            .args(["--role", name, role])
            .arg(&dir)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()
            .expect("start a role");
        children.push(child);
    }
    let all = futures_join(children);
    let timed_out = tokio::time::timeout(Duration::from_secs(secs), all).await.is_err();
    let mut out = Vec::new();
    for role in &roles {
        let checks: Vec<Check> = std::fs::read_to_string(dir.join(format!("checks-{role}.json")))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| vec![Check { name: "finished".into(), pass: false, detail: "the role wrote no checks: see its log".into() }]);
        out.extend(checks.into_iter().map(|c| (role.clone(), c)));
    }
    if keep || timed_out || out.iter().any(|(_, c)| !c.pass) {
        println!("  logs: {}", dir.display());
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
    (out, timed_out)
}

async fn futures_join(children: Vec<tokio::process::Child>) {
    for mut c in children {
        let _ = c.wait().await;
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--role") {
        let (name, role, dir) = (&args[1], &args[2], PathBuf::from(&args[3]));
        let opts: Opts = serde_json::from_str(&std::fs::read_to_string(dir.join("opts.json")).unwrap()).unwrap();
        run_role(name, role, Lab { dir, opts }).await;
        return;
    }

    let mut opts = Opts { bots: 4, messages: 25, ..Default::default() };
    let (mut wanted, mut keep) = (Vec::new(), false);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--relays" => opts.relays = it.next().unwrap_or_default().split(',').map(str::trim).filter(|r| !r.is_empty()).map(String::from).collect(),
            "--long" => opts.long = true,
            "--keep" => keep = true,
            "--bots" => opts.bots = it.next().and_then(|n| n.parse().ok()).unwrap_or(4).max(2),
            "--messages" => opts.messages = it.next().and_then(|n| n.parse().ok()).unwrap_or(25).max(1),
            "all" => wanted.extend(["events", "gap", "voice", "flood"].map(String::from)),
            "-h" | "--help" => return usage(),
            s => wanted.push(s.to_string()),
        }
    }
    if wanted.is_empty() {
        return usage();
    }
    let mut failed = 0;
    for name in wanted {
        if roles(&name, &opts).is_none() {
            println!("{name}: not a scenario this build runs");
            failed += 1;
            continue;
        }
        println!("== {name}");
        let t = Instant::now();
        let (checks, timed_out) = scenario(&name, &opts, keep).await;
        for (role, c) in &checks {
            println!("  {} {role}: {} ({})", if c.pass { "PASS" } else { "FAIL" }, c.name, c.detail);
        }
        let bad = checks.iter().filter(|(_, c)| !c.pass).count() + timed_out as usize;
        if timed_out {
            println!("  FAIL: ran out of time");
        }
        println!("  {name}: {} in {:.0}s", if bad == 0 { "passed" } else { "FAILED" }, t.elapsed().as_secs_f32());
        failed += bad;
    }
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

fn usage() {
    println!("vector-lab [events] [gap] [voice] [flood] [all] [--relays wss://a,wss://b] [--long] [--keep] [--bots N] [--messages M]");
}
