//! Pairwise payload crypto for Flutter (plan §3.2 / ADR-004).
//!
//! Thin `flutter_rust_bridge` surface over `rvp-crypto` — clients only; the
//! SFU never sees keys. One [`PairwiseKey`] per recipient pair; the GCM nonce
//! is derived from the media `Seq` (8 zero bytes ‖ `seq.to_be_bytes()`).

use rvp_crypto::{CryptoError as RvpCryptoError, PairwiseKey as RvpInner};

/// Pairwise crypto failure (bad key length, wrong key/tampered payload).
#[derive(Debug, thiserror::Error)]
#[error("pairwise crypto: {message}")]
pub struct PairwiseCryptoError {
    pub message: String,
}

impl PairwiseCryptoError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// One pairwise AES-128-GCM key (delivered out-of-band by the control plane).
///
/// The same key seals outbound media for one recipient pair and opens
/// inbound media stamped with that pair's `kid`.
pub struct PairwiseKey {
    inner: RvpInner,
}

impl PairwiseKey {
    /// Create from a raw 16-byte AES-128 key.
    pub fn new(key: Vec<u8>) -> Result<Self, PairwiseCryptoError> {
        let arr: [u8; rvp_crypto::KEY_LEN] = key.as_slice().try_into().map_err(|_| {
            PairwiseCryptoError::new(format!(
                "key must be {} bytes, got {}",
                rvp_crypto::KEY_LEN,
                key.len()
            ))
        })?;
        Ok(Self {
            inner: RvpInner::new(&arr),
        })
    }

    /// Encrypt one Opus packet for this pair at media `seq`.
    /// Returns ciphertext ‖ 16-byte GCM tag.
    pub fn seal(&self, seq: u32, plaintext: Vec<u8>) -> Result<Vec<u8>, PairwiseCryptoError> {
        self.inner.seal(seq, &plaintext).map_err(map_err)
    }

    /// Decrypt one received payload for this pair at media `seq`.
    pub fn open(&self, seq: u32, ciphertext: Vec<u8>) -> Result<Vec<u8>, PairwiseCryptoError> {
        self.inner.open(seq, &ciphertext).map_err(map_err)
    }
}

fn map_err(e: RvpCryptoError) -> PairwiseCryptoError {
    PairwiseCryptoError::new(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];

    #[test]
    fn seal_open_roundtrip_and_vector() {
        let k = PairwiseKey::new(KEY.to_vec()).unwrap();
        let ct = k.seal(1, b"rvp-test-vector-1".to_vec()).unwrap();
        let hex: String = ct.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "c8a3df4eb98cb95a6332219f57ca7c09edf3bdfdd685fb8270b3ca62826050d6eb"
        );
        assert_eq!(k.open(1, ct).unwrap(), b"rvp-test-vector-1".to_vec());
    }

    #[test]
    fn rejects_bad_key_length() {
        let err = match PairwiseKey::new(vec![0u8; 15]) {
            Ok(_) => panic!("15-byte key must be rejected"),
            Err(e) => e,
        };
        assert!(err.message.contains("15"));
    }

    #[test]
    fn wrong_seq_open_fails() {
        let k = PairwiseKey::new(KEY.to_vec()).unwrap();
        let ct = k.seal(7, b"secret".to_vec()).unwrap();
        assert!(k.open(8, ct).is_err());
    }
}
