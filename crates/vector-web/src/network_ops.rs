//! Relays, Blossom servers and notification preferences.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::commands::Args;

pub fn dispatch<'a>(cmd: &'a str, a: &'a Args) -> Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + 'a>> {
    Box::pin(async move {
        let _ = a;
        Some(match cmd {
            _ => return None,
        })
    })
}
