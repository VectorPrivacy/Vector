//! Android NIP-55 signer bridge — the platform impl of [`vector_core::Nip55Backend`].
//!
//! Two transports to the on-device signer app (Amber), both delegating the
//! heavy Java work to the Kotlin `ExternalSigner` object so the Rust side stays
//! a thin orchestrator:
//!
//! - **ContentResolver** (`content_query`) — the silent background hot path.
//!   Once the user grants "remember", every `sign_event` / `nip44_*` is a
//!   background query with no UI. This is what makes per-message decrypt/sign
//!   viable (we can't pop an Amber dialog per gift wrap).
//! - **Intent for result** (`run_intent`) — pairing (`get_public_key`) and the
//!   fallback when a ContentResolver query comes back `rejected`/null. Needs a
//!   foreground Activity, so it's gated on `is_activity_in_foreground()`: in the
//!   background an un-remembered op fails soft (NotAuthorized) and defers to
//!   foreground rather than launching Amber behind the user's back.
//!
//! Blocking is fine here: `Nip55Signer` calls every method inside
//! `spawn_blocking` behind a bounded semaphore, so parking on the Intent
//! condvar doesn't starve the async runtime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::time::Duration;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::jint;
use jni::JNIEnv;

use vector_core::{Nip55Backend, Nip55Error, Nip55ResolverOutcome};

use super::utils::{with_android_activity, with_android_context};

const EXTERNAL_SIGNER_CLASS: &str = "io/vectorapp/ExternalSigner";
/// Pairing is a multi-screen human flow (pick account, choose permission mode,
/// tap Connect) — 60s is too tight and times out a slow-but-valid user.
const PAIRING_TIMEOUT_SECS: u64 = 300;
/// A single foreground sign approval (the fallback for an un-remembered kind).
/// The user just taps Accept, but give room to read the request.
const SIGN_INTENT_TIMEOUT_SECS: u64 = 120;

fn jni_err<E: std::fmt::Debug>(e: E) -> String {
    format!("{:?}", e)
}

/// Resolve an app (`io.vectorapp.*`) class via the Context's PathClassLoader.
/// `env.find_class` on a native (tokio/spawn_blocking) thread uses the boot
/// classloader, which can't see app classes — so every JNI call into
/// `ExternalSigner` MUST route through here or it throws ClassNotFound. Mirrors
/// `android::storage::load_class`.
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
// Paired signer package — process-global, per-session
// ============================================================================
//
// The ContentResolver authority is `content://<package>.<METHOD>` and every
// post-pairing intent pins `<package>`, so the signer's package name must be
// resolvable on every op. It's public material (persisted plaintext as
// `nip55_signer_package`); this global is the hot-path cache, set at login /
// pairing and cleared on account swap so account A's signer can't be addressed
// under account B.

static SIGNER_PACKAGE: RwLock<Option<String>> = RwLock::new(None);

/// Pin the paired signer's package for this session. Called from the NIP-55
/// login/boot branch and after a successful pairing.
pub fn set_signer_package(package: String) {
    if let Ok(mut g) = SIGNER_PACKAGE.write() {
        *g = Some(package);
    }
}

fn signer_package() -> Option<String> {
    SIGNER_PACKAGE.read().ok().and_then(|g| g.clone())
}

// ============================================================================
// Intent-for-result waiters — keyed by request id
// ============================================================================
//
// Each foreground intent installs a keyed waiter; the Kotlin
// `onActivityResult → nativeOnSignerResult` path wakes it. Per-id keying (not a
// single slot) means a queued second request can't steal the first's result,
// and a swap can cancel each parked thread individually.

#[derive(Clone, Default)]
struct SignerReply {
    result: Option<String>,
    event: Option<String>,
    package: Option<String>,
    rejected: bool,
    cancelled: bool,
}

type Waiter = Arc<(Mutex<Option<SignerReply>>, Condvar)>;

static INTENT_WAITERS: LazyLock<Mutex<HashMap<i32, Waiter>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_REQUEST_ID: AtomicI32 = AtomicI32::new(1);

fn parse_reply_json(json: &str) -> SignerReply {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    SignerReply {
        result: v.get("result").and_then(|x| x.as_str()).map(String::from),
        event: v.get("event").and_then(|x| x.as_str()).map(String::from),
        package: v.get("package").and_then(|x| x.as_str()).map(String::from),
        rejected: v.get("rejected").and_then(|x| x.as_bool()).unwrap_or(false),
        cancelled: false,
    }
}

fn deliver_signer_result(request_id: i32, json: &str) {
    let reply = parse_reply_json(json);
    // Deliver ONLY by exact request-id match. Amber echoes the `id` we send, so
    // an exact match always exists for a live request. A miss means the waiter
    // already timed out and was removed; delivering its late reply to whatever
    // else is in flight would cross-contaminate an unrelated op (a decrypt
    // getting another request's plaintext), so drop it and let the caller time
    // out cleanly.
    let waiter = { INTENT_WAITERS.lock().unwrap().get(&request_id).cloned() };
    if let Some(waiter) = waiter {
        let (lock, cvar) = &*waiter;
        *lock.lock().unwrap() = Some(reply);
        cvar.notify_all();
    }
}

/// Wake every stranded Intent waiter with a cancelled sentinel and drop the
/// pinned package. Called from `reset_session` — a `spawn_blocking` thread
/// parked on the condvar won't poll session validity on its own, and each
/// parked thread holds its own `Arc`, so `notify_all` per waiter is required
/// (clearing the map alone would leak the threads until their timeout).
pub fn on_session_reset() {
    if let Ok(mut g) = SIGNER_PACKAGE.write() {
        *g = None;
    }
    let waiters: Vec<Waiter> = {
        let mut map = INTENT_WAITERS.lock().unwrap();
        map.drain().map(|(_, w)| w).collect()
    };
    for waiter in waiters {
        let (lock, cvar) = &*waiter;
        let mut slot = lock.lock().unwrap();
        if slot.is_none() {
            *slot = Some(SignerReply {
                cancelled: true,
                ..Default::default()
            });
        }
        cvar.notify_all();
    }
}

// ============================================================================
// ContentResolver — the silent background hot path
// ============================================================================

/// Query the signer's ContentResolver for one op. `arg0/arg1/arg2` are the
/// projection slots Amber overloads: `[payload, counterparty, current_user]`
/// (counterparty is empty for `sign_event`). Returns the tri-state the caller
/// must keep apart: `requires_approval` (null/empty cursor -> escalate to the
/// Intent) vs `rejected` column (remembered reject -> stop) vs a value.
fn content_query(authority: &str, arg0: &str, arg1: &str, arg2: &str) -> Nip55ResolverOutcome {
    let blob = match with_android_context(|env, ctx| {
        let cls = load_class(env, ctx, EXTERNAL_SIGNER_CLASS)?;
        let a: JObject = env.new_string(authority).map_err(jni_err)?.into();
        let s0: JObject = env.new_string(arg0).map_err(jni_err)?.into();
        let s1: JObject = env.new_string(arg1).map_err(jni_err)?.into();
        let s2: JObject = env.new_string(arg2).map_err(jni_err)?.into();
        let res = env
            .call_static_method(
                &cls,
                "queryContentResolver",
                "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                &[
                    JValue::Object(ctx),
                    JValue::Object(&a),
                    JValue::Object(&s0),
                    JValue::Object(&s1),
                    JValue::Object(&s2),
                ],
            )
            .map_err(jni_err)?;
        let jobj = res.l().map_err(jni_err)?;
        let s: String = env.get_string(&JString::from(jobj)).map_err(jni_err)?.into();
        Ok(s)
    }) {
        Ok(s) => s,
        Err(e) => return Nip55ResolverOutcome::Error(e),
    };

    let v: serde_json::Value = match serde_json::from_str(&blob) {
        Ok(v) => v,
        Err(e) => return Nip55ResolverOutcome::Error(format!("bad signer reply: {e}")),
    };
    // Tri-state: a null/empty cursor is NOT a rejection and NOT an empty decrypt.
    if v.get("requires_approval").and_then(|x| x.as_bool()).unwrap_or(false) {
        return Nip55ResolverOutcome::RequiresApproval;
    }
    if v.get("rejected").and_then(|x| x.as_bool()).unwrap_or(false) {
        return Nip55ResolverOutcome::Rejected;
    }
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        return Nip55ResolverOutcome::Error(err.to_string());
    }
    Nip55ResolverOutcome::Value {
        result: v.get("result").and_then(|x| x.as_str()).map(String::from),
        event: v.get("event").and_then(|x| x.as_str()).map(String::from),
    }
}

// ============================================================================
// Intent for result — pairing + fallback
// ============================================================================

/// Launch a signer intent for result and block (bounded by timeout_secs) on its reply.
/// MUST only be reached with a foreground Activity — guarded here to avoid the
/// `with_android_activity` panic in the Activity-less service process.
fn run_intent(
    intent_type: &str,
    data: &str,
    counterparty: Option<&str>,
    current_user: Option<&str>,
    perms_json: Option<&str>,
    pin_package: bool,
    timeout_secs: u64,
) -> Result<SignerReply, Nip55Error> {
    if !super::background_sync::is_activity_in_foreground() {
        return Err(Nip55Error::Ipc(
            "no foreground activity to prompt the signer".to_string(),
        ));
    }

    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::SeqCst);
    let waiter: Waiter = Arc::new((Mutex::new(None), Condvar::new()));
    INTENT_WAITERS.lock().unwrap().insert(request_id, waiter.clone());

    // Pairing (`get_public_key`) sends NO package so the OS resolves the
    // installed signer; every post-pairing intent pins the paired package.
    let package = if pin_package { signer_package() } else { None };

    let launch = with_android_activity(|env, activity| {
        let cls = load_class(env, activity, EXTERNAL_SIGNER_CLASS)?;
        let j_type: JObject = env.new_string(intent_type).map_err(jni_err)?.into();
        let j_data: JObject = env.new_string(data).map_err(jni_err)?.into();
        let j_pk: JObject = env.new_string(counterparty.unwrap_or("")).map_err(jni_err)?.into();
        let j_cu: JObject = env.new_string(current_user.unwrap_or("")).map_err(jni_err)?.into();
        let j_perms: JObject = env.new_string(perms_json.unwrap_or("")).map_err(jni_err)?.into();
        let j_pkg: JObject = env.new_string(package.as_deref().unwrap_or("")).map_err(jni_err)?.into();
        env.call_static_method(
            &cls,
            "launch",
            "(Landroid/app/Activity;ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(activity),
                JValue::Int(request_id),
                JValue::Object(&j_type),
                JValue::Object(&j_data),
                JValue::Object(&j_pk),
                JValue::Object(&j_cu),
                JValue::Object(&j_perms),
                JValue::Object(&j_pkg),
            ],
        )
        .map_err(jni_err)?;
        Ok(())
    });
    if let Err(e) = launch {
        INTENT_WAITERS.lock().unwrap().remove(&request_id);
        return Err(Nip55Error::Ipc(format!("failed to launch signer intent: {e}")));
    }

    let (lock, cvar) = &*waiter;
    let reply = {
        let guard = lock.lock().unwrap();
        let (guard, timeout_res) = cvar
            .wait_timeout_while(guard, Duration::from_secs(timeout_secs), |r| r.is_none())
            .unwrap();
        if timeout_res.timed_out() {
            INTENT_WAITERS.lock().unwrap().remove(&request_id);
            return Err(Nip55Error::Ipc("signer request timed out".to_string()));
        }
        guard.clone()
    };
    INTENT_WAITERS.lock().unwrap().remove(&request_id);

    let reply = reply.ok_or_else(|| Nip55Error::Ipc("signer result missing".to_string()))?;
    if reply.cancelled {
        return Err(Nip55Error::Ipc("cancelled by session reset".to_string()));
    }
    if reply.rejected {
        return Err(Nip55Error::NotAuthorized);
    }
    Ok(reply)
}

// ============================================================================
// AmberBackend — the registered Nip55Backend impl
// ============================================================================

struct AmberBackend;

impl Nip55Backend for AmberBackend {
    fn is_installed(&self) -> Result<bool, Nip55Error> {
        with_android_context(|env, ctx| {
            let cls = load_class(env, ctx, EXTERNAL_SIGNER_CLASS)?;
            let res = env
                .call_static_method(&cls, "isInstalled", "(Landroid/content/Context;)Z", &[JValue::Object(ctx)])
                .map_err(jni_err)?;
            res.z().map_err(jni_err)
        })
        .map_err(Nip55Error::Ipc)
    }

    fn get_public_key_pairing(&self, perms_json: &str) -> Result<(String, String), Nip55Error> {
        if !super::background_sync::is_activity_in_foreground() {
            return Err(Nip55Error::Ipc(
                "pairing requires the app to be in the foreground".to_string(),
            ));
        }
        let reply = run_intent("get_public_key", "", None, None, Some(perms_json), false, PAIRING_TIMEOUT_SECS)?;
        let pk = reply
            .result
            .ok_or_else(|| Nip55Error::Ipc("signer returned no pubkey".to_string()))?;
        let package = reply
            .package
            .ok_or_else(|| Nip55Error::Ipc("signer returned no package".to_string()))?;
        Ok((pk, package))
    }

    fn resolver_op(&self, method: &str, data: &str, counterparty: &str, current_user: &str) -> Nip55ResolverOutcome {
        let pkg = match signer_package() {
            Some(p) => p,
            None => return Nip55ResolverOutcome::Error("no signer package pinned".to_string()),
        };
        let authority = format!("{}.{}", pkg, method);
        content_query(&authority, data, counterparty, current_user)
    }

    fn intent_op(
        &self,
        intent_type: &str,
        data: &str,
        counterparty: &str,
        current_user: &str,
    ) -> Result<(Option<String>, Option<String>), Nip55Error> {
        let cp = if counterparty.is_empty() { None } else { Some(counterparty) };
        let reply = run_intent(intent_type, data, cp, Some(current_user), None, true, SIGN_INTENT_TIMEOUT_SECS)?;
        Ok((reply.result, reply.event))
    }

    fn is_foreground(&self) -> bool {
        super::background_sync::is_activity_in_foreground()
    }
}

/// Register the Amber backend with vector-core. Idempotent (OnceLock) — safe to
/// call from both the Activity startup hook and the background service init.
pub fn register() {
    vector_core::set_nip55_backend(Box::new(AmberBackend));
}

// ============================================================================
// JNI native callback — wakes the keyed Intent waiter
// ============================================================================

#[no_mangle]
#[allow(non_snake_case)]
pub extern "system" fn Java_io_vectorapp_ExternalSigner_nativeOnSignerResult<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    request_id: jint,
    result_json: JString<'local>,
) {
    let json: String = match env.get_string(&result_json) {
        Ok(s) => s.into(),
        Err(_) => return,
    };
    deliver_signer_result(request_id, &json);
}
