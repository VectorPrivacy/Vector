//! Accounts whose identity lives in a signer app on the phone (NIP-55, such as Amber): nothing
//! secret is stored here, and every signature is the signer app's.

use nostr_sdk::prelude::*;
use vector_core::db;
#[cfg(target_os = "android")]
use zeroize::Zeroizing;

/// A signer app this device can sign with is installed.
pub fn installed() -> bool {
    matches!(vector_core::nip55_is_installed(), Ok(true))
}

/// The paired identity while this account signs through a signer app, and whether that app is
/// still installed.
pub fn status() -> Option<(String, bool)> {
    if db::get_signer_type().ok()?.as_str() != "nip55" {
        return None;
    }
    let hex = db::get_nip55_user_pubkey().ok().flatten()?;
    let npub = PublicKey::from_hex(&hex).ok()?.to_bech32().ok()?;
    Some((npub, installed()))
}

/// Pair with the signer app, then stage the account for its security step, as a key import
/// would. An identity already on this device becomes the one to unlock instead.
#[cfg(target_os = "android")]
pub async fn login() -> Result<super::account::Imported, String> {
    use super::account::{self, Imported};
    super::security::refuse_if_migrating()?;
    if !installed() {
        return Err(crate::t::login_signer_app_missing().into());
    }
    if vector_core::state::nostr_client().is_some() {
        return Err(crate::t::login_already_signed_in().into());
    }
    let (pk, package) = vector_core::nip55_pair().await?;
    let npub = pk.to_bech32().map_err(|e| e.to_string())?;
    if account::is_committed(&npub) {
        db::write_active_account_file(&npub)?;
        return Ok(Imported::Existing(npub));
    }
    vector_core::set_pending_nip55_setup(pk.to_hex(), package.clone());
    vector_core::set_signer_kind(vector_core::SignerKind::Nip55);
    crate::android::external_signer::set_signer_package(package);
    vector_core::state::set_my_public_key(pk);
    account::install_client();
    account::insert_own_profile(&npub).await;
    account::set_pending(npub.clone());
    Ok(Imported::Staged(npub))
}

/// The staged signer-app account's commit: a key, when it has one, opens only the local data,
/// proven later by the canary it seals.
#[cfg(target_os = "android")]
pub(super) async fn commit(
    sealed: Option<(Zeroizing<[u8; 32]>, vector_core::crypto::Kdf)>,
    security_type: &str,
    wrap: Option<String>,
) -> Result<(), String> {
    let (hex, package) = vector_core::pending_nip55_setup().ok_or_else(|| crate::t::login_signer_setup_lost().to_string())?;
    super::account::open_pending().await?;
    let canary = sealed
        .as_ref()
        .map(|(key, _)| vector_core::crypto::encrypt_with_key(db::at_rest::NIP55_PIN_CANARY, key))
        .transpose()?;
    let kdf_row = sealed.as_ref().and_then(|(_, kdf)| kdf.descriptor());
    let encrypted = sealed.is_some();
    db::commit_nip55_account_setup(
        &hex,
        &package,
        encrypted,
        encrypted.then_some(security_type),
        wrap.as_deref(),
        canary.as_deref(),
        kdf_row.as_deref(),
    )?;
    vector_core::clear_pending_nip55_setup();
    if let Some((key, _)) = &sealed {
        vector_core::ENCRYPTION_KEY.set(**key, &[&vector_core::MY_SECRET_KEY]);
    }
    vector_core::state::set_encryption_enabled(encrypted);
    super::account::touch_last_active();
    vector_core::blossom_servers::refresh_cache();
    Ok(())
}

/// Pair with the signer app again, as after it was reinstalled or its permission revoked. A
/// different identity is refused: this account's data must never mix with another's.
#[cfg(target_os = "android")]
pub async fn reauthorize() -> Result<(), String> {
    if vector_core::signer_kind() != vector_core::SignerKind::Nip55 {
        return Err(crate::t::settings_signer_not_offline().into());
    }
    if !installed() {
        vector_core::set_nip55_state(vector_core::Nip55State::Missing);
        return Err(crate::t::settings_signer_app_missing().into());
    }
    let expected = db::get_nip55_user_pubkey()?
        .ok_or_else(|| crate::t::login_signer_setup_lost().to_string())?
        .to_ascii_lowercase();
    let (pk, package) = vector_core::nip55_pair().await?;
    if pk.to_hex() != expected {
        return Err(crate::t::settings_signer_other_identity().into());
    }
    db::set_nip55_signer_package(&package)?;
    crate::android::external_signer::set_signer_package(package);
    vector_core::set_nip55_state(vector_core::Nip55State::Ready);
    Ok(())
}
