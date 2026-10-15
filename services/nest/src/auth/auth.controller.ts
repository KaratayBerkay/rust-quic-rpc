import { Body, Controller, Get, HttpCode, Post, UnauthorizedException } from '@nestjs/common';
import type { JWK } from 'jose';
import { KeysService } from '../keys/keys.service';
import { GrantIssuesService } from '../grants/grant-issues.service';

/** POST /auth/login — issue a long-lived session JWT (plan §5). */
@Controller('auth')
export class AuthController {
  constructor(
    private readonly keys: KeysService,
    private readonly grantIssues: GrantIssuesService,
  ) {}

  @Post('login')
  @HttpCode(200)
  async login(@Body() body: { userId?: unknown }): Promise<{
    token: string;
    userId: string;
    ttlSeconds: number;
  }> {
    const userId = body?.userId;
    if (typeof userId !== 'string' || userId.length === 0 || userId.length > 128) {
      throw new UnauthorizedException('userId (non-empty string, ≤128 chars) required');
    }
    const ttlSeconds = this.keys.sessionTtlSeconds;
    const token = await this.keys.signSession(userId, ttlSeconds);
    return { token, userId, ttlSeconds };
  }

  /** Public: JWKS for rvp-cli / rvp-auth (plan §5). */
  @Get('jwks')
  jwks(): { keys: JWK[] } {
    return this.keys.jwks();
  }

  /** Public: revoked media-grant jtis for rvp-cli --nest-revoked (P7c). */
  @Get('revoked')
  async revoked(): Promise<{ jtis: string[] }> {
    return { jtis: await this.grantIssues.revokedJtis() };
  }
}
