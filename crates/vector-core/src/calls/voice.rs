//! A voice Channel's call (CORD-07 §8). Each join binds a fresh iroh endpoint, unlinkable across
//! calls and relayed unless the account chose Direct, names it in presence every 30 s, and keeps a
//! full mesh in which the lower EndpointId dials. Only an endpoint that fresh presence binds to a
//! member who may connect is dialled or accepted, so the QUIC handshake says whose each stream is.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, TransportAddr};
use nostr_sdk::prelude::{PublicKey, ToBech32};
use serde::Serialize;

use crate::community::roles::Permissions;
use crate::community::transport::LiveTransport;
use crate::community::v2::{community::CommunityV2, voice as presence};
use crate::community::{ChannelId, CommunityId};

pub const VOICE_ALPN: &[u8] = b"vector/voice/1";
/// Others in a call besides us: a full mesh carries voice to about fifteen (CORD-07 §8).
pub const MAX_PEERS: usize = 14;
const DIAL_EVERY: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// What a client's media engine does with a call's peers: one capture sent to every connection,
/// and every connection's audio mixed for playout.
pub trait VoiceMedia: Send + Sync {
    fn add_peer(&self, endpoint: [u8; 32], conn: Connection);
    fn remove_peer(&self, endpoint: &[u8; 32]);
    fn set_muted(&self, on: bool);
    fn set_deafened(&self, on: bool);
    /// Whether a peer's audio plays: a member without SPEAK, or timed out, isn't heard.
    fn set_audible(&self, _endpoint: &[u8; 32], _on: bool) {}
    /// False when the call went ahead without a microphone: muted, whatever the switch says.
    fn has_mic(&self) -> bool {
        true
    }
    /// How loud each peer is now, 0 to 1, and our own microphone under `None`.
    fn levels(&self) -> Vec<(Option<[u8; 32]>, f32)> {
        Vec::new()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct VoicePeer {
    pub npub: String,
    pub connected: bool,
    /// The selected path is direct, not through a relay.
    pub direct: bool,
    /// How loud they are now, 0 to 1.
    pub level: f32,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct VoiceState {
    pub community_id: String,
    pub channel_id: String,
    pub peers: Vec<VoicePeer>,
    pub muted: bool,
    pub deafened: bool,
    /// False when no media engine runs here: in the call, but neither heard nor hearing.
    pub audio: bool,
    /// How loud our microphone is now, 0 to 1.
    pub mic_level: f32,
    /// False when there's no microphone to hear: refused access, or none there.
    pub mic: bool,
}

struct Link {
    author: PublicKey,
    conn: Connection,
}

struct Voice {
    owner: u64,
    community: CommunityId,
    channel: ChannelId,
    channel_hex: String,
    endpoint: Endpoint,
    me: PublicKey,
    links: Mutex<HashMap<[u8; 32], Link>>,
    dialing: Mutex<HashSet<[u8; 32]>>,
    media: Option<Box<dyn VoiceMedia>>,
    muted: AtomicBool,
    deafened: AtomicBool,
    nudge: tokio::sync::Notify,
    tasks: Mutex<Vec<crate::rt::JoinHandle<()>>>,
}

static VOICE: Mutex<Option<Arc<Voice>>> = Mutex::new(None);

fn live() -> Option<Arc<Voice>> {
    let v = VOICE.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
    (v.owner == crate::db::current_session().id()).then_some(v)
}

fn now_ms() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The lower EndpointId dials, so a pair never opens two connections.
pub fn dials(me: &[u8; 32], them: &[u8; 32]) -> bool {
    me < them
}

/// A relay a peer may be dialled through: iroh's defaults, or one the Community lists.
pub fn allowed_relay(url: &RelayUrl, community: &CommunityV2) -> bool {
    let host = |u: &RelayUrl| u.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase());
    let theirs = host(url);
    theirs.is_some()
        && iroh::defaults::prod::default_relay_map()
            .urls::<Vec<RelayUrl>>()
            .into_iter()
            .chain(voice_relays(community))
            .any(|d| host(&d) == theirs)
}

/// Whether `member` may be in this Channel's call now: not banned, and the Channel resolves
/// View and Connect for them (CORD-07 §1: Connect is holding its key).
pub fn may_connect(community: &CommunityV2, channel: &ChannelId, member: &PublicKey) -> bool {
    let cid = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let chan = crate::simd::hex::bytes_to_hex_32(&channel.0);
    let hex = member.to_hex();
    if crate::db::community::get_community_banlist(&cid).unwrap_or_default().contains(&hex) {
        return false;
    }
    let Some(ch) = community.channel(channel) else { return false };
    let owner = community.owner().ok().map(|o| o.to_hex());
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    if roster.everyone.is_none() {
        return !ch.private || roster.is_entitled(owner.as_deref(), &hex, &chan, &[], &[]);
    }
    roster.channel_permissions(owner.as_deref(), &hex, &chan, ch.private).contains(Permissions::VIEW_CHANNEL | Permissions::CONNECT)
}

/// Whether `member` is heard: Speak, and not under a Timeout.
pub fn may_speak(community: &CommunityV2, channel: &ChannelId, member: &PublicKey) -> bool {
    let cid = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let chan = crate::simd::hex::bytes_to_hex_32(&channel.0);
    let hex = member.to_hex();
    let owner = community.owner().ok().map(|o| o.to_hex());
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    if roster.everyone.is_none() {
        return true;
    }
    let timed_out = crate::community::v2::service::community_timeouts(community).is_ok_and(|t| t.contains_key(member));
    !timed_out && roster.channel_permissions(owner.as_deref(), &hex, &chan, community.channel(channel).is_some_and(|c| c.private)).contains(Permissions::SPEAK)
}

fn voice_relays(community: &CommunityV2) -> Vec<RelayUrl> {
    community.voice_relays().iter().filter_map(|u| u.parse().ok()).collect()
}

async fn bind(community: &CommunityV2) -> Result<Endpoint, String> {
    struct Voice(RelayMode);
    impl iroh::endpoint::presets::Preset for Voice {
        fn apply(self, builder: iroh::endpoint::Builder) -> iroh::endpoint::Builder {
            builder.relay_mode(self.0).crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        }
    }
    let relays = voice_relays(community);
    let mode = if relays.is_empty() { RelayMode::Default } else { RelayMode::Custom(relays.into_iter().collect()) };
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .keep_alive_interval(Duration::from_secs(5))
        .max_idle_timeout(Some(Duration::from_secs(30).try_into().map_err(|e| format!("{e}"))?))
        .send_observed_address_reports(false)
        .receive_observed_address_reports(false)
        .build();
    let builder = Endpoint::builder(Voice(mode)).alpns(vec![VOICE_ALPN.to_vec()]).transport_config(transport);
    #[cfg(not(target_arch = "wasm32"))]
    let builder = if crate::transport::realtime::direct() { builder } else { builder.clear_ip_transports() };
    let endpoint = builder.bind().await.map_err(|e| format!("Voice unavailable: {e}"))?;
    let _ = crate::rt::time::timeout(Duration::from_secs(8), endpoint.online()).await;
    Ok(endpoint)
}

fn relay_of(endpoint: &Endpoint) -> Option<String> {
    endpoint.addr().addrs.iter().find_map(|a| match a {
        TransportAddr::Relay(u) => Some(u.to_string()),
        _ => None,
    })
}

fn held(community: &CommunityId) -> Result<CommunityV2, String> {
    crate::db::community::load_community_v2(community)?.ok_or_else(|| "Community not found".to_string())
}

async fn announce(v: &Voice, joined: bool) -> Result<(), String> {
    let community = held(&v.community)?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    let endpoint = joined.then(|| (*v.endpoint.id().as_bytes(), relay_of(&v.endpoint)));
    let muted = v.muted.load(Ordering::SeqCst) || v.media.as_ref().is_none_or(|m| !m.has_mic());
    crate::community::v2::service::send_voice_presence(&transport, &community, &v.channel, endpoint, muted, v.deafened.load(Ordering::SeqCst)).await
}

/// Join a voice Channel's call, leaving any other first.
pub async fn join(community_id: &str, channel_id: &str) -> Result<VoiceState, String> {
    crate::transport::realtime::check()?;
    if super::session::snapshot().is_some() {
        return Err("End your call first".into());
    }
    leave().await;
    let me = crate::state::my_public_key().ok_or("Not signed in")?;
    let cid = CommunityId(crate::community::id32_from_hex(community_id).ok_or("bad community id")?);
    let channel = ChannelId(crate::community::id32_from_hex(channel_id).ok_or("bad channel id")?);
    let community = held(&cid)?;
    let ch = community.channel(&channel).ok_or("No such channel")?;
    if ch.view() != crate::community::v2::community::ChannelView::Voice {
        return Err("That isn't a voice channel".into());
    }
    if ch.private && ch.key.is_none() {
        return Err("This channel's key hasn't arrived yet".into());
    }
    if !may_connect(&community, &channel, &me) {
        return Err("You can't connect to this channel".into());
    }
    let endpoint = bind(&community).await?;
    let media = match super::platform::get() {
        Ok(p) => match p.start_voice().await {
            Ok(m) => Some(m),
            Err(e) => {
                crate::log_warn!("[voice] in the call without audio: {e}");
                None
            }
        },
        Err(_) => None,
    };
    let v = Arc::new(Voice {
        owner: crate::db::current_session().id(),
        community: cid,
        channel,
        channel_hex: crate::simd::hex::bytes_to_hex_32(&channel.0),
        endpoint,
        me,
        links: Mutex::new(HashMap::new()),
        dialing: Mutex::new(HashSet::new()),
        media,
        muted: AtomicBool::new(false),
        deafened: AtomicBool::new(false),
        nudge: tokio::sync::Notify::new(),
        tasks: Mutex::new(Vec::new()),
    });
    if let Err(e) = announce(&v, true).await {
        v.endpoint.close().await;
        return Err(e);
    }
    *VOICE.lock().unwrap_or_else(|e| e.into_inner()) = Some(v.clone());
    let tasks = vec![
        crate::db::spawn_bound(levels(v.clone())),
        crate::db::spawn_bound(heartbeat(v.clone())),
        crate::db::spawn_bound(dialer(v.clone())),
        crate::db::spawn_bound(acceptor(v.clone())),
    ];
    *v.tasks.lock().unwrap_or_else(|e| e.into_inner()) = tasks;
    changed(&v);
    Ok(state(&v))
}

/// Leave the call, if in one: a `left` is best-effort, staleness heals a missed one.
pub async fn leave() {
    let Some(v) = VOICE.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    for t in v.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        t.abort();
    }
    for (id, link) in v.links.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        link.conn.close(0u32.into(), b"left");
        if let Some(m) = &v.media {
            m.remove_peer(&id);
        }
    }
    let _ = announce(&v, false).await;
    v.endpoint.close().await;
    crate::emit_event("voice_state", &serde_json::Value::Null);
}

/// Someone's presence changed in a Channel: re-check the mesh if it is ours.
pub fn nudge(channel_hex: &str) {
    if let Some(v) = live().filter(|v| v.channel_hex == channel_hex) {
        v.nudge.notify_one();
    }
}

pub fn snapshot() -> Option<VoiceState> {
    live().map(|v| state(&v))
}

pub fn set_muted(on: bool) -> Result<(), String> {
    let v = live().ok_or("Not in a call")?;
    v.muted.store(on, Ordering::SeqCst);
    if let Some(m) = &v.media {
        m.set_muted(on);
    }
    reannounce(&v);
    changed(&v);
    Ok(())
}

pub fn set_deafened(on: bool) -> Result<(), String> {
    let v = live().ok_or("Not in a call")?;
    v.deafened.store(on, Ordering::SeqCst);
    if let Some(m) = &v.media {
        m.set_deafened(on);
    }
    reannounce(&v);
    changed(&v);
    Ok(())
}

/// Everyone sees a mute or deafen now, not at the next heartbeat.
fn reannounce(v: &Arc<Voice>) {
    let v = v.clone();
    crate::db::spawn_bound(async move {
        if let Err(e) = announce(&v, true).await {
            crate::log_warn!("[voice] presence update failed: {e}");
        }
    });
}

fn state(v: &Voice) -> VoiceState {
    let levels = v.media.as_ref().map(|m| m.levels()).unwrap_or_default();
    let level_of = |ep: Option<[u8; 32]>| levels.iter().find(|(e, _)| *e == ep).map_or(0.0, |(_, l)| *l);
    let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
    let mut peers: Vec<VoicePeer> = presence::present(&v.channel_hex, now_ms())
        .into_iter()
        .filter(|p| p.author != v.me)
        .map(|p| {
            let link = p.endpoint.and_then(|ep| links.get(&ep));
            VoicePeer {
                npub: p.author.to_bech32().unwrap_or_default(),
                connected: link.is_some(),
                direct: link.is_some_and(|l| l.conn.paths().iter().any(|path| path.is_selected() && path.is_ip())),
                level: if link.is_some() { level_of(p.endpoint) } else { 0.0 },
                muted: p.muted,
                deafened: p.deafened,
            }
        })
        .collect();
    peers.sort_by(|a, b| a.npub.cmp(&b.npub));
    VoiceState {
        community_id: crate::simd::hex::bytes_to_hex_32(&v.community.0),
        channel_id: v.channel_hex.clone(),
        peers,
        muted: v.muted.load(Ordering::SeqCst),
        deafened: v.deafened.load(Ordering::SeqCst),
        audio: v.media.is_some(),
        mic_level: level_of(None),
        mic: v.media.as_ref().is_some_and(|m| m.has_mic()),
    }
}

fn changed(v: &Voice) {
    crate::emit_event("voice_state", &state(v));
}

/// Who is speaking, five times a second while there is audio to show it.
async fn levels(v: Arc<Voice>) {
    if v.media.is_none() {
        return;
    }
    loop {
        crate::rt::time::sleep(Duration::from_millis(200)).await;
        crate::emit_event("voice_levels", &state(&v));
    }
}

async fn heartbeat(v: Arc<Voice>) {
    loop {
        crate::rt::time::sleep(Duration::from_millis(presence::HEARTBEAT_MS)).await;
        if let Err(e) = announce(&v, true).await {
            crate::log_warn!("[voice] presence heartbeat failed: {e}");
        }
    }
}

/// Keep the mesh what presence says: drop who left or may no longer connect, dial who we should.
async fn dialer(v: Arc<Voice>) {
    let my_id = *v.endpoint.id().as_bytes();
    loop {
        let Ok(community) = held(&v.community) else { return };
        let present = presence::present(&v.channel_hex, now_ms());
        let gone: Vec<[u8; 32]> = {
            let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
            links
                .iter()
                .filter(|(id, l)| !present.iter().any(|p| p.endpoint.as_ref() == Some(*id) && p.author == l.author) || !may_connect(&community, &v.channel, &l.author))
                .map(|(id, _)| *id)
                .collect()
        };
        for id in gone {
            unlink(&v, &id, b"not in this call");
        }
        if let Some(m) = &v.media {
            let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
            for (id, l) in links.iter() {
                m.set_audible(id, may_speak(&community, &v.channel, &l.author));
            }
        }
        for p in present.iter().filter(|p| p.author != v.me) {
            let (Some(ep), Some(relay)) = (p.endpoint, p.relay.as_deref().and_then(|r| r.parse::<RelayUrl>().ok())) else { continue };
            let busy = v.links.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&ep) || v.dialing.lock().unwrap_or_else(|e| e.into_inner()).contains(&ep);
            let full = v.links.lock().unwrap_or_else(|e| e.into_inner()).len() >= MAX_PEERS;
            if busy || full || !dials(&my_id, &ep) || !allowed_relay(&relay, &community) || !may_connect(&community, &v.channel, &p.author) {
                continue;
            }
            let Ok(id) = iroh::EndpointId::from_bytes(&ep) else { continue };
            v.dialing.lock().unwrap_or_else(|e| e.into_inner()).insert(ep);
            let (v2, author) = (v.clone(), p.author);
            crate::db::spawn_bound(async move {
                let addr = EndpointAddr { id, addrs: [TransportAddr::Relay(relay)].into_iter().collect() };
                match crate::rt::time::timeout(CONNECT_TIMEOUT, v2.endpoint.connect(addr, VOICE_ALPN)).await {
                    Ok(Ok(conn)) => link(&v2, ep, author, conn),
                    Ok(Err(e)) => crate::log_debug!("[voice] dial failed: {e}"),
                    Err(_) => crate::log_debug!("[voice] dial timed out"),
                }
                v2.dialing.lock().unwrap_or_else(|e| e.into_inner()).remove(&ep);
            });
        }
        changed(&v);
        tokio::select! {
            _ = crate::rt::time::sleep(DIAL_EVERY) => {}
            _ = v.nudge.notified() => {}
        }
    }
}

async fn acceptor(v: Arc<Voice>) {
    let my_id = *v.endpoint.id().as_bytes();
    while let Some(incoming) = v.endpoint.accept().await {
        let v = v.clone();
        crate::db::spawn_bound(async move {
            let Ok(conn) = incoming.await else { return };
            let id = *conn.remote_id().as_bytes();
            let author = presence::author_of(&v.channel_hex, &id, now_ms()).filter(|a| *a != v.me);
            let allowed = author.is_some_and(|a| held(&v.community).is_ok_and(|c| may_connect(&c, &v.channel, &a)));
            match author {
                Some(a) if allowed && dials(&id, &my_id) => link(&v, id, a, conn),
                _ => conn.close(0u32.into(), b"not in this call"),
            }
        });
    }
}

fn link(v: &Arc<Voice>, id: [u8; 32], author: PublicKey, conn: Connection) {
    {
        let mut links = v.links.lock().unwrap_or_else(|e| e.into_inner());
        if links.contains_key(&id) || links.len() >= MAX_PEERS {
            conn.close(0u32.into(), b"already connected");
            return;
        }
        links.insert(id, Link { author, conn: conn.clone() });
    }
    if let Some(m) = &v.media {
        m.add_peer(id, conn.clone());
        if let Ok(c) = held(&v.community) {
            m.set_audible(&id, may_speak(&c, &v.channel, &author));
        }
    }
    let watch = v.clone();
    crate::db::spawn_bound(async move {
        conn.closed().await;
        let same = watch.links.lock().unwrap_or_else(|e| e.into_inner()).get(&id).is_some_and(|l| l.conn.stable_id() == conn.stable_id());
        if same {
            unlink(&watch, &id, b"closed");
        }
    });
    changed(v);
}

fn unlink(v: &Voice, id: &[u8; 32], why: &[u8]) {
    // Out of the lock first: `changed` reads the links again.
    let removed = v.links.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    if let Some(l) = removed {
        l.conn.close(0u32.into(), why);
        if let Some(m) = &v.media {
            m.remove_peer(id);
        }
        changed(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_peer_dropped_from_the_call_never_blocks_on_its_own_state() {
        struct Offline;
        impl iroh::endpoint::presets::Preset for Offline {
            fn apply(self, b: iroh::endpoint::Builder) -> iroh::endpoint::Builder {
                b.relay_mode(RelayMode::Disabled).crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            }
        }
        let bind = || async { Endpoint::builder(Offline).alpns(vec![VOICE_ALPN.to_vec()]).bind().await.unwrap() };
        let (a, b) = (bind().await, bind().await);
        let server = b.clone();
        let accepted = tokio::spawn(async move { server.accept().await.unwrap().await.unwrap() });
        let conn = a.connect(b.addr(), VOICE_ALPN).await.unwrap();
        let _held = accepted.await.unwrap();
        let v = Arc::new(Voice {
            owner: 0,
            community: CommunityId([0; 32]),
            channel: ChannelId([1; 32]),
            channel_hex: crate::simd::hex::bytes_to_hex_32(&[1; 32]),
            endpoint: a.clone(),
            me: nostr_sdk::prelude::Keys::generate().public_key(),
            links: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashSet::new()),
            media: None,
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            nudge: tokio::sync::Notify::new(),
            tasks: Mutex::new(Vec::new()),
        });
        let id = *b.id().as_bytes();
        link(&v, id, nostr_sdk::prelude::Keys::generate().public_key(), conn);
        assert_eq!(v.links.lock().unwrap().len(), 1);
        let (gone, (tx, rx)) = (v.clone(), std::sync::mpsc::channel());
        std::thread::spawn(move || {
            unlink(&gone, &id, b"left");
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok(), "unlinking repaints the state, which reads the links: it must not hold them");
        assert!(v.links.lock().unwrap().is_empty());
    }

    #[test]
    fn of_a_pair_exactly_one_dials() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert!(dials(&a, &b) ^ dials(&b, &a));
        assert!(!dials(&a, &a), "never itself");
    }
}
