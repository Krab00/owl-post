//! Pinned mTLS handshakes over a real TCP socket (127.0.0.1:0, sync rustls).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use owlpost::identity::{Identity, fingerprint};
use owlpost::tls::{
    AllowedKeys, cert_from_identity, client_config, peer_fingerprint_from_cert, server_config,
};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, ServerConnection, StreamOwned};

const TIMEOUT: Duration = Duration::from_secs(3);
const REJECTED: &str = "invalid peer certificate: ApplicationVerificationFailure";

fn id(n: u8) -> Identity {
    Identity::from_seed([n; 32])
}
fn key(id: &Identity) -> [u8; 32] {
    *id.verifying_key().as_bytes()
}

/// Server thread: accept one connection, complete the handshake, write the client's
/// fingerprint (or "-" when no cert) back, return Ok(()) or the handshake error.
fn spawn_server(cfg: Arc<rustls::ServerConfig>) -> (u16, JoinHandle<Result<(), String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (tcp, _) = listener.accept().map_err(|e| e.to_string())?;
        tcp.set_read_timeout(Some(TIMEOUT)).unwrap();
        let conn = ServerConnection::new(cfg).map_err(|e| e.to_string())?;
        let mut s = StreamOwned::new(conn, tcp);
        s.conn.complete_io(&mut s.sock).map_err(|e| e.to_string())?;
        let fp = match s.conn.peer_certificates() {
            Some([cert, ..]) => peer_fingerprint_from_cert(cert).map_err(|e| e.to_string())?,
            _ => "-".to_string(),
        };
        s.write_all(fp.as_bytes()).map_err(|e| e.to_string())?;
        s.conn.send_close_notify();
        let _ = s.flush();
        Ok(())
    });
    (port, h)
}

/// Client: handshake, then read whatever the server sent. Err on handshake failure.
fn connect(port: u16, cfg: Arc<rustls::ClientConfig>) -> Result<String, String> {
    let tcp = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(TIMEOUT)).unwrap();
    let name = ServerName::try_from("owlpost").unwrap();
    let conn = ClientConnection::new(cfg, name).map_err(|e| e.to_string())?;
    let mut s = StreamOwned::new(conn, tcp);
    s.conn.complete_io(&mut s.sock).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn book(ids: &[&Identity]) -> AllowedKeys {
    ids.iter().map(|i| key(i)).collect()
}

#[test]
fn pinned_handshake_succeeds() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let got = connect(port, client_config(Some(&a), Some(key(&b))).unwrap()).unwrap();
    assert_eq!(got, fingerprint(&a.verifying_key()));
    srv.join().unwrap().unwrap();
}

#[test]
fn unknown_client_is_refused() {
    let (a, b, c) = (id(1), id(2), id(3));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let res = connect(port, client_config(Some(&c), Some(key(&b))).unwrap());
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(res.is_err(), "client should not get data: {res:?}");
    assert_eq!(server_err, REJECTED);
}

#[test]
fn wrong_server_key_is_refused() {
    let (a, b, c) = (id(1), id(2), id(3));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let err = connect(port, client_config(Some(&a), Some(key(&c))).unwrap()).unwrap_err();
    assert_eq!(err, REJECTED);
    assert!(srv.join().unwrap().is_err());
}

#[test]
fn unpinned_client_allowed_when_enabled() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), true).unwrap());
    let got = connect(port, client_config(None, Some(key(&b))).unwrap()).unwrap();
    assert_eq!(got, "-", "server must see no client certificate");
    srv.join().unwrap().unwrap();

    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let res = connect(port, client_config(None, Some(key(&b))).unwrap());
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(
        res.is_err(),
        "flag=false must refuse a cert-less client: {res:?}"
    );
    assert_eq!(server_err, "peer sent no certificates");
}

#[test]
fn unpinned_mode_still_identifies_known_client() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), true).unwrap());
    let got = connect(port, client_config(Some(&a), Some(key(&b))).unwrap()).unwrap();
    assert_eq!(
        got,
        fingerprint(&a.verifying_key()),
        "known client must be identified even with the flag"
    );
    srv.join().unwrap().unwrap();
}

#[test]
fn unpinned_mode_still_refuses_unknown_client_cert() {
    let (a, b, c) = (id(1), id(2), id(3));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), true).unwrap());
    let res = connect(port, client_config(Some(&c), Some(key(&b))).unwrap());
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(
        res.is_err(),
        "unknown cert must be refused even with the flag: {res:?}"
    );
    assert_eq!(server_err, REJECTED);
}

#[test]
fn near_miss_client_key_is_refused() {
    let (a, b) = (id(1), id(2));
    let mut almost_a = key(&a);
    almost_a[31] ^= 0x01;
    let (port, srv) = spawn_server(server_config(&b, [almost_a].into(), false).unwrap());
    let res = connect(port, client_config(Some(&a), Some(key(&b))).unwrap());
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(res.is_err(), "{res:?}");
    assert_eq!(server_err, REJECTED);
}

#[test]
fn near_miss_server_key_is_refused() {
    let (a, b) = (id(1), id(2));
    let mut almost_b = key(&b);
    almost_b[31] ^= 0x01;
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let err = connect(port, client_config(Some(&a), Some(almost_b)).unwrap()).unwrap_err();
    assert_eq!(err, REJECTED);
    assert!(srv.join().unwrap().is_err());
}

/// Server presenting a P-256 (non-Ed25519) cert; the TOFU client must still refuse it.
#[test]
fn tofu_client_refuses_non_ed25519_server() {
    let a = id(1);
    let kp = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = rcgen::CertificateParams::new(vec!["owlpost".to_string()])
        .unwrap()
        .self_signed(&kp)
        .unwrap();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(kp.serialize_der().into());
    let cfg = rustls::ServerConfig::builder_with_provider(ring())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap();
    let (port, srv) = spawn_server(Arc::new(cfg));
    let err = connect(port, client_config(Some(&a), None).unwrap()).unwrap_err();
    assert_eq!(err, REJECTED);
    assert!(srv.join().unwrap().is_err());
}

/// Client presents A's certificate but signs the handshake with C's key.
#[test]
fn forged_handshake_signature_is_refused() {
    let (a, b, c) = (id(1), id(2), id(3));
    let (cert_a, _) = cert_from_identity(&a).unwrap();
    let (_, key_c) = cert_from_identity(&c).unwrap();
    let signer_c = rustls::crypto::ring::sign::any_supported_type(&key_c).unwrap();
    let forged = Arc::new(rustls::sign::CertifiedKey::new(vec![cert_a], signer_c));
    let cfg = rustls::ClientConfig::builder_with_provider(ring())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServer))
        .with_client_cert_resolver(Arc::new(Forged(forged)));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let res = connect(port, Arc::new(cfg));
    let server_err = srv.join().unwrap().unwrap_err();
    assert!(res.is_err(), "{res:?}");
    assert_eq!(server_err, "invalid peer certificate: BadSignature");
}

fn ring() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct Forged(Arc<rustls::sign::CertifiedKey>);

impl rustls::client::ResolvesClientCert for Forged {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
    fn has_certs(&self) -> bool {
        true
    }
}

/// Test-only server verifier that accepts anything (the forged test targets the server side).
#[derive(Debug)]
struct AcceptAnyServer;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        ring().signature_verification_algorithms.supported_schemes()
    }
}

#[test]
fn tofu_client_accepts_any_server_key() {
    let (a, b) = (id(1), id(2));
    let (port, srv) = spawn_server(server_config(&b, book(&[&a]), false).unwrap());
    let got = connect(port, client_config(Some(&a), None).unwrap()).unwrap();
    assert_eq!(got, fingerprint(&a.verifying_key()));
    srv.join().unwrap().unwrap();
}

// ---- A victim's key planted in the serial / CN must not identify the peer ----

/// SubjectPublicKeyInfo header for Ed25519: SEQ(42) { SEQ(5) { OID 1.3.101.112 }, BIT STRING(33) 0x00 }.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The old scanner (first SPKI header anywhere in the DER), kept to prove the
/// forged fixtures really carry the victim's key ahead of the attacker's SPKI.
fn old_scan(der: &[u8]) -> Option<[u8; 32]> {
    let at = der
        .windows(ED25519_SPKI_PREFIX.len())
        .position(|w| w == ED25519_SPKI_PREFIX)?;
    der.get(at + 12..at + 44)?.try_into().ok()
}

#[derive(Clone, Copy, Debug)]
enum Forge {
    Serial,
    CommonName,
}

/// Attacker-signed certificate carrying `<SPKI header><victim pubkey>` in the serial number or
/// the CommonName (as a BMPString, whose DER content is the raw pattern), plus the attacker's key.
fn forged(
    attacker: &Identity,
    victim: &Identity,
    how: Forge,
) -> (
    rustls::pki_types::CertificateDer<'static>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let (_, key) = cert_from_identity(attacker).unwrap();
    let rustls::pki_types::PrivateKeyDer::Pkcs8(pkcs8) = &key else {
        panic!("cert_from_identity returns PKCS#8")
    };
    let kp = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(pkcs8, &rcgen::PKCS_ED25519).unwrap();
    let mut pattern = ED25519_SPKI_PREFIX.to_vec();
    pattern.extend_from_slice(&key_of(victim));
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    match how {
        Forge::Serial => params.serial_number = Some(rcgen::SerialNumber::from(pattern)),
        Forge::CommonName => {
            let cn = rcgen::string::BmpString::from_utf16be(pattern).unwrap();
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, rcgen::DnValue::BmpString(cn));
        }
    }
    let cert = params.self_signed(&kp).unwrap().der().clone();
    assert_eq!(
        old_scan(cert.as_ref()),
        Some(key_of(victim)),
        "{how:?}: fixture must carry the victim pattern before the real SPKI"
    );
    (cert, key)
}

fn key_of(id: &Identity) -> [u8; 32] {
    key(id)
}

/// Victim seed whose key bytes are valid UTF-16BE (no surrogates), so the CN variant encodes.
fn victim() -> Identity {
    id(7)
}

#[test]
fn forged_spki_certificate_fails_handshake() {
    let (attacker, victim, daemon) = (id(3), victim(), id(2));
    for how in [Forge::Serial, Forge::CommonName] {
        let (cert, key) = forged(&attacker, &victim, how);
        let cfg = rustls::ClientConfig::builder_with_provider(ring())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServer))
            .with_client_auth_cert(vec![cert], key)
            .unwrap();
        // Daemon lists only the victim; pinned mode.
        let (port, srv) = spawn_server(server_config(&daemon, book(&[&victim]), false).unwrap());
        let res = connect(port, Arc::new(cfg));
        let server_err = srv.join().unwrap().unwrap_err();
        assert!(
            res.is_err(),
            "{how:?}: forged client must get no data: {res:?}"
        );
        assert_eq!(server_err, REJECTED, "{how:?}");
    }
    // The genuine victim still connects and is identified as itself.
    let (port, srv) = spawn_server(server_config(&daemon, book(&[&victim]), false).unwrap());
    let got = connect(
        port,
        client_config(Some(&victim), Some(key(&daemon))).unwrap(),
    )
    .unwrap();
    assert_eq!(got, fingerprint(&victim.verifying_key()));
    srv.join().unwrap().unwrap();
}

/// Server side of the same forgery: a client pinned to the victim's key must refuse a server
/// presenting the attacker-signed certificate with the victim pattern planted.
#[test]
fn forged_spki_server_certificate_fails_handshake() {
    let (attacker, victim, client) = (id(3), victim(), id(1));
    for how in [Forge::Serial, Forge::CommonName] {
        let (cert, key) = forged(&attacker, &victim, how);
        let cfg = rustls::ServerConfig::builder_with_provider(ring())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        let (port, srv) = spawn_server(Arc::new(cfg));
        let err = connect(
            port,
            client_config(Some(&client), Some(key_of(&victim))).unwrap(),
        )
        .unwrap_err();
        assert_eq!(err, REJECTED, "{how:?}");
        assert!(srv.join().unwrap().is_err(), "{how:?}");
    }
    let (port, srv) = spawn_server(server_config(&victim, book(&[&client]), false).unwrap());
    let got = connect(
        port,
        client_config(Some(&client), Some(key_of(&victim))).unwrap(),
    )
    .unwrap();
    assert_eq!(got, fingerprint(&client.verifying_key()));
    srv.join().unwrap().unwrap();
}
