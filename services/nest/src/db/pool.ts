import { Pool } from 'pg';
import { AppConfig } from '../config';

export function createPool(config: AppConfig): Pool {
  return new Pool({ connectionString: config.databaseUrl, max: 10 });
}

/**
 * Idempotent bootstrap DDL (plan §5: rooms + membership + pairwise keys).
 *
 * kid is per-room, dense from 1, bounded to 255 (media-header byte3, P4b).
 * Keys are AES-128 (16 bytes); ordered (user_a < user_b) so each pair has
 * exactly one row. A room exceeding 255 live pairs fails loudly — acceptable
 * for the v1 control plane, revisit before production.
 */
export const MIGRATION_SQL = `
CREATE TABLE IF NOT EXISTS rooms (
  id          uuid PRIMARY KEY,
  name        text NOT NULL,
  created_by  text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  -- P8a: media-node pin (plan §2/§5). NULL = unpinned (single-node).
  media_node  text
);

-- Existing rows (pre-P8a) stay NULL; new rooms get pinned round-robin when
-- a media-node directory is configured (MEDIA_NODES / RVP_MEDIA_NODES_URL).
ALTER TABLE rooms ADD COLUMN IF NOT EXISTS media_node text;

CREATE TABLE IF NOT EXISTS room_members (
  room_id  uuid NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
  user_id  text NOT NULL,
  role     text NOT NULL CHECK (role IN ('host', 'member')),
  joined_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (room_id, user_id)
);

CREATE TABLE IF NOT EXISTS pair_keys (
  room_id   uuid NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
  user_a    text NOT NULL,
  user_b    text NOT NULL,
  kid       int NOT NULL CHECK (kid BETWEEN 1 AND 255),
  key_bytes bytea NOT NULL CHECK (octet_length(key_bytes) = 16),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (room_id, kid),
  UNIQUE (room_id, user_a, user_b),
  CHECK (user_a < user_b)
);

-- P7c revocation: every issued media grant's jti, so a host kick can mark
-- the live ones revoked. rvp-cli polls GET /auth/revoked (unexpired revoked
-- jtis); the media server denies them at Join and kicks live sessions.
CREATE TABLE IF NOT EXISTS grant_issues (
  jti        text PRIMARY KEY,
  room_id    uuid NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
  user_id    text NOT NULL,
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz
);
CREATE INDEX IF NOT EXISTS grant_issues_room_user_idx ON grant_issues (room_id, user_id);
`;

export async function migrate(pool: Pool): Promise<void> {
  await pool.query(MIGRATION_SQL);
}
