import { INestApplication } from '@nestjs/common';
import { Test } from '@nestjs/testing';
import { Pool } from 'pg';
import * as jose from 'jose';
import request from 'supertest';
import { AppModule } from '../src/app.module';
import { APP_CONFIG } from '../src/config.token';
import type { AppConfig } from '../src/config';

/**
 * P7c revocation e2e (plan §6): grant jtis are recorded at issue, a host
 * kick marks them revoked + removes membership, and GET /auth/revoked —
 * the endpoint `rvp-cli --nest-revoked` polls — exposes exactly the
 * unexpired revoked jtis. Requires a real Postgres (same setup as
 * app.e2e-spec.ts).
 */
describe('rvp-nest revocation (e2e)', () => {
  let app: INestApplication;
  let pool: Pool;
  let aliceToken: string;
  let bobToken: string;
  let roomId: string;
  let bobGrantJti: string;
  let bobGrantJtiAfterReadd: string;

  const config: AppConfig = {
    databaseUrl: process.env.DATABASE_URL ?? '',
    port: 0,
    eddsaSeedHex: '00'.repeat(32),
    grantTtlSeconds: 120,
    sessionTtlSeconds: 3600,
    mediaNodes: [],
    mediaNodesUrl: null,
    redisUrl: null,
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
    await pool.query(
      'TRUNCATE pair_keys, grant_issues, room_members, rooms RESTART IDENTITY CASCADE',
    );

    const alice = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'alice' })
      .expect(200);
    aliceToken = alice.body.token;
    const bob = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'bob' })
      .expect(200);
    bobToken = bob.body.token;

    const room = await request(app.getHttpServer())
      .post('/rooms')
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ name: 'kick-room' })
      .expect(201);
    roomId = room.body.id;
    for (const userId of ['bob', 'carol']) {
      await request(app.getHttpServer())
        .post(`/rooms/${roomId}/members`)
        .set('Authorization', `Bearer ${aliceToken}`)
        .send({ userId })
        .expect(201);
    }
  });

  afterAll(async () => {
    await app?.close();
    await pool?.end();
  });

  async function issueGrant(token: string): Promise<string> {
    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${token}`)
      .expect(201);
    const { jti } = jose.decodeJwt(res.body.grant);
    expect(typeof jti).toBe('string');
    return jti as string;
  }

  async function revokedList(): Promise<string[]> {
    // No Authorization header — the poll endpoint is public (like /auth/jwks).
    const res = await request(app.getHttpServer()).get('/auth/revoked').expect(200);
    expect(Array.isArray(res.body.jtis)).toBe(true);
    return res.body.jtis;
  }

  it('GET /auth/revoked is public and empty before any kick', async () => {
    expect(await revokedList()).toEqual([]);
  });

  it('kick requires an authenticated host, not another member', async () => {
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .send({ userId: 'bob' })
      .expect(401);

    // bob is a member but not the host.
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .set('Authorization', `Bearer ${bobToken}`)
      .send({ userId: 'carol' })
      .expect(403);
  });

  it('kick validates its target', async () => {
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({})
      .expect(400);

    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'alice' })
      .expect(400);

    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'ghost' })
      .expect(404);
  });

  it('issuing a grant records its jti without revoking it', async () => {
    bobGrantJti = await issueGrant(bobToken);
    expect(await revokedList()).not.toContain(bobGrantJti);

    const rows = await pool.query<{ revoked_at: Date | null }>(
      'SELECT revoked_at FROM grant_issues WHERE jti = $1',
      [bobGrantJti],
    );
    expect(rows.rowCount).toBe(1);
    expect(rows.rows[0].revoked_at).toBeNull();
  });

  it('host kick revokes unexpired grants and removes membership', async () => {
    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'bob' })
      .expect(201);
    expect(res.body).toEqual({ userId: 'bob', revokedGrants: 1 });

    expect(await revokedList()).toEqual([bobGrantJti]);

    // Membership is gone: room reads and fresh grants are refused...
    await request(app.getHttpServer())
      .get(`/rooms/${roomId}`)
      .set('Authorization', `Bearer ${bobToken}`)
      .expect(403);
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${bobToken}`)
      .expect(403);

    // ...while the kicked member's session token itself still works.
    await request(app.getHttpServer())
      .post('/rooms')
      .set('Authorization', `Bearer ${bobToken}`)
      .send({ name: 'bobs-own-room' })
      .expect(201);
  });

  it('re-adding the member grants fresh unrevoked access', async () => {
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/members`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'bob' })
      .expect(201);

    bobGrantJtiAfterReadd = await issueGrant(bobToken);
    const list = await revokedList();
    expect(list).toContain(bobGrantJti);
    expect(list).not.toContain(bobGrantJtiAfterReadd);

    await request(app.getHttpServer())
      .get(`/rooms/${roomId}`)
      .set('Authorization', `Bearer ${bobToken}`)
      .expect(200);
  });

  it('a second kick revokes the new grant too', async () => {
    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/kick`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'bob' })
      .expect(201);
    expect(res.body).toEqual({ userId: 'bob', revokedGrants: 1 });

    const list = await revokedList();
    expect(list).toEqual(
      expect.arrayContaining([bobGrantJti, bobGrantJtiAfterReadd]),
    );
    expect(list).toHaveLength(2);
  });
});
