//! The session keeper: one task per instance that finds the router, opens both lane sessions,
//! watches them, and starts over with backoff whenever they go. Never sends PING: i2pd answers
//! it and then destroys the session.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;

use super::sam::{self, ControlEvent, ControlSocket, HelloError, Liveness};
use super::{I2pPhase, Inner, Sessions};

struct Backoff {
    steps: Vec<Duration>,
    i: usize,
}

impl Backoff {
    fn new(steps: &[Duration]) -> Self {
        Backoff { steps: steps.to_vec(), i: 0 }
    }

    fn next(&mut self) -> Duration {
        let d = self.steps.get(self.i).or(self.steps.last()).copied().unwrap_or(Duration::from_secs(60));
        self.i = (self.i + 1).min(self.steps.len().saturating_sub(1));
        d
    }

    fn reset(&mut self) {
        self.i = 0;
    }
}

/// Sleep `d`, cut short by a kick or by the owner leaving the screen.
async fn rest(inner: &Inner, d: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {}
        _ = inner.kick.notified() => {}
        _ = wait_owner_gone(inner) => {}
    }
}

/// Resolves once something accepts on the SAM port: a bare loopback connect, nothing sent, so a
/// router that comes back is noticed within seconds instead of at the end of a long backoff.
async fn router_back(inner: &Inner) {
    loop {
        tokio::time::sleep(inner.timing.port_watch).await;
        if tokio::net::TcpStream::connect((sam::SAM_HOST, inner.port)).await.is_ok() {
            return;
        }
    }
}

/// Hold no router sessions while the owning account is off screen.
async fn park(inner: &Arc<Inner>) {
    inner.set_sessions(None);
    inner.set_phase(I2pPhase::Parked, false);
    while !inner.owner_live() {
        let since = crate::transport::epoch();
        tokio::select! {
            _ = crate::transport::changed(since) => {}
            _ = inner.kick.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

enum WatchEnd {
    Lost,
    Renew,
    Parked,
}

pub(crate) async fn run(inner: Arc<Inner>) {
    let mut backoff = Backoff::new(&inner.timing.backoff);
    let mut ever_ready = false;
    let mut skip_probe = false;
    // After a failure, a retry probe keeps the waiting phase (and its reason) until the router
    // answers: a quarter second of "starting" per retry only makes the UI blink.
    let mut waiting = false;
    loop {
        if !inner.owner_live() {
            park(&inner).await;
            waiting = false;
            continue;
        }
        if !skip_probe {
            if !waiting {
                inner.set_phase(I2pPhase::Probing, false);
            }
            match sam::hello(inner.port, inner.auth.as_ref()).await {
                Ok((s, v)) => {
                    drop(s);
                    inner.set_router(true, Some(v));
                }
                Err(e) => {
                    let (wait, phase) = rejection(&inner, &e, &mut backoff);
                    let refused = e == HelloError::Refused;
                    inner.set_router(false, None);
                    inner.set_phase(phase, false);
                    waiting = true;
                    if refused {
                        tokio::select! {
                            _ = rest(&inner, wait) => {}
                            _ = router_back(&inner) => {}
                        }
                    } else {
                        rest(&inner, wait).await;
                    }
                    continue;
                }
            }
        }
        skip_probe = false;
        waiting = false;

        let since = Instant::now();
        inner.set_phase(I2pPhase::CreatingSessions { since }, false);
        let (acct_nick, shared_nick) = (sam::new_nick(), sam::new_nick());
        let both = async {
            tokio::join!(
                sam::create_session(inner.port, inner.auth.as_ref(), &acct_nick),
                sam::create_session(inner.port, inner.auth.as_ref(), &shared_nick),
            )
        };
        let created = tokio::select! {
            r = tokio::time::timeout(inner.timing.session_cap, both) => r,
            _ = wait_owner_gone(&inner) => continue,
        };
        let (mut acct, mut shared) = match created {
            Ok((Ok(a), Ok(s))) => (a, s),
            Ok((a, s)) => {
                let why = a.err().or(s.err()).unwrap_or_default();
                crate::log_warn!("[I2P] the router couldn't open a session: {why}");
                let wait = backoff.next();
                inner.set_phase(I2pPhase::SessionFailed { next: Instant::now() + wait, why }, false);
                waiting = true;
                rest(&inner, wait).await;
                continue;
            }
            Err(_) => {
                let wait = backoff.next();
                inner.set_phase(I2pPhase::SessionFailed { next: Instant::now() + wait, why: "timed out".into() }, false);
                waiting = true;
                rest(&inner, wait).await;
                continue;
            }
        };
        if !inner.owner_live() {
            continue;
        }
        crate::log_info!("[I2P] sessions ready after {:.1}s", since.elapsed().as_secs_f64());
        inner.set_sessions(Some(Sessions {
            account: acct_nick.clone(),
            shared: shared_nick.clone(),
            account_address: acct.address().map(str::to_string),
            shared_address: shared.address().map(str::to_string),
        }));
        inner.renew.store(false, Ordering::Release);
        inner.set_phase(I2pPhase::Ready { since: Instant::now() }, ever_ready);
        ever_ready = true;
        backoff.reset();
        super::probe::check_pending_soon(&inner);

        let end = watch(&inner, &acct_nick, &shared_nick, &mut acct, &mut shared).await;
        inner.set_sessions(None);
        drop(acct);
        drop(shared);
        match end {
            WatchEnd::Renew => {
                inner.set_phase(I2pPhase::Lost { next: Instant::now() }, false);
                inner.renewed.notify_waiters();
                skip_probe = true;
            }
            WatchEnd::Lost => {
                let wait = backoff.next();
                inner.set_phase(I2pPhase::Lost { next: Instant::now() + wait }, false);
                waiting = true;
                rest(&inner, wait).await;
            }
            WatchEnd::Parked => {}
        }
    }
}

async fn wait_owner_gone(inner: &Inner) {
    while inner.owner_live() {
        let since = crate::transport::epoch();
        crate::transport::changed(since).await;
    }
}

fn rejection(inner: &Inner, e: &HelloError, backoff: &mut Backoff) -> (Duration, I2pPhase) {
    let port = inner.port;
    let rejected = |wait: Duration, code: &'static str, text: String| {
        (wait, I2pPhase::SamRejected { next: Instant::now() + wait, code, text })
    };
    match e {
        // A silent port may be a router still waking: the backoff, not the not-SAM wait.
        HelloError::Refused | HelloError::Timeout | HelloError::Silent => {
            let wait = backoff.next();
            (wait, I2pPhase::RouterUnreachable { next: Instant::now() + wait })
        }
        HelloError::NotSam => {
            rejected(inner.timing.not_sam, "sam_refused", format!("The service on port {port} isn't an I2P SAM bridge."))
        }
        HelloError::NoVersion(v) => rejected(inner.timing.rejected, "sam_too_old", sam::too_old_text(v)),
        HelloError::AuthRequired => {
            rejected(inner.timing.rejected, "sam_auth_required", "Your router asks for a SAM username and password.".into())
        }
        HelloError::AuthFailed => {
            rejected(inner.timing.rejected, "sam_auth_failed", "Your router turned down the SAM username or password.".into())
        }
    }
}

/// Watch both sessions until one goes, the owner leaves the screen, or new addresses are asked
/// for. Liveness runs every `watch` interval and at once on a kick.
async fn watch(inner: &Arc<Inner>, acct_nick: &str, shared_nick: &str, acct: &mut ControlSocket, shared: &mut ControlSocket) -> WatchEnd {
    let mut quiet_until: Option<Instant> = None;
    loop {
        let event = tokio::select! {
            _ = tokio::time::sleep(inner.timing.watch) => None,
            _ = inner.kick.notified() => None,
            _ = wait_owner_gone(inner) => None,
            e = acct.next_event() => Some((true, e)),
            e = shared.next_event() => Some((false, e)),
        };
        match event {
            Some((_, ControlEvent::Closed(why))) => {
                crate::log_warn!("[I2P] a session closed: {why}");
                return WatchEnd::Lost;
            }
            Some((is_acct, ControlEvent::Ping(text))) => {
                let sock = if is_acct { &mut *acct } else { &mut *shared };
                if sock.pong(&text).await.is_err() {
                    return WatchEnd::Lost;
                }
                continue;
            }
            Some((_, ControlEvent::Other)) => continue,
            None => {}
        }
        if !inner.owner_live() {
            return WatchEnd::Parked;
        }
        if inner.renew.swap(false, Ordering::AcqRel) {
            return WatchEnd::Renew;
        }
        let (a, s) = tokio::join!(
            sam::liveness(inner.port, inner.auth.as_ref(), acct_nick),
            sam::liveness(inner.port, inner.auth.as_ref(), shared_nick),
        );
        for l in [&a, &s] {
            match l {
                Liveness::Gone => {
                    inner.set_router(true, None);
                    return WatchEnd::Lost;
                }
                Liveness::RouterDown => {
                    inner.set_router(false, None);
                    return WatchEnd::Lost;
                }
                Liveness::Inconclusive(why) => {
                    if quiet_until.is_none_or(|t| Instant::now() >= t) {
                        crate::log_warn!("[I2P] liveness inconclusive: {why}");
                        quiet_until = Some(Instant::now() + Duration::from_secs(60));
                    }
                }
                Liveness::Alive => {}
            }
        }
    }
}
