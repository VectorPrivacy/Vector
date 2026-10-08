//! Time budgets: a caller's clearnet value, raised to the floor of the chosen kind.

use std::time::Duration;

use super::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    RelayConnect,
    RelayRequest,
    HttpConnect,
    HttpTotal,
    HttpRead,
    TransferStall,
    ReadyWait,
    Startup,
    /// A request beside a send that the send waits on but never fails over (mirror fan-out,
    /// the post-upload check, an upload's preflight). Tor keeps the caller's value.
    BestEffort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    pub relay_connect: Duration,
    pub relay_request: Duration,
    pub http_connect: Duration,
    pub http: Duration,
    pub transfer_stall: Duration,
    pub ready_wait: Duration,
    pub startup: Duration,
    pub best_effort: Duration,
}

impl Budgets {
    pub fn floor(&self, op: Op) -> Duration {
        match op {
            Op::RelayConnect => self.relay_connect,
            Op::RelayRequest => self.relay_request,
            Op::HttpConnect => self.http_connect,
            Op::HttpTotal | Op::HttpRead => self.http,
            Op::TransferStall => self.transfer_stall,
            Op::ReadyWait => self.ready_wait,
            Op::Startup => self.startup,
            Op::BestEffort => self.best_effort,
        }
    }
}

const fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

const CLEARNET: Budgets = Budgets {
    relay_connect: secs(0),
    relay_request: secs(0),
    http_connect: secs(0),
    http: secs(0),
    transfer_stall: secs(0),
    ready_wait: secs(0),
    startup: secs(0),
    best_effort: secs(0),
};

/// Circuit construction dominates under Tor; these are the values Vector has always used.
const TOR: Budgets = Budgets {
    relay_connect: secs(60),
    relay_request: secs(30),
    http_connect: secs(45),
    http: secs(90),
    transfer_stall: secs(120),
    ready_wait: secs(30),
    startup: secs(120),
    best_effort: secs(0),
};

/// Session setup takes 9-42 s, sends stall 10-60 s and a first connect to an unseen
/// destination around 32 s; a first request through an outproxy to a new host takes 5-15 s.
const I2P: Budgets = Budgets {
    relay_connect: secs(90),
    relay_request: secs(60),
    http_connect: secs(90),
    http: secs(120),
    transfer_stall: secs(180),
    ready_wait: secs(90),
    startup: secs(180),
    best_effort: secs(45),
};

pub(super) fn table(kind: Kind) -> &'static Budgets {
    match kind {
        Kind::Clearnet => &CLEARNET,
        Kind::Tor => &TOR,
        Kind::I2p => &I2P,
    }
}

/// The highest floor of any compiled kind: what an Unknown preference waits for.
pub fn highest_compiled_floor(op: Op) -> Duration {
    super::supported().into_iter().map(|k| table(k).floor(op)).max().unwrap_or_default()
}

/// `max(clearnet, floor of the chosen kind)`. A kind's floors hold while it is starting too,
/// since that is when connects are slowest.
pub fn budget(op: Op, clearnet: Duration) -> Duration {
    #[cfg(test)]
    if let Some(d) = FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(o, _)| *o == op).map(|(_, d)| *d) {
        return clearnet.max(d);
    }
    let floor = match super::preference() {
        Some(k) => table(k).floor(op),
        None => highest_compiled_floor(op),
    };
    clearnet.max(floor)
}

/// Budgets short enough for a test to wait them out, in place of the chosen kind's floor.
#[cfg(test)]
static FOR_TEST: std::sync::Mutex<Vec<(Op, Duration)>> = std::sync::Mutex::new(Vec::new());

/// Set (or with `None`, drop) a test's budget for `op`.
#[cfg(test)]
pub(crate) fn override_for_test(op: Op, d: Option<Duration>) {
    let mut v = FOR_TEST.lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|(o, _)| *o != op);
    if let Some(d) = d {
        v.push((op, d));
    }
}

#[cfg(test)]
pub(crate) fn clear_overrides_for_test() {
    FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
