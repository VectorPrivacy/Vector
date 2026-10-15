//! TLS for the relay sockets Vector opens itself: ring, the webpki roots, a verifier that
//! remembers each certificate chain it has verified, and session resumption.
//!
//! A boot opens a hundred-odd relay sockets to a handful of hosts, and each handshake verified
//! the same chain again: two P-384 ECDSA checks per socket, a fifth of boot CPU. A remembered
//! chain is accepted again for that exact name, and a resumed session skips the certificate
//! and its signature altogether.
//!
//! A resumed session tells the server it is talking to the same client as before, so both
//! caches live on the account's session, one set per lane: an account's sockets never resume
//! another account's sessions, and a shared-lane socket (a stranger can name its host) never
//! resumes one that identified as the npub. A switch drops them with the session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, DistinguishedName, Error, SignatureScheme};
use web_time::Instant;

/// How long a verified chain is trusted again without checking: a boot's burst of sockets,
/// well short of the time a certificate lives past its last check.
const REMEMBER: Duration = Duration::from_secs(15 * 60);
/// Chains remembered at once; past it the oldest go.
const CAP: usize = 256;

#[derive(Debug)]
struct RememberingVerifier {
    inner: Arc<WebPkiServerVerifier>,
    seen: Mutex<HashMap<[u8; 32], Instant>>,
}

/// The chain and the name it was presented for, as one key.
fn chain_key(end_entity: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>], name: &ServerName<'_>) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut part = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    part(name.to_str().as_bytes());
    part(end_entity.as_ref());
    for c in intermediates {
        part(c.as_ref());
    }
    h.finalize().into()
}

impl ServerCertVerifier for RememberingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let key = chain_key(end_entity, intermediates, server_name);
        {
            let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            if seen.get(&key).is_some_and(|at| at.elapsed() < REMEMBER) {
                return Ok(ServerCertVerified::assertion());
            }
        }
        let verified = self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)?;
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() >= CAP {
            seen.retain(|_, at| at.elapsed() < REMEMBER);
            if seen.len() >= CAP {
                if let Some(oldest) = seen.iter().min_by_key(|(_, at)| **at).map(|(k, _)| *k) {
                    seen.remove(&oldest);
                }
            }
        }
        seen.insert(key, Instant::now());
        Ok(verified)
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }

    fn root_hint_subjects(&self) -> Option<&[DistinguishedName]> {
        self.inner.root_hint_subjects()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static P: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    P.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())).clone()
}

fn roots() -> Arc<rustls::RootCertStore> {
    static R: OnceLock<Arc<rustls::RootCertStore>> = OnceLock::new();
    R.get_or_init(|| Arc::new(rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() })).clone()
}

fn verifier() -> Arc<RememberingVerifier> {
    let inner = WebPkiServerVerifier::builder_with_provider(roots(), provider())
        .build()
        .expect("the webpki roots build a verifier");
    Arc::new(RememberingVerifier { inner, seen: Mutex::new(HashMap::new()) })
}

/// A client config with its own chain memory and session store.
fn remembering_config() -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS 1.2 and 1.3")
        .dangerous()
        .with_custom_certificate_verifier(verifier())
        .with_no_client_auth()
}

#[derive(Default)]
struct LaneConfigs {
    account: OnceLock<Arc<rustls::ClientConfig>>,
    shared: OnceLock<Arc<rustls::ClientConfig>>,
}

struct WebsocketTls;

/// The config `session`'s relay websockets on `lane` use.
pub fn websocket_config(session: &Arc<crate::db::Session>, lane: crate::transport::Lane) -> Arc<rustls::ClientConfig> {
    let configs = session.scoped::<WebsocketTls, LaneConfigs>();
    let slot = match lane {
        crate::transport::Lane::Account => &configs.account,
        crate::transport::Lane::Shared => &configs.shared,
    };
    slot.get_or_init(|| Arc::new(remembering_config())).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn der(bytes: &[u8]) -> CertificateDer<'static> {
        CertificateDer::from(bytes.to_vec())
    }

    #[test]
    fn a_chain_is_keyed_by_its_name_and_every_certificate() {
        let a = ServerName::try_from("relay.example.com").unwrap();
        let b = ServerName::try_from("other.example.com").unwrap();
        let k = chain_key(&der(b"leaf"), &[der(b"int")], &a);
        assert_eq!(k, chain_key(&der(b"leaf"), &[der(b"int")], &a));
        assert_ne!(k, chain_key(&der(b"leaf"), &[der(b"int")], &b), "another name");
        assert_ne!(k, chain_key(&der(b"leaf2"), &[der(b"int")], &a), "another leaf");
        assert_ne!(k, chain_key(&der(b"leaf"), &[der(b"int2")], &a), "another intermediate");
        assert_ne!(chain_key(&der(b"ab"), &[der(b"c")], &a), chain_key(&der(b"a"), &[der(b"bc")], &a), "lengths are part of the key");
    }

    #[test]
    fn each_account_and_lane_resumes_only_its_own_sessions() {
        use crate::transport::Lane;
        let a = crate::db::Session::empty();
        let b = crate::db::Session::empty();
        let a_account = websocket_config(&a, Lane::Account);
        assert!(Arc::ptr_eq(&a_account, &websocket_config(&a, Lane::Account)), "one store per account and lane");
        assert!(!Arc::ptr_eq(&a_account, &websocket_config(&a, Lane::Shared)), "lanes never share");
        assert!(!Arc::ptr_eq(&a_account, &websocket_config(&b, Lane::Account)), "accounts never share");
    }

    #[test]
    fn an_unverifiable_chain_is_never_remembered() {
        let v = verifier();
        let name = ServerName::try_from("relay.example.com").unwrap();
        let bogus = der(b"not a certificate");
        assert!(v.verify_server_cert(&bogus, &[], &name, &[], UnixTime::now()).is_err());
        assert!(v.verify_server_cert(&bogus, &[], &name, &[], UnixTime::now()).is_err(), "a failure is not cached as a pass");
    }
}
