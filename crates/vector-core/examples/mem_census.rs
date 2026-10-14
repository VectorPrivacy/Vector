//! Live-heap census of the chat state a steady-state account holds, through the same
//! `ChatState` paths the app uses. Counts every live allocation, rounded to the macOS
//! malloc size classes, so a layout change shows up in both bytes and block count.
//!
//! `cargo run --release -p vector-core --example mem_census`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

struct Counting;
static BYTES: AtomicIsize = AtomicIsize::new(0);
static BLOCKS: AtomicIsize = AtomicIsize::new(0);

/// macOS malloc: 16-byte quantum up to 1008 B, 512-byte quantum up to 127 KB, pages above.
fn class(n: usize) -> isize {
    let r = if n <= 1008 { n.div_ceil(16) * 16 } else if n <= 127 * 1024 { n.div_ceil(512) * 512 } else { n.div_ceil(16384) * 16384 };
    r.max(16) as isize
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BYTES.fetch_add(class(l.size()), Relaxed);
        BLOCKS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        BYTES.fetch_sub(class(l.size()), Relaxed);
        BLOCKS.fetch_sub(1, Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        BYTES.fetch_add(class(new) - class(l.size()), Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn live() -> (isize, isize) {
    (BYTES.load(Relaxed), BLOCKS.load(Relaxed))
}

/// Deterministic xorshift, so every run builds the same account.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pct(&mut self, p: u64) -> bool {
        self.below(100) < p
    }
    fn hex(&mut self) -> String {
        (0..4).map(|_| format!("{:016x}", self.next())).collect()
    }
    fn text(&mut self, min: usize, max: usize) -> String {
        const WORDS: &[&str] = &["the", "vector", "relay", "message", "privacy", "nostr", "community", "hello", "build", "ship", "today", "anyone", "works", "nice", "thanks", "check", "this", "out", "release", "soon"];
        let target = min + self.below((max - min) as u64) as usize;
        let mut s = String::new();
        while s.len() < target {
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(WORDS[self.below(WORDS.len() as u64) as usize]);
        }
        s
    }
}

fn npub(rng: &mut Rng) -> String {
    use nostr_sdk::prelude::{Keys, SecretKey, ToBech32};
    loop {
        let mut b = [0u8; 32];
        for c in b.chunks_mut(8) {
            c.copy_from_slice(&rng.next().to_le_bytes());
        }
        if let Ok(sk) = SecretKey::from_slice(&b) {
            return Keys::new(sk).public_key().to_bech32().unwrap();
        }
    }
}

const PROFILES: usize = 2000;
const FETCHED: usize = 1200;
const DMS: usize = 100;
const COMMUNITIES: usize = 15;
const CHANNELS: usize = 150;
const HELD: usize = 5;
const HELD_MESSAGES: usize = 1000;
const TAIL_MESSAGES: usize = 20;

fn main() {
    use vector_core::profile::SlimProfile;
    use vector_core::state::ChatState;
    use vector_core::types::{Attachment, EditEntry, ImageMetadata, Message, Reaction, SiteMetadata};

    let mut rng = Rng(0x9E3779B97F4A7C15);
    let npubs: Vec<String> = (0..PROFILES).map(|_| npub(&mut rng)).collect();
    let me = npubs[0].clone();
    let app = "/Users/jskitty/Library/Application Support/io.vectorapp";
    vector_core::db::set_app_data_dir(std::path::PathBuf::from(app));
    let community_ids: Vec<String> = (0..COMMUNITIES).map(|_| rng.hex()).collect();
    let primaries: Vec<String> = (0..COMMUNITIES).map(|_| rng.hex()).collect();
    let channels: Vec<String> = (0..CHANNELS).map(|i| if i < COMMUNITIES { primaries[i].clone() } else { rng.hex() }).collect();
    let chat_ids: Vec<String> = npubs[1..=DMS].iter().cloned().chain(channels.iter().cloned()).collect();
    let msg_ids: Vec<Vec<String>> = (0..chat_ids.len())
        .map(|ci| (0..if ci < HELD { HELD_MESSAGES } else { TAIL_MESSAGES }).map(|_| rng.hex()).collect())
        .collect();

    // Each phase builds its own inputs, hands them over and drops what is left, so its
    // delta is only what the state keeps.
    let mut rows: Vec<(&str, isize, isize)> = Vec::new();
    let mut state = ChatState::new();

    let (b0, n0) = live();
    {
        let slims: Vec<SlimProfile> = npubs
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut p = SlimProfile { id: id.clone(), mine: i == 0, ..Default::default() };
                if i < FETCHED {
                    p.name = rng.text(5, 14);
                    p.display_name = rng.text(5, 18);
                    p.about = rng.text(40, 220);
                    p.avatar = format!("https://image.nostr.build/{}.jpg", rng.hex());
                    if rng.pct(60) {
                        p.banner = format!("https://image.nostr.build/{}.png", rng.hex());
                    }
                    if rng.pct(30) {
                        p.website = format!("https://{}.com", rng.text(5, 12).replace(' ', ""));
                    }
                    if rng.pct(40) {
                        p.nip05 = format!("{}@nostrplebs.com", rng.text(4, 10).replace(' ', ""));
                    }
                    if rng.pct(40) {
                        p.lud16 = format!("{}@getalby.com", rng.text(4, 10).replace(' ', ""));
                    }
                    if i < 1000 {
                        p.avatar_cached = format!("{app}/cache/avatars/{}.webp", &rng.hex()[..32]);
                    }
                    if i < 500 {
                        p.banner_cached = format!("{app}/cache/banners/{}.webp", &rng.hex()[..32]);
                    }
                    p.last_updated = 1_790_000_000 + rng.below(1_000_000);
                }
                p
            })
            .collect();
        state.merge_db_profiles(slims, &me);
    }
    let (b1, n1) = live();
    rows.push(("profiles + interner", b1 - b0, n1 - n0));

    for i in 0..DMS {
        let _ = state.create_dm_chat(&npubs[1 + i]);
    }
    for (i, channel) in channels.iter().enumerate() {
        let c = i % COMMUNITIES;
        let name = rng.text(6, 20);
        let desc = rng.text(20, 120);
        let channel_name = rng.text(4, 12);
        state.upsert_community_chat(
            channel, &name, &desc, &community_ids[c], c == 0, true, Some(&npubs[1 + c]),
            Some(1_790_000_000_000), false, &channel_name, &primaries[c],
        );
    }
    let (b2, n2) = live();
    rows.push(("chats (100 DMs, 150 channels)", b2 - b1, n2 - n1));

    let mut at = 1_790_000_000_000u64;
    for (ci, chat_id) in chat_ids.iter().enumerate() {
        let is_dm = ci < DMS;
        let mut batch: Vec<Message> = Vec::with_capacity(msg_ids[ci].len());
        for (k, id) in msg_ids[ci].iter().enumerate() {
            at += 1 + rng.below(60_000);
            let author = if is_dm { if rng.pct(50) { me.clone() } else { npubs[1 + ci].clone() } } else { npubs[rng.below(PROFILES as u64) as usize].clone() };
            let mut m = Message { id: id.clone(), content: rng.text(8, 160), at, mine: author == me, npub: Some(author), ..Default::default() };
            if k > 0 && rng.pct(20) {
                let parent = rng.below(k as u64) as usize;
                m.replied_to = msg_ids[ci][parent].clone();
                m.replied_to_content = Some(rng.text(8, 160));
                m.replied_to_npub = Some(npubs[rng.below(PROFILES as u64) as usize].clone());
            }
            if rng.pct(10) {
                let hash = rng.hex();
                let ext = ["jpg", "png", "mp4", "webp"][rng.below(4) as usize];
                m.attachments.push(Attachment {
                    id: hash.clone(),
                    key: rng.hex(),
                    nonce: rng.hex()[..32].to_string(),
                    extension: ext.to_string(),
                    url: format!("https://blossom.primal.net/{hash}.{ext}"),
                    path: format!("/Users/jskitty/Downloads/vector/{hash}.{ext}"),
                    size: 50_000 + rng.below(5_000_000),
                    img_meta: rng.pct(70).then(|| ImageMetadata { thumbhash: rng.hex()[..28].to_string(), width: 1080, height: 1350 }),
                    downloaded: true,
                    original_hash: Some(hash.clone()),
                    ..Default::default()
                });
            }
            if rng.pct(3) {
                let title = rng.text(20, 70);
                let desc = rng.text(60, 200);
                m.preview_metadata = Some(SiteMetadata {
                    domain: "github.com".into(),
                    og_title: Some(title.clone()),
                    og_description: Some(desc.clone()),
                    og_image: Some(format!("https://opengraph.githubassets.com/{}", rng.hex())),
                    og_url: Some(format!("https://github.com/VectorPrivacy/Vector/pull/{}", rng.below(999))),
                    og_type: Some("object".into()),
                    title: Some(title),
                    description: Some(desc),
                    favicon: Some("https://github.githubassets.com/favicons/favicon.svg".into()),
                });
            }
            if rng.pct(2) {
                let first = rng.text(8, 120);
                m.edited = true;
                m.edit_history = Some(vec![
                    EditEntry { content: first, edited_at: at },
                    EditEntry { content: m.content.clone(), edited_at: at + 5_000 },
                ]);
            }
            batch.push(m);
        }
        state.add_messages_to_chat_batch(chat_id, batch);
    }
    let (b3, n3) = live();
    rows.push(("messages", b3 - b2, n3 - n2));

    let total_msgs: usize = msg_ids.iter().map(Vec::len).sum();
    let reactions = total_msgs / 2;
    for _ in 0..reactions {
        let ci = rng.below(chat_ids.len() as u64) as usize;
        let mid = &msg_ids[ci][rng.below(msg_ids[ci].len() as u64) as usize];
        let emoji = ["❤️", "🔥", "😂", "👍", "🫡", "💯"][rng.below(6) as usize];
        state.add_reaction_to_message(
            mid,
            Reaction {
                id: rng.hex(),
                reference_id: mid.clone(),
                author_id: npubs[rng.below(PROFILES as u64) as usize].clone(),
                emoji: emoji.to_string(),
                emoji_url: None,
            },
        );
    }
    let (b4, n4) = live();
    rows.push(("reactions", b4 - b3, n4 - n3));

    let messages: usize = state.chats.iter().map(|c| c.messages.len()).sum();
    println!("messages held: {messages}, reactions added: {reactions}");
    println!("{:<34} {:>12} {:>10}", "part", "live bytes", "blocks");
    let (mut tb, mut tn) = (0, 0);
    for (name, b, n) in &rows {
        println!("{name:<34} {b:>12} {n:>10}");
        tb += b;
        tn += n;
    }
    println!("{:<34} {tb:>12} {tn:>10}", "total");
    std::hint::black_box(&state);
}
