//! Message search: building the corpus, then the scans a keystroke runs.

use std::time::Instant;
use vector_core::search::{Corpus, Filter, Has, Query, Record};

const WORDS: &[&str] = &[
    "the", "build", "relay", "lunch", "tomorrow", "café", "release", "voice", "channel", "bug",
    "fixed", "photo", "meeting", "deploy", "東京", "会議", "night", "cats", "music", "invoice",
];

/// 200,000 messages of 12 to 30 words across 300 chats.
pub fn scan() {
    const N: usize = 200_000;
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let records: Vec<Record> = (0..N)
        .map(|i| {
            let n = 12 + (next() % 19) as usize;
            let text = (0..n).map(|_| WORDS[(next() % WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ");
            let files = if i % 25 == 0 { vec![("jpg".to_string(), format!("IMG_{i}.jpg"))] } else { Vec::new() };
            Record {
                chat: format!("chat{}", next() % 300),
                id: format!("{i:064x}"),
                author: Some(format!("npub1{:0>58}", next() % 500)),
                mine: i % 7 == 0,
                at: 1_800_000_000_000 - i as u64 * 1000,
                text,
                files,
            }
        })
        .collect();
    let t = Instant::now();
    let mut corpus = Corpus::new();
    for r in &records {
        corpus.push(r, &r.text);
    }
    println!("build      {:>9.1} ms  ({} messages)", t.elapsed().as_secs_f64() * 1e3, corpus.len());

    let cases: &[(&str, &str, Filter)] = &[
        ("rare word", "invoice deploy", Filter::default()),
        ("common word", "the", Filter::default()),
        ("no match", "zzzzzz", Filter::default()),
        ("cjk", "会議", Filter::default()),
        ("phrase", "\"lunch tomorrow\"", Filter::default()),
        ("has:image", "", Filter { has: Has::IMAGE, ..Default::default() }),
        ("in: + word", "relay", Filter { chats: vec!["chat7".into()], ..Default::default() }),
        ("oldest", "cats", Filter { oldest: true, ..Default::default() }),
    ];
    for (name, q, f) in cases {
        let query = Query::parse(q);
        let runs = 20;
        let t = Instant::now();
        let mut total = 0;
        for _ in 0..runs {
            total = corpus.search(&query, f, 200).total;
        }
        println!("{name:<12} {:>7.2} ms  ({total} matches)", t.elapsed().as_secs_f64() * 1e3 / runs as f64);
    }
}
