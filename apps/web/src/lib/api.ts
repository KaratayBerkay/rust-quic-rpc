/**
 * NestJS control-plane client (plan §5, P5 endpoints).
 */

/** Raw `POST /auth/login` response (`services/nest/src/auth/auth.controller.ts`). */
interface LoginRawResponse {
  token: string;
  userId: string;
  ttlSeconds: number;
}

/** Normalized session used by the app (maps Nest's `token` → `accessToken`). */
export interface SessionResponse {
  accessToken: string;
  expiresAt: number;
  userId: string;
}

export interface RoomResponse {
  id: string;
  name: string;
  createdBy: string;
  createdAt: string;
  /** P8a media-node pin (plan §2/§5). Null = unpinned (single-node). */
  mediaNode?: string | null;
  /** P8a `host:port` of the pinned node, when resolvable from the directory. */
  mediaAddress?: string | null;
}

export interface MemberResponse {
  roomId: string;
  userId: string;
  role: "host" | "member";
  joinedAt: string;
}

export interface PairKey {
  peer: string;
  kid: number;
  key: string;
}

export interface GrantResponse {
  grant: string;
  expiresAt: string;
  roomId: string;
  userId: string;
  role: string;
  pairs: PairKey[];
  /** P8a: pin's `host:port`, when the room is pinned and resolvable. */
  mediaAddress?: string | null;
}

export class NestApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "NestApiError";
  }
}

async function request<T>(
  baseUrl: string,
  path: string,
  init: RequestInit = {},
  token?: string,
): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("Content-Type", "application/json");
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  const res = await fetch(`${baseUrl.replace(/\/$/, "")}${path}`, {
    ...init,
    headers,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new NestApiError(
      `${init.method ?? "GET"} ${path}: ${res.status} ${text}`.trim(),
      res.status,
    );
  }
  return (await res.json()) as T;
}

export async function login(
  baseUrl: string,
  userId: string,
): Promise<SessionResponse> {
  const raw = await request<LoginRawResponse>(baseUrl, "/auth/login", {
    method: "POST",
    body: JSON.stringify({ userId }),
  });
  return {
    accessToken: raw.token,
    userId: raw.userId,
    expiresAt: Math.floor(Date.now() / 1000) + raw.ttlSeconds,
  };
}

export function createRoom(
  baseUrl: string,
  token: string,
  name: string,
): Promise<RoomResponse> {
  return request<RoomResponse>(
    baseUrl,
    "/rooms",
    { method: "POST", body: JSON.stringify({ name }) },
    token,
  );
}

export function getRoom(
  baseUrl: string,
  token: string,
  roomId: string,
): Promise<RoomResponse> {
  return request<RoomResponse>(baseUrl, `/rooms/${roomId}`, {}, token);
}

export function addMember(
  baseUrl: string,
  token: string,
  roomId: string,
  userId: string,
): Promise<MemberResponse> {
  return request<MemberResponse>(
    baseUrl,
    `/rooms/${roomId}/members`,
    { method: "POST", body: JSON.stringify({ userId }) },
    token,
  );
}

export function createGrant(
  baseUrl: string,
  token: string,
  roomId: string,
): Promise<GrantResponse> {
  return request<GrantResponse>(
    baseUrl,
    `/rooms/${roomId}/grants`,
    { method: "POST" },
    token,
  );
}

export interface KeysResponse {
  pairs: PairKey[];
}

/** Pull the caller's current pairwise keys (e.g. after a relayed keyUpdate). */
export function fetchPairKeys(
  baseUrl: string,
  token: string,
  roomId: string,
): Promise<KeysResponse> {
  return request<KeysResponse>(baseUrl, `/rooms/${roomId}/keys`, {}, token);
}

/**
 * Host-only: rotate every pairwise key in the room (new dense kids, new
 * random keys) and receive the caller's fresh pairs to announce via
 * `KeyUpdate` (plan §5).
 */
export function rotatePairKeys(
  baseUrl: string,
  token: string,
  roomId: string,
): Promise<KeysResponse> {
  return request<KeysResponse>(
    baseUrl,
    `/rooms/${roomId}/keys/rotate`,
    { method: "POST" },
    token,
  );
}
