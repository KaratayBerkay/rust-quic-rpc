/**
 * Ambient types for AudioWorkletProcessor / registerProcessor — these live on
 * the audio render thread and are not in the DOM lib. The worklet file is
 * loaded at runtime via `audioWorklet.addModule()`.
 */

declare class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor();
  process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean;
}

declare function registerProcessor(
  name: string,
  processorCtor: new () => AudioWorkletProcessor,
): void;
