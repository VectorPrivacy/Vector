//! Safe STATE lock helper patterns to simplify state access.
//!
//! These utilities provide cleaner patterns for accessing the global STATE
//! mutex, reducing boilerplate and ensuring consistent lock handling.

use crate::state::{ChatState, STATE};

/// Execute a closure with read access to the ChatState.
///
/// This acquires the STATE lock, runs the closure, and releases the lock.
/// The lock is held for the duration of the closure execution.
///
/// # Example
/// ```rust
/// let profile = with_state(|state| {
///     state.get_profile(&npub).cloned()
/// }).await;
/// ```
pub async fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&ChatState) -> R,
{
    let state = STATE.lock().await;
    f(&state)
}

/// Execute a closure with mutable access to the ChatState.
///
/// This acquires the STATE lock mutably, runs the closure, and releases the lock.
/// The lock is held for the duration of the closure execution.
///
/// # Example
/// ```rust
/// with_state_mut(|state| {
///     state.add_message_to_chat(&chat_id, message);
/// }).await;
/// ```
pub async fn with_state_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut ChatState) -> R,
{
    let mut state = STATE.lock().await;
    f(&mut state)
}

/// Try to execute a closure with read access to the ChatState.
///
/// If the lock is already held, returns None immediately without blocking.
///
/// # Example
/// ```rust
/// if let Some(result) = try_with_state(|state| {
///     state.count_unread_messages()
/// }) {
///     // Got the lock and result
/// }
/// ```
pub fn try_with_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&ChatState) -> R,
{
    STATE.try_lock().ok().map(|state| f(&state))
}

/// Try to execute a closure with mutable access to the ChatState.
///
/// If the lock is already held, returns None immediately without blocking.
pub fn try_with_state_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut ChatState) -> R,
{
    STATE.try_lock().ok().map(|mut state| f(&mut state))
}
