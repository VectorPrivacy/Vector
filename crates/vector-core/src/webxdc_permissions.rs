//! Mini App Permissions System
//!
//! This module handles optional permissions that Mini Apps can request.
//! Permissions are declared when publishing an app and must be explicitly
//! granted by users before they take effect.
//!
//! ## Security Model
//!
//! By default, ALL sensitive permissions are denied via the Permissions-Policy header.
//! Apps can request specific permissions in their metadata, but users must:
//! 1. Be prompted on first launch (if any permissions are requested)
//! 2. Explicitly grant each permission
//! 3. Have the ability to toggle permissions at any time in App Details
//!
//! ## Available Permissions
//!
//! - `microphone` - Access to microphone for voice chat, recording, etc.
//! - `camera` - Access to camera for video calls, photos, etc.
//! - `geolocation` - Access to device location
//! - `clipboard-read` - Read from clipboard
//! - `clipboard-write` - Write to clipboard
//! - `fullscreen` - Allow fullscreen mode
//! - `autoplay` - Allow media autoplay
//! - `display-capture` - Screen capture via getDisplayMedia()
//! - `midi` - Web MIDI API for musical instruments
//! - `picture-in-picture` - Picture-in-Picture video mode
//! - `screen-wake-lock` - Prevent screen from sleeping
//! - `speaker-selection` - Select audio output device
//! - `accelerometer` - Device acceleration sensor
//! - `gyroscope` - Device orientation/rotation sensor
//! - `magnetometer` - Compass/magnetic field sensor
//! - `ambient-light-sensor` - Ambient light level sensor
//! - `bluetooth` - Web Bluetooth API
//!
//! ## Nostr Event Tag Format
//!
//! Permissions are stored as a tag in the marketplace event:
//! ```json
//! ["permissions", "microphone,camera,fullscreen"]
//! ```
//!
//! The value is a comma-separated list of permission names.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::collections::HashSet;
use std::str::FromStr;

/// Available permissions that Mini Apps can request
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MiniAppPermission {
    /// Access to microphone for voice chat, recording, etc.
    Microphone,
    /// Access to camera for video calls, photos, etc.
    Camera,
    /// Access to device location
    Geolocation,
    /// Read from clipboard
    ClipboardRead,
    /// Write to clipboard
    ClipboardWrite,
    /// Allow fullscreen mode
    Fullscreen,
    /// Allow media autoplay
    Autoplay,
    /// Screen capture via getDisplayMedia()
    DisplayCapture,
    /// Web MIDI API for musical instruments
    Midi,
    /// Picture-in-Picture video mode
    PictureInPicture,
    /// Prevent screen from sleeping
    ScreenWakeLock,
    /// Select audio output device
    SpeakerSelection,
    /// Device acceleration sensor
    Accelerometer,
    /// Device orientation/rotation sensor
    Gyroscope,
    /// Compass/magnetic field sensor
    Magnetometer,
    /// Ambient light level sensor
    AmbientLightSensor,
    /// Web Bluetooth API
    Bluetooth,
}

impl MiniAppPermission {
    /// Get all available permissions
    pub fn all() -> Vec<MiniAppPermission> {
        vec![
            MiniAppPermission::Microphone,
            MiniAppPermission::Camera,
            MiniAppPermission::Geolocation,
            MiniAppPermission::ClipboardRead,
            MiniAppPermission::ClipboardWrite,
            MiniAppPermission::Fullscreen,
            MiniAppPermission::Autoplay,
            MiniAppPermission::DisplayCapture,
            MiniAppPermission::Midi,
            MiniAppPermission::PictureInPicture,
            MiniAppPermission::ScreenWakeLock,
            MiniAppPermission::SpeakerSelection,
            MiniAppPermission::Accelerometer,
            MiniAppPermission::Gyroscope,
            MiniAppPermission::Magnetometer,
            MiniAppPermission::AmbientLightSensor,
            MiniAppPermission::Bluetooth,
        ]
    }

    /// Get the Permissions-Policy directive name for this permission
    pub fn policy_name(&self) -> &'static str {
        match self {
            MiniAppPermission::Microphone => "microphone",
            MiniAppPermission::Camera => "camera",
            MiniAppPermission::Geolocation => "geolocation",
            MiniAppPermission::ClipboardRead => "clipboard-read",
            MiniAppPermission::ClipboardWrite => "clipboard-write",
            MiniAppPermission::Fullscreen => "fullscreen",
            MiniAppPermission::Autoplay => "autoplay",
            MiniAppPermission::DisplayCapture => "display-capture",
            MiniAppPermission::Midi => "midi",
            MiniAppPermission::PictureInPicture => "picture-in-picture",
            MiniAppPermission::ScreenWakeLock => "screen-wake-lock",
            MiniAppPermission::SpeakerSelection => "speaker-selection",
            MiniAppPermission::Accelerometer => "accelerometer",
            MiniAppPermission::Gyroscope => "gyroscope",
            MiniAppPermission::Magnetometer => "magnetometer",
            MiniAppPermission::AmbientLightSensor => "ambient-light-sensor",
            MiniAppPermission::Bluetooth => "bluetooth",
        }
    }

    /// Get a human-readable label for this permission
    pub fn label(&self) -> &'static str {
        match self {
            MiniAppPermission::Microphone => "Microphone",
            MiniAppPermission::Camera => "Camera",
            MiniAppPermission::Geolocation => "Location",
            MiniAppPermission::ClipboardRead => "Read Clipboard",
            MiniAppPermission::ClipboardWrite => "Write Clipboard",
            MiniAppPermission::Fullscreen => "Fullscreen",
            MiniAppPermission::Autoplay => "Autoplay Media",
            MiniAppPermission::DisplayCapture => "Screen Capture",
            MiniAppPermission::Midi => "MIDI Devices",
            MiniAppPermission::PictureInPicture => "Picture-in-Picture",
            MiniAppPermission::ScreenWakeLock => "Keep Screen On",
            MiniAppPermission::SpeakerSelection => "Speaker Selection",
            MiniAppPermission::Accelerometer => "Accelerometer",
            MiniAppPermission::Gyroscope => "Gyroscope",
            MiniAppPermission::Magnetometer => "Magnetometer",
            MiniAppPermission::AmbientLightSensor => "Light Sensor",
            MiniAppPermission::Bluetooth => "Bluetooth",
        }
    }

    /// Get a description of what this permission allows
    pub fn description(&self) -> &'static str {
        match self {
            MiniAppPermission::Microphone => "Access your microphone for voice chat or recording",
            MiniAppPermission::Camera => "Access your camera for video calls or photos",
            MiniAppPermission::Geolocation => "Access your device location",
            MiniAppPermission::ClipboardRead => "Read text from your clipboard",
            MiniAppPermission::ClipboardWrite => "Copy text to your clipboard",
            MiniAppPermission::Fullscreen => "Enter fullscreen mode",
            MiniAppPermission::Autoplay => "Automatically play audio and video",
            MiniAppPermission::DisplayCapture => "Capture your screen or window",
            MiniAppPermission::Midi => "Connect to MIDI instruments and controllers",
            MiniAppPermission::PictureInPicture => "Play video in a floating window",
            MiniAppPermission::ScreenWakeLock => "Prevent your screen from sleeping",
            MiniAppPermission::SpeakerSelection => "Choose which speaker to use for audio",
            MiniAppPermission::Accelerometer => "Detect device acceleration and movement",
            MiniAppPermission::Gyroscope => "Detect device rotation and orientation",
            MiniAppPermission::Magnetometer => "Access compass and magnetic field data",
            MiniAppPermission::AmbientLightSensor => "Detect ambient light levels",
            MiniAppPermission::Bluetooth => "Connect to Bluetooth devices",
        }
    }

    /// Get an icon name for this permission (for UI display)
    pub fn icon(&self) -> &'static str {
        match self {
            MiniAppPermission::Microphone => "microphone",
            MiniAppPermission::Camera => "camera",
            MiniAppPermission::Geolocation => "location",
            MiniAppPermission::ClipboardRead => "clipboard",
            MiniAppPermission::ClipboardWrite => "clipboard",
            MiniAppPermission::Fullscreen => "maximize",
            MiniAppPermission::Autoplay => "play",
            MiniAppPermission::DisplayCapture => "monitor",
            MiniAppPermission::Midi => "music",
            MiniAppPermission::PictureInPicture => "picture-in-picture",
            MiniAppPermission::ScreenWakeLock => "sun",
            MiniAppPermission::SpeakerSelection => "speaker",
            MiniAppPermission::Accelerometer => "activity",
            MiniAppPermission::Gyroscope => "rotate-3d",
            MiniAppPermission::Magnetometer => "compass",
            MiniAppPermission::AmbientLightSensor => "lightbulb",
            MiniAppPermission::Bluetooth => "bluetooth",
        }
    }
}

impl fmt::Display for MiniAppPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.policy_name())
    }
}

impl FromStr for MiniAppPermission {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().trim() {
            "microphone" => Ok(MiniAppPermission::Microphone),
            "camera" => Ok(MiniAppPermission::Camera),
            "geolocation" | "location" => Ok(MiniAppPermission::Geolocation),
            "clipboard-read" | "clipboardread" => Ok(MiniAppPermission::ClipboardRead),
            "clipboard-write" | "clipboardwrite" => Ok(MiniAppPermission::ClipboardWrite),
            "fullscreen" => Ok(MiniAppPermission::Fullscreen),
            "autoplay" => Ok(MiniAppPermission::Autoplay),
            "display-capture" | "displaycapture" => Ok(MiniAppPermission::DisplayCapture),
            "midi" => Ok(MiniAppPermission::Midi),
            "picture-in-picture" | "pictureinpicture" | "pip" => Ok(MiniAppPermission::PictureInPicture),
            "screen-wake-lock" | "screenwakelock" => Ok(MiniAppPermission::ScreenWakeLock),
            "speaker-selection" | "speakerselection" => Ok(MiniAppPermission::SpeakerSelection),
            "accelerometer" => Ok(MiniAppPermission::Accelerometer),
            "gyroscope" => Ok(MiniAppPermission::Gyroscope),
            "magnetometer" => Ok(MiniAppPermission::Magnetometer),
            "ambient-light-sensor" | "ambientlightsensor" => Ok(MiniAppPermission::AmbientLightSensor),
            "bluetooth" => Ok(MiniAppPermission::Bluetooth),
            other => Err(format!("Unknown permission: {}", other)),
        }
    }
}

/// Parse a comma-separated string of permissions into a set
pub fn parse_permissions(permissions_str: &str) -> HashSet<MiniAppPermission> {
    permissions_str
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// Serialize a set of permissions to a comma-separated string
#[cfg(test)]
pub fn serialize_permissions(permissions: &HashSet<MiniAppPermission>) -> String {
    let mut perms: Vec<_> = permissions.iter().map(|p| p.to_string()).collect();
    perms.sort();
    perms.join(",")
}

/// Permission info for frontend display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub icon: String,
}

impl From<MiniAppPermission> for PermissionInfo {
    fn from(perm: MiniAppPermission) -> Self {
        PermissionInfo {
            id: perm.to_string(),
            label: perm.label().to_string(),
            description: perm.description().to_string(),
            icon: perm.icon().to_string(),
        }
    }
}

/// Get all available permissions as PermissionInfo for frontend display
pub fn get_all_permission_info() -> Vec<PermissionInfo> {
    MiniAppPermission::all().into_iter().map(|p| p.into()).collect()
}

// ─── Permissions-Policy header ──────────────────────────────────────────────

/// Base Permissions Policy that denies all sensitive APIs by default
/// This is a comprehensive list from DeltaChat based on W3C spec
/// https://github.com/w3c/webappsec-permissions-policy/blob/main/features.md
///
/// NOTE: Some permissions can be dynamically enabled if the user grants them.
/// See `build_permissions_policy()` for dynamic generation.
const PERMISSIONS_POLICY_DENY_ALL: &str = concat!(
    "accelerometer=(), ",
    "ambient-light-sensor=(), ",
    "attribution-reporting=(), ",
    "autoplay=(self), ",
    "battery=(), ",
    "bluetooth=(), ",
    "camera=(), ",
    "ch-ua=(), ",
    "ch-ua-arch=(), ",
    "ch-ua-bitness=(), ",
    "ch-ua-full-version=(), ",
    "ch-ua-full-version-list=(), ",
    "ch-ua-high-entropy-values=(), ",
    "ch-ua-mobile=(), ",
    "ch-ua-model=(), ",
    "ch-ua-platform=(), ",
    "ch-ua-platform-version=(), ",
    "ch-ua-wow64=(), ",
    "compute-pressure=(), ",
    "cross-origin-isolated=(), ",
    "direct-sockets=(), ",
    "display-capture=(), ",
    "encrypted-media=(), ",
    "execution-while-not-rendered=(), ",
    "execution-while-out-of-viewport=(), ",
    "fullscreen=(), ",
    "geolocation=(), ",
    "gyroscope=(), ",
    "hid=(), ",
    "identity-credentials-get=(), ",
    "idle-detection=(), ",
    "keyboard-map=(), ",
    "magnetometer=(), ",
    "mediasession=(), ",
    "microphone=(), ",
    "midi=(), ",
    "navigation-override=(), ",
    "otp-credentials=(), ",
    "payment=(), ",
    "picture-in-picture=(), ",
    "publickey-credentials-create=(), ",
    "publickey-credentials-get=(), ",
    "screen-wake-lock=(), ",
    "serial=(), ",
    "sync-xhr=(), ",
    "storage-access=(), ",
    "usb=(), ",
    "web-share=(), ",
    "window-management=(), ",
    "xr-spatial-tracking=(), ",
    "autofill=(), ",
    "clipboard-read=(), ",
    "clipboard-write=(), ",
    "deferred-fetch=(), ",
    "gamepad=(self), ",
    "language-detector=(), ",
    "language-model=(), ",
    "manual-text=(), ",
    "rewriter=(), ",
    "speaker-selection=(), ",
    "summarizer=(), ",
    "translator=(), ",
    "writer=(), ",
    "all-screens-capture=(), ",
    "browsing-topics=(), ",
    "captured-surface-control=(), ",
    "conversion-measurement=(), ",
    "digital-credentials-get=(), ",
    "digital-credentials-create=(), ",
    "focus-without-user-activation=(), ",
    "join-ad-interest-group=(), ",
    "local-fonts=(), ",
    "monetization=(), ",
    "run-ad-auction=(), ",
    "smart-card=(), ",
    "sync-script=(), ",
    "trust-token-redemption=(), ",
    "unload=(), ",
    "vertical-scroll=(), ",
    "document-domain=(), ",
    "window-placement=()",
);

/// Permission policies that can be dynamically enabled based on user grants
/// Maps permission name -> (policy directive name, allow value when enabled)
const GRANTABLE_PERMISSIONS: &[(&str, &str)] = &[
    ("microphone", "microphone"),
    ("camera", "camera"),
    ("geolocation", "geolocation"),
    ("clipboard-read", "clipboard-read"),
    ("clipboard-write", "clipboard-write"),
    ("fullscreen", "fullscreen"),
    ("autoplay", "autoplay"),
    ("display-capture", "display-capture"),
    ("midi", "midi"),
    ("picture-in-picture", "picture-in-picture"),
    ("screen-wake-lock", "screen-wake-lock"),
    ("speaker-selection", "speaker-selection"),
    ("accelerometer", "accelerometer"),
    ("gyroscope", "gyroscope"),
    ("magnetometer", "magnetometer"),
    ("ambient-light-sensor", "ambient-light-sensor"),
    ("bluetooth", "bluetooth"),
];

/// Build a dynamic Permissions-Policy header based on granted permissions
///
/// This takes the base deny-all policy and enables specific permissions
/// that the user has granted for this app.
///
/// # Arguments
/// * `granted_permissions` - Comma-separated string of granted permission names
///
/// # Returns
/// The complete Permissions-Policy header value
pub fn build_permissions_policy(granted_permissions: &str) -> String {
    // Parse granted permissions into a set for fast lookup
    let granted: std::collections::HashSet<&str> = granted_permissions
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // If no permissions granted, use the static deny-all policy
    if granted.is_empty() {
        return PERMISSIONS_POLICY_DENY_ALL.to_string();
    }

    // Build the policy by modifying the base policy
    // For each grantable permission, if granted, change from () to (self)
    let mut policy = PERMISSIONS_POLICY_DENY_ALL.to_string();

    for (perm_name, directive) in GRANTABLE_PERMISSIONS {
        if granted.contains(*perm_name) {
            // Replace "directive=()" with "directive=(self)"
            let deny_pattern = format!("{}=()", directive);
            let allow_pattern = format!("{}=(self)", directive);
            policy = policy.replace(&deny_pattern, &allow_pattern);
        }
    }

    policy
}

/// Permissions-Policy for an app served cross-origin isolated: the user's
/// grants plus `cross-origin-isolated=(self)`. Chromium honours `()` and
/// would switch isolation (and with it SharedArrayBuffer) back off.
pub fn build_isolated_permissions_policy(granted_permissions: &str) -> String {
    let policy = build_permissions_policy(granted_permissions);
    debug_assert!(policy.contains("cross-origin-isolated=()"));
    policy.replacen("cross-origin-isolated=()", "cross-origin-isolated=(self)", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_policy_allows_only_cross_origin_isolation_for_self() {
        let base = build_permissions_policy("camera");
        let isolated = build_isolated_permissions_policy("camera");
        assert!(base.contains("cross-origin-isolated=()"));
        assert!(isolated.contains("cross-origin-isolated=(self)"));
        assert!(!isolated.contains("cross-origin-isolated=()"));
        assert_eq!(base.replace("cross-origin-isolated=()", "cross-origin-isolated=(self)"), isolated);
        assert!(isolated.contains("camera=(self)") && isolated.contains("microphone=()"));
        assert!(build_isolated_permissions_policy("").contains("cross-origin-isolated=(self)"));
    }

    #[test]
    fn test_parse_permissions() {
        let perms = parse_permissions("microphone, camera, fullscreen");
        assert!(perms.contains(&MiniAppPermission::Microphone));
        assert!(perms.contains(&MiniAppPermission::Camera));
        assert!(perms.contains(&MiniAppPermission::Fullscreen));
        assert_eq!(perms.len(), 3);
    }

    #[test]
    fn test_serialize_permissions() {
        let mut perms = HashSet::new();
        perms.insert(MiniAppPermission::Camera);
        perms.insert(MiniAppPermission::Microphone);
        let serialized = serialize_permissions(&perms);
        // Should be sorted alphabetically
        assert!(serialized == "camera,microphone" || serialized == "microphone,camera");
    }

    #[test]
    fn test_parse_unknown_permission() {
        let perms = parse_permissions("microphone, unknown, camera");
        assert_eq!(perms.len(), 2); // unknown is filtered out
    }
}
