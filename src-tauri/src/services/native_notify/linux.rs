//! Linux notifications through the freedesktop notification server, which GNOME Shell and
//! Plasma both are: the round avatar as the notification's image, a reply field where the
//! server offers one (Plasma's `inline-reply`), then Mark as Read and the shortest mutes.
//!
//! The server keeps no history a client can search, so retraction closes the ids shown this
//! run. Signals arrive on the GTK main loop; a click's activation token reaches GDK before the
//! window is presented, since Wayland focuses only a window activated with one.

use std::collections::HashMap;
use std::ffi::{c_char, CString};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use glib::prelude::*;
use glib::translate::ToGlibPtr;
use gtk::gio;
use tauri::AppHandle;

use super::{Action, Activation, Backend, Matcher};
use crate::services::notification_service::{NotificationData, NotificationType};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const DEFAULT: &str = "default";
const REPLY: &str = "inline-reply";
const READ: &str = "read";
const MUTE_PREFIX: &str = "mute:";
const ORIGIN: &str = "x-kde-origin-name";
/// `NotificationClosed` reason for a popup that timed out: Plasma keeps it in its history,
/// where closing it by id still removes it.
const EXPIRED: u32 = 1;
const CALL_TIMEOUT_MS: i32 = 5_000;
/// Shown at up to 64 logical pixels, on screens scaled up to 3x.
const ICON_PX: u32 = 192;
const MAX_TRACKED: usize = 256;
/// GNOME draws no more, and Plasma lays every one out in a single row.
const MAX_BUTTONS: usize = 3;

static WORKER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
/// What each notification shown this run opens, by server id, oldest first.
static SHOWN: Mutex<Vec<(u32, Activation)>> = Mutex::new(Vec::new());
/// The activation token for the notification most recently clicked.
static TOKEN: Mutex<Option<(u32, String)>> = Mutex::new(None);

enum Job {
    Show(Box<NotificationData>, Activation),
    Retract(Matcher),
}

/// What the server draws, from its capabilities.
#[derive(Default)]
struct Caps {
    markup: bool,
    reply: bool,
    /// `x-kde-origin-name`, shown beside the app name in the header.
    origin: bool,
}

pub struct Linux;

impl Backend for Linux {
    fn init(app: &AppHandle) -> Result<(), String> {
        let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| e.to_string())?;
        // Subscribed from the main thread, so signals are dispatched on the GTK main loop.
        bus.signal_subscribe(Some(NAME), Some(NAME), None, Some(PATH), None, gio::DBusSignalFlags::NONE, |_, _, _, _, signal, params| {
            on_signal(signal, params)
        });
        let product = app.config().product_name.clone().unwrap_or_else(|| "Vector".into());
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new().name("notify".into()).spawn(move || serve(bus, product, rx)).map_err(|e| e.to_string())?;
        let _ = WORKER.set(Mutex::new(tx));
        Ok(())
    }

    fn show(data: &NotificationData, account: &str) -> Result<(), String> {
        let open = Activation::new(Action::Open, account, data).ok_or("no chat id")?;
        worker().ok_or("no notification worker")?.send(Job::Show(Box::new(data.clone()), open)).map_err(|e| e.to_string())
    }

    fn retract(matches: Matcher) {
        if let Some(tx) = worker() {
            let _ = tx.send(Job::Retract(matches));
        }
    }
}

fn worker() -> Option<Sender<Job>> {
    WORKER.get()?.lock().ok().map(|t| t.clone())
}

/// One thread makes every call, so a retraction always follows the show it retracts.
fn serve(bus: gio::DBusConnection, product: String, jobs: Receiver<Job>) {
    let mut caps: Option<Caps> = None;
    let mut entry: Option<String> = None;
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Show(data, open) => {
                if caps.is_none() {
                    caps = capabilities(&bus);
                }
                let entry = entry.get_or_insert_with(|| desktop_entry(&product));
                if let Err(e) = show_now(&bus, caps.as_ref().unwrap_or(&Caps::default()), entry, &data, open) {
                    log_warn!("[Notify] Notification not delivered: {e}");
                }
            }
            Job::Retract(matches) => retract_now(&bus, &matches),
        }
    }
}

fn call(bus: &gio::DBusConnection, method: &str, args: Option<&glib::Variant>) -> Result<glib::Variant, glib::Error> {
    bus.call_sync(Some(NAME), PATH, NAME, method, args, None, gio::DBusCallFlags::NONE, CALL_TIMEOUT_MS, gio::Cancellable::NONE)
}

fn capabilities(bus: &gio::DBusConnection) -> Option<Caps> {
    let (caps,) = call(bus, "GetCapabilities", None).ok()?.get::<(Vec<String>,)>()?;
    let has = |cap: &str| caps.iter().any(|c| c == cap);
    Some(Caps { markup: has("body-markup"), reply: has(REPLY), origin: has(ORIGIN) })
}

fn show_now(bus: &gio::DBusConnection, caps: &Caps, entry: &str, data: &NotificationData, open: Activation) -> Result<(), String> {
    let (summary, body, actions, mut hints) = content(caps, entry, data);
    let community = data.notification_type == NotificationType::CommunityMessage;
    let icon = super::notification_icon(
        data.avatar_path.as_deref().map(Path::new),
        data.group_avatar_path.as_deref().filter(|_| community).map(Path::new),
        ICON_PX,
    );
    if let Some(image) = icon.as_deref().and_then(image_data) {
        hints.insert("image-data".into(), image);
    }

    // One notification per message: a re-delivery replaces rather than duplicates it.
    let same = |a: &Activation| a.account == open.account && a.chat == open.chat && a.message == open.message;
    let replaces = shown().iter().find(|(_, a)| same(a)).map_or(0, |(id, _)| *id);
    let args = ("Vector", replaces, "", summary, body, actions, hints, -1i32).to_variant();
    let (id,) = call(bus, "Notify", Some(&args)).map_err(|e| e.to_string())?.get::<(u32,)>().ok_or("unexpected Notify reply")?;

    let mut shown = shown();
    shown.retain(|(old, a)| *old != id && !same(a));
    shown.push((id, open));
    let over = shown.len().saturating_sub(MAX_TRACKED);
    shown.drain(..over);
    Ok(())
}

fn retract_now(bus: &gio::DBusConnection, matches: &Matcher) {
    let gone: Vec<u32> = {
        let mut shown = shown();
        let ids = shown.iter().filter(|(_, a)| matches(a)).map(|(id, _)| *id).collect();
        shown.retain(|(_, a)| !matches(a));
        ids
    };
    for id in gone {
        let _ = call(bus, "CloseNotification", Some(&(id,).to_variant()));
    }
}

/// Summary, body, actions and hints for `data`, before its image.
fn content(caps: &Caps, entry: &str, data: &NotificationData) -> (String, String, Vec<String>, HashMap<String, glib::Variant>) {
    let community = data.notification_type == NotificationType::CommunityMessage;
    let origin = data.group_name.clone().filter(|name| caps.origin && community && !name.is_empty());
    // Where the community has its own place in the header, the summary names just the sender.
    let summary = match &origin {
        Some(_) => data.sender_name.clone().unwrap_or_else(|| data.title.clone()),
        None => data.title.clone(),
    };
    let body = if caps.markup { escape_markup(&data.body) } else { data.body.clone() };

    let mut hints: HashMap<String, glib::Variant> = HashMap::new();
    let mut buttons = Vec::with_capacity(2 + super::MUTE_CHOICES.len());
    if caps.reply {
        buttons.push((REPLY.to_string(), "Reply".to_string()));
        // The sender's name can be a fallback ("New Message") or masked, so the placeholder names nobody.
        hints.insert("x-kde-reply-placeholder-text".into(), "Type a reply…".to_variant());
        hints.insert("x-kde-reply-submit-button-text".into(), "Send".to_variant());
    }
    buttons.push((READ.to_string(), "Mark as Read".to_string()));
    buttons.extend(super::MUTE_CHOICES.iter().map(|(label, ms)| (format!("{MUTE_PREFIX}{ms}"), label.to_string())));
    let mut actions = vec![DEFAULT.to_string(), "Open".to_string()];
    actions.extend(buttons.into_iter().take(MAX_BUTTONS).flat_map(|(key, label)| [key, label]));

    hints.insert("desktop-entry".into(), entry.to_variant());
    hints.insert("category".into(), "im.received".to_variant());
    hints.insert("urgency".into(), 1u8.to_variant());
    // Vector plays its own sound.
    hints.insert("suppress-sound".into(), true.to_variant());
    if let Some(name) = origin {
        hints.insert(ORIGIN.into(), name.to_variant());
    }
    (summary, body, actions, hints)
}

/// The spec's `image-data` (`iiibiiay`): unpremultiplied RGBA, so it needs no file the server can reach.
fn image_data(png: &Path) -> Option<glib::Variant> {
    let rgba = image::open(png).ok()?.to_rgba8();
    let (w, h) = (rgba.width() as i32, rgba.height() as i32);
    Some(glib::Variant::tuple_from_iter([
        w.to_variant(),
        h.to_variant(),
        (w * 4).to_variant(),
        true.to_variant(),
        8i32.to_variant(),
        4i32.to_variant(),
        glib::Variant::array_from_fixed_array(rgba.as_raw()),
    ]))
}

/// A server that renders body markup parses whatever it's given; message text is not markup.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// The installed .desktop id the server names Vector by: an app it can find, which on GNOME is
/// what gets a click an activation token.
///
/// snapd prefixes the snap's own. Otherwise the entry the compositor ties the window to (its
/// app id is the program name), then `{productName}.desktop` as packages install it, then the
/// deep-link plugin's scheme handler for this binary, which is all an AppImage may have.
fn desktop_entry(product: &str) -> String {
    if let Some(snap) = std::env::var("SNAP_INSTANCE_NAME").ok().filter(|s| !s.is_empty()) {
        return format!("{snap}_vector");
    }
    let exe = std::env::current_exe().ok();
    let handler = exe.as_deref().and_then(Path::file_name).map(|bin| format!("{}-handler", bin.to_string_lossy()));
    let candidates = [glib::prgname().map(|p| p.to_string()), Some(product.to_string()), handler];
    candidates
        .into_iter()
        .flatten()
        .find(|id| gio::DesktopAppInfo::new(&format!("{id}.desktop")).is_some())
        .unwrap_or_else(|| product.to_string())
}

fn shown() -> std::sync::MutexGuard<'static, Vec<(u32, Activation)>> {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner())
}

fn opens(id: u32) -> Option<Activation> {
    shown().iter().find(|(shown, _)| *shown == id).map(|(_, a)| a.clone())
}

/// On the GTK main loop. Every client hears every signal, so ids not shown here are ignored.
fn on_signal(signal: &str, params: &glib::Variant) {
    match signal {
        "ActivationToken" => {
            if let Some((id, token)) = params.get::<(u32, String)>().filter(|(id, _)| opens(*id).is_some()) {
                *TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((id, token));
            }
        }
        "ActionInvoked" => {
            if let Some((id, key)) = params.get::<(u32, String)>() {
                if let Some(a) = opens(id).and_then(|open| activation_for(&open, &key)) {
                    activate(id, a);
                }
            }
        }
        "NotificationReplied" => {
            if let Some((id, text)) = params.get::<(u32, String)>() {
                if let Some(open) = opens(id) {
                    let mut reply = open.with(Action::Reply, true);
                    reply.reply = Some(text.trim().to_string()).filter(|t| !t.is_empty());
                    activate(id, reply);
                }
            }
        }
        "NotificationClosed" => {
            if let Some((id, _)) = params.get::<(u32, u32)>().filter(|(_, reason)| *reason != EXPIRED) {
                shown().retain(|(shown, _)| *shown != id);
            }
        }
        _ => {}
    }
}

/// What the pressed action asks for. Dismissal (and anything unknown) does nothing.
fn activation_for(open: &Activation, key: &str) -> Option<Activation> {
    match key {
        DEFAULT => Some(open.clone()),
        READ => Some(open.with(Action::MarkRead, true)),
        _ => key.strip_prefix(MUTE_PREFIX).and_then(|ms| ms.parse().ok()).map(|ms| open.mute(ms)),
    }
}

fn activate(id: u32, a: Activation) {
    let token = TOKEN.lock().unwrap_or_else(|e| e.into_inner()).take().filter(|(clicked, _)| *clicked == id);
    if let Some((_, token)) = token.filter(|_| a.action == Action::Open) {
        hand_to_gdk(&token);
    }
    super::handle(a);
}

/// GDK spends the token on the window's next present, which is how Wayland lets it take focus.
fn hand_to_gdk(token: &str) {
    let Some(display) = gdk::Display::default() else { return };
    if display.type_().name() != "GdkWaylandDisplay" {
        return;
    }
    // Looked up at run time: a GTK built without its Wayland backend lacks the symbol.
    let set = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"gdk_wayland_display_set_startup_notification_id".as_ptr()) };
    if set.is_null() {
        return;
    }
    let Ok(token) = CString::new(token) else { return };
    // SAFETY: GDK 3's `void (GdkDisplay *, const char *)`, given a Wayland display.
    let set: unsafe extern "C" fn(*mut gdk::ffi::GdkDisplay, *const c_char) = unsafe { std::mem::transmute(set) };
    unsafe { set(display.to_glib_none().0, token.as_ptr()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kde() -> Caps {
        Caps { markup: true, reply: true, origin: true }
    }

    fn gnome() -> Caps {
        Caps { markup: true, reply: false, origin: false }
    }

    fn community() -> NotificationData {
        NotificationData::community_message(
            "Alice".into(),
            "Cats".into(),
            "hi <b>&</b>".into(),
            None,
            None,
            "chan".into(),
        )
    }

    fn keys(actions: &[String]) -> Vec<&str> {
        actions.iter().step_by(2).map(String::as_str).collect()
    }

    #[test]
    fn three_buttons_reply_first_where_offered_then_read_then_the_shortest_mutes() {
        let mute = |i: usize| format!("{MUTE_PREFIX}{}", super::super::MUTE_CHOICES[i].1);

        let (_, _, actions, hints) = content(&gnome(), "Vector", &community());
        assert_eq!(keys(&actions), [DEFAULT, READ, mute(0).as_str(), mute(1).as_str()]);
        assert!(!hints.contains_key("x-kde-reply-placeholder-text"));

        let (_, _, actions, hints) = content(&kde(), "Vector", &community());
        assert_eq!(keys(&actions), [DEFAULT, REPLY, READ, mute(0).as_str()]);
        assert!(hints.contains_key("x-kde-reply-placeholder-text"));
    }

    #[test]
    fn a_community_names_its_sender_where_the_header_carries_the_community() {
        let (summary, _, _, hints) = content(&kde(), "Vector", &community());
        assert_eq!(summary, "Alice");
        assert_eq!(hints["x-kde-origin-name"].get::<String>().as_deref(), Some("Cats"));

        let (summary, _, _, hints) = content(&gnome(), "Vector", &community());
        assert_eq!(summary, "Alice - Cats");
        assert!(!hints.contains_key("x-kde-origin-name"));
    }

    #[test]
    fn the_body_is_escaped_only_for_a_server_that_parses_markup() {
        assert_eq!(content(&gnome(), "Vector", &community()).1, "hi &lt;b&gt;&amp;&lt;/b&gt;");
        assert_eq!(content(&Caps::default(), "Vector", &community()).1, "hi <b>&</b>");
    }

    #[test]
    fn notify_arguments_match_the_spec_signature() {
        let (summary, body, actions, hints) = content(&kde(), "vector_vector", &community());
        assert_eq!(hints["desktop-entry"].get::<String>().as_deref(), Some("vector_vector"));
        let args = ("Vector", 0u32, "", summary, body, actions, hints, -1i32).to_variant();
        assert_eq!(args.type_().as_str(), "(susssasa{sv}i)");
    }

    #[test]
    fn image_data_is_rgba_rows_in_the_spec_layout() {
        let dir = std::env::temp_dir().join(format!("vector-notify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("icon.png");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 4])).save(&png).unwrap();
        let v = image_data(&png).unwrap();
        assert_eq!(v.type_().as_str(), "(iiibiiay)");
        assert_eq!(v.child_value(2).get::<i32>(), Some(12));
        assert_eq!(v.child_value(6).fixed_array::<u8>().unwrap(), [1, 2, 3, 4].repeat(6).as_slice());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn actions_map_back_to_what_they_ask_for() {
        let open = Activation {
            action: Action::Open,
            account: "npub1me".into(),
            chat: "npub1chat".into(),
            message: Some("ab12".into()),
            sender: Some("npub1from".into()),
            mute_ms: None,
            reply: None,
        };
        assert_eq!(activation_for(&open, DEFAULT), Some(open.clone()));
        assert_eq!(activation_for(&open, READ).map(|a| a.action), Some(Action::MarkRead));
        let mute = activation_for(&open, "mute:-1").unwrap();
        assert_eq!((mute.action, mute.mute_ms), (Action::Mute, Some(-1)));
        assert_eq!(activation_for(&open, REPLY), None);
        assert_eq!(activation_for(&open, "mute:soon"), None);
    }
}
