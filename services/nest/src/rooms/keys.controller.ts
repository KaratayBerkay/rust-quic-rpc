import {
  Controller,
  Get,
  Param,
  Post,
  Req,
} from '@nestjs/common';
import { RoomsService, type PairKey } from './rooms.service';
import type { AuthedRequest } from '../auth/session.guard';

interface PairGrant {
  peer: string;
  kid: number;
  key: string; // hex, 16 bytes
}

function toPairGrant(userId: string, k: PairKey): PairGrant {
  return {
    peer: k.userA === userId ? k.userB : k.userA,
    kid: k.kid,
    key: k.keyBytes.toString('hex'),
  };
}

/**
 * Pairwise-key endpoints (P7c key rotation, plan §5).
 *
 * `GET /rooms/:id/keys` — a member pulls their current pairwise keys (the
 * same shape the grant response carries), e.g. after seeing a relayed
 * `KeyUpdate` from another member.
 *
 * `POST /rooms/:id/keys/rotate` — the host rotates every pairwise key in the
 * room (fresh keys under new dense kids) and receives the caller's new keys.
 * The host then announces `KeyUpdate{kid}` on its media control stream; other
 * members pull `GET /rooms/:id/keys` and swap their pipeline peers.
 */
@Controller('rooms/:id/keys')
export class KeysController {
  constructor(private readonly rooms: RoomsService) {}

  @Get()
  async list(
    @Req() req: AuthedRequest,
    @Param('id') id: string,
  ): Promise<{ pairs: PairGrant[] }> {
    await this.rooms.memberRole(id, req.userId);
    const keys = await this.rooms.pairKeysFor(id, req.userId);
    return { pairs: keys.map((k) => toPairGrant(req.userId, k)) };
  }

  @Post('rotate')
  async rotate(
    @Req() req: AuthedRequest,
    @Param('id') id: string,
  ): Promise<{ pairs: PairGrant[] }> {
    const keys = await this.rooms.rotatePairKeys(id, req.userId);
    return { pairs: keys.map((k) => toPairGrant(req.userId, k)) };
  }
}
