//! MLS (Message Layer Security) — re-exports from vector-core.

pub use vector_core::mls::{
    MlsGroup, MlsGroupProfile, MlsGroupFull,
    MlsService,
    send_mls_message, emit_group_metadata_event,
    metadata_to_frontend,
};
