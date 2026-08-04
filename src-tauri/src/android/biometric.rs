//! Android biometric unlock bridge — Keystore-wrapped Local Encryption.
//!
//! The heavy lifting (Keystore key management, BiometricPrompt) lives in the
//! Kotlin `BiometricUnlock` object; this side is a thin orchestrator that
//! launches an op and parks on a keyed condvar waiter until the prompt's
//! callback fires `nativeOnBiometricResult`. Mirrors `external_signer.rs`.
//!
//! Blocking is fine: every entry point is called inside `spawn_blocking` from
//! the Tauri command layer, so parking never starves the async runtime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::jint;
use jni::JNIEnv;

use super::utils::{with_android_activity, with_android_context};

const BIOMETRIC_CLASS: &str = "io/vectorapp/BiometricUnlock";
/// One prompt interaction: read the sheet, place a finger (or type the device
/// PIN on the credential path). Generous without parking a thread forever.
const PROMPT_TIMEOUT_SECS: u64 = 120;

/// Outcome of a prompt-gated keystore op.
pub enum BiometricError {
    /// The keystore key was permanently invalidated (new biometric enrolled on
    /// the device) or is missing. The enrollment is dead; caller must clear it.
    Invalidated,
    /// The user dismissed the prompt. Not an error state — fall back to PIN.
    Cancelled,
    Other(String),
}

fn jni_err<E: std::fmt::Debug>(e: E) -> String {
    format!("{:?}", e)
}

/// Resolve the app class via the Context's PathClassLoader — `env.find_class`
/// on a native thread uses the boot classloader and can't see app classes.
fn load_class<'a>(env: &mut JNIEnv<'a>, ctx: &JObject<'a>, name: &str) -> Result<JClass<'a>, String> {
    let class_loader = env
        .call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .map_err(jni_err)?
        .l()
        .map_err(jni_err)?;
    let j_name = env.new_string(name.replace('/', ".")).map_err(jni_err)?;
    let cls = env
        .call_method(
            &class_loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&j_name)],
        )
        .map_err(jni_err)?
        .l()
        .map_err(jni_err)?;
    Ok(JClass::from(cls))
}

// ============================================================================
// Keyed waiters — one per in-flight prompt
// ============================================================================

#[derive(Default)]
struct BiometricReply {
    data: Option<String>,
    /// Unwrapped vault-key bytes — delivered via the dedicated byte-array
    /// callback so the secret never rides through a Java String / JSON copy.
    /// `Zeroizing` wipes on drop, so an un-consumed reply (timeout or reset
    /// race, no listening waiter) leaves no plaintext behind.
    key_bytes: Option<zeroize::Zeroizing<Vec<u8>>>,
    invalidated: bool,
    cancelled: bool,
    error: Option<String>,
    reset: bool,
}

type Waiter = Arc<(Mutex<Option<BiometricReply>>, Condvar)>;

static WAITERS: LazyLock<Mutex<HashMap<i32, Waiter>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_REQUEST_ID: AtomicI32 = AtomicI32::new(1);

fn deliver_reply(request_id: i32, reply: BiometricReply) {
    let waiter = { WAITERS.lock().unwrap().get(&request_id).cloned() };
    match waiter {
        Some(waiter) => {
            let (lock, cvar) = &*waiter;
            *lock.lock().unwrap() = Some(reply);
            cvar.notify_all();
        }
        // Waiter already timed out or was reset: `reply` drops here and its
        // Drop impl wipes any key bytes it carried.
        None => drop(reply),
    }
}

fn deliver_result(request_id: i32, json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    deliver_reply(request_id, BiometricReply {
        data: v.get("data").and_then(|x| x.as_str()).map(String::from),
        key_bytes: None,
        invalidated: v.get("invalidated").and_then(|x| x.as_bool()).unwrap_or(false),
        cancelled: v.get("cancelled").and_then(|x| x.as_bool()).unwrap_or(false),
        error: v.get("error").and_then(|x| x.as_str()).map(String::from),
        reset: false,
    });
}

/// Wake every parked prompt waiter with a reset sentinel. Called from
/// `reset_session` — a `spawn_blocking` thread on the condvar won't poll
/// session validity on its own.
pub fn on_session_reset() {
    let waiters: Vec<Waiter> = {
        let mut map = WAITERS.lock().unwrap();
        map.drain().map(|(_, w)| w).collect()
    };
    for waiter in waiters {
        let (lock, cvar) = &*waiter;
        let mut slot = lock.lock().unwrap();
        if slot.is_none() {
            *slot = Some(BiometricReply { reset: true, ..Default::default() });
        }
        cvar.notify_all();
    }
}

/// Launch `unlock` (payload = the non-secret wrapped blob) and block on its
/// reply. The secret comes back on the byte channel, never through a String.
fn run_prompt_op(method: &str, alias: &str, payload_b64: &str) -> Result<BiometricReply, BiometricError> {
    if !super::background_sync::is_activity_in_foreground() {
        return Err(BiometricError::Other(
            "no foreground activity for the biometric prompt".to_string(),
        ));
    }

    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
    let waiter: Waiter = Arc::new((Mutex::new(None), Condvar::new()));
    WAITERS.lock().unwrap().insert(request_id, waiter.clone());

    let launch = with_android_activity(|env, activity| {
        let cls = load_class(env, activity, BIOMETRIC_CLASS)?;
        let j_alias: JObject = env.new_string(alias).map_err(jni_err)?.into();
        let j_payload: JObject = env.new_string(payload_b64).map_err(jni_err)?.into();
        env.call_static_method(
            &cls,
            method,
            "(Landroid/app/Activity;ILjava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(activity),
                JValue::Int(request_id),
                JValue::Object(&j_alias),
                JValue::Object(&j_payload),
            ],
        )
        .map_err(jni_err)?;
        Ok(())
    });
    if let Err(e) = launch {
        WAITERS.lock().unwrap().remove(&request_id);
        return Err(BiometricError::Other(format!("failed to launch biometric op: {e}")));
    }

    await_reply(request_id, &waiter)
}

/// Park on `waiter` until the prompt reports, then classify. TAKES the reply
/// out of the slot (never clones): a cloned key would leave a second copy in
/// the mutex to drop un-wiped.
fn await_reply(request_id: i32, waiter: &Waiter) -> Result<BiometricReply, BiometricError> {
    let (lock, cvar) = &**waiter;
    let reply = {
        let guard = lock.lock().unwrap();
        let (mut guard, timeout_res) = cvar
            .wait_timeout_while(guard, Duration::from_secs(PROMPT_TIMEOUT_SECS), |r| r.is_none())
            .unwrap();
        if timeout_res.timed_out() {
            WAITERS.lock().unwrap().remove(&request_id);
            return Err(BiometricError::Other("biometric prompt timed out".to_string()));
        }
        guard.take()
    };
    WAITERS.lock().unwrap().remove(&request_id);

    let reply = reply.ok_or_else(|| BiometricError::Other("biometric result missing".to_string()))?;
    if reply.reset {
        return Err(BiometricError::Cancelled);
    }
    if reply.invalidated {
        return Err(BiometricError::Invalidated);
    }
    if reply.cancelled {
        return Err(BiometricError::Cancelled);
    }
    if let Some(e) = &reply.error {
        return Err(BiometricError::Other(e.clone()));
    }
    Ok(reply)
}

// ============================================================================
// Public API (called from the Tauri command layer inside spawn_blocking)
// ============================================================================

/// Hardware availability: "available", "none_enrolled", "no_hardware", "unsupported".
pub fn availability() -> Result<String, String> {
    with_android_context(|env, ctx| {
        let cls = load_class(env, ctx, BIOMETRIC_CLASS)?;
        let res = env
            .call_static_method(
                &cls,
                "availability",
                "(Landroid/content/Context;)Ljava/lang/String;",
                &[JValue::Object(ctx)],
            )
            .map_err(jni_err)?;
        let jobj = res.l().map_err(jni_err)?;
        let s: String = env.get_string(&JString::from(jobj)).map_err(jni_err)?.into();
        Ok(s)
    })
}

/// Enroll: wrap `key` under a fresh auth-gated keystore key. Returns the
/// wrapped blob (base64 of iv||ct) after the user passes the prompt. The key
/// crosses JNI as a byte array — a Java String copy could not be wiped.
pub fn enroll_wrap(alias: &str, key: &[u8; 32]) -> Result<String, BiometricError> {
    if !super::background_sync::is_activity_in_foreground() {
        return Err(BiometricError::Other(
            "no foreground activity for the biometric prompt".to_string(),
        ));
    }
    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
    let waiter: Waiter = Arc::new((Mutex::new(None), Condvar::new()));
    WAITERS.lock().unwrap().insert(request_id, waiter.clone());

    let launch = with_android_activity(|env, activity| {
        let cls = load_class(env, activity, BIOMETRIC_CLASS)?;
        let j_alias: JObject = env.new_string(alias).map_err(jni_err)?.into();
        // byte_array_from_slice copies into the JVM heap; Kotlin wipes that
        // copy on every exit path, and this side never holds a String.
        let j_key = env.byte_array_from_slice(key).map_err(jni_err)?;
        env.call_static_method(
            &cls,
            "enroll",
            "(Landroid/app/Activity;ILjava/lang/String;[B)V",
            &[
                JValue::Object(activity),
                JValue::Int(request_id),
                JValue::Object(&j_alias),
                JValue::Object(&j_key),
            ],
        )
        .map_err(jni_err)?;
        Ok(())
    });
    if let Err(e) = launch {
        WAITERS.lock().unwrap().remove(&request_id);
        return Err(BiometricError::Other(format!("failed to launch enroll: {e}")));
    }

    let reply = await_reply(request_id, &waiter)?;
    reply
        .data
        .ok_or_else(|| BiometricError::Other("enroll result carried no data".to_string()))
}

/// Unlock: unwrap the 32 vault-key bytes from `wrapped_b64` after the prompt.
/// The bytes arrive on the dedicated byte-array callback — no String/JSON copy.
pub fn unlock_unwrap(alias: &str, wrapped_b64: &str) -> Result<[u8; 32], BiometricError> {
    let mut reply = run_prompt_op("unlock", alias, wrapped_b64)?;
    // `bytes` is Zeroizing: every exit path below wipes it on drop.
    let bytes = reply
        .key_bytes
        .take()
        .ok_or_else(|| BiometricError::Other("unlock result carried no key".to_string()))?;
    if bytes.len() != 32 {
        return Err(BiometricError::Other("unwrapped key has wrong length".to_string()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// The OS's localized label for the unlock affordance ("Use fingerprint",
/// "Use screen lock", ...). Empty = caller falls back to default copy.
pub fn unlock_label() -> Result<String, String> {
    with_android_context(|env, ctx| {
        let cls = load_class(env, ctx, BIOMETRIC_CLASS)?;
        let res = env
            .call_static_method(
                &cls,
                "unlockLabel",
                "(Landroid/content/Context;)Ljava/lang/String;",
                &[JValue::Object(ctx)],
            )
            .map_err(jni_err)?;
        let jobj = res.l().map_err(jni_err)?;
        let s: String = env.get_string(&JString::from(jobj)).map_err(jni_err)?.into();
        Ok(s)
    })
}

/// Delete the keystore key for `alias`. Safe when absent; best-effort.
pub fn remove_key(alias: &str) {
    let _ = with_android_context(|env, ctx| {
        let cls = load_class(env, ctx, BIOMETRIC_CLASS)?;
        let j_alias: JObject = env.new_string(alias).map_err(jni_err)?.into();
        env.call_static_method(
            &cls,
            "removeKey",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&j_alias)],
        )
        .map_err(jni_err)?;
        Ok(())
    });
}

// ============================================================================
// JNI native callback — wakes the keyed waiter
// ============================================================================

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn Java_io_vectorapp_BiometricUnlock_nativeOnBiometricResult<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    request_id: jint,
    result_json: JString<'local>,
) {
    let json: String = match env.get_string(&result_json) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    deliver_result(request_id, &json);
}

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn Java_io_vectorapp_BiometricUnlock_nativeOnBiometricKey<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    request_id: jint,
    data: jni::objects::JByteArray<'local>,
) {
    let bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(_) => return,
    };
    deliver_reply(request_id, BiometricReply {
        key_bytes: Some(zeroize::Zeroizing::new(bytes)),
        ..Default::default()
    });
}
