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
    /// Optional media-node pin (P8a): the server that hosts this room. When
    /// present, a media server whose `--node-id` differs refuses the join so
    /// the client can retry the pinned node (plan §2/§5).
    pub media_node: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawClaims {
    sub: String,
    room_id: String,
    role: String,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    media_node: Option<String>,
}

/// Offline JWT verifier bound to the control plane's Ed25519 key set.
///
/// Holds one key after [`Verifier::from_public_pem`] / [`Verifier::from_public_der`],
/// or the full NestJS JWKS after [`Verifier::from_jwks`] (plan §5). The key set
/// can be swapped at runtime via [`Verifier::refresh_jwks`] — used by `rvp-cli`
/// for signing-key rotation without a media-server restart. Verification is
/// sync and lock-cheap; `refresh_jwks` never blocks the media path longer than
/// a key-set swap.
pub struct Verifier {
    keys: std::sync::RwLock<Vec<DecodingKey>>,
    validation: Validation,
}

impl Verifier {
    /// Build from the NestJS Ed25519 public key in PEM (SPKI) form.
    pub fn from_public_pem(pem: &[u8]) -> Result<Self, AuthError> {
        let decoding_key = DecodingKey::from_ed_pem(pem)
            .map_err(|e| AuthError::InvalidToken(format!("public key: {e}")))?;
        Ok(Self::with_keys(vec![decoding_key]))
    }

    /// Build from raw SPKI DER (tests / embedded configs).
    pub fn from_public_der(der: &[u8]) -> Result<Self, AuthError> {
        Ok(Self::with_keys(vec![DecodingKey::from_ed_der(der)]))
    }

    /// Build from a NestJS `GET /auth/jwks` document (plan §5).
    ///
    /// Keeps every OKP/Ed25519 key the `jsonwebtoken` build understands and
    /// ignores the rest (rotation: old + new keys coexist in the set).
    /// Errors when the document carries no usable key.
    pub fn from_jwks(jwks: &str) -> Result<Self, AuthError> {
        let keys = parse_jwks(jwks)?;
        Ok(Self::with_keys(keys))
    }

    /// Atomically replace the key set (signing-key rotation). Returns the
    /// number of usable keys after the swap.
    pub fn refresh_jwks(&self, jwks: &str) -> Result<usize, AuthError> {
        let keys = parse_jwks(jwks)?;
        let count = keys.len();
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = keys;
        Ok(count)
    }

    fn with_keys(keys: Vec<DecodingKey>) -> Self {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[DEFAULT_ISSUER]);
        validation.set_audience(&[DEFAULT_AUDIENCE]);
        validation.leeway = 5;
        Self {
            keys: std::sync::RwLock::new(keys),
            validation,
        }
    }

    /// Verify signature + claims and extract the grant.
    ///
    /// Tries every key in the set (JWKS rotation keeps old + new keys valid
    /// until refresh drops the old one).
    pub fn verify(&self, token: &str) -> Result<GrantClaims, AuthError> {
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        if keys.is_empty() {
            return Err(AuthError::InvalidToken("no verification keys".into()));
        }
        let mut last: Option<AuthError> = None;
        for key in keys.iter() {
            match Self::decode_with(key, token, &self.validation) {
                Ok(claims) => return Ok(claims),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("non-empty key set"))
    }

    fn decode_with(
        decoding_key: &DecodingKey,
        token: &str,
        validation: &Validation,
    ) -> Result<GrantClaims, AuthError> {
        let data = jsonwebtoken::decode::<RawClaims>(token, decoding_key, validation).map_err(
            |e| match e.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::Expired,
                jsonwebtoken::errors::ErrorKind::InvalidIssuer => AuthError::WrongIssuer,
                jsonwebtoken::errors::ErrorKind::InvalidAudience => AuthError::WrongAudience,
                _ => AuthError::InvalidToken(e.to_string()),
            },
        )?;
        let raw = data.claims;
        Ok(GrantClaims {
            user_id: raw.sub,
            room_id: raw.room_id,
            role: raw.role,
            jti: raw.jti,
            media_node: raw.media_node,
        })
    }
}

/// Parse a JWKS document into decoding keys (Ed25519 only; skips the rest).
fn parse_jwks(jwks: &str) -> Result<Vec<DecodingKey>, AuthError> {
    let set: jsonwebtoken::jwk::JwkSet =
        serde_json::from_str(jwks).map_err(|e| AuthError::InvalidToken(format!("jwks: {e}")))?;
    let mut keys = Vec::new();
    for jwk in &set.keys {
        if let Ok(key) = DecodingKey::from_jwk(jwk) {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return Err(AuthError::InvalidToken("jwks: no OKP/Ed25519 keys".into()));
    }
    Ok(keys)
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
        #[serde(skip_serializing_if = "Option::is_none")]
        media_node: Option<&'static str>,
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
                media_node: None,
            },
        );
        let claims = verifier.verify(&token).unwrap();
        assert_eq!(claims.user_id, "user-1");
        assert_eq!(claims.room_id, "room-a");
        assert_eq!(claims.role, "host");
        assert_eq!(claims.jti.as_deref(), Some("jti-1"));
        assert_eq!(claims.media_node, None);
    }

    #[test]
    fn media_node_claim_verifies() {
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
                jti: None,
                media_node: Some("node-1"),
            },
        );
        let claims = verifier.verify(&token).unwrap();
        assert_eq!(claims.media_node.as_deref(), Some("node-1"));
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
                media_node: None,
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
                media_node: None,
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
                media_node: None,
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

    /// Minimal NestJS-shaped JWKS document for one Ed25519 key.
    fn jwks_for(kp: &rcgen::KeyPair, kid: &str) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let der = kp.public_key_der();
        let raw = &der[der.len() - 32..]; // SPKI prefix ‖ 32-byte raw key
        let x = URL_SAFE_NO_PAD.encode(raw);
        format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"{kid}","x":"{x}","alg":"EdDSA","use":"sig"}}]}}"#
        )
    }

    fn claims_for(sub: &'static str, exp: u64) -> TestClaims {
        TestClaims {
            sub,
            room_id: "room-a",
            role: "host",
            iss: DEFAULT_ISSUER,
            aud: DEFAULT_AUDIENCE,
            exp,
            jti: None,
            media_node: None,
        }
    }

    #[test]
    fn jwks_verifier_accepts_signed_grant() {
        let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let verifier = Verifier::from_jwks(&jwks_for(&kp, "key-1")).unwrap();
        let token = sign(&kp, &claims_for("alice", future_exp()));
        assert_eq!(verifier.verify(&token).unwrap().user_id, "alice");
    }

    #[test]
    fn jwks_rejects_document_without_ed_keys() {
        assert!(matches!(
            Verifier::from_jwks(r#"{"keys":[]}"#),
            Err(AuthError::InvalidToken(_))
        ));
        assert!(matches!(
            Verifier::from_jwks("not json"),
            Err(AuthError::InvalidToken(_))
        ));
    }

    #[test]
    fn jwks_rotation_old_key_rejected_after_refresh() {
        let old = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let new = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();

        let verifier = Verifier::from_jwks(&jwks_for(&old, "key-old")).unwrap();
        let old_token = sign(&old, &claims_for("alice", future_exp()));
        let new_token = sign(&new, &claims_for("alice", future_exp()));
        assert!(verifier.verify(&old_token).is_ok());
        assert!(verifier.verify(&new_token).is_err());

        // Rotation: refresh to the new key set (old key rotated out).
        assert_eq!(
            verifier.refresh_jwks(&jwks_for(&new, "key-new")).unwrap(),
            1
        );
        assert!(verifier.verify(&old_token).is_err());
        assert!(verifier.verify(&new_token).is_ok());
    }

    #[test]
    fn jwks_overlapping_set_accepts_both_keys() {
        let old = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let new = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        // During rotation the JWKS serves both keys; both tokens must verify.
        let both = format!(
            r#"{{"keys":[{},{}
            ]}}"#,
            jwks_for(&old, "key-old")
                .trim_start_matches(r#"{"keys":["#)
                .trim_end_matches("]}"),
            jwks_for(&new, "key-new")
                .trim_start_matches(r#"{"keys":["#)
                .trim_end_matches("]}"),
        );
        let verifier = Verifier::from_jwks(&both).unwrap();
        let old_token = sign(&old, &claims_for("alice", future_exp()));
        let new_token = sign(&new, &claims_for("bob", future_exp()));
        assert_eq!(verifier.verify(&old_token).unwrap().user_id, "alice");
        assert_eq!(verifier.verify(&new_token).unwrap().user_id, "bob");
    }

    /// Live document captured from `services/nest` running with
    /// NEST_EDDSA_SEED=00…00 (interoperability fixture — plan §5 contract).
    const NEST_JWKS_FIXTURE: &str = r#"{"keys":[{"kty":"OKP","crv":"Ed25519","x":"O2onvM62pC1io6jQKm8Nc2UyFXcd4kOmOsBIoYtZ2ik","kid":"nest-1","alg":"EdDSA","use":"sig"}]}"#;

    #[test]
    fn nest_jwks_fixture_verifies_zero_seed_grant() {
        // Sign with the same Ed25519 zero-seed key Nest used (RFC 8410 PKCS8).
        let mut pkcs8 = vec![
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        pkcs8.extend_from_slice(&[0u8; 32]);
        let key = EncodingKey::from_ed_der(&pkcs8);
        let claims = TestClaims {
            sub: "alice",
            room_id: "room-interop",
            role: "host",
            iss: DEFAULT_ISSUER,
            aud: DEFAULT_AUDIENCE,
            exp: future_exp(),
            jti: Some("jti-interop"),
            media_node: None,
        };
        let token = encode(&Header::new(Algorithm::EdDSA), &claims, &key).unwrap();

        let verifier = Verifier::from_jwks(NEST_JWKS_FIXTURE).unwrap();
        let grant = verifier.verify(&token).unwrap();
        assert_eq!(grant.user_id, "alice");
        assert_eq!(grant.room_id, "room-interop");
        assert_eq!(grant.role, "host");
    }
}
