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
import { useState, useRef, useSyncExternalStore } from "react";
import {
  addMember,
  createGrant,
  createRoom,
  fetchPairKeys,
  login,
  NestApiError,
  rotatePairKeys,
  type GrantResponse,
  type SessionResponse,
} from "./lib/api";
import { VoicePipeline } from "./lib/voice";
import { connectWt, type WtSession } from "./lib/wt";
import { connectWs } from "./lib/ws";
import {
  createWebCodecsOpusDecoder,
  createWebCodecsOpusEncoder,
  loadWorklets,
  startCapture,
  startPlayback,
  type CaptureHandle,
  type PlaybackHandle,
} from "./audio/browser-audio";

// --- session store (localStorage-backed; fine for the P6a demo shell) ------

const SESSION_KEY = "rvp.session";
const SERVER_KEY = "rvp.nestUrl";
const WT_KEY = "rvp.wtUrl";
const HASH_KEY = "rvp.wtCertHash";
const WS_KEY = "rvp.wsUrl";

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
  const [wsUrl, setWsUrl] = useState(
    () => readStore<string>(WS_KEY) ?? "ws://127.0.0.1:4435/",
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
      writeStore(WS_KEY, wsUrl);
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
      <p>
        <label>
          WebSocket URL (UDP-blocked fallback){" "}
          <input
            value={wsUrl}
            onChange={(e) => setWsUrl(e.target.value)}
            size={32}
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
  const wsUrl = () => readStore<string>(WS_KEY) ?? "ws://127.0.0.1:4435/";

  const [grant, setGrant] = useState<GrantResponse | null>(null);
  const [session_, setSession_] = useState<WtSession | null>(null);
  const [transport, setTransport] = useState<"webtransport" | "websocket" | null>(
    null,
  );
  const [roster, setRoster] = useState<
    Array<{ participantId: number; userId: string }>
  >([]);
  const [events, setEvents] = useState<string[]>([]);
  const [voiceStats, setVoiceStats] = useState({
    framesSent: 0,
    datagramsSent: 0,
    datagramsReceived: 0,
    decryptFailures: 0,
    missingKeyDropped: 0,
  });
  const [muted, setMuted] = useState(false);
  const [audioActive, setAudioActive] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!session) {
    return (
      <p>
        <Link to="/">Sign in first →</Link>
      </p>
    );
  }

  // Refs hold the live audio objects outside React render cycles.
  const voiceRef = useRef<VoicePipeline | null>(null);
  const captureRef = useRef<CaptureHandle | null>(null);
  const playbackRef = useRef<PlaybackHandle | null>(null);
  const contextRef = useRef<AudioContext | null>(null);
  const unsubRef = useRef<(() => void) | null>(null);

  const onConnect = async () => {
    setBusy(true);
    setError(null);
    try {
      const g = await createGrant(nestUrl(), session.accessToken, roomId);
      setGrant(g);

      // P9a: WebTransport first (QUIC CC), fall back to WebSocket when the
      // browser or network blocks UDP (open decision 8 — degraded path).
      let s: WtSession;
      let used: "webtransport" | "websocket";
      try {
        s = await connectWt({
          url: wtUrl(),
          certHashHex: certHash(),
          grantJwt: g.grant,
        });
        used = "webtransport";
      } catch (wtErr) {
        const reason = wtErr instanceof Error ? wtErr.message : String(wtErr);
        setEvents((prev) => [...prev, `WebTransport unavailable: ${reason}`]);
        s = await connectWs({ url: wsUrl(), grantJwt: g.grant });
        used = "websocket";
      }
      setSession_(s);
      setTransport(used);
      setRoster(s.roster);

      // Build the pairwise key table from the grant (kid is room-wide unique).
      const pipeline = new VoicePipeline({
        sendDatagram: (d) => s.sendDatagram(d),
        createEncoder: createWebCodecsOpusEncoder,
        createDecoder: createWebCodecsOpusDecoder,
      });
      pipeline.applyPairs(g.pairs);
      pipeline.onPlaybackPcm = (frame) => {
        playbackRef.current?.enqueue(frame);
      };
      voiceRef.current = pipeline;

      // Subscribe to control events (roster + key rotation) and media datagrams.
      s.onControl((msg) => {
        setEvents((prev) => [...prev, JSON.stringify(msg)]);
        if (msg.type === "participantJoined") {
          setRoster((prev) =>
            prev.some((r) => r.participantId === msg.participantId)
              ? prev
              : [...prev, { participantId: msg.participantId, userId: msg.userId }],
          );
          // Register the new peer's pairwise key if present in the grant.
          const pair = g.pairs.find((p) => p.peer === msg.userId);
          if (pair) {
            pipeline.applyPairs([pair]);
          }
        } else if (msg.type === "participantLeft") {
          setRoster((prev) =>
            prev.filter((r) => r.participantId !== msg.participantId),
          );
        } else if (msg.type === "keyUpdate") {
          // A member rotated keys (plan §5): pull the fresh pairs from Nest
          // and swap them into the pipeline (old kids stay receivable).
          void fetchPairKeys(nestUrl(), session.accessToken, roomId)
            .then((res) => pipeline.applyPairs(res.pairs))
            .catch((e) =>
              setError(e instanceof Error ? e.message : String(e)),
            );
        }
      });
      s.onData((payload) => pipeline.receiveDatagram(payload));

      // Playout ticker.
      const unsub = pipeline.start();
      unsubRef.current = unsub;

      // Kick off audio (requires a user gesture — this is one).
      const ctx = new AudioContext({ sampleRate: 48000 });
      contextRef.current = ctx;
      await loadWorklets(ctx);
      const playback = startPlayback(ctx);
      playbackRef.current = playback;
      const capture = await startCapture(ctx, (frame) => {
        void pipeline.pushPcm(frame);
      });
      captureRef.current = capture;
      setAudioActive(true);

      // Periodic stats refresh.
      const interval = setInterval(() => {
        setVoiceStats({ ...pipeline.stats });
      }, 500);
      unsubRef.current = () => {
        unsub();
        clearInterval(interval);
      };
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onToggleMute = () => {
    const next = !muted;
    setMuted(next);
    voiceRef.current?.setMuted(next);
  };

  const onRotateKeys = async () => {
    if (!session_ || !grant) return;
    setBusy(true);
    setError(null);
    try {
      // Host-only endpoint: rotate every pair in the room (new dense kids).
      const res = await rotatePairKeys(nestUrl(), session.accessToken, roomId);
      voiceRef.current?.applyPairs(res.pairs);
      // Announce the highest new kid so other members pull fresh keys.
      const maxKid = res.pairs.reduce((m, p) => Math.max(m, p.kid), 0);
      if (maxKid > 0) {
        session_.sendControl({ type: "keyUpdate", kid: maxKid });
      }
      setEvents((prev) => [...prev, `rotated ${res.pairs.length} pair(s), announced kid=${maxKid}`]);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onLeave = () => {
    unsubRef.current?.();
    captureRef.current?.stop();
    playbackRef.current?.stop();
    voiceRef.current?.dispose();
    void contextRef.current?.close();
    session_?.close();
    setSession_(null);
    setTransport(null);
    setAudioActive(false);
  };

  return (
    <section>
      <h2>Room {roomId}</h2>
      {!session_ ? (
        <>
          <button type="button" disabled={busy} onClick={() => void onConnect()}>
            {busy ? "Connecting…" : "Fetch grant + connect + join voice"}
          </button>
        </>
      ) : (
        <>
          <p>
            Participant <code>{session_.participantId}</code> · max datagram{" "}
            {session_.maxDatagram} · transport{" "}
            <strong>{transport ?? "unknown"}</strong>
            {transport === "websocket" && (
              <small> (UDP blocked — degraded fallback, plan decision 8)</small>
            )}
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
            <button type="button" onClick={onToggleMute}>
              {muted ? "Unmute" : "Mute"}
            </button>{" "}
            {audioActive && <span>🎤 audio active</span>}
            {grant?.role === "host" && (
              <>
                {" "}
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void onRotateKeys()}
                >
                  {busy ? "Rotating…" : "Rotate keys"}
                </button>
              </>
            )}
          </p>
          <h3>Voice stats</h3>
          <pre style={{ background: "#f4f4f4", padding: 8, overflowX: "auto" }}>
            {JSON.stringify(voiceStats, null, 2)}
          </pre>
          <h3>Control events</h3>
          <pre style={{ background: "#f4f4f4", padding: 8, overflowX: "auto" }}>
            {events.join("\n") || "(none)"}
          </pre>
          <p>
            <button type="button" onClick={onLeave}>
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
