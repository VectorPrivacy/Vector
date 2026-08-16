//! The real account swap, end to end.
//!
//! The unit tests build sessions by hand; this drives the path the app actually
//! takes — `init_database` → work → `swap_session` → `init_database` for the
//! next account — through the public API, in its own process, with no globals
//! shared with any other test.
//!
//! This is the property the whole session design exists for, and until now
//! `swap_session` had no test at all.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use vector_core::chat::{Chat, ChatType};
use vector_core::{db, state, VectorCore};

/// One data dir for the whole binary, never dropped.
///
/// `APP_DATA_DIR` is set-once, so only the first `set_app_data_dir` in a process
/// takes effect — a per-test `TempDir` leaves every later test pointed at a
/// directory the first one deleted on its way out. Accounts isolate by
/// subdirectory, so sharing the root is safe; the lifetime is what they cannot
/// share.
fn data_dir() -> &'static std::path::Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("test data dir")).path()
}

/// These drive the live session, which is one global. They run one at a time.
fn serialized() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn boot() -> MutexGuard<'static, ()> {
    let guard = serialized();
    db::set_app_data_dir(data_dir().to_path_buf());
    guard
}

/// A deterministic, well-formed npub. The bech32 alphabet only, and distinct
/// seeds must not collapse to the same string.
fn npub(seed: usize) -> String {
    const B: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let mut out = String::from("npub1");
    let mut v = seed;
    for _ in 0..58 {
        out.push(B[v % 32] as char);
        v = v / 32 + 11;
    }
    out
}

fn open(account: &str) {
    db::set_current_account(account.to_string()).expect("set account");
    db::init_database(account).expect("init database");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_swap_leaves_the_next_account_with_nothing_of_the_previous_one() {
    let _serialized = boot();
    let (a, b) = (npub(4_100), npub(9_700));
    assert_ne!(a, b, "the seeds must produce distinct accounts");

    // Account A logs in and accumulates the things a session holds.
    open(&a);
    state::STATE.lock().await.chats.push(Chat::new("a-chat".into(), ChatType::DirectMessage, Vec::new()));
    state::set_active_chat(Some("a-chat".into()));
    state::set_my_public_key(nostr_sdk::prelude::Keys::generate().public_key());
    let a_identity = state::my_public_key().expect("A has an identity");
    assert!(db::session_is_live(), "A is the account on screen");

    // ...and the user switches accounts.
    VectorCore.swap_session().await;

    assert!(state::STATE.lock().await.chats.is_empty(), "the chat list is empty between accounts");
    assert_eq!(state::my_public_key(), None, "and there is no identity");
    assert_eq!(state::get_active_chat(), None, "and no chat is open");

    // Account B logs in.
    open(&b);
    assert!(state::STATE.lock().await.chats.is_empty(), "B starts with an empty chat list");
    assert_eq!(state::my_public_key(), None, "B has no identity until it installs one");

    state::STATE.lock().await.chats.push(Chat::new("b-chat".into(), ChatType::DirectMessage, Vec::new()));
    let visible: Vec<String> = state::STATE.lock().await.chats.iter().map(|c| c.id.clone()).collect();
    assert_eq!(visible, vec!["b-chat".to_string()], "B sees only its own chat");
    assert_ne!(state::my_public_key(), Some(a_identity), "and never A's identity");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_that_outlives_the_swap_writes_to_the_account_it_started_under() {
    // The bug in the form it actually took. A task holding account A's work
    // finishes after the user has switched to B; before the session design it
    // resolved whoever was live and inserted A's chat into B's list, where it
    // sat invisible until the user opened it.
    let _serialized = boot();
    let (a, b) = (npub(21_300), npub(56_800));

    open(&a);
    let a_session = db::current_session();

    // Slow work started under A: it will land well after the swap.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let landed = db::spawn_bound(async move {
        let _ = release_rx.await;
        state::STATE.lock().await.chats.push(Chat::new("late-from-a".into(), ChatType::DirectMessage, Vec::new()));
        // What it can still paint, now that it is not the account on screen.
        db::session_is_live()
    });

    VectorCore.swap_session().await;
    open(&b);
    state::STATE.lock().await.chats.push(Chat::new("b-chat".into(), ChatType::DirectMessage, Vec::new()));

    // Now let A's task finish.
    let _ = release_tx.send(());
    let painted = tokio::time::timeout(Duration::from_secs(5), landed)
        .await
        .expect("the task finished")
        .expect("without panicking");

    let visible: Vec<String> = state::STATE.lock().await.chats.iter().map(|c| c.id.clone()).collect();
    assert_eq!(visible, vec!["b-chat".to_string()], "B's chat list is untouched by A's late write");
    assert!(!painted, "and A's task knows not to paint into the account on screen");

    let stranded: Vec<String> =
        a_session.chat_state().lock().await.chats.iter().map(|c| c.id.clone()).collect();
    assert_eq!(
        stranded,
        vec!["late-from-a".to_string()],
        "it landed in A's own state, where nothing displays it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_account_keeps_its_own_rows_on_disk() {
    // The session routes the connection, so the check that matters is which
    // FILE the writes reached — not which pool handed out the connection.
    let _serialized = boot();
    let (a, b) = (npub(33_400), npub(77_200));

    open(&a);
    db::set_sql_setting("marker".into(), "belongs-to-a".into()).expect("write A's setting");
    assert_eq!(db::get_sql_setting("marker".to_string()).unwrap().as_deref(), Some("belongs-to-a"));

    VectorCore.swap_session().await;
    open(&b);
    assert_eq!(db::get_sql_setting("marker".to_string()).unwrap(), None, "B's database is its own, and empty");

    db::set_sql_setting("marker".into(), "belongs-to-b".into()).expect("write B's setting");

    VectorCore.swap_session().await;
    open(&a);
    assert_eq!(
        db::get_sql_setting("marker".to_string()).unwrap().as_deref(),
        Some("belongs-to-a"),
        "A's row survived B's session untouched"
    );
}
