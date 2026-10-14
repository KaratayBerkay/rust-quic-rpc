//! Pairwise payload encryption (plan §3.2, ADR-004).
//!
//! **CLIENTS ONLY** — `rvp-server` never links this crate and never holds media
//! keys; the SFU forwards ciphertext opaquely.
//!
//! - Cipher: AES-128-GCM
//! - Nonce: 12 bytes = 8 zero bytes ‖ u32 BE `Seq` (rekey via `kid` long before wrap)
//! - `kid` rides in the RVP header (offset 3); `PT=1` marks encrypted Opus

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};

pub const KEY_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("encryption failed")]
    Seal,
    #[error("decryption failed (wrong key, seq, or tampered payload)")]
    Open,
}

/// One pairwise AES-128 key (NestJS delivers these out-of-band, plan §5).
#[derive(Clone)]
pub struct PairwiseKey {
    cipher: Aes128Gcm,
}

impl PairwiseKey {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Self {
            cipher: Aes128Gcm::new_from_slice(key).expect("16-byte key"),
        }
    }

    /// nonce = zeros(8) ‖ seq.to_be_bytes()  (rekey before u32 wrap)
    pub fn nonce_for_seq(seq: u32) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        nonce[8..].copy_from_slice(&seq.to_be_bytes());
        nonce
    }

    /// Encrypt `plaintext` for one recipient pair at media `seq`.
    /// Output: ciphertext ‖ 16-byte GCM tag.
    pub fn seal(&self, seq: u32, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let nonce = Self::nonce_for_seq(seq);
        self.cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| CryptoError::Seal)
    }

    /// Decrypt a payload received for this pair at media `seq`.
    pub fn open(&self, seq: u32, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let nonce = Self::nonce_for_seq(seq);
        self.cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext)
            .map_err(|_| CryptoError::Open)
    }

    /// Encrypt with AAD (e.g. sealed header bytes) for tamper-binding.
    pub fn seal_with_ad(
        &self,
        seq: u32,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce = Self::nonce_for_seq(seq);
        self.cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::Seal)
    }

    /// Decrypt with AAD.
    pub fn open_with_ad(
        &self,
        seq: u32,
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let nonce = Self::nonce_for_seq(seq);
        self.cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];

    #[test]
    fn seal_open_roundtrip() {
        let k = PairwiseKey::new(&KEY);
        let pt = b"twenty milliseconds of opus";
        let ct = k.seal(7, pt).unwrap();
        assert_eq!(ct.len(), pt.len() + TAG_LEN);
        assert_eq!(k.open(7, &ct).unwrap(), pt);
    }

    #[test]
    fn wrong_seq_fails_open() {
        let k = PairwiseKey::new(&KEY);
        let ct = k.seal(7, b"secret").unwrap();
        assert_eq!(k.open(8, &ct), Err(CryptoError::Open));
    }

    #[test]
    fn nonce_deterministic_and_seq_bound() {
        assert_eq!(
            PairwiseKey::nonce_for_seq(0x0102_0304),
            [0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x02, 0x03, 0x04]
        );
    }

    /// Fixed test vector — cross-language (TS/Flutter) must match this output.
    #[test]
    fn aes_gcm_test_vector() {
        let k = PairwiseKey::new(&KEY);
        let ct = k.seal(1, b"rvp-test-vector-1").unwrap();
        let hex: String = ct.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "c8a3df4eb98cb95a6332219f57ca7c09edf3bdfdd685fb8270b3ca62826050d6eb"
        );
    }

    #[test]
    fn aad_binding_rejects_tampered_header() {
        let k = PairwiseKey::new(&KEY);
        let header = [1u8; 16];
        let ct = k.seal_with_ad(3, &header, b"opus").unwrap();
        let mut bad = header;
        bad[3] = 9; // tamper kid
        assert_eq!(k.open_with_ad(3, &bad, &ct), Err(CryptoError::Open));
        assert_eq!(k.open_with_ad(3, &header, &ct).unwrap(), b"opus");
    }
}
