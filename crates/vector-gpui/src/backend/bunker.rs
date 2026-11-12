//! Remote signers (NIP-46): an account whose key lives in a signer app. Vector holds only a
//! client keypair that the signer approves; the account's identity is the signer's.

use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use nostr_sdk::prelude::*;
use vector_core::state::{self, PENDING_NSEC};
use vector_core::{BunkerConnectionState, MY_SECRET_KEY, SignerKind};
use zeroize::{Zeroize, Zeroizing};

use super::account::{self, Imported};

/// How a pairing started from a link goes, for the screen that shows the link.
pub enum Update {
    /// The signer answered; the user may still have to approve in it.
    Awaiting,
    Done(Result<Imported, String>),
}

/// Pairing meanwhile stops a switch of network for its sockets: say so rather than the raw error.
fn failed(pairing: &vector_core::signer::PairingGuard, e: String) -> String {
    if pairing.aborted() { vector_core::signer::NETWORK_CHANGED.to_string() } else { e }
}

fn relays() -> Result<Vec<RelayUrl>, String> {
    // Multi-relay by design: one relay down must not lock the user out of their signer.
    let relays: Vec<RelayUrl> = state::TRUSTED_RELAYS.iter().filter_map(|s| RelayUrl::parse(s).ok()).collect();
    if relays.is_empty() { Err("No trusted relays configured".into()) } else { Ok(relays) }
}

/// Drop a pairing that never reached its security step, so the next attempt starts clean.
pub async fn cancel() {
    if account::pending().is_none() && state::nostr_client().is_some() {
        return;
    }
    if let Some(b) = vector_core::drain_bunker_state() {
        let _ = b.shutdown().await;
    }
    MY_SECRET_KEY.clear(&[&vector_core::ENCRYPTION_KEY]);
    vector_core::clear_my_public_key();
    state::clear_pending_bunker_setup();
    {
        let mut g = PENDING_NSEC.lock().unwrap();
        if let Some(s) = g.as_mut() {
            s.zeroize();
        }
        *g = None;
    }
    account::abandon_pending();
    if let Some(client) = vector_core::take_nostr_client() {
        let _ = client.shutdown().await;
    }
}

async fn refuse_if_signed_in() -> Result<(), String> {
    if state::nostr_client().is_some() && account::pending().is_some() {
        cancel().await;
    }
    if state::nostr_client().is_some() {
        return Err("Another account is already signed in.".into());
    }
    Ok(())
}

/// The signer answered with `remote`: an account already here becomes the active one; a new one
/// is staged for its security step with the client key and link held in memory.
async fn stage(remote: PublicKey, client_keys: &Keys, url: String) -> Result<Imported, String> {
    let npub = remote.to_bech32().map_err(|e| e.to_string())?;
    if account::is_committed(&npub) {
        // The existing account keeps its own client key; this pairing is discarded.
        if let Some(b) = vector_core::drain_bunker_state() {
            let _ = b.shutdown().await;
        }
        vector_core::db::write_active_account_file(&npub)?;
        return Ok(Imported::Existing(npub));
    }
    if vector_core::bunker_signer().is_none() {
        return Err("The signer went away before it was set up".into());
    }
    let client_nsec = Zeroizing::new(client_keys.secret_key().to_bech32().map_err(|e| e.to_string())?);
    *PENDING_NSEC.lock().unwrap() = Some(String::clone(&client_nsec));
    state::set_pending_bunker_setup(url, remote.to_hex());
    let secret = Zeroizing::new(client_keys.secret_key().to_secret_bytes());
    MY_SECRET_KEY.set(*secret, &[&vector_core::ENCRYPTION_KEY]);
    state::set_my_public_key(remote);
    vector_core::set_signer_kind(SignerKind::Bunker);
    account::install_client();
    account::insert_own_profile(&npub).await;
    account::set_pending(npub.clone());
    vector_core::set_bunker_state(BunkerConnectionState::Online);
    Ok(Imported::Staged(npub))
}

/// Sign in with a pasted `bunker://` link: waits while the signer approves.
pub async fn connect(url: String) -> Result<Imported, String> {
    let url = url.trim().to_string();
    if !url.to_ascii_lowercase().starts_with("bunker://") {
        return Err("Must start with bunker://".into());
    }
    refuse_if_signed_in().await?;
    let client_keys = Keys::generate();
    // Long enough for a signer on a phone to be unlocked and tapped.
    let remote = vector_core::attempt_bunker_login(&url, client_keys.clone(), Duration::from_secs(60)).await?;
    match stage(remote, &client_keys, url).await {
        Ok(done) => Ok(done),
        Err(e) => {
            cancel().await;
            Err(format!("Bunker setup failed: {e}"))
        }
    }
}

/// A `nostrconnect://` link for the signer to scan or paste. The pairing runs on in the
/// background and reports through `updates`.
pub async fn start_session(updates: UnboundedSender<Update>) -> Result<String, String> {
    refuse_if_signed_in().await?;
    let client_keys = Keys::generate();
    let (nc, uri) = vector_core::build_nostrconnect_session(client_keys.clone(), relays()?, Duration::from_secs(120))?;
    vector_core::set_bunker_signer(nc);
    vector_core::set_bunker_state(BunkerConnectionState::Connecting);
    vector_core::db::spawn_bound(async move {
        let done = |r: Result<Imported, String>| {
            let _ = updates.unbounded_send(Update::Done(r));
        };
        let Some(signer) = vector_core::bunker_signer() else { return };
        let pairing = vector_core::signer::track_pairing(&signer);
        let give_up = |e: String| async move {
            vector_core::set_bunker_state(BunkerConnectionState::Offline);
            if let Some(b) = vector_core::take_bunker_signer() {
                let _ = b.shutdown().await;
            }
            e
        };
        let storage_url = match signer.bunker_uri().await {
            Ok(uri) => uri.to_string(),
            Err(e) => return done(Err(give_up(failed(&pairing, e.to_string())).await)),
        };
        let _ = updates.unbounded_send(Update::Awaiting);
        let remote = match signer.get_public_key_async().await {
            Ok(pk) => pk,
            Err(e) => {
                let e = failed(&pairing, format!("Signer didn't return your pubkey. If you're in Manual mode, check your signer app for an approval prompt. ({e})"));
                return done(Err(give_up(e).await));
            }
        };
        // Abandoned meanwhile: the screen is gone.
        if vector_core::bunker_signer().is_none() {
            let _ = signer.shutdown().await;
            return;
        }
        if pairing.install(signer).is_err() {
            return done(Err(give_up(vector_core::signer::NETWORK_CHANGED.to_string()).await));
        }
        match stage(remote, &client_keys, storage_url).await {
            Ok(r) => done(Ok(r)),
            Err(e) => {
                cancel().await;
                done(Err(e));
            }
        }
    });
    Ok(uri)
}

/// Re-pair the signer of this bunker account, keeping its client key: a `nostrconnect://` link
/// for the signer, reporting through `updates`. The live signer keeps working until the new
/// pairing has proved it is the same identity. Before a session exists (an unlock the signer
/// refused) nothing is installed: the unlock runs again on the stored link.
pub async fn reauthorize(updates: UnboundedSender<Update>) -> Result<String, String> {
    if vector_core::db::get_signer_type().ok().as_deref() != Some("bunker") {
        return Err("This account is not a remote-signer account.".into());
    }
    let expected = vector_core::db::get_bunker_remote_pubkey()
        .ok()
        .flatten()
        .ok_or("Bunker account missing cached remote pubkey")?
        .to_ascii_lowercase();
    // owned-key: the NIP-46 client keypair, which the NostrConnect session owns for its life.
    let client_keys = MY_SECRET_KEY.to_keys().ok_or("No client keypair loaded — please return to the login screen and try again")?;
    let (nc, uri) = vector_core::build_nostrconnect_session(client_keys, relays()?, Duration::from_secs(120))?;
    let pairing = vector_core::signer::track_pairing(&nc);
    let session = vector_core::db::current_session();
    vector_core::db::spawn_bound(async move {
        let done = |r: Result<Imported, String>| {
            let _ = updates.unbounded_send(Update::Done(r));
        };
        let storage_url = match nc.bunker_uri().await {
            Ok(uri) => uri.to_string(),
            Err(e) => {
                let _ = nc.shutdown().await;
                return done(Err(failed(&pairing, e.to_string())));
            }
        };
        let _ = updates.unbounded_send(Update::Awaiting);
        let remote = match nc.get_public_key_async().await {
            Ok(pk) => pk,
            Err(e) => {
                let _ = nc.shutdown().await;
                return done(Err(failed(&pairing, format!("Signer didn't return your pubkey. Check the signer app for an approval prompt. ({e})"))));
            }
        };
        if remote.to_hex() != expected {
            let _ = nc.shutdown().await;
            return done(Err("Signer returned a different identity. Re-authorize is only for the original account — logout to switch accounts.".into()));
        }
        if !session.is_live() {
            let _ = nc.shutdown().await;
            return;
        }
        if let Err(e) = vector_core::db::set_bunker_url(&storage_url).await {
            let _ = nc.shutdown().await;
            return done(Err(format!("Failed to persist bunker URL: {e}")));
        }
        let npub = match remote.to_bech32() {
            Ok(n) => n,
            Err(e) => {
                let _ = nc.shutdown().await;
                return done(Err(e.to_string()));
            }
        };
        let at_unlock = state::nostr_client().is_none();
        let old = match pairing.install(nc) {
            Ok(old) => old,
            Err(_) => return done(Err(vector_core::signer::NETWORK_CHANGED.to_string())),
        };
        if let Some(old) = old {
            let _ = old.shutdown().await;
        }
        if at_unlock {
            if let Some(b) = vector_core::take_bunker_signer() {
                let _ = b.shutdown().await;
            }
            vector_core::set_bunker_state(BunkerConnectionState::Idle);
        } else {
            state::set_my_public_key(remote);
            vector_core::set_signer_kind(SignerKind::Bunker);
            vector_core::set_bunker_state(BunkerConnectionState::Online);
        }
        done(Ok(Imported::Existing(npub)));
    });
    Ok(uri)
}

/// The signer of a stored bunker account, reconnected at unlock: the identity it returns must be
/// the one on record. Returns that identity.
pub async fn reconnect(client_keys: &Keys) -> Result<PublicKey, String> {
    let url = vector_core::db::get_bunker_url().await?.ok_or("Bunker account missing bunker_url")?;
    super::transport::wait_for_active_network().await;
    // An already-approved pairing: no human in the loop, so a short wait.
    let remote = vector_core::attempt_bunker_login(&url, client_keys.clone(), vector_core::relay_connect_timeout(Duration::from_secs(15)))
        .await
        .map_err(|e| format!("Remote signer unreachable — please ensure your signer app is online and retry. ({e})"))?;
    let expected = vector_core::db::get_bunker_remote_pubkey()?.ok_or("Bunker account missing cached remote pubkey")?;
    if !remote.to_hex().eq_ignore_ascii_case(&expected) {
        if let Some(b) = vector_core::take_bunker_signer() {
            let _ = b.shutdown().await;
        }
        vector_core::set_bunker_state(BunkerConnectionState::Idle);
        return Err("Remote signer returned a different identity than this account. Either re-authorize from Settings, or logout and re-add the account.".into());
    }
    vector_core::set_signer_kind(SignerKind::Bunker);
    Ok(remote)
}

/// The signer a bunker account is paired with, for Security settings.
pub fn status() -> Option<String> {
    if vector_core::db::get_signer_type().ok()?.as_str() != "bunker" {
        return None;
    }
    let hex = vector_core::db::get_bunker_remote_pubkey().ok().flatten()?;
    PublicKey::from_hex(&hex).ok()?.to_bech32().ok()
}
