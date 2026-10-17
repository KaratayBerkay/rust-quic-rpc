/**
 * Env-driven configuration (plan §5).
 *
 * DATABASE_URL  — Postgres connection string (required).
 * PORT          — HTTP listen port (default 3000).
 * NEST_EDDSA_SEED — optional 32-byte hex seed for the Ed25519 signing key;
 *                   without it an ephemeral key is generated at boot (keys
 *                   rotate on restart — fine for dev, set the seed in prod).
 * GRANT_TTL_SECONDS / SESSION_TTL_SECONDS — JWT lifetimes (120 / 86400).
 * MEDIA_NODES   — optional comma-separated list of media-node ids the media
 *                 plane may pin a room to (P8a). Empty = no pinning (single
 *                 node). Rooms are pinned round-robin; the id lands in the
 *                 grant's `media_node` claim and in `rooms.media_node`.
 * RVP_MEDIA_NODES_URL — optional `http://host:port/nodes` JSON endpoint
 *                 (`{"nodes":[{"id":"node-1","address":"host:4433"}]}`)
 *                 polled every 15s for the same purpose (P8a directory).
 * REDIS_URL       — optional `redis://host:port` (P8b multi-instance). When
 *                 set, the media-node directory lives in Redis (media nodes
 *                 self-register with a TTL) and revocation kicks are published
 *                 on `rvp:revocations` so every media node sees them. Empty
 *                 = single-node: static MEDIA_NODES / RVP_MEDIA_NODES_URL and
 *                 the 30 s HTTP revocation poll keep working unchanged.
 */
export interface AppConfig {
  databaseUrl: string;
  port: number;
  eddsaSeedHex: string | null;
  grantTtlSeconds: number;
  sessionTtlSeconds: number;
  mediaNodes: string[];
  mediaNodesUrl: string | null;
  redisUrl: string | null;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): AppConfig {
  const databaseUrl = env.DATABASE_URL;
  if (!databaseUrl) {
    throw new Error('DATABASE_URL is required');
  }
  const mediaNodes = (env.MEDIA_NODES ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  return {
    databaseUrl,
    port: Number(env.PORT ?? 3000),
    eddsaSeedHex: env.NEST_EDDSA_SEED ?? null,
    grantTtlSeconds: Number(env.GRANT_TTL_SECONDS ?? 120),
    sessionTtlSeconds: Number(env.SESSION_TTL_SECONDS ?? 86400),
    mediaNodes,
    mediaNodesUrl: env.RVP_MEDIA_NODES_URL ?? null,
    redisUrl: env.REDIS_URL ?? null,
  };
}
