//! P4a/P4b test harness: starts an in-process `rvp-server` on a random port,
//! mints EdDSA grants for the requested users (default `alice bob`), and
//! generates pairwise AES-128-GCM payload keys for every user pair (plan §3.2;
//! stands in for the NestJS grants/keys service until P5).
//!
//! Prints one JSON line to stdout:
//! `{"port": u16, "grants": {"<user>": "<jwt>", ...}, "pairs": [{"a","b","kid","key": hex}]}`
//!
//! Also serves the same JSON over a tiny dev HTTP endpoint (for apps that
//! cannot read stdout): `GET http://127.0.0.1:7860/state`
//! (override the address with `RVP_HARNESS_HTTP`, disable with `off`).

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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

    // Pairwise keys: one per user pair, kid unique room-wide (0 = plaintext).
    let mut pairs = Vec::new();
    let mut kid: u32 = 1;
    for i in 0..users.len() {
        for j in (i + 1)..users.len() {
            let mut key = [0u8; 16];
            getrandom::getrandom(&mut key)?;
            pairs.push(serde_json::json!({
                "a": users[i],
                "b": users[j],
                "kid": kid,
                "key": hex(&key),
            }));
            kid += 1;
        }
    }

    let state = serde_json::json!({
        "port": server.local_addr().port(),
        "grants": serde_json::Value::Object(grants),
        "pairs": pairs,
    });
    println!("{state}");
    std::io::stdout().flush()?;

    serve_http(state.to_string()).await;

    // Serve until the test kills us (or stdin closes).
    let _ = std::future::pending::<()>().await;
    server.shutdown().await;
    Ok(())
}

/// Tiny dev HTTP endpoint: `GET /state` → the harness state JSON.
async fn serve_http(body: String) {
    let addr = std::env::var("RVP_HARNESS_HTTP").unwrap_or_else(|_| "127.0.0.1:7860".into());
    if addr == "off" {
        return;
    }
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("harness: HTTP endpoint {addr} unavailable: {e}");
            return;
        }
    };
    let state = std::sync::Arc::new(body);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let mut read = 0;
                loop {
                    match sock.read(&mut buf[read..]).await {
                        Ok(0) => return,
                        Ok(n) => {
                            read += n;
                            if (read >= 4 && buf[read - 4..read] == *b"\r\n\r\n")
                                || read == buf.len()
                            {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let request = String::from_utf8_lossy(&buf[..read]);
                let requested_state = request
                    .lines()
                    .next()
                    .is_some_and(|line| line.starts_with("GET /state"));
                let (status, body) = if requested_state {
                    ("200 OK", state.as_str())
                } else {
                    ("404 Not Found", "not found")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
}
