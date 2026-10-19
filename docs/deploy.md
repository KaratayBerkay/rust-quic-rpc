# Deployment runbook — one server, Nginx Proxy Manager

Single-node production deploy: Postgres + Nest (control plane), `rvp-cli`
(media), Nginx Proxy Manager (TLS + SPA + WebSocket), phone APK + browser as
clients. Commands assume the repo cloned at `/opt/rvp/repo` on a Linux host
with Docker, Node 20+, and the Rust toolchain.

## Topology

| Service | Bind | Reachable how |
|---|---|---|
| Nginx Proxy Manager | 80/443 (Docker) | public TCP — SPA, `/auth`,`/rooms` → Nest, `/ws/` → WS relay |
| NPM admin UI | `127.0.0.1:81` (Docker) | SSH tunnel only (Docker publishes bypass ufw) |
| SPA (`nginx:alpine`) | internal Docker network | via NPM only |
| `rvp-cli` `--listen` | `0.0.0.0:4433` | **public UDP** — Flutter QUIC (direct, NPM never touches UDP) |
| `rvp-cli` `--wt-listen` | `0.0.0.0:4434` | optional public UDP — browser WebTransport (cert-hash pinned) |
| `rvp-cli` `--ws-listen` | `127.0.0.1:4435` | via NPM `wss://…/ws/` (P9a fallback) |
| `rvp-cli` `--metrics-listen` | `127.0.0.1:9464` | local (or NPM location + IP allow) |
| Nest | `0.0.0.0:3000` | via NPM `/auth`, `/rooms` only (firewall) |
| Postgres | `127.0.0.1:5432` | local only |

## 1. Build

```bash
cd /opt/rvp/repo
cargo build --release -p rvp-cli

cd apps/web && npm ci && npm run build        # → apps/web/dist
cd ../../services/nest && npm ci && npm run build   # → dist/
```

Phone APK (any dev machine):

```bash
cd apps/flutter && flutter build apk --release
# → build/app/outputs/flutter-apk/app-release.apk
```

## 2. Postgres

```bash
cd /opt/rvp/repo
# PRODUCTION: the compose publishes 5432 on all interfaces (Docker bypasses
# ufw). Edit services/nest/docker-compose.yml first:
#   ports: ['127.0.0.1:5432:5432']
docker compose -f services/nest/docker-compose.yml up -d
```

Defaults in that file: user `rvp` / password `rvp` / db `rvp` — change the
password and mirror it in `DATABASE_URL`.

## 3. Nest (systemd)

`/opt/rvp/nest.env`:

```bash
DATABASE_URL=postgres://rvp:<pass>@127.0.0.1:5432/rvp
PORT=3000
NEST_EDDSA_SEED=<openssl rand -hex 32>   # stable: keeps JWKS valid across restarts
GRANT_TTL_SECONDS=120
# Single node: leave MEDIA_NODES / REDIS_URL unset.
```

`/etc/systemd/system/rvp-nest.service`:

```ini
[Service]
EnvironmentFile=/opt/rvp/nest.env
WorkingDirectory=/opt/rvp/repo/services/nest
ExecStart=/usr/bin/node dist/main.js
Restart=always

[Install]
WantedBy=multi-user.target
```

```bash
systemctl enable --now rvp-nest   # auto-migrates the schema at boot
```

## 4. Media server (systemd)

`/etc/systemd/system/rvp-media.service`:

```ini
[Service]
WorkingDirectory=/opt/rvp/repo
ExecStart=/opt/rvp/repo/target/release/rvp-cli \
  --listen 0.0.0.0:4433 \
  --wt-listen 0.0.0.0:4434 \
  --ws-listen 127.0.0.1:4435 \
  --metrics-listen 127.0.0.1:9464 \
  --nest-jwks http://127.0.0.1:3000/auth/jwks \
  --nest-revoked http://127.0.0.1:3000/auth/revoked
Restart=always

[Install]
WantedBy=multi-user.target
```

```bash
systemctl enable --now rvp-media
journalctl -u rvp-media -f       # note the logged WebTransport cert hash (§7)
```

The JWKS fetch requires `rvp-nest` up first; with `Restart=always` a wrong
boot order self-heals.

## 5. Nginx Proxy Manager

NPM terminates TLS and proxies HTTP/WebSocket — it **cannot proxy UDP**, so
the two QUIC doors stay on the host (§6). It also **cannot serve static
files**, so the SPA runs as a companion `nginx:alpine` container that only
NPM can reach.

### 5.1 `/opt/rvp/docker-compose.yml`

```yaml
services:
  npm:
    image: jc21/nginx-proxy-manager:latest
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
      - "127.0.0.1:81:81"   # admin UI: SSH tunnel only (Docker bypasses ufw)
    volumes:
      - ./npm/data:/data
      - ./npm/letsencrypt:/etc/letsencrypt
    extra_hosts:
      - "host.docker.internal:host-gateway"   # reach host Nest/WS (Docker 20.10+)
  spa:
    image: nginx:alpine
    restart: unless-stopped
    volumes:
      - /opt/rvp/repo/apps/web/dist:/usr/share/nginx/html:ro
      - /opt/rvp/spa-default.conf:/etc/nginx/conf.d/default.conf:ro
```

`/opt/rvp/spa-default.conf` (deep links fall back to `index.html`):

```nginx
server {
  listen 80;
  root /usr/share/nginx/html;
  index index.html;
  location / { try_files $uri /index.html; }
}
```

```bash
cd /opt/rvp && docker compose up -d
```

### 5.2 UI setup

Point a DNS **A record** (`voice.example.com`) at the server first — Let's
Encrypt HTTP-01 needs port 80 reachable.

1. Open the UI through a tunnel: `ssh -L 8181:127.0.0.1:81 user@server`,
   then `http://localhost:8181` (default `admin@example.com` / `changeme` —
   change it).
2. **Proxy Hosts → Add Proxy Host → Details:**
   - Domain Names: `voice.example.com`
   - Forward Scheme `http`, Forward Hostname `spa`, Port `80`
   - Websockets Support: on (harmless here)
   - **SSL tab**: Request a new Let's Encrypt Certificate, agree to TOS,
     Force SSL, HTTP/2 → Save.
3. **Custom Locations** on that host:

| Location | Forward to | Extras |
|---|---|---|
| `/auth` | `host.docker.internal` : `3000` | — |
| `/rooms` | `host.docker.internal` : `3000` | — |
| `/ws` | `host.docker.internal` : `4435` | Websockets Support **on** + Advanced below |

Advanced for `/ws` (idle calls must not hit the 60 s default):

```nginx
proxy_read_timeout 1h;
proxy_send_timeout 1h;
proxy_buffering off;
```

Longest-prefix matching routes `/auth/*` and `/rooms/*` to Nest and everything
else to the SPA — matches the app's URL building (`<nestUrl>/auth/login`).

### 5.3 Plain-nginx alternative

If you ever drop NPM, the equivalent server block:

```nginx
server {
  listen 443 ssl;
  http2 on;
  server_name voice.example.com;
  root /opt/rvp/repo/apps/web/dist;

  location / { try_files $uri /index.html; }
  location /auth/  { proxy_pass http://127.0.0.1:3000; proxy_set_header Host $host; }
  location /rooms/ { proxy_pass http://127.0.0.1:3000; proxy_set_header Host $host; }

  location /ws/ {
    proxy_pass http://127.0.0.1:4435/;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 1h;
    proxy_send_timeout 1h;
    proxy_buffering off;
  }
}
```

## 6. Firewall and ports

```bash
ufw allow 80,443/tcp
ufw allow 4433/udp     # phone → Flutter QUIC (required)
ufw allow 4434/udp     # browser WebTransport (optional, §7)
ufw enable
```

- NPM's published TCP ports ride Docker's own iptables rules — 80/443 are
  intended to be public; **81 is bound to loopback** precisely because Docker
  bypasses ufw.
- Behind a home router / CGNAT: forward **UDP 4433** (and 4434) to this host.
  No inbound UDP possible → only the browser's WSS path survives; Flutter
  then cannot call (P9b would fix that).
- LAN-only deployment (no domain): skip NPM TLS, set browser WS URL
  `ws://<server-ip>/ws/` and Nest URL `http://<server-ip>:3000` — never
  expose these beyond your LAN (§10).

## 7. Client configuration

**Browser** (gear icon; values stored in `localStorage`):

| Setting | Value |
|---|---|
| Nest URL | `https://voice.example.com` |
| WS URL | `wss://voice.example.com/ws/` |
| WT URL | `https://<server-ip>:4434/` |
| WT cert hash | colon-hex SHA-256 from the `rvp-cli` boot log line *"webtransport endpoint up"* |

- With the hash set, the browser speaks WebTransport over UDP 4434
  (self-signed cert is pinned via `serverCertificateHashes`; the cert is
  regenerated on every `rvp-cli` restart — re-paste the hash then, or leave
  WT unset and let it **auto-fall back to WSS**, which needs no hash).

**Phone APK:**

| Field | Value |
|---|---|
| Nest URL | `https://voice.example.com` |
| User ID | e.g. `phone1` |
| Media address (QUIC) | `<server-ip>:4433` |
| Room id | from Create room / shown by the peer |

Flutter uses `connectInsecure` — the media server's self-signed certificate is
accepted by design; QUIC goes direct to UDP 4433 regardless of NPM.

## 8. Place a test call

1. Browser → **Create room** → note the id → **Add member** `phone1`.
2. Phone → Connect with user `phone1` + that room id.
3. Browser → **Connect**. Both hear each other.

Order matters: grants snapshot membership when fetched.

## 9. Verify

```bash
curl -s https://voice.example.com/auth/jwks            # {"keys":[…]}
curl -s -X POST https://voice.example.com/auth/login \
  -H 'content-type: application/json' -d '{"userId":"bob"}'
docker compose -f /opt/rvp/docker-compose.yml ps       # npm + spa Up
systemctl status rvp-nest rvp-media                    # active (running)
ss -ulpn | grep 4433                                   # rvp-cli on UDP 4433
```

From the browser devtools: the call connects over `wss://…/ws/` (or WT when
the hash is set); the phone log shows a QUIC session to `:4433`.

## 10. Caveats

- **Grants live 120 s** (`GRANT_TTL_SECONDS`) — clients mint right before
  connecting, so this is fine; raise it if clients pre-fetch and wait.
- **`POST /auth/login` has no password** — anyone who can reach the API can
  claim a user id. Until real auth exists, restrict `/auth` and `/rooms`:
  NPM → Access Control list, or Advanced `allow <your-ip>; deny all;`.
- **8 connected participants per room** (`crates/rvp-server/src/rooms.rs`).
- Nest listens on all interfaces — the firewall (not Nest) keeps `:3000`
  private; same for the compose-published Postgres port.
- Multi-instance (Redis, `MEDIA_NODES` pinning) is a separate scale-out step;
  single box needs neither.
