import { afterEach, describe, expect, it, vi } from "vitest";
import { login } from "./api";

/**
 * Nest contract test (plan §5): the server issues `{token, userId,
 * ttlSeconds}` (`services/nest/src/auth/auth.controller.ts`); the app stores
 * `{accessToken, userId, expiresAt}`. A drift here breaks every authorized
 * call (Authorization: Bearer undefined) — the mapping is pinned against the
 * Nest e2e fixture shape.
 */
describe("login (Nest §5 contract)", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("maps Nest's token/ttlSeconds onto the app session shape", async () => {
    const fetchMock = vi.fn(async () =>
      new Response(
        JSON.stringify({ token: "tok-123", userId: "alice", ttlSeconds: 86400 }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    const before = Math.floor(Date.now() / 1000);
    const session = await login("http://127.0.0.1:3000", "alice");

    expect(session.accessToken).toBe("tok-123");
    expect(session.userId).toBe("alice");
    expect(session.expiresAt).toBeGreaterThanOrEqual(before + 86400);
    expect(session.expiresAt).toBeLessThanOrEqual(
      Math.floor(Date.now() / 1000) + 86400,
    );

    // The request itself carries the userId Nest expects.
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://127.0.0.1:3000/auth/login");
    expect(JSON.parse(String(init.body))).toEqual({ userId: "alice" });
  });

  it("propagates Nest errors with status", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response("unauthorized", { status: 401 }),
      ),
    );
    await expect(login("http://127.0.0.1:3000", "")).rejects.toMatchObject({
      status: 401,
    });
  });
});
