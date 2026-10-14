//! `rvp-server` binary — Phase 2: JWT-authenticated QUIC media server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "rvp-server",
    version,
    about = "RVP QUIC media server (P2: JWT handshake + echo)"
)]
struct Args {
    /// UDP listen address.
    #[arg(long, default_value = "127.0.0.1:4433")]
    listen: SocketAddr,
    /// NestJS Ed25519 media-grant public key (PEM, SPKI).
    #[arg(long)]
    jwt_public_key: PathBuf,
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
    let pem = std::fs::read(&args.jwt_public_key)?;
    let verifier = Arc::new(rvp_auth::Verifier::from_public_pem(&pem)?);

    let server = rvp_server::start_server(
        args.listen,
        rvp_server::server_config_self_signed()?,
        verifier,
    )
    .await?;
    info!(addr = %server.local_addr(), "rvp-server up — press ctrl-c to stop");
    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    server.shutdown().await;
    Ok(())
}
