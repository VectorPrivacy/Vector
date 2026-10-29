//! Vector Core's benchmarks, built optimised and kept out of the test suite. From `crates/`:
//!
//!     cargo bench -p vector-core --features bench                # every benchmark
//!     cargo bench -p vector-core --features bench -- vault db::  # names containing either
//!     cargo bench -p vector-core --features bench -- --list
//!
//! They run one at a time in one process, each printing its own table. The ones that touch a
//! database log in a fresh account first.

mod community;
mod compact;
mod db_reads;
mod parse;
mod pool;

use nostr_sdk::prelude::{Keys, ToBech32};
use vector_core::crypto::guarded_key::bench as vault;

const ALL: &[(&str, fn())] = &[
    ("compact::messages", compact::messages),
    ("compact::profile_lookup", compact::profile_lookup),
    ("community::load_encrypted", community::load_encrypted),
    ("community::policy_window", community::policy_window),
    ("concord::live_dispatch", concord_live_dispatch),
    ("concord::history_catchup", concord_history_catchup),
    ("concord::control_fold", concord_control_fold),
    ("concord::guestbook_ingest", concord_guestbook_ingest),
    ("concord::ledger_chain", vector_core::community::v2::bench::ledger_chain),
    ("db::pool_uncontended", pool::uncontended),
    ("db::pool_contended", pool::contended),
    ("db::scoped_lookup", pool::scoped_lookup),
    ("db::read_paths", db_reads::read_paths),
    ("db::page_pipeline", db_reads::page_pipeline),
    ("db::compose_parts", db_reads::compose_parts),
    ("parse::gift_wrap_layers", parse::gift_wrap_layers),
    ("crypto::chacha20_raw", parse::chacha20_raw),
    ("crypto::nip44_decrypt", parse::nip44_decrypt),
    ("crypto::chacha20_backends", parse::chacha20_backends),
    ("profile::processor", vector_core::profile::sync::bench::processor),
    ("vault::designs", vault::designs),
    ("vault::set_sources", vault::set_sources),
    ("vault::fill", vault::fill),
    ("vault::hot_paths", vault::hot_paths),
    ("vault::set_with_a_live_partner", vault::set_with_a_live_partner),
    ("vault::rounds_dial", vault::rounds_dial),
];

fn main() {
    // cargo passes `--bench`; every flag but `--list` is ignored.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        for (name, _) in ALL {
            println!("{name}");
        }
        return;
    }
    let filters: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    let chosen: Vec<_> = ALL.iter().filter(|(name, _)| filters.is_empty() || filters.iter().any(|f| name.contains(f))).collect();
    if chosen.is_empty() {
        eprintln!("no benchmark matches {filters:?}; `-- --list` names them");
        std::process::exit(1);
    }
    for (name, run) in chosen {
        println!("\n== {name}");
        run();
    }
}

fn concord_live_dispatch() {
    fresh_account();
    vector_core::community::v2::bench::live_dispatch();
}

fn concord_history_catchup() {
    fresh_account();
    vector_core::community::v2::bench::history_catchup();
}

fn concord_control_fold() {
    fresh_account();
    vector_core::community::v2::bench::control_fold();
}

fn concord_guestbook_ingest() {
    fresh_account();
    vector_core::community::v2::bench::guestbook_ingest();
}

/// Log in a new account under a temp root that lives until the process exits.
pub fn fresh_account() {
    let root = Box::leak(Box::new(tempfile::tempdir().expect("tempdir"))).path().to_path_buf();
    open_account(&root, &Keys::generate().public_key().to_bech32().unwrap());
}

pub fn open_account(root: &std::path::Path, npub: &str) {
    vector_core::db::close_database();
    vector_core::db::clear_id_caches();
    std::fs::create_dir_all(root.join(npub)).expect("account dir");
    vector_core::db::set_app_data_dir(root.to_path_buf());
    vector_core::db::set_current_account(npub.to_string()).expect("set account");
    vector_core::db::init_database(npub).expect("init db");
}

pub fn multi_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}
