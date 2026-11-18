//! `humanizeUploadError` (main.js): a raw upload error as a titled popup.

use crate::kit::escape;
use crate::t;
use gpui::SharedString;

pub struct Humanized {
    pub title: SharedString,
    /// Popup markup: `<br>` and escaped text.
    pub body: String,
    pub fine: Option<String>,
    /// The network refused it: offer Routing, not another file.
    pub routing: bool,
}

fn any(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| lower.contains(n))
}

fn status(lower: &str, code: &str) -> bool {
    lower.match_indices("status").any(|(i, _)| {
        let rest = lower[i + 6..].trim_start();
        rest.len() < lower.len() - i - 6 && rest.starts_with(code)
    })
}

/// `/storage on .+ is full/`
fn storage_full(lower: &str) -> bool {
    lower
        .find("storage on ") // i18n-exempt: core's error text
        .is_some_and(|i| lower[i + 11..].get(1..).is_some_and(|r| r.contains(" is full"))) // i18n-exempt: core's error text
}

pub fn humanize(raw: &str) -> Humanized {
    let trimmed = raw.trim_start();
    let msg = trimmed
        // i18n-exempt: core's error text
        .get(..14)
        .filter(|p| p.eq_ignore_ascii_case("upload failed:")) // i18n-exempt: core's error text
        .map_or(trimmed, |_| &trimmed[14..])
        .trim();
    let lower = msg.to_lowercase();

    // i18n-exempt: core's error text
    if msg.starts_with("No media server accepted the file:") {
        let items = msg
            .split('\n')
            .skip(1)
            .map(|l| format!("• {}", escape(l.trim_start_matches('•').trim_start())))
            .collect::<Vec<_>>()
            .join("<br>");
        return Humanized {
            title: t::upload_no_server_accepted(),
            body: items,
            fine: Some(t::upload_servers_hint().into()),
            routing: false,
        };
    }

    // Core's needles, the lead it opens with (`<lead>, so …`, which is cut), and the title.
    let network: [(&[&str], &str, fn() -> SharedString); 5] = [
        // i18n-exempt: core's error text
        (&["\u{0}i2p-only is on"], "i2p-only is on, so ", t::upload_net_i2p_only),
        // i18n-exempt: core's error text
        (&["only reachable over"], "needs another network, so ", t::upload_net_other),
        // i18n-exempt: core's error text
        (&["network changed"], "the network changed, so ", t::upload_net_changed),
        // i18n-exempt: core's error text
        (&["this build doesn't include"], "network not in this build, so ", t::upload_net_missing),
        (
            // i18n-exempt: core's error text
            &["still connecting", "i2p router", "sam username", "sam bridge", "speaks sam", "sam is too old", "i2p session", "outproxy", "tor timed out", "local proxy", "choose how vector connects"],
            // i18n-exempt: core's error text
            "not connected yet, so ",
            t::upload_net_connecting,
        ),
    ];
    for (needles, lead, title) in network {
        let hit = needles.iter().any(|n| match n.strip_prefix('\u{0}') {
            Some(prefix) => lower.starts_with(prefix),
            None => lower.contains(n),
        });
        if hit {
            let rest = if lower.starts_with(&lead) { &msg[lead.len()..] } else { msg };
            let mut chars = rest.chars();
            let text = chars
                .next()
                .map(|f| f.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default();
            return Humanized { title: title(), body: escape(&text), fine: None, routing: true };
        }
    }

    // Core's error texts are needles here, never shown.
    let title: Option<fn() -> SharedString> = if lower.contains("upload allowance") { // i18n-exempt: core's error text
        Some(t::upload_allowance)
    } else if storage_full(&lower) || lower.contains("out of space") { // i18n-exempt: core's error text
        Some(t::upload_storage_full)
    // i18n-exempt: core's error text
    } else if any(&lower, &["allows files up to", "won't accept a file this large", "payload too large"])
        || status(&lower, "413")
    {
        Some(t::upload_too_large)
    } else if any(
        &lower,
        // i18n-exempt: core's error text
        &["doesn't accept", "could not be processed", "file type not detected", "not allowed", "unsupported"],
    ) || status(&lower, "415")
    {
        Some(t::upload_bad_type)
    } else if lower.contains("accounts it recognises") { // i18n-exempt: core's error text
        Some(t::upload_unknown_account)
    } else if lower.contains("clock may be wrong") { // i18n-exempt: core's error text
        Some(t::upload_clock)
    } else if lower.contains("rate-limiting") {
        Some(t::upload_slow_down)
    // i18n-exempt: core's error text
    } else if any(&lower, &["upload signature", "upload authorization", "unauthorized"]) || status(&lower, "401") {
        Some(t::upload_rejected)
    // i18n-exempt: core's error text
    } else if any(&lower, &["could not be reached", "stopped accepting", "stalled"]) {
        Some(t::upload_unreachable)
    } else if lower.contains("no media server is configured") { // i18n-exempt: core's error text
        Some(t::upload_no_server)
    } else {
        None
    };
    match title {
        Some(title) => Humanized { title: title(), body: escape(msg), fine: Some(t::upload_servers_hint().into()), routing: false },
        None => Humanized {
            title: t::upload_failed(),
            body: t::upload_failed_text().into(),
            fine: Some(msg.to_string()),
            routing: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(humanize("Upload failed: status 413").title, "File too large");
        assert_eq!(humanize("server answered status  401").title, "Media server rejected your account");
        assert_eq!(humanize("The storage on x.com is full").title, "Storage full");
        assert_eq!(humanize("boom").title, "File send failed");
        let n = humanize("The network changed, so this upload stopped.");
        assert!(n.routing);
        assert_eq!(n.body, "This upload stopped.");
        assert_eq!(humanize("Tor is still connecting.").title, "Not connected yet");
        let l = humanize("No media server accepted the file:\n• a said no\n• b said no");
        assert_eq!(l.body, "• a said no<br>• b said no");
    }
}
