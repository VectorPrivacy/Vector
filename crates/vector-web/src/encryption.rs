//! Turning local encryption on, off, or onto a new PIN after setup.
//!
//! The re-encryption runs synchronously on the worker's one thread, so no
//! inbound event can land between its read and its write.

use vector_core::db::at_rest::{self, MigrationProgress};
use vector_core::state;
use vector_core::{SignerKind, ENCRYPTION_KEY, MY_SECRET_KEY};
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
    let kdf = vector_core::crypto::Kdf::fresh();
    let mut key = vector_core::crypto::derive_key(&credential, &kdf).await;
    // Checked again after the derivation: a second Enable can start while the first awaits it.
    if state::is_encryption_enabled_fast() {
        key.zeroize();
        return Err("Encryption is already enabled".into());
    }
    // An account with no sealed key has nothing to reject a wrong PIN; the canary does.
    let canary = match vector_core::signer_kind() {
        SignerKind::Nip55 | SignerKind::Nip07 => {
            match vector_core::crypto::encrypt_with_key(at_rest::NIP55_PIN_CANARY, &key) {
                Ok(c) => Some(c),
                Err(e) => {
                    key.zeroize();
                    return Err(e);
                }
            }
        }
        _ => None,
    };
    ENCRYPTION_KEY.set(key, &[&MY_SECRET_KEY]);
    let result = at_rest::enable(&key, &kdf, &security_type, None, canary.as_deref(), &progress);
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
    finish();
    Ok(())
}

pub async fn rekey(old_credential: String, new_credential: String, security_type: String) -> Result<(), String> {
    if !state::is_encryption_enabled_fast() {
        return Err("Local Encryption is not enabled".into());
    }
    let mut old_key = vector_core::crypto::derive_key(&old_credential, &vector_core::crypto::Kdf::of_account()?).await;
    if !at_rest::key_matches_account(&old_key) {
        old_key.zeroize();
        return Err("Incorrect current credential.".into());
    }
    let new_kdf = vector_core::crypto::Kdf::fresh();
    let mut new_key = vector_core::crypto::derive_key(&new_credential, &new_kdf).await;
    let result = at_rest::rekey(&old_key, &new_key, &new_kdf, &security_type, None, &progress);
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
