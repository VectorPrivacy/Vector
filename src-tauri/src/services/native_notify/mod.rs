//! Rich OS notifications, one backend per platform behind [`Backend`].
//!
//! Everything a platform doesn't care about lives here: what a notification can do
//! ([`Action`]), how its target survives a round trip through the OS ([`Activation`]
//! encode/decode), what clicking, replying or marking read does ([`handle`]), and
//! avatars converted for platforms that render only PNG ([`avatar_png`]).
//!
//! A backend's whole job is to draw a [`NotificationData`] with its account attached,
//! retract a chat's notifications, and hand every click or button press back to
//! [`handle`] as an [`Activation`]. A platform with no backend (or one whose `init`
//! failed) keeps tauri-plugin-notification's plain title and body.
//!
//! Adding a platform: implement [`Backend`] in `<platform>.rs`, then point `Platform`
//! at it under that platform's `cfg`.

// The helpers for backends go unused on a platform until it has one.
#![cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use nostr_sdk::prelude::ToBech32;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::notification_service::NotificationData;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
type Platform = windows::Windows;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
type Platform = macos::MacOS;

#[cfg(not(any(windows, target_os = "macos")))]
type Platform = Unsupported;

/// Which delivered notifications a retraction removes, judged by their [`Activation`].
pub type Matcher = Box<dyn Fn(&Activation) -> bool + Send>;

/// One platform's notification system.
pub trait Backend {
    /// Register with the OS. Called once from setup, on the main thread.
    fn init(app: &AppHandle) -> Result<(), String>;
    /// Show `data` for `account` (an npub; empty if none is live). `Err` falls back to the plugin.
    fn show(data: &NotificationData, account: &str) -> Result<(), String>;
    /// Remove every delivered notification, on screen or in the notification centre, that
    /// `matches`. A notification from before a restart counts where the platform keeps
    /// them; a backend whose platform keeps no history tracks what it showed itself.
    fn retract(matches: Matcher);
}

#[cfg(not(any(windows, target_os = "macos")))]
pub struct Unsupported;

#[cfg(not(any(windows, target_os = "macos")))]
impl Backend for Unsupported {
    fn init(_: &AppHandle) -> Result<(), String> {
        Err("no native notification backend on this platform".into())
    }
    fn show(_: &NotificationData, _: &str) -> Result<(), String> {
        Err("no native notification backend".into())
    }
    fn retract(_: Matcher) {}
}

static READY: OnceLock<()> = OnceLock::new();
static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();
/// A reply sent from a notification while no account was logged in: sent once its account is.
static PENDING_REPLY: Mutex<Option<Activation>> = Mutex::new(None);

pub fn init(app: &AppHandle) {
    if let Ok(dir) = app.path().app_data_dir() {
        let dir = dir.join("notifications");
        if std::fs::create_dir_all(dir.join("avatars")).is_ok() {
            let _ = CACHE_DIR.set(dir);
        }
    }
    match Platform::init(app) {
        Ok(()) => {
            let _ = READY.set(());
        }
        Err(e) => log_warn!("[Notify] Native notifications unavailable, using the plugin: {e}"),
    }
}

/// `Err` when there is no native backend, so the caller shows the plain notification instead.
pub fn show(data: &NotificationData) -> Result<(), String> {
    if READY.get().is_none() {
        return Err("native notifications not initialised".into());
    }
    Platform::show(data, &live_account().unwrap_or_default())
}

fn retract(matches: impl Fn(&Activation) -> bool + Send + 'static) {
    if READY.get().is_some() {
        Platform::retract(Box::new(matches));
    }
}

/// A chat was read, here or on another device.
pub fn remove_chat(chat_id: &str) {
    let chat = chat_id.to_string();
    retract(move |a| a.chat == chat);
}

/// A message was deleted, or expired.
pub fn remove_message(message_id: &str) {
    let msg = message_id.to_string();
    retract(move |a| a.message.as_deref() == Some(msg.as_str()));
}

/// The sender was blocked.
pub fn remove_sender(npub: &str) {
    let sender = npub.to_string();
    retract(move |a| a.sender.as_deref() == Some(sender.as_str()));
}

/// The account is being switched away from, logged out or deleted: its messages
/// don't stay on screen for whoever uses the machine next.
pub fn remove_account(npub: &str) {
    let account = npub.to_string();
    retract(move |a| a.account == account);
}

/// Directory a backend may keep its own files in (icons, converted images).
pub fn cache_dir() -> Option<&'static Path> {
    CACHE_DIR.get().map(PathBuf::as_path)
}

/// The mutes a notification offers for its chat, in `set_notify_mute`'s units (a negative
/// duration is indefinite). A subset of the app's own mute menu, small enough for any
/// platform's notification menu.
pub const MUTE_CHOICES: [(&str, i64); 3] = [
    ("Mute for 1 hour", 60 * 60 * 1000),
    ("Mute for 8 hours", 8 * 60 * 60 * 1000),
    ("Mute until I turn it back on", -1),
];

// ---------------------------------------------------------------------------
// Activation: what a click or a button asks for, and doing it
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Bring Vector forward on the chat, at the message when there is one.
    Open,
    /// Send the typed text to the chat.
    Reply,
    MarkRead,
    /// Mute the chat for [`Activation::mute_ms`].
    Mute,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Open => "open",
            Action::Reply => "reply",
            Action::MarkRead => "read",
            Action::Mute => "mute",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "reply" => Action::Reply,
            "read" => Action::MarkRead,
            "mute" => Action::Mute,
            _ => Action::Open,
        }
    }
}

/// A notification's target as it travels through the OS and back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub action: Action,
    /// The npub the notification was shown for.
    pub account: String,
    pub chat: String,
    pub message: Option<String>,
    /// Who sent the message, for retracting a blocked sender's notifications.
    pub sender: Option<String>,
    /// For [`Action::Mute`]: how long, in the units `set_notify_mute` takes.
    pub mute_ms: Option<i64>,
    /// Text typed into the notification, for [`Action::Reply`].
    pub reply: Option<String>,
}

impl Activation {
    pub fn new(action: Action, account: &str, data: &NotificationData) -> Option<Self> {
        Some(Self {
            action,
            account: account.to_string(),
            chat: data.chat_id.clone()?,
            message: data.message_id.clone(),
            sender: data.sender_npub.clone(),
            mute_ms: None,
            reply: None,
        })
    }

    /// The same target with a different action, or with the message dropped.
    pub fn with(&self, action: Action, keep_message: bool) -> Self {
        Self {
            action,
            message: if keep_message { self.message.clone() } else { None },
            ..self.clone()
        }
    }

    /// The same target as a mute of `ms`.
    pub fn mute(&self, ms: i64) -> Self {
        Self { action: Action::Mute, mute_ms: Some(ms), ..self.clone() }
    }

    /// A query string that fits any platform's opaque "arguments" or "user info" slot.
    pub fn encode(&self) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        s.append_pair("a", self.action.as_str())
            .append_pair("acct", &self.account)
            .append_pair("chat", &self.chat);
        if let Some(m) = &self.message {
            s.append_pair("msg", m);
        }
        if let Some(from) = &self.sender {
            s.append_pair("from", from);
        }
        if let Some(ms) = self.mute_ms {
            s.append_pair("dur", &ms.to_string());
        }
        s.finish()
    }

    /// Reverse of [`encode`](Self::encode); `reply` is whatever the user typed, if anything.
    pub fn decode(encoded: &str, reply: Option<&str>) -> Option<Self> {
        let mut map: HashMap<String, String> =
            url::form_urlencoded::parse(encoded.as_bytes()).into_owned().collect();
        Some(Self {
            action: Action::parse(map.get("a").map(String::as_str).unwrap_or("")),
            account: map.remove("acct").unwrap_or_default(),
            chat: map.remove("chat").filter(|c| !c.is_empty())?,
            message: map.remove("msg").filter(|m| !m.is_empty()),
            sender: map.remove("from").filter(|f| !f.is_empty()),
            mute_ms: map.remove("dur").and_then(|d| d.parse().ok()),
            reply: reply.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string),
        })
    }
}

fn live_account() -> Option<String> {
    vector_core::my_public_key().and_then(|pk| pk.to_bech32().ok())
}

/// Carry out a click or button press. Safe from any thread, including an OS callback
/// thread outside the async runtime; a panic never crosses back into the OS.
///
/// Nothing writes into an account other than the one the notification was shown for:
/// a read or reply for an account that isn't live is dropped, and opening one only
/// brings Vector forward.
pub fn handle(a: Activation) {
    let runtime = tauri::async_runtime::handle();
    let _context = runtime.inner().enter();
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(a))).is_err() {
        log_warn!("[Notify] Activation handler panicked");
    }
}

fn dispatch(a: Activation) {
    let Some(app) = crate::TAURI_APP.get() else { return };
    let live = live_account();
    let ours = live.as_deref() == Some(a.account.as_str());
    match a.action {
        Action::MarkRead if ours => {
            let chat = a.chat;
            vector_core::db::spawn_bound(async move {
                crate::chat::mark_as_read_headless(&chat).await;
                if let Some(app) = crate::TAURI_APP.get() {
                    let _ = crate::commands::messaging::update_unread_counter(app.clone()).await;
                }
            });
        }
        Action::MarkRead => {}
        Action::Mute if ours && a.mute_ms.is_some() => {
            let (chat, ms) = (a.chat, a.mute_ms.unwrap_or_default());
            vector_core::db::spawn_bound(async move {
                if let Err(e) = crate::commands::notify::set_notify_mute(chat, ms).await {
                    log_warn!("[Notify] Mute from a notification failed: {e}");
                }
            });
        }
        Action::Mute => {}
        Action::Reply if a.reply.is_some() && ours => send_reply(a),
        Action::Reply if a.reply.is_some() && live.is_none() => {
            // The notification launched Vector: the reply waits for its account to log in.
            *PENDING_REPLY.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
            reveal(app);
        }
        _ => {
            reveal(app);
            if live.is_none() || ours {
                crate::deep_link::open_chat_from_notification(app, &a.chat, a.message.as_deref(), Some(&a.account));
            }
        }
    }
}

fn send_reply(a: Activation) {
    let (Some(text), chat) = (a.reply, a.chat) else { return };
    vector_core::db::spawn_bound(async move {
        if let Err(e) = crate::message::send_text_reply_headless(&chat, &text).await {
            log_warn!("[Notify] Reply failed: {e}");
            if let Some(app) = crate::TAURI_APP.get() {
                let _ = app.emit("notification_reply_failed", serde_json::json!({ "chat_id": chat, "error": e }));
            }
        }
    });
}

/// Send a reply held from before login, if it belongs to the account now live.
pub fn flush_pending_reply() {
    let pending = PENDING_REPLY.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(a) = pending {
        if live_account().as_deref() == Some(a.account.as_str()) {
            send_reply(a);
        }
    }
}

fn reveal<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// ---------------------------------------------------------------------------
// Shared helpers for backends
// ---------------------------------------------------------------------------

/// The community badge's share of the icon's width, and the clear ring cut around it.
const BADGE_FRACTION: f32 = 0.42;
const BADGE_GAP_FRACTION: f32 = 0.045;

/// A notification's icon: a round PNG of `px` pixels on a transparent background, cached
/// under [`cache_dir`]. `main` (the sender) fills the circle, and `badge` (their
/// community) sits in the bottom-right corner as a smaller circle cut out of it, so the
/// community reads at a glance. Without a usable `main` the badge fills the circle alone.
///
/// Drawn here rather than cropped by the platform, whose crop would cut the badge in half.
/// Sources arrive as WebP, GIF, ICO and more where several platforms render only PNG; one
/// the image crate can't decode (SVG) is left out.
pub fn notification_icon(main: Option<&Path>, badge: Option<&Path>, px: u32) -> Option<PathBuf> {
    let main = main.filter(|p| p.is_file());
    let badge = badge.filter(|p| p.is_file());
    let sources: Vec<&Path> = main.into_iter().chain(badge).collect();
    if sources.is_empty() {
        return None;
    }
    let key = format!("{}|{}|{px}", main.map(|p| p.display().to_string()).unwrap_or_default(),
        badge.map(|p| p.display().to_string()).unwrap_or_default());
    let mut id = [0u8; 16];
    id.copy_from_slice(&Sha256::digest(key.as_bytes())[..16]);
    let out = cache_dir()?.join("avatars").join(format!("{}.png", vector_core::simd::hex::bytes_to_hex_16(&id)));

    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let fresh = modified(&out).is_some_and(|o| sources.iter().all(|s| modified(s).is_some_and(|s| o >= s)));
    if !fresh {
        let decode = |p: Option<&Path>| p.and_then(|p| image::open(p).ok());
        let (main, badge) = match (decode(main), decode(badge)) {
            (Some(m), b) => (m, b),
            (None, Some(b)) => (b, None),
            (None, None) => return None,
        };
        compose_icon(&main, badge.as_ref(), px).save_with_format(&out, image::ImageFormat::Png).ok()?;
    }
    Some(out)
}

/// How much of a pixel centred `(dx, dy)` from a circle's centre lies inside `radius`:
/// a one-pixel ramp, so edges are smooth at any size.
fn coverage(dx: f32, dy: f32, radius: f32) -> f32 {
    (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0)
}

fn compose_icon(main: &image::DynamicImage, badge: Option<&image::DynamicImage>, px: u32) -> image::RgbaImage {
    use image::imageops::FilterType::Lanczos3;
    let main = main.resize_to_fill(px, px, Lanczos3).to_rgba8();
    let size = px as f32;
    let radius = size / 2.0;
    let badge_px = ((size * BADGE_FRACTION).round() as u32).max(1);
    let badge = badge.map(|b| b.resize_to_fill(badge_px, badge_px, Lanczos3).to_rgba8());
    let badge_radius = badge_px as f32 / 2.0;
    let badge_centre = size - badge_radius;
    let badge_origin = px - badge_px;
    let gap = size * BADGE_GAP_FRACTION;

    let mut out = image::RgbaImage::new(px, px);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let m = main.get_pixel(x, y).0;
        let mut main_a = coverage(fx - radius, fy - radius, radius) * m[3] as f32 / 255.0;
        let (mut badge_rgb, mut badge_a) = ([0.0f32; 3], 0.0f32);
        if let Some(b) = &badge {
            let (dx, dy) = (fx - badge_centre, fy - badge_centre);
            main_a *= 1.0 - coverage(dx, dy, badge_radius + gap);
            if x >= badge_origin && y >= badge_origin {
                let bp = b.get_pixel(x - badge_origin, y - badge_origin).0;
                badge_a = coverage(dx, dy, badge_radius) * bp[3] as f32 / 255.0;
                badge_rgb = [bp[0] as f32, bp[1] as f32, bp[2] as f32];
            }
        }
        // The badge over the avatar ("over" with straight alpha).
        let a = badge_a + main_a * (1.0 - badge_a);
        if a > 0.0 {
            let mix = |i: usize| ((badge_rgb[i] * badge_a + m[i] as f32 * main_a * (1.0 - badge_a)) / a).round() as u8;
            *pixel = image::Rgba([mix(0), mix(1), mix(2), (a * 255.0).round() as u8]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_round_trips_and_trims_the_reply() {
        let a = Activation {
            action: Action::Reply,
            account: "npub1abc".into(),
            chat: "npub1chat".into(),
            message: Some("ff00".into()),
            sender: Some("npub1from".into()),
            mute_ms: Some(-1),
            reply: None,
        };
        let back = Activation::decode(&a.encode(), Some("  hi there  ")).unwrap();
        assert_eq!(back, Activation { reply: Some("hi there".into()), ..a });
    }

    #[test]
    fn activation_without_a_chat_is_rejected() {
        assert!(Activation::decode("a=open&acct=npub1abc", None).is_none());
    }

    #[test]
    fn unknown_or_missing_action_opens() {
        assert_eq!(Activation::decode("chat=x&a=launch-missiles", None).unwrap().action, Action::Open);
        assert_eq!(Activation::decode("chat=x", None).unwrap().action, Action::Open);
    }

    fn test_cache_dir() -> &'static Path {
        let dir = std::env::temp_dir().join(format!("vector-notify-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("avatars")).unwrap();
        let _ = CACHE_DIR.set(dir);
        cache_dir().unwrap()
    }

    fn bundled(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("icons").join(name)
    }

    #[test]
    fn icon_is_round_cached_and_needs_a_source() {
        let dir = test_cache_dir();
        // An ICO, which no toast can render, comes out as PNG.
        let main = bundled("icon.ico");
        let out = notification_icon(Some(&main), None, 64).unwrap();
        let img = image::open(&out).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (64, 64));
        assert_eq!(img.get_pixel(0, 0)[3], 0, "the corners are outside the circle");

        let written = std::fs::metadata(&out).unwrap().modified().unwrap();
        assert_eq!(notification_icon(Some(&main), None, 64).unwrap(), out);
        assert_eq!(std::fs::metadata(&out).unwrap().modified().unwrap(), written, "reused, not redrawn");

        assert!(notification_icon(Some(&dir.join("missing.png")), None, 64).is_none());
        assert!(notification_icon(None, None, 64).is_none());
        assert!(notification_icon(None, Some(&bundled("128x128.png")), 64).is_some(), "a badge alone still shows");
    }

    #[test]
    fn community_badge_sits_bottom_right_behind_a_clear_ring() {
        let solid = |c: [u8; 4]| image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(100, 100, image::Rgba(c)));
        let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
        let px = 200;
        let icon = compose_icon(&solid(red), Some(&solid(blue)), px);

        let badge_radius = (px as f32 * BADGE_FRACTION).round() / 2.0;
        let badge_centre = px as f32 - badge_radius;
        let ring = badge_radius + px as f32 * BADGE_GAP_FRACTION / 2.0;
        let towards_middle = (badge_centre - ring / std::f32::consts::SQRT_2) as u32;

        assert_eq!(icon.get_pixel(px / 2, px / 2).0, red, "the sender fills the middle");
        assert_eq!(icon.get_pixel(badge_centre as u32, badge_centre as u32).0, blue, "the community, bottom right");
        assert_eq!(icon.get_pixel(towards_middle, towards_middle)[3], 0, "a clear ring parts them");
        assert_eq!(icon.get_pixel(0, 0)[3], 0);
        assert_eq!(icon.get_pixel(px - 1, 0)[3], 0);
    }

    #[test]
    fn blank_reply_is_none() {
        assert!(Activation::decode("a=reply&chat=x", Some("   ")).unwrap().reply.is_none());
    }
}
