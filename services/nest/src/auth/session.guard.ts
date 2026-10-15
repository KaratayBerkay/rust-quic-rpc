import {
  CanActivate,
  ExecutionContext,
  Injectable,
  UnauthorizedException,
} from '@nestjs/common';
import { randomUUID } from 'node:crypto';
import * as jose from 'jose';
import type { Request } from 'express';
import { KeysService } from '../keys/keys.service';

export interface AuthedRequest extends Request {
  userId: string;
  jti: string;
}

const PUBLIC_PATHS = new Set(['/auth/login', '/auth/jwks', '/auth/revoked']);

/**
 * Bearer-session guard for every endpoint except POST /auth/login,
 * GET /auth/jwks and GET /auth/revoked (all machine-to-machine endpoints
 * consumed by rvp-cli without a user session). Verifies the long-lived
 * session JWT (iss=nest-api, aud=rvp-api) signed by [KeysService] and
 * attaches `userId`/`jti`.
 */
@Injectable()
export class SessionGuard implements CanActivate {
  constructor(private readonly keys: KeysService) {}

  async canActivate(context: ExecutionContext): Promise<boolean> {
    const req = context.switchToHttp().getRequest<Request>();
    if (PUBLIC_PATHS.has(req.path)) {
      return true;
    }
    const header = req.headers.authorization;
    if (!header?.startsWith('Bearer ')) {
      throw new UnauthorizedException('missing bearer session token');
    }
    const { payload } = await jose.jwtVerify(header.slice(7), this.keys.publicVerificationKey(), {
      issuer: 'nest-api',
      audience: 'rvp-api',
      clockTolerance: 5,
    });
    if (typeof payload.sub !== 'string' || payload.sub.length === 0) {
      throw new UnauthorizedException('session token missing sub');
    }
    (req as AuthedRequest).userId = payload.sub;
    (req as AuthedRequest).jti = typeof payload.jti === 'string' ? payload.jti : randomUUID();
    return true;
  }
}
