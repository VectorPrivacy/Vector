//! Reaching the session from a thread with no tokio runtime.
//!
//! Android's WebView `shouldInterceptRequest` threads have no runtime at all —
//! `Handle::current()` panics there, which is why those paths use `try_lock`
//! with a retry loop instead of awaiting. Resolving the session now sits under
//! every one of those calls, so it has to survive the same conditions.
//!
//! It does, because a tokio task-local answers `try_with` with `Err` when there
//! is no task rather than demanding a runtime, and the fallback is a plain
//! `RwLock`. Nothing here would fail loudly in development — the Android
//! release build is where it would have surfaced, as a crash in the media
//! server — so it is pinned down in a test instead.

#[test]
fn the_session_resolves_from_a_thread_with_no_tokio_runtime() {
    let probe = std::thread::spawn(|| {
        // Each of these is on an Android JNI path.
        let session = vector_core::db::current_session();
        let live = vector_core::db::session_is_live();
        let chats = vector_core::state::STATE.try_lock().is_ok();
        let own = session.chat_state().try_lock().is_ok();
        (live, chats, own)
    });

    let (live, chats, own) = probe.join().expect("resolving the session must not panic off-runtime");
    assert!(live, "an unbound caller is, by definition, the account on screen");
    assert!(chats, "STATE.try_lock() works with no runtime present");
    assert!(own, "and so does locking a held session's state directly");
}

#[test]
fn a_scoped_resource_is_reachable_off_runtime_too() {
    // The per-account caches resolve through the same path, and Android's
    // localhost media server reads them from its own threads.
    struct ProbeKey;
    let probe = std::thread::spawn(|| {
        let cell = vector_core::db::current_session().scoped::<ProbeKey, std::sync::Mutex<u32>>();
        *cell.lock().unwrap() += 1;
        let seen = *cell.lock().unwrap();
        seen
    });
    assert_eq!(probe.join().expect("no panic"), 1);
}
