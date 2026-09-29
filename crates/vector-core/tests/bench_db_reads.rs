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
                        // Every fifth message quotes one a little earlier, as real threads do.
                        replied_to: if n % 5 == 4 { format!("{:016x}{:048x}", c, n - 3) } else { String::new() },
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

/// One page as the app serves it: compose from the DB, merge into STATE and read back compact,
/// re-attach reply quotes, serialise for IPC.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark, not an assertion"]
async fn bench_page_pipeline() {
    setup().await;
    let chats: Vec<(String, i64)> = (0..CHATS)
        .map(|c| (chat_npub(c), vector_core::db::id_cache::get_chat_id_by_identifier(&chat_npub(c)).expect("id")))
        .collect();
    let mut t = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut bytes = 0;
    for round in 0..ROUNDS + 2 {
        let mut acc = [0f64; 4];
        for (npub, id) in &chats {
            let s = Instant::now();
            let messages = vector_core::db::events::get_message_views(*id, 50, 0).await.unwrap();
            acc[0] += s.elapsed().as_secs_f64();
            // Taken before the merge consumes the page, but billed to the quote stage.
            let s = Instant::now();
            let quotes: std::collections::HashMap<String, (Option<String>, Option<String>)> = messages
                .iter()
                .filter(|m| m.replied_to_content.is_some())
                .map(|m| (m.id.clone(), (m.replied_to_content.clone(), m.replied_to_npub.clone())))
                .collect();
            acc[2] += s.elapsed().as_secs_f64();
            let s = Instant::now();
            let ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
            let mut served = {
                let mut state = vector_core::STATE.lock().await;
                state.add_messages_to_chat_batch(npub, messages);
                let chat = state.get_chat(npub).unwrap();
                ids.iter().filter_map(|id| chat.get_compact_message(id)).map(|c| c.to_message(&state.interner)).collect::<Vec<_>>()
            };
            acc[1] += s.elapsed().as_secs_f64();
            let s = Instant::now();
            for m in &mut served {
                if let Some((content, npub)) = quotes.get(&m.id) {
                    (m.replied_to_content, m.replied_to_npub) = (content.clone(), npub.clone());
                }
            }
            let unresolved: Vec<&mut vector_core::Message> =
                served.iter_mut().filter(|m| !m.replied_to.is_empty() && m.replied_to_content.is_none()).collect();
            vector_core::db::events::populate_reply_contexts(unresolved).await.unwrap();
            acc[2] += s.elapsed().as_secs_f64();
            let s = Instant::now();
            let json = serde_json::to_vec(&served).unwrap();
            acc[3] += s.elapsed().as_secs_f64();
            bytes = json.len();
            black_box(json);
        }
        if round >= 2 {
            for (v, a) in t.iter_mut().zip(acc) {
                v.push(a * 1e6 / CHATS as f64);
            }
        }
    }
    for (name, v) in ["compose from DB", "merge into STATE + read back", "carry reply quotes across", "serialise JSON"].iter().zip(t) {
        println!("{name:<32} {:>8.1} µs", median(v));
    }
    println!("page of 50: {bytes} bytes of JSON");
}

/// Where a page's compose time goes: each query it makes, timed on its own.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark, not an assertion"]
async fn bench_compose_parts() {
    setup().await;
    use vector_core::db::events;
    let ids: Vec<i64> = (0..CHATS).map(|c| vector_core::db::id_cache::get_chat_id_by_identifier(&chat_npub(c)).unwrap()).collect();
    let kinds = [vector_core::stored_event::event_kind::CHAT_MESSAGE, vector_core::stored_event::event_kind::PRIVATE_DIRECT_MESSAGE, vector_core::stored_event::event_kind::FILE_ATTACHMENT];
    let mut t = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for round in 0..ROUNDS + 2 {
        let mut acc = [0f64; 5];
        for id in &ids {
            let s = Instant::now();
            let evs = events::get_events(*id, Some(&kinds), 50, 0).await.unwrap();
            acc[0] += s.elapsed().as_secs_f64();
            let msg_ids: Vec<String> = evs.iter().map(|e| e.id.clone()).collect();
            let s = Instant::now();
            black_box(events::get_related_events(&msg_ids).await.unwrap());
            acc[1] += s.elapsed().as_secs_f64();
            let s = Instant::now();
            black_box(vector_core::db::attachments::get_attachments_for_events(&msg_ids).unwrap());
            acc[2] += s.elapsed().as_secs_f64();
            let replies: Vec<String> = evs.iter().filter_map(|e| e.get_reply_reference().map(str::to_string)).collect();
            let s = Instant::now();
            black_box(events::get_reply_contexts(&replies).await.unwrap());
            acc[3] += s.elapsed().as_secs_f64();
            let s = Instant::now();
            black_box(events::get_message_views(*id, 50, 0).await.unwrap());
            acc[4] += s.elapsed().as_secs_f64();
        }
        if round >= 2 {
            for (v, a) in t.iter_mut().zip(acc) { v.push(a * 1e6 / CHATS as f64); }
        }
    }
    for (name, v) in ["get_events (query + decrypt)", "related events", "attachments", "reply contexts", "whole compose"].iter().zip(t) {
        println!("{name:<32} {:>8.1} µs", median(v));
    }
}
