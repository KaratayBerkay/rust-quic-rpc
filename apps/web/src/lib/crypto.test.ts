import { describe, expect, it } from "vitest";
import {
  KEY_LEN,
  NONCE_LEN,
  PairwiseCryptoError,
  PairwiseKey,
  keyFromHex,
  nonceForSeq,
  open,
  seal,
} from "./crypto";

/**
 * Cross-anchored AES-GCM vector from Rust (`rvp-crypto` tests). Key/seq/pt/ct
 * are pinned in Rust, Flutter, and TypeScript so all three cannot drift.
 */
const KEY_HEX = "000102030405060708090a0b0c0d0e0f";
const CT_HEX = "c8a3df4eb98cb95a6332219f57ca7c09edf3bdfdd685fb8270b3ca62826050d6eb";

function enc(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

function dec(b: Uint8Array): string {
  return new TextDecoder().decode(b);
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function fromHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

describe("nonce construction", () => {
  it("builds 8 zero bytes ‖ seq BE", () => {
    expect(Array.from(nonceForSeq(0x01020304))).toEqual([
      0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4,
    ]);
    expect(nonceForSeq(1).length).toBe(NONCE_LEN);
  });

  it("wraps seq at u32 max", () => {
    expect(Array.from(nonceForSeq(0xffffffff))).toEqual([
      0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff,
    ]);
  });
});

describe("pairwise AES-128-GCM", () => {
  it("matches the pinned Rust test vector", async () => {
    const key = keyFromHex(KEY_HEX);
    const ct = await seal(key, 1, enc("rvp-test-vector-1"));
    expect(toHex(ct)).toBe(CT_HEX);
  });

  it("opens the pinned Rust test vector", async () => {
    const key = keyFromHex(KEY_HEX);
    const ct = fromHex(CT_HEX);
    const pt = await open(key, 1, ct);
    expect(dec(pt)).toBe("rvp-test-vector-1");
  });

  it("seal/open roundtrips", async () => {
    const key = keyFromHex(KEY_HEX);
    const pt = enc("twenty milliseconds of opus");
    const ct = await seal(key, 7, pt);
    expect(ct.length).toBe(pt.length + 16);
    expect(await open(key, 7, ct)).toEqual(pt);
  });

  it("fails to open with the wrong seq", async () => {
    const key = keyFromHex(KEY_HEX);
    const ct = await seal(key, 7, enc("secret"));
    await expect(open(key, 8, ct)).rejects.toThrow(PairwiseCryptoError);
  });

  it("fails to open with the wrong key", async () => {
    const key = keyFromHex(KEY_HEX);
    const other = keyFromHex("0f0e0d0c0b0a09080706050403020100");
    const ct = await seal(key, 7, enc("secret"));
    await expect(open(other, 7, ct)).rejects.toThrow(PairwiseCryptoError);
  });

  it("fails to open a tampered ciphertext", async () => {
    const key = keyFromHex(KEY_HEX);
    const ct = await seal(key, 7, enc("secret"));
    ct[0] ^= 0xff;
    await expect(open(key, 7, ct)).rejects.toThrow(PairwiseCryptoError);
  });

  it("rejects ciphertext shorter than the GCM tag", async () => {
    const key = keyFromHex(KEY_HEX);
    await expect(open(key, 1, new Uint8Array(8))).rejects.toThrow(
      /shorter than GCM tag/,
    );
  });

  it("rejects non-16-byte keys on PairwiseKey", () => {
    expect(() => new PairwiseKey(new Uint8Array(15))).toThrow(
      PairwiseCryptoError,
    );
    expect(() => new PairwiseKey(new Uint8Array(17))).toThrow(
      PairwiseCryptoError,
    );
  });

  it("PairwiseKey wrapper roundtrips", async () => {
    const key = new PairwiseKey(keyFromHex(KEY_HEX));
    const pt = enc("via wrapper");
    const ct = await key.seal(3, pt);
    expect(await key.open(3, ct)).toEqual(pt);
  });

  it("keyFromHex parses 32 hex chars", () => {
    const key = keyFromHex(KEY_HEX);
    expect(key.length).toBe(KEY_LEN);
    expect(key[0]).toBe(0x00);
    expect(key[15]).toBe(0x0f);
  });

  it("keyFromHex rejects bad input", () => {
    expect(() => keyFromHex("abc")).toThrow(PairwiseCryptoError);
    expect(() => keyFromHex("gg".repeat(16))).toThrow(PairwiseCryptoError);
  });
});
