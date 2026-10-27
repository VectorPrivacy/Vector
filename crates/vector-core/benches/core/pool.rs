//! Connection-pool acquire/release overhead — the machinery an account switch
//! made safe, measured so a redesign of it can't quietly cost the hot path.
//!
//! Deliberately measures ONLY the pool: every iteration hits a pre-warmed pool,
//! so no connection is opened and no statement runs. That isolates the guard
//! bookkeeping, which is the thing that changed. A real caller runs a SQLite
//! query straight after, costing microseconds to milliseconds, so anything here
//! under ~100ns is invisible in production either way.

use std::hint::black_box;
use std::time::Instant;

const WARMUP: usize = 2_000;
const ITERS: usize = 200_000;

/// One acquire + release cycle against a warm pool.
fn cycle() {
    let guard = vector_core::db::get_db_connection_guard_static().expect("guard");
    black_box(&*guard);
}

pub fn uncontended() {
    crate::fresh_account();
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

pub fn contended() {
    // Where a regression would actually hide: the old path read an atomic, the
    // new one takes an RwLock read and touches an Arc refcount. Under real
    // parallel load those behave differently, and this is the number that says
    // by how much.
    crate::fresh_account();
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

/// The cost of reaching a per-account resource through the session.
///
/// Every cache that used to be a `LazyLock` static now costs one session
/// resolve plus one type-keyed lookup on top of its own lock. That is paid on
/// paths that were already taking a mutex next to I/O, but the claim deserves
/// a number rather than a shrug.
pub fn scoped_lookup() {
    use std::sync::Mutex;
    struct BenchKey;

    let baseline: &'static Mutex<u64> = Box::leak(Box::new(Mutex::new(0)));
    let bump_static = || {
        *baseline.lock().unwrap() += 1;
    };
    let bump_scoped = || {
        let cell = vector_core::db::current_session().scoped::<BenchKey, Mutex<u64>>();
        *cell.lock().unwrap() += 1;
    };

    for _ in 0..WARMUP {
        bump_static();
        bump_scoped();
    }

    let start = Instant::now();
    for _ in 0..ITERS {
        bump_static();
    }
    let flat = start.elapsed();

    let start = Instant::now();
    for _ in 0..ITERS {
        bump_scoped();
    }
    let via_session = start.elapsed();

    println!(
        "STATIC LOOKUP    {:>10.1} ns/op\nSESSION LOOKUP   {:>10.1} ns/op   (+{:.1} ns)",
        flat.as_nanos() as f64 / ITERS as f64,
        via_session.as_nanos() as f64 / ITERS as f64,
        (via_session.as_nanos() as f64 - flat.as_nanos() as f64) / ITERS as f64,
    );
}
