//! Event database operations — delegates entirely to vector-core.

pub use vector_core::db::events::{
    save_event, event_exists, save_reaction_event,
    save_pivx_payment_event, save_edit_event, delete_event,
    save_system_event_by_id,
    populate_reply_context, get_message_views, get_all_chats_last_messages,
};
