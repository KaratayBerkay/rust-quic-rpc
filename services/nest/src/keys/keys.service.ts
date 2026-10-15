import { Injectable } from '@nestjs/common';
import {
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  randomBytes,
  randomUUID,
  type KeyObject,
} from 'node:crypto';
import * as jose from 'jose';
import { AppConfig } from '../config';

const KEY_ID = 'nest-1';

/** RFC 8410 PKCS8 Ed25519 header; ‖ 32-byte seed = DER private key. */
const PKCS8_ED25519_PREFIX = Buffer.from('302e020100300506032b657004220420', 'hex');

/**
 * Ed25519 signing-key holder (plan §5).
 *
 * One key signs both session JWTs (iss=nest-api, aud=rvp-api) and media
 * grants (iss=nest, aud=rvp — what `rvp-auth::Verifier` expects). The public
 * half is served at GET /auth/jwks; `rvp-cli --nest-jwks` polls it, so a
 * restart with a new key is picked up by the media server without action.
 */
@Injectable()
export class KeysService {
  private readonly privateKey: KeyObject;
  readonly publicJwk: jose.JWK;
  readonly grantIssuer = 'nest';
  readonly grantAudience = 'rvp';
  readonly sessionTtlSeconds: number;
  readonly grantTtlSeconds: number;

  private constructor(
    privateKey: KeyObject,
    publicJwk: jose.JWK,
    sessionTtlSeconds: number,
    grantTtlSeconds: number,
  ) {
    this.privateKey = privateKey;
    this.publicJwk = publicJwk;
    this.sessionTtlSeconds = sessionTtlSeconds;
    this.grantTtlSeconds = grantTtlSeconds;
  }

  static async create(config: AppConfig): Promise<KeysService> {
    let privateKey: KeyObject;
    if (config.eddsaSeedHex) {
      const seed = Buffer.from(config.eddsaSeedHex, 'hex');
      if (seed.length !== 32) {
        throw new Error('NEST_EDDSA_SEED must be 32 bytes of hex');
      }
      privateKey = createPrivateKey({
        key: Buffer.concat([PKCS8_ED25519_PREFIX, seed]),
        format: 'der',
        type: 'pkcs8',
      });
    } else {
      privateKey = generateKeyPairSync('ed25519').privateKey;
      if (process.env.NODE_ENV !== 'production') {
        console.warn(
          '[keys] NEST_EDDSA_SEED not set — generated an ephemeral Ed25519 key; ' +
            'media grants will not verify after a restart',
        );
      }
    }
    const rawPub = await jose.exportJWK(createPublicKey(privateKey));
    const publicJwk: jose.JWK = {
      kty: 'OKP',
      crv: 'Ed25519',
      x: rawPub.x,
      kid: KEY_ID,
      alg: 'EdDSA',
      use: 'sig',
    };
    return new KeysService(
      privateKey,
      publicJwk,
      config.sessionTtlSeconds,
      config.grantTtlSeconds,
    );
  }

  async signSession(userId: string, ttlSeconds: number): Promise<string> {
    const now = Math.floor(Date.now() / 1000);
    return new jose.SignJWT({ sub: userId })
      .setProtectedHeader({ alg: 'EdDSA', kid: KEY_ID })
      .setIssuer('nest-api')
      .setAudience('rvp-api')
      .setIssuedAt(now)
      .setExpirationTime(now + ttlSeconds)
      .setJti(randomUUID())
      .sign(this.privateKey);
  }

  /**
   * Media grant for the SFU handshake (plan §5). `rvp-auth` validates
   * iss=nest, aud=rvp, exp, and the sub/room_id/role claims. The `jti` is
   * returned so the caller can record it for revocation (P7c).
   */
  async signMediaGrant(args: {
    userId: string;
    roomId: string;
    role: string;
    ttlSeconds: number;
    mediaNode?: string;
  }): Promise<{ token: string; expiresAt: number; jti: string }> {
    const now = Math.floor(Date.now() / 1000);
    const expiresAt = now + args.ttlSeconds;
    const jti = randomUUID();
    const payload: Record<string, string> = {
      sub: args.userId,
      room_id: args.roomId,
      role: args.role,
    };
    if (args.mediaNode) {
      payload.media_node = args.mediaNode;
    }
    const token = await new jose.SignJWT(payload)
      .setProtectedHeader({ alg: 'EdDSA', kid: KEY_ID })
      .setIssuer(this.grantIssuer)
      .setAudience(this.grantAudience)
      .setIssuedAt(now)
      .setExpirationTime(expiresAt)
      .setJti(jti)
      .sign(this.privateKey);
    return { token, expiresAt, jti };
  }

  /** Public half, as a CryptoKey, for verifying our own session tokens. */
  publicVerificationKey(): KeyObject {
    return createPublicKey(this.privateKey);
  }

  /** JWKS document for GET /auth/jwks (rvp-auth `Verifier::from_jwks`). */
  jwks(): { keys: jose.JWK[] } {
    return { keys: [this.publicJwk] };
  }
}

/** Random 16-byte AES-128 pairwise key, hex-encoded (P4b `PairwiseKey`). */
export function randomPairwiseKeyHex(): string {
  return randomBytes(16).toString('hex');
}
