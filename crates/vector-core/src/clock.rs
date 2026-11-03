//! The time Vector stamps and judges by: this device's clock, corrected when the account's
//! relays agree it is off. Every HTTPS answer carries a `Date` (RFC 9110 §6.6.1), so any relay
//! can tell the time, whether or not it serves NIP-11; their median moves the clock, so one wrong
//! relay can't, and nothing of Vector's own is asked.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};

use nostr_sdk::prelude::{Timestamp, UnsignedEvent};

/// The account setting: `"false"` keeps the device's clock, anything else follows the relays.
pub const SETTING_KEY: &str = "clock_decentralised";

/// What the UI hears when the clock in force, or what is known about it, changes.
pub const EVENT: &str = "clock_changed";

/// How far the relays' clock is ahead of this device's, in ms, as last agreed.
static MEASURED_MS: AtomicI64 = AtomicI64::new(0);

/// Whether Vector runs on the relays' clock (the default) or the device's.
static DECENTRALISED: AtomicBool = AtomicBool::new(true);

/// The last check: how many relays answered, how many agreed, and when (device unix secs).
static ANSWERED: AtomicU8 = AtomicU8::new(0);
static AGREEING: AtomicU8 = AtomicU8::new(0);
static CHECKED_AT: AtomicU64 = AtomicU64::new(0);

/// `Date` has one-second resolution: a smaller gap is no evidence of a wrong clock.
const NOISE_MS: i64 = 2_000;

/// How close a relay must be to the median to count as agreeing with it.
const AGREE_MS: i64 = 5_000;

/// How many relays must agree before their time is used. Never two: when two disagree, there is
/// no telling which is wrong.
const QUORUM: usize = 3;

/// A sample whose round trip took longer than this can't place the time closely enough.
const MAX_ROUND_TRIP_MS: u64 = 8_000;

/// How often the relays are asked again.
pub(crate) const RESYNC: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The soonest an early check may follow the last one, so a burst of clock refusals asks once.
const MIN_RESYNC_SECS: u64 = 60;

/// Now, in unix ms, on the clock in force.
pub fn now_ms() -> u64 {
    let offset = offset_ms();
    #[cfg(test)]
    let offset = offset + TEST_SHIFT_MS.with(|s| s.get());
    device_ms().saturating_add_signed(offset)
}

#[cfg(test)]
thread_local! {
    static TEST_SHIFT_MS: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

/// Move this test thread's clock by `ms`, as a relays' correction would.
#[cfg(test)]
pub(crate) fn shift_for_test(ms: i64) {
    TEST_SHIFT_MS.with(|s| s.set(ms));
}

/// Now, in unix seconds, on the clock in force.
pub fn now_secs() -> u64 {
    now_ms() / 1000
}

/// Now, as the `created_at` or filter bound of an event. Use this, never `Timestamp::now()`.
pub fn timestamp() -> Timestamp {
    Timestamp::from_secs(now_secs())
}

/// The correction in force, in ms (positive when this device runs behind). Zero on the device clock.
pub fn offset_ms() -> i64 {
    if DECENTRALISED.load(Ordering::Relaxed) {
        MEASURED_MS.load(Ordering::Relaxed)
    } else {
        0
    }
}

fn device_ms() -> u64 {
    // device-clock: what every correction is measured from.
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Move an event nostr stamped with the device's clock onto the clock in force. A time the caller
/// chose (a backdated wrap, an edition's own date) is left alone, and so is the id it fixed.
pub fn restamp(mut unsigned: UnsignedEvent) -> UnsignedEvent {
    if offset_ms() == 0 || unsigned.created_at.as_secs().abs_diff(device_ms() / 1000) > 1 {
        return unsigned;
    }
    unsigned.created_at = timestamp();
    if unsigned.id.take().is_some() {
        unsigned.ensure_id();
    }
    unsigned
}

/// A length of time as a person says it: "40 seconds", "4 minutes", "2 hours".
pub fn describe_span(secs: u64) -> String {
    let (n, unit) = match secs {
        0..=89 => (secs, "second"),
        90..=5_399 => ((secs + 30) / 60, "minute"),
        5_400..=172_799 => ((secs + 1_800) / 3_600, "hour"),
        _ => ((secs + 43_200) / 86_400, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// Whether Vector runs on the relays' clock.
pub fn is_decentralised() -> bool {
    DECENTRALISED.load(Ordering::Relaxed)
}

/// Read the live account's choice. Called as its session starts.
pub fn load_setting() {
    let on = !matches!(crate::db::get_sql_setting(SETTING_KEY.to_string()), Ok(Some(v)) if v == "false" || v == "0");
    if DECENTRALISED.swap(on, Ordering::Relaxed) != on {
        announce();
    }
}

/// Choose the relays' clock or the device's, for this account, from now on.
pub fn set_decentralised(on: bool) -> Result<(), String> {
    crate::db::set_sql_setting(SETTING_KEY.to_string(), if on { "true" } else { "false" }.to_string())?;
    DECENTRALISED.store(on, Ordering::Relaxed);
    announce();
    Ok(())
}

/// What the clock is doing, for Settings.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    /// Running on the relays' clock.
    pub decentralised: bool,
    /// How far the relays' clock is ahead of this device's, in ms, whichever clock is in force.
    pub measured_offset_ms: i64,
    /// The correction applied now, in ms.
    pub offset_ms: i64,
    /// At the last check: relays that answered, and how many of them agreed.
    pub answered: u8,
    pub agreeing: u8,
    /// When the last check ran, in unix secs on the clock in force; 0 before the first.
    pub checked_at: u64,
}

pub fn status() -> Status {
    let checked = CHECKED_AT.load(Ordering::Relaxed);
    Status {
        decentralised: is_decentralised(),
        measured_offset_ms: MEASURED_MS.load(Ordering::Relaxed),
        offset_ms: offset_ms(),
        answered: ANSWERED.load(Ordering::Relaxed),
        agreeing: AGREEING.load(Ordering::Relaxed),
        checked_at: if checked == 0 { 0 } else { checked.saturating_add_signed(offset_ms() / 1000) },
    }
}

fn announce() {
    crate::traits::emit_event(EVENT, &status());
}

/// The offset `samples` (each a relay's clock minus this device's, in ms) agree on: their median,
/// when at least [`QUORUM`] lie within [`AGREE_MS`] of it, with how many do.
fn agreed(samples: &mut [i64]) -> Option<(i64, usize)> {
    if samples.len() < QUORUM {
        return None;
    }
    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let near = samples.iter().filter(|s| (*s - median).abs() <= AGREE_MS).count();
    (near >= QUORUM).then_some((median, near))
}

/// Ask `relays` for the time and correct the clock by what they agree on. Leaves it alone when
/// fewer than [`QUORUM`] answer alike, the device's clock is used until they do.
pub async fn sync_with(relays: &[String]) {
    let asked = relays.iter().take(8).map(|r| sample(r));
    let mut samples: Vec<i64> = futures_util::future::join_all(asked).await.into_iter().flatten().collect();
    let answered = samples.len();
    let agreement = agreed(&mut samples);
    ANSWERED.store(answered.min(u8::MAX as usize) as u8, Ordering::Relaxed);
    AGREEING.store(agreement.map_or(0, |(_, n)| n.min(u8::MAX as usize) as u8), Ordering::Relaxed);
    CHECKED_AT.store(device_ms() / 1000, Ordering::Relaxed);
    let offset = match agreement {
        Some((offset, _)) if offset.abs() >= NOISE_MS => offset,
        _ => 0,
    };
    if MEASURED_MS.swap(offset, Ordering::Relaxed) != offset {
        match agreement {
            Some(_) if offset != 0 => crate::log_warn!("[clock] this device's clock is {} ms off the relays'", -offset),
            Some(_) => {}
            None => crate::log_warn!("[clock] fewer than {QUORUM} relays agree on the time; using this device's clock"),
        }
    }
    announce();
}

static RESYNC_NOW: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Ask the relays again soon, rather than at the next hourly check: a server said this clock is off.
pub fn resync_soon() {
    let last = CHECKED_AT.load(Ordering::Relaxed);
    if last == 0 || device_ms() / 1000 >= last.saturating_add(MIN_RESYNC_SECS) {
        RESYNC_NOW.notify_one();
    }
}

/// Until the next check is due: the hourly one, or one [`resync_soon`] asked for.
pub(crate) async fn until_next_check() {
    let hourly = std::pin::pin!(crate::rt::time::sleep(RESYNC));
    let early = std::pin::pin!(RESYNC_NOW.notified());
    futures_util::future::select(hourly, early).await;
}

/// One relay's clock minus this device's, in ms, from the `Date` (and any `Age`) on its answer.
/// In a browser, only a relay that sends `Access-Control-Expose-Headers: Date` can answer.
async fn sample(relay: &str) -> Option<i64> {
    let url = http_url(relay)?;
    let client = crate::net::build_http_client(std::time::Duration::from_secs(10)).ok()?;
    let sent = device_ms();
    let resp = client
        .get(url)
        .header("Accept", "application/nostr+json")
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .ok()?;
    let back = device_ms();
    if back.saturating_sub(sent) > MAX_ROUND_TRIP_MS {
        return None;
    }
    let date = parse_http_date(resp.headers().get("date")?.to_str().ok()?)?;
    let age = resp.headers().get("age").and_then(|a| a.to_str().ok()?.trim().parse::<u64>().ok()).unwrap_or(0);
    // The relay stamped a whole second it was somewhere inside, met about mid-trip.
    let relay_ms = (date + age) * 1000 + 500;
    Some(relay_ms as i64 - (sent + back) as i64 / 2)
}

/// Where a relay answers over HTTP: its own address, path included.
fn http_url(relay: &str) -> Option<String> {
    let relay = relay.trim();
    relay.strip_prefix("wss://").map(|r| format!("https://{r}")).or_else(|| relay.strip_prefix("ws://").map(|r| format!("http://{r}")))
}

/// Unix seconds of an IMF-fixdate, `Sun, 06 Nov 1994 08:49:37 GMT`: the form RFC 9110 has every
/// server send.
fn parse_http_date(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let [_, day, month, year, time, "GMT"] = parts.as_slice() else { return None };
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| m == month)? as i64 + 1;
    let (day, year): (i64, i64) = (day.parse().ok()?, year.parse().ok()?);
    let hms: Vec<i64> = time.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let [h, m, sec] = hms.as_slice() else { return None };
    if !(1..=31).contains(&day) || *h > 23 || *m > 59 || *sec > 60 {
        return None;
    }
    // Days from 1970-01-01 to the civil date (proleptic Gregorian).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + m * 60 + sec).ok()
}

/// Every place under `src_root` that reads the time off the device rather than this clock, as
/// `path:line` relative to `crate_root`: `Timestamp::now()`, unix time from `SystemTime`, and
/// signing with bare keys through nostr's `finalize`, which stamps the device's clock. A read
/// that must stay on the device (a file's age, a log line, an id) says why with
/// `// device-clock: <why>` on its line or the three above.
pub fn device_clock_reads(crate_root: &std::path::Path, src_root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![src_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(crate_root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if path.extension().and_then(|e| e.to_str()) != Some("rs") || ["/tests", "fixtures", "bench"].iter().any(|t| rel.contains(t)) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            found.extend(device_reads_in(&src).into_iter().map(|n| format!("{rel}:{n}")));
        }
    }
    found.sort();
    found
}

/// The 1-based lines of one file's shipping code that read the device's clock unmarked.
fn device_reads_in(src: &str) -> Vec<usize> {
    let lines: Vec<&str> = src.lines().collect();
    if lines.iter().map(|l| l.trim()).find(|l| !l.is_empty() && !l.starts_with("//")) == Some("#![cfg(test)]") {
        return Vec::new();
    }
    let cut = lines
        .windows(2)
        .position(|w| {
            w[0].trim() == "#[cfg(test)]"
                && ["mod ", "pub mod ", "pub(crate) mod "].iter().any(|m| w[1].trim_start().starts_with(m))
        })
        .unwrap_or(lines.len());
    let shipping = &lines[..cut];
    let signs_bare = |l: &str| {
        l.split(concat!(".final", "ize(")).skip(1).any(|rest| rest.trim_start().chars().next().is_some_and(|c| c != ')'))
    };
    (0..shipping.len())
        .filter(|&i| {
            let l = shipping[i];
            let unix = l.contains(concat!("SystemTime::", "now()"))
                && shipping[i..(i + 4).min(cut)].iter().any(|n| n.contains(concat!("UNIX_", "EPOCH")));
            !l.trim_start().starts_with("//")
                && (l.contains(concat!("Timestamp::", "now()")) || unix || signs_bare(l))
                && !shipping[i.saturating_sub(3)..=i].iter().any(|p| p.contains("device-clock:"))
        })
        .map(|i| i + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_http_date_reads_as_its_unix_time() {
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(parse_http_date("Wed, 21 Oct 2026 16:59:42 GMT"), Some(1_792_601_982));
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"), None, "only the form servers must send");
        assert_eq!(parse_http_date("Sun, 06 Foo 1994 08:49:37 GMT"), None);
    }

    #[test]
    fn the_clock_moves_only_on_relays_that_agree() {
        assert_eq!(agreed(&mut []), None);
        assert_eq!(agreed(&mut [90_000]), None, "one relay is no agreement");
        assert_eq!(agreed(&mut [-120_000, -119_000]), None, "nor are two, even agreeing: never two compasses");
        assert_eq!(agreed(&mut [90_000, 91_000, 89_500, -3_000_000]), Some((90_000, 3)), "a lone liar is outvoted");
        assert_eq!(agreed(&mut [90_000, 91_000, -3_000_000]), None, "two against one is not three");
        assert_eq!(agreed(&mut [-120_000, -119_000, -121_000]), Some((-120_000, 3)));
    }

    #[test]
    fn nothing_in_the_core_reads_the_device_clock_unmarked() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let found = device_clock_reads(root, &root.join("src"));
        assert!(
            found.is_empty(),
            "these read the device's clock: use crate::clock (timestamp, now_secs, now_ms, FinalizeOnClock), \
             or mark the site `// device-clock: <why>`:\n  {}",
            found.join("\n  ")
        );
    }

    #[test]
    fn the_audit_sees_each_kind_of_device_read() {
        let src = "fn a() { Timestamp::now(); }\nfn b() {\n    let t = SystemTime::now()\n        .duration_since(UNIX_EPOCH);\n}\nfn c() { b.finalize(&keys); h.finalize(); }\n// device-clock: ok\nfn d() { Timestamp::now(); }\n#[cfg(test)]\nmod tests { fn e() { Timestamp::now(); } }\n";
        assert_eq!(device_reads_in(&src.replace("Timestamp::now", concat!("Timestamp::", "now"))), vec![1, 3, 6]);
    }

    #[test]
    fn restamping_moves_only_a_device_stamp() {
        use nostr_sdk::prelude::FinalizeUnsignedEvent;
        let keys = nostr_sdk::prelude::Keys::generate();
        let builder = || nostr_sdk::prelude::EventBuilder::new(nostr_sdk::prelude::Kind::TextNote, "hi");
        shift_for_test(0);
        MEASURED_MS.store(-240_000, Ordering::Relaxed);
        DECENTRALISED.store(true, Ordering::Relaxed);

        let mut defaulted = builder().finalize_unsigned(keys.public_key());
        defaulted.ensure_id();
        let before = defaulted.id;
        let moved = restamp(defaulted);
        assert_eq!(moved.created_at.as_secs(), device_ms() / 1000 - 240, "onto the relays' clock");
        assert_ne!(moved.id, before, "and its id follows");

        let chosen = builder().custom_created_at(Timestamp::from_secs(1_000)).finalize_unsigned(keys.public_key());
        assert_eq!(restamp(chosen).created_at.as_secs(), 1_000, "a chosen time stays");

        DECENTRALISED.store(false, Ordering::Relaxed);
        let device = builder().finalize_unsigned(keys.public_key());
        let stamp = device.created_at;
        assert_eq!(restamp(device).created_at, stamp, "the device clock is left alone when chosen");
        assert_eq!(offset_ms(), 0);
        assert_eq!(status().measured_offset_ms, -240_000, "but what the relays said is still shown");

        DECENTRALISED.store(true, Ordering::Relaxed);
        MEASURED_MS.store(0, Ordering::Relaxed);
    }
}
