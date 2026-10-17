/** Redis key/channel constants (P8b) — mirrors `crates/rvp-directory`. */

/** Prefix for per-node directory keys (`rvp:node:<id>`). Each key holds the
 * node's JSON entry with its own TTL, so one node's heartbeat never extends
 * another's liveness (a single shared hash has no per-field TTL). */
export const NODE_KEY_PREFIX = 'rvp:node:';

/** Glob matching every live node key (for SCAN in the reader). */
export const NODE_KEY_GLOB = 'rvp:node:*';

/** Channel carrying revocation events (`{"jtis":[...]}`, one publish per kick). */
export const REVOCATION_CHANNEL = 'rvp:revocations';

/** Per-node registration TTL in seconds. */
export const NODE_TTL_SECS = 30;

/** Build the directory key for one node id. */
export function nodeKey(id: string): string {
  return `${NODE_KEY_PREFIX}${id}`;
}

/** One media node the control plane may pin a room to (P8a, plan §2/§5). */
export interface MediaNode {
  id: string;
  /** `host:port` of the media server (QUIC/UDP). Used by clients to connect. */
  address: string;
}
