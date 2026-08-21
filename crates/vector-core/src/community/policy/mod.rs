//! The moderation policy engine (Phase 1).
//!
//! A read-only conviction engine: communities declare declarative policies, the
//! engine evaluates evidence and reports convictions — "member X was convicted
//! of rule Y, here is the proof" — and the CONSUMER (GUI, mod bot, send
//! pipeline) decides every fate. The engine holds no keys, performs no I/O, and
//! never sentences; `evaluate` is a pure function of its inputs, so any two
//! clients holding the same inputs reach byte-identical conclusions.
//!
//! Design: `MOD_POLICY_ENGINE_DESIGN.md` (repo root, local). The scoring core,
//! pipeline order, id preimages and canonical encoding there are wire-frozen;
//! the reference values in [`combine`]'s tests are the first conformance
//! vectors. The shipped raid assessor ([`super::raid`]) is the ancestor of the
//! `cohort`/`join_burst` rules and keeps serving the console until the facade
//! swaps over.

pub mod combine;
pub mod document;
pub mod engine;
pub mod harness;
pub mod matchers;
pub mod normalize;
pub mod presets;
pub mod types;

pub use types::*;
