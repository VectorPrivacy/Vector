//! Turning local encryption on, off, or onto a new PIN after setup.
//!
//! The re-encryption runs synchronously on the worker's one thread, so no
//! inbound event can land between its read and its write.

use vector_core::db::at_rest::{self, MigrationProgress};
use vector_core::state;
use vector_core::{ENCRYPTION_KEY, MY_SECRET_KEY};
use zeroize::Zeroize;

use serde_json::Value;

use crate::commands::Args;
use crate::emitter;

fn progress(p: MigrationProgress) {
    emitter::emit("encryption_migration_progress", &p);
}

fn finish() {
    emitter::emit("encryption_migration_complete", &());
}

pub async fn enable(credential: String, security_type: String) -> Result<(), String> {
    if state::is_encryption_enabled_fast() {
        return Err("Encryption is already enabled".into());
    }
    let mut key = vector_core::crypto::hash_pass(&credential).await;
    ENCRYPTION_KEY.set(key, &[&MY_SECRET_KEY]);
    let result = at_rest::enable(&key, &security_type, None, None, &progress);
    key.zeroize();
    match result {
        Ok(()) => {
            finish();
            Ok(())
        }
        Err(e) => {
            ENCRYPTION_KEY.clear(&[&MY_SECRET_KEY]);
            Err(e)
        }
    }
}

pub fn disable() -> Result<(), String> {
    let mut key = ENCRYPTION_KEY.get().ok_or("No encryption key available")?;
    let result = at_rest::disable(&key, &progress);
    key.zeroize();
    result?;
    ENCRYPTION_KEY.clear(&[&MY_SECRET_KEY]);
    finish();
    Ok(())
}

pub async fn rekey(old_credential: String, new_credential: String, security_type: String) -> Result<(), String> {
    let mut old_key = vector_core::crypto::hash_pass(&old_credential).await;
    if !at_rest::key_matches_account(&old_key) {
        old_key.zeroize();
        return Err("Incorrect current credential.".into());
    }
    let mut new_key = vector_core::crypto::hash_pass(&new_credential).await;
    let result = at_rest::rekey(&old_key, &new_key, &security_type, None, &progress);
    if result.is_ok() {
        ENCRYPTION_KEY.set(new_key, &[&MY_SECRET_KEY]);
    }
    old_key.zeroize();
    new_key.zeroize();
    result?;
    finish();
    Ok(())
}

fn security_type(a: &Args) -> String {
    a.opt_str("securityType").unwrap_or_else(|| "pin".into())
}

pub async fn enable_cmd(a: &Args) -> Result<Value, String> {
    enable(a.str("credential")?, security_type(a)).await.map(|_| Value::Null)
}

pub async fn rekey_cmd(a: &Args) -> Result<Value, String> {
    rekey(a.str("oldCredential")?, a.str("newCredential")?, security_type(a)).await.map(|_| Value::Null)
}
