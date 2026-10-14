//! `rvp-server` binary — Phase 1: run the QUIC datagram echo server.

use std::net::SocketAddr;

use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    name = "rvp-server",
    version,
    about = "RVP QUIC media server (P1: echo)"
)]
struct Args {
    /// UDP listen address.
    #[arg(long, default_value = "127.0.0.1:4433")]
    listen: SocketAddr,
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
    let server = rvp_server::start_echo_server(args.listen).await?;
    info!(addr = %server.local_addr(), "rvp-server up — press ctrl-c to stop");
    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    server.shutdown().await;
    Ok(())
}
