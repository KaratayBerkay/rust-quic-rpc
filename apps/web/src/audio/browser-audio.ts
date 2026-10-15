/**
 * Browser audio I/O: AudioWorklet capture/playback + WebCodecs Opus
 * encode/decode (plan §7, P6b). These factories are injected into
 * `VoicePipeline` so the pure fanout/decrypt logic stays testable in Node.
 */

import {
  CHANNELS,
  FRAME_SAMPLES,
  SAMPLE_RATE,
  float32ToInt16,
  int16ToFloat32,
  type OpusDecoder,
  type OpusEncoder,
} from "../lib/voice";

function toArrayBuffer(view: Uint8Array | Float32Array): ArrayBuffer {
  return view.buffer.slice(
    view.byteOffset,
    view.byteOffset + view.byteLength,
  ) as ArrayBuffer;
}

/** WebCodecs Opus encoder (Chrome/Edge 94+). */
export function createWebCodecsOpusEncoder(): OpusEncoder {
  let encoder: AudioEncoder | null = null;
  const outputChunks: EncodedAudioChunk[] = [];
  let error: Error | null = null;

  encoder = new AudioEncoder({
    output: (chunk) => {
      outputChunks.push(chunk);
    },
    error: (e) => {
      error = e instanceof Error ? e : new Error(String(e));
    },
  });
  encoder.configure({
    codec: "opus",
    sampleRate: SAMPLE_RATE,
    numberOfChannels: CHANNELS,
    bitrate: 32_000,
  });

  let timestampUs = 0;

  return {
    encodeFrame(pcm: Int16Array): Uint8Array {
      if (error) throw error;
      outputChunks.length = 0;
      const f32 = int16ToFloat32(pcm);
      const data = new AudioData({
        format: "f32",
        sampleRate: SAMPLE_RATE,
        numberOfFrames: FRAME_SAMPLES,
        numberOfChannels: CHANNELS,
        timestamp: timestampUs,
        data: toArrayBuffer(f32),
      });
      encoder!.encode(data);
      data.close();
      timestampUs += (FRAME_SAMPLES / SAMPLE_RATE) * 1_000_000;
      // The encoder may buffer internally; flush is not needed per-frame
      // for streaming Opus, but we drain whatever is ready.
      if (outputChunks.length === 0) {
        return new Uint8Array(0);
      }
      // Concatenate any chunks produced for this frame.
      let total = 0;
      for (const c of outputChunks) total += c.byteLength;
      const out = new Uint8Array(total);
      let offset = 0;
      for (const c of outputChunks) {
        const buf = new Uint8Array(c.byteLength);
        c.copyTo(buf);
        out.set(buf, offset);
        offset += c.byteLength;
      }
      return out;
    },
    close() {
      encoder?.close();
      encoder = null;
    },
  };
}

/** WebCodecs Opus decoder with a silence fallback for PLC. */
export function createWebCodecsOpusDecoder(): OpusDecoder {
  let decoder: AudioDecoder | null = null;
  const pending: Int16Array[] = [];
  let error: Error | null = null;

  decoder = new AudioDecoder({
    output: (audioData) => {
      const f32 = new Float32Array(audioData.numberOfFrames * audioData.numberOfChannels);
      audioData.copyTo(f32, { planeIndex: 0, format: "f32" });
      // Mono downmix if the decoder ever emits multi-channel.
      if (audioData.numberOfChannels === 1) {
        pending.push(float32ToInt16(f32));
      } else {
        const mono = new Float32Array(audioData.numberOfFrames);
        for (let i = 0; i < audioData.numberOfFrames; i += 1) {
          let sum = 0;
          for (let c = 0; c < audioData.numberOfChannels; c += 1) {
            sum += f32[i * audioData.numberOfChannels + c];
          }
          mono[i] = sum / audioData.numberOfChannels;
        }
        pending.push(float32ToInt16(mono));
      }
      audioData.close();
    },
    error: (e) => {
      error = e instanceof Error ? e : new Error(String(e));
    },
  });
  decoder.configure({
    codec: "opus",
    sampleRate: SAMPLE_RATE,
    numberOfChannels: CHANNELS,
  });

  let timestampUs = 0;

  return {
    decodePacket(packet: Uint8Array): Int16Array {
      if (error) throw error;
      pending.length = 0;
      const chunk = new EncodedAudioChunk({
        type: "key",
        timestamp: timestampUs,
        data: toArrayBuffer(packet),
      });
      decoder!.decode(chunk);
      timestampUs += (FRAME_SAMPLES / SAMPLE_RATE) * 1_000_000;
      if (pending.length > 0) return pending[0];
      // Decoder may still be draining; return silence rather than block audio.
      return new Int16Array(FRAME_SAMPLES);
    },
    conceal(): Int16Array {
      // WebCodecs does not expose libopus PLC; emit silence for one frame.
      return new Int16Array(FRAME_SAMPLES);
    },
    close() {
      decoder?.close();
      decoder = null;
    },
  };
}

export interface CaptureHandle {
  /** Stop mic capture and disconnect the worklet. */
  stop(): void;
}

export interface PlaybackHandle {
  /** Enqueue one 960-sample Int16 frame for playout. */
  enqueue(frame: Int16Array): void;
  stop(): void;
}

/**
 * Load the shared worklet module (capture + playback processors) into the
 * given AudioContext. Safe to call multiple times.
 */
export async function loadWorklets(context: AudioContext): Promise<void> {
  const url = new URL("./worklet-processor.ts", import.meta.url);
  await context.audioWorklet.addModule(url);
}

/**
 * Start mic capture at 48 kHz mono. Calls `onFrame` with each exact
 * 960-sample Int16 frame (20 ms).
 */
export async function startCapture(
  context: AudioContext,
  onFrame: (frame: Int16Array) => void,
): Promise<CaptureHandle> {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: CHANNELS,
      sampleRate: SAMPLE_RATE,
      echoCancellation: true,
      noiseSuppression: true,
    },
  });
  const source = context.createMediaStreamSource(stream);
  const node = new AudioWorkletNode(context, "rvp-capture", {
    numberOfInputs: 1,
    numberOfOutputs: 1,
    channelCount: CHANNELS,
  });
  node.port.onmessage = (event: MessageEvent) => {
    onFrame(event.data as Int16Array);
  };
  source.connect(node);
  // Keep the graph alive without routing mic audio to speakers (feedback).
  node.connect(context.destination);

  return {
    stop() {
      node.port.onmessage = null;
      node.disconnect();
      source.disconnect();
      for (const track of stream.getTracks()) track.stop();
    },
  };
}

/** Start speaker playback at 48 kHz mono. */
export function startPlayback(context: AudioContext): PlaybackHandle {
  const node = new AudioWorkletNode(context, "rvp-playback", {
    numberOfInputs: 1,
    numberOfOutputs: 1,
    channelCount: CHANNELS,
  });
  node.connect(context.destination);

  return {
    enqueue(frame: Int16Array) {
      const f32 = int16ToFloat32(frame);
      node.port.postMessage(f32, [f32.buffer]);
    },
    stop() {
      node.disconnect();
    },
  };
}
