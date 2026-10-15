import {
  QueryClient,
  QueryClientProvider,
} from "@tanstack/react-query";
import {
  createRootRoute,
  createRoute,
  createRouter,
  Link,
  Outlet,
  RouterProvider,
  useNavigate,
} from "@tanstack/react-router";
import { useState, useSyncExternalStore } from "react";
import {
  addMember,
  createGrant,
  createRoom,
  login,
  NestApiError,
  type GrantResponse,
  type SessionResponse,
} from "./lib/api";
import { connectWt, type WtSession } from "./lib/wt";

// --- session store (localStorage-backed; fine for the P6a demo shell) ------

const SESSION_KEY = "rvp.session";
const SERVER_KEY = "rvp.nestUrl";
const WT_KEY = "rvp.wtUrl";
const HASH_KEY = "rvp.wtCertHash";

type StoredSession = SessionResponse;

function readStore<T>(key: string): T | null {
  const raw = localStorage.getItem(key);
  return raw ? (JSON.parse(raw) as T) : null;
}

function writeStore(key: string, value: unknown): void {
  localStorage.setItem(key, JSON.stringify(value));
}

function subscribe(cb: () => void): () => void {
  window.addEventListener("storage", cb);
  return () => window.removeEventListener("storage", cb);
}

function useSession(): StoredSession | null {
  return useSyncExternalStore(
    subscribe,
    () => readStore<StoredSession>(SESSION_KEY),
    () => null,
  );
}

// --- root ------------------------------------------------------------------

const queryClient = new QueryClient();

const rootRoute = createRootRoute({
  component: () => (
    <div style={{ fontFamily: "system-ui, sans-serif", padding: 24, maxWidth: 720, margin: "0 auto" }}>
      <h1 style={{ marginTop: 0 }}>
        <Link to="/" style={{ color: "inherit", textDecoration: "none" }}>
          RVP
        </Link>
      </h1>
      <Outlet />
    </div>
  ),
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: IndexPage,
});

const roomsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/rooms",
  component: RoomsPage,
});

const roomRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/rooms/$roomId",
  component: RoomPage,
});

const routeTree = rootRoute.addChildren([indexRoute, roomsRoute, roomRoute]);
const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

// --- pages -----------------------------------------------------------------

function IndexPage() {
  const session = useSession();
  const navigate = useNavigate({ from: "/" });
  const [userId, setUserId] = useState("alice");
  const [nestUrl, setNestUrl] = useState(
    () => readStore<string>(SERVER_KEY) ?? "http://127.0.0.1:3000",
  );
  const [wtUrl, setWtUrl] = useState(
    () => readStore<string>(WT_KEY) ?? "https://127.0.0.1:4434/",
  );
  const [certHash, setCertHash] = useState(
    () => readStore<string>(HASH_KEY) ?? "",
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (session) {
    return (
      <section>
        <p>
          Signed in as <strong>{session.userId}</strong>.
        </p>
        <Link to="/rooms">Go to rooms →</Link>
        <p>
          <button
            type="button"
            onClick={() => {
              localStorage.removeItem(SESSION_KEY);
              window.dispatchEvent(new Event("storage"));
            }}
          >
            Sign out
          </button>
        </p>
      </section>
    );
  }

  const onLogin = async () => {
    setBusy(true);
    setError(null);
    try {
      writeStore(SERVER_KEY, nestUrl);
      writeStore(WT_KEY, wtUrl);
      writeStore(HASH_KEY, certHash);
      const session = await login(nestUrl, userId);
      writeStore(SESSION_KEY, session);
      window.dispatchEvent(new Event("storage"));
      navigate({ to: "/rooms" });
    } catch (e) {
      setError(e instanceof NestApiError ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section>
      <h2>Sign in</h2>
      <p>
        <label>
          Nest URL{" "}
          <input
            value={nestUrl}
            onChange={(e) => setNestUrl(e.target.value)}
            size={32}
          />
        </label>
      </p>
      <p>
        <label>
          User ID{" "}
          <input value={userId} onChange={(e) => setUserId(e.target.value)} />
        </label>
      </p>
      <p>
        <label>
          WebTransport URL{" "}
          <input
            value={wtUrl}
            onChange={(e) => setWtUrl(e.target.value)}
            size={32}
          />
        </label>
      </p>
      <p>
        <label>
          WT cert hash (from rvp-cli log){" "}
          <input
            value={certHash}
            onChange={(e) => setCertHash(e.target.value)}
            size={40}
            placeholder="ab:cd:ef:..."
          />
        </label>
      </p>
      <button type="button" disabled={busy} onClick={() => void onLogin()}>
        {busy ? "Signing in…" : "Sign in"}
      </button>
      {error && (
        <p style={{ color: "crimson" }} role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

function RoomsPage() {
  const session = useSession();
  const nestUrl = () => readStore<string>(SERVER_KEY) ?? "http://127.0.0.1:3000";
  const [name, setName] = useState("demo-room");
  const [inviteUserId, setInviteUserId] = useState("bob");
  const [lastRoom, setLastRoom] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (!session) {
    return (
      <p>
        <Link to="/">Sign in first →</Link>
      </p>
    );
  }

  const onCreate = async () => {
    setError(null);
    try {
      const room = await createRoom(nestUrl(), session.accessToken, name);
      setLastRoom(room.id);
      // Creator joins as host so the grant path is complete for the demo.
      await addMember(nestUrl(), session.accessToken, room.id, session.userId);
    } catch (e) {
      setError(e instanceof NestApiError ? e.message : String(e));
    }
  };

  const onInvite = async () => {
    if (!lastRoom) return;
    setError(null);
    try {
      await addMember(nestUrl(), session.accessToken, lastRoom, inviteUserId);
    } catch (e) {
      setError(e instanceof NestApiError ? e.message : String(e));
    }
  };

  return (
    <section>
      <h2>Rooms</h2>
      <p>
        <label>
          Room name{" "}
          <input value={name} onChange={(e) => setName(e.target.value)} />
        </label>{" "}
        <button type="button" onClick={() => void onCreate()}>
          Create room
        </button>
      </p>
      {lastRoom && (
        <>
          <p>
            Created room <code>{lastRoom}</code>{" "}
            <Link to="/rooms/$roomId" params={{ roomId: lastRoom }}>
              Join →
            </Link>
          </p>
          <p>
            <label>
              Invite user{" "}
              <input
                value={inviteUserId}
                onChange={(e) => setInviteUserId(e.target.value)}
              />
            </label>{" "}
            <button type="button" onClick={() => void onInvite()}>
              Add member
            </button>
          </p>
        </>
      )}
      {error && (
        <p style={{ color: "crimson" }} role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

function RoomPage() {
  const { roomId } = roomRoute.useParams();
  const session = useSession();
  const nestUrl = () => readStore<string>(SERVER_KEY) ?? "http://127.0.0.1:3000";
  const wtUrl = () => readStore<string>(WT_KEY) ?? "https://127.0.0.1:4434/";
  const certHash = () => readStore<string>(HASH_KEY) ?? "";

  const [grant, setGrant] = useState<GrantResponse | null>(null);
  const [session_, setSession_] = useState<WtSession | null>(null);
  const [roster, setRoster] = useState<
    Array<{ participantId: number; userId: string }>
  >([]);
  const [events, setEvents] = useState<string[]>([]);
  const [rxCount, setRxCount] = useState(0);
  const [txCount, setTxCount] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!session) {
    return (
      <p>
        <Link to="/">Sign in first →</Link>
      </p>
    );
  }

  const onConnect = async () => {
    setBusy(true);
    setError(null);
    try {
      const g = await createGrant(nestUrl(), session.accessToken, roomId);
      setGrant(g);
      const s = await connectWt({
        url: wtUrl(),
        certHashHex: certHash(),
        grantJwt: g.grant,
      });
      setSession_(s);
      setRoster(s.roster);
      s.onControl((msg) => {
        setEvents((prev) => [...prev, JSON.stringify(msg)]);
        if (msg.type === "participantJoined") {
          setRoster((prev) =>
            prev.some((r) => r.participantId === msg.participantId)
              ? prev
              : [...prev, { participantId: msg.participantId, userId: msg.userId }],
          );
        } else if (msg.type === "participantLeft") {
          setRoster((prev) =>
            prev.filter((r) => r.participantId !== msg.participantId),
          );
        }
      });
      s.onData(() => setRxCount((c) => c + 1));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onPing = () => {
    if (!session_) return;
    // 12-byte dummy media frame; real audio pipeline lands in P6b.
    session_.sendDatagram(new Uint8Array([0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]));
    setTxCount((c) => c + 1);
  };

  return (
    <section>
      <h2>Room {roomId}</h2>
      {!session_ ? (
        <>
          <button type="button" disabled={busy} onClick={() => void onConnect()}>
            {busy ? "Connecting…" : "Fetch grant + connect WebTransport"}
          </button>
        </>
      ) : (
        <>
          <p>
            Participant <code>{session_.participantId}</code> · max datagram{" "}
            {session_.maxDatagram}
          </p>
          <h3>Roster</h3>
          <ul>
            {roster.map((r) => (
              <li key={r.participantId}>
                {r.userId} (pid {r.participantId})
              </li>
            ))}
            {roster.length === 0 && <li>(empty)</li>}
          </ul>
          <p>
            <button type="button" onClick={onPing}>
              Send test datagram
            </button>{" "}
            tx={txCount} rx={rxCount}
          </p>
          <h3>Control events</h3>
          <pre style={{ background: "#f4f4f4", padding: 8, overflowX: "auto" }}>
            {events.join("\n") || "(none)"}
          </pre>
          <p>
            <button
              type="button"
              onClick={() => {
                session_.close();
                setSession_(null);
              }}
            >
              Leave room
            </button>
          </p>
        </>
      )}
      {grant && (
        <p>
          <small>
            Grant expires {grant.expiresAt} · {grant.pairs.length} pair key(s)
          </small>
        </p>
      )}
      {error && (
        <p style={{ color: "crimson" }} role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

// --- entry -----------------------------------------------------------------

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  );
}
