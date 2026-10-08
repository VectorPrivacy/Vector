//! The network side, and the one transfer a process runs at a time.
//!
//! A temporary client on the trusted relays carries the session, signing every event and every
//! relay AUTH as a throwaway key, never the account. Progress reaches the page as `transfer_state`.
//! The sender's page never sees the number: the user carries it from the new device's screen.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use nostr_sdk::prelude::*;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use super::code::Code;
use super::session::{ApproveError, Bundle, Failure, Out, Presentation, Role, Session};
use super::wire::Msg;
use crate::rt::time::{sleep_until, Instant};

/// Ephemeral: relays pass it on without keeping it for long.
pub const KIND: u16 = 24681;
/// How long a shown code waits for the other device.
const CODE_WINDOW: Duration = Duration::from_secs(5 * 60);
/// How long a matched pair waits for the user to approve. PIN entry and Tor can be slow.
const APPROVAL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// How long a receiver stays to answer a repeated bundle after it has the identity.
const LINGER: Duration = Duration::from_secs(20);
/// Gaps between repeats of a message the peer hasn't answered.
const BACKOFF_SECS: [u64; 4] = [3, 6, 12, 15];
/// Events older than the session by more than this are someone else's.
const STALE_SECS: u64 = 120;

enum Command {
    CheckNumber(String, oneshot::Sender<Result<(), ApproveError>>),
    Approve(Bundle, oneshot::Sender<Result<(), ApproveError>>),
    Deny,
    Cancel,
}

struct Active {
    id: u64,
    role: Role,
    cmd: mpsc::UnboundedSender<Command>,
    /// A sender's account. Approval refuses once it's no longer the one on screen.
    owner: Option<Arc<crate::db::Session>>,
}

// Process-wide rather than on the session: a receiver has no account yet, and a sender's run
// carries its own account in `owner`.
static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);
/// The identity a receiver holds until it has signed in with it, and when it arrived.
static RECEIVED: Mutex<Option<(u64, Bundle, Instant)>> = Mutex::new(None);
/// How long an identity waits for its sign-in before it's wiped.
const RECEIVED_TTL: Duration = Duration::from_secs(10 * 60);
/// How often dropped relays get another try.
const RECONNECT_EVERY: Duration = Duration::from_secs(10);
/// Moved by every start and cancel, so a start that a newer one overtook installs nothing.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// What the page is told, as `transfer_state`.
#[derive(Serialize, Default)]
struct UiState {
    /// The transfer this belongs to, so the page ignores a session it has moved on from.
    id: u64,
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// The sender's avatar, re-encoded here as a PNG data URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    unconfirmed: bool,
}

/// What `start` hands back: the transfer's id, and the code to display when this device shows it.
pub struct Started {
    pub id: u64,
    pub code: Option<String>,
    pub qr: Option<String>,
}

/// Begin a transfer. With `typed`, this device joins that code; without, it shows a new one once
/// the relays are listening. A sender passes the identity it will hand over.
pub async fn start(role: Role, typed: Option<&str>, identity: Option<PublicKey>) -> Result<Started, String> {
    let code = match typed {
        Some(t) => Code::parse(t)?,
        None => Code::generate(),
    };
    let presentation = if typed.is_some() { Presentation::Joiner } else { Presentation::Shower };
    if role == Role::Sender && identity.is_none() {
        return Err("This account has no key on this device to move".to_string());
    }
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    stop_active();
    let id = next_id();
    let (cmd, cmds) = mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let owner = (role == Role::Sender).then(crate::db::current_session);
    let face = match identity {
        Some(pk) if role == Role::Sender => own_face(pk).await,
        _ => (None, None),
    };
    {
        let mut active = ACTIVE.lock().unwrap();
        if GENERATION.load(std::sync::atomic::Ordering::SeqCst) != generation {
            return Err("A newer transfer replaced this one".to_string());
        }
        if let Some(previous) = active.replace(Active { id, role, cmd, owner: owner.clone() }) {
            let _ = previous.cmd.send(Command::Cancel);
        }
    }
    // spawn-detached: a receiver has no account yet; a sender's run checks `owner` before acting.
    crate::rt::spawn(run(id, role, presentation, code.clone(), identity, face, cmds, ready_tx, owner));
    match ready_rx.await {
        Ok(Ok(())) => Ok(match presentation {
            Presentation::Shower => Started { id, code: Some(code.canonical().to_string()), qr: Some(code.qr().to_string()) },
            Presentation::Joiner => Started { id, code: None, qr: None },
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("The transfer stopped before it began".to_string()),
    }
}

/// This device's name and a thumbnail of its cached avatar, for the new device's screen.
async fn own_face(pk: PublicKey) -> (Option<String>, Option<Vec<u8>>) {
    let Ok(npub) = pk.to_bech32();
    let (name, cached) = {
        let state = crate::STATE.lock().await;
        match state.get_profile(&npub) {
            Some(p) => {
                let name = if p.display_name.trim().is_empty() { p.name.trim() } else { p.display_name.trim() };
                ((!name.is_empty()).then(|| name.to_string()), p.avatar_cached.to_string())
            }
            None => (None, String::new()),
        }
    };
    // An avatar file this large isn't one Vector cached; skip it rather than decode it.
    const MAX_SOURCE: usize = 8 * 1024 * 1024;
    let source = match cached.is_empty() {
        true => None,
        false => crate::files::read(std::path::Path::new(&cached)).await.ok().filter(|b| b.len() <= MAX_SOURCE),
    };
    let avatar = match source {
        Some(bytes) => crate::rt::spawn_blocking(move || super::avatar::thumbnail(&bytes)).await.ok().flatten(),
        None => None,
    };
    (name, avatar)
}

fn command(make: impl FnOnce() -> Command) -> Result<(), String> {
    let guard = ACTIVE.lock().unwrap();
    let active = guard.as_ref().ok_or("No transfer is running")?;
    active.cmd.send(make()).map_err(|_| "The transfer has ended".to_string())
}

fn active_role() -> Option<Role> {
    ACTIVE.lock().unwrap().as_ref().map(|a| a.role)
}

/// Sender: is this the number on the new device's screen?
pub async fn check_number(typed: &str) -> Result<(), ApproveError> {
    if active_role() != Some(Role::Sender) {
        return Err(ApproveError::NotReady);
    }
    let (tx, rx) = oneshot::channel();
    let typed = typed.to_string();
    command(|| Command::CheckNumber(typed, tx)).map_err(|_| ApproveError::NotReady)?;
    rx.await.unwrap_or(Err(ApproveError::NotReady))
}

/// Whether a sender's transfer may still hand over: its account is the one on screen.
pub fn sender_is_current() -> bool {
    matches!(ACTIVE.lock().unwrap().as_ref(), Some(Active { role: Role::Sender, owner: Some(owner), .. }) if owner.is_live())
}

/// Sender: hand the identity over. The host has already proven the user and opened the key.
pub async fn approve(bundle: Bundle) -> Result<(), ApproveError> {
    if !sender_is_current() {
        return Err(ApproveError::NotReady);
    }
    let (tx, rx) = oneshot::channel();
    command(|| Command::Approve(bundle, tx)).map_err(|_| ApproveError::NotReady)?;
    rx.await.unwrap_or(Err(ApproveError::NotReady))
}

pub fn deny() {
    let _ = command(|| Command::Deny);
}

/// Stop whatever transfer is running and forget anything it received.
pub fn cancel() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    stop_active();
}

fn stop_active() {
    // Under the ACTIVE lock, so an identity arriving for the run being stopped can't land after this.
    let mut active = ACTIVE.lock().unwrap();
    if let Some(previous) = active.take() {
        let _ = previous.cmd.send(Command::Cancel);
    }
    RECEIVED.lock().unwrap().take();
}

/// Receiver: the identity that arrived. It stays held until [`forget_received`], so a sign-in
/// that fails can be tried again.
pub fn received() -> Option<Bundle> {
    let mut held = RECEIVED.lock().unwrap();
    if held.as_ref().is_some_and(|(_, _, at)| at.elapsed() > RECEIVED_TTL) {
        held.take();
    }
    held.as_ref().map(|(_, bundle, _)| bundle.clone())
}

/// Receiver: signed in with it; let it go.
pub fn forget_received() {
    RECEIVED.lock().unwrap().take();
}

#[cfg(test)]
static TAP: Mutex<Vec<(u64, &'static str, serde_json::Value)>> = Mutex::new(Vec::new());

fn emit(id: u64, mut state: UiState) {
    state.id = id;
    #[cfg(test)]
    TAP.lock().unwrap().push((id, state.stage, serde_json::to_value(&state).unwrap()));
    let current = ACTIVE.lock().unwrap().as_ref().map(|a| (a.id, a.owner.clone()));
    match current {
        Some((active, owner)) if active == id && owner.as_ref().is_none_or(|o| o.is_live()) => {
            crate::traits::emit_event("transfer_state", &state);
        }
        _ => {}
    }
}

fn finish(id: u64) {
    let mut guard = ACTIVE.lock().unwrap();
    if guard.as_ref().is_some_and(|a| a.id == id) {
        guard.take();
    }
}

fn explain(failure: &Failure) -> String {
    match failure {
        Failure::WrongCode => "The codes didn't match. Check the code and try again.".into(),
        Failure::Contention => "Another device tried to use this code, so the transfer stopped. Start again with a new code.".into(),
        Failure::RoleClash(Role::Sender) => "Both devices are signed in. On the new device, choose Sign in with another device on the sign in screen.".into(),
        Failure::RoleClash(Role::Receiver) => "Neither device is signed in. On your signed-in device, open Settings and choose Sign in on another device.".into(),
        Failure::Version => "The other device is running a different version of Vector. Update both and try again.".into(),
        Failure::Denied => "The transfer was declined on your other device.".into(),
        Failure::BadBundle => "The account didn't arrive intact, so nothing was saved. Try again.".into(),
        Failure::WrongNumber => "The number was entered wrong too many times. Start again.".into(),
        Failure::Protocol(_) => "Something went wrong between the devices. Try again.".into(),
        Failure::PeerAborted(why) => match why.as_str() {
            "code" => "The codes didn't match. Check the code and try again.".into(),
            "contention" => "Another device tried to use this code, so the transfer stopped. Start again with a new code.".into(),
            "role" => "Both devices are on the same side of the transfer. One must be signed in and the other new.".into(),
            "number" => "The number was entered wrong too many times on your other device. Start again.".into(),
            "timeout" => "Your other device stopped waiting. Start again.".into(),
            _ => "The transfer was cancelled on your other device.".into(),
        },
    }
}

/// Signs relay AUTH as the throwaway key, never the account's.
#[derive(Debug)]
struct ThrowawayAuth(Keys);

impl nostr_sdk::prelude::Authenticator for ThrowawayAuth {
    fn make_auth_event<'a>(
        &'a self,
        relay_url: &'a RelayUrl,
        challenge: &'a str,
    ) -> crate::signer::BoxedFuture<'a, std::result::Result<Event, nostr_sdk::prelude::Error>> {
        Box::pin(async move {
            Ok(ClientAuthentication::new(challenge, relay_url.clone()).finalize_async(&self.0).await?)
        })
    }
}

struct Wire {
    client: Client,
    keys: Keys,
    room: PublicKey,
    /// Since when no publish has reached a relay. Reconnects get their full budget before this counts.
    failing_since: Option<Instant>,
}

impl Wire {
    /// Publish `msg`, signed fresh each time: relays refuse a repeat of an old event.
    async fn publish(&mut self, msg: &Msg) {
        let Ok(event) = EventBuilder::new(Kind::Custom(KIND), msg.encode()).tag(Tag::public_key(self.room)).finalize(&self.keys) else {
            return;
        };
        match crate::transport_aware(self.client.send_event(&event)).await {
            Ok(out) if !out.success.is_empty() => self.failing_since = None,
            _ => {
                self.failing_since.get_or_insert_with(Instant::now);
            }
        }
    }
}

struct Driver {
    id: u64,
    role: Role,
    session: Session,
    wire: Wire,
    deadline: Instant,
    backoff: usize,
    retry_at: Option<Instant>,
    ended: bool,
    announced: Option<PublicKey>,
}

impl Driver {
    fn emit(&self, state: UiState) {
        emit(self.id, state);
    }

    fn fail(&mut self, error: impl Into<String>) {
        self.emit(UiState { stage: "error", error: Some(error.into()), ..Default::default() });
        self.ended = true;
    }

    async fn dispatch(&mut self, outs: Vec<Out>) {
        let mut linger = false;
        for out in outs {
            match out {
                Out::Send(msg) => {
                    self.wire.publish(&msg).await;
                    self.backoff = 0;
                    self.retry_at = None;
                }
                Out::Resend => {
                    if let Some(msg) = self.session.last_sent().cloned() {
                        self.wire.publish(&msg).await;
                    }
                }
                Out::Matched { sas, sender } => {
                    self.deadline = Instant::now() + APPROVAL_WINDOW;
                    self.announced = sender;
                    match self.role {
                        Role::Receiver => {
                            let mut state = self.who();
                            state.stage = "match";
                            state.sas = Some(sas);
                            self.emit(state);
                        }
                        Role::Sender => self.emit(UiState { stage: "approve", ..Default::default() }),
                    }
                }
                Out::Received(bundle) => {
                    let kept = {
                        let active = ACTIVE.lock().unwrap();
                        let ours = active.as_ref().is_some_and(|a| a.id == self.id);
                        if ours {
                            *RECEIVED.lock().unwrap() = Some((self.id, bundle, Instant::now()));
                        }
                        ours
                    };
                    if !kept {
                        // Cancelled while it arrived: no ack, so the sender doesn't report success.
                        let outs = self.session.abort("cancel");
                        Box::pin(self.dispatch(outs)).await;
                        self.ended = true;
                        return;
                    }
                    let id = self.id;
                    // spawn-detached: wipes a process-wide slot after its TTL, no account state.
                    crate::rt::spawn(async move {
                        crate::rt::time::sleep(RECEIVED_TTL).await;
                        let mut held = RECEIVED.lock().unwrap();
                        if held.as_ref().is_some_and(|(r, _, _)| *r == id) {
                            held.take();
                        }
                    });
                    let mut state = self.who();
                    state.stage = "received";
                    self.emit(state);
                    linger = true;
                }
                Out::Acked => {
                    self.emit(UiState { stage: "sent", ..Default::default() });
                    self.ended = true;
                }
                Out::Failed(failure) => self.fail(explain(&failure)),
            }
        }
        // Counted from after the ack's publish, which can take a while on Tor.
        if linger {
            self.deadline = Instant::now() + crate::relay_request_timeout(LINGER);
        }
    }

    /// The sender as it announced itself: npub, name and re-encoded avatar.
    fn who(&self) -> UiState {
        let (name, avatar) = self.session.sender_face();
        UiState {
            sender: self.announced.and_then(|pk| pk.to_bech32().ok()),
            name: name.map(str::to_string),
            avatar: avatar.and_then(super::avatar::sanitize),
            ..Default::default()
        }
    }

    async fn command(&mut self, cmd: Option<Command>, owner: &Option<Arc<crate::db::Session>>) {
        match cmd {
            Some(Command::CheckNumber(typed, reply)) => {
                let result = self.session.check_number(&typed);
                let over = self.session.is_over();
                let _ = reply.send(result);
                if over {
                    self.wire.publish(&Msg::Abort("number".to_string())).await;
                    self.fail(explain(&Failure::WrongNumber));
                }
            }
            Some(Command::Approve(bundle, reply)) => {
                if owner.as_ref().is_some_and(|o| !o.is_live()) {
                    let _ = reply.send(Err(ApproveError::NotReady));
                    let outs = self.session.abort("cancel");
                    self.dispatch(outs).await;
                    self.ended = true;
                    return;
                }
                match self.session.approve(bundle) {
                    Ok(outs) => {
                        let _ = reply.send(Ok(()));
                        self.emit(UiState { stage: "sending", ..Default::default() });
                        self.dispatch(outs).await;
                        // Counted from after the publish, which on Tor can take most of a minute itself.
                        self.deadline = Instant::now() + crate::relay_request_timeout(Duration::from_secs(30));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Some(Command::Deny) => {
                let outs = self.session.deny();
                self.dispatch(outs).await;
                // Nothing answers a denial, so it goes out twice rather than leave the receiver waiting.
                if let Some(msg) = self.session.last_sent().cloned() {
                    crate::rt::time::sleep(Duration::from_millis(1500)).await;
                    self.wire.publish(&msg).await;
                }
                self.ended = true;
            }
            Some(Command::Cancel) | None => {
                let outs = self.session.abort("cancel");
                self.dispatch(outs).await;
                self.ended = true;
            }
        }
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        if self.retry_at.is_some_and(|r| r <= now) {
            self.retry_at = None;
            if self.session.awaiting_reply() {
                if let Some(msg) = self.session.last_sent().cloned() {
                    self.wire.publish(&msg).await;
                }
                self.backoff += 1;
            }
        }
        let given_up = crate::relay_connect_timeout(Duration::from_secs(10)) + RECONNECT_EVERY * 2;
        if self.wire.failing_since.is_some_and(|since| since.elapsed() > given_up) {
            return self.fail("Lost the connection to the relays. Check your connection and that the device's clock is right.");
        }
        if now < self.deadline {
            return;
        }
        let lingering = RECEIVED.lock().unwrap().as_ref().is_some_and(|(r, _, _)| *r == self.id) || self.session.is_over();
        match self.role {
            // The identity is held (or already used); the linger for a repeated bundle is over.
            Role::Receiver if lingering => {}
            // Sent, but no ack came back. It most likely arrived anyway.
            Role::Sender if self.session.is_sent() => {
                self.emit(UiState { stage: "sent", unconfirmed: true, ..Default::default() });
            }
            _ => {
                let matched = self.session.is_matched();
                let outs = self.session.abort("timeout");
                self.dispatch(outs).await;
                return self.fail(if matched {
                    "Nothing happened for a while, so the transfer stopped. Start again when you're ready."
                } else {
                    "The code expired. Start again for a new one."
                });
            }
        }
        self.ended = true;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    id: u64,
    role: Role,
    presentation: Presentation,
    code: Code,
    identity: Option<PublicKey>,
    face: (Option<String>, Option<Vec<u8>>),
    mut cmds: mpsc::UnboundedReceiver<Command>,
    ready: oneshot::Sender<Result<(), String>>,
    owner: Option<Arc<crate::db::Session>>,
) {
    let keys = Keys::generate();
    let client = crate::apply_transport(
        ClientBuilder::new().authenticator(ThrowawayAuth(keys.clone())),
        crate::transport::Lane::Shared,
    )
    .build();
    crate::transport::cycle::track(&client);
    for url in crate::state::TRUSTED_RELAYS {
        let _ = crate::ClientRelayExt::add_managed_relay(&client, *url).await;
    }
    client.connect().and_wait(crate::relay_connect_timeout(Duration::from_secs(10))).await;

    let (mut session, first) = Session::new(role, presentation, &code, keys.public_key(), identity);
    session.set_face(face.0, face.1);
    drop(code);
    let room = session.room();
    let began = Timestamp::now();
    let mut notes = client.notifications();
    let sub = client
        .subscribe(Filter::new().kind(Kind::Custom(KIND)).pubkey(room).since(began - STALE_SECS))
        .await
        .ok()
        .map(|o| o.value);

    // Only a relay that has finished replaying what it kept is trusted to be live: those stored
    // events are other, older sessions.
    let mut live: HashSet<RelayUrl> = HashSet::new();
    if let Some(sub) = &sub {
        let deadline = Instant::now() + crate::relay_request_timeout(Duration::from_secs(8));
        while live.is_empty() {
            let next = tokio::select! {
                n = notes.next() => n,
                _ = sleep_until(deadline) => None,
            };
            let Some(note) = next else { break };
            if let ClientNotification::Message { relay_url, message } = note {
                if matches!(&*message, RelayMessage::EndOfStoredEvents(s) if **s == *sub) {
                    live.insert(relay_url);
                }
            }
        }
    }
    if live.is_empty() {
        let _ = ready.send(Err("Couldn't reach the relays. Check your connection and try again.".to_string()));
        finish(id);
        client.shutdown().await;
        return;
    }
    let _ = ready.send(Ok(()));

    let mut d = Driver {
        id,
        role,
        session,
        wire: Wire { client: client.clone(), keys, room, failing_since: None },
        deadline: Instant::now() + CODE_WINDOW,
        backoff: 0,
        retry_at: None,
        ended: false,
        announced: None,
    };
    d.dispatch(first).await;
    let mut seen: HashSet<EventId> = HashSet::new();
    let mut reconnect_at = Instant::now() + RECONNECT_EVERY;

    while !d.ended {
        if d.session.awaiting_reply() && d.retry_at.is_none() {
            d.retry_at = Some(Instant::now() + Duration::from_secs(BACKOFF_SECS[d.backoff.min(BACKOFF_SECS.len() - 1)]));
        }
        let wake = d.retry_at.map_or(d.deadline, |r| r.min(d.deadline));
        tokio::select! {
            // A message already waiting beats a deadline that expired while it was in flight.
            biased;
            note = notes.next() => {
                let Some(note) = note else { break };
                match note {
                    ClientNotification::Message { relay_url, message } => {
                        if let (RelayMessage::EndOfStoredEvents(s), Some(sub)) = (&*message, &sub) {
                            if **s == *sub {
                                live.insert(relay_url);
                            }
                        }
                    }
                    ClientNotification::Event { relay_url, event, .. } => {
                        let fresh = event.created_at.as_secs() + STALE_SECS >= began.as_secs();
                        let ours = event.kind == Kind::Custom(KIND) && event.tags.public_keys().any(|p| p == room);
                        // A relay replaying what it kept after a reconnect still counts for events
                        // from this session's own lifetime; older ones belong to someone else.
                        let replay_ok = event.created_at >= began;
                        if (live.contains(&relay_url) || replay_ok) && fresh && ours && seen.insert(event.id) {
                            let outs = d.session.on_message(event.pubkey, Msg::decode(&event.content));
                            d.dispatch(outs).await;
                        }
                    }
                    _ => {}
                }
            }
            cmd = cmds.recv() => d.command(cmd, &owner).await,
            _ = sleep_until(reconnect_at) => {
                reconnect_at = Instant::now() + RECONNECT_EVERY;
                for (url, relay) in client.relays().await {
                    if matches!(relay.status(), RelayStatus::Disconnected | RelayStatus::Terminated) {
                        // Live again only once it has replayed what it kept and sent a fresh EOSE.
                        live.remove(&url);
                        let client = client.clone();
                        // spawn-detached: redials a relay of this transfer's own temporary client.
                        crate::rt::spawn(async move {
                            if relay.try_connect().timeout(crate::relay_connect_timeout(Duration::from_secs(10))).await.is_ok() {
                                crate::resubscribe_relay_after_reconnect(&client, &url).await;
                            }
                        });
                    }
                }
            }
            _ = sleep_until(wake) => d.tick().await,
        }
    }

    finish(id);
    client.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for(id: u64, stage: &str) -> Option<serde_json::Value> {
        for _ in 0..600 {
            if let Some((_, _, state)) = TAP.lock().unwrap().iter().find(|(i, s, _)| *i == id && *s == stage) {
                return Some(state.clone());
            }
            if let Some((_, _, state)) = TAP.lock().unwrap().iter().find(|(i, s, _)| *i == id && *s == "error") {
                panic!("{id} failed: {state}");
            }
            crate::rt::time::sleep(Duration::from_millis(100)).await;
        }
        None
    }

    /// Both sides over the real trusted relays, one process. `cargo test -p vector-core
    /// transfer::service -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn an_identity_crosses_the_live_relays() {
        let identity = Keys::generate();
        let code = Code::generate();
        let (a, b) = (next_id(), next_id());
        let (cmd_a, cmds_a) = mpsc::unbounded_channel();
        let (_cmd_b, cmds_b) = mpsc::unbounded_channel();
        let (ready_a, ready_rx_a) = oneshot::channel();
        let (ready_b, ready_rx_b) = oneshot::channel();
        let face = image::RgbaImage::from_fn(400, 300, |x, y| image::Rgba([x as u8, y as u8, 90, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(face).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let thumb = super::super::avatar::thumbnail(&png).unwrap();
        tokio::spawn(run(a, Role::Sender, Presentation::Shower, code.clone(), Some(identity.public_key()), (Some("Tester".into()), Some(thumb)), cmds_a, ready_a, None));
        ready_rx_a.await.unwrap().unwrap();
        // The receiver is the active transfer, as `start` makes it: an identity only lands for that one.
        *ACTIVE.lock().unwrap() = Some(Active { id: b, role: Role::Receiver, cmd: _cmd_b.clone(), owner: None });
        tokio::spawn(run(b, Role::Receiver, Presentation::Joiner, code, None, (None, None), cmds_b, ready_b, None));
        ready_rx_b.await.unwrap().unwrap();

        let matched = wait_for(b, "match").await.expect("receiver matched");
        assert_eq!(matched["name"], "Tester");
        assert!(matched["avatar"].as_str().is_some_and(|a| a.starts_with("data:image/png;base64,")), "avatar re-encoded as png");
        let sas = matched["sas"].as_str().unwrap().to_string();
        wait_for(a, "approve").await.expect("sender asked to approve");
        let (tx, rx) = oneshot::channel();
        cmd_a.send(Command::CheckNumber(sas, tx)).unwrap();
        rx.await.unwrap().unwrap();
        let (tx, rx) = oneshot::channel();
        let nsec = identity.secret_key().to_bech32().unwrap();
        cmd_a.send(Command::Approve(Bundle { nsec: nsec.clone(), seed: None }, tx)).unwrap();
        rx.await.unwrap().unwrap();

        wait_for(b, "received").await.expect("receiver got it");
        wait_for(a, "sent").await.expect("sender heard the ack");
        assert_eq!(RECEIVED.lock().unwrap().as_ref().map(|(i, bundle, _)| (*i, bundle.nsec.clone())), Some((b, nsec.clone())));
        assert_eq!(received().map(|b| b.nsec.clone()).as_deref(), Some(nsec.as_str()), "held until signed in");
        assert!(received().is_some(), "reading it doesn't use it up");
        forget_received();
        assert!(received().is_none());
    }
}
