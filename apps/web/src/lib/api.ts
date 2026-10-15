/**
 * NestJS control-plane client (plan §5, P5 endpoints).
 */

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

export function login(baseUrl: string, userId: string): Promise<SessionResponse> {
  return request<SessionResponse>(baseUrl, "/auth/login", {
    method: "POST",
    body: JSON.stringify({ userId }),
  });
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
