//! P4a test harness: starts an in-process `rvp-server` on a random port and
//! mints EdDSA grants for the requested users (default `alice bob`).
//!
//! Prints one JSON line to stdout and serves until killed:
//! `{"port": u16, "grants": {"<user>": "<jwt>", ...}}`

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;

#[derive(Serialize)]
struct Claims {
    sub: String,
    room_id: String,
    role: String,
    iss: String,
    aud: String,
    exp: u64,
    jti: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let users: Vec<String> = std::env::args().skip(1).collect();
    let users = if users.is_empty() {
        vec!["alice".to_owned(), "bob".to_owned()]
    } else {
        users
    };

    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
    let verifier = rvp_auth::Verifier::from_public_pem(key_pair.public_key_pem().as_bytes())?;
    let server = rvp_server::start_server(
        "127.0.0.1:0".parse()?,
        rvp_server::server_config_self_signed()?,
        std::sync::Arc::new(verifier),
    )
    .await?;

    let exp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 3600;
    let key = EncodingKey::from_ed_pem(key_pair.serialize_pem().as_bytes())?;
    let mut grants = serde_json::Map::new();
    for user in &users {
        let claims = Claims {
            sub: user.clone(),
            room_id: "demo".to_owned(),
            role: "host".to_owned(),
            iss: "nest".to_owned(),
            aud: "rvp".to_owned(),
            exp,
            jti: None,
        };
        let jwt = encode(&Header::new(Algorithm::EdDSA), &claims, &key)?;
        grants.insert(user.clone(), serde_json::Value::String(jwt));
    }
    let out = serde_json::json!({
        "port": server.local_addr().port(),
        "grants": serde_json::Value::Object(grants),
    });
    println!("{out}");
    std::io::stdout().flush()?;

    // Serve until the test kills us (or stdin closes).
    let _ = std::future::pending::<()>().await;
    server.shutdown().await;
    Ok(())
}
