import {
  BadRequestException,
  Body,
  Controller,
  Get,
  Param,
  Post,
  Req,
} from '@nestjs/common';
import { RoomsService, type Member, type Room } from './rooms.service';
import type { AuthedRequest } from '../auth/session.guard';

/** Rooms + membership endpoints (plan §5). */
@Controller('rooms')
export class RoomsController {
  constructor(private readonly rooms: RoomsService) {}

  @Post()
  async createRoom(
    @Req() req: AuthedRequest,
    @Body() body: { name?: unknown },
  ): Promise<Room> {
    const name = typeof body?.name === 'string' ? body.name : '';
    return this.rooms.createRoom(req.userId, name);
  }

  @Get(':id')
  getRoom(
    @Req() req: AuthedRequest,
    @Param('id') id: string,
  ): Promise<Room & { members: Member[] }> {
    return this.rooms.getRoom(id, req.userId);
  }

  @Post(':id/members')
  async addMember(
    @Req() req: AuthedRequest,
    @Param('id') id: string,
    @Body() body: { userId?: unknown; role?: unknown },
  ): Promise<Member> {
    const userId = typeof body?.userId === 'string' ? body.userId : '';
    const role = body?.role === 'host' ? 'host' : 'member';
    return this.rooms.addMember(id, req.userId, userId, role);
  }

  /**
   * POST /rooms/:id/kick — host removes a member and revokes their unexpired
   * media grants (P7c). rvp-cli's next `/auth/revoked` poll closes their live
   * media session; the media server also refuses their grant at rejoin.
   */
  @Post(':id/kick')
  async kick(
    @Req() req: AuthedRequest,
    @Param('id') id: string,
    @Body() body: { userId?: unknown },
  ): Promise<{ userId: string; revokedGrants: number }> {
    const userId = typeof body?.userId === 'string' ? body.userId : '';
    if (userId.length === 0) {
      throw new BadRequestException('userId (non-empty string) required');
    }
    return this.rooms.kickMember(id, req.userId, userId);
  }
}
