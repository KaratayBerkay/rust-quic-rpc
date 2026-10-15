/**
 * RVP/1 control-stream codec — TypeScript port of `rvp-proto::control`.
 *
 * Wire format (plan §3.1): frame = u32 BE length ‖ u16 BE type ‖ body,
 * where `length` covers type+body. All multi-byte integers are big-endian.
 *
 * The unit tests pin the same hex vectors as the Rust tests
 * (`crates/rvp-proto/src/control.rs`), so both sides cannot drift silently.
 */

export const TYPE_HELLO = 1;
export const TYPE_JOIN_ROOM = 2;
export const TYPE_WELCOME = 3;
export const TYPE_PARTICIPANT_JOINED = 4;
export const TYPE_PARTICIPANT_LEFT = 5;
export const TYPE_KEY_UPDATE = 6;
export const TYPE_ERROR = 15;

export const PROTOCOL_VERSION = 1;
export const MAX_FRAME_LEN = 16 * 1024;

export const ErrorCode = {
  BAD_REQUEST: 400,
  UNAUTHORIZED: 401,
  FORBIDDEN: 403,
  TIMEOUT: 408,
  ROOM_FULL: 429,
  INTERNAL: 500,
  CONNECTION_LIMIT: 503,
  VERSION_UNSUPPORTED: 505,
} as const;

export interface RosterEntry {
  participantId: number;
  userId: string;
}

export type ControlMessage =
  | { type: "hello"; protocolVersion: number; grantJwt: string }
  | { type: "joinRoom" }
  | {
      type: "welcome";
      participantId: number;
      maxDatagram: number;
      roster: RosterEntry[];
    }
  | { type: "participantJoined"; participantId: number; userId: string }
  | { type: "participantLeft"; participantId: number }
  | { type: "keyUpdate"; kid: number }
  | { type: "error"; code: number; message: string };

function writeU16(view: DataView, offset: number, value: number): number {
  view.setUint16(offset, value, false);
  return offset + 2;
}

function writeU32(view: DataView, offset: number, value: number): number {
  view.setUint32(offset, value, false);
  return offset + 4;
}

function writeU64(view: DataView, offset: number, value: number): number {
  view.setBigUint64(offset, BigInt(value), false);
  return offset + 8;
}

function readU16(view: DataView, offset: number): [number, number] {
  return [view.getUint16(offset, false), offset + 2];
}

function readU64(view: DataView, offset: number): [number, number] {
  const big = view.getBigUint64(offset, false);
  return [Number(big), offset + 8];
}

function utf8(value: string): Uint8Array {
  return new TextEncoder().encode(value);
}

function fromUtf8(bytes: Uint8Array): string {
  return new TextDecoder().decode(bytes);
}

/** Encode a complete frame (length prefix included). */
export function encode(msg: ControlMessage): Uint8Array {
  let bodyLen = 2;
  switch (msg.type) {
    case "hello":
      bodyLen += 2 + utf8(msg.grantJwt).length;
      break;
    case "joinRoom":
      break;
    case "welcome":
      bodyLen += 8 + 2 + 2;
      for (const entry of msg.roster) {
        bodyLen += 8 + 2 + utf8(entry.userId).length;
      }
      break;
    case "participantJoined":
      bodyLen += 8 + 2 + utf8(msg.userId).length;
      break;
    case "participantLeft":
      bodyLen += 8;
      break;
    case "keyUpdate":
      bodyLen += 1;
      break;
    case "error":
      bodyLen += 2 + utf8(msg.message).length;
      break;
  }

  const frame = new Uint8Array(4 + bodyLen);
  const view = new DataView(frame.buffer);
  let offset = writeU32(view, 0, bodyLen);

  switch (msg.type) {
    case "hello":
      offset = writeU16(view, offset, TYPE_HELLO);
      offset = writeU16(view, offset, msg.protocolVersion);
      frame.set(utf8(msg.grantJwt), offset);
      break;
    case "joinRoom":
      writeU16(view, offset, TYPE_JOIN_ROOM);
      break;
    case "welcome": {
      offset = writeU16(view, offset, TYPE_WELCOME);
      offset = writeU64(view, offset, msg.participantId);
      offset = writeU16(view, offset, msg.maxDatagram);
      offset = writeU16(view, offset, msg.roster.length);
      for (const entry of msg.roster) {
        offset = writeU64(view, offset, entry.participantId);
        offset = writeU16(view, offset, utf8(entry.userId).length);
        frame.set(utf8(entry.userId), offset);
        offset += utf8(entry.userId).length;
      }
      break;
    }
    case "participantJoined": {
      offset = writeU16(view, offset, TYPE_PARTICIPANT_JOINED);
      offset = writeU64(view, offset, msg.participantId);
      offset = writeU16(view, offset, utf8(msg.userId).length);
      frame.set(utf8(msg.userId), offset);
      break;
    }
    case "participantLeft":
      offset = writeU16(view, offset, TYPE_PARTICIPANT_LEFT);
      writeU64(view, offset, msg.participantId);
      break;
    case "keyUpdate":
      offset = writeU16(view, offset, TYPE_KEY_UPDATE);
      view.setUint8(offset, msg.kid);
      break;
    case "error":
      offset = writeU16(view, offset, TYPE_ERROR);
      offset = writeU16(view, offset, msg.code);
      frame.set(utf8(msg.message), offset);
      break;
  }
  return frame;
}

export class ControlDecodeError extends Error {
  constructor(
    message: string,
    readonly detail?: unknown,
  ) {
    super(message);
    this.name = "ControlDecodeError";
  }
}

/** Decode one frame body (after the u32 length prefix has been consumed). */
export function decodeBody(body: Uint8Array): ControlMessage {
  if (body.length < 2) {
    throw new ControlDecodeError("frame truncated");
  }
  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  const [msgType, typeEnd] = readU16(view, 0);
  const rest = body.subarray(typeEnd);
  const restView = new DataView(rest.buffer, rest.byteOffset, rest.byteLength);

  switch (msgType) {
    case TYPE_HELLO: {
      if (rest.length < 2) {
        throw new ControlDecodeError("bad hello body", msgType);
      }
      const [protocolVersion] = readU16(restView, 0);
      return {
        type: "hello",
        protocolVersion,
        grantJwt: fromUtf8(rest.subarray(2)),
      };
    }
    case TYPE_JOIN_ROOM:
      return { type: "joinRoom" };
    case TYPE_WELCOME: {
      if (rest.length < 12) {
        throw new ControlDecodeError("bad welcome body", msgType);
      }
      let offset = 0;
      const [participantId, o1] = readU64(restView, offset);
      offset = o1;
      const [maxDatagram, o2] = readU16(restView, offset);
      offset = o2;
      const [rosterLen, o3] = readU16(restView, offset);
      offset = o3;
      const roster: RosterEntry[] = [];
      for (let i = 0; i < rosterLen; i++) {
        if (rest.length < offset + 10) {
          throw new ControlDecodeError("bad roster entry", msgType);
        }
        const [pid, o4] = readU64(restView, offset);
        offset = o4;
        const [idLen, o5] = readU16(restView, offset);
        offset = o5;
        if (rest.length < offset + idLen) {
          throw new ControlDecodeError("truncated roster user_id", msgType);
        }
        roster.push({
          participantId: pid,
          userId: fromUtf8(rest.subarray(offset, offset + idLen)),
        });
        offset += idLen;
      }
      return { type: "welcome", participantId, maxDatagram, roster };
    }
    case TYPE_PARTICIPANT_JOINED: {
      if (rest.length < 10) {
        throw new ControlDecodeError("bad joined body", msgType);
      }
      const [participantId] = readU64(restView, 0);
      const [idLen] = readU16(restView, 8);
      if (rest.length < 10 + idLen) {
        throw new ControlDecodeError("truncated joined user_id", msgType);
      }
      return {
        type: "participantJoined",
        participantId,
        userId: fromUtf8(rest.subarray(10, 10 + idLen)),
      };
    }
    case TYPE_PARTICIPANT_LEFT: {
      if (rest.length < 8) {
        throw new ControlDecodeError("bad left body", msgType);
      }
      const [participantId] = readU64(restView, 0);
      return { type: "participantLeft", participantId };
    }
    case TYPE_KEY_UPDATE: {
      if (rest.length < 1) {
        throw new ControlDecodeError("bad keyUpdate body", msgType);
      }
      return { type: "keyUpdate", kid: rest[0] };
    }
    case TYPE_ERROR: {
      if (rest.length < 2) {
        throw new ControlDecodeError("bad error body", msgType);
      }
      const [code] = readU16(restView, 0);
      return { type: "error", code, message: fromUtf8(rest.subarray(2)) };
    }
    default:
      throw new ControlDecodeError(`unknown frame type ${msgType}`, msgType);
  }
}

/**
 * Incremental frame reader for a stream of raw bytes. Feed it every chunk;
 * it yields complete decoded messages.
 */
export class ControlFrameReader {
  private buffer = new Uint8Array(0);

  push(chunk: Uint8Array): void {
    const merged = new Uint8Array(this.buffer.length + chunk.length);
    merged.set(this.buffer, 0);
    merged.set(chunk, this.buffer.length);
    this.buffer = merged;
  }

  /** Drain every complete frame currently buffered. */
  drain(): ControlMessage[] {
    const messages: ControlMessage[] = [];
    while (this.buffer.length >= 4) {
      const view = new DataView(
        this.buffer.buffer,
        this.buffer.byteOffset,
        this.buffer.byteLength,
      );
      const len = view.getUint32(0, false);
      if (len > MAX_FRAME_LEN) {
        throw new ControlDecodeError(`frame too large: ${len}`);
      }
      if (this.buffer.length < 4 + len) {
        break;
      }
      messages.push(decodeBody(this.buffer.subarray(4, 4 + len)));
      this.buffer = this.buffer.subarray(4 + len);
    }
    return messages;
  }
}
