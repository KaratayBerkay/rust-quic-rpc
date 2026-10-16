import { Controller, Inject, Param, Post, Req } from '@nestjs/common';
import { RoomsService } from '../rooms/rooms.service';
import { KeysService } from '../keys/keys.service';
import { GrantIssuesService } from './grant-issues.service';
import { MediaNodesService } from '../media-nodes/media-nodes.service';
import { APP_CONFIG } from '../config.token';
import type { AppConfig } from '../config';
import type { AuthedRequest } from '../auth/session.guard';

interface PairGrant {
  peer: string;
  kid: number;
  key: string; // hex, 16 bytes
}

interface GrantResponse {
  grant: string;
  expiresAt: number;
  roomId: string;
  userId: string;
  role: 'host' | 'member';
  pairs: PairGrant[];
  /** P8a: pin's `host:port`, when the room is pinned and resolvable. */
  mediaAddress?: string | null;
}

/**
 * POST /rooms/:id/grants — media grant + pairwise keys (plan §5).
 *
 * Verifies membership, signs a short-lived EdDSA media grant (iss=nest,
 * aud=rvp — consumed by `rvp-auth::Verifier`), and returns the pairwise
 * AES-128 keys this user needs, one per other member, with dense per-room
 * kids (P4b media-header byte3).
 *
 * P8a: when the room is pinned to a media node, the grant carries the
 * `media_node` claim and the response includes `mediaAddress` so the client
 * can connect to the pinned node (plan §2/§5).
 */
@Controller('rooms/:id/grants')
export class GrantsController {
  constructor(
    private readonly rooms: RoomsService,
    private readonly keys: KeysService,
    private readonly grantIssues: GrantIssuesService,
    private readonly mediaNodes: MediaNodesService,
    @Inject(APP_CONFIG) private readonly config: AppConfig,
  ) {}

  @Post()
  async issue(
    @Req() req: AuthedRequest,
    @Param('id') id: string,
  ): Promise<GrantResponse> {
    const role = await this.rooms.memberRole(id, req.userId);
    // P8a: look up the room's pin (if any) to stamp the grant + address.
    const room = await this.rooms.getRoom(id, req.userId);
    const { token, expiresAt, jti } = await this.keys.signMediaGrant({
      userId: req.userId,
      roomId: id,
      role,
      ttlSeconds: this.config.grantTtlSeconds,
      mediaNode: room.mediaNode ?? undefined,
    });
    // Record before returning: the grant must already be revocable when the
    // client starts using it (P7c).
    await this.grantIssues.record({
      jti,
      roomId: id,
      userId: req.userId,
      expiresAt,
    });
    const members = await this.rooms.listMembers(id);
    const pairs: PairGrant[] = [];
    for (const member of members) {
      if (member.userId === req.userId) {
        continue;
      }
      const { kid, keyBytes } = await this.rooms.ensurePairKey(
        id,
        req.userId,
        member.userId,
      );
      pairs.push({ peer: member.userId, kid, key: keyBytes.toString('hex') });
    }
    const pin = room.mediaNode
      ? this.mediaNodes.list().find((n) => n.id === room.mediaNode)
      : undefined;
    return {
      grant: token,
      expiresAt,
      roomId: id,
      userId: req.userId,
      role,
      pairs,
      mediaAddress: pin?.address ?? null,
    };
  }
}
