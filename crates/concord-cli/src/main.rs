//! `concord`: read a Concord Community from a public invite link with no account. Nothing is
//! stored and nothing is published: the transport below refuses every publish, so no Guestbook
//! Join or presence can leave this process, and relays see only reads (AUTH, where a relay asks,
//! is answered as the plane being read, never as a person).

use std::io::Read;

use async_trait::async_trait;
use nostr_sdk::prelude::{Event, Keys};
use vector_core::community::transport::{LiveTransport, PlaneFetch, Query, Transport};
use vector_core::community::v2::service::{inspect_public_link, Inspection};

const USAGE: &str = "\
usage: concord inspect <invite-url | -> [--relay <wss://...>]... [--json | --filter]

  <invite-url>   the whole link, fragment included; `-` reads it from stdin, which keeps
                 the token out of shell history and the process list
  --relay        read only from this relay (repeatable); default: the link's own relays
                 for the bundle, then the Community's
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
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--relay" => relays.push(args.next().unwrap_or_else(|| fail("--relay needs a relay URL"))),
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
    let seen = inspect_public_link(&transport, &url, relays).await.unwrap_or_else(|e| fail(&e));
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
    println!("  link      {}", seen.link_signer);
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
}

fn fail(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(2);
}
