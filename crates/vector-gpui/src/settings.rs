//! Settings (`settings/Settings.svelte` in widescreen): a `SectionNav` in the list column,
//! and the chosen category's headed blocks beside it.

use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;
use std::rc::Rc;

use gpui::AnimationExt;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Entity, FontWeight, PathBuilder, ScrollHandle, SharedString, Window, canvas,
    div, img, px, svg,
};

use crate::composer::TextInput;
use crate::theme::*;

#[path = "settings_slim.rs"]
mod slim;
pub use slim::slim_view;

pub struct Anchor {
    pub id: &'static str,
    pub label: &'static str,
    pub keys: &'static str,
    /// The release line it arrived in, for its NEW pill.
    since: Option<&'static str>,
}

pub struct Category {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: &'static str,
    pub danger: bool,
    pub anchors: Vec<Anchor>,
}

pub const fn anchor(id: &'static str, label: &'static str, keys: &'static str) -> Anchor {
    Anchor {
        id,
        label,
        keys,
        since: None,
    }
}

const fn fresh(
    id: &'static str,
    label: &'static str,
    since: &'static str,
    keys: &'static str,
) -> Anchor {
    Anchor {
        id,
        label,
        keys,
        since: Some(since),
    }
}

/// The categories this build offers, in the nav's order.
pub fn categories(offers: &Offers) -> Vec<Category> {
    let mut audio = Vec::new();
    if offers.voice {
        audio.push(anchor(
            "voice",
            "Voice",
            "voice whisper transcribe transcription translate model download",
        ));
    }
    audio.push(fresh("calls", "Calls", "0.5", "calls microphone speaker automatic gain echo cancellation noise suppression test mic device"));
    let mut privacy = vec![
        anchor(
            "privacy",
            "Privacy",
            "web previews video players youtube embeds url tracking typing indicators proxy media",
        ),
        fresh(
            "streamer",
            "Streamer Mode",
            "0.5",
            "streamer stream streaming live obs twitch screen share hide names pictures notifications",
        ),
    ];
    if offers.routing {
        privacy.push(fresh("routing", "Routing", "0.5", "routing network tor i2p onion standard clearnet bridges obfs4 circuit outproxy sam router anonymity"));
    }
    privacy.push(anchor(
        "blocked",
        "Blocked Users",
        "blocked users block unblock",
    ));
    let cat = |id, label, icon, anchors| Category {
        id,
        label,
        icon,
        danger: false,
        anchors,
    };
    let mut out = vec![
        cat(
            "appearance",
            "Appearance",
            "palette",
            vec![anchor(
                "display",
                "Display",
                "theme colour color vector satoshi chatstr cosmic purple neon xmr display image types background wallpaper rich composer emoticon suggestions time suggestions countdown autocorrect send on enter return key floating player main menu navbar navigation pins sidebar tabs",
            )],
        ),
        cat("audio", "Audio", "volume-max", audio),
        cat(
            "notifications",
            "Notifications",
            "bell",
            vec![anchor(
                "notifications",
                "Notifications",
                "mute sounds everyone pings content privacy sound prelude techno custom",
            )],
        ),
        cat(
            "network",
            "Network",
            "globe",
            vec![
                anchor("relays", "Nostr Relays", "network nostr relays add custom"),
                anchor(
                    "servers",
                    "Media Servers",
                    "network media servers blossom upload add custom",
                ),
                anchor(
                    "clock",
                    "Clock",
                    "clock time date decentralised decentralized device relays slow fast timestamp",
                ),
            ],
        ),
        cat(
            "storage",
            "Storage",
            "folder",
            vec![anchor(
                "storage",
                "Storage",
                "storage usage clear auto-download download limit gallery show save keep files unencrypted media video quality compression compress",
            )],
        ),
        cat("privacy", "Privacy", "eye-off", privacy),
        cat(
            "security",
            "Security",
            "locked",
            vec![anchor(
                "security",
                "Security",
                "security pin password biometric signer bunker encryption key",
            )],
        ),
    ];
    if offers.updates {
        out.push(cat(
            "updates",
            "Updates",
            "download",
            vec![anchor(
                "updates",
                "Updates",
                "updates version release changelog download install",
            )],
        ));
    }
    out.push(Category {
        id: "danger",
        label: "Danger Zone",
        icon: "warning",
        danger: true,
        anchors: vec![
            anchor(
                "advanced",
                "Advanced",
                "advanced mode developer bot ids copy id identifier",
            ),
            anchor("logs", "Logs", "copy logs crash debug"),
            anchor("logout", "Logout", "logout log out sign out"),
        ],
    });
    out
}

/// What this build and platform can show.
pub struct Offers {
    pub voice: bool,
    pub routing: bool,
    pub updates: bool,
}

/// Where a toggle's value lives.
#[derive(Clone, Copy, PartialEq)]
pub enum Home {
    /// An account setting, with its default when never set.
    Sql(bool),
    /// This device's own, stored as one of two words: (on, off, default).
    Device(&'static str, &'static str, bool),
    /// The account's synced settings document.
    Synced,
    /// A field of our own profile.
    Profile,
}

pub struct Toggle {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub home: Home,
    group: &'static str,
}

const fn t(
    key: &'static str,
    label: &'static str,
    help: &'static str,
    home: Home,
    group: &'static str,
) -> Toggle {
    Toggle {
        key,
        label,
        help,
        home,
        group,
    }
}

/// `Display.svelte`'s rows in widescreen.
pub const DISPLAY: [Toggle; 8] = [
    t(
        "ws_nav",
        "Main Menu",
        "startMenu",
        Home::Device("start", "navbar", true),
        "Navigation",
    ),
    t(
        "display_image_types",
        "Display Image Types",
        "imageTypes",
        Home::Sql(false),
        "Chat",
    ),
    t(
        "chat_bg_enabled",
        "Background Wallpaper",
        "chatBg",
        Home::Sql(true),
        "Chat",
    ),
    t(
        "emoticon_suggestions",
        "Emoticon Suggestions",
        "emoticons",
        Home::Sql(true),
        "Composer",
    ),
    t(
        "time_suggestions",
        "Time Suggestions",
        "timeSuggestions",
        Home::Sql(true),
        "Composer",
    ),
    t(
        "autocorrect",
        "Autocorrect",
        "autocorrect",
        Home::Sql(true),
        "Composer",
    ),
    t(
        "send_on_enter",
        "Send on Enter",
        "sendOnEnter",
        Home::Device("true", "false", true),
        "Composer",
    ),
    t(
        "floating_player",
        "Floating Player",
        "floatingPlayer",
        Home::Sql(true),
        "Media",
    ),
];

/// Display's slim-only row: the rail folded with nothing to pull it out.
pub const RAIL_HIDDEN: Toggle = t(
    "cc_rail_hidden",
    "Hide Community Rail",
    "hideRail",
    Home::Device("true", "false", false),
    "Chat List",
);

pub const PRIVACY: [Toggle; 4] = [
    t(
        "web_previews",
        "Display Web Previews",
        "webPreviews",
        Home::Sql(true),
        "",
    ),
    t(
        "strip_tracking",
        "Prevent URL Tracking",
        "stripTracking",
        Home::Sql(true),
        "",
    ),
    t(
        "send_typing_indicators",
        "Send Typing Indicators",
        "sendTyping",
        Home::Sql(true),
        "",
    ),
    t(
        "privacy_proxy_media",
        "Proxy Previews & Media",
        "proxyMedia",
        Home::Sql(true),
        "",
    ),
];

pub const OTHER: [Toggle; 4] = [
    t(
        "streamer_on",
        "Streamer Mode",
        "streamerMode",
        Home::Synced,
        "",
    ),
    t(
        "stream_consent",
        "Show me on streams",
        "streamConsent",
        Home::Profile,
        "",
    ),
    t(
        "streamer_wallpapers",
        "Hide chat wallpapers",
        "streamerWallpapers",
        Home::Synced,
        "",
    ),
    t(
        "advanced",
        "Advanced Mode",
        "advancedMode",
        Home::Synced,
        "",
    ),
];

pub const NOTIFY: [Toggle; 2] = [
    t(
        "notif_global_mute",
        "Mute All Notification Sounds",
        "mute",
        Home::Sql(false),
        "",
    ),
    t(
        "notif_mute_everyone",
        "Mute @Everyone Pings",
        "everyone",
        Home::Sql(false),
        "",
    ),
];

pub const STORAGE: [Toggle; 3] = [
    t(
        "auto_download_enabled",
        "Auto-Download Media",
        "autoDownload",
        Home::Sql(true),
        "",
    ),
    t(
        "keep_media",
        "Keep Files on This Device",
        "keepMedia",
        Home::Sql(false),
        "",
    ),
    t(
        "keep_unencrypted",
        "Keep Unencrypted",
        "keepUnencrypted",
        Home::Sql(false),
        "",
    ),
];

pub const CLOCK: Toggle = t(
    "clock_decentralised",
    "Decentralised Clock",
    "clock",
    Home::Sql(true),
    "",
);

pub fn toggles() -> impl Iterator<Item = &'static Toggle> {
    DISPLAY
        .iter()
        .chain(PRIVACY.iter())
        .chain(NOTIFY.iter())
        .chain(STORAGE.iter())
        .chain(OTHER.iter())
        .chain(std::iter::once(&CLOCK))
        .chain(std::iter::once(&RAIL_HIDDEN))
}

pub fn toggle_of(key: &str) -> Option<&'static Toggle> {
    toggles().find(|t| t.key == key)
}

/// One explainer per (i): a title and HTML-lite text (`SETTINGS_HELP`, `DISPLAY_EXPLAINERS`).
pub fn help(key: &str) -> Option<(&'static str, String)> {
    let (title, text) = match key {
        "startMenu" => (
            "Main Menu",
            "<b>When enabled:</b> your Profile, Chat, Bookmarks, Mini Apps and Settings live in a Main Menu that opens from your row at the bottom of the sidebar, beside pins for the apps, chats and places you use most.<br><br><b>When disabled:</b> they sit down the sidebar as tabs, as before.<br><br>This applies to this device only.",
        ),
        "hideRail" => (
            "Hide Community Rail",
            "When enabled, the <b>community rail and its pull tab are hidden</b>, and your communities sit in the chat list behind the <b>All</b>, <b>Messages</b>, <b>Communities</b> and <b>Requests</b> filters.<br><br>This applies to this device only.",
        ),
        "imageTypes" => (
            "Display Image Types",
            "When enabled, images in chat will display a <b>small badge showing the file type</b> (e.g., PNG, GIF, WEBP) in the corner.<br><br>This helps identify image formats at a glance.",
        ),
        "chatBg" => (
            "Background Wallpaper",
            "This feature enables and disables background images inside of Chats (DMs & Communities).<br><br>Only applies to certain themes.",
        ),
        "timeSuggestions" => (
            "Time Suggestions",
            "When you type a time to come, like <b>in 6 hours</b> or <b>dec 25 at 8pm</b>, the composer offers to send it as a time everyone sees in their own zone: a live countdown, or the date and time where they are.<br><br>Press <b>Tab</b> or tap the suggestion to use it; keep typing to leave it as written.",
        ),
        "emoticons" => (
            "Emoticon Suggestions",
            "When enabled, text emoticons suggest the matching emoji as you type:<br><br><b>:)</b> → 🙂&nbsp;&nbsp; <b>:D</b> → 😄&nbsp;&nbsp; <b>:P</b> → 😛&nbsp;&nbsp; <b>:3</b> → 😺<br><br>Turn it off to type emoticons as plain text (e.g. <b>:3</b>) without the emoji selector getting in the way.",
        ),
        "autocorrect" => (
            "Autocorrect",
            "When enabled, your device corrects typos as you type in the chat box, using your system's autocorrect.<br><br>Turn it off if your system keeps \"fixing\" words you meant to type.",
        ),
        "sendOnEnter" => (
            "Send on Enter",
            "When enabled, <b>Enter</b> sends your message, and <b>Shift + Enter</b> starts a new line.<br><br>Turn it off to have Enter always start a new line; send with the Send button, or <b>Ctrl + Enter</b> (<b>Cmd + Enter</b> on a Mac).<br><br>On by default on computers and off on phones. This applies to this device only.",
        ),
        "floatingPlayer" => (
            "Floating Player",
            "When enabled, <b>audio and video keep playing</b> when you leave their chat, in a small player you can move and resize anywhere.<br><br>Runs of voice messages play through, and a video can shrink to sound only.<br><br>Turn it off to stop media when you leave its chat.",
        ),
        "relays" => (
            "Nostr Relays",
            "Nostr Relays are <b>decentralized servers that store and relay your messages</b> across the Nostr network.<br><br>Vector connects to multiple relays simultaneously to ensure your messages are delivered reliably and are censorship-resistant.",
        ),
        "servers" => (
            "Media Servers",
            "Media Servers are <b>Blossom-compatible servers that store your files</b> (images, videos, documents) for sharing in messages and for storage in an encrypted cloud.<br><br>Your server list syncs automatically across your devices.",
        ),
        "mute" => (
            "Mute Notification Sounds",
            "When enabled, Vector will <b>not play any notification sounds</b> for incoming messages.<br><br>You will still receive visual notifications and badges.",
        ),
        "everyone" => (
            "Mute @everyone Pings",
            "When enabled, <b>@everyone</b> mentions from group admins will <b>not bypass</b> your group mute setting.<br><br>By default, @everyone pings from admins will notify you even if the group is muted.",
        ),
        "notifPrivacy" => (
            "Notification Content Privacy",
            "Controls how much of a message shows in OS notifications (lock screen, banners).<br><br><b>Show sender and message</b>: full preview.<br><b>Hide message</b>: shows who messaged you, not what.<br><b>Hide sender</b>: shows the message, not who sent it.<br><b>Hide sender and message</b>: a generic \"You received a message\", revealing nothing.",
        ),
        "clock" => (
            "Decentralised Clock",
            "When enabled, Vector keeps time by your Nostr relays instead of this device. It asks up to eight of them for the time every hour, and uses their time only when at least three agree; otherwise it keeps this device’s.<br><br>A device whose clock has drifted otherwise sends messages that look minutes old or from the future, sees others’ messages the same way, and gets turned away by media servers that check the time.<br><br>Off: Vector uses this device’s clock as it is.",
        ),
        "keepMedia" => (
            "Keep Files on This Device",
            "<b>When enabled:</b> every file you receive is kept on this device, encrypted, so it opens instantly and never downloads twice.<br><br><b>When disabled:</b> files stay in memory while Vector is open, and load again after it closes. One too large to hold waits for a tap, then is kept. Turning it off deletes what it kept.",
        ),
        "keepUnencrypted" => {
            return Some((
                "Keep Unencrypted",
                format!("What Vector keeps is saved as normal files in {}, so other apps and backups can open them.<br><br>Anyone with access to this device's files can open them too.", crate::backend::mediaport::public_folder()),
            ));
        }
        "stripTracking" => (
            "Strip Tracking Markers",
            "When enabled, Vector will <b>automatically remove tracking markers</b> from URLs before displaying or sending them.<br><br>This helps reduce your footprint and enhances your privacy with no loss in functionality, only disable if you know what you're doing.",
        ),
        "sendTyping" => (
            "Send Typing Indicators",
            "When enabled, Vector will <b>notify your contacts when you are typing</b> a message to them.<br><br>Disable this if you prefer to type without others knowing you are composing a message.",
        ),
        "proxyMedia" => (
            "Proxy Previews & Media",
            "Fetches link previews and every download (avatars, banners, custom emoji, GIFs, pictures posted as links, attachments, wallpapers, community logos, mini apps) through your Magnitude server instead of from this device. Only your Nostr relays and Vector's own servers are reached directly.<br><br>A file loaded directly is a request from <b>your</b> address to whoever hosts it; through Magnitude, they see the server instead. Needs a Magnitude server in your Blossom list that offers it.<br><br>On Tor or I2P, a link preview with no Magnitude server to fetch it loads through that network instead.<br><br>Off: everything loads directly from its source.",
        ),
        "torMultiCircuit" => (
            "Multi-Circuit",
            "Each relay and media server gets its own Tor circuit. No single exit sees every server you use, and a slow circuit only slows the one connection riding it.<br><br>Turn this off to send everything over one shared circuit, like a VPN: fewer circuits to build, and one exit for all your traffic.",
        ),
        "torExternal" => (
            "Use Running Tor",
            "Vector connects through a Tor that's already running on this device instead of starting its own: <b>Tor Browser</b> (port 9150), or <b>tor</b> or <b>Orbot</b> (port 9050). It starts instantly and shares that Tor's bridges.<br><br>Vector trusts whatever answers on this port. Leave the port empty to find it.",
        ),
        "torBridges" => (
            "Use Bridges",
            "Bridges are private Tor relays that aren't listed publicly, so a network that blocks Tor can't block them as easily.<br><br>Turn this on if Tor fails to connect where you are, then paste bridge lines from <b>bridges.torproject.org</b>. You can set them up before switching to Tor.",
        ),
        "i2pRouter" => (
            "I2P Router",
            "Vector uses the I2P router already running on this device, through its SAM bridge.<br><br>In i2pd, set <b>[sam] enabled = true</b>. In Java I2P, start the <b>SAM application bridge</b>. On Android, turn on <b>SAM</b> in the i2pd app, where it starts off.<br><br>For a router on another machine, forward its SAM port to this one, for example over SSH.<br><br>Vector trusts whatever answers on this port. Other apps that can reach your router can use Vector's I2P address.",
        ),
        "i2pAuth" => (
            "SAM Password",
            "Only needed if your router asks for one.",
        ),
        "i2pIdentity" => (
            "New Address",
            "Vector gets fresh I2P addresses for this account and reconnects.<br><br>Your relays and other servers see different addresses, both shown under Connection. Servers see the new ones from then on.",
        ),
        "i2pOnly" => (
            "I2P-Only",
            "Vector reaches only I2P relays and servers, plus the I2P addresses you added.<br><br>Other relays and servers stay unreachable while this is on.",
        ),
        "i2pOutproxy" => (
            "Outproxies",
            "Relays and servers outside I2P are reached through an outproxy, tried in this order.<br><br>An outproxy sees which servers you use, and everyone using it shares its IP address.<br><br>It can link everything you reach through it.",
        ),
        "onionAlias" => (
            "Onion Address",
            "A server can also be reachable as a Tor onion service. Vector connects there instead of through a Tor exit.<br><br>Your connection stays encrypted to the server itself.",
        ),
        "i2pAlias" => (
            "I2P Address",
            "A server can also be reachable inside I2P. Vector connects there instead of through an outproxy.<br><br>Your connection stays encrypted to the server itself.",
        ),
        "transport" => (
            "Routing",
            "Choose how Vector reaches relays and servers.<br><br><b>Tor</b> and <b>I2P</b> hide your IP address. Calls and multiplayer Mini Apps ask before connecting outside them.",
        ),
        "autoDownload" => (
            "Auto-Download Media",
            "When enabled, Vector automatically downloads incoming photos, videos, voice messages and files (up to the size limit below).<br><br>Turn this off to keep attachments as previews and download them by hand, one at a time.",
        ),
        "autoDownloadLimit" => (
            "Auto-Download Limit",
            "The largest attachment size Vector will fetch automatically.<br><br>Anything above this waits for you to tap Download. Only applies while Auto-Download Media is on.",
        ),
        "videoQuality" => (
            "Video Quality",
            "How much Vector shrinks the videos you send. Smaller videos send faster and play smoothly on older phones.<br><br><b>Small</b> fits 540p, <b>Balanced</b> and <b>High</b> fit 720p, all at up to 30 fps. A video that is already small enough is sent as it is.<br><br><b>Original</b> never compresses, and the Compress option no longer shows.",
        ),
        "clearStorage" => (
            "Clear Storage",
            "Deletes the downloaded and sent files Vector has cached on this device, to free up space.<br><br>Your messages stay. Attachments can be downloaded again later if they are still available from their sender.",
        ),
        "encryption" => (
            "Local Encryption",
            "Protects your messages and keys if your device is lost or stolen.<br><br>Disabling speeds up app launch but stores data in plain text.",
        ),
        "changePin" => (
            "Change PIN",
            "Your PIN encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new PIN.",
        ),
        "changePassword" => (
            "Change Password",
            "Your password encrypts all local data including messages, keys, and secrets stored on your device. Resetting it will re-encrypt everything with your new password.",
        ),
        "exportAccount" => (
            "Private Keys",
            "Shows your private key and seed phrase. You only need these to back up your account offline or to use it in another Nostr app. To use Vector on another device, use Sign in on Another Device instead.",
        ),
        "transfer" => (
            "Sign in on Another Device",
            "Bring this account to a phone, computer or browser by scanning a code or typing it in. Both devices check a number together and you approve on this device, so your keys never leave it in a form anyone else can read. This device stays signed in.",
        ),
        "updates" => (
            "Updates",
            "This copy of Vector checks for new releases by itself: Check for Updates finds the newest one, then Download Update takes you to it.<br><br>We recommend always running the latest version: every release carries security, privacy and stability fixes.",
        ),
        "beta" => (
            "Beta Updates",
            "Beta builds are <b>release candidates of the next Vector version</b>, offered here before the official release.<br><br>They carry the newest fixes and features with a little less polish, and you'll be moved onto the official build the moment it releases.<br><br>Turning this off on a beta parks you on your current build until the next official release, then you ride stable from there.",
        ),
        "autoGain" => (
            "Automatic Gain",
            "Lifts a quiet microphone so nobody has to shout, and eases off a loud one.",
        ),
        "echoCancel" => (
            "Echo Cancellation",
            "Keeps your speakers out of your microphone, so the other side never hears themselves back.",
        ),
        "noiseSuppress" => (
            "Noise Suppression",
            "Takes down fans, keyboards and hum while you talk.",
        ),
        "direct" => (
            "Direct Connection",
            "Off, calls and Mini Apps reach people through a relay, so they never learn your IP address. On, you connect straight to them for lower latency, and they can see your IP address.",
        ),
        "streamerMode" => (
            "Streamer Mode",
            "Hides the names, pictures and keys of anyone who has not allowed streams, so you can share your screen.",
        ),
        "streamConsent" => (
            "Show me on streams",
            "Lets people using Streamer Mode show your name and picture on their streams.",
        ),
        "streamerWallpapers" => (
            "Hide chat wallpapers",
            "Shows a plain background in chats while you stream, since a wallpaper can give away who you are talking to.",
        ),
        "streamerNotif" => (
            "While streaming, also hide",
            "Hides more of each notification while Streamer Mode is on, on top of Notification Content Privacy.",
        ),
        "players" => (
            "Video Players",
            "Lets YouTube links play right in the chat, from their Web Preview. Nothing loads from YouTube until you press play, and then YouTube sees your IP address like any visit would.<br><br>Players stay off while you use Tor or I2P, since they load outside them.<br><br>This setting follows your account to your other devices.",
        ),
        "advancedMode" => (
            "Advanced Mode",
            "Shows extra detail meant for developers, such as <b>Copy ID</b> on communities, channels and messages, so a bot can be set up to work in just one of them.<br><br>This setting follows your account to your other devices.",
        ),
        "crashLog" => (
            "Logs",
            "Copies error logs and crash details to your clipboard.<br><br>Share with developers when reporting bugs to help diagnose issues.",
        ),
        "voiceModel" => (
            "Vector Voice AI Model",
            "The Vector Voice AI model <b>determines the Quality of your transcriptions.</b><br><br>A larger model will provide more accurate transcriptions & translations, but require more Disk Space, Memory and CPU power to run.",
        ),
        "voiceTranslate" => (
            "Vector Voice Translations",
            "Vector Voice AI can <b>automatically detect non-English languages and translate them in to English text for you.</b><br><br>You can decide whether Vector Voice transcribes in to their native spoken language, or instead translates in to English on your behalf.",
        ),
        "voiceTranscribe" => (
            "Vector Voice Transcriptions",
            "Vector Voice AI can <b>automatically transcribe incoming Voice Messages</b> for immediate reading, without needing to listen.<br><br>You can decide whether Vector Voice transcribes automatically, or if you prefer to transcribe each message explicitly.",
        ),
        "logout" => (
            "Logout",
            "Logout will erase the local database and remove all stored keys. You will lose access to your communities unless you have a backup.",
        ),
        "webPreviews" => {
            // Off Clearnet every preview fetch rides the chosen network, or fails closed.
            let lead = "When enabled, Vector will <b>automatically fetch and display previews</b> for links shared in messages.<br><br>";
            use vector_core::transport::{Kind, prefs, supported};
            let kind = prefs::live()
                .kind()
                .filter(|k| *k != Kind::Clearnet)
                .map(Kind::label);
            let text = match kind {
                Some(label) => format!(
                    "{lead}You have <b>{label}</b> on, so preview fetches route through it. Your IP address stays hidden from the linked sites."
                ),
                None => {
                    let others = supported()
                        .into_iter()
                        .filter(|k| *k != Kind::Clearnet)
                        .map(Kind::label)
                        .collect::<Vec<_>>()
                        .join(" or ");
                    let hint = if others.is_empty() {
                        "<b>Use a VPN</b>".to_string()
                    } else {
                        format!("<b>Use {others}</b> (Privacy, Routing) <b>or a VPN</b>")
                    };
                    format!(
                        "{lead}This may expose your IP address to the linked sites. {hint} if that's a concern."
                    )
                }
            };
            return Some(("Web Previews", text));
        }
        _ => return None,
    };
    Some((title, text.to_string()))
}

pub struct Theme {
    pub value: &'static str,
    pub label: &'static str,
    /// The swatch's stops, top to bottom; Vector wears its logo instead.
    swatch: &'static [u32],
}

pub const THEMES: [Theme; 7] = [
    Theme {
        value: "vector",
        label: "Vector",
        swatch: &[],
    },
    Theme {
        value: "pivx",
        label: "Keep it Purple",
        swatch: &[0x560ec4, 0x1c0244],
    },
    Theme {
        value: "satoshi",
        label: "サトシ",
        swatch: &[0xe5953c, 0xae712c],
    },
    Theme {
        value: "chatstr",
        label: "Chatstr",
        swatch: &[0xb949cd, 0x5949db],
    },
    Theme {
        value: "gifverse",
        label: "Cosmic",
        swatch: &[0xeab1d3, 0xf0febd],
    },
    Theme {
        value: "monero",
        label: "XMR",
        swatch: &[0xed702d, 0xed702d],
    },
    Theme {
        value: "cyberpunk",
        label: "Neon",
        swatch: &[0x8bf2f0, 0xe46aad],
    },
];

pub struct Settings {
    pub cat: &'static str,
    pub search: Entity<TextInput>,
    pub scroll: ScrollHandle,
    pub offers: Offers,
    pub theme: String,
    /// The dropdown open, by its id.
    pub open_select: Option<&'static str>,
    /// Every toggle's value, by its key.
    pub on: std::collections::HashMap<&'static str, bool>,
    /// Toggles flipped while open: only those slide.
    pub flipped: HashSet<&'static str>,
    /// What notifications also hide while streaming, and the base privacy that already hides.
    pub streamer_notif: String,
    pub notif_privacy: String,
    /// `notif_sound` as stored: default, techno, none, or `custom:<path>`.
    pub notif_sound: String,
    /// Custom is chosen but no file is picked yet.
    pub sound_custom: bool,
    /// Bytes by type, once reported.
    pub storage: Option<HashMap<String, u64>>,
    pub donut: Donut,
    /// The auto-download size limit, in bytes.
    pub auto_limit: u64,
    pub clearing: bool,
    /// Kept files moving, as "N of M", and whether out to plain files.
    pub moving: Option<(String, bool)>,
    pub relays: Option<Vec<crate::backend::network::RelayRow>>,
    pub servers: Option<Vec<vector_core::blossom_servers::BlossomServerInfo>>,
    pub clock: Option<vector_core::clock::Status>,
    pub dialog: Option<crate::netdialog::NetDialog>,
    /// The credential Local Encryption uses: `pin` or `password`.
    pub sec_kind: String,
    /// A remote-signer account: the signer's npub and its connection state.
    pub signer: Option<(String, &'static str)>,
    /// The fold over Private Keys.
    pub sec_advanced: bool,
    /// The release check, mirrored from the app's own.
    pub updates: Updates,
    /// Calls: the devices and switches once read, the test's state.
    pub audio: Option<AudioView>,
    /// Each slider's track, from its last paint, and the one being dragged.
    pub sliders: Rc<std::cell::RefCell<HashMap<&'static str, gpui::Bounds<gpui::Pixels>>>>,
    pub dragging: Rc<std::cell::Cell<Option<&'static str>>>,
    /// Blocked users, once loaded.
    pub blocked: Option<Vec<Blocked>>,
    /// Voice: the models and switches once read.
    pub voice: Option<VoiceView>,
    /// Anchors whose NEW pill has been seen.
    pub seen: HashSet<String>,
    /// The anchor a nav click scrolled to, until the reader scrolls.
    pub jumped: Option<&'static str>,
    pub net: crate::routing::Routing,
    /// How each listed relay and server is reached on the network in use.
    pub book: crate::routes::RouteBook,
    /// Laid out for a phone: one card of categories that fold open.
    pub slim: bool,
    pub slim_open: Option<&'static str>,
    /// Categories a search matched that the reader folded shut.
    pub slim_shut: Vec<&'static str>,
    pub slim_scroll: ScrollHandle,
    /// The window is 600px or less: paired controls stack.
    pub narrow: bool,
    /// 520px or less: hero cards stack.
    pub phone: bool,
    pub slim_geo: Rc<std::cell::RefCell<SlimGeo>>,
}

/// Where the phone list's rows and open bodies landed last frame, so a fold can hold the
/// tapped row still and then lift it to show its settings.
#[derive(Default)]
pub struct SlimGeo {
    pub rows: HashMap<&'static str, gpui::Bounds<gpui::Pixels>>,
    pub bodies: HashMap<&'static str, f32>,
    /// A category just opened: lift it once its body has a height.
    pub follow: Option<&'static str>,
    /// `(start, offset y at start, distance)` of the lift under way.
    pub glide: Option<(std::time::Instant, f32, f32)>,
}

impl SlimGeo {
    /// `hold` + `follow`: 320ms, cubic ease-out.
    pub fn step(&mut self, scroll: &ScrollHandle) -> bool {
        let Some((t0, from, by)) = self.glide else { return false };
        let t = (t0.elapsed().as_secs_f32() / 0.32).min(1.);
        let e = 1. - (1. - t).powi(3);
        let x = scroll.offset().x;
        scroll.set_offset(gpui::point(x, px(from - by * e)));
        if t >= 1. {
            self.glide = None;
        }
        t < 1.
    }
}

/// The chart's pointer state: a slice previewed, one picked, a Delete running.
#[derive(Default)]
pub struct Donut {
    pub hover: Option<usize>,
    pub picked: Option<usize>,
    pub busy: bool,
    /// Where it last painted, for hit-testing by angle.
    pub bounds: Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
}

pub struct Blocked {
    pub npub: String,
    pub name: String,
    /// The name is a streamer veil, not one to qualify with the key.
    pub veiled: bool,
    pub avatar: Option<std::path::PathBuf>,
}

pub enum Act {
    /// Copy the Vector link to `category/anchor`.
    CopyPlace(&'static str, &'static str),
    Category(&'static str),
    /// A phone's category row: fold it open or shut.
    Fold(&'static str),
    Anchor(&'static str, &'static str),
    Toggle(&'static str, bool),
    Help(&'static str),
    /// Open or close a dropdown.
    Select(&'static str),
    Pick(&'static str, &'static str),
    Unblock(String),
    CopyLogs,
    Logout,
    AddRelay,
    AddServer,
    OpenRelay(String),
    OpenServer(String),
    DonutHover(Option<usize>),
    DonutPick(usize),
    DonutDelete,
    ClearStorage,
    PreviewSound,
    PickSound,
    ClearSound,
    ChangeCredential,
    ShowKeys,
    Reauthorize,
    Transfer,
    SecAdvanced,
    CheckUpdates,
    Beta(bool),
    /// `mic` or `speaker`, 0..1, while a slider moves.
    Volume(&'static str, f32),
    /// A voice processing switch: `autoGain`, `echoCancel` or `noiseSuppress`.
    AudioSwitch(&'static str, bool),
    Direct(bool),
    MicTest,
    /// `input` or `output`; None is the system default.
    Device(&'static str, Option<String>),
    Link(&'static str),
    /// Voice: fetch the chosen model, stop fetching it, or delete it.
    VoiceDownload,
    VoiceCancel,
    VoiceDelete,
    ClearSearch,
    /// The reader scrolled the page: the spy takes over from a jump.
    Scrolled,
    Net(crate::routing::NetAct),
}

pub type OnAct = Rc<dyn Fn(Act, &mut Window, &mut App)>;

fn click(
    on: &OnAct,
    act: impl Fn() -> Act + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(act(), w, cx)
}

impl Settings {
    /// The release line NEW pills belong to.
    fn line() -> &'static str {
        let v = env!("CARGO_PKG_VERSION");
        let mut parts = v.splitn(3, '.');
        let (major, minor) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        &v[..major.len() + 1 + minor.len()]
    }

    /// Ids flagged NEW: at most three, past which the pill stops meaning "look here".
    pub fn fresh_ids(&self, cats: &[Category]) -> Vec<&'static str> {
        let line = Self::line();
        cats.iter()
            .flat_map(|c| &c.anchors)
            .filter(|a| a.since == Some(line) && !self.seen.contains(a.id))
            .take(3)
            .map(|a| a.id)
            .collect()
    }

    /// The block being read: the last whose top has passed the pane's top.
    pub fn active_anchor(&self, cat: &Category) -> &'static str {
        if let Some(j) = self.jumped {
            return j;
        }
        let last = cat.anchors.len().saturating_sub(1);
        cat.anchors
            .get(self.scroll.top_item().min(last))
            .map(|a| a.id)
            .unwrap_or("")
    }
}

fn click_wheel(on: &OnAct) -> impl Fn(&gpui::ScrollEvent, &mut Window, &mut App) + 'static {
    let on = on.clone();
    move |_, w, cx| on(Act::Scrolled, w, cx)
}

/// The `.neon-toggle` switch.
pub fn toggle(
    id: impl Into<SharedString>,
    on: bool,
    slide: bool,
    click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let id = id.into();
    let knob = |x: f32, white: bool| {
        div()
            .absolute()
            .top(px(2.))
            .left(px(2. + x))
            .size(px(20.))
            .rounded_full()
            .bg(if white { c(0xffffff) } else { c(0x333333) })
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.4),
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
                inset: false,
            }])
    };
    let track = div()
        .id(SharedString::from(format!("{id}-switch")))
        .relative()
        .w(px(50.))
        .h(px(24.))
        .flex_shrink_0()
        .rounded(px(12.))
        .cursor_pointer()
        .on_click(click)
        .bg(if on {
            gpui::linear_gradient(
                90.,
                gpui::linear_color_stop(c(0x33db98), 0.),
                gpui::linear_color_stop(c(0x00ff9d), 1.),
            )
        } else {
            c(0x1a1a1a).into()
        });
    if !slide {
        return track
            .child(knob(if on { 26. } else { 0. }, on))
            .into_any_element();
    }
    track
        .child(
            knob(0., on).with_animation(
                SharedString::from(format!("{id}-{on}")),
                gpui::Animation::new(std::time::Duration::from_millis(300))
                    .with_easing(crate::kit::bezier(0.25, 0.1, 0.25, 1.)),
                move |d, t| d.left(px(2. + 26. * if on { t } else { 1. - t })),
            ),
        )
        .into_any_element()
}

/// The (i) explainer, which only ever opens its popup.
pub fn info(
    id: impl Into<SharedString>,
    click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id.into())
        .size(px(16.))
        .flex_shrink_0()
        .cursor_pointer()
        .on_click(click)
        .child(
            svg()
                .path("icons/info.svg")
                .size_full()
                .text_color(c(0xffffff))
                .opacity(0.5),
        )
}

/// `.toggle-container` in a settings page: a 16px label, its explainer, the switch flush right.
pub fn toggle_row(
    id: &str,
    label: impl Into<SharedString>,
    explain: Option<Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>>,
    switch: AnyElement,
    grouped: bool,
) -> impl IntoElement {
    let mut name = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(16.))
        .line_height(px(19.))
        .text_color(c(0xffffff))
        .child(label.into());
    if let Some(explain) = explain {
        name = name.child(info(SharedString::from(format!("{id}-info")), explain));
    }
    div()
        .min_h(px(36.))
        .mb(px(if grouped { 5. } else { 10. }))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(name)
        .child(switch)
}

/// `.st-group.section-label`. Its 36px top margin collapses with the 5px under the row above.
fn group_label(text: &str, first: bool) -> impl IntoElement {
    div()
        .mt(px(if first { 0. } else { 31. }))
        .mb(px(7.))
        .h(px(14.))
        .flex()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0x8a8a8a))
        .child(crate::kit::spaced(text.to_uppercase(), 11. * 0.09))
}

/// A theme's swatch (or Vector's mark) at `size`.
fn swatch(t: &Theme, size: f32) -> AnyElement {
    if t.swatch.is_empty() {
        return img("icons/vector-mark.svg")
            .size(px(size))
            .flex_shrink_0()
            .into_any_element();
    }
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(c(t.swatch[0]), 0.),
            gpui::linear_color_stop(c(t.swatch[1]), 1.),
        ))
        .into_any_element()
}

/// The theme picker: a `.vselect` whose rows wear each theme's swatch.
fn theme_picker(s: &Settings, on: &OnAct) -> AnyElement {
    let current = THEMES
        .iter()
        .find(|t| t.value == s.theme)
        .unwrap_or(&THEMES[0]);
    let open = s.open_select == Some("theme");
    let caret = svg()
        .path("ui/caret-down.svg")
        .w(px(10.))
        .h(px(6.))
        .flex_shrink_0()
        .text_color(c(0x888888))
        .when(open, |d| {
            d.with_transformation(gpui::Transformation::rotate(gpui::radians(
                std::f32::consts::PI,
            )))
        });
    let button = div()
        .id("theme-picker")
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(36.))
        .px(px(12.))
        .rounded(px(6.))
        .border_1()
        .border_color(c(if open { 0x474747 } else { 0x333333 }))
        .hover(|d| d.border_color(c(0x474747)))
        .bg(c(0x0a0a0a))
        .text_size(px(16.))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .on_click(click(on, || Act::Select("theme")))
        .child(
            div()
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .child(swatch(current, 16.)),
        )
        .child(div().flex_1().min_w_0().truncate().child(current.label))
        .child(caret);
    let mut root = div().relative().w(px(230.)).flex_shrink_0().child(button);
    if open {
        let mut list = div()
            .absolute()
            .top(px(42.))
            .left_0()
            .right_0()
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(c(0x2a2a2a))
            .bg(c(0x111111))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.55),
                offset: gpui::point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude();
        for t in &THEMES {
            let selected = t.value == s.theme;
            let v = t.value;
            list = list.child(
                div()
                    .id(t.value)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_size(px(16.))
                    .text_color(c(if selected { 0xffffff } else { 0xcfcfcf }))
                    .hover(|d| d.bg(c(0x1f1f1f)).text_color(c(0xffffff)))
                    .cursor_pointer()
                    .on_click(click(on, move || Act::Pick("theme", v)))
                    .child(
                        div()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(swatch(t, 16.)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(t.label))
                    .when(selected, |d| {
                        d.child(
                            svg()
                                .path("icons/check.svg")
                                .size(px(14.))
                                .text_color(c(ACCENT)),
                        )
                    }),
            );
        }
        root = root.child(gpui::deferred(list).with_priority(2));
    }
    root.into_any_element()
}

/// A row for the toggle `key`, grouped or not.
fn row(s: &Settings, key: &'static str, on: &OnAct, grouped: bool) -> AnyElement {
    let Some(t) = toggle_of(key) else {
        return div().into_any_element();
    };
    let value = s.on.get(key).copied().unwrap_or(false);
    let help = t.help;
    let switch = toggle(
        SharedString::from(format!("tg-{key}")),
        value,
        s.flipped.contains(key),
        click(on, move || Act::Toggle(key, !value)),
    );
    toggle_row(
        key,
        t.label,
        Some(Box::new(click(on, move || Act::Help(help)))),
        switch,
        grouped,
    )
    .into_any_element()
}

fn display_block(s: &Settings, on: &OnAct) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .child(group_label("Appearance", true))
        .child(
            div()
                .min_h(px(36.))
                .mb(px(5.))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(16.))
                        .line_height(px(19.))
                        .text_color(c(0xffffff))
                        .child("Theme"),
                )
                .child(theme_picker(s, on)),
        );
    let mut last = "";
    let rows: Vec<&Toggle> = if s.slim {
        std::iter::once(&RAIL_HIDDEN).chain(DISPLAY.iter().filter(|t| t.key != "ws_nav")).collect()
    } else {
        DISPLAY.iter().collect()
    };
    for t in rows {
        if t.group != last {
            col = col.child(group_label(t.group, false));
            last = t.group;
        }
        col = col.child(row(s, t.key, on, true));
    }
    col.into_any_element()
}

fn privacy_block(s: &Settings, on: &OnAct) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .children(PRIVACY.iter().map(|t| row(s, t.key, on, false)))
        .into_any_element()
}

/// A `.st-line`: a label, its explainer, and a control flush right.
fn line(
    id: &str,
    label: &'static str,
    help: Option<&'static str>,
    control: AnyElement,
    on: &OnAct,
) -> impl IntoElement {
    let mut name = div()
        .flex()
        .items_center()
        .min_w_0()
        .flex_shrink(1.)
        .gap(px(6.))
        .text_size(px(16.))
        .line_height(px(19.))
        .text_color(c(0xffffff))
        .child(div().min_w_0().flex_shrink(1.).child(label));
    if let Some(help) = help {
        name = name.child(info(
            SharedString::from(format!("{id}-info")),
            click(on, move || Act::Help(help)),
        ));
    }
    div()
        .min_h(px(36.))
        .mb(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(name)
        .child(control)
}

/// Notification privacy as two bits: 1 hides the sender, 2 hides the message.
fn notif_bits(level: &str) -> u8 {
    match level {
        "hide_sender" => 1,
        "hide_content" => 2,
        "hide_all" => 3,
        _ => 0,
    }
}

const STREAMER_LEVELS: [crate::kit::Choice; 4] = [
    crate::kit::Choice {
        value: "none",
        label: "Nothing extra",
    },
    crate::kit::Choice {
        value: "hide_content",
        label: "Messages",
    },
    crate::kit::Choice {
        value: "hide_sender",
        label: "Names",
    },
    crate::kit::Choice {
        value: "hide_all",
        label: "Names and messages",
    },
];

fn streamer_block(s: &Settings, on: &OnAct) -> AnyElement {
    let live = s.on.get("streamer_on").copied().unwrap_or(false);
    let base = notif_bits(&s.notif_privacy);
    let bits = base | notif_bits(&s.streamer_notif);
    let caption = format!(
        "Notifications while streaming: {}, {}",
        if bits & 1 != 0 {
            "name hidden"
        } else {
            "name shown if they allow streams"
        },
        if bits & 2 != 0 {
            "message hidden"
        } else {
            "message shown"
        }
    );
    let disabled: Vec<&'static str> = STREAMER_LEVELS
        .iter()
        .filter(|o| o.value != "none" && notif_bits(o.value) & !base == 0)
        .map(|o| o.value)
        .collect();
    let card = div()
        .mb(px(18.))
        .flex()
        .items_center()
        .when(s.phone, |d| d.flex_col().gap(px(10.)).py(px(16.)).px(px(14.)))
        .when(!s.phone, |d| d.gap(px(16.)).py(px(14.)).px(px(16.)))
        .rounded(px(14.))
        .bg(white_a(0.03))
        .border_1()
        .border_color(if live {
            c(ACCENT).opacity(0.35)
        } else {
            white_a(0.06)
        })
        .child(
            div()
                .size(px(72.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path("icons/video.svg")
                        .size(px(37.44))
                        .text_color(if live { c(ACCENT) } else { white_a(0.35) }),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_center()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(white_a(0.92))
                        .child("Streamer Mode"),
                )
                .child(info(
                    "streamer-card-info",
                    click(on, || Act::Help("streamerMode")),
                )),
        )
        .child(div().mb(px(10.)).child(toggle(
            "tg-streamer_on",
            live,
            s.flipped.contains("streamer_on"),
            click(on, move || Act::Toggle("streamer_on", !live)),
        )));
    let select = div().w(px(180.)).min_w(px(110.)).flex_shrink(1.).child(crate::kit::select_with(
        "streamer-notif",
        f32::INFINITY,
        12.,
        &STREAMER_LEVELS,
        &disabled,
        &s.streamer_notif,
        s.open_select == Some("streamer-notif"),
        {
            let on = on.clone();
            move |w, cx| on(Act::Select("streamer-notif"), w, cx)
        },
        {
            let on = on.clone();
            move |v, w, cx| on(Act::Pick("streamer-notif", v), w, cx)
        },
    ))
    .into_any_element();
    div()
        .flex()
        .flex_col()
        .child(card)
        .child(row(s, "stream_consent", on, false))
        .child(row(s, "streamer_wallpapers", on, false))
        .child(line(
            "streamer-notif",
            "While streaming, also hide",
            Some("streamerNotif"),
            select,
            on,
        ))
        // Its 6px top margin collapses into the row's 10.
        .child(
            div()
                .mb(px(16.))
                .text_size(px(12.))
                .line_height(px(12. * 1.4))
                .text_color(white_a(0.5))
                .child(caption),
        )
        .into_any_element()
}

fn blocked_block(s: &Settings, on: &OnAct) -> AnyElement {
    let Some(list) = &s.blocked else {
        return div().into_any_element();
    };
    if list.is_empty() {
        return div()
            .my(px(13.))
            .text_size(px(13.))
            .text_color(c(0x666666))
            .child("No blocked users")
            .into_any_element();
    }
    div()
        .flex()
        .flex_col()
        .children(list.iter().map(|u| {
            let npub = u.npub.clone();
            let avatar = match &u.avatar {
                Some(p) => img(p.clone())
                    .size(px(30.))
                    .rounded_full()
                    .flex_shrink_0()
                    .into_any_element(),
                None => img("icons/user-placeholder.svg")
                    .size(px(30.))
                    .rounded_full()
                    .flex_shrink_0()
                    .into_any_element(),
            };
            let name: AnyElement = if u.veiled {
                div().child(u.name.clone()).into_any_element()
            } else if !u.name.is_empty() {
                div()
                    .flex()
                    .gap(px(4.))
                    .child(u.name.clone())
                    .child(
                        div()
                            .opacity(0.4)
                            .text_size(px(12.))
                            .child(format!("({})", &u.npub[..8.min(u.npub.len())])),
                    )
                    .into_any_element()
            } else {
                div()
                    .child(format!("{}...", &u.npub[..20.min(u.npub.len())]))
                    .into_any_element()
            };
            div()
                .flex()
                .items_center()
                .justify_between()
                .py(px(8.))
                .px(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .min_w_0()
                        .flex_1()
                        .child(avatar)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(14.))
                                .text_color(c(0xdddddd))
                                .child(name),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("unblock-{}", u.npub)))
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(c(crate::timeline::DANGER))
                        .text_size(px(13.))
                        .text_color(c(crate::timeline::DANGER))
                        .hover(|d| d.bg(c(crate::timeline::DANGER).opacity(0.15)))
                        .cursor_pointer()
                        .on_click(click(on, move || Act::Unblock(npub.clone())))
                        .child("Unblock"),
                )
        }))
        .into_any_element()
}

/// `.st-page .cancel-btn`: the small grey button.
fn small_button(
    id: &'static str,
    label: &'static str,
    act: impl Fn() -> Act + 'static,
    on: &OnAct,
) -> AnyElement {
    div()
        .id(id)
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(c(0x333333))
        .bg(c(0x262626))
        .hover(|d| d.bg(c(0x333333)))
        .text_size(px(12.))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .on_click(click(on, act))
        .child(label)
        .into_any_element()
}

fn danger_button(on: &OnAct) -> AnyElement {
    div()
        .id("logout-btn")
        .h(px(32.))
        .px(px(14.))
        .pt(px(2.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(c(crate::timeline::DANGER))
        .text_size(px(12.))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .hover(|d| d.opacity(0.75))
        .on_click(click(on, || Act::Logout))
        .child(
            svg()
                .path("icons/vector_warning.svg")
                .size(px(18.))
                .mt(px(-2.))
                .text_color(c(crate::timeline::DANGER)),
        )
        .child("Logout")
        .into_any_element()
}

const NOTIF_PRIVACY: [crate::kit::Choice; 4] = [
    crate::kit::Choice {
        value: "full",
        label: "Show sender and message",
    },
    crate::kit::Choice {
        value: "hide_content",
        label: "Hide message",
    },
    crate::kit::Choice {
        value: "hide_sender",
        label: "Hide sender",
    },
    crate::kit::Choice {
        value: "hide_all",
        label: "Hide sender and message",
    },
];

const SOUNDS: [crate::kit::Choice; 4] = [
    crate::kit::Choice {
        value: "default",
        label: "Prélude",
    },
    crate::kit::Choice {
        value: "techno",
        label: "Techno",
    },
    crate::kit::Choice {
        value: "none",
        label: "None",
    },
    crate::kit::Choice {
        value: "custom",
        label: "Custom...",
    },
];

impl Settings {
    /// The sound dropdown's value: Custom while a file is still being picked.
    pub fn sound_choice(&self) -> &'static str {
        match self.notif_sound.as_str() {
            _ if self.sound_custom => "custom",
            "techno" => "techno",
            "none" => "none",
            s if s.starts_with("custom:") => "custom",
            _ => "default",
        }
    }
}

fn dropdown(
    id: &'static str,
    choices: &'static [crate::kit::Choice],
    value: &str,
    s: &Settings,
    on: &OnAct,
) -> AnyElement {
    dropdown_w(id, 230., choices, value, s, on)
}

/// 230px, giving way when the row runs short (`.vselect` shrinks like any flex item).
fn dropdown_w(
    id: &'static str,
    width: f32,
    choices: &'static [crate::kit::Choice],
    value: &str,
    s: &Settings,
    on: &OnAct,
) -> AnyElement {
    div().w(px(width)).min_w(px(120.)).flex_shrink(1.).child(crate::kit::select_with(
        id,
        f32::INFINITY,
        12.,
        choices,
        &[],
        value,
        s.open_select == Some(id),
        {
            let on = on.clone();
            move |w, cx| on(Act::Select(id), w, cx)
        },
        {
            let on = on.clone();
            move |v, w, cx| on(Act::Pick(id, v), w, cx)
        },
    ))
    .into_any_element()
}

fn notifications_block(s: &Settings, on: &OnAct) -> AnyElement {
    let privacy = dropdown("notif-privacy", &NOTIF_PRIVACY, &s.notif_privacy, s, on);
    let choice = s.sound_choice();
    let preview = div()
        .id("notif-preview")
        .tooltip(crate::kit::tooltip("Preview Sound"))
        .w(px(28.))
        .h(px(36.))
        .flex()
        .items_center()
        .justify_center()
        .opacity(0.75)
        .hover(|d| d.opacity(1.))
        .cursor_pointer()
        .on_click(click(on, || Act::PreviewSound))
        .child(
            svg()
                .path("icons/speaker_volume.svg")
                .size(px(18.))
                .text_color(c(0xffffff)),
        );
    let mut col = div()
        .flex()
        .flex_col()
        .children(NOTIFY.iter().map(|t| row(s, t.key, on, false)))
        .child(
            // `#notif-privacy-group` keeps the form group's 15px.
            div()
                .min_h(px(36.))
                .mb(px(15.))
                .flex()
                .when(s.narrow, |d| d.flex_col().items_stretch())
                .when(!s.narrow, |d| d.items_center().justify_between())
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(16.))
                        .line_height(px(19.))
                        .text_color(c(0xffffff))
                        .child("Content Privacy")
                        .child(info(
                            "notif-privacy-info",
                            click(on, || Act::Help("notifPrivacy")),
                        )),
                )
                .child(privacy),
        )
        .child(
            div()
                .min_h(px(36.))
                .when(choice == "custom", |d| d.mb(px(15.)))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex_grow(1.)
                        .flex_shrink_0()
                        .text_size(px(16.))
                        .line_height(px(24.))
                        .text_color(c(0xffffff))
                        .child("Notification Sound"),
                )
                .child(preview)
                .child(dropdown("notif-sound", &SOUNDS, choice, s, on)),
        );
    if choice == "custom" {
        let name = s
            .notif_sound
            .strip_prefix("custom:")
            .map(|p| {
                let file = std::path::Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Unknown file".into());
                // `name_48000.raw` is the cache form; show the name as picked.
                match file
                    .strip_suffix(".raw")
                    .and_then(|f| f.rsplit_once('_'))
                    .filter(|(_, rate)| rate.bytes().all(|b| b.is_ascii_digit()))
                {
                    Some((stem, _)) => stem.to_string(),
                    None => file,
                }
            })
            .unwrap_or_else(|| "No file selected".into());
        // `.notif-custom-container`: its 20px top margin swallows the row's 15.
        col = col.child(
            div()
                .id("notif-sound-chip")
                .tooltip(crate::kit::tooltip("Click to change sound"))
                .mt(px(5.))
                .mb(px(15.))
                .flex()
                .items_center()
                .gap(px(10.))
                .py(px(12.))
                .pl(px(15.))
                .pr(px(12.))
                .rounded(px(25.))
                .border_1()
                .border_color(white_a(0.12))
                .bg(white_a(0.08))
                .hover(|d| d.bg(white_a(0.12)).border_color(white_a(0.2)))
                .cursor_pointer()
                .on_click(click(on, || Act::PickSound))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(20.))
                        .opacity(0.7)
                        .text_color(c(0xffffff))
                        .child("♪"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(14.))
                        .text_color(white_a(0.85))
                        .child(name),
                )
                .child(
                    div()
                        .id("notif-sound-clear")
                        .tooltip(crate::kit::tooltip("Remove custom sound"))
                        .size(px(20.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(white_a(0.15))
                        .text_size(px(14.))
                        .text_color(white_a(0.7))
                        .hover(|d| {
                            d.bg(c(crate::timeline::DANGER).opacity(0.1))
                                .text_color(c(crate::timeline::DANGER))
                        })
                        .on_click({
                            let on = on.clone();
                            move |_, w, cx| {
                                cx.stop_propagation();
                                on(Act::ClearSound, w, cx)
                            }
                        })
                        .child("×"),
                ),
        );
    }
    col.into_any_element()
}

pub struct StoreCat {
    pub name: &'static str,
    pub exts: &'static [&'static str],
    /// A synthetic bucket: `/cache` or `/ai_models`.
    pub key: Option<&'static str>,
    pub title: &'static str,
    pub noun: &'static str,
}

pub const STORE_CATS: [StoreCat; 7] = [
    StoreCat {
        name: "Images",
        exts: &[
            "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "avif", "heic", "heif", "tif",
            "tiff", "ico",
        ],
        key: None,
        title: "Delete all Images?",
        noun: "downloaded images",
    },
    StoreCat {
        name: "Video",
        exts: &[
            "mp4", "mov", "avi", "mkv", "flv", "wmv", "3gp", "webm", "m4v", "mpeg", "mpg",
        ],
        key: None,
        title: "Delete all Videos?",
        noun: "downloaded videos",
    },
    StoreCat {
        name: "Audio",
        exts: &[
            "mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac", "weba", "wma", "aiff",
        ],
        key: None,
        title: "Delete all Audio?",
        noun: "downloaded audio and voice messages",
    },
    StoreCat {
        name: "Apps",
        exts: &["xdc", "jsdos"],
        key: None,
        title: "Delete all Mini Apps?",
        noun: "downloaded Mini Apps",
    },
    StoreCat {
        name: "AI",
        exts: &[],
        key: Some("/ai_models"),
        title: "Delete AI Models?",
        noun: "",
    },
    StoreCat {
        name: "Cache",
        exts: &[],
        key: Some("/cache"),
        title: "Clear the Cache?",
        noun: "",
    },
    StoreCat {
        name: "Files",
        exts: &[],
        key: None,
        title: "Delete all Files?",
        noun: "downloaded files",
    },
];

/// Slice colours rank by size: the largest is always purple.
const RAMP: [u32; 7] = [
    0x9d5df9, 0x5ec4f7, 0x4ad99d, 0xfba35b, 0xfbc85b, 0xfc595c, 0xb2b2b2,
];
const R_OUT: f32 = 96.;
const R_IN: f32 = 57.;
const GAP: f32 = 5.;
/// Slivers keep this floor so they stay visible, paid for by the largest slice.
const MIN_SWEEP: f32 = 0.13;

pub struct Segment {
    pub cat: usize,
    pub size: u64,
    pub color: u32,
    pub a0: f32,
    pub a1: f32,
    pub pct: f32,
}

fn owner(key: &str) -> usize {
    STORE_CATS
        .iter()
        .position(|c| c.key == Some(key) || c.exts.contains(&key))
        .unwrap_or(STORE_CATS.len() - 1)
}

pub fn segments(dist: &HashMap<String, u64>) -> Vec<Segment> {
    let mut sizes = [0u64; 7];
    for (k, b) in dist {
        sizes[owner(k)] += b;
    }
    let mut segs: Vec<(usize, u64)> = sizes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > 0)
        .map(|(i, s)| (i, *s))
        .collect();
    segs.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u64 = segs.iter().map(|s| s.1).sum();
    let mut stolen = 0.;
    let mut sweeps: Vec<f32> = segs
        .iter()
        .map(|(_, s)| {
            let a = TAU * (*s as f32 / total as f32);
            if a < MIN_SWEEP {
                stolen += MIN_SWEEP - a;
                MIN_SWEEP
            } else {
                a
            }
        })
        .collect();
    if let Some(first) = sweeps.first_mut() {
        *first -= stolen;
    }
    let mut angle = 0.;
    segs.iter()
        .enumerate()
        .map(|(i, (cat, size))| {
            let a0 = angle;
            angle += sweeps[i];
            Segment {
                cat: *cat,
                size: *size,
                color: RAMP[i.min(RAMP.len() - 1)],
                a0,
                a1: angle,
                pct: *size as f32 / total as f32 * 100.,
            }
        })
        .collect()
}

/// The extensions a slice clears: its own, or for the rest bucket, exactly those it counted.
pub fn slice_exts(cat: usize, dist: &HashMap<String, u64>) -> Vec<String> {
    let c = &STORE_CATS[cat];
    if !c.exts.is_empty() {
        return c.exts.iter().map(|e| e.to_string()).collect();
    }
    dist.keys().filter(|k| owner(k) == cat).cloned().collect()
}

/// The slice under a window point, by angle, so a gap belongs to its neighbour.
pub fn slice_at(
    bounds: gpui::Bounds<gpui::Pixels>,
    at: gpui::Point<gpui::Pixels>,
    segs: &[Segment],
) -> Option<usize> {
    let scale = 200. / f32::from(bounds.size.width).max(1.);
    let vx = f32::from(at.x - bounds.origin.x) * scale - 100.;
    let vy = f32::from(at.y - bounds.origin.y) * scale - 100.;
    let dist = vx.hypot(vy);
    if dist < R_IN - 2. || dist > R_OUT + 6. || segs.is_empty() {
        return None;
    }
    let mut a = vx.atan2(-vy);
    if a < 0. {
        a += TAU;
    }
    Some(segs.iter().rposition(|s| a >= s.a0).unwrap_or(0))
}

/// Binary units, with three whole digits trimmed to none after the point: the ring is full.
fn ring_size(n: u64) -> String {
    let s = crate::media::format_bytes(n);
    if s.split(' ')
        .next()
        .and_then(|v| v.parse::<f32>().ok())
        .is_some_and(|v| v >= 100.)
    {
        crate::media::format_bytes_with(n as f64, 0, false)
    } else {
        s
    }
}

fn donut(s: &Settings, on: &OnAct) -> AnyElement {
    let dist = s.storage.clone().unwrap_or_default();
    let segs = segments(&dist);
    let total: u64 = segs.iter().map(|g| g.size).sum();
    let view = s.donut.hover.or(s.donut.picked).filter(|i| *i < segs.len());
    let delete = s.donut.picked.is_some() && view == s.donut.picked;
    let (value, label) = match view {
        Some(i) => {
            let g = &segs[i];
            let pct = if g.pct < 1. {
                "<1".to_string()
            } else {
                format!("{}", g.pct.round() as u32)
            };
            (
                ring_size(g.size),
                format!("{} · {pct}%", STORE_CATS[g.cat].name),
            )
        }
        None => (ring_size(total), "Total".into()),
    };
    let paint: Vec<(f32, f32, u32, bool, bool)> = segs
        .iter()
        .enumerate()
        .map(|(i, g)| {
            (
                g.a0,
                g.a1,
                g.color,
                view == Some(i),
                view.is_some() && view != Some(i),
            )
        })
        .collect();
    let loaded = s.storage.is_some();
    let cell = s.donut.bounds.clone();
    let chart = canvas(
        move |bounds, _, _| cell.set(Some(bounds)),
        move |bounds: gpui::Bounds<gpui::Pixels>, _, window, _| {
            let k = f32::from(bounds.size.width) / 200.;
            let o = bounds.origin;
            let ring =
                |a0: f32, a1: f32, gap: bool, pop: f32| -> Option<gpui::Path<gpui::Pixels>> {
                    let (g_out, g_in) = if gap {
                        (GAP / 2. / R_OUT, GAP / 2. / R_IN)
                    } else {
                        (0., 0.)
                    };
                    let at = |r: f32, a: f32| {
                        gpui::point(
                            o.x + px((100. + r * pop * a.sin()) * k),
                            o.y + px((100. - r * pop * a.cos()) * k),
                        )
                    };
                    let arc = |r: f32, from: f32, to: f32| -> Vec<gpui::Point<gpui::Pixels>> {
                        let steps = (((to - from).abs() / TAU) * 128.).ceil().max(2.) as usize;
                        (0..=steps)
                            .map(|j| at(r, from + (to - from) * j as f32 / steps as f32))
                            .collect()
                    };
                    let mut pts = arc(R_OUT, a0 + g_out, a1 - g_out);
                    let mut inner = arc(R_IN, a0 + g_in, a1 - g_in);
                    inner.reverse();
                    pts.extend(inner);
                    let mut b = PathBuilder::fill();
                    b.add_polygon(&pts, true);
                    b.build().ok()
                };
            if !loaded {
                return;
            }
            if paint.is_empty() {
                if let Some(p) = ring(0., TAU, false, 1.) {
                    window.paint_path(p, white_a(0.07));
                }
                return;
            }
            let lone = paint.len() == 1;
            for (a0, a1, color, pop, dim) in &paint {
                let pop_k = if *pop { 1.05 } else { 1. };
                let mut col = c(*color);
                if *pop {
                    col.l = (col.l * 1.12).min(1.);
                }
                if *dim {
                    col = col.opacity(0.35);
                }
                if let Some(p) = ring(*a0, if lone { TAU } else { *a1 }, !lone, pop_k) {
                    window.paint_path(p, col);
                }
            }
        },
    )
    .size_full();
    let hovering = view.is_some() && s.donut.hover.is_some();
    let hover = s.donut.hover;
    let bounds = s.donut.bounds.clone();
    let seg_angles: Vec<Segment> = segments(&dist);
    let seg_click: Vec<Segment> = segments(&dist);
    let ring_el = div()
        .id("storage-donut")
        .size(px(220.))
        .flex_shrink_0()
        .relative()
        .when(hovering, |d| d.cursor_pointer())
        .on_mouse_move({
            let on = on.clone();
            let bounds = bounds.clone();
            move |e: &gpui::MouseMoveEvent, w, cx| {
                let at = bounds
                    .get()
                    .and_then(|b| slice_at(b, e.position, &seg_angles));
                if at != hover {
                    on(Act::DonutHover(at), w, cx);
                }
            }
        })
        .on_hover({
            let on = on.clone();
            move |inside: &bool, w, cx| {
                if !*inside {
                    on(Act::DonutHover(None), w, cx)
                }
            }
        })
        .on_click({
            let on = on.clone();
            move |e: &gpui::ClickEvent, w, cx| {
                if let Some(i) = bounds
                    .get()
                    .and_then(|b| slice_at(b, e.position(), &seg_click))
                {
                    on(Act::DonutPick(i), w, cx)
                }
            }
        })
        .child(chart)
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(24.))
                        .line_height(px(28.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xffffff))
                        .child(value),
                )
                .when(!delete, |d| {
                    d.child(
                        div()
                            .mt(px(1.))
                            .max_w(px(100.))
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .text_color(c(0x5b6060))
                            .child(label),
                    )
                })
                .when(delete, |d| {
                    let danger = c(crate::timeline::DANGER);
                    d.child(
                        div()
                            .id("storage-donut-delete")
                            .mt(px(5.))
                            .px(px(14.))
                            .py(px(4.))
                            .rounded_full()
                            .border_1()
                            .border_color(danger.opacity(0.55))
                            .bg(danger.opacity(0.18))
                            .hover(|h| h.bg(danger.opacity(0.4)).border_color(danger))
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(0xffffff))
                            .when(s.donut.busy, |b| b.opacity(0.5))
                            .when(!s.donut.busy, |b| {
                                b.cursor_pointer().on_click(click(on, || Act::DonutDelete))
                            })
                            .child(if s.donut.busy {
                                "Deleting..."
                            } else {
                                "Delete"
                            }),
                    )
                }),
        );
    // Four rows a column, filled top to bottom.
    let legend = div()
        .flex()
        .gap(px(40.))
        .children(segs.chunks(4).enumerate().map(|(col, chunk)| {
            div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .children(chunk.iter().enumerate().map(|(row, g)| {
                    let i = col * 4 + row;
                    let on_enter = on.clone();
                    div()
                        .id(SharedString::from(format!("legend-{i}")))
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .text_size(px(16.))
                        .text_color(c(0xffffff))
                        .when(view.is_some() && view != Some(i), |d| d.opacity(0.4))
                        .on_hover(move |inside: &bool, w, cx| {
                            on_enter(Act::DonutHover(if *inside { Some(i) } else { None }), w, cx)
                        })
                        .on_click(click(on, move || Act::DonutPick(i)))
                        .child(
                            div()
                                .size(px(14.))
                                .rounded(px(4.))
                                .flex_shrink_0()
                                .bg(c(g.color)),
                        )
                        .child(STORE_CATS[g.cat].name)
                }))
        }));
    div()
        .mb(px(20.))
        .pb(px(28.))
        .border_b_1()
        .border_color(c(0x262626))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(48.))
        .child(ring_el)
        .child(legend)
        .into_any_element()
}

/// The auto-download limits on offer.
const LIMITS: [crate::kit::Choice; 6] = [
    crate::kit::Choice {
        value: "1048576",
        label: "1 MB",
    },
    crate::kit::Choice {
        value: "5242880",
        label: "5 MB",
    },
    crate::kit::Choice {
        value: "10485760",
        label: "10 MB",
    },
    crate::kit::Choice {
        value: "26214400",
        label: "25 MB",
    },
    crate::kit::Choice {
        value: "52428800",
        label: "50 MB",
    },
    crate::kit::Choice {
        value: "104857600",
        label: "100 MB",
    },
];

fn storage_block(s: &Settings, on: &OnAct) -> AnyElement {
    let auto = s.on.get("auto_download_enabled").copied().unwrap_or(true);
    let limit = s.auto_limit.to_string();
    let select = crate::kit::select_with(
        "auto-limit",
        120.,
        12.,
        &LIMITS,
        &[],
        &limit,
        auto && s.open_select == Some("auto-limit"),
        {
            let on = on.clone();
            move |w, cx| on(Act::Select("auto-limit"), w, cx)
        },
        {
            let on = on.clone();
            move |v, w, cx| on(Act::Pick("auto-limit", v), w, cx)
        },
    );
    let clear = small_button(
        "clear-storage",
        if s.clearing { "Clearing..." } else { "Clear" },
        || Act::ClearStorage,
        on,
    );
    div()
        .flex()
        .flex_col()
        .child(donut(s, on))
        .child(row(s, "auto_download_enabled", on, false))
        .child(
            div()
                .when(!auto, |d| d.opacity(0.45).block_mouse_except_scroll())
                .child(line(
                    "auto-limit",
                    "Auto-Download Limit",
                    Some("autoDownloadLimit"),
                    select,
                    on,
                )),
        )
        // The next group's 15px top margin outweighs the line's 10.
        .child(div().mt(px(5.)).child(row(s, "keep_media", on, false)))
        .when(shows_plain(s), |d| d.child(div().mt(px(5.)).child(row(s, "keep_unencrypted", on, false))))
        .when_some(s.moving.as_ref(), |d, (count, plain)| {
            d.child(line(
                "moving",
                if *plain { "Decrypting your kept files" } else { "Moving your files into Vector" },
                None,
                div().child(count.clone()).into_any_element(),
                on,
            ))
        })
        .child(line(
            "clear",
            "Clear Storage",
            Some("clearStorage"),
            clear,
            on,
        ))
        .into_any_element()
}

/// Keep Unencrypted shows under Keep Files, for Advanced Mode or once it is on.
fn shows_plain(s: &Settings) -> bool {
    let on = |k| s.on.get(k).copied().unwrap_or(false);
    on("keep_media") && (on("advanced") || on("keep_unencrypted"))
}

/// `.st-btn`: the small grey button.
fn st_btn(
    id: &'static str,
    label: &'static str,
    act: impl Fn() -> Act + 'static,
    on: &OnAct,
) -> AnyElement {
    small_button(id, label, act, on)
}

/// `.st-row`: a label with its explainer, an action flush right.
fn st_row(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    action: AnyElement,
    on: &OnAct,
) -> AnyElement {
    div()
        .min_h(px(36.))
        .mb(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(16.))
                .line_height(px(24.))
                .text_color(c(0xffffff))
                .child(label)
                .child(info(
                    SharedString::from(format!("{id}-info")),
                    click(on, move || Act::Help(help)),
                )),
        )
        .child(action)
        .into_any_element()
}

/// A connection's state in words, with its icon: `.st-page .relay-status`.
pub fn relay_status(tone: &str, text: impl Into<SharedString>) -> AnyElement {
    let (color, icon) = match tone {
        "connected" => (c(ACCENT), "icons/check-circle.svg"),
        "disconnected" | "terminated" | "banned" => {
            (c(crate::timeline::DANGER), "icons/cancel.svg")
        }
        "disabled" => (c(0xb2b2b2), "icons/disable.svg"),
        "connecting" => (c(0xffc107), "icons/clock.svg"),
        "pending" => (c(0x9c27b0), "icons/clock.svg"),
        "sleeping" => (c(0x6495ed), "icons/clock.svg"),
        _ => (c(0x808080), "icons/clock.svg"),
    };
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        .text_size(px(16.))
        .line_height(px(19.))
        .text_color(color)
        .child(
            svg()
                .path(icon)
                .size(px(16.))
                .flex_shrink_0()
                .text_color(color),
        )
        .child(text.into())
        .into_any_element()
}

/// The relay list's word for a socket state.
pub fn said(status: &str) -> String {
    if status == "disconnected" {
        return "Not Connected".into();
    }
    let mut ch = status.chars();
    ch.next()
        .map(|f| f.to_uppercase().collect::<String>() + ch.as_str())
        .unwrap_or_default()
}

/// Where a row came from: `default` outlined, `custom` filled.
fn origin_pill(default: bool) -> impl IntoElement {
    div()
        .min_w(px(64.))
        .h(px(32.))
        .px(px(10.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .when(default, |d| d.border_color(c(0x474747)))
        .when(!default, |d| d.border_color(c(0x333333)).bg(c(0x333333)))
        .text_size(px(12.))
        .text_color(c(0xb2b2b2))
        .child(if default { "Default" } else { "Custom" })
}

pub fn host(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.trim_end_matches('/').to_string()
}

/// `.st-page .relay-item`: flat, full width past the column's edges, lit on hover.
fn net_row(
    id: SharedString,
    enabled: bool,
    act: impl Fn() -> Act + 'static,
    on: &OnAct,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .mx(px(-8.))
        .mb(px(2.))
        .h(px(44.))
        .px(px(8.))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|d| d.bg(c(0x171717)))
        .when(!enabled, |d| d.opacity(0.55))
        .on_click(click(on, act))
}

fn relays_block(s: &Settings, on: &OnAct) -> AnyElement {
    let mut col = div().flex().flex_col().child(st_row(
        "relays",
        "Add Custom Relay",
        "relays",
        st_btn("add-relay", "Add Relay", || Act::AddRelay, on),
        on,
    ));
    for r in s.relays.iter().flatten() {
        let mut content = div()
            .map(|d| if s.phone { d.flex_grow(1.).flex_shrink(1.).flex_basis(gpui::relative(0.4)).min_w(gpui::relative(0.4)) } else { d.flex_1().min_w_0() })
            .flex()
            .items_center()
            .gap(px(12.));
        if !r.is_default && r.mode != "both" {
            content = content.child(
                div()
                    .px(px(6.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(ca(139, 92, 246, 0.2))
                    .text_size(px(10.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(0x8b5cf6))
                    .child(if r.mode == "read" { "R" } else { "W" }),
            );
        }
        let info = crate::relayinfo::get(&r.url);
        content = content
            .child(origin_pill(r.is_default))
            .child(
                div()
                    .size(px(22.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match info.as_ref().and_then(|m| m.icon.clone()) {
                        Some(pic) => gpui::img(pic)
                            .size_full()
                            .rounded(px(5.))
                            .object_fit(gpui::ObjectFit::Cover)
                            .into_any_element(),
                        None => svg()
                            .path("icons/globe.svg")
                            .size(px(16.))
                            .text_color(c(0x7a7a7a))
                            .into_any_element(),
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .mr(px(15.))
                    .text_size(px(16.))
                    .line_height(px(24.))
                    .text_color(c(0xffffff))
                    .child(info.and_then(|m| m.name).unwrap_or_else(|| host(&r.url))),
            );
        col = col.child(
            net_row(
                SharedString::from(format!("relay-{}", r.url)),
                r.enabled,
                {
                    let u = r.url.clone();
                    move || Act::OpenRelay(u.clone())
                },
                on,
            )
            .child(content)
            .child({
                let right = div().ml(px(10.)).flex().items_center();
                match s.book.tag(&r.url).filter(|_| r.enabled) {
                    Some(t) => right.child(crate::routes::tag_status(&t, s.phone)),
                    None => right
                        .children(s.book.badge(&r.url).filter(|_| r.enabled).map(|b| div().mr(px(8.)).child(crate::routes::badge(&b, s.phone))))
                        .child(relay_status(r.status, said(r.status))),
                }
            }),
        );
    }
    col.into_any_element()
}

fn servers_block(s: &Settings, on: &OnAct) -> AnyElement {
    let mut col = div().flex().flex_col().child(st_row(
        "servers",
        "Add Custom Server",
        "servers",
        st_btn("add-server", "Add Server", || Act::AddServer, on),
        on,
    ));
    for sv in s.servers.iter().flatten() {
        let content = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(origin_pill(sv.is_default))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .mr(px(15.))
                    .text_size(px(16.))
                    .line_height(px(24.))
                    .text_color(c(0xffffff))
                    .child(host(&sv.url)),
            );
        let mut right = div().flex().items_center();
        let tag = s.book.tag(&sv.url).filter(|_| sv.enabled);
        if let Some(t) = &tag {
            right = right.child(crate::routes::tag_status(t, s.phone));
        } else if let Some(b) = s.book.badge(&sv.url).filter(|_| sv.enabled) {
            right = right.child(crate::routes::badge(&b, s.phone));
        }
        if let Some(ms) = sv.status.latency_ms.filter(|_| sv.enabled && tag.is_none()) {
            right = right.child(
                div()
                    .mr(px(8.))
                    .text_size(px(11.))
                    .line_height(px(24.))
                    .opacity(0.45)
                    .text_color(c(0xffffff))
                    .child(format!("{ms} ms")),
            );
        }
        if tag.is_none() {
            right = right.child(relay_status(sv.status.tone, sv.status.label));
        }
        col = col.child(
            net_row(
                SharedString::from(format!("server-{}", sv.url)),
                sv.enabled,
                {
                    let u = sv.url.clone();
                    move || Act::OpenServer(u.clone())
                },
                on,
            )
            .child(content)
            .child(right),
        );
    }
    col.into_any_element()
}

fn span(ms: i64) -> String {
    let s = (ms.unsigned_abs() as f64 / 1000.).round() as u64;
    let (n, unit) = if s < 90 {
        (s, "second")
    } else if s < 5400 {
        ((s as f64 / 60.).round() as u64, "minute")
    } else {
        ((s as f64 / 3600.).round() as u64, "hour")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

fn clock_block(s: &Settings, on: &OnAct) -> AnyElement {
    let note = match &s.clock {
        Some(c) if c.checked_at == 0 => "Checking the time with your relays…".to_string(),
        None => "Checking the time with your relays…".to_string(),
        Some(c) if c.agreeing < 3 => {
            "Fewer than three of your relays agreed on the time, so this device’s clock is in use."
                .into()
        }
        Some(c) if c.measured_offset_ms == 0 => "This device’s clock matches your relays.".into(),
        Some(c) => {
            let apart = format!(
                "{} {}",
                span(c.measured_offset_ms),
                if c.measured_offset_ms > 0 {
                    "slow"
                } else {
                    "fast"
                }
            );
            if c.decentralised {
                format!("This device’s clock is {apart}. Vector is using your relays’ time.")
            } else {
                format!("Your relays say this device’s clock is {apart}.")
            }
        }
    };
    div()
        .flex()
        .flex_col()
        .child(row(s, "clock_decentralised", on, false))
        // `.notif-note`: its -4px pulls it under the row's margin.
        .child(
            div()
                .mt(px(-4.))
                .mb(px(14.))
                .text_size(px(13.))
                .line_height(px(17.))
                .text_color(c(0x8a8a8a))
                .child(note),
        )
        .into_any_element()
}

/// `.sec-advanced-toggle`: the fold that keeps the raw keys out of the way.
fn advanced_fold(open: bool, on: &OnAct) -> AnyElement {
    div()
        .id("sec-advanced")
        .w_full()
        .h(px(33.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(0xb2b2b2))
        .hover(|d| d.text_color(c(0xffffff)))
        .cursor_pointer()
        .on_click(click(on, || Act::SecAdvanced))
        .child(
            svg()
                .path("icons/chevron-down.svg")
                .size(px(14.))
                .text_color(c(0xb2b2b2))
                .when(open, |d| {
                    d.with_transformation(gpui::Transformation::rotate(gpui::radians(
                        std::f32::consts::PI,
                    )))
                }),
        )
        .child("Advanced")
        .into_any_element()
}

/// `.remote-signer-card`: who signs for this account, and a way to re-pair it.
fn signer_card(npub: &str, dot: &str, on: &OnAct) -> AnyElement {
    let short = if npub.len() > 18 { format!("{}…{}", &npub[..12], &npub[npub.len() - 6..]) } else { npub.to_string() };
    let (fill, glow) = match dot {
        "online" => (c(0x2ecc71), Some(ca(46, 204, 113, 0.55))),
        "offline" => (c(0xe74c3c), Some(ca(231, 76, 60, 0.45))),
        "connecting" => (c(0xf1c40f), Some(ca(241, 196, 15, 0.6))),
        _ => (c(0x9aa0a6), None),
    };
    let dot = div().size(px(8.)).flex_none().rounded_full().bg(fill).when_some(glow, |d, g| {
        d.shadow(vec![gpui::BoxShadow { color: g, offset: gpui::point(px(0.), px(0.)), blur_radius: px(6.), spread_radius: px(0.), inset: false }])
    });
    // Its top margin collapses into the heading's.
    div()
        .mb(px(16.))
        .py(px(12.))
        .px(px(14.))
        .rounded(px(10.))
        .bg(white_a(0.03))
        .border_1()
        .border_color(white_a(0.08))
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(15.2))
                        .child(dot)
                        .child("Remote Signer"),
                )
                .child(
                    st_btn("signer-reauth", "Re-authorize", || Act::Reauthorize, on),
                ),
        )
        .child(
            div()
                .mt(px(10.))
                .flex()
                .items_baseline()
                .gap(px(10.))
                .text_size(px(13.6))
                .line_height(px(24.))
                .child(div().w(px(70.)).flex_none().text_color(white_a(0.45)).child("Signer key"))
                .child(
                    div()
                        .flex_1()
                        .font_family(crate::markdown::MONO)
                        .text_size(px(10.6))
                        .text_color(white_a(0.85))
                        .child(short),
                ),
        )
        .child(
            div()
                .mt(px(10.))
                .text_size(px(12.5))
                .line_height(px(17.5))
                .text_color(white_a(0.45))
                .child("Your identity key lives on your signer app. Vector only holds a device pairing key."),
        )
        .into_any_element()
}

fn security_block(s: &Settings, on: &OnAct) -> AnyElement {
    let enabled = s.on.get("encryption").copied().unwrap_or(false);
    let switch = toggle(
        "tg-encryption",
        enabled,
        s.flipped.contains("encryption"),
        click(on, move || Act::Toggle("encryption", !enabled)),
    );
    let mut col = div()
        .flex()
        .flex_col()
        .children(s.signer.as_ref().map(|(npub, dot)| signer_card(npub, dot, on)))
        .child(toggle_row(
        "encryption",
        "Local Encryption",
        Some(Box::new(click(on, || Act::Help("encryption")))),
        switch,
        false,
    ));
    if enabled {
        let (label, help) = if s.sec_kind == "password" {
            ("Change Password", "changePassword")
        } else {
            ("Change PIN", "changePin")
        };
        col = col.child(st_row(
            "change-cred",
            label,
            help,
            st_btn("change-cred-btn", "Change", || Act::ChangeCredential, on),
            on,
        ));
    }
    // An external signer keeps the identity key off this device, so Export and transfer go.
    if s.signer.is_some() {
        return col.into_any_element();
    }
    col = col.child(st_row(
        "transfer",
        "Sign in on Another Device",
        "transfer",
        st_btn("transfer-btn", "Start", || Act::Transfer, on),
        on,
    ));
    col = col.child(advanced_fold(s.sec_advanced, on));
    if s.sec_advanced {
        col = col.child(st_row(
            "keys",
            "Private Keys",
            "exportAccount",
            st_btn("keys-btn", "Show", || Act::ShowKeys, on),
            on,
        ));
    }
    col.into_any_element()
}

#[derive(Clone, Default, PartialEq)]
pub enum UpdPhase {
    #[default]
    Idle,
    Checking,
    /// A manual check that found nothing; it fades back to idle.
    NoUpdates,
    Available,
    Error(String),
}

#[derive(Clone, Default)]
pub struct Updates {
    pub phase: UpdPhase,
    /// A check found nothing newer: outlives its status line until the next check.
    pub latest: bool,
    pub found: Option<(String, String)>,
    pub beta: bool,
}

/// `.upd-card`: the standing, its lead coloured.
fn upd_card(lead: Option<(&str, gpui::Hsla)>, rest: &str) -> AnyElement {
    let mut text = String::new();
    let mut runs = Vec::new();
    if let Some((lead, color)) = lead {
        text.push_str(lead);
        runs.push((
            0..lead.len(),
            gpui::HighlightStyle {
                color: Some(color),
                ..Default::default()
            },
        ));
        text.push(' ');
    }
    text.push_str(rest);
    div()
        .w_full()
        .mb(px(16.))
        .px(px(20.))
        .py(px(14.))
        .border_1()
        .border_color(c(0x2c2c2c))
        .rounded(px(8.))
        .text_size(px(14.))
        .line_height(px(20.3))
        .text_color(c(0xb2b2b2))
        .child(gpui::StyledText::new(text).with_highlights(runs))
        .into_any_element()
}

/// `Updates.svelte`: the version and its standing, what a check found, the one action.
fn updates_block(s: &Settings, on: &OnAct) -> AnyElement {
    let u = &s.updates;
    let current = crate::backend::updates::Version::current();
    let found = if u.phase == UpdPhase::Available {
        u.found.as_ref()
    } else {
        None
    };
    let standing = if found.is_some() {
        "outdated"
    } else if current.preview.is_some() {
        "preview"
    } else if u.latest {
        "latest"
    } else {
        ""
    };
    let badge = match standing {
        "outdated" => Some(
            img("ui/upd-outdated.svg")
                .size(px(22.))
                .flex_shrink_0()
                .into_any_element(),
        ),
        "latest" => Some(
            img("ui/upd-latest.svg")
                .size(px(22.))
                .flex_shrink_0()
                .into_any_element(),
        ),
        "preview" => Some(
            svg()
                .path("ui/upd-preview.svg")
                .w(px(21.))
                .h(px(20.))
                .flex_shrink_0()
                .text_color(c(crate::timeline::DANGER))
                .into_any_element(),
        ),
        _ => None,
    };
    let version_row = div()
        .min_h(px(36.))
        .mb(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .text_size(px(16.))
        .line_height(px(24.))
        .text_color(c(0xffffff))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child("Current Version")
                .child(info("upd-info", click(on, || Act::Help("updates")))),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .children(badge)
                .child(current.display()),
        );
    let mut col = div()
        .flex()
        .flex_col()
        .items_start()
        .child(div().w_full().child(version_row));
    col = match standing {
        "outdated" => col.child(upd_card(None, &format!("Your client is currently out of date. The latest version is {}.", found.map(|f| f.0.as_str()).unwrap_or("")))),
        "latest" => col.child(upd_card(Some(("Your client is up to date.", c(ACCENT))), "It is currently on the latest release.")),
        "preview" => col.child(upd_card(
            Some(("This is a Preview Build.", c(crate::timeline::DANGER))),
            "It is released ahead of the official version. Preview Builds are public prototypes used to test new features and surface bugs, so feedback from users helps improve the final release. Expect bugs, unfinished features, and occasional instability.",
        )),
        _ => col,
    };
    if let Some((_, notes)) = found.filter(|f| !f.1.is_empty()) {
        col = col.child(
            div()
                .w_full()
                .flex()
                .justify_center()
                // The card's 16px margin collapses into this 20.
                .mt(px(4.))
                .mb(px(20.))
                .child(
                    div()
                        .w_full()
                        .max_w(px(400.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .mb(px(10.))
                                .text_size(px(16.))
                                .line_height(px(24.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(c(0xf6f6f6))
                                .child("What's New:"),
                        )
                        .child(
                            div()
                                .id("changelog")
                                .max_h(px(200.))
                                .overflow_y_scroll()
                                .p(px(15.))
                                .rounded(px(8.))
                                .bg(ca(0, 0, 0, 0.3))
                                .border_1()
                                .border_color(ca(89, 252, 179, 0.2))
                                .text_size(px(14.))
                                .line_height(px(22.4))
                                .text_color(white_a(0.8))
                                .child(notes.clone()),
                        ),
                ),
        );
    }
    let beta = u.beta;
    col = col.child(
        // `.form-group`'s 15px under the row, which brings its own 10.
        div().w_full().mb(px(5.)).child(toggle_row(
            "beta",
            "Beta Updates",
            Some(Box::new(click(on, || Act::Help("beta")))),
            toggle(
                "tg-beta",
                beta,
                s.flipped.contains("beta"),
                click(on, move || Act::Beta(!beta)),
            ),
            false,
        )),
    );
    let (label, enabled) = match u.phase {
        UpdPhase::Checking => ("Checking...", false),
        UpdPhase::Available => ("Download Update", true),
        _ => ("Check for Updates", true),
    };
    col = col.child(
        div()
            .id("check-updates-btn")
            .h(px(37.))
            .px(px(24.))
            .flex()
            .items_center()
            .rounded(px(8.))
            .border_1()
            .border_color(ca(90, 252, 180, 0.65))
            .bg(ca(89, 252, 179, 0.1))
            .text_size(px(16.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(0xffffff))
            .child(label)
            .when(!enabled, |d| d.opacity(0.5))
            .when(enabled, |d| {
                d.cursor_pointer().on_click(click(on, || Act::CheckUpdates))
            }),
    );
    let status = match &u.phase {
        UpdPhase::Checking => Some(("Checking for updates...".to_string(), white_a(0.8))),
        UpdPhase::Error(e) => Some((e.clone(), c(0xff5252))),
        UpdPhase::NoUpdates if current.preview.is_some() => Some((
            "No newer build yet. You'll be offered the official build as soon as it releases."
                .to_string(),
            c(ACCENT),
        )),
        _ => None,
    };
    if let Some((text, color)) = status {
        col = col.child(
            div()
                .mt(px(10.))
                .mb(px(15.))
                .text_size(px(14.))
                .line_height(px(24.))
                .text_color(color)
                .child(text),
        );
    }
    col.into_any_element()
}

pub struct VoiceView {
    pub models: Vec<crate::backend::transcribe::Model>,
    pub memory_mb: u64,
    pub selected: &'static str,
    pub auto_translate: bool,
    pub auto_transcribe: bool,
    /// The fetch under way, as its progress line.
    pub download: Option<String>,
    pub error: String,
}

impl VoiceView {
    /// `findBestModelForDevice`: small, base, tiny, the first this memory can run.
    pub fn recommended(&self) -> &'static str {
        ["small", "base", "tiny"]
            .into_iter()
            .find(|n| self.models.iter().any(|m| m.name == *n && self.memory_mb >= m.ram as u64))
            .or(self.models.first().map(|m| m.name))
            .unwrap_or("small")
    }

    pub fn current(&self) -> Option<&crate::backend::transcribe::Model> {
        self.models.iter().find(|m| m.name == self.selected)
    }

    /// `findBestFallbackModel`: when `gone` is no longer usable, the nearest downloaded model
    /// above it, then below, then small, then the largest downloaded.
    pub fn fallback(&self, gone: &str) -> &'static str {
        let mut order: Vec<_> = self.models.iter().collect();
        order.sort_by_key(|m| m.size);
        let has = |n: &str| self.models.iter().any(|m| m.name == n && m.downloaded);
        if let Some(i) = order.iter().position(|m| m.name == gone) {
            if let Some(m) = order[i + 1..].iter().chain(order[..i].iter().rev()).find(|m| has(m.name)) {
                return m.name;
            }
        }
        if has("small") || !self.models.iter().any(|m| m.downloaded) {
            return "small";
        }
        order.iter().rev().find(|m| has(m.name)).map_or("small", |m| m.name)
    }
}

/// A label made at runtime for a static-labelled select; the set of labels is small and fixed.
fn interned(text: String) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static LABELS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = LABELS.get_or_init(Default::default).lock().unwrap();
    if let Some(l) = set.get(text.as_str()) {
        return l;
    }
    let l: &'static str = Box::leak(text.into_boxed_str());
    set.insert(l);
    l
}

fn voice_block(s: &Settings, on: &OnAct) -> AnyElement {
    let Some(v) = s.voice.as_ref() else {
        return div().into_any_element();
    };
    let current = v.current();
    let can_translate = current.is_none_or(|m| m.translate);
    let busy = v.download.is_some();
    let switch = |key: &'static str, value: bool, enabled: bool| {
        toggle(key, value, s.flipped.contains(key), {
            let on = on.clone();
            move |_, w, cx| {
                if enabled {
                    on(Act::Toggle(key, !value), w, cx)
                }
            }
        })
    };
    let translate = div()
        .when(!can_translate, |d| d.opacity(0.45))
        .child(toggle_row(
            "voice-translate",
            "Auto-Translate",
            Some(Box::new(click(on, || Act::Help("voiceTranslate")))),
            switch("voiceTranslate", can_translate && v.auto_translate, can_translate),
            false,
        ));
    let warning = (!can_translate).then(|| {
        div()
            .h(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_size(px(13.))
            .text_color(c(0xFCE459))
            .child(svg().path("icons/vector_warning.svg").size(px(16.)).flex_shrink_0().text_color(c(0xFCE459)))
            .child("Selected model does not support translation")
            .mb(px(10.))
    });
    let transcribe = toggle_row(
        "voice-transcribe",
        "Auto-Transcribe",
        Some(Box::new(click(on, || Act::Help("voiceTranscribe")))),
        switch("voiceTranscribe", v.auto_transcribe, true),
        false,
    );
    let recommended = v.recommended();
    let choices: Vec<crate::kit::Choice> = v
        .models
        .iter()
        .map(|m| crate::kit::Choice {
            value: m.name,
            label: interned(if v.memory_mb < m.ram as u64 {
                format!("{} (Insufficient RAM)", m.display)
            } else if m.name == recommended {
                format!("{} [Recommended]", m.display)
            } else {
                m.display.to_string()
            }),
        })
        .collect();
    let disabled: Vec<&str> = v.models.iter().filter(|m| v.memory_mb < m.ram as u64).map(|m| m.name).collect();
    let select = crate::kit::select_with(
        "whisper-model",
        230.,
        12.,
        &choices,
        &disabled,
        v.selected,
        s.open_select == Some("whisper-model") && !busy,
        {
            let on = on.clone();
            move |w, cx| {
                if !busy {
                    on(Act::Select("whisper-model"), w, cx)
                }
            }
        },
        {
            let on = on.clone();
            move |v, w, cx| on(Act::Pick("whisper-model", v), w, cx)
        },
    );
    let trash = current.filter(|m| m.downloaded && !busy).map(|_| {
        div()
            .id("delete-model")
            .w(px(28.))
            .h(px(36.))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .cursor_pointer()
            .opacity(0.75)
            .hover(|d| d.opacity(1.))
            .on_click(click(on, || Act::VoiceDelete))
            .child(svg().path("icons/trash.svg").size(px(20.)).text_color(c(crate::timeline::DANGER)))
    });
    let label = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(16.))
        .line_height(px(24.))
        .text_color(c(0xffffff))
        .child("Whisper Model")
        .child(info("voice-model-info", click(on, || Act::Help("voiceModel"))));
    let warn = |text: SharedString| {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_color(c(0xb2b2b2))
            .child(div().size(px(7.)).rounded_full().flex_shrink_0().bg(c(0xFCA559)))
            .child(text)
            .into_any_element()
    };
    let status: Option<AnyElement> = if let Some(d) = &v.download {
        Some(
            div()
                .flex()
                .items_center()
                .text_color(c(0x59FCB3))
                .child(
                    div()
                        .size(px(16.))
                        .mr(px(6.))
                        .rounded_full()
                        .border_2()
                        .border_color(white_a(0.1))
                        .child(
                            svg()
                                .path("icons/loading.svg")
                                .size_full()
                                .text_color(c(PRIMARY))
                                .with_animation(
                                    "model-spin",
                                    gpui::Animation::new(std::time::Duration::from_millis(800)).repeat(),
                                    |s, t| s.with_transformation(gpui::Transformation::rotate(gpui::percentage(t))),
                                ),
                        ),
                )
                .child(format!(" Downloading... {d}"))
                .into_any_element(),
        )
    } else if !v.error.is_empty() {
        Some(warn(v.error.clone().into()))
    } else if current.is_some_and(|m| m.downloaded) {
        Some(div().text_color(c(0x59FCB3)).child("Vector AI is ready").into_any_element())
    } else if current.is_some() {
        Some(warn("Model not downloaded".into()))
    } else {
        None
    };
    let model = div()
        .mb(px(10.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(label)
                .child(div().flex().items_center().gap(px(8.)).children(trash).child(select)),
        )
        .child(div().mt(px(8.)).min_h(px(24.)).text_size(px(14.)).line_height(px(24.)).children(status));
    let action: Option<AnyElement> = if busy {
        Some(
            div()
                .mt(px(16.))
                .child(small_button("cancel-download", "Cancel Download", || Act::VoiceCancel, on))
                .into_any_element(),
        )
    } else {
        current.filter(|m| !m.downloaded).map(|m| {
            div()
                .id("download-model")
                .w(gpui::relative(0.8))
                .mx_auto()
                .h(px(43.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .bg(gpui::linear_gradient(
                    90.,
                    gpui::linear_color_stop(c(0x33db98), 0.),
                    gpui::linear_color_stop(c(0x00ff9d), 1.),
                ))
                .text_size(px(16.))
                .font_weight(FontWeight::BOLD)
                .text_color(c(0x000000))
                .cursor_pointer()
                .hover(|d| d.opacity(0.75))
                .on_click(click(on, || Act::VoiceDownload))
                .child(format!("Download Model ({})", crate::media::format_bytes(m.size as u64 * 1024 * 1024)))
                .into_any_element()
        })
    };
    div()
        .child(translate)
        .children(warning)
        .child(transcribe)
        .child(model)
        .children(action)
        .into_any_element()
}

pub struct AudioView {
    pub settings: vector_core::calls::settings::AudioSettings,
    pub devices: crate::backend::mic::DeviceList,
    pub direct: bool,
    pub testing: bool,
    pub level: f32,
    /// A call is up: the bench shows its mic instead of a test.
    pub call: Option<f32>,
}

/// `.vselect-tag`: the pill that marks the system default.
fn vselect_tag() -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(1.))
        .border_1()
        .border_color(c(0x3a3a3a))
        .rounded_full()
        .text_size(px(10.))
        .line_height(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(0x9a9a9a))
        .child("Default")
}

/// A device `.vselect-fill`: the system default first, named for what it is now.
fn device_select(
    id: &'static str,
    kind: &'static str,
    icon: &'static str,
    names: &[String],
    default: &str,
    chosen: Option<&String>,
    s: &Settings,
    on: &OnAct,
) -> AnyElement {
    let open = s.open_select == Some(id);
    let mut rows: Vec<(Option<String>, String, bool)> = vec![(
        None,
        if default.is_empty() {
            "System Default".into()
        } else {
            default.to_string()
        },
        !default.is_empty(),
    )];
    rows.extend(names.iter().map(|n| (Some(n.clone()), n.clone(), false)));
    let current = rows
        .iter()
        .find(|r| r.0.as_ref() == chosen)
        .unwrap_or(&rows[0])
        .clone();
    let lead = || {
        svg()
            .path(icon)
            .size(px(18.))
            .flex_shrink_0()
            .text_color(c(0xb2b2b2))
    };
    let caret = div().w(px(10.)).h(px(6.)).flex_shrink_0().ml_auto().child(
        svg()
            .path("ui/caret-down.svg")
            .size_full()
            .text_color(c(0x888888))
            .when(open, |d| {
                d.with_transformation(gpui::Transformation::rotate(gpui::radians(
                    std::f32::consts::PI,
                )))
            }),
    );
    let button = div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .h(px(36.))
        .px(px(12.))
        .border_1()
        .border_color(c(if open { 0x474747 } else { 0x333333 }))
        .hover(|d| d.border_color(c(0x474747)))
        .rounded(px(6.))
        .bg(c(0x0a0a0a))
        .text_size(px(12.))
        .line_height(px(14.))
        .text_color(c(0xffffff))
        .cursor_pointer()
        .on_click(click(on, move || Act::Select(id)))
        .child(lead())
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(current.1.clone()),
        )
        .when(current.2, |d| d.child(vselect_tag()))
        .child(caret);
    let mut root = div().relative().w_full().child(button);
    if open {
        let mut list = div()
            .absolute()
            .top(px(42.))
            .left_0()
            .right_0()
            .p(px(4.))
            .border_1()
            .border_color(c(0x2a2a2a))
            .rounded(px(8.))
            .bg(c(0x111111))
            .shadow(vec![gpui::BoxShadow {
                color: ca(0, 0, 0, 0.55),
                offset: gpui::point(px(0.), px(12.)),
                blur_radius: px(32.),
                spread_radius: px(0.),
                inset: false,
            }])
            .occlude();
        for (ix, (value, label, tag)) in rows.into_iter().enumerate() {
            let selected = value.as_ref() == current.0.as_ref();
            let pick = value.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("{id}-{ix}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .text_color(c(if selected { 0xffffff } else { 0xcfcfcf }))
                    .when(selected, |d| d.bg(c(0x1f1f1f)))
                    .hover(|d| d.bg(c(0x1f1f1f)).text_color(c(0xffffff)))
                    .cursor_pointer()
                    .on_click(click(on, move || Act::Device(kind, pick.clone())))
                    .child(lead())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(label),
                    )
                    .when(tag, |d| d.child(vselect_tag()))
                    .when(selected, |d| {
                        d.child(
                            svg()
                                .path("icons/check.svg")
                                .size(px(14.))
                                .flex_shrink_0()
                                .text_color(c(PRIMARY)),
                        )
                    }),
            );
        }
        root = root.child(gpui::deferred(list).with_priority(1));
    }
    root.into_any_element()
}

/// `.st-slider`: a thin track filled to the value, a round white handle; a press or a drag sets it.
fn slider(key: &'static str, value: f32, s: &Settings, on: &OnAct) -> AnyElement {
    let value = value.clamp(0., 1.);
    let bounds = s.sliders.clone();
    let dragging = s.dragging.clone();
    let at = |b: gpui::Bounds<gpui::Pixels>, x: gpui::Pixels| {
        ((x - b.origin.x - px(10.)) / (b.size.width - px(20.)).max(px(1.))).clamp(0., 1.)
    };
    let down = {
        let (on, bounds, dragging) = (on.clone(), bounds.clone(), dragging.clone());
        move |e: &gpui::MouseDownEvent, w: &mut Window, cx: &mut App| {
            let Some(b) = bounds.borrow().get(key).copied() else {
                return;
            };
            dragging.set(Some(key));
            on(Act::Volume(key, at(b, e.position.x)), w, cx);
        }
    };
    let track = {
        let (on, bounds, dragging) = (on.clone(), bounds.clone(), dragging.clone());
        let record = bounds.clone();
        gpui::canvas(
            move |b, _, _| {
                record.borrow_mut().insert(key, b);
            },
            move |_, _, window, _| {
                if dragging.get() != Some(key) {
                    return;
                }
                let (on, bounds, d) = (on.clone(), bounds.clone(), dragging.clone());
                window.on_mouse_event(move |e: &gpui::MouseMoveEvent, phase, w, cx| {
                    if phase != gpui::DispatchPhase::Bubble || d.get() != Some(key) {
                        return;
                    }
                    if !e.dragging() {
                        d.set(None);
                        return;
                    }
                    if let Some(b) = bounds.borrow().get(key).copied() {
                        on(Act::Volume(key, at(b, e.position.x)), w, cx);
                    }
                });
                let d = dragging.clone();
                window.on_mouse_event(move |_: &gpui::MouseUpEvent, _, _, _| d.set(None));
            },
        )
        .absolute()
        .inset_0()
    };
    div()
        .id(key)
        .relative()
        .w_full()
        .h(px(22.))
        .cursor_pointer()
        .on_mouse_down(gpui::MouseButton::Left, down)
        .child(track)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(8.))
                .h(px(6.))
                .rounded(px(3.))
                .bg(c(0x1a1a1a))
                .child(
                    div()
                        .h_full()
                        .rounded(px(3.))
                        .bg(c(0x6dd89d))
                        .w(gpui::relative(value)),
                ),
        )
        .child(
            div()
                .absolute()
                .top(px(1.))
                .left(gpui::relative(value))
                .child(
                    div()
                        .ml(px(-20. * value))
                        .size(px(20.))
                        .rounded_full()
                        .bg(c(0xffffff))
                        .shadow(vec![gpui::BoxShadow {
                            color: ca(0, 0, 0, 0.35),
                            offset: gpui::point(px(0.), px(1.)),
                            blur_radius: px(4.),
                            spread_radius: px(0.),
                            inset: false,
                        }]),
                ),
        )
        .into_any_element()
}

/// `VoiceMeter`: the gradient spans the track, the level uncovers as much as it reaches.
pub fn voice_meter(level: f32, active: bool) -> impl IntoElement {
    let level = level.clamp(0., 1.);
    div()
        .flex_1()
        .h(px(6.))
        .rounded(px(3.))
        .bg(c(0x1a1a1a))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(gpui::relative(level))
                .overflow_hidden()
                .rounded(px(3.))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(if level > 0. { 1. / level } else { 0. }))
                        .flex()
                        .when(!active, |d| d.opacity(0.35))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(0.8))
                                .bg(gpui::linear_gradient(
                                    90.,
                                    gpui::linear_color_stop(c(0x6ed99e), 0.),
                                    gpui::linear_color_stop(c(0xf7e470), 1.),
                                )),
                        )
                        .child(div().h_full().flex_1().bg(gpui::linear_gradient(
                            90.,
                            gpui::linear_color_stop(c(0xf7e470), 0.),
                            gpui::linear_color_stop(c(0xea6662), 1.),
                        ))),
                ),
        )
}

fn calls_field(label: &'static str, control: AnyElement) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .text_size(px(14.))
                .line_height(px(17.))
                .text_color(c(0xb2b2b2))
                .child(label),
        )
        .child(control)
}

/// `Calls.svelte`: devices and volumes, the microphone test, then the processing switches.
fn calls_block(s: &Settings, on: &OnAct) -> AnyElement {
    let Some(a) = s.audio.as_ref() else {
        return div().into_any_element();
    };
    let d = &a.devices;
    let mut col = div().flex().flex_col();
    if !d.inputs.is_empty() || !d.outputs.is_empty() {
        col = col.child(
            div()
                .flex()
                .when(s.narrow, |d| d.flex_col())
                .gap(px(20.))
                .mb(px(28.))
                .child(calls_field(
                    "Microphone",
                    device_select(
                        "dev-input",
                        "input",
                        "icons/mic-on.svg",
                        &d.inputs,
                        &d.default_input,
                        d.prefs.input.as_ref(),
                        s,
                        on,
                    ),
                ))
                .child(calls_field(
                    "Speaker",
                    device_select(
                        "dev-output",
                        "output",
                        "icons/volume-max.svg",
                        &d.outputs,
                        &d.default_output,
                        d.prefs.output.as_ref(),
                        s,
                        on,
                    ),
                )),
        );
    }
    col = col.child(
        div()
            .flex()
            .when(s.narrow, |d| d.flex_col())
                .gap(px(20.))
            .mb(px(28.))
            .child(calls_field(
                "Microphone Volume",
                slider("mic", a.settings.mic_volume, s, on),
            ))
            .child(calls_field(
                "Speaker Volume",
                slider("speaker", a.settings.speaker_volume, s, on),
            )),
    );
    col = col.child(
        div()
            .flex()
            .items_center()
            .gap(px(22.))
            .mb(px(16.))
            .when_some(a.call, |d, _| {
                d.child(div().flex_none().text_size(px(14.)).text_color(c(0xb2b2b2)).child("On a call"))
            })
            .when(a.call.is_none(), |d| d.child(
                div()
                    .id("mic-test")
                    .w(px(86.))
                    .h(px(32.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(c(0x333333))
                    .hover(|d| d.bg(c(0x3d3d3d)))
                    .text_size(px(12.))
                    .text_color(c(0xffffff))
                    .cursor_pointer()
                    .on_click(click(on, || Act::MicTest))
                    .child(if a.testing { "Stop Test" } else { "Test Mic" }),
            ))
            .child(match a.call {
                Some(level) => voice_meter(level, true),
                None => voice_meter(a.level, a.testing),
            }),
    );
    // `.st-group`'s 36px margin swallows the bench's 16.
    col = col.child(div().mt(px(20.)).child(group_label("Microphone", true)));
    for (key, label, on_) in [
        ("autoGain", "Automatic Gain", a.settings.auto_gain),
        ("echoCancel", "Echo Cancellation", a.settings.echo_cancel),
        (
            "noiseSuppress",
            "Noise Suppression",
            a.settings.noise_suppress,
        ),
    ] {
        let switch = toggle(
            SharedString::from(format!("tg-{key}")),
            on_,
            s.flipped.contains(key),
            click(on, move || Act::AudioSwitch(key, !on_)),
        );
        col = col.child(toggle_row(
            key,
            label,
            Some(Box::new(click(on, move || Act::Help(key)))),
            switch,
            true,
        ));
    }
    col = col.child(group_label("Connection", false));
    let direct = a.direct;
    let switch = toggle(
        "tg-direct",
        direct,
        s.flipped.contains("direct"),
        click(on, move || Act::Direct(!direct)),
    );
    col.child(toggle_row(
        "direct",
        "Direct Connection",
        Some(Box::new(click(on, || Act::Help("direct")))),
        switch,
        true,
    ))
    .into_any_element()
}

fn block(id: &str, s: &Settings, on: &OnAct) -> Option<AnyElement> {
    Some(match id {
        "security" => security_block(s, on),
        "updates" => updates_block(s, on),
        "calls" => calls_block(s, on),
        "voice" => voice_block(s, on),
        "display" => display_block(s, on),
        "privacy" => privacy_block(s, on),
        "notifications" => notifications_block(s, on),
        "storage" => storage_block(s, on),
        "relays" => relays_block(s, on),
        "servers" => servers_block(s, on),
        "clock" => clock_block(s, on),
        "streamer" => streamer_block(s, on),
        "routing" => crate::routing::block(&s.net, s.phone, on),
        "blocked" => blocked_block(s, on),
        "advanced" => row(s, "advanced", on, false),
        "logs" => line(
            "logs",
            "Copy Logs",
            Some("crashLog"),
            small_button("copy-logs", "Copy", || Act::CopyLogs, on),
            on,
        )
        .into_any_element(),
        "logout" => {
            line("logout", "Logout", Some("logout"), danger_button(on), on).into_any_element()
        }
        _ => return None,
    })
}

fn nav_icon(name: &str, color: gpui::Hsla) -> impl IntoElement {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(17.))
        .flex_shrink_0()
        .text_color(color)
}

fn badge_new() -> impl IntoElement {
    div()
        .flex_shrink_0()
        .ml_auto()
        .px(px(7.))
        .py(px(2.))
        .rounded_full()
        .bg(c(ACCENT))
        .text_color(c(0x052b1e))
        .text_size(px(10.))
        .line_height(px(12.))
        .font_weight(FontWeight::BOLD)
        .child(crate::kit::spaced("NEW", 0.5))
}

/// `SectionNav`: the search, the title, each section with its anchors, the footer links.
pub type NavFn<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

/// What a sectioned settings nav acts on: its section, an anchor inside it, the cleared search.
pub struct NavOn {
    pub section: NavFn<&'static str>,
    pub anchor: NavFn<(&'static str, &'static str)>,
    pub clear: NavFn<()>,
}

/// `.cs-nav`: the search, a head, each section with its timeline of anchors, and a footer.
pub fn section_nav(
    cats: &[Category],
    current: &str,
    fresh: &[&'static str],
    active: &'static str,
    search: &Entity<crate::composer::TextInput>,
    head: AnyElement,
    footer: Option<AnyElement>,
    width: f32,
    bg: u32,
    on: &NavOn,
    cx: &App,
) -> AnyElement {
    let query = search.read(cx).text().trim().to_lowercase();
    let mut list = div().flex().flex_col();
    let mut any = false;
    for cat in cats {
        let hits: Vec<bool> = cat
            .anchors
            .iter()
            .map(|a| {
                query.is_empty()
                    || format!("{} {} {}", cat.label, a.label, a.keys)
                        .to_lowercase()
                        .contains(&query)
            })
            .collect();
        if !hits.contains(&true) {
            continue;
        }
        any = true;
        let selected = query.is_empty() && cat.id == current;
        let open = if query.is_empty() {
            cat.id == current
        } else {
            true
        };
        let flag = !open && cat.anchors.iter().any(|a| fresh.contains(&a.id));
        let id = cat.id;
        let color = if cat.danger {
            c(crate::timeline::DANGER)
        } else if selected {
            c(0xffffff)
        } else {
            c(0xb2b2b2)
        };
        let icon = if cat.danger {
            c(crate::timeline::DANGER)
        } else if selected {
            c(0xffffff)
        } else {
            c(0x5b6060)
        };
        let pick = on.section.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("cs-section-{id}")))
                .group(SharedString::from(format!("cs-section-{id}")))
                .h(px(38.))
                .mb(px(1.))
                .px(px(11.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(8.))
                .text_size(px(15.))
                .line_height(px(18.))
                .font_weight(if cat.danger {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(color)
                .when(selected, |d| d.bg(c(0x1b1b1b)))
                .when(!selected, |d| {
                    d.hover(|d| {
                        d.bg(c(0x151515)).text_color(if cat.danger {
                            c(crate::timeline::DANGER)
                        } else {
                            c(0xffffff)
                        })
                    })
                })
                .cursor_pointer()
                .on_click(move |_, w, cx| pick(id, w, cx))
                .child(
                    svg()
                        .path(SharedString::from(format!(
                            "icons/{}.svg",
                            if cat.icon == "warning" {
                                "vector_warning"
                            } else {
                                cat.icon
                            }
                        )))
                        .size(px(17.))
                        .flex_shrink_0()
                        .text_color(icon)
                        .when(!cat.danger, |d| {
                            d.group_hover(SharedString::from(format!("cs-section-{id}")), |d| {
                                d.text_color(c(0xffffff))
                            })
                        }),
                )
                .child(div().min_w_0().truncate().child(cat.label))
                .when(flag, |d| d.child(badge_new())),
        );
        if open {
            let shown: Vec<&Anchor> = cat
                .anchors
                .iter()
                .zip(&hits)
                .filter(|(_, h)| **h)
                .map(|(a, _)| a)
                .collect();
            let marker = selected
                .then(|| shown.iter().position(|a| a.id == active))
                .flatten();
            let mut anchors = div().relative().pb(px(6.)).child(
                div()
                    .absolute()
                    .left(px(18.5))
                    .top(px(6.))
                    .bottom(px(12.))
                    .w(px(2.))
                    .bg(c(0x333333)),
            );
            if let Some(i) = marker {
                anchors = anchors.child(
                    div()
                        .absolute()
                        .left(px(18.5))
                        .top(px(34. * i as f32 + 17. - 10.))
                        .w(px(2.))
                        .h(px(20.))
                        .rounded(px(1.))
                        .bg(c(0xffffff)),
                );
            }
            for a in shown {
                let on_it = selected && a.id == active;
                let (cid, aid) = (cat.id, a.id);
                let jump = on.anchor.clone();
                anchors = anchors.child(
                    div()
                        .id(SharedString::from(format!("cs-anchor-{aid}")))
                        .h(px(34.))
                        .pl(px(40.))
                        .pr(px(11.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(14.))
                        .line_height(px(17.))
                        .font_weight(if on_it {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if on_it { c(0xffffff) } else { c(0xb2b2b2) })
                        .hover(|d| d.text_color(if on_it { c(0xffffff) } else { c(0xe6e6e6) }))
                        .cursor_pointer()
                        .on_click(move |_, w, cx| jump((cid, aid), w, cx))
                        .child(div().min_w_0().truncate().child(a.label))
                        .when(fresh.contains(&a.id), |d| d.child(badge_new())),
                );
            }
            list = list.child(anchors);
        }
    }
    if !any {
        list = list.child(
            div()
                .mx(px(10.))
                .my(px(12.))
                .text_size(px(13.))
                .text_color(c(0x6f6f6f))
                .child("No matches"),
        );
    }
    let has_query = !query.is_empty();
    let clear = on.clear.clone();
    div()
        .w(px(width))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .bg(c(bg))
        .border_r_1()
        .border_color(c(DIVIDER))
        .child(
            // .cs-search
            div()
                .h(px(50.))
                .flex_shrink_0()
                .px(px(10.))
                .relative()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(c(0x171717))
                .child(
                    div()
                        .w_full()
                        .h(px(34.))
                        .rounded(px(8.))
                        .bg(c(0x1f1f1f))
                        .pl(px(34.))
                        .pr(px(if has_query { 38. } else { 12. }))
                        .flex()
                        .items_center()
                        .child(div().flex_1().min_w_0().child(search.clone())),
                )
                .child(
                    svg()
                        .path("icons/search.svg")
                        .absolute()
                        .left(px(20.5))
                        .top(px(16.5))
                        .size(px(17.))
                        .text_color(c(0xb2b2b2)),
                )
                .when(has_query, |d| {
                    d.child(
                        div()
                            .id("cs-search-clear")
                            .absolute()
                            .right(px(21.))
                            .top(px(16.))
                            .size(px(18.))
                            .cursor_pointer()
                            .group("cs-clear")
                            .on_click(move |_, w, cx| clear((), w, cx))
                            .child(
                                svg()
                                    .path("ui/search-clear.svg")
                                    .size_full()
                                    .text_color(c(0xb2b2b2))
                                    .group_hover("cs-clear", |d| d.text_color(c(0xffffff))),
                            ),
                    )
                }),
        )
        .child(
            div()
                .id("cs-nav-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pt(px(18.))
                .px(px(10.))
                .pb(px(24.))
                .child(head)
                .child(list)
                .children(footer),
        )
        .into_any_element()
}

fn nav(
    s: &Settings,
    cats: &[Category],
    fresh: &[&'static str],
    active: &'static str,
    on: &OnAct,
    cx: &App,
) -> AnyElement {
    let link = |label: &'static str, key: &'static str| {
        div()
            .id(key)
            .text_size(px(12.))
            .line_height(px(14.))
            .text_color(c(0x5b6060))
            .hover(|d| d.text_color(c(0xffffff)))
            .cursor_pointer()
            .on_click(click(on, move || Act::Link(key)))
            .child(label)
    };
    let head = div()
        .mx(px(10.))
        .mb(px(17.))
        .pb(px(16.))
        .border_b_1()
        .border_color(c(0x262626))
        .text_size(px(20.))
        .line_height(px(24.))
        .font_weight(FontWeight::BOLD)
        .text_color(c(0xffffff))
        .child("Settings")
        .into_any_element();
    let footer = div()
        .mt(px(12.))
        .pt(px(16.))
        .px(px(8.))
        .border_t_1()
        .border_color(c(0x262626))
        .flex()
        .flex_wrap()
        .gap_x(px(16.))
        .gap_y(px(6.))
        .child(link("Donate", "donate"))
        .child(link("Docs", "gitbook"))
        .child(link("Privacy Policy", "privacy"))
        .into_any_element();
    let (a, b, c_) = (on.clone(), on.clone(), on.clone());
    let nav_on = NavOn {
        section: Rc::new(move |id, w, cx| a(Act::Category(id), w, cx)),
        anchor: Rc::new(move |(cat, anchor), w, cx| b(Act::Anchor(cat, anchor), w, cx)),
        clear: Rc::new(move |_, w, cx| c_(Act::ClearSearch, w, cx)),
    };
    section_nav(
        cats,
        s.cat,
        fresh,
        active,
        &s.search,
        head,
        Some(footer),
        crate::ui::LIST_W,
        LIST_BG,
        &nav_on,
        cx,
    )
}

/// The whole screen: the nav in the list column and the page beside it, `width` wide.
pub fn view(s: &Settings, width: f32, on: &OnAct, cx: &App) -> AnyElement {
    let cats = categories(&s.offers);
    let fresh = s.fresh_ids(&cats);
    let cat = cats.iter().find(|c| c.id == s.cat).unwrap_or(&cats[0]);
    let active = s.active_anchor(cat);
    let pane = width - crate::ui::LIST_W;
    let gutter = crate::kit::page_gutter(pane);
    let blocks = cat.anchors.iter().enumerate().map(|(i, a)| {
        // `.cs-content.st-page`: a 720px column at the pane's left, inside its gutters.
        div().max_w(px(720. + 2. * gutter)).px(px(gutter)).child(
            div()
                .pt(px(30.))
                .pb(px(34.))
                .when(i > 0, |d| d.border_t_1().border_color(c(0x262626)))
                .child({
                    let group = SharedString::from(format!("cs-h-{}", a.id));
                    let (cat_id, anchor) = (cat.id, a.id);
                    div()
                        .group(group.clone())
                        .mb(px(20.))
                        .flex()
                        .items_center()
                        .text_size(px(24.))
                        .line_height(px(28.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(0xffffff))
                        .child(a.label)
                        .child(
                            div()
                                .id(SharedString::from(format!("cs-link-{}", a.id)))
                                .tooltip(crate::kit::tooltip("Copy Link"))
                                .ml(px(6.))
                                .p(px(5.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .opacity(0.)
                                .group_hover(group, |d| d.opacity(0.55))
                                .hover(|d| d.opacity(1.))
                                .on_click(click(on, move || Act::CopyPlace(cat_id, anchor)))
                                .child(svg().path("icons/share.svg").size(px(14.)).text_color(c(0xffffff))),
                        )
                })
                .children(block(a.id, s, on))
                .with_animation(
                    SharedString::from(format!("cs-in-{}", a.id)),
                    gpui::Animation::new(std::time::Duration::from_millis(300))
                        .with_easing(crate::kit::bezier(0.22, 1., 0.36, 1.)),
                    |d, t| d.opacity(t).mt(px(8. * (1. - t))),
                ),
        )
    });
    let main = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(c(APP_BG))
        .child(
            div()
                .h(px(50.))
                .flex_shrink_0()
                .pl(px(28.))
                .pr(px(18.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(c(0x171717))
                .text_size(px(16.))
                .line_height(px(19.))
                .text_color(c(0xffffff))
                .child(cat.label),
        )
        .child(
            div()
                .id(SharedString::from(format!("cs-scroll-{}", cat.id)))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&s.scroll)
                .when(s.jumped.is_some(), |d| d.on_scroll(click_wheel(on)))
                .pt(px(12.))
                .children(blocks)
                .child(div().h(px(140.))),
        );
    div()
        .size_full()
        .flex()
        .child(nav(s, &cats, &fresh, active, on, cx))
        .child(main)
        .into_any_element()
}
