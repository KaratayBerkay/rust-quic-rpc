//! `rvp-server` binary (config load, run media server).
//!
//! Grant verification key material (plan §5):
//! - `--jwt-public-key <pem>` — static NestJS Ed25519 public key (PEM/SPKI), or
//! - `--nest-jwks <url>` — NestJS `GET /auth/jwks` endpoint; fetched at boot
//!   and refreshed every 5 minutes so signing-key rotation needs no restart.
//!
//! Grant revocation (plan §6, P7c decision 5):
//! - `--nest-revoked <url>` — NestJS `GET /auth/revoked` denylist
//!   (`{"jtis":[...]}`), polled every 30s: newly revoked jtis reject joins
//!   with 403 and kick the live sessions holding them.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tracing::{info, warn};

const JWKS_REFRESH: Duration = Duration::from_secs(300);
const REVOKED_POLL: Duration = Duration::from_secs(30);

#[derive(Parser, Debug)]
#[command(
    name = "rvp-server",
    version,
    about = "RVP QUIC media server (JWT handshake, rooms, SFU fanout)"
)]
#[command(group(clap::ArgGroup::new("auth_key").required(true).multiple(false)))]
struct Args {
    /// UDP listen address.
    #[arg(long, default_value = "127.0.0.1:4433")]
    listen: SocketAddr,
    /// WebTransport/HTTP3 listen address (same JWT grants + rooms as --listen).
    #[arg(long)]
    wt_listen: Option<SocketAddr>,
    /// Prometheus /metrics listen address (plan §9, P7a).
    #[arg(long)]
    metrics_listen: Option<SocketAddr>,
    /// NestJS Ed25519 media-grant public key (PEM, SPKI).
    #[arg(long, group = "auth_key")]
    jwt_public_key: Option<PathBuf>,
    /// NestJS JWKS endpoint (e.g. http://127.0.0.1:3000/auth/jwks).
    #[arg(long, group = "auth_key")]
    nest_jwks: Option<String>,
    /// NestJS revoked-grant denylist endpoint (e.g. http://127.0.0.1:3000/auth/revoked);
    /// polled every 30s so kicks reach sessions already in a room (P7c).
    #[arg(long)]
    nest_revoked: Option<String>,
    /// Sustained ingress ceiling in packets/s (load-test override, plan §9/P7d).
    #[arg(long)]
    ingress_pps: Option<f64>,
    /// Sustained ingress ceiling in bytes/s (load-test override, plan §9/P7d).
    #[arg(long)]
    ingress_bps: Option<f64>,
    /// Concurrent connections allowed per source IP (load-test override).
    #[arg(long)]
    max_conn_per_ip: Option<usize>,
    /// Concurrent sessions allowed per user (grant subject; load-test override).
    #[arg(long)]
    max_conn_per_user: Option<usize>,
    /// Disable the QUIC Retry handshake (avoid latency noise in benchmarks).
    #[arg(long)]
    no_retry: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,quinn=info".into()),
        )
        .init();

    let args = Args::parse();
    let metrics = match args.metrics_listen {
        Some(addr) => Some(rvp_metrics::install(addr).await?),
        None => None,
    };
    let verifier = Arc::new(match (&args.jwt_public_key, &args.nest_jwks) {
        (Some(pem_path), _) => {
            let pem = std::fs::read(pem_path)?;
            info!(path = %pem_path.display(), "grant verification: static PEM key");
            rvp_auth::Verifier::from_public_pem(&pem)?
        }
        (_, Some(url)) => {
            let jwks = fetch_jwks(url).await.map_err(std::io::Error::other)?;
            info!(url = %url, "grant verification: JWKS from NestJS");
            rvp_auth::Verifier::from_jwks(&jwks)?
        }
        _ => unreachable!("clap requires exactly one of --jwt-public-key/--nest-jwks"),
    });
    if let Some(url) = &args.nest_jwks {
        spawn_jwks_refresh(Arc::clone(&verifier), url.clone());
    }

    // Load-test overrides (plan §9/P7d): raise caps so a local benchmark is not
    // throttled by the interactive-server defaults; unset flags keep defaults.
    let mut limits = rvp_server::Limits::default();
    if let Some(pps) = args.ingress_pps {
        limits.ingress_pps = pps;
    }
    if let Some(bps) = args.ingress_bps {
        limits.ingress_bps = bps;
    }
    if let Some(n) = args.max_conn_per_ip {
        limits.max_conn_per_ip = n;
    }
    if let Some(n) = args.max_conn_per_user {
        limits.max_conn_per_user = n;
    }
    if args.no_retry {
        limits.enable_retry = false;
    }
    let state = Arc::new(rvp_server::ServerState::with_limits(verifier, limits));
    if let Some(url) = &args.nest_revoked {
        spawn_revocation_poll(Arc::clone(&state), url.clone());
    }
    let server = rvp_server::start_server_with_state(
        args.listen,
        rvp_server::server_config_self_signed()?,
        state.clone(),
    )
    .await?;
    info!(addr = %server.local_addr(), "quic endpoint up");
    let wt = match args.wt_listen {
        Some(addr) => {
            let wt = rvp_server::wt::start_wt_server(addr, state).await?;
            info!(
                addr = %wt.local_addr(),
                cert_hash = %wt.cert_hash_hex(),
                "webtransport endpoint up — browsers pin this hash via \
                 serverCertificateHashes"
            );
            Some(wt)
        }
        None => None,
    };
    info!("rvp-server up — press ctrl-c to stop");
    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    if let Some(wt) = wt {
        wt.shutdown().await;
    }
    server.shutdown().await;
    if let Some(metrics) = metrics {
        metrics.shutdown().await;
    }
    Ok(())
}

/// Fetch the JWKS document over HTTP (blocking client on a worker thread).
async fn fetch_jwks(url: &str) -> Result<String, String> {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut response = ureq::get(&url)
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;
        let value: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|e| format!("GET {url}: body: {e}"))?;
        Ok(value.to_string())
    })
    .await
    .map_err(|e| format!("jwks fetch task: {e}"))?
}

/// Re-fetch the JWKS every [JWKS_REFRESH] so rotated-out keys stop verifying
/// without a media-server restart (plan §5: "support rotation").
fn spawn_jwks_refresh(verifier: Arc<rvp_auth::Verifier>, url: String) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(JWKS_REFRESH);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match fetch_jwks(&url).await {
                Ok(jwks) => match verifier.refresh_jwks(&jwks) {
                    Ok(keys) => info!(url = %url, keys, "jwks refreshed"),
                    Err(e) => warn!(url = %url, error = %e, "jwks refresh rejected"),
                },
                Err(e) => warn!(url = %url, error = %e, "jwks fetch failed"),
            }
        }
    });
}

/// Poll `GET /auth/revoked` (`{"jtis":["..."]}`) every [REVOKED_POLL] and
/// feed the denylist (plan §6, P7c decision 5). The first tick fires
/// immediately, so the list is populated at boot; `Revocation::apply` is
/// idempotent, so unchanged polls are no-ops.
fn spawn_revocation_poll(state: Arc<rvp_server::ServerState>, url: String) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REVOKED_POLL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match fetch_revoked(&url).await {
                Ok(jtis) => {
                    let newly = state.revocation.apply(jtis);
                    if newly > 0 {
                        warn!(url = %url, newly, "revoked grant jtis ingested");
                    }
                }
                Err(e) => warn!(url = %url, error = %e, "revoked list fetch failed"),
            }
        }
    });
}

/// Fetch the revoked-jti list (blocking client on a worker thread).
async fn fetch_revoked(url: &str) -> Result<Vec<String>, String> {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut response = ureq::get(&url)
            .call()
            .map_err(|e| format!("GET {url}: {e}"))?;
        let body: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|e| format!("GET {url}: body: {e}"))?;
        let Some(list) = body.get("jtis").and_then(|v| v.as_array()) else {
            return Err(format!("GET {url}: missing \"jtis\" array"));
        };
        list.iter()
            .map(|j| {
                j.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("GET {url}: non-string jti"))
            })
            .collect()
    })
    .await
    .map_err(|e| format!("revoked fetch task: {e}"))?
}
