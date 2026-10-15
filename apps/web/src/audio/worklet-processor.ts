/**
 * AudioWorklet processor for 48 kHz mono capture and playback (plan §7).
 *
 * Loaded via `audioContext.audioWorklet.addModule()` — runs on the audio
 * render thread. Capture accumulates the 128-sample render quanta into exact
 * 960-sample (20 ms) frames and posts them to the main thread. Playback
 * receives 960-sample Float32 frames over the port and plays them, padding
 * with silence on underrun.
 */

const FRAME_SAMPLES = 960;

class RvpCaptureProcessor extends AudioWorkletProcessor {
  private buffer: Float32Array;
  private buffered = 0;

  constructor() {
    super();
    this.buffer = new Float32Array(FRAME_SAMPLES);
  }

  process(inputs: Float32Array[][]): boolean {
    const input = inputs[0];
    if (!input || input.length === 0 || !input[0]) return true;
    const channel = input[0];
    let offset = 0;
    while (offset < channel.length) {
      const take = Math.min(FRAME_SAMPLES - this.buffered, channel.length - offset);
      this.buffer.set(channel.subarray(offset, offset + take), this.buffered);
      this.buffered += take;
      offset += take;
      if (this.buffered === FRAME_SAMPLES) {
        const frame = this.buffer.slice();
        this.port.postMessage(frame, [frame.buffer]);
        this.buffered = 0;
      }
    }
    return true;
  }
}

class RvpPlaybackProcessor extends AudioWorkletProcessor {
  private queue: Float32Array[] = [];
  private readOffset = 0;

  constructor() {
    super();
    this.port.onmessage = (event: MessageEvent) => {
      const frame = event.data as Float32Array;
      // Bound the queue so a stalled decoder cannot grow unbounded.
      if (this.queue.length < 16) {
        this.queue.push(frame);
      }
    };
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const output = outputs[0];
    if (!output || output.length === 0 || !output[0]) return true;
    const channel = output[0];
    for (let i = 0; i < channel.length; i += 1) {
      if (this.queue.length === 0) {
        channel[i] = 0;
      } else {
        const current = this.queue[0];
        channel[i] = current[this.readOffset] ?? 0;
        this.readOffset += 1;
        if (this.readOffset >= current.length) {
          this.queue.shift();
          this.readOffset = 0;
        }
      }
    }
    return true;
  }
}

registerProcessor("rvp-capture", RvpCaptureProcessor);
registerProcessor("rvp-playback", RvpPlaybackProcessor);
