//! A voice Channel's call (CORD-07 §8). Each join binds a fresh iroh endpoint, unlinkable across
//! calls and relayed unless the account chose Direct, names it in presence every 30 s, and keeps a
//! full mesh in which the lower EndpointId dials. Only an endpoint that fresh presence binds to a
//! member who may connect is dialled or accepted, so the QUIC handshake says whose each stream is.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, VarInt};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, TransportAddr};
use nostr_sdk::prelude::{PublicKey, ToBech32};
use serde::Serialize;

use super::platform::AwakeLevel;
use crate::community::roles::Permissions;
use crate::community::transport::LiveTransport;
use crate::community::v2::{community::CommunityV2, voice as presence};
use crate::community::{ChannelId, CommunityId};

mod video;

pub const VOICE_ALPN: &[u8] = b"concord/voice/1";
/// Others in a call besides us: a full mesh carries voice to about fifteen (CORD-07 §8).
pub const MAX_PEERS: usize = 14;
const DIAL_EVERY: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long an incoming connection waits for the presence that names it: whoever dials us
/// saw our presence first, and theirs can still be crossing the relays to us.
const PRESENCE_GRACE: Duration = Duration::from_secs(15);
/// The spread of a room's answers to a newcomer.
const GREET_JITTER_MS: u64 = 1500;
const JOIN_TRIES: u32 = 3;

/// A cue the call plays: who came and went, and our own mute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chime {
    Joined,
    Left,
    PeerJoined,
    PeerLeft,
    Muted,
    Unmuted,
}

/// What a client's media engine does with a call's peers: one capture sent to every connection,
/// and every connection's audio mixed for playout.
pub trait VoiceMedia: Send + Sync {
    fn chime(&self, _cue: Chime) {}
    fn add_peer(&self, endpoint: [u8; 32], conn: Connection);
    fn remove_peer(&self, endpoint: &[u8; 32]);
    fn set_muted(&self, on: bool);
    fn set_deafened(&self, on: bool);
    /// Whether a peer's audio plays: a member without SPEAK, or timed out, isn't heard.
    fn set_audible(&self, _endpoint: &[u8; 32], _on: bool) {}
    /// How loud a peer plays here, 1.0 as sent, up to 2.0.
    fn set_volume(&self, _endpoint: &[u8; 32], _gain: f32) {}
    /// A peer with Priority Speaker: while they talk, everyone else plays quieter.
    fn set_priority(&self, _endpoint: &[u8; 32], _on: bool) {}
    /// Let go of the devices now, though other holders of the engine may outlive this call.
    fn stop(&self) {}
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
    /// Their key, hex: what their screen's frames are tagged with on the video link.
    pub pubkey: String,
    /// How loud they play here, as this account set it.
    pub volume: f32,
    pub connected: bool,
    /// The selected path is direct, not through a relay.
    pub direct: bool,
    /// The selected path's round trip, 0 until there is one.
    pub rtt_ms: u32,
    /// How loud they are now, 0 to 1.
    pub level: f32,
    pub muted: bool,
    pub deafened: bool,
    /// A moderator muted or deafened them for this call.
    pub server_muted: bool,
    pub server_deafened: bool,
    /// They are sharing their screen, or their camera.
    pub screen: bool,
    pub camera: bool,
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
    /// A moderator muted or deafened us for this call.
    pub server_muted: bool,
    pub server_deafened: bool,
    /// We are sharing our screen, and to how many; our camera is on.
    pub sharing: bool,
    pub viewers: u32,
    pub camera: bool,
    /// We may send a camera or a screen in this Channel.
    pub can_video: bool,
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
    /// Swapped whole when the devices change.
    media: Mutex<Option<Arc<dyn VoiceMedia>>>,
    muted: AtomicBool,
    deafened: AtomicBool,
    /// Our app is paused by its system (a locked phone): shown muted until it's back.
    away: AtomicBool,
    nudge: tokio::sync::Notify,
    /// An answer to a newcomer is pending; more arrivals ride it.
    greeting: AtomicBool,
    /// Whether we hold Video here, as the mesh upkeep last judged it.
    can_video: AtomicBool,
    video: video::Video,
    /// The machine stays awake for the call, and the display too while video shows.
    awake: Mutex<Option<(AwakeLevel, Box<dyn std::any::Any + Send>)>>,
    /// The sound routed as a phone call's, out loud.
    call_audio: Mutex<Option<Box<dyn std::any::Any + Send>>>,
    /// The channel's and community's names, and when we joined, for a phone's call notification.
    shown: (String, String, u64),
    tasks: Mutex<Vec<crate::rt::JoinHandle<()>>>,
}

static VOICE: Mutex<Option<Arc<Voice>>> = Mutex::new(None);

impl Voice {
    fn engine(&self) -> Option<Arc<dyn VoiceMedia>> {
        self.media.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

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

/// Whether `member` is heard: Speak, not under a Timeout, and not muted or deafened by a moderator.
/// The original roles carry no voice bits, but a Timeout and a moderator's mute hold there too.
pub fn may_speak(community: &CommunityV2, channel: &ChannelId, member: &PublicKey) -> bool {
    let cid = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let chan = crate::simd::hex::bytes_to_hex_32(&channel.0);
    let hex = member.to_hex();
    let timed_out = crate::community::v2::service::community_timeouts(community).is_ok_and(|t| t.contains_key(member));
    let (server_muted, server_deafened) = presence::server_state(&chan, member);
    if timed_out || server_muted || server_deafened {
        return false;
    }
    let owner = community.owner().ok().map(|o| o.to_hex());
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    roster.everyone.is_none() || roster.channel_permissions(owner.as_deref(), &hex, &chan, community.channel(channel).is_some_and(|c| c.private)).contains(Permissions::SPEAK)
}

/// Whether `member` holds `bit` in this Channel; on the original roles, nobody holds the voice bits.
fn has_bit(community: &CommunityV2, channel: &ChannelId, member: &PublicKey, bit: u64) -> bool {
    let cid = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let chan = crate::simd::hex::bytes_to_hex_32(&channel.0);
    let owner = community.owner().ok().map(|o| o.to_hex());
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    roster.everyone.is_some() && roster.channel_permissions(owner.as_deref(), &member.to_hex(), &chan, community.channel(channel).is_some_and(|c| c.private)).contains(bit)
}

/// Whether `member`'s camera or screen is shown: Video, and not under a Timeout (CORD-07 §7).
pub fn may_video(community: &CommunityV2, channel: &ChannelId, member: &PublicKey) -> bool {
    if crate::community::v2::service::community_timeouts(community).is_ok_and(|t| t.contains_key(member)) {
        return false;
    }
    let cid = crate::simd::hex::bytes_to_hex_32(&community.id().0);
    let chan = crate::simd::hex::bytes_to_hex_32(&channel.0);
    let owner = community.owner().ok().map(|o| o.to_hex());
    let roster = crate::db::community::get_community_roles(&cid).unwrap_or_default();
    roster.everyone.is_none() || roster.channel_permissions(owner.as_deref(), &member.to_hex(), &chan, community.channel(channel).is_some_and(|c| c.private)).contains(Permissions::VIDEO)
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
    #[cfg(not(target_arch = "wasm32"))]
    let mode = if relays.is_empty() { RelayMode::Default } else { RelayMode::Custom(relays.into_iter().collect()) };
    #[cfg(target_arch = "wasm32")]
    let mode = {
        let urls = if relays.is_empty() { iroh::defaults::prod::default_relay_map().urls::<Vec<RelayUrl>>() } else { relays };
        RelayMode::Custom(urls.iter().map(browser_relay).collect())
    };
    let transport = iroh::endpoint::QuicTransportConfig::builder()
        .keep_alive_interval(Duration::from_secs(5))
        .max_idle_timeout(Some(Duration::from_secs(30).try_into().map_err(|e| format!("{e}"))?))
        .send_observed_address_reports(false)
        .receive_observed_address_reports(false)
        .stream_receive_window(VarInt::from_u32(512 * 1024))
        .receive_window(VarInt::from_u32(2 * 1024 * 1024))
        .max_concurrent_uni_streams(VarInt::from_u32(256))
        .build();
    let builder = Endpoint::builder(Voice(mode)).alpns(vec![VOICE_ALPN.to_vec()]).transport_config(transport);
    #[cfg(not(target_arch = "wasm32"))]
    let builder = if crate::transport::realtime::direct() { builder } else { builder.clear_ip_transports() };
    let endpoint = builder.bind().await.map_err(|e| format!("Voice unavailable: {e}"))?;
    let _ = crate::rt::time::timeout(Duration::from_secs(8), endpoint.online()).await;
    Ok(endpoint)
}

/// A relay as a browser can dial it: WebKit refuses a WebSocket to a fully qualified host, and
/// iroh spells its relays with the trailing dot. `xdc::wire::browser_relay` is the mesh's twin.
#[cfg(target_arch = "wasm32")]
fn browser_relay(url: &RelayUrl) -> RelayUrl {
    let mut u = (**url).clone();
    if let Some(host) = u.host_str().and_then(|h| h.strip_suffix('.')).map(str::to_string) {
        let _ = u.set_host(Some(&host));
    }
    RelayUrl::from(u)
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
    crate::community::v2::service::send_voice_presence(&transport, &community, &v.channel, endpoint, showing(v)).await
}

/// What our presence shows of us now.
fn showing(v: &Voice) -> presence::Showing {
    let sending = v.video.sending();
    presence::Showing {
        muted: v.muted.load(Ordering::SeqCst) || v.away.load(Ordering::SeqCst) || v.engine().is_none_or(|m| !m.has_mic()),
        deafened: v.deafened.load(Ordering::SeqCst),
        screen: sending.screen,
        camera: sending.camera,
    }
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
    crate::log_info!("[voice] joined via {}", relay_of(&endpoint).as_deref().unwrap_or("no relay yet"));
    let call_audio = super::platform::get().ok().and_then(|p| p.hold_call_audio(true));
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
        media: Mutex::new(media.map(Arc::from)),
        muted: AtomicBool::new(false),
        deafened: AtomicBool::new(false),
        away: AtomicBool::new(false),
        nudge: tokio::sync::Notify::new(),
        greeting: AtomicBool::new(false),
        can_video: AtomicBool::new(may_video(&community, &channel, &me)),
        video: video::Video::default(),
        awake: Mutex::new(None),
        call_audio: Mutex::new(call_audio),
        shown: (ch.name.clone(), community.name.clone(), now_ms()),
        tasks: Mutex::new(Vec::new()),
    });
    // Just after launch the relays may still be connecting: a refused first presence is retried.
    let mut tries = 0;
    while let Err(e) = announce(&v, true).await {
        tries += 1;
        if tries >= JOIN_TRIES {
            v.endpoint.close().await;
            return Err(e);
        }
        crate::rt::time::sleep(Duration::from_millis(1500)).await;
    }
    // A join that raced this one installed its call meanwhile: that call goes, not just its slot.
    let raced = VOICE.lock().unwrap_or_else(|e| e.into_inner()).replace(v.clone());
    if let Some(old) = raced {
        crate::db::spawn_bound(shutdown(old));
    }
    if let Some(m) = v.engine() {
        apply_to(&v, &*m);
        m.chime(Chime::Joined);
    }
    let mut tasks = vec![
        crate::db::spawn_bound(levels(v.clone())),
        crate::db::spawn_bound(heartbeat(v.clone())),
        crate::db::spawn_bound(dialer(v.clone())),
        crate::db::spawn_bound(acceptor(v.clone())),
    ];
    tasks.extend(video::start(&v));
    *v.tasks.lock().unwrap_or_else(|e| e.into_inner()) = tasks;
    changed(&v);
    Ok(state(&v))
}

/// Leave the call, if in one: a `left` is best-effort, staleness heals a missed one.
pub async fn leave() {
    let Some(v) = VOICE.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if let Some(ringer) = super::session::ringer() {
        ringer.voice(None);
    }
    shutdown(v).await;
    crate::emit_event("voice_state", &serde_json::Value::Null);
}

/// Take a call down: its tasks, its links, its devices, a `left`, its endpoint.
async fn shutdown(v: Arc<Voice>) {
    if let Some(m) = v.engine() {
        m.chime(Chime::Left);
    }
    for t in v.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        t.abort();
    }
    video::stop(&v);
    for (id, link) in v.links.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        link.conn.close(0u32.into(), b"left");
        if let Some(m) = v.engine() {
            m.remove_peer(&id);
        }
    }
    // The microphone closes now, not when the last task holding the session lets go.
    let engine = v.media.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(engine) = engine {
        engine.stop();
    }
    v.call_audio.lock().unwrap_or_else(|e| e.into_inner()).take();
    let _ = announce(&v, false).await;
    v.endpoint.close().await;
    v.awake.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// Start or stop sending our camera or screen to whoever watches (CORD-07 §6). Video is judged
/// by every receiver; we refuse here too, so a member without it isn't left sending to no one.
pub fn set_video(kind: crate::calls::wire::VideoKind, on: bool) -> Result<(), String> {
    let v = live().ok_or("Not in a call")?;
    if on && !may_video(&held(&v.community)?, &v.channel, &v.me) {
        return Err("You can't share video in this channel".into());
    }
    video::set_sending(&v, kind, on)?;
    reannounce(&v);
    changed(&v);
    Ok(())
}

/// Which of a member's pictures we show in our call; neither stops them coming.
pub fn watch(member: &PublicKey, want: crate::calls::wire::Tracks) -> Result<(), String> {
    let v = live().ok_or("Not in a call")?;
    video::watch(&v, member, want)?;
    refresh_awake(&v);
    Ok(())
}

/// A moderator's directive, already judged: one aimed at us is obeyed, and anyone else's mute
/// changes who is heard.
pub fn on_directive(channel_hex: &str, target: &PublicKey, directive: presence::Directive, to: Option<[u8; 32]>) {
    use presence::Directive;
    let Some(v) = live().filter(|v| v.channel_hex == channel_hex) else { return };
    if *target != v.me {
        v.nudge.notify_one();
        changed(&v);
        return;
    }
    match directive {
        Directive::Mute | Directive::Unmute | Directive::Deafen | Directive::Undeafen => {
            if let Some(m) = v.engine() {
                apply_to(&v, &*m);
                match directive {
                    Directive::Mute | Directive::Deafen => m.chime(Chime::Muted),
                    _ if !effective(&v).0 => m.chime(Chime::Unmuted),
                    _ => {}
                }
            }
        }
        Directive::Disconnect => {
            crate::db::spawn_bound(async { leave().await });
            return;
        }
        Directive::Move => {
            let Some(to) = to else { return };
            let community = crate::simd::hex::bytes_to_hex_32(&v.community.0);
            let to = crate::simd::hex::bytes_to_hex_32(&to);
            crate::db::spawn_bound(async move {
                if let Err(e) = join(&community, &to).await {
                    crate::log_warn!("[voice] couldn't follow a move: {e}");
                    leave().await;
                }
            });
            return;
        }
    }
    changed(&v);
}

/// Someone is newly reachable in our call: answer with our presence now rather than at our next
/// heartbeat, since whichever of us has the lower id can't dial until it has our endpoint.
pub fn greet(channel_hex: &str, author: &PublicKey) {
    let Some(v) = live().filter(|v| v.channel_hex == channel_hex && v.me != *author) else { return };
    if !crate::rt::can_spawn() || v.greeting.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::db::spawn_bound(async move {
        crate::rt::time::sleep(Duration::from_millis(rand::random::<u64>() % GREET_JITTER_MS)).await;
        v.greeting.store(false, Ordering::SeqCst);
        if !live().is_some_and(|now| Arc::ptr_eq(&now, &v)) {
            return;
        }
        if let Err(e) = announce(&v, true).await {
            crate::log_warn!("[voice] answering a newcomer failed: {e}");
        }
    });
}

/// Someone's presence changed in a Channel: re-check the mesh if it is ours.
pub fn nudge(channel_hex: &str) {
    if let Some(v) = live().filter(|v| v.channel_hex == channel_hex) {
        v.nudge.notify_one();
    }
}

/// A member in a voice Channel's call, as every client lists them.
#[derive(Clone, Debug, Serialize)]
pub struct InCall {
    pub npub: String,
    /// Their own mute or deafen, or a moderator's.
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    pub screen: bool,
    pub camera: bool,
    /// In the call on another transport: out of reach of ours.
    pub elsewhere: bool,
}

/// Who is in a voice Channel's call now, whether or not we are, earliest first.
pub fn in_call(channel_hex: &str) -> Vec<InCall> {
    presence::present(channel_hex, now_ms())
        .into_iter()
        .filter_map(|p| {
            let elsewhere = p.transport.as_deref() != Some(crate::community::v2::chat::TRANSPORT_IROH);
            Some(InCall {
                npub: p.author.to_bech32().ok()?,
                muted: p.muted || p.server_muted,
                deafened: p.deafened || p.server_deafened,
                server_muted: p.server_muted,
                server_deafened: p.server_deafened,
                screen: p.screen,
                camera: p.camera,
                elsewhere,
            })
        })
        .collect()
}

/// What this account may do to a member in a voice call.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Powers {
    pub mute: bool,
    pub deafen: bool,
    #[serde(rename = "move")]
    pub move_: bool,
}

/// Our moderation powers over `target` in `community`: never over ourselves or the owner.
pub fn powers(community_id: &str, target: &PublicKey) -> Powers {
    let Some(me) = crate::state::my_public_key() else { return Powers::default() };
    let Some(id) = crate::simd::hex::hex_to_bytes_32_checked(community_id) else { return Powers::default() };
    let Ok(Some(community)) = crate::db::community::load_community_v2(&CommunityId(id)) else { return Powers::default() };
    let owner = community.owner().ok().map(|o| o.to_hex());
    let roles = crate::db::community::get_community_roles(community_id).unwrap_or_default();
    let (me, them) = (me.to_hex(), target.to_hex());
    let can = |bit: u64| me != them && owner.as_deref() != Some(them.as_str()) && (owner.as_deref() == Some(me.as_str()) || roles.can_act_on_member(&me, owner.as_deref(), &them, bit));
    Powers { mute: can(Permissions::MUTE_MEMBERS), deafen: can(Permissions::DEAFEN_MEMBERS), move_: can(Permissions::MOVE_MEMBERS) }
}

/// Mute, deafen, move or disconnect `target` in a voice Channel's call, as a moderator
/// (CORD-07 §7). `to` names the Channel a move sends them to.
pub async fn moderate(community_id: &str, channel_id: &str, target: &PublicKey, action: &str, to: Option<&str>) -> Result<(), String> {
    let directive = presence::Directive::parse(action).ok_or("unknown action")?;
    let id = CommunityId(crate::simd::hex::hex_to_bytes_32_checked(community_id).ok_or("bad community id")?);
    let channel = ChannelId(crate::simd::hex::hex_to_bytes_32_checked(channel_id).ok_or("bad channel id")?);
    let to = to.map(|t| crate::simd::hex::hex_to_bytes_32_checked(t).ok_or("bad channel id")).transpose()?;
    let community = held(&id)?;
    let transport = LiveTransport::with_timeout(Duration::from_secs(12));
    crate::community::v2::service::send_voice_directive(&transport, &community, &channel, target, directive, to).await
}

pub fn snapshot() -> Option<VoiceState> {
    live().map(|v| state(&v))
}

pub fn set_muted(on: bool) -> Result<(), String> {
    let v = live().ok_or("Not in a call")?;
    let (server_muted, server_deafened) = presence::server_state(&v.channel_hex, &v.me);
    if !on && server_muted {
        return Err("A moderator muted you in this call".into());
    }
    if !on && server_deafened {
        return Err("A moderator deafened you in this call".into());
    }
    v.muted.store(on, Ordering::SeqCst);
    if let Some(m) = v.engine() {
        apply_to(&v, &*m);
        m.chime(if on { Chime::Muted } else { Chime::Unmuted });
    }
    reannounce(&v);
    changed(&v);
    Ok(())
}

pub fn set_deafened(on: bool) -> Result<(), String> {
    let v = live().ok_or("Not in a call")?;
    if !on && presence::server_state(&v.channel_hex, &v.me).1 {
        return Err("A moderator deafened you in this call".into());
    }
    v.deafened.store(on, Ordering::SeqCst);
    if let Some(m) = v.engine() {
        apply_to(&v, &*m);
        m.chime(if on { Chime::Muted } else { Chime::Unmuted });
    }
    reannounce(&v);
    changed(&v);
    Ok(())
}

/// Our app is paused by its system or back: shown muted meanwhile, since nothing we say is
/// sent; back, we announce at once and dial, as a long pause aged our presence out.
pub fn set_away(on: bool) {
    let Some(v) = live() else { return };
    if v.away.swap(on, Ordering::SeqCst) == on && on {
        return;
    }
    reannounce(&v);
    if !on {
        v.nudge.notify_one();
    }
    changed(&v);
}

const VOLUMES_KEY: &str = "voice_volumes";

struct Volumes;

/// Read once per account, then kept: the state repaints five times a second.
fn volumes() -> HashMap<String, f32> {
    let cache = crate::db::current_session().scoped::<Volumes, Mutex<Option<HashMap<String, f32>>>>();
    let mut held = cache.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(|| crate::db::get_sql_setting(VOLUMES_KEY.to_string()).ok().flatten().and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default()).clone()
}

fn store_volumes(all: HashMap<String, f32>) -> Result<(), String> {
    crate::db::set_sql_setting(VOLUMES_KEY.to_string(), serde_json::to_string(&all).map_err(|e| e.to_string())?)?;
    *crate::db::current_session().scoped::<Volumes, Mutex<Option<HashMap<String, f32>>>>().lock().unwrap_or_else(|e| e.into_inner()) = Some(all);
    Ok(())
}

/// How loud `member` plays in this account's calls: 1.0 unless set.
pub fn peer_volume(member: &PublicKey) -> f32 {
    volumes().get(&member.to_hex()).copied().unwrap_or(1.0)
}

/// Remember how loud `member` plays, 0 to 2, and apply it to the call in progress.
pub fn set_peer_volume(member: &PublicKey, gain: f32) -> Result<(), String> {
    let gain = if gain.is_finite() { gain.clamp(0.0, 2.0) } else { 1.0 };
    let mut all = volumes();
    if (gain - 1.0).abs() < 0.01 {
        all.remove(&member.to_hex());
    } else {
        all.insert(member.to_hex(), gain);
    }
    store_volumes(all)?;
    if let Some(v) = live() {
        if let Some(m) = v.engine() {
            let ids: Vec<[u8; 32]> = v.links.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, l)| l.author == *member).map(|(id, _)| *id).collect();
            for id in ids {
                m.set_volume(&id, gain);
            }
        }
        changed(&v);
    }
    Ok(())
}

/// Everyone sees a mute or deafen now, not at the next heartbeat.
fn reannounce(v: &Arc<Voice>) {
    // Ours applies the moment it's set, so the list shows it before any relay has the announcement.
    let at = now_ms();
    let heard = presence::Heard::announcing(Some((*v.endpoint.id().as_bytes(), relay_of(&v.endpoint))), showing(v));
    presence::note(&v.channel_hex, v.me, at, heard, at);
    let v = v.clone();
    crate::db::spawn_bound(async move {
        // A call left meanwhile must not announce itself joined after its `left`.
        if !live().is_some_and(|now| Arc::ptr_eq(&now, &v)) {
            return;
        }
        if let Err(e) = announce(&v, true).await {
            crate::log_warn!("[voice] presence update failed: {e}");
        }
    });
}

fn state(v: &Voice) -> VoiceState {
    let levels = v.engine().map(|m| m.levels()).unwrap_or_default();
    let level_of = |ep: Option<[u8; 32]>| levels.iter().find(|(e, _)| *e == ep).map_or(0.0, |(_, l)| *l);
    // Read before the links are held: the video side takes its locks one at a time too.
    let (sending, viewers) = (v.video.sending(), v.video.viewers());
    let links = v.links.lock().unwrap_or_else(|e| e.into_inner());
    let mut peers: Vec<VoicePeer> = presence::present(&v.channel_hex, now_ms())
        .into_iter()
        .filter(|p| p.author != v.me)
        .map(|p| {
            let link = p.endpoint.and_then(|ep| links.get(&ep));
            let path = link.and_then(|l| {
                let paths = l.conn.paths();
                paths.iter().find(|path| path.is_selected()).map(|path| (path.is_ip(), path.rtt().as_millis() as u32))
            });
            VoicePeer {
                volume: peer_volume(&p.author),
                npub: p.author.to_bech32().unwrap_or_default(),
                pubkey: p.author.to_hex(),
                screen: p.screen,
                camera: p.camera,
                connected: link.is_some(),
                direct: path.is_some_and(|(ip, _)| ip),
                rtt_ms: path.map_or(0, |(_, rtt)| rtt),
                level: if link.is_some() { level_of(p.endpoint) } else { 0.0 },
                muted: p.muted,
                deafened: p.deafened,
                server_muted: p.server_muted,
                server_deafened: p.server_deafened,
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
        audio: v.engine().is_some(),
        mic_level: level_of(None),
        mic: v.engine().is_some_and(|m| m.has_mic()),
        server_muted: presence::server_state(&v.channel_hex, &v.me).0,
        server_deafened: presence::server_state(&v.channel_hex, &v.me).1,
        sharing: sending.screen,
        viewers,
        camera: sending.camera,
        can_video: v.can_video.load(Ordering::SeqCst),
    }
}

/// What the engine does, (mute, deafen): our own choices, or a moderator's, whichever is
/// stricter. Deafened also means muted, as it does everywhere voice chat is.
fn effective(v: &Voice) -> (bool, bool) {
    stricter((v.muted.load(Ordering::SeqCst), v.deafened.load(Ordering::SeqCst)), presence::server_state(&v.channel_hex, &v.me))
}

fn stricter((muted, deafened): (bool, bool), (server_muted, server_deafened): (bool, bool)) -> (bool, bool) {
    let deafened = deafened || server_deafened;
    (muted || server_muted || deafened, deafened)
}

fn apply_to(v: &Voice, m: &dyn VoiceMedia) {
    let (muted, deafened) = effective(v);
    m.set_muted(muted);
    m.set_deafened(deafened);
}

/// Repaint, if this is still the live call: one taken down meanwhile paints nothing.
fn changed(v: &Voice) {
    let state = state(v);
    if live().is_some_and(|now| std::ptr::eq(Arc::as_ptr(&now), v)) {
        refresh_awake(v);
        crate::emit_event("voice_state", &state);
        if let Some(ringer) = super::session::ringer() {
            let (channel, community, since_ms) = &v.shown;
            ringer.voice(Some(&super::session::VoiceShown { channel_id: &v.channel_hex, channel, community, muted: effective(v).0, since_ms: *since_ms }));
        }
    }
}

/// The hold the call needs now; the new one is taken before the old goes, so the machine is
/// covered in between.
fn refresh_awake(v: &Voice) {
    let level = if v.video.any() { AwakeLevel::Display } else { AwakeLevel::System };
    let mut slot = v.awake.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|(held, _)| *held == level) {
        return;
    }
    let Some(hold) = super::platform::get().ok().and_then(|p| p.hold_awake(level)) else { return };
    *slot = Some((level, hold));
}

/// Who is speaking, five times a second while there is audio to show it.
async fn levels(v: Arc<Voice>) {
    loop {
        crate::rt::time::sleep(Duration::from_millis(200)).await;
        if v.engine().is_some() {
            crate::emit_event("voice_levels", &state(&v));
        }
    }
}

/// The microphone or speaker changed: a fresh engine on the new devices, carrying every peer,
/// their volume and whether they're heard, and our mute and deafen.
pub async fn restart_audio() {
    let Some(v) = live() else { return };
    let Ok(platform) = super::platform::get() else { return };
    // The old engine lets go of the devices before the new one opens them, though a task may
    // still hold it for a moment.
    let old = v.media.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(old) = old {
        old.stop();
    }
    let fresh: Arc<dyn VoiceMedia> = match platform.start_voice().await {
        Ok(m) => Arc::from(m),
        Err(e) => {
            crate::log_warn!("[voice] audio didn't restart: {e}");
            changed(&v);
            return;
        }
    };
    apply_to(&v, &*fresh);
    // Installed before the peers are added: a link landing meanwhile adds itself to it too.
    *v.media.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&fresh));
    let community = held(&v.community).ok();
    let links: Vec<([u8; 32], PublicKey, Connection)> = v.links.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(id, l)| (*id, l.author, l.conn.clone())).collect();
    for (id, author, conn) in links {
        fresh.add_peer(id, conn);
        fresh.set_volume(&id, peer_volume(&author));
        if let Some(c) = &community {
            fresh.set_audible(&id, may_speak(c, &v.channel, &author));
            fresh.set_priority(&id, has_bit(c, &v.channel, &author, Permissions::PRIORITY_SPEAKER));
        }
    }
    changed(&v);
}

/// Presence every heartbeat, and at once when our home relay changes: peers dial us through it.
async fn heartbeat(v: Arc<Voice>) {
    use iroh::Watcher;
    let mut addr = v.endpoint.watch_addr();
    let mut relay = relay_of(&v.endpoint);
    loop {
        let due = web_time::Instant::now() + Duration::from_millis(presence::HEARTBEAT_MS);
        loop {
            let left = due.saturating_duration_since(web_time::Instant::now());
            tokio::select! {
                _ = crate::rt::time::sleep(left) => break,
                moved = addr.updated() => {
                    if moved.is_err() {
                        return;
                    }
                    if relay_of(&v.endpoint) != relay {
                        break;
                    }
                }
            }
        }
        relay = relay_of(&v.endpoint);
        if let Err(e) = announce(&v, true).await {
            crate::log_warn!("[voice] presence heartbeat failed: {e}");
        }
    }
}

/// Keep the mesh what presence says: drop who left or may no longer connect, dial who we should.
async fn dialer(v: Arc<Voice>) {
    let my_id = *v.endpoint.id().as_bytes();
    loop {
        // Gone ends the upkeep; a failed read only skips this pass.
        let community = match crate::db::community::load_community_v2(&v.community) {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                crate::log_warn!("[voice] mesh upkeep skipped a pass: {e}");
                crate::rt::time::sleep(DIAL_EVERY).await;
                continue;
            }
        };
        let present = presence::present(&v.channel_hex, now_ms());
        let linked: Vec<([u8; 32], PublicKey)> = v.links.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(id, l)| (*id, l.author)).collect();
        let gone: Vec<[u8; 32]> = linked
            .into_iter()
            .filter(|(id, author)| !present.iter().any(|p| p.endpoint.as_ref() == Some(id) && p.author == *author) || !may_connect(&community, &v.channel, author))
            .map(|(id, _)| id)
            .collect();
        for id in gone {
            unlink(&v, &id, b"not in this call");
        }
        if let Some(m) = v.engine() {
            // Judged outside the lock: every frame we send waits on it.
            let linked: Vec<([u8; 32], PublicKey)> = v.links.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(id, l)| (*id, l.author)).collect();
            for (id, author) in linked {
                m.set_audible(&id, may_speak(&community, &v.channel, &author));
                m.set_priority(&id, has_bit(&community, &v.channel, &author, Permissions::PRIORITY_SPEAKER));
            }
        }
        video::revalidate(&v, |author| may_video(&community, &v.channel, author));
        // Video lost while sending: stop, as every receiver already has.
        let can_video = may_video(&community, &v.channel, &v.me);
        if v.can_video.swap(can_video, Ordering::SeqCst) != can_video {
            changed(&v);
        }
        let sending = v.video.sending();
        if sending.any() && !can_video {
            for kind in [crate::calls::wire::VideoKind::Camera, crate::calls::wire::VideoKind::Screen] {
                let _ = video::set_sending(&v, kind, false);
            }
            reannounce(&v);
            changed(&v);
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
            #[cfg(target_arch = "wasm32")]
            let relay = browser_relay(&relay);
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
            if !dials(&id, &my_id) {
                conn.close(0u32.into(), b"not in this call");
                return;
            }
            let named = || presence::author_of(&v.channel_hex, &id, now_ms()).filter(|a| *a != v.me);
            let waited = crate::rt::time::timeout(PRESENCE_GRACE, async {
                loop {
                    if let Some(a) = named() {
                        return a;
                    }
                    crate::rt::time::sleep(Duration::from_millis(250)).await;
                }
            });
            let author = tokio::select! {
                a = waited => a.ok(),
                _ = conn.closed() => return,
            };
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
    video::on_link(v, id, author, conn.clone());
    if let Some(m) = v.engine() {
        m.add_peer(id, conn.clone());
        m.set_volume(&id, peer_volume(&author));
        m.chime(Chime::PeerJoined);
        if let Ok(c) = held(&v.community) {
            m.set_audible(&id, may_speak(&c, &v.channel, &author));
            m.set_priority(&id, has_bit(&c, &v.channel, &author, Permissions::PRIORITY_SPEAKER));
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
    video::on_unlink(v, id);
    if let Some(l) = removed {
        l.conn.close(0u32.into(), why);
        if let Some(m) = v.engine() {
            m.remove_peer(id);
            m.chime(Chime::PeerLeft);
        }
        changed(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moderator_lifting_their_mute_hands_back_our_own_choice() {
        assert_eq!(stricter((false, false), (true, false)), (true, false));
        assert_eq!(stricter((false, false), (false, false)), (false, false), "unmuted again once lifted");
        assert_eq!(stricter((true, false), (false, false)), (true, false), "our own mute outlives theirs");
        assert_eq!(stricter((false, true), (false, false)), (true, true), "deafened is muted too");
        assert_eq!(stricter((false, false), (false, true)), (true, true));
    }

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
            media: Mutex::new(None),
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            away: AtomicBool::new(false),
            nudge: tokio::sync::Notify::new(),
            greeting: AtomicBool::new(false),
            can_video: AtomicBool::new(false),
            video: video::Video::default(),
            awake: Mutex::new(None),
            call_audio: Mutex::new(None),
            shown: Default::default(),
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_state_and_the_screen_fan_out_never_wait_on_each_other() {
        let _db = crate::db::DB_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
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
            channel: ChannelId([2; 32]),
            channel_hex: crate::simd::hex::bytes_to_hex_32(&[2; 32]),
            endpoint: a.clone(),
            me: nostr_sdk::prelude::Keys::generate().public_key(),
            links: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashSet::new()),
            media: Mutex::new(None),
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            away: AtomicBool::new(false),
            nudge: tokio::sync::Notify::new(),
            greeting: AtomicBool::new(false),
            can_video: AtomicBool::new(false),
            video: video::Video::default(),
            awake: Mutex::new(None),
            call_audio: Mutex::new(None),
            shown: Default::default(),
            tasks: Mutex::new(Vec::new()),
        });
        let id = *b.id().as_bytes();
        v.links.lock().unwrap().insert(id, Link { author: nostr_sdk::prelude::Keys::generate().public_key(), conn });
        v.video.add_watcher(id);
        v.video.set_sending_for_test(crate::calls::wire::Tracks { camera: true, screen: true });
        let run = |f: Box<dyn Fn() + Send>| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    f();
                }
                let _ = tx.send(());
            });
            rx
        };
        let (s, f) = (v.clone(), v.clone());
        let painted = run(Box::new(move || drop(state(&s))));
        let fanned = run(Box::new(move || drop(video::targets(&f, crate::calls::wire::VideoKind::Screen))));
        let done = painted.recv_timeout(Duration::from_secs(20)).is_ok() && fanned.recv_timeout(Duration::from_secs(20)).is_ok();
        assert!(done, "the state read and a frame's fan-out each take the links and the viewers: never both at once");
    }

    #[test]
    fn of_a_pair_exactly_one_dials() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert!(dials(&a, &b) ^ dials(&b, &a));
        assert!(!dials(&a, &a), "never itself");
    }
}
