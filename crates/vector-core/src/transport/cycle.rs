//! Moving relay sockets onto the network now in use. Runs on the embedding app's runtime, never
//! the transport runtime: nostr-sdk spawns a relay's tasks on the runtime that connects it.

use std::sync::Mutex;

use nostr_sdk::prelude::{Client, RelayStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleScope {
    /// New circuit, bridges, circuit mode: the main client's relays reconnect at once.
    Circuits,
    /// The fail-closed step of a network switch: everything that pools or authenticates
    /// outside a guarded socket is dropped.
    Switch,
    /// The network became ready: revive every relay the switch left dead.
    Kick,
}

const TRACK_CAP: usize = 8;

struct Tracked {
    id: u64,
    owner: u64,
    client: Client,
}

static TRACKED: Mutex<Vec<Tracked>> = Mutex::new(Vec::new());
static TRACK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A client outside the session's own (Android background, device transfer) that `Kick` revives
/// too, until [`untrack`]; shut-down clients and those of a session no longer live drop out.
pub fn track(client: &Client) -> u64 {
    let id = TRACK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut v = TRACKED.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut v);
    if v.len() >= TRACK_CAP {
        v.remove(0);
    }
    v.push(Tracked { id, owner: crate::db::current_session_id(), client: client.clone() });
    id
}

/// A tracked client that is done: Kick must not bring a merely disconnected one back.
pub fn untrack(id: u64) {
    TRACKED.lock().unwrap_or_else(|e| e.into_inner()).retain(|t| t.id != id);
}

fn prune(v: &mut Vec<Tracked>) {
    let live = crate::db::live_session_id();
    v.retain(|t| !t.client.is_shutdown() && t.owner == live);
}

fn tracked() -> Vec<Client> {
    let mut v = TRACKED.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut v);
    v.iter().map(|t| t.client.clone()).collect()
}

#[cfg(test)]
pub(crate) fn tracked_count() -> usize {
    tracked().len()
}

pub async fn cycle_all(scope: CycleScope) {
    match scope {
        CycleScope::Circuits => {
            // First: pooled HTTP connections must not keep riding the circuits being left while
            // the relays reconnect.
            crate::net::forget_clients();
            cycle_circuits().await;
            crate::net::forget_clients();
        }
        CycleScope::Switch => {
            crate::signer::suspend_bunker().await;
            crate::community::transport::on_transport_switch();
            crate::community::transport::clear_plane_pool();
            #[cfg(feature = "xdc")]
            crate::xdc::mesh::retire_live_if_idle().await;
            crate::net::forget_clients();
        }
        CycleScope::Kick => {
            let mut clients: Vec<Client> = crate::state::nostr_client().into_iter().collect();
            clients.extend(tracked());
            futures_util::future::join_all(clients.iter().map(kick)).await;
            crate::signer::resume_bunker().await;
        }
    }
}

/// Every relay of the main client, GOSSIP-only ones included, reconnects through the current
/// transport. Per-relay `try_connect`: `client.connect()` can wedge a relay in Pending.
async fn cycle_circuits() {
    let Some(client) = crate::state::nostr_client() else { return };
    let relays = client.relays().all().await;
    crate::log_info!("[Tor] cycling {} relay connection(s) onto new transport...", relays.len());
    let budget = super::budget(super::Op::RelayConnect, std::time::Duration::from_secs(15));
    for relay in relays.values() {
        relay.disconnect();
    }
    // Let the old connection tasks see the terminate signal: a reconnect while one still runs
    // gets the fresh socket dropped on arrival.
    crate::rt::time::sleep(std::time::Duration::from_millis(500)).await;
    futures_util::future::join_all(relays.into_values().map(|relay| async move {
        let _ = relay.try_connect().timeout(budget).await;
    }))
    .await;
    crate::log_info!("[Tor] relay transport switch complete");
}

/// A relay a revival pass should try: connectable (Initialized when added while the network was
/// down, Terminated once its socket died) and not refused by policy, which no retry changes.
pub fn revivable(relay: &nostr_sdk::prelude::Relay) -> bool {
    matches!(relay.status(), RelayStatus::Initialized | RelayStatus::Terminated | RelayStatus::Sleeping)
        && super::refuses(relay.url().as_str()).is_none()
}

async fn kick(client: &Client) {
    let budget = super::budget(super::Op::RelayConnect, std::time::Duration::from_secs(15));
    let relays = client.relays().all().await;
    futures_util::future::join_all(relays.into_values().filter(revivable).map(|relay| async move {
        let _ = relay.try_connect().timeout(budget).await;
    }))
    .await;
}
