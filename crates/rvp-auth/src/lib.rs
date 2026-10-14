//! RVP media-grant verification (plan §3.4, §5).
//!
//! NestJS (`iss=nest`) signs short-lived EdDSA/Ed25519 JWTs; the media server
//! verifies them **offline** with the shared public key. No HS256 shared secret
//! ever touches the media path.
//!
//! Grant claims validated on every `Hello`:
//! - `iss = nest` (configurable)
//! - `aud = rvp`
//! - `exp` in the future (leeway-skewed)
//! - `sub` (user_id), `room_id`, `role` present

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

pub const DEFAULT_ISSUER: &str = "nest";
pub const DEFAULT_AUDIENCE: &str = "rvp";

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid token: {0}")]
    InvalidToken(String),
    #[error("token expired")]
    Expired,
    #[error("wrong issuer")]
    WrongIssuer,
    #[error("wrong audience")]
    WrongAudience,
    #[error("missing claim: {0}")]
    MissingClaim(&'static str),
}

/// Validated media-grant claims (subset carried after verification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantClaims {
    /// `sub` — user id from the control plane.
    pub user_id: String,
    pub room_id: String,
    /// e.g. `host` | `guest` (authorization checks land in P3+).
    pub role: String,
    /// Optional unique token id (force-kick denylist hook, P3/P5).
    pub jti: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawClaims {
    sub: String,
    room_id: String,
    role: String,
    #[serde(default)]
    jti: Option<String>,
}

/// Offline JWT verifier bound to one issuer public key.
pub struct Verifier {
    decoding_key: DecodingKey,
    validation: Validation,
}

impl Verifier {
    /// Build from the NestJS Ed25519 public key in PEM (SPKI) form.
    pub fn from_public_pem(pem: &[u8]) -> Result<Self, AuthError> {
        let decoding_key = DecodingKey::from_ed_pem(pem)
            .map_err(|e| AuthError::InvalidToken(format!("public key: {e}")))?;
        Ok(Self::with_key(decoding_key))
    }

    /// Build from raw SPKI DER (tests / embedded configs).
    pub fn from_public_der(der: &[u8]) -> Result<Self, AuthError> {
        Ok(Self::with_key(DecodingKey::from_ed_der(der)))
    }

    fn with_key(decoding_key: DecodingKey) -> Self {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[DEFAULT_ISSUER]);
        validation.set_audience(&[DEFAULT_AUDIENCE]);
        validation.leeway = 5;
        Self {
            decoding_key,
            validation,
        }
    }

    /// Verify signature + claims and extract the grant.
    pub fn verify(&self, token: &str) -> Result<GrantClaims, AuthError> {
        let data = jsonwebtoken::decode::<RawClaims>(token, &self.decoding_key, &self.validation)
            .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::Expired,
            jsonwebtoken::errors::ErrorKind::InvalidIssuer => AuthError::WrongIssuer,
            jsonwebtoken::errors::ErrorKind::InvalidAudience => AuthError::WrongAudience,
            _ => AuthError::InvalidToken(e.to_string()),
        })?;
        let raw = data.claims;
        Ok(GrantClaims {
            user_id: raw.sub,
            room_id: raw.room_id,
            role: raw.role,
            jti: raw.jti,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    #[derive(serde::Serialize)]
    struct TestClaims {
        sub: &'static str,
        room_id: &'static str,
        role: &'static str,
        iss: &'static str,
        aud: &'static str,
        exp: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        jti: Option<&'static str>,
    }

    fn keypair() -> (rcgen::KeyPair, Verifier) {
        let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let verifier = Verifier::from_public_pem(kp.public_key_pem().as_bytes()).unwrap();
        (kp, verifier)
    }

    fn sign(kp: &rcgen::KeyPair, claims: &TestClaims) -> String {
        let key = EncodingKey::from_ed_pem(kp.serialize_pem().as_bytes()).unwrap();
        encode(&Header::new(Algorithm::EdDSA), claims, &key).unwrap()
    }

    fn future_exp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120
    }

    #[test]
    fn valid_grant_verifies() {
        let (kp, verifier) = keypair();
        let token = sign(
            &kp,
            &TestClaims {
                sub: "user-1",
                room_id: "room-a",
                role: "host",
                iss: DEFAULT_ISSUER,
                aud: DEFAULT_AUDIENCE,
                exp: future_exp(),
                jti: Some("jti-1"),
            },
        );
        let claims = verifier.verify(&token).unwrap();
        assert_eq!(claims.user_id, "user-1");
        assert_eq!(claims.room_id, "room-a");
        assert_eq!(claims.role, "host");
        assert_eq!(claims.jti.as_deref(), Some("jti-1"));
    }

    #[test]
    fn expired_grant_rejected() {
        let (kp, verifier) = keypair();
        let token = sign(
            &kp,
            &TestClaims {
                sub: "user-1",
                room_id: "room-a",
                role: "guest",
                iss: DEFAULT_ISSUER,
                aud: DEFAULT_AUDIENCE,
                exp: 1, // long past
                jti: None,
            },
        );
        assert!(matches!(verifier.verify(&token), Err(AuthError::Expired)));
    }

    #[test]
    fn wrong_issuer_rejected() {
        let (kp, verifier) = keypair();
        let token = sign(
            &kp,
            &TestClaims {
                sub: "user-1",
                room_id: "room-a",
                role: "guest",
                iss: "evil",
                aud: DEFAULT_AUDIENCE,
                exp: future_exp(),
                jti: None,
            },
        );
        assert!(matches!(
            verifier.verify(&token),
            Err(AuthError::WrongIssuer)
        ));
    }

    #[test]
    fn wrong_audience_rejected() {
        let (kp, verifier) = keypair();
        let token = sign(
            &kp,
            &TestClaims {
                sub: "user-1",
                room_id: "room-a",
                role: "guest",
                iss: DEFAULT_ISSUER,
                aud: "other-api",
                exp: future_exp(),
                jti: None,
            },
        );
        assert!(matches!(
            verifier.verify(&token),
            Err(AuthError::WrongAudience)
        ));
    }

    #[test]
    fn garbage_token_rejected() {
        let (_kp, verifier) = keypair();
        assert!(verifier.verify("not-a-jwt").is_err());
    }
}
