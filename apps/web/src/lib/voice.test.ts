import { describe, expect, it } from "vitest";
import { PairwiseKey, keyFromHex } from "./crypto";
import { FRAME_SAMPLES } from "./jitter";
import { PT_OPUS, PT_OPUS_ENC, parseDatagram } from "./media";
import { VoicePipeline, float32ToInt16, int16ToFloat32 } from "./voice";
import { defaultHeader, buildDatagram } from "./media";

const KEY_A = keyFromHex("000102030405060708090a0b0c0d0e0f");
const KEY_B = keyFromHex("0f0e0d0c0b0a09080706050403020100");

function silence(): Int16Array {
  return new Int16Array(FRAME_SAMPLES);
}

function fakeEncoder() {
  return {
    encodeFrame: (pcm: Int16Array) =>
      new Uint8Array(pcm.buffer.slice(0), pcm.byteOffset, pcm.byteLength),
    close: () => {},
  };
}

function fakeDecoder() {
  return {
    decodePacket: (packet: Uint8Array) => new Int16Array(packet.buffer.slice(0)),
    conceal: () => new Int16Array(FRAME_SAMPLES),
    close: () => {},
  };
}

function makePipeline(opts: {
  peers?: Array<{ userId: string; kid: number; key: PairwiseKey }>;
  plaintext?: boolean;
  sent?: Uint8Array[];
} = {}) {
  const sent = opts.sent ?? [];
  const pipeline = new VoicePipeline({
    sendDatagram: (d) => sent.push(d),
    createEncoder: fakeEncoder,
    createDecoder: fakeDecoder,
    plaintext: opts.plaintext,
  });
  for (const p of opts.peers ?? []) pipeline.addPeer(p);
  return { pipeline, sent };
}

describe("VoicePipeline send", () => {
  it("rejects frames that are not 960 samples", async () => {
    const { pipeline } = makePipeline();
    await expect(pushN(pipeline, 100)).rejects.toThrow(/expected 960/);
  });

  it("plaintext mode sends one PT=0 datagram with kid 0", async () => {
    const { pipeline, sent } = makePipeline({ plaintext: true });
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(1);
    const { header } = parseDatagram(sent[0]);
    expect(header.pt).toBe(PT_OPUS);
    expect(header.kid).toBe(0);
    expect(header.sequence).toBe(0);
    expect(header.timestamp).toBe(0);
    expect(pipeline.stats.framesSent).toBe(1);
    expect(pipeline.stats.datagramsSent).toBe(1);
  });

  it("sends N−1 sealed datagrams per frame, one per peer", async () => {
    const { pipeline, sent } = makePipeline({
      peers: [
        { userId: "bob", kid: 1, key: new PairwiseKey(KEY_A) },
        { userId: "carol", kid: 2, key: new PairwiseKey(KEY_B) },
      ],
    });
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(2);
    const kids = sent.map((d) => parseDatagram(d).header.kid).sort();
    expect(kids).toEqual([1, 2]);
    for (const d of sent) {
      expect(parseDatagram(d).header.pt).toBe(PT_OPUS_ENC);
      expect(parseDatagram(d).header.sequence).toBe(0);
    }
    expect(pipeline.stats.framesSent).toBe(1);
    expect(pipeline.stats.datagramsSent).toBe(2);
  });

  it("increments sequence and timestamp across frames", async () => {
    const { pipeline, sent } = makePipeline({
      peers: [{ userId: "bob", kid: 1, key: new PairwiseKey(KEY_A) }],
    });
    await pipeline.pushPcm(silence());
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(2);
    expect(parseDatagram(sent[0]).header.sequence).toBe(0);
    expect(parseDatagram(sent[1]).header.sequence).toBe(1);
    expect(parseDatagram(sent[0]).header.timestamp).toBe(0);
    expect(parseDatagram(sent[1]).header.timestamp).toBe(FRAME_SAMPLES);
  });

  it("preserves send order across async seal (serialized chain)", async () => {
    const { pipeline, sent } = makePipeline({
      peers: [{ userId: "bob", kid: 1, key: new PairwiseKey(KEY_A) }],
    });
    const a = pipeline.pushPcm(silence());
    const b = pipeline.pushPcm(silence());
    const c = pipeline.pushPcm(silence());
    await Promise.all([a, b, c]);
    const seqs = sent.map((d) => parseDatagram(d).header.sequence);
    expect(seqs).toEqual([0, 1, 2]);
  });

  it("muted pipeline sends nothing and consumes no sequence", async () => {
    const { pipeline, sent } = makePipeline({
      peers: [{ userId: "bob", kid: 1, key: new PairwiseKey(KEY_A) }],
    });
    pipeline.setMuted(true);
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(0);
    pipeline.setMuted(false);
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(1);
    expect(parseDatagram(sent[0]).header.sequence).toBe(0);
  });

  it("removePeer drops that recipient from fanout", async () => {
    const { pipeline, sent } = makePipeline({
      peers: [
        { userId: "bob", kid: 1, key: new PairwiseKey(KEY_A) },
        { userId: "carol", kid: 2, key: new PairwiseKey(KEY_B) },
      ],
    });
    pipeline.removePeer("carol");
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(1);
    expect(parseDatagram(sent[0]).header.kid).toBe(1);
    expect(pipeline.peerCount()).toBe(1);
  });
});

describe("VoicePipeline receive", () => {
  function sealedDatagram(
    key: PairwiseKey,
    kid: number,
    seq: number,
    plaintext: Uint8Array,
  ): Promise<Uint8Array> {
    return key.seal(seq, plaintext).then((sealed) =>
      buildDatagram(
        {
          ...defaultHeader(),
          pt: PT_OPUS_ENC,
          kid,
          sequence: seq,
          timestamp: seq * FRAME_SAMPLES,
        },
        sealed,
      ),
    );
  }

  it("opens a sealed datagram with the matching kid and plays it", async () => {
    const { pipeline } = makePipeline({
      peers: [{ userId: "bob", kid: 1, key: new PairwiseKey(KEY_A) }],
    });
    const opus = new Uint8Array([1, 2, 3, 4]);
    // Two frames to clear the 40 ms prebuffer.
    pipeline.receiveDatagram(
      await sealedDatagram(new PairwiseKey(KEY_A), 1, 0, opus),
    );
    pipeline.receiveDatagram(
      await sealedDatagram(new PairwiseKey(KEY_A), 1, 1, opus),
    );
    await pipeline.flush();
    const result = pipeline.tick();
    expect(result.kind).toBe("packet");
    expect(pipeline.stats.decryptFailures).toBe(0);
    expect(pipeline.stats.missingKeyDropped).toBe(0);
  });

  it("drops datagrams with an unknown kid", async () => {
    const { pipeline } = makePipeline();
    const dg = await sealedDatagram(new PairwiseKey(KEY_A), 9, 0, new Uint8Array(4));
    pipeline.receiveDatagram(dg);
    await pipeline.flush();
    expect(pipeline.stats.missingKeyDropped).toBe(1);
  });

  it("drops datagrams that fail to decrypt (wrong key)", async () => {
    const { pipeline } = makePipeline({
      peers: [{ userId: "bob", kid: 1, key: new PairwiseKey(KEY_B) }],
    });
    const dg = await sealedDatagram(new PairwiseKey(KEY_A), 1, 0, new Uint8Array(4));
    pipeline.receiveDatagram(dg);
    await pipeline.flush();
    expect(pipeline.stats.decryptFailures).toBe(1);
  });

  it("drops malformed datagrams", async () => {
    const { pipeline } = makePipeline();
    pipeline.receiveDatagram(new Uint8Array(4));
    await pipeline.flush();
    expect(pipeline.stats.malformedDropped).toBe(1);
  });

  it("accepts plaintext PT=0 datagrams without a peer key", async () => {
    const { pipeline } = makePipeline();
    const payload = new Uint8Array([9, 9]);
    pipeline.receiveDatagram(
      buildDatagram({ ...defaultHeader(), pt: PT_OPUS, kid: 0, sequence: 0 }, payload),
    );
    pipeline.receiveDatagram(
      buildDatagram({ ...defaultHeader(), pt: PT_OPUS, kid: 0, sequence: 1 }, payload),
    );
    await pipeline.flush();
    const result = pipeline.tick();
    expect(result.kind).toBe("packet");
  });
});

describe("pcm converters", () => {
  it("int16 ↔ float32 roundtrips within quantization", () => {
    const src = new Int16Array([0, 16384, -16384, 32767, -32768]);
    const back = float32ToInt16(int16ToFloat32(src));
    for (let i = 0; i < src.length; i += 1) {
      expect(Math.abs(back[i] - src[i])).toBeLessThanOrEqual(1);
    }
  });

  it("clamps float32 outside [-1, 1]", () => {
    const out = float32ToInt16(new Float32Array([2, -2, 0]));
    expect(out[0]).toBe(32767);
    expect(out[1]).toBe(-32768);
    expect(out[2]).toBe(0);
  });
});

describe("VoicePipeline key rotation (plan §5)", () => {
  const KEY_ROTATED = keyFromHex("101112131415161718191a1b1c1d1e1f");

  it("applyPairs installs Nest pairs and seals with the announced kid", async () => {
    const { pipeline, sent } = makePipeline();
    pipeline.applyPairs([{ peer: "bob", kid: 1, key: "000102030405060708090a0b0c0d0e0f" }]);
    await pipeline.pushPcm(silence());
    expect(sent.length).toBe(1);
    expect(parseDatagram(sent[0]).header.kid).toBe(1);
    expect(pipeline.peerCount()).toBe(1);
  });

  it("rotation swaps the send kid but keeps the old kid receivable", async () => {
    const { pipeline, sent } = makePipeline();
    pipeline.applyPairs([{ peer: "bob", kid: 1, key: "000102030405060708090a0b0c0d0e0f" }]);

    // Rotate: same peer, new kid + new key (Nest rotate response shape).
    pipeline.applyPairs([{ peer: "bob", kid: 2, key: "101112131415161718191a1b1c1d1e1f" }]);

    // Outbound now uses the new kid.
    await pipeline.pushPcm(silence());
    expect(parseDatagram(sent[0]).header.kid).toBe(2);

    // In-flight datagrams sealed with the OLD key still open (grace).
    const opus = new Uint8Array([7, 7, 7, 7]);
    pipeline.receiveDatagram(
      await new PairwiseKey(KEY_A)
        .seal(0, opus)
        .then((sealed) =>
          buildDatagram(
            { ...defaultHeader(), pt: PT_OPUS_ENC, kid: 1, sequence: 0, timestamp: 0 },
            sealed,
          ),
        ),
    );
    // And the NEW key opens with the new kid.
    pipeline.receiveDatagram(
      await new PairwiseKey(KEY_ROTATED)
        .seal(1, opus)
        .then((sealed) =>
          buildDatagram(
            { ...defaultHeader(), pt: PT_OPUS_ENC, kid: 2, sequence: 1, timestamp: FRAME_SAMPLES },
            sealed,
          ),
        ),
    );
    await pipeline.flush();
    expect(pipeline.stats.decryptFailures).toBe(0);
    expect(pipeline.stats.missingKeyDropped).toBe(0);
    expect(pipeline.tick().kind).toBe("packet");
  });
});

async function pushN(pipeline: VoicePipeline, samples: number): Promise<void> {
  await pipeline.pushPcm(new Int16Array(samples));
}
