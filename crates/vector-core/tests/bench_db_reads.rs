//! Read-path timings on a populated database: message pages at several depths, the chat
//! list's last-message query and unread counts, the queries a user waits on.
//!
//! `#[ignore]`d: a measurement, not an assertion. The database is built once under
//! `VECTOR_BENCH_DB` (default: a temp dir) so repeated runs read the same file:
//!   VECTOR_BENCH_DB=/tmp/vbench cargo test --release -p vector-core --test bench_db_reads -- --ignored --nocapture

use std::hint::black_box;
use std::time::Instant;

const CHATS: usize = 40;
const PER_CHAT: usize = 5_000;
const ROUNDS: usize = 15;

fn chat_npub(i: usize) -> String {
    format!("npub1{:q>58}", i)
}

async fn setup() {
    let root = std::env::var("VECTOR_BENCH_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Box::leak(Box::new(tempfile::tempdir().expect("tempdir"))).path().to_path_buf());
    let me: &'static str = Box::leak(format!("npub1{}", "p".repeat(58)).into_boxed_str());
    let fresh = !root.join(me).exists();
    std::fs::create_dir_all(root.join(me)).expect("account dir");
    vector_core::db::set_app_data_dir(root.clone());
    vector_core::db::set_current_account(me.to_string()).expect("set account");
    vector_core::db::init_database(me).expect("init db");
    if !fresh {
        return;
    }
    let t = Instant::now();
    let text = "hey! are we still on for tomorrow? I'll bring the snacks, and the tickets are on me this time";
    for c in 0..CHATS {
        let chat = chat_npub(c);
        vector_core::db::id_cache::get_or_create_chat_id(&chat).expect("chat");
        for page in 0..PER_CHAT / 500 {
            let msgs: Vec<vector_core::Message> = (0..500)
                .map(|i| {
                    let n = page * 500 + i;
                    vector_core::Message {
                        id: format!("{:016x}{:048x}", c, n),
                        content: format!("{text} #{n}"),
                        at: 1_700_000_000_000 + (n as u64) * 60_000,
                        mine: n % 3 == 0,
                        npub: Some(chat.clone()),
                        ..Default::default()
                    }
                })
                .collect();
            let refs: Vec<&vector_core::Message> = msgs.iter().collect();
            vector_core::db::events::save_messages_batch(&chat, &refs).await.expect("save");
        }
    }
    println!("populated {} messages in {:?}", CHATS * PER_CHAT, t.elapsed());
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark, not an assertion"]
async fn bench_db_read_paths() {
    setup().await;
    let chat_ids: Vec<i64> = (0..CHATS)
        .map(|c| vector_core::db::id_cache::get_chat_id_by_identifier(&chat_npub(c)).expect("id"))
        .collect();
    let mut rows: Vec<(&str, Vec<f64>)> = vec![
        ("page of 50, newest", Vec::new()),
        ("page of 50, 2,000 deep", Vec::new()),
        ("page of 50, 4,900 deep", Vec::new()),
        ("chat list: last message per chat", Vec::new()),
        ("unread counts", Vec::new()),
    ];
    for round in 0..ROUNDS + 2 {
        let mut times = [0f64; 5];
        let t = Instant::now();
        for id in &chat_ids {
            black_box(vector_core::db::events::get_message_views(*id, 50, 0).await.unwrap());
        }
        times[0] = t.elapsed().as_secs_f64() * 1e6 / CHATS as f64;
        let t = Instant::now();
        for id in &chat_ids {
            black_box(vector_core::db::events::get_message_views(*id, 50, 2_000).await.unwrap());
        }
        times[1] = t.elapsed().as_secs_f64() * 1e6 / CHATS as f64;
        let t = Instant::now();
        for id in &chat_ids {
            black_box(vector_core::db::events::get_message_views(*id, 50, 4_900).await.unwrap());
        }
        times[2] = t.elapsed().as_secs_f64() * 1e6 / CHATS as f64;
        let t = Instant::now();
        black_box(vector_core::db::events::get_all_chats_last_messages().await.unwrap());
        times[3] = t.elapsed().as_secs_f64() * 1e6;
        let t = Instant::now();
        black_box(vector_core::db::events::unread_counts().await.unwrap());
        times[4] = t.elapsed().as_secs_f64() * 1e6;
        if round >= 2 {
            for (row, t) in rows.iter_mut().zip(times) {
                row.1.push(t);
            }
        }
    }
    for (name, t) in rows {
        println!("{name:<36} {:>10.1} µs", median(t));
    }
}
