import { INestApplication } from '@nestjs/common';
import { Test } from '@nestjs/testing';
import { Pool } from 'pg';
import request from 'supertest';
import { AppModule } from '../src/app.module';
import { APP_CONFIG } from '../src/config.token';
import type { AppConfig } from '../src/config';

/**
 * P7c key-rotation e2e (plan §5): a host rotates every pairwise key in the
 * room (new dense kids, new random keys), members pull the fresh keys over
 * `GET /rooms/:id/keys`, and non-hosts cannot rotate. Requires a real
 * Postgres (same setup as app.e2e-spec.ts).
 */
describe('rvp-nest key rotation (e2e)', () => {
  let app: INestApplication;
  let pool: Pool;
  let aliceToken: string;
  let bobToken: string;
  let carolToken: string;
  let roomId: string;

  const config: AppConfig = {
    databaseUrl: process.env.DATABASE_URL ?? '',
    port: 0,
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
    const carol = await request(app.getHttpServer())
      .post('/auth/login')
      .send({ userId: 'carol' })
      .expect(200);
    carolToken = carol.body.token;

    const room = await request(app.getHttpServer())
      .post('/rooms')
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ name: 'rotate-room' })
      .expect(201);
    roomId = room.body.id;
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/members`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ userId: 'bob' })
      .expect(201);
  });

  afterAll(async () => {
    await app?.close();
    await pool?.end();
  });

  async function issueGrant(token: string): Promise<{ pairs: Array<{ peer: string; kid: number; key: string }> }> {
    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/grants`)
      .set('Authorization', `Bearer ${token}`)
      .expect(201);
    return { pairs: res.body.pairs };
  }

  async function getKeys(token: string): Promise<Array<{ peer: string; kid: number; key: string }>> {
    const res = await request(app.getHttpServer())
      .get(`/rooms/${roomId}/keys`)
      .set('Authorization', `Bearer ${token}`)
      .expect(200);
    return res.body.pairs;
  }

  it('requires auth and membership for GET /rooms/:id/keys', async () => {
    await request(app.getHttpServer()).get(`/rooms/${roomId}/keys`).expect(401);
    // Valid session, but not a member of the room.
    await request(app.getHttpServer())
      .get(`/rooms/${roomId}/keys`)
      .set('Authorization', `Bearer ${carolToken}`)
      .expect(403);
  });

  it('issues matching pairs to both members', async () => {
    const alice = await issueGrant(aliceToken);
    const bob = await issueGrant(bobToken);
    expect(alice.pairs).toHaveLength(1);
    expect(bob.pairs).toHaveLength(1);
    expect(alice.pairs[0].peer).toBe('bob');
    expect(bob.pairs[0].peer).toBe('alice');
    expect(alice.pairs[0].kid).toBe(bob.pairs[0].kid);
    expect(alice.pairs[0].key).toBe(bob.pairs[0].key);
    expect(alice.pairs[0].key).toMatch(/^[0-9a-f]{32}$/);
  });

  it('rejects rotation from a non-host member', async () => {
    await request(app.getHttpServer())
      .post(`/rooms/${roomId}/keys/rotate`)
      .set('Authorization', `Bearer ${bobToken}`)
      .expect(403);
  });

  it('host rotation issues new dense kids and new keys to both members', async () => {
    const beforeAlice = await getKeys(aliceToken);
    const beforeBob = await getKeys(bobToken);
    // Both sides hold the same kid+key (the `peer` field is per-perspective:
    // alice's pair.peer is bob and vice versa).
    expect(beforeAlice).toHaveLength(1);
    expect(beforeBob).toHaveLength(1);
    expect(beforeAlice[0].kid).toBe(beforeBob[0].kid);
    expect(beforeAlice[0].key).toBe(beforeBob[0].key);
    expect(beforeAlice[0].peer).toBe('bob');
    expect(beforeBob[0].peer).toBe('alice');
    const oldKid = beforeAlice[0].kid;
    const oldKey = beforeAlice[0].key;

    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/keys/rotate`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .expect(201);
    expect(res.body.pairs).toHaveLength(1);
    const newKid = res.body.pairs[0].kid as number;
    const newKey = res.body.pairs[0].key as string;
    expect(newKid).toBeGreaterThan(oldKid);
    expect(newKey).not.toBe(oldKey);
    expect(newKey).toMatch(/^[0-9a-f]{32}$/);

    // Both members pull the same fresh kid+key (peer is per-perspective).
    const aliceAfter = await getKeys(aliceToken);
    const bobAfter = await getKeys(bobToken);
    expect(aliceAfter[0].kid).toBe(newKid);
    expect(aliceAfter[0].key).toBe(newKey);
    expect(bobAfter[0].kid).toBe(newKid);
    expect(bobAfter[0].key).toBe(newKey);
  });

  it('a second rotation keeps incrementing kids', async () => {
    const before = await getKeys(aliceToken);
    const res = await request(app.getHttpServer())
      .post(`/rooms/${roomId}/keys/rotate`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .expect(201);
    expect(res.body.pairs[0].kid).toBe(before[0].kid + 1);
    expect(res.body.pairs[0].key).not.toBe(before[0].key);
  });

  it('rotation is a no-op on rooms with no pairs', async () => {
    const empty = await request(app.getHttpServer())
      .post('/rooms')
      .set('Authorization', `Bearer ${aliceToken}`)
      .send({ name: 'empty-room' })
      .expect(201);
    const res = await request(app.getHttpServer())
      .post(`/rooms/${empty.body.id}/keys/rotate`)
      .set('Authorization', `Bearer ${aliceToken}`)
      .expect(201);
    expect(res.body.pairs).toEqual([]);
  });
});
