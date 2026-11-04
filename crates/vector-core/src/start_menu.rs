//! The widescreen Main Menu's pins: what a user keeps one click away, in the
//! rail's shape ([`RailLayout`]) and synced the same way, as its own document
//! (`vector/start`, [`crate::synced_prefs::Pref::Start`]).
//!
//! A pin's key names what it opens, in the grammar links already use, so a key
//! means the same thing on every device and needs nothing stored beside it:
//!
//! ```text
//!   app/<nexus app id>                 a Mini App from the Nexus, launched
//!   dm/<npub>                          a conversation
//!   c/<community>[/<channel>]          a community, or one of its channels
//!   settings[/<category>[/<section>]]  a place in Settings
//!   nexus | requests                   the Nexus, the Requests list
//! ```
//!
//! What a key shows (a name, an icon) is resolved on the device from what it
//! holds; a key it cannot resolve keeps its place, as the rail keeps a community
//! it has not synced.

use crate::rail_layout::RailLayout;

/// Enough for any launcher, few enough that a malformed document stays small.
pub const MAX_PINS: usize = 120;

/// Whether `key` is a pin this build can open.
pub fn is_pin_key(key: &str) -> bool {
    if key.len() > 160 {
        return false;
    }
    let parts: Vec<&str> = key.split('/').collect();
    match parts.as_slice() {
        ["app", id] => crate::webxdc::is_safe_app_id(id),
        ["dm", npub] => npub.starts_with("npub1") && npub.len() == 63 && npub.bytes().all(|b| b.is_ascii_alphanumeric()),
        ["c", ids @ ..] => (1..=2).contains(&ids.len()) && ids.iter().all(|id| is_hex_id(id)),
        ["settings", rest @ ..] => rest.len() <= 2 && rest.iter().all(|s| is_settings_id(s)),
        ["nexus"] | ["requests"] => true,
        _ => false,
    }
}

fn is_hex_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_settings_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Pin `key` at the end. `Ok(false)` when it is pinned already.
pub fn pin(layout: &mut RailLayout, key: &str) -> Result<bool, String> {
    if !is_pin_key(key) {
        return Err("That can't be pinned".to_string());
    }
    if !layout.is_editable() {
        return Err("Your pins were arranged by a newer Vector; update to change them".to_string());
    }
    if layout.contains(key) {
        return Ok(false);
    }
    if layout.flatten().len() >= MAX_PINS {
        return Err(format!("Start holds up to {MAX_PINS} pins"));
    }
    layout.nodes.push(crate::rail_layout::RailNode::item(key));
    Ok(true)
}

/// Take `key` off Start, out of whichever folder holds it. `false` when it wasn't pinned.
pub fn unpin(layout: &mut RailLayout, key: &str) -> bool {
    if !layout.contains(key) {
        return false;
    }
    layout.remove_key(key);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn npub() -> String {
        format!("npub1{}", "q".repeat(58))
    }

    #[test]
    fn a_pin_names_what_it_opens_in_the_link_grammar() {
        for key in [
            "app/chess.v2_beta".to_string(),
            format!("dm/{}", npub()),
            format!("c/{A}"),
            format!("c/{A}/{A}"),
            "settings".to_string(),
            "settings/privacy/streamer".to_string(),
            "nexus".to_string(),
            "requests".to_string(),
        ] {
            assert!(is_pin_key(&key), "{key}");
        }
        for key in [
            "app/../etc".to_string(),
            "dm/notanpub".to_string(),
            format!("c/{A}/{A}/{A}"),
            format!("c/{}", A.to_uppercase()),
            "settings/Privacy".to_string(),
            "settings/a/b/c".to_string(),
            "nexus/chess".to_string(),
            "profile".to_string(),
            String::new(),
        ] {
            assert!(!is_pin_key(&key), "{key}");
        }
    }

    #[test]
    fn pinning_appends_once_and_unpinning_finds_it_in_a_folder() {
        let mut l = RailLayout::default();
        assert_eq!(pin(&mut l, "nexus"), Ok(true));
        assert_eq!(pin(&mut l, "nexus"), Ok(false), "pinned twice is pinned once");
        assert_eq!(pin(&mut l, "requests"), Ok(true));
        assert!(pin(&mut l, "nope").is_err());
        assert_eq!(l.flatten(), vec!["nexus".to_string(), "requests".to_string()]);

        let drop = crate::rail_layout::RailDrop::Combine { with_key: "nexus".into() };
        l.apply_drop(&crate::rail_layout::RailDrag::Item { key: "requests".into() }, &drop, "f1").unwrap();
        assert!(matches!(l.nodes.as_slice(), [crate::rail_layout::RailNode::Folder { .. }]));
        assert!(unpin(&mut l, "requests"));
        assert!(!unpin(&mut l, "requests"));
        assert_eq!(l.flatten(), vec!["nexus".to_string()]);
    }

    #[test]
    fn start_holds_a_bounded_number_of_pins() {
        let mut l = RailLayout::default();
        for i in 0..MAX_PINS {
            pin(&mut l, &format!("app/app{i}")).unwrap();
        }
        assert!(pin(&mut l, "app/one-more").is_err());
    }
}
