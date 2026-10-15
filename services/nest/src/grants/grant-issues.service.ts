import { Injectable } from '@nestjs/common';
import { Pool } from 'pg';

/**
 * Media-grant issue ledger (P7c revocation, plan §6).
 *
 * Every issued media grant's jti is recorded with its room, user and
 * expiry. A host kick marks the unexpired ones revoked
 * (see `RoomsService.kickMember`); rvp-cli polls `GET /auth/revoked` and the
 * media server denies those jtis at Join and closes sessions already
 * holding them (decision 5: in-memory denylist, no Redis for v1).
 */
@Injectable()
export class GrantIssuesService {
  constructor(private readonly pool: Pool) {}

  /** Record one freshly issued grant, before the token is handed out. */
  async record(args: {
    jti: string;
    roomId: string;
    userId: string;
    /** Grant expiry, epoch seconds (matches the JWT `exp`). */
    expiresAt: number;
  }): Promise<void> {
    await this.pool.query(
      `INSERT INTO grant_issues (jti, room_id, user_id, expires_at)
       VALUES ($1, $2, $3, to_timestamp($4))
       ON CONFLICT (jti) DO NOTHING`,
      [args.jti, args.roomId, args.userId, args.expiresAt],
    );
  }

  /**
   * Unexpired revoked jtis for `GET /auth/revoked` (rvp-cli `--nest-revoked`).
   * Expired entries drop out of the list automatically — they can no longer
   * pass the `exp` check, so the media server's in-memory copy lingering is
   * harmless.
   */
  async revokedJtis(): Promise<string[]> {
    const res = await this.pool.query<{ jti: string }>(
      `SELECT jti FROM grant_issues
       WHERE revoked_at IS NOT NULL AND expires_at > now()
       ORDER BY revoked_at DESC`,
    );
    return res.rows.map((row) => row.jti);
  }
}
