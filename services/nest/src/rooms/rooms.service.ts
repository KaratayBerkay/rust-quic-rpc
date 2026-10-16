import {
  BadRequestException,
  ConflictException,
  ForbiddenException,
  Injectable,
  NotFoundException,
} from '@nestjs/common';
import { randomBytes, randomUUID } from 'node:crypto';
import { Pool } from 'pg';

export interface Room {
  id: string;
  name: string;
  createdBy: string;
  createdAt: Date;
}

export interface Member {
  userId: string;
  role: 'host' | 'member';
  joinedAt: Date;
}

export interface PairKey {
  userA: string;
  userB: string;
  kid: number;
  keyBytes: Buffer;
}

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const USER_RE = /^[A-Za-z0-9_.-]{1,128}$/;

/**
 * Rooms + membership + pairwise-key storage (plan §5).
 *
 * Pairwise keys are ordered (userA < userB), one row per pair, kid dense
 * per-room from 1 (media-header byte3, P4b). Keys are created on demand
 * when a grant is issued for a new pair and never leave the two members.
 */
@Injectable()
export class RoomsService {
  constructor(private readonly pool: Pool) {}

  async createRoom(ownerId: string, name: string): Promise<Room> {
    if (!name || name.length > 200) {
      throw new ConflictException('name (1–200 chars) required');
    }
    const id = randomUUID();
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      const room = await client.query<RoomRow>(
        'INSERT INTO rooms (id, name, created_by) VALUES ($1, $2, $3) RETURNING *',
        [id, name, ownerId],
      );
      await client.query(
        'INSERT INTO room_members (room_id, user_id, role) VALUES ($1, $2, $3)',
        [id, ownerId, 'host'],
      );
      await client.query('COMMIT');
      return toRoom(room.rows[0]);
    } catch (err) {
      await client.query('ROLLBACK');
      throw err;
    } finally {
      client.release();
    }
  }

  async getRoom(roomId: string, requesterId: string): Promise<Room & { members: Member[] }> {
    if (!UUID_RE.test(roomId)) {
      throw new NotFoundException('room not found');
    }
    const room = await this.pool.query<RoomRow>('SELECT * FROM rooms WHERE id = $1', [roomId]);
    const row = room.rows[0];
    if (!row) {
      throw new NotFoundException('room not found');
    }
    await this.requireMember(roomId, requesterId);
    const members = await this.pool.query<MemberRow>(
      'SELECT user_id, role, joined_at FROM room_members WHERE room_id = $1 ORDER BY joined_at',
      [roomId],
    );
    return { ...toRoom(row), members: members.rows.map(toMember) };
  }

  async addMember(
    roomId: string,
    requesterId: string,
    userId: string,
    role: 'host' | 'member' = 'member',
  ): Promise<Member> {
    if (!UUID_RE.test(roomId)) {
      throw new NotFoundException('room not found');
    }
    if (!USER_RE.test(userId)) {
      throw new ConflictException('invalid userId');
    }
    await this.requireMember(roomId, requesterId);
    const res = await this.pool.query<MemberRow>(
      `INSERT INTO room_members (room_id, user_id, role) VALUES ($1, $2, $3)
       ON CONFLICT (room_id, user_id) DO NOTHING
       RETURNING user_id, role, joined_at`,
      [roomId, userId, role],
    );
    if (res.rowCount === 0) {
      throw new ConflictException('already a member');
    }
    return toMember(res.rows[0]);
  }

  async listMembers(roomId: string): Promise<Member[]> {
    const members = await this.pool.query<MemberRow>(
      'SELECT user_id, role, joined_at FROM room_members WHERE room_id = $1 ORDER BY joined_at',
      [roomId],
    );
    return members.rows.map(toMember);
  }

  /**
   * Kick a member (P7c moderation, plan §6): remove their membership and
   * revoke their unexpired media grants for this room in one transaction.
   * The next `rvp-cli --nest-revoked` poll then closes their live media
   * session and blocks rejoin until a host re-adds them (a fresh grant is
   * recorded unrevoked, so re-adding fully restores access).
   *
   * Only a host may kick, and not themselves. A grant issued in the same
   * instant as the kick can slip past the revoke (membership is checked
   * before its jti insert); the short grant TTL bounds that window.
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
      const revoked = await client.query(
        `UPDATE grant_issues SET revoked_at = now()
         WHERE room_id = $1 AND user_id = $2
           AND revoked_at IS NULL AND expires_at > now()`,
        [roomId, userId],
      );
      await client.query(
        'DELETE FROM room_members WHERE room_id = $1 AND user_id = $2',
        [roomId, userId],
      );
      await client.query('COMMIT');
      return { userId, revokedGrants: revoked.rowCount ?? 0 };
    } catch (err) {
      await client.query('ROLLBACK');
      throw err;
    } finally {
      client.release();
    }
  }

  async memberRole(roomId: string, userId: string): Promise<'host' | 'member'> {
    const res = await this.pool.query<{ role: 'host' | 'member' }>(
      'SELECT role FROM room_members WHERE room_id = $1 AND user_id = $2',
      [roomId, userId],
    );
    const row = res.rows[0];
    if (!row) {
      throw new ForbiddenException('not a member of this room');
    }
    return row.role;
  }

  /**
   * Ensure a pairwise key exists for the ordered pair (returning its kid and
   * key). Called by the grants service for every (requester, other-member)
   * pair so both sides always see the same key (plan §5). The room row is
   * locked while allocating the next kid, so concurrent grants in the same
   * room serialize on kid allocation.
   */
  async ensurePairKey(
    roomId: string,
    userA: string,
    userB: string,
  ): Promise<{ kid: number; keyBytes: Buffer }> {
    const [a, b] = userA < userB ? [userA, userB] : [userB, userA];
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      await client.query('SELECT id FROM rooms WHERE id = $1 FOR UPDATE', [roomId]);
      const existing = await client.query<{ kid: number; key_bytes: Buffer }>(
        'SELECT kid, key_bytes FROM pair_keys WHERE room_id = $1 AND user_a = $2 AND user_b = $3',
        [roomId, a, b],
      );
      const hit = existing.rows[0];
      if (hit) {
        await client.query('COMMIT');
        return { kid: hit.kid, keyBytes: hit.key_bytes };
      }
      const res = await client.query<{ kid: number; key_bytes: Buffer }>(
        `INSERT INTO pair_keys (room_id, user_a, user_b, kid, key_bytes)
         VALUES ($1, $2, $3,
                 COALESCE((SELECT MAX(kid) FROM pair_keys WHERE room_id = $1), 0) + 1,
                 $4)
         RETURNING kid, key_bytes`,
        [roomId, a, b, randomKey()],
      );
      await client.query('COMMIT');
      return { kid: res.rows[0].kid, keyBytes: res.rows[0].key_bytes };
    } catch (err) {
      await client.query('ROLLBACK');
      throw err;
    } finally {
      client.release();
    }
  }

  /** All pairwise keys for one member of a room (delivered over REST, §5). */
  async pairKeysFor(roomId: string, userId: string): Promise<PairKey[]> {
    const res = await this.pool.query<{ user_a: string; user_b: string; kid: number; key_bytes: Buffer }>(
      `SELECT user_a, user_b, kid, key_bytes FROM pair_keys
       WHERE room_id = $1 AND (user_a = $2 OR user_b = $2)
       ORDER BY kid`,
      [roomId, userId],
    );
    return res.rows.map((r) => ({
      userA: r.user_a,
      userB: r.user_b,
      kid: r.kid,
      keyBytes: r.key_bytes,
    }));
  }

  /**
   * Rotate every pairwise key in a room (P7c key rotation, plan §5): each
   * pair gets a fresh random key under a new dense kid, continuing past the
   * room's current max (so old and new kids never collide and receivers can
   * keep both installed during the switch). The room row is locked so
   * concurrent rotations/grants serialize on kid allocation, mirroring
   * `ensurePairKey`. Only a host may rotate.
   *
   * The caller receives the new keys and announces `KeyUpdate{kid}` on its
   * control stream; other members pull `GET /rooms/:id/keys` and swap.
   * A room that has rotated past kid 255 fails loudly on the next UPDATE
   * (same bound as new-pair inserts — acceptable for v1, revisit before
   * production).
   */
  async rotatePairKeys(roomId: string, requesterId: string): Promise<PairKey[]> {
    if (!UUID_RE.test(roomId)) {
      throw new NotFoundException('room not found');
    }
    const client = await this.pool.connect();
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
        throw new ForbiddenException('only a host can rotate keys');
      }
      await client.query('SELECT id FROM rooms WHERE id = $1 FOR UPDATE', [roomId]);
      const pairs = await client.query<{ user_a: string; user_b: string }>(
        'SELECT user_a, user_b FROM pair_keys WHERE room_id = $1',
        [roomId],
      );
      const maxRes = await client.query<{ max: number | null }>(
        'SELECT MAX(kid) AS max FROM pair_keys WHERE room_id = $1',
        [roomId],
      );
      let kid = (maxRes.rows[0].max ?? 0) + 1;
      for (const row of pairs.rows) {
        await client.query(
          `UPDATE pair_keys SET kid = $1, key_bytes = $2, created_at = now()
           WHERE room_id = $3 AND user_a = $4 AND user_b = $5`,
          [kid, randomKey(), roomId, row.user_a, row.user_b],
        );
        kid += 1;
      }
      await client.query('COMMIT');
    } catch (err) {
      await client.query('ROLLBACK');
      throw err;
    } finally {
      client.release();
    }
    return this.pairKeysFor(roomId, requesterId);
  }

  private async requireMember(roomId: string, userId: string): Promise<void> {
    const res = await this.pool.query(
      'SELECT 1 FROM room_members WHERE room_id = $1 AND user_id = $2',
      [roomId, userId],
    );
    if (res.rowCount === 0) {
      throw new ForbiddenException('not a member of this room');
    }
  }
}

interface RoomRow {
  id: string;
  name: string;
  created_by: string;
  created_at: Date;
}

interface MemberRow {
  user_id: string;
  role: 'host' | 'member';
  joined_at: Date;
}

function toRoom(row: RoomRow): Room {
  return { id: row.id, name: row.name, createdBy: row.created_by, createdAt: row.created_at };
}

function toMember(row: MemberRow): Member {
  return { userId: row.user_id, role: row.role, joinedAt: row.joined_at };
}

function randomKey(): Buffer {
  return randomBytes(16);
}
