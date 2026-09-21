//! Notification levels and mutes: the command surface, the `chats.muted`
//! mirror, and the expiry sweep.
//!
//! The mirror exists because `muted` is read from a dozen places that predate
//! per-scope preferences, and a community-scope mute or a lapsed timer changes
//! the answer for chats nobody touched. Every write here ends by reconciling
//! it, so those readers stay correct without learning about the chain.

use vector_core::notify::{self, NotifyLevel, MUTE_FOREVER, MUTE_OFF};

/// One scope as the frontend needs it: what is stored here, and what the chain
/// resolves to, so a menu can tick the right row and print the inherited value.
#[derive(serde::Serialize)]
pub struct NotifyView {
    pub scope_id: String,
    /// The level stored for this scope alone, or null when it inherits.
    pub level: Option<String>,
    pub mute_until: i64,
    pub suppress_everyone: Option<bool>,
    /// What the chain resolves to before the mute clamp.
    pub effective_level: String,
    pub muted: bool,
    pub everyone_allowed: bool,
    /// The App Settings kill switch, so a menu can say why its @everyone rows
    /// are inert rather than leaving the user to guess.
    pub everyone_muted_globally: bool,
}

fn view(scope_id: &str, community_id: Option<&str>) -> NotifyView {
    let stored = notify::prefs(scope_id);
    let resolved = notify::resolve(scope_id, community_id, notify::now_ms());
    NotifyView {
        scope_id: scope_id.to_string(),
        level: stored.level.map(|l| l.as_str().to_string()),
        mute_until: stored.mute_until,
        suppress_everyone: stored.suppress_everyone,
        effective_level: resolved.level.as_str().to_string(),
        muted: resolved.muted,
        everyone_allowed: notify::everyone_allowed(community_id.or(Some(scope_id))),
        everyone_muted_globally: !notify::everyone_allowed(None),
    }
}

/// Every scope a community's menus need in one round trip: the community
/// itself, then each channel resolved against it.
#[tauri::command]
pub async fn get_notify_prefs(
    community_id: Option<String>,
    scope_ids: Vec<String>,
) -> Result<Vec<NotifyView>, String> {
    vector_core::db::scoped(async move {
        notify::warm();
        let cid = community_id.as_deref();
        let mut out = Vec::with_capacity(scope_ids.len() + 1);
        if let Some(id) = cid {
            out.push(view(id, None));
        }
        for scope in scope_ids.iter() {
            if Some(scope.as_str()) == cid {
                continue;
            }
            out.push(view(scope, cid));
        }
        Ok(out)
    })
    .await
}

/// `level` is one of "all" | "mentions" | "nothing", or null to inherit.
#[tauri::command]
pub async fn set_notify_level(scope_id: String, level: Option<String>) -> Result<(), String> {
    let parsed = match level.as_deref() {
        None => None,
        Some(s) => Some(NotifyLevel::from_label(s).ok_or_else(|| format!("unknown level: {s}"))?),
    };
    vector_core::db::scoped(async move {
        notify::set_level(&scope_id, parsed)?;
        reconcile().await;
        Ok(())
    })
    .await
}

/// `duration_ms` is the length of the mute; 0 unmutes, and a negative value
/// means indefinite. An absolute deadline would be the caller's clock rather
/// than ours, which is the one thing a synced timestamp must not be.
#[tauri::command]
pub async fn set_notify_mute(scope_id: String, duration_ms: i64) -> Result<(), String> {
    let until = if duration_ms == 0 {
        MUTE_OFF
    } else if duration_ms < 0 {
        MUTE_FOREVER
    } else {
        notify::now_ms().saturating_add(duration_ms)
    };
    vector_core::db::scoped(async move {
        notify::set_mute(&scope_id, until)?;
        reconcile().await;
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_suppress_everyone(community_id: String, suppress: Option<bool>) -> Result<(), String> {
    vector_core::db::scoped(async move {
        notify::set_suppress_everyone(&community_id, suppress)?;
        reconcile().await;
        Ok(())
    })
    .await
}

// ============================================================================
// Mirror + sweep
// ============================================================================

/// Bring `chats.muted` back in line with what the chain resolves to, persisting
/// and surfacing only the rows that actually moved.
///
/// Split out from [`reconcile`] because the badge seed calls this on its way to
/// computing the first count of a login, and routing that through the badge
/// path would be a cycle.
pub async fn reconcile_mirror() {
    // An unloaded cache answers "default" for every scope, so acting on it here
    // would clear every mute in memory AND persist it. Same rule as the publish
    // gate: never overwrite state from a view we could not read.
    if !notify::is_loaded() {
        notify::warm();
        if !notify::is_loaded() {
            return;
        }
    }
    let (changed, slims, resolved) = {
        let mut state = crate::STATE.lock().await;
        let now = notify::now_ms();
        let mut moved: Vec<(String, bool)> = Vec::new();
        let mut rows = Vec::with_capacity(state.chats.len());
        for i in 0..state.chats.len() {
            // Resolve without the stored flag folded in, or a mirror that is
            // already stale would keep confirming itself.
            let community = state.chats[i]
                .metadata
                .custom_fields
                .get("community_id")
                .cloned();
            let want = notify::resolve(&state.chats[i].id, community.as_deref(), now);
            if state.chats[i].muted != want.muted {
                state.chats[i].muted = want.muted;
                moved.push((state.chats[i].id.clone(), want.muted));
            }
            rows.push(serde_json::json!({
                "id": state.chats[i].id,
                "muted": want.muted,
                "notify": want.ring.as_u8(),
                "everyone": notify::everyone_pings_for_chat(&state.chats[i]),
            }));
        }
        let slims: Vec<_> = state
            .chats
            .iter()
            .filter(|c| moved.iter().any(|(id, _)| id == &c.id))
            .map(|c| crate::db::chats::SlimChatDB::from_chat(c, &state.interner))
            .collect();
        (moved, slims, rows)
    };

    for slim in slims {
        let _ = crate::db::chats::save_slim_chat(slim).await;
    }
    for (chat_id, muted) in &changed {
        vector_core::traits::emit_event_json(
            "chat_muted",
            serde_json::json!({ "chat_id": chat_id, "value": muted }),
        );
    }
    // A level change moves no mirror but still changes what every row badges,
    // so this carries the resolved pair for all of them rather than a signal to
    // go and ask again.
    vector_core::traits::emit_event_json(
        "notify_prefs_changed",
        serde_json::json!({ "chats": resolved }),
    );
}

/// Mirror, re-badge and re-arm, without publishing.
///
/// What an apply path uses. Publishing from inside one answers a sibling's list
/// with this device's pre-adoption view, which is the overwrite the hydration
/// gate exists to prevent.
pub async fn reconcile_local() {
    reconcile_mirror().await;
    let counts = crate::db::unread_counts().await.unwrap_or_default();
    crate::STATE.lock().await.unread_seed(counts);
    if let Some(handle) = crate::TAURI_APP.get() {
        let _ = crate::commands::messaging::update_unread_counter(handle.clone()).await;
    }
    arm_expiry_timer();
}

/// The full write path: everything above, plus both projections.
pub async fn reconcile() {
    reconcile_local().await;
    crate::commands::prefs::publish_projection(vector_core::synced_prefs::Pref::Notify);
    crate::commands::prefs::publish_projection(vector_core::synced_prefs::Pref::Mutes);
}

/// Clear whatever has run out and repaint. Cheap when nothing has.
pub async fn sweep_now() {
    if notify::sweep_expired(notify::now_ms()).is_empty() {
        arm_expiry_timer();
        return;
    }
    reconcile().await;
}

/// The boot variant, for the badge seed: it may not republish or re-badge,
/// because the seed it runs inside is what computes that badge.
pub async fn sweep_before_seed() {
    if !notify::sweep_expired(notify::now_ms()).is_empty() {
        reconcile_mirror().await;
    }
    arm_expiry_timer();
}

/// The session's pending expiry wake-up, so re-arming replaces rather than
/// stacks.
struct ExpiryTimer;

fn timer_slot() -> std::sync::Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>> {
    vector_core::db::current_session().scoped::<ExpiryTimer, _>()
}

/// Sleep until the nearest expiry and sweep. One timer for every mute: a timer
/// each would be N wake-ups to do one thing.
///
/// A suspended device fires nothing, which is why the foreground sweep exists;
/// this is the case where the app stays open long enough to watch a mute end.
pub fn arm_expiry_timer() {
    let slot = timer_slot();
    let next = notify::next_expiry(notify::now_ms());
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(previous) = guard.take() {
        // The timer's own body re-arms, and aborting the task we are running in
        // would cancel whatever the caller does after this returns.
        if !previous.is_finished() && tokio::task::try_id() != Some(previous.id()) {
            previous.abort();
        }
    }
    let Some(at) = next else { return };
    let delay = (at - notify::now_ms()).max(0) as u64;
    *guard = Some(vector_core::db::spawn_bound(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        sweep_now().await;
    }));
}

// Handlers: get_notify_prefs, set_notify_level, set_notify_mute, set_suppress_everyone
