/**
 * Env-driven configuration (plan §5).
 *
 * DATABASE_URL  — Postgres connection string (required).
 * PORT          — HTTP listen port (default 3000).
 * NEST_EDDSA_SEED — optional 32-byte hex seed for the Ed25519 signing key;
 *                   without it an ephemeral key is generated at boot (keys
 *                   rotate on restart — fine for dev, set the seed in prod).
 * GRANT_TTL_SECONDS / SESSION_TTL_SECONDS — JWT lifetimes (120 / 86400).
 */
export interface AppConfig {
  databaseUrl: string;
  port: number;
  eddsaSeedHex: string | null;
  grantTtlSeconds: number;
  sessionTtlSeconds: number;
}

export function loadConfig(env: NodeJS.ProcessEnv = process.env): AppConfig {
  const databaseUrl = env.DATABASE_URL;
  if (!databaseUrl) {
    throw new Error('DATABASE_URL is required');
  }
  return {
    databaseUrl,
    port: Number(env.PORT ?? 3000),
    eddsaSeedHex: env.NEST_EDDSA_SEED ?? null,
    grantTtlSeconds: Number(env.GRANT_TTL_SECONDS ?? 120),
    sessionTtlSeconds: Number(env.SESSION_TTL_SECONDS ?? 86400),
  };
}
