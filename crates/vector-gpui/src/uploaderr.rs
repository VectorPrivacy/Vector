//! `humanizeUploadError` (main.js): a raw upload error as a titled popup.

use crate::kit::escape;

pub struct Humanized {
    pub title: &'static str,
    /// Popup markup: `<br>` and escaped text.
    pub body: String,
    pub fine: Option<String>,
    /// The network refused it: offer Routing, not another file.
    pub routing: bool,
}

const SERVERS: &str = "Your media servers are in Settings → Network.";

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
        .find("storage on ")
        .is_some_and(|i| lower[i + 11..].get(1..).is_some_and(|r| r.contains(" is full")))
}

pub fn humanize(raw: &str) -> Humanized {
    let trimmed = raw.trim_start();
    let msg = trimmed
        .get(..14)
        .filter(|p| p.eq_ignore_ascii_case("upload failed:"))
        .map_or(trimmed, |_| &trimmed[14..])
        .trim();
    let lower = msg.to_lowercase();

    if msg.starts_with("No media server accepted the file:") {
        let items = msg
            .split('\n')
            .skip(1)
            .map(|l| format!("• {}", escape(l.trim_start_matches('•').trim_start())))
            .collect::<Vec<_>>()
            .join("<br>");
        return Humanized {
            title: "No media server accepted the file",
            body: items,
            fine: Some(SERVERS.into()),
            routing: false,
        };
    }

    let network: [(&[&str], &'static str); 5] = [
        (&["\u{0}i2p-only is on"], "I2P-Only is on"),
        (&["only reachable over"], "Needs another network"),
        (&["network changed"], "The network changed"),
        (&["this build doesn't include"], "Network not in this build"),
        (
            &[
                "still connecting",
                "i2p router",
                "sam username",
                "sam bridge",
                "speaks sam",
                "sam is too old",
                "i2p session",
                "outproxy",
                "tor timed out",
                "local proxy",
                "choose how vector connects",
            ],
            "Not connected yet",
        ),
    ];
    for (needles, title) in network {
        let hit = needles.iter().any(|n| match n.strip_prefix('\u{0}') {
            Some(prefix) => lower.starts_with(prefix),
            None => lower.contains(n),
        });
        if hit {
            let lead = format!("{}, so ", title.to_lowercase());
            let rest = if lower.starts_with(&lead) { &msg[lead.len()..] } else { msg };
            let mut chars = rest.chars();
            let text = chars
                .next()
                .map(|f| f.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default();
            return Humanized { title, body: escape(&text), fine: None, routing: true };
        }
    }

    let title = if lower.contains("upload allowance") {
        Some("Upload allowance used up")
    } else if storage_full(&lower) || lower.contains("out of space") {
        Some("Storage full")
    } else if any(&lower, &["allows files up to", "won't accept a file this large", "payload too large"])
        || status(&lower, "413")
    {
        Some("File too large")
    } else if any(
        &lower,
        &["doesn't accept", "could not be processed", "file type not detected", "not allowed", "unsupported"],
    ) || status(&lower, "415")
    {
        Some("File type not supported")
    } else if lower.contains("accounts it recognises") {
        Some("Media server doesn’t recognise your account")
    } else if lower.contains("clock may be wrong") {
        Some("Check this device’s clock")
    } else if lower.contains("rate-limiting") {
        Some("Slow down a moment")
    } else if any(&lower, &["upload signature", "upload authorization", "unauthorized"]) || status(&lower, "401") {
        Some("Media server rejected your account")
    } else if any(&lower, &["could not be reached", "stopped accepting", "stalled"]) {
        Some("Media server unreachable")
    } else if lower.contains("no media server is configured") {
        Some("No media server configured")
    } else {
        None
    };
    match title {
        Some(title) => Humanized { title, body: escape(msg), fine: Some(SERVERS.into()), routing: false },
        None => Humanized {
            title: "File send failed",
            body: "Vector could not send this file. Check your connection and try again, or pick a different file."
                .into(),
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
