//! `rvp-loadgen` binary (plan §9, P7d): run a load scenario and print a JSON
//! report. See the crate docs for the crypto/plaintext fanout model.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use rvp_loadgen::Config;

#[derive(Parser, Debug)]
#[command(
    name = "rvp-loadgen",
    version,
    about = "RVP load generator: N quinn clients x rooms, plaintext + pairwise AES-GCM fanout"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a load scenario (in-process server by default).
    Run {
        /// External rvp-server address (requires --grant-key).
        #[arg(long)]
        server: Option<std::net::SocketAddr>,
        /// Ed25519 private key PEM used to sign grants (for --server).
        #[arg(long)]
        grant_key: Option<PathBuf>,
        /// Number of rooms.
        #[arg(long, default_value_t = 2)]
        rooms: usize,
        /// Participants per room (server caps a room at 8).
        #[arg(long, default_value_t = 3)]
        clients_per_room: usize,
        /// Measured publish window in seconds (after warm-up).
        #[arg(long, default_value_t = 5)]
        duration: u64,
        /// Warm-up seconds excluded from the report.
        #[arg(long, default_value_t = 1)]
        warmup: u64,
        /// Publisher tick in milliseconds (20 = 50 fps).
        #[arg(long, default_value_t = 20)]
        tick_ms: u64,
        /// Plaintext payload bytes per frame (>= 8; timestamp prefix included).
        #[arg(long, default_value_t = 80)]
        payload_bytes: usize,
        /// Plaintext mode: one PT_OPUS datagram per tick instead of N-1 sealed copies.
        #[arg(long)]
        plaintext: bool,
        /// Prometheus /metrics listen address for the in-process server.
        #[arg(long)]
        metrics_listen: Option<std::net::SocketAddr>,
        /// CA PEM to verify an external server (default: skip verification).
        #[arg(long)]
        ca: Option<PathBuf>,
        /// Write the JSON report to this path (also printed to stdout).
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Generate an Ed25519 keypair for external-server grant signing.
    Keygen {
        /// Output directory for grant_key.pem + grant_pub.pem.
        #[arg(long, default_value = "loadgen-keys")]
        out_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,quinn=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            server,
            grant_key,
            rooms,
            clients_per_room,
            duration,
            warmup,
            tick_ms,
            payload_bytes,
            plaintext,
            metrics_listen,
            ca,
            report: report_path,
        } => {
            let grant_key_pem = match grant_key {
                Some(path) => Some(std::fs::read_to_string(path)?),
                None => None,
            };
            let ca_pem = match ca {
                Some(path) => Some(std::fs::read_to_string(path)?),
                None => None,
            };
            let cfg = Config {
                server,
                grant_key_pem,
                rooms,
                clients_per_room,
                duration: std::time::Duration::from_secs(duration),
                warmup: std::time::Duration::from_secs(warmup),
                tick: std::time::Duration::from_millis(tick_ms),
                payload_bytes,
                crypto: !plaintext,
                metrics_listen,
                ca_pem,
            };
            let report = rvp_loadgen::run(&cfg).await?;
            let json = serde_json::to_string_pretty(&report)?;
            if let Some(path) = report_path {
                std::fs::write(&path, &json)?;
                tracing::info!(path = %path.display(), "report written");
            }
            println!("{json}");
            Ok(())
        }
        Command::Keygen { out_dir } => {
            let (key_path, pub_path) = rvp_loadgen::write_keypair(&out_dir)?;
            tracing::info!(key = %key_path.display(), pub = %pub_path.display(), "keypair written");
            println!(
                "start the server with: rvp-server --jwt-public-key {}",
                pub_path.display()
            );
            Ok(())
        }
    }
}
