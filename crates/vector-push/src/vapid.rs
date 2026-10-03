//! RFC 8292 application server keys. Each device makes its own, so no key links two devices
//! at the push service, and whoever holds a device's key plus its endpoint can push to it.

use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::SecretKey;

use crate::{b64url, unb64url, Error, Result};

/// A fresh key pair: the private scalar and the uncompressed public point, both base64url.
/// The public half is the `applicationServerKey` a browser subscribes with.
pub fn generate() -> (String, String) {
    let secret = SecretKey::random(&mut rand::rngs::OsRng);
    let public = secret.public_key().to_encoded_point(false);
    (b64url(&secret.to_bytes()), b64url(public.as_bytes()))
}

pub fn public_key(secret_b64: &str) -> Result<String> {
    let secret = parse(secret_b64)?;
    Ok(b64url(secret.public_key().to_encoded_point(false).as_bytes()))
}

fn parse(secret_b64: &str) -> Result<SecretKey> {
    SecretKey::from_slice(&unb64url(secret_b64)?).map_err(|_| Error::Key("bad VAPID key"))
}

/// The `Authorization` header for a push to `endpoint`, valid for 12 hours from `now`.
pub fn authorization(secret_b64: &str, endpoint: &str, subject: &str, now: u64) -> Result<String> {
    let secret = parse(secret_b64)?;
    let audience = origin(endpoint).ok_or(Error::Format("endpoint is not an https URL".into()))?;
    let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::json!({ "aud": audience, "exp": now + 12 * 3600, "sub": subject });
    let unsigned = format!("{header}.{}", b64url(claims.to_string().as_bytes()));
    let signature: Signature = SigningKey::from(&secret).sign(unsigned.as_bytes());
    let public = b64url(secret.public_key().to_encoded_point(false).as_bytes());
    Ok(format!("vapid t={unsigned}.{}, k={public}", b64url(&signature.to_bytes())))
}

/// `https://host[:port]` of an endpoint, the JWT's audience.
pub fn origin(endpoint: &str) -> Option<&str> {
    let rest = endpoint.strip_prefix("https://")?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    if host_end == 0 {
        return None;
    }
    Some(&endpoint[..8 + host_end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    #[test]
    fn the_jwt_verifies_against_the_advertised_key() {
        let (secret, public) = generate();
        let header = authorization(&secret, "https://web.push.apple.com/QGuQyavXutnMH", "mailto:push@vectorapp.io", 1_800_000_000).unwrap();
        let (t, k) = header.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(k, public);
        let (unsigned, sig) = t.rsplit_once('.').unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&unb64url(unsigned.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["exp"], 1_800_000_000u64 + 43_200);
        let key = VerifyingKey::from_sec1_bytes(&unb64url(k).unwrap()).unwrap();
        let sig = Signature::from_slice(&unb64url(sig).unwrap()).unwrap();
        key.verify(unsigned.as_bytes(), &sig).unwrap();
    }

    #[test]
    fn audience_is_scheme_host_and_port() {
        assert_eq!(origin("https://fcm.googleapis.com/fcm/send/abc"), Some("https://fcm.googleapis.com"));
        assert_eq!(origin("https://push.example:8443/x"), Some("https://push.example:8443"));
        assert_eq!(origin("https://push.example"), Some("https://push.example"));
        assert_eq!(origin("http://push.example/x"), None);
    }
}
