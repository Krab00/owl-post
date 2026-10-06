//! iroh transport: the daemon's second listener and the dial path.
//!
//! A peer is reached by its ed25519 key: the daemon's iroh endpoint identity is derived from
//! the same seed as `identity::Identity`, so the endpoint id **is** the contact's `pubkey`.
//! iroh finds a direct path by hole punching and falls back to a relay. The HTTP contract
//! (§7) is unchanged: the same axum handlers are served with hyper over an iroh bi-stream
//! (ALPN `owl/1`, one HTTP/1 request per stream); only the owner's forward route is not
//! mounted here (`server::iroh_router`).
//!
//! The endpoint identity is a network singleton — the relay bumps a second registration of
//! the same key — so only the daemon binds one. The CLI reaches peers through the daemon's
//! forward route (`server::forward`).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use axum::body::Bytes;
use axum::extract::Extension;
use axum::http::{HeaderMap, Method};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use iroh::endpoint::{Connection, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey, Watcher};
use tower::Layer;

use crate::contacts::ContactBook;
use crate::identity::{self, Identity};
use crate::server::{AppState, PeerId};

/// Application protocol on the QUIC connection.
pub const ALPN: &[u8] = b"owl/1";
/// Bound on establishing a connection (relay round trip + hole punching). An offline peer
/// costs this much before the configured `endpoints` are tried.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on one request over an established connection (same ceiling as the HTTPS client).
pub const REQUEST_TIMEOUT: Duration = crate::client::REQUEST_TIMEOUT;
/// QUIC close code sent to a key that is not in the contact book.
pub const CLOSE_UNKNOWN_KEY: VarInt = VarInt::from_u32(1);
/// QUIC close code after a completed request.
const CLOSE_DONE: VarInt = VarInt::from_u32(0);

/// Parses `relay_urls` from `config.json`; every entry must be a URL.
pub fn parse_relay_urls(urls: &[String]) -> anyhow::Result<Vec<RelayUrl>> {
    urls.iter()
        .map(|u| {
            u.parse::<RelayUrl>()
                .map_err(|e| anyhow!("relay_urls entry {u:?}: {e}"))
        })
        .collect()
}

/// Binds the daemon's endpoint with `identity`'s key. `relay_urls = None` uses n0's public
/// relays and DNS discovery; `Some(urls)` uses exactly those relays and no public discovery
/// (an empty list = no relay at all: the endpoint is bound but unreachable from outside).
pub async fn endpoint(
    identity: &Identity,
    relay_urls: Option<&[String]>,
) -> anyhow::Result<Endpoint> {
    let builder = match relay_urls {
        None => Endpoint::builder(presets::N0),
        Some(urls) => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::custom(parse_relay_urls(urls)?)),
    };
    builder
        .secret_key(SecretKey::from_bytes(&identity.seed()))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .context("binding the iroh endpoint")
}

/// The home relay the endpoint is connected to, if any.
pub fn home_relay(endpoint: &Endpoint) -> Option<String> {
    endpoint
        .home_relay_status()
        .get()
        .iter()
        .find(|s| s.is_connected())
        .map(|s| s.url().to_string())
}

/// Address of the peer with `pubkey`: its key plus the local relay set (both sides are
/// assumed to use the same relays; with `None` n0's discovery finds the peer's relay).
pub fn peer_addr(pubkey: &str, relay_urls: Option<&[String]>) -> anyhow::Result<EndpointAddr> {
    let key = identity::parse_pubkey(pubkey)?;
    let id = PublicKey::from_bytes(key.as_bytes()).context("iroh endpoint id")?;
    let mut addr = EndpointAddr::new(id);
    if let Some(urls) = relay_urls {
        for url in parse_relay_urls(urls)? {
            addr = addr.with_relay_url(url);
        }
    }
    Ok(addr)
}

/// Fingerprint of the contact whose pubkey bytes equal `key` (byte comparison of the parsed
/// key; the pubkey strings themselves are never compared).
pub fn fingerprint_of_key(book: &ContactBook, key: &[u8; 32]) -> Option<String> {
    book.contacts
        .iter()
        .find(|c| identity::parse_pubkey(&c.pubkey).is_ok_and(|pk| pk.as_bytes() == key))
        .map(|c| c.fingerprint.clone())
}

/// Accepts connections until the endpoint is closed. Each connection is checked against the
/// (reloaded) contact book before any stream is accepted.
pub async fn accept_loop(endpoint: Endpoint, state: Arc<AppState>) {
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            let accepting = match incoming.accept() {
                Ok(a) => a,
                Err(e) => {
                    tracing::debug!(error = %e, "iroh: incoming refused");
                    return;
                }
            };
            match accepting.await {
                Ok(conn) => serve(conn, state).await,
                Err(e) => tracing::debug!(error = %e, "iroh: handshake failed"),
            }
        });
    }
}

/// Key check first, then hyper over every bi-stream the peer opens.
async fn serve(conn: Connection, state: Arc<AppState>) {
    let remote = conn.remote_id();
    let book = match state.contacts() {
        Ok(book) => book,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "iroh: contact book unreadable");
            conn.close(CLOSE_UNKNOWN_KEY, b"unknown key");
            return;
        }
    };
    let Some(fingerprint) = fingerprint_of_key(&book, remote.as_bytes()) else {
        tracing::warn!(remote = %remote.fmt_short(), "iroh: unknown key refused");
        conn.close(CLOSE_UNKNOWN_KEY, b"unknown key");
        return;
    };
    tracing::debug!(peer = %fingerprint, "iroh: connection accepted");
    let service = TowerToHyperService::new(
        Extension(PeerId(Some(fingerprint))).layer(crate::server::iroh_router(state)),
    );
    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            Err(e) => {
                tracing::debug!(error = %e, "iroh: connection ended");
                return;
            }
        };
        let service = service.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(tokio::io::join(recv, send));
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                tracing::debug!(error = %e, "iroh: request stream ended");
            }
        });
    }
}

/// A peer's answer to one request, whatever it was.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Dials `addr` (bounded by `DIAL_TIMEOUT`) and sends one HTTP/1 request over a fresh
/// bi-stream (bounded by `REQUEST_TIMEOUT`). A peer that refused the key surfaces as its
/// close reason (`unknown key`), never as an HTTP status.
pub async fn request(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> anyhow::Result<Reply> {
    let conn = tokio::time::timeout(DIAL_TIMEOUT, endpoint.connect(addr, ALPN))
        .await
        .map_err(|_| anyhow!("dial timeout after {}s", DIAL_TIMEOUT.as_secs()))?
        .map_err(|e| anyhow!("dial: {e}"))?;
    let result = tokio::time::timeout(
        REQUEST_TIMEOUT,
        exchange(&conn, method, path, headers, body),
    )
    .await
    .map_err(|_| anyhow!("request timeout after {}s", REQUEST_TIMEOUT.as_secs()))
    .and_then(|r| r);
    match result {
        Ok(reply) => {
            conn.close(CLOSE_DONE, b"done");
            Ok(reply)
        }
        Err(e) => match conn.close_reason() {
            Some(reason) => Err(anyhow!("connection closed by peer: {reason}")),
            None => Err(e),
        },
    }
}

async fn exchange(
    conn: &Connection,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> anyhow::Result<Reply> {
    let (send, recv) = conn.open_bi().await.context("opening stream")?;
    let io = TokioIo::new(tokio::io::join(recv, send));
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .context("http handshake")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "owl")
        .body(Full::new(body))
        .context("building request")?;
    req.headers_mut().extend(headers);
    let resp = sender.send_request(req).await.context("sending request")?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = resp
        .into_body()
        .collect()
        .await
        .context("reading response")?
        .to_bytes();
    Ok(Reply {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::Contact;

    fn contact(pubkey: &str, fingerprint: &str) -> Contact {
        Contact {
            name: String::new(),
            emails: vec![],
            pubkey: pubkey.into(),
            endpoints: vec![],
            source: "local".into(),
            policy: None,
            added_at: None,
            fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn relay_urls_parse_or_name_the_bad_entry() {
        assert!(parse_relay_urls(&[]).unwrap().is_empty());
        let urls = parse_relay_urls(&[
            "http://127.0.0.1:3340".into(),
            "https://relay.example.org/".into(),
        ])
        .unwrap();
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].to_string(), "http://127.0.0.1:3340/");
        let err = parse_relay_urls(&["https://ok.example".into(), "not a url".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"not a url\""), "{err}");
    }

    #[test]
    fn peer_addr_carries_the_key_and_the_local_relays() {
        let id = Identity::from_seed([4u8; 32]);
        let pubkey = identity::pubkey_string(&id.verifying_key());
        let addr = peer_addr(&pubkey, None).unwrap();
        assert_eq!(addr.id.as_bytes(), id.verifying_key().as_bytes());
        assert!(addr.is_empty(), "no relay without a local relay set");
        let addr = peer_addr(&pubkey, Some(&[])).unwrap();
        assert!(addr.is_empty(), "empty relay set: still only the key");
        let addr = peer_addr(
            &pubkey,
            Some(&["http://127.0.0.1:1/".into(), "http://127.0.0.1:2/".into()]),
        )
        .unwrap();
        assert_eq!(addr.relay_urls().count(), 2);
        assert!(peer_addr("garbage", None).is_err());
        assert!(peer_addr(&pubkey, Some(&["nope".into()])).is_err());
    }

    #[test]
    fn fingerprint_of_key_compares_bytes_not_strings() {
        let (a, b) = (
            Identity::from_seed([1u8; 32]),
            Identity::from_seed([2u8; 32]),
        );
        let pk_a = identity::pubkey_string(&a.verifying_key());
        let pk_b = identity::pubkey_string(&b.verifying_key());
        // Decoys: a garbage pubkey, a superstring of A's pubkey string, a key one byte off.
        let mut off = *a.verifying_key().as_bytes();
        off[31] ^= 1;
        let book = ContactBook {
            contacts: vec![
                contact("garbage", "owl:garbage"),
                contact(&format!("{pk_a}AAAA"), "owl:superstring"),
                contact(&pk_b, "owl:b"),
                contact(&pk_a, "owl:a"),
            ],
        };
        assert_eq!(
            fingerprint_of_key(&book, a.verifying_key().as_bytes()).as_deref(),
            Some("owl:a")
        );
        assert_eq!(
            fingerprint_of_key(&book, b.verifying_key().as_bytes()).as_deref(),
            Some("owl:b")
        );
        assert_eq!(
            fingerprint_of_key(&book, &off),
            None,
            "one bit off is unknown"
        );
        assert_eq!(
            fingerprint_of_key(
                &book,
                Identity::from_seed([3u8; 32]).verifying_key().as_bytes()
            ),
            None
        );
        assert_eq!(fingerprint_of_key(&ContactBook::default(), &off), None);
    }

    #[tokio::test]
    async fn endpoint_id_is_the_identity_key_and_relays_follow_config() {
        let id = Identity::from_seed([9u8; 32]);
        let ep = endpoint(&id, Some(&[])).await.unwrap();
        assert_eq!(ep.id().as_bytes(), id.verifying_key().as_bytes());
        assert_eq!(ep.addr().relay_urls().count(), 0, "empty relay set");
        assert!(
            ep.address_lookup().unwrap().is_empty(),
            "no public discovery"
        );
        assert_eq!(home_relay(&ep), None);
        ep.close().await;
        // A custom relay list: exactly that relay, still no discovery, not connected yet.
        let ep = endpoint(&id, Some(&["http://127.0.0.1:1/".into()]))
            .await
            .unwrap();
        assert!(ep.address_lookup().unwrap().is_empty());
        assert_eq!(home_relay(&ep), None, "nobody listens on port 1");
        ep.close().await;
        let err = endpoint(&id, Some(&["bad url".into()]))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"bad url\""), "{err}");
    }
}
