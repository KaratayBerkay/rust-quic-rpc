import {
  BadRequestException,
  ConflictException,
  ForbiddenException,
  Injectable,
  Logger,
  NotFoundException,
} from '@nestjs/common';
import { Pool } from 'pg';
import { RedisService } from '../redis/redis.service';
import { REVOCATION_CHANNEL } from '../redis/keys';

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const USER_RE = /^[A-Za-z0-9_.-]{1,128}$/;

/**
 * Revocation publisher (P8b).
 *
 * P7c kicked a user by marking `grant_issues.revoked_at`; each media server
 * then polled `GET /auth/revoked` every 30 s. In a multi-instance setup that
 * latency (and the single control-plane HTTP endpoint) is too slow, so on a
 * kick we also publish the revoked jtis on the Redis `rvp:revocations`
 * channel. Every media server subscribed applies them immediately, closing
 * the live sessions wherever they live — not just on one node.
 *
 * Postgres stays the source of truth (the publish is a best-effort fast
 * path; the HTTP poll remains the reliable fallback). No Redis → no publish,
 * unchanged single-node behaviour.
 */
@Injectable()
export class RevocationService {
  private readonly logger = new Logger(RevocationService.name);

  constructor(
    private readonly pool: Pool,
    private readonly redis: RedisService,
  ) {}

  /**
   * Kick a member (P7c moderation, plan §6): remove membership, revoke their
   * unexpired media grants for the room, and (P8b) publish the revoked jtis
   * so every media node closes their live session immediately.
   */
  async kickMember(
    roomId: string,
    requesterId: string,
    userId: string,
  ): Promise<{ userId: string; revokedGrants: number }> {
    if (!UUID_RE.test(roomId)) {
      throw new NotFoundException('room not found');
    }
    if (!USER_RE.test(userId)) {
      throw new ConflictException('invalid userId');
    }
    if (userId === requesterId) {
      throw new BadRequestException('cannot kick yourself');
    }
    const client = await this.pool.connect();
    let revokedJtis: string[] = [];
    try {
      await client.query('BEGIN');
      const requester = await client.query<{ role: 'host' | 'member' }>(
        'SELECT role FROM room_members WHERE room_id = $1 AND user_id = $2',
        [roomId, requesterId],
      );
      if (!requester.rows[0]) {
        throw new ForbiddenException('not a member of this room');
      }
      if (requester.rows[0].role !== 'host') {
        throw new ForbiddenException('only a host can kick');
      }
      const target = await client.query(
        'SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2',
        [roomId, userId],
      );
      if (!target.rows[0]) {
        throw new NotFoundException('user is not a member of this room');
      }
      const revoked = await client.query<{ jti: string }>(
        `UPDATE grant_issues SET revoked_at = now()
         WHERE room_id = $1 AND user_id = $2
           AND revoked_at IS NULL AND expires_at > now()
         RETURNING jti`,
        [roomId, userId],
      );
      revokedJtis = revoked.rows.map((r) => r.jti);
      await client.query(
        'DELETE FROM room_members WHERE room_id = $1 AND user_id = $2',
        [roomId, userId],
      );
      await client.query('COMMIT');
    } catch (err) {
      await client.query('ROLLBACK');
      throw err;
    } finally {
      client.release();
    }

    // P8b fast path: tell every media node now (best-effort; the HTTP poll
    // stays as the reliable fallback if Redis is down).
    if (revokedJtis.length > 0) {
      await this.publishRevocations(revokedJtis);
    }
    return { userId, revokedGrants: revokedJtis.length };
  }

  /** Publish revoked jtis on the Redis channel (no-op without Redis). */
  private async publishRevocations(jtis: string[]): Promise<void> {
    const client = this.redis.get();
    if (!client) {
      return;
    }
    try {
      await client.publish(REVOCATION_CHANNEL, JSON.stringify({ jtis }));
    } catch (err) {
      this.logger.warn(`revocation publish failed (HTTP poll still active): ${err}`);
    }
  }
}
