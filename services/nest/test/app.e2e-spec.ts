import { INestApplication } from '@nestjs/common';
import { Test } from '@nestjs/testing';
import { Pool } from 'pg';
import * as jose from 'jose';
import request from 'supertest';
import { AppModule } from '../src/app.module';
import { APP_CONFIG } from '../src/config.token';
import type { AppConfig } from '../src/config';

/**
 * End-to-end control-plane flow (plan §5), against a real Postgres
 * (DATABASE_URL; docker-compose.yml provides one locally, CI runs
 * postgres:16). Covers: login → JWKS → rooms → membership → grants with
 * shared pairwise keys.
 */
describe('rvp-nest (e2e)', () => {
  let app: INestApplication;
  let pool: Pool;
  let aliceToken: string;
  let bobToken: string;
  let roomId: string;

  const config: AppConfig = {
    databaseUrl: process.env.DATABASE_URL ?? '',
    port: 0,
    // Deterministic dev seed — 32 bytes of hex (config contract).
    eddsaSeedHex: '00'.repeat(32),
    grantTtlSeconds: 120,
    sessionTtlSeconds: 3600,
  };

  beforeAll(async () => {
    if (!config.databaseUrl) {
      throw new Error('DATABASE_URL is required for e2e tests');
    }
    const moduleRef = await Test.createTestingModule({ imports: [AppModule] })
      .overrideProvider(APP_CONFIG)
      .useValue(config)
      .compile();
    app = moduleRef.createNestApplication();
    await app.init();
    pool = app.get(Pool);
    // Clean slate for this run (dev DB).
    await pool.query(
      'TRUNCATE pair_keys, room_members, rooms RESTART IDENTITY CASCADE',
    );
  });

  afterAll(async () => {
    await app?.close();
    await pool?.end();
  });

  it('POST /auth/login issues a session JWT', async () => {
    const res = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'alice' })
      .expect(200);
    expect(res.body.token).toEqual(expect.any(String));
    aliceToken = res.body.token;

    const bob = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'bob' })
      .expect(200);
    bobToken = bob.body.token;
  });

  it('POST /auth/login rejects missing userId', async () => {
    await request(app.getHttpServer()).post('/auth/login').send({}).expect(401);
  });

  it('GET /auth/jwks serves an Ed25519 verification key', async () => {
    const res = await request(app.getHttpServer()).get('/auth/jwks').expect(200);
    expect(res.body.keys).toHaveLength(1);
    const key = res.body.keys[0];
    expect(key).toMatchObject({ kty: 'OKP', crv: 'Ed25519', alg: 'EdDSA' });
    expect(key.x).toEqual(expect.any(String));
  });

  it('rejects protected endpoints without a session token', async () => {
    await request(app.getHttpServer()).post('/rooms').send({ name: 'x' }).expect(401);
    await request(app.getHttpServer()).get('/rooms/anything').expect(401);
  });

  it('POST /rooms creates a room with the creator as host', async () => {
    const res = await request(app.getHttpServer())
      .post('/rooms')
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ name: 'demo' })
      .expect(201);
    roomId = res.body.id;
    expect(res.body.name).toBe('demo');
    expect(res.body.createdBy).toBe('alice');
  });

  it('GET /rooms/:id returns members to room members only', async () => {
    await request(app.getHttpServer())
      .get(`/rooms/${roomId}`)
      .set('Authorization', `Bearer ${bobToken}`)
      .expect(403);

    const res = await request(app.getHttpServer())
      .get(`/rooms/${roomId}`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .expect(200);
    expect(res.body.members).toHaveLength(1);
    expect(res.body.members[0]).toMatchObject({ userId: 'alice', role: 'host' });
  });

  it('POST /rooms/:id/members adds a member (alice adds bob)', async () => {
    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/members`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'bob' })
      .expect(201);
    expect(res.body).toMatchObject({ userId: 'bob', role: 'member' });

    // Bob cannot add himself again (already a member).
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/members`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'bob' })
      .expect(409);
  });

  it('POST /rooms/:id/grants issues a verifiable grant + shared pairwise key', async () => {
    const bobGrant = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${bobToken}`)
      .expect(201);
    expect(bobGrant.body).toMatchObject({ userId: 'bob', role: 'member', roomId });
    expect(bobGrant.body.pairs).toHaveLength(1);
    const bobPair = bobGrant.body.pairs[0];
    expect(bobPair.peer).toBe('alice');
    expect(bobPair.kid).toBe(1);
    expect(bobPair.key).toMatch(/^[0-9a-f]{32}$/);

    const aliceGrant = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .expect(201);
    const alicePair = aliceGrant.body.pairs[0];
    expect(alicePair.peer).toBe('bob');
    // Both sides see the SAME kid + key for the pair (plan §5 / P4b).
    expect(alicePair.kid).toBe(bobPair.kid);
    expect(alicePair.key).toBe(bobPair.key);

    // Grant exp is within the configured TTL.
    const now = Math.floor(Date.now() / 1000);
    expect(bobGrant.body.expiresAt).toBeGreaterThan(now);
    expect(bobGrant.body.expiresAt).toBeLessThanOrEqual(now + config.grantTtlSeconds + 5);

    // Grant signature verifies against /auth/jwks (what rvp-cli does).
    const jwks = await request(app.getHttpServer()).get('/auth/jwks').expect(200);
    const key = await jose.importJWK(jwks.body.keys[0], 'EdDSA');
    const { payload } = await jose.jwtVerify(bobGrant.body.grant, key, {
      issuer: 'nest',
      audience: 'rvp',
    });
    expect(payload.sub).toBe('bob');
    expect(payload.room_id).toBe(roomId);
    expect(payload.role).toBe('member');
  });

  it('POST /rooms/:id/grants rejects non-members', async () => {
    const carol = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'carol' })
      .expect(200);
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${carol.body.token}`)
      .expect(403);
  });

  it('second pair (alice↔carol) gets the next kid', async () => {
    const carol = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'carol' })
      .expect(200);
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/members`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'carol' })
      .expect(201);

    const grant = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${carol.body.token}`)
      .expect(201);
    const peers = grant.body.pairs.map((p: { peer: string; kid: number }) => p.peer).sort();
    expect(peers).toEqual(['alice', 'bob']);
    // (alice,bob) took kid 1 in the previous test; carol's pairs continue
    // the per-room sequence: (alice,carol)=2, (bob,carol)=3.
    const kids = grant.body.pairs.map((p: { kid: number }) => p.kid).sort();
    expect(kids).toEqual([2, 3]);
  });
});
