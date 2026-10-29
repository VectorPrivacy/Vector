//! `concord`: read a Concord Community from a public invite link or an invite bundle with no
//! account. Nothing is stored and nothing is published: the transport below refuses every
//! publish, so no Guestbook Join or presence can leave this process, and relays see only reads
//! (AUTH, where a relay asks, is answered as the plane being read, never as a person).

use std::io::Read;

use async_trait::async_trait;
use nostr_sdk::prelude::{Event, Keys};
use vector_core::community::transport::{LiveTransport, PlaneFetch, Query, Transport};
use vector_core::community::v2::invite::CommunityInvite;
use vector_core::community::v2::service::{inspect_bundle, inspect_public_link, Inspection, MessageRead};

const USAGE: &str = "\
usage: concord inspect <invite-url | bundle | -> [--relay <wss://...>]... [--json | --filter]

  <invite-url>   the whole link, fragment included
  <bundle>       a CORD-05 invite bundle (JSON), as Vector's Copy Invite Bundle gives it
  -              either, read from stdin: keeps the token or the bundle's keys out of
                 shell history and the process list
  --relay        read only from this relay (repeatable); default: the link's own relays
                 for the bundle, then the Community's
  --messages <n> also print the newest n posts, across every channel the keys can read
  --channel <c>  only this channel's posts, by name or id (repeatable; implies --messages 20)
  --json         everything as JSON
  --filter       only a relay filter matching every named plane: {\"authors\":[...]}";

/// Reads go to the live network; publishes are refused before they reach a relay, and said so.
struct ReadOnly(LiveTransport);

fn refuse(event: &Event) -> Result<(), String> {
    eprintln!("refused to publish a kind {} event: concord is read-only", event.kind.as_u16());
    Err("concord is read-only".into())
}

#[async_trait]
impl Transport for ReadOnly {
    async fn publish(&self, event: &Event, _: &[String]) -> Result<(), String> {
        refuse(event)
    }

    async fn fetch(&self, query: &Query, relays: &[String]) -> Result<Vec<Event>, String> {
        self.0.fetch(query, relays).await
    }

    async fn fetch_plane(&self, plane: &Keys, query: &Query, relays: &[String]) -> Result<PlaneFetch, String> {
        self.0.fetch_plane(plane, query, relays).await
    }

    async fn publish_durable(&self, event: &Event, _: &[String]) -> Result<(), String> {
        refuse(event)
    }
}

enum Output {
    Text,
    Json,
    Filter,
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("inspect") {
        fail(USAGE);
    }
    let (mut url, mut relays, mut output) = (None, Vec::new(), Output::Text);
    let (mut count, mut channels) = (None, Vec::new());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => relays.push(args.next().unwrap_or_else(|| fail("--relay needs a relay URL"))),
            "--messages" => {
                let n = args.next().and_then(|n| n.parse::<usize>().ok()).filter(|n| *n > 0);
                count = Some(n.unwrap_or_else(|| fail("--messages needs a count above 0")));
            }
            "--channel" => channels.push(args.next().unwrap_or_else(|| fail("--channel needs a name or id"))),
            "--json" => output = Output::Json,
            "--filter" => output = Output::Filter,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ if url.is_none() => url = Some(arg),
            _ => fail(USAGE),
        }
    }
    let mut url = url.unwrap_or_else(|| fail(USAGE));
    if url == "-" {
        url.clear();
        std::io::stdin().read_to_string(&mut url).unwrap_or_else(|e| fail(&format!("stdin: {e}")));
        url = url.trim().to_string();
    }

    // No account and no database: the network is chosen here rather than loaded from one.
    vector_core::transport::set_preference(Some(vector_core::transport::Kind::Clearnet));
    vector_core::state::set_nostr_client(vector_core::nostr_client_builder().build());

    let transport = ReadOnly(LiveTransport::new());
    let relays = (!relays.is_empty()).then_some(relays.as_slice());
    let read = (count.is_some() || !channels.is_empty()).then(|| MessageRead { count: count.unwrap_or(20), channels });
    let seen = if url.starts_with('{') {
        let bundle = CommunityInvite::from_bundle_json(&url).unwrap_or_else(|e| fail(&format!("bundle: {e}")));
        inspect_bundle(&transport, &bundle, relays, read.as_ref()).await
    } else {
        inspect_public_link(&transport, &url, relays, read.as_ref()).await
    }
    .unwrap_or_else(|e| fail(&e));
    match output {
        Output::Json => println!("{}", serde_json::to_string_pretty(&seen).expect("an inspection serializes")),
        Output::Filter => println!("{}", serde_json::json!({ "authors": seen.planes.iter().map(|p| &p.pk).collect::<Vec<_>>() })),
        Output::Text => print_text(&seen),
    }
}

fn print_text(seen: &Inspection) {
    println!("{}  ({})", seen.name, seen.community_id);
    println!("  owner     {}", seen.owner);
    println!("  verified  {}", if seen.verified { "yes, the owner's genesis is under the link's root" } else { "NO: the Control Plane couldn't be read, so this is the bundle's view" });
    println!("  epoch     {}", seen.root_epoch);
    println!("  relays    {}", seen.relays.join(" "));
    if let Some(signer) = &seen.link_signer {
        println!("  link      {signer}");
    }
    println!("\nchannels ({})", seen.channels.len());
    for c in &seen.channels {
        let mut what = vec![if c.private { "private" } else { "public" }.to_string(), format!("epoch {}", c.epoch)];
        if c.keyless {
            what.push("no key in this link".into());
        }
        if c.deleted {
            what.push("deleted".into());
        }
        println!("  #{:<24} {}", c.name, what.join(", "));
    }
    println!("\nplanes ({})", seen.planes.len());
    for p in &seen.planes {
        let kinds: Vec<String> = p.kinds.iter().map(u16::to_string).collect();
        println!("  {}  {:<40} {}", p.pk, p.role, kinds.join(","));
    }
    if !seen.gaps.is_empty() {
        println!("\nout of reach");
        for g in &seen.gaps {
            println!("  {g}");
        }
    }
    if !seen.messages.is_empty() {
        println!("\nmessages ({})", seen.messages.len());
        for m in &seen.messages {
            let mut lines = m.content.lines();
            let first = lines.next().unwrap_or("");
            let edited = if m.edited { " (edited)" } else { "" };
            println!("  {}  #{}  {}: {first}{edited}", utc(m.at_ms / 1000), m.channel, short(&m.author));
            for line in lines {
                println!("      {line}");
            }
        }
    }
}

/// `npub1abcdef…wxyz`: enough to tell people apart in a listing.
fn short(npub: &str) -> String {
    if npub.len() <= 18 {
        return npub.to_string();
    }
    format!("{}…{}", &npub[..11], &npub[npub.len() - 4..])
}

/// `YYYY-MM-DD HH:MM` in UTC, from unix seconds.
fn utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", rem / 3_600, rem % 3_600 / 60)
}

fn fail(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_reads_unix_seconds_across_leap_years_and_centuries() {
        assert_eq!(super::utc(0), "1970-01-01 00:00");
        assert_eq!(super::utc(951_782_400), "2000-02-29 00:00");
        assert_eq!(super::utc(1_761_750_000), "2025-10-29 15:00");
        assert_eq!(super::utc(4_107_542_399), "2100-02-28 23:59");
    }
}
