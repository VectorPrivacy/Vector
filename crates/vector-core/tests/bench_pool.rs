//! Connection-pool acquire/release overhead — the machinery an account switch
//! made safe, measured so a redesign of it can't quietly cost the hot path.
//!
//! Deliberately measures ONLY the pool: every iteration hits a pre-warmed pool,
//! so no connection is opened and no statement runs. That isolates the guard
//! bookkeeping, which is the thing that changed. A real caller runs a SQLite
//! query straight after, costing microseconds to milliseconds, so anything here
//! under ~100ns is invisible in production either way.
//!
//! `#[ignore]`d: it is a measurement, not an assertion, and timings under a
//! loaded CI runner are meaningless. Run deliberately:
//!   cargo test -p vector-core --test bench_pool -- --ignored --nocapture

use std::hint::black_box;
use std::time::Instant;

const WARMUP: usize = 2_000;
const ITERS: usize = 200_000;

fn setup() {
    // A fresh account under a temp root; leaked on purpose so the directory
    // outlives the run (the process exits immediately after).
    let dir = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));
    // 63 chars: "npub1" + 58 from the bech32 alphabet.
    let npub: &'static str = Box::leak(format!("npub1{}", "q".repeat(58)).into_boxed_str());
    std::fs::create_dir_all(dir.path().join(npub)).expect("account dir");
    vector_core::db::set_app_data_dir(dir.path().to_path_buf());
    vector_core::db::set_current_account(npub.to_string()).expect("set account");
    vector_core::db::init_database(npub).expect("init db");
}

/// One acquire + release cycle against a warm pool.
fn cycle() {
    let guard = vector_core::db::get_db_connection_guard_static().expect("guard");
    black_box(&*guard);
}

#[test]
#[ignore = "benchmark, not an assertion"]
fn bench_pool_acquire_release_uncontended() {
    setup();
    for _ in 0..WARMUP {
        cycle();
    }

    let start = Instant::now();
    for _ in 0..ITERS {
        cycle();
    }
    let elapsed = start.elapsed();
    println!(
        "UNCONTENDED  {:>10.1} ns/op   ({} iterations in {:?})",
        elapsed.as_nanos() as f64 / ITERS as f64,
        ITERS,
        elapsed
    );
}

#[test]
#[ignore = "benchmark, not an assertion"]
fn bench_pool_acquire_release_contended() {
    // Where a regression would actually hide: the old path read an atomic, the
    // new one takes an RwLock read and touches an Arc refcount. Under real
    // parallel load those behave differently, and this is the number that says
    // by how much.
    setup();
    for _ in 0..WARMUP {
        cycle();
    }

    for threads in [2usize, 4, 8] {
        let per_thread = ITERS / threads;
        let start = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(move || {
                    for _ in 0..per_thread {
                        cycle();
                    }
                });
            }
        });
        let elapsed = start.elapsed();
        let ops = per_thread * threads;
        println!(
            "CONTENDED x{:<2}  {:>10.1} ns/op   ({} ops in {:?})",
            threads,
            elapsed.as_nanos() as f64 / ops as f64,
            ops,
            elapsed
        );
    }
}
