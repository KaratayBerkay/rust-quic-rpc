/**
 * Pairwise AES-128-GCM media crypto — TypeScript port of `rvp-crypto`.
 *
 * Matches the Rust implementation exactly:
 * - key: raw 16-byte AES-128 per (room, pair)
 * - nonce: 8 zero bytes ‖ u32 BE of the media sequence
 * - AAD: none in the production path (empty additionalData)
 * - output: ciphertext ‖ 16-byte GCM tag (payload grows by 16 bytes)
 *
 * Cross-anchored test vector (must match Rust, Flutter, and TS):
 *   key  000102030405060708090a0b0c0d0e0f
 *   seq  1
 *   pt   "rvp-test-vector-1"
 *   ct   c8a3df4eb98cb95a6332219f57ca7c09edf3bdfdd685fb8270b3ca62826050d6eb
 */

export const KEY_LEN = 16;
export const NONCE_LEN = 12;
export const TAG_LEN = 16;

export class PairwiseCryptoError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "PairwiseCryptoError";
  }
}

/** nonce = zeros(8) ‖ seq.to_be_bytes() — must match Rust `nonce_for_seq`. */
export function nonceForSeq(seq: number): Uint8Array {
  const nonce = new Uint8Array(NONCE_LEN);
  new DataView(nonce.buffer).setUint32(8, seq >>> 0, false);
  return nonce;
}

function toBuffer(view: Uint8Array): ArrayBuffer {
  return view.buffer.slice(
    view.byteOffset,
    view.byteOffset + view.byteLength,
  ) as ArrayBuffer;
}

async function importKey(keyBytes: Uint8Array): Promise<CryptoKey> {
  if (keyBytes.length !== KEY_LEN) {
    throw new PairwiseCryptoError(`expected ${KEY_LEN}-byte key, got ${keyBytes.length}`);
  }
  return crypto.subtle.importKey("raw", toBuffer(keyBytes), { name: "AES-GCM" }, false, [
    "encrypt",
    "decrypt",
  ]);
}

/**
 * Encrypt `plaintext` for one recipient pair at media `seq`.
 * Output: ciphertext ‖ 16-byte GCM tag.
 */
export async function seal(
  keyBytes: Uint8Array,
  seq: number,
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const key = await importKey(keyBytes);
  const nonce = nonceForSeq(seq);
  try {
    const ct = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: toBuffer(nonce), tagLength: TAG_LEN * 8 },
      key,
      toBuffer(plaintext),
    );
    return new Uint8Array(ct);
  } catch {
    throw new PairwiseCryptoError("encryption failed");
  }
}

/** Decrypt a payload received for this pair at media `seq`. */
export async function open(
  keyBytes: Uint8Array,
  seq: number,
  ciphertext: Uint8Array,
): Promise<Uint8Array> {
  if (ciphertext.length < TAG_LEN) {
    throw new PairwiseCryptoError("decryption failed (ciphertext shorter than GCM tag)");
  }
  const key = await importKey(keyBytes);
  const nonce = nonceForSeq(seq);
  try {
    const pt = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: toBuffer(nonce), tagLength: TAG_LEN * 8 },
      key,
      toBuffer(ciphertext),
    );
    return new Uint8Array(pt);
  } catch {
    throw new PairwiseCryptoError("decryption failed (wrong key, seq, or tampered payload)");
  }
}

/** Convenience wrapper holding one pairwise key. */
export class PairwiseKey {
  private readonly keyBytes: Uint8Array;

  constructor(keyBytes: Uint8Array) {
    if (keyBytes.length !== KEY_LEN) {
      throw new PairwiseCryptoError(`expected ${KEY_LEN}-byte key, got ${keyBytes.length}`);
    }
    this.keyBytes = keyBytes.slice();
  }

  seal(seq: number, plaintext: Uint8Array): Promise<Uint8Array> {
    return seal(this.keyBytes, seq, plaintext);
  }

  open(seq: number, ciphertext: Uint8Array): Promise<Uint8Array> {
    return open(this.keyBytes, seq, ciphertext);
  }
}

/** Parse a hex string (NestJS grant key field) into raw key bytes. */
export function keyFromHex(hex: string): Uint8Array {
  if (!/^[0-9a-fA-F]{32}$/.test(hex)) {
    throw new PairwiseCryptoError(`expected 32 hex chars, got ${hex.length}`);
  }
  const out = new Uint8Array(KEY_LEN);
  for (let i = 0; i < KEY_LEN; i += 1) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
