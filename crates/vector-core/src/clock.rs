//! The time Vector stamps and judges by: this device's clock, corrected when the account's
//! relays agree it is off. Every HTTPS answer carries a `Date` (RFC 9110 §6.6.1), so any relay
//! can tell the time, whether or not it serves NIP-11; their median moves the clock, so one wrong
//! relay can't, and nothing of Vector's own is asked.

use std::sync::atomic::{AtomicI64, Ordering};

/// The correction: how far the relays' clock is ahead of this device's, in ms.
static OFFSET_MS: AtomicI64 = AtomicI64::new(0);

/// `Date` has one-second resolution: a smaller gap is no evidence of a wrong clock.
const NOISE_MS: i64 = 2_000;

/// How close two relays must be to count as agreeing.
const AGREE_MS: i64 = 5_000;

/// A sample whose round trip took longer than this can't place the time closely enough.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_ROUND_TRIP_MS: u64 = 8_000;

/// How often the relays are asked again.
pub(crate) const RESYNC: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Now, in unix ms, corrected.
pub fn now_ms() -> u64 {
    let offset = OFFSET_MS.load(Ordering::Relaxed);
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

/// Now, in unix seconds, corrected.
pub fn now_secs() -> u64 {
    now_ms() / 1000
}

/// The correction in force, in ms (positive when this device runs behind).
pub fn offset_ms() -> i64 {
    OFFSET_MS.load(Ordering::Relaxed)
}

fn device_ms() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The offset `samples` (each a relay's clock minus this device's, in ms) agree on: their median,
/// when at least two lie within [`AGREE_MS`] of it.
fn agreed(samples: &mut [i64]) -> Option<i64> {
    if samples.len() < 2 {
        return None;
    }
    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    (samples.iter().filter(|s| (*s - median).abs() <= AGREE_MS).count() >= 2).then_some(median)
}

/// Ask `relays` for the time and correct the clock by what they agree on. Leaves it alone when
/// fewer than two answer alike.
pub async fn sync_with(relays: &[String]) {
    let asked = relays.iter().take(8).map(|r| sample(r));
    let mut samples: Vec<i64> = futures_util::future::join_all(asked).await.into_iter().flatten().collect();
    if let Some(offset) = agreed(&mut samples) {
        let offset = if offset.abs() < NOISE_MS { 0 } else { offset };
        if OFFSET_MS.swap(offset, Ordering::Relaxed) != offset && offset != 0 {
            crate::log_warn!("[clock] this device's clock is {} ms off the relays'; correcting", -offset);
        }
    }
}

/// One relay's clock minus this device's, in ms, from the `Date` (and any `Age`) on its answer.
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
fn http_url(relay: &str) -> Option<String> {
    let relay = relay.trim();
    relay.strip_prefix("wss://").map(|r| format!("https://{r}")).or_else(|| relay.strip_prefix("ws://").map(|r| format!("http://{r}")))
}

/// A browser hides `Date` from a cross-origin page, so Vector Web keeps the device's clock.
#[cfg(target_arch = "wasm32")]
async fn sample(_relay: &str) -> Option<i64> {
    None
}

/// Unix seconds of an IMF-fixdate, `Sun, 06 Nov 1994 08:49:37 GMT`: the form RFC 9110 has every
/// server send.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
        assert_eq!(agreed(&mut [90_000, 91_000, -3_000_000]), Some(90_000), "a lone liar is outvoted");
        assert_eq!(agreed(&mut [90_000, 400_000]), None, "two that disagree move nothing");
        assert_eq!(agreed(&mut [-120_000, -119_000]), Some(-119_000));
    }
}
