//! mTLS from the ed25519 identity: self-signed cert via rcgen, pinned verifiers (§4, §7).
//!
//! Crypto provider: `ring` for both rustls and rcgen — one backend, supports Ed25519
//! certificates and TLS 1.3 Ed25519 handshake signatures, accepts PKCS#8 v1 seeds, and
//! needs no cmake/nasm at build time (aws-lc-rs does). TLS 1.3 only (`tls12` feature off):
//! every peer is this same binary, so there is nothing to be compatible with, and it keeps
//! the verifier surface (and this spike) smaller.
//!
//! rustls `danger` APIs used, and why:
//! - `rustls::server::danger::ClientCertVerifier` — replaces the PKI chain check with
//!   "client SPKI key ∈ allowed set"; signatures are still verified via the provider.
//! - `rustls::client::danger::ServerCertVerifier` +
//!   `ClientConfig::dangerous().with_custom_certificate_verifier` — pins the server's
//!   SPKI key instead of checking a CA chain / hostname. `expected = None` accepts any
//!   server key and must only be used by `owl add` (TOFU).

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use ed25519_dalek::VerifyingKey;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, Error, ServerConfig, SignatureScheme,
};

use crate::identity::{self, Identity};

/// Raw ed25519 public keys allowed to connect. Built from the ContactBook.
pub type AllowedKeys = HashSet<[u8; 32]>;

/// PKCS#8 v1 PrivateKeyInfo header for an Ed25519 seed (RFC 8410 §7).
const PKCS8_ED25519_HEADER: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// rcgen signing key + PKCS#8 DER for the identity's ed25519 seed.
fn keypair(id: &Identity) -> anyhow::Result<(rcgen::KeyPair, PrivatePkcs8KeyDer<'static>)> {
    let mut pkcs8 = PKCS8_ED25519_HEADER.to_vec();
    pkcs8.extend_from_slice(&id.seed());
    let key_der = PrivatePkcs8KeyDer::from(pkcs8);
    let kp = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &rcgen::PKCS_ED25519)
        .context("rcgen keypair from identity")?;
    Ok((kp, key_der))
}

/// Self-signed Ed25519 X.509 cert (CN = fingerprint) + PKCS#8 key for the identity.
pub fn cert_from_identity(
    id: &Identity,
) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let (kp, key_der) = keypair(id)?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        identity::fingerprint(&id.verifying_key()),
    );
    let cert = params
        .self_signed(&kp)
        .context("self-signing certificate")?;
    Ok((cert.der().clone(), PrivateKeyDer::Pkcs8(key_der)))
}

// DER universal tags and the id-Ed25519 OID (RFC 8410 §3) used by the SPKI walk.
const TAG_INTEGER: u8 = 0x02;
const TAG_BIT_STRING: u8 = 0x03;
const TAG_OID: u8 = 0x06;
const TAG_SEQUENCE: u8 = 0x30;
const TAG_VERSION: u8 = 0xa0; // [0] EXPLICIT
const OID_ED25519: [u8; 3] = [0x2b, 0x65, 0x70]; // 1.3.101.112

/// One DER TLV: `(tag, content, rest)`. Definite lengths only (short and long form);
/// indefinite length and lengths wider than 4 bytes are rejected. Never panics on short input.
fn tlv(input: &[u8]) -> anyhow::Result<(u8, &[u8], &[u8])> {
    let tag = *input.first().context("DER: unexpected end of input")?;
    let first = *input.get(1).context("DER: missing length")?;
    let (len, header) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let n = usize::from(first & 0x7f);
        anyhow::ensure!((1..=4).contains(&n), "DER: unsupported length encoding");
        let bytes = input.get(2..2 + n).context("DER: truncated length")?;
        let len = bytes
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (len, 2 + n)
    };
    let content = input
        .get(header..header + len)
        .context("DER: truncated element")?;
    Ok((tag, content, &input[header + len..]))
}

/// Next TLV must carry `tag`; returns `(content, rest)`.
fn expect<'a>(input: &'a [u8], tag: u8, what: &str) -> anyhow::Result<(&'a [u8], &'a [u8])> {
    let (got, content, rest) = tlv(input)?;
    anyhow::ensure!(
        got == tag,
        "DER: expected {what} (tag {tag:#04x}), got tag {got:#04x}"
    );
    Ok((content, rest))
}

/// Raw 32-byte Ed25519 public key from the certificate's SubjectPublicKeyInfo, located by
/// walking the X.509 structure (RFC 5280 §4.1): Certificate → TBSCertificate → `[0]` version →
/// serialNumber → signature → issuer → validity → subject → SubjectPublicKeyInfo. Only an SPKI
/// whose algorithm is exactly `id-Ed25519` without parameters and whose BIT STRING holds 32
/// bytes with zero unused bits is accepted. Anything found elsewhere in the DER (serial, DN
/// attributes, extensions) is never consulted, so an attacker cannot plant a victim's key there.
pub fn spki_pubkey(cert: &CertificateDer<'_>) -> anyhow::Result<[u8; 32]> {
    let (certificate, _) = expect(cert.as_ref(), TAG_SEQUENCE, "Certificate")?;
    let (tbs, _) = expect(certificate, TAG_SEQUENCE, "TBSCertificate")?;
    let (_, rest) = expect(tbs, TAG_VERSION, "TBSCertificate [0] version")?;
    let (_, rest) = expect(rest, TAG_INTEGER, "serialNumber")?;
    let (_, rest) = expect(rest, TAG_SEQUENCE, "signature AlgorithmIdentifier")?;
    let (_, rest) = expect(rest, TAG_SEQUENCE, "issuer")?;
    let (_, rest) = expect(rest, TAG_SEQUENCE, "validity")?;
    let (_, rest) = expect(rest, TAG_SEQUENCE, "subject")?;
    let (spki, _) = expect(rest, TAG_SEQUENCE, "SubjectPublicKeyInfo")?;
    let (algorithm, rest) = expect(spki, TAG_SEQUENCE, "SPKI AlgorithmIdentifier")?;
    let (oid, params) = expect(algorithm, TAG_OID, "SPKI algorithm OID")?;
    anyhow::ensure!(
        oid == OID_ED25519,
        "certificate has no Ed25519 SubjectPublicKeyInfo (algorithm OID {oid:02x?})"
    );
    anyhow::ensure!(
        params.is_empty(),
        "Ed25519 SubjectPublicKeyInfo must not carry algorithm parameters"
    );
    let (bits, _) = expect(rest, TAG_BIT_STRING, "SPKI subjectPublicKey")?;
    let (unused, key) = bits
        .split_first()
        .context("SPKI subjectPublicKey BIT STRING is empty")?;
    anyhow::ensure!(
        *unused == 0,
        "SPKI subjectPublicKey BIT STRING has {unused} unused bits, expected 0"
    );
    key.try_into()
        .map_err(|_| anyhow::anyhow!("Ed25519 public key must be 32 bytes, got {}", key.len()))
}

pub fn peer_fingerprint_from_cert(cert: &CertificateDer<'_>) -> anyhow::Result<String> {
    let key = VerifyingKey::from_bytes(&spki_pubkey(cert)?).context("invalid ed25519 key")?;
    Ok(identity::fingerprint(&key))
}

fn rejected() -> Error {
    Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
}

#[derive(Debug)]
struct PinnedClientVerifier {
    allowed: AllowedKeys,
    allow_unpinned: bool,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn offer_client_auth(&self) -> bool {
        true
    }
    fn client_auth_mandatory(&self) -> bool {
        !self.allow_unpinned
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        match spki_pubkey(end_entity) {
            Ok(k) if self.allowed.contains(&k) => Ok(ClientCertVerified::assertion()),
            _ => Err(rejected()),
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct PinnedServerVerifier {
    expected: Option<[u8; 32]>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let key = spki_pubkey(end_entity).map_err(|_| rejected())?;
        match self.expected {
            Some(e) if e != key => Err(rejected()),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Server accepting clients whose cert key is in `allowed`; with `allow_unpinned_for_card`
/// also clients presenting no certificate (card routes only — handlers must check).
pub fn server_config(
    id: &Identity,
    allowed: AllowedKeys,
    allow_unpinned_for_card: bool,
) -> anyhow::Result<Arc<ServerConfig>> {
    let (cert, key) = cert_from_identity(id)?;
    let provider = provider();
    let verifier = PinnedClientVerifier {
        allowed,
        allow_unpinned: allow_unpinned_for_card,
        provider: provider.clone(),
    };
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec![cert], key)?;
    Ok(Arc::new(cfg))
}

/// Client pinned to `expected` server key (`None` = TOFU, `owl add` only). `id = None`
/// presents no certificate (card fetch from a peer that does not know us yet).
pub fn client_config(
    id: Option<&Identity>,
    expected: Option<[u8; 32]>,
) -> anyhow::Result<Arc<ClientConfig>> {
    let provider = provider();
    let verifier = PinnedServerVerifier {
        expected,
        provider: provider.clone(),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let cfg = match id {
        Some(id) => {
            let (cert, key) = cert_from_identity(id)?;
            builder.with_client_auth_cert(vec![cert], key)?
        }
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SubjectPublicKeyInfo header for Ed25519: SEQ(42) { SEQ(5) { OID 1.3.101.112 }, BIT STRING(33) 0x00 }.
    const ED25519_SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];

    /// The old scanner: first occurrence of the SPKI header anywhere in the DER.
    /// Kept only to prove the forged fixtures would have fooled it.
    fn old_scan(der: &[u8]) -> Option<[u8; 32]> {
        let at = der
            .windows(ED25519_SPKI_PREFIX.len())
            .position(|w| w == ED25519_SPKI_PREFIX)?;
        let start = at + ED25519_SPKI_PREFIX.len();
        der.get(start..start + 32)?.try_into().ok()
    }

    fn pubkey(id: &Identity) -> [u8; 32] {
        *id.verifying_key().as_bytes()
    }

    /// `<SPKI header><victim pubkey>`: the pattern an attacker plants before the real SPKI.
    fn victim_pattern(victim: &Identity) -> Vec<u8> {
        let mut p = ED25519_SPKI_PREFIX.to_vec();
        p.extend_from_slice(&pubkey(victim));
        p
    }

    enum Forge {
        Serial,
        CommonName,
    }

    /// Certificate signed by `attacker` whose serial number (or CN) is the victim pattern.
    fn forged_cert(attacker: &Identity, victim: &Identity, how: Forge) -> CertificateDer<'static> {
        let (kp, _) = keypair(attacker).unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        let pattern = victim_pattern(victim);
        match how {
            Forge::Serial => params.serial_number = Some(rcgen::SerialNumber::from(pattern)),
            Forge::CommonName => {
                // BMPString content is raw UTF-16BE, so the DER bytes are exactly the pattern.
                let cn = rcgen::string::BmpString::from_utf16be(pattern).unwrap();
                params
                    .distinguished_name
                    .push(rcgen::DnType::CommonName, rcgen::DnValue::BmpString(cn));
            }
        }
        params.self_signed(&kp).unwrap().der().clone()
    }

    /// Victim whose 32-byte key is valid UTF-16BE (no surrogates), so it fits a BMPString CN.
    fn victim() -> Identity {
        let v = Identity::from_seed([7u8; 32]);
        assert!(rcgen::string::BmpString::from_utf16be(victim_pattern(&v)).is_ok());
        v
    }

    fn assert_forgery_detected(
        forged: &CertificateDer<'_>,
        attacker: &Identity,
        victim: &Identity,
    ) {
        assert_eq!(
            old_scan(forged.as_ref()),
            Some(pubkey(victim)),
            "fixture must contain the victim pattern before the real SPKI"
        );
        let got = spki_pubkey(forged).unwrap();
        assert_eq!(got, pubkey(attacker), "walker must return the signing key");
        assert_ne!(got, pubkey(victim));
        assert_eq!(
            peer_fingerprint_from_cert(forged).unwrap(),
            identity::fingerprint(&attacker.verifying_key())
        );
    }

    // ---- DER builders for the negative-shape fixtures ----

    fn der(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let len = content.len();
        if len < 0x80 {
            out.push(len as u8);
        } else if len <= 0xff {
            out.extend_from_slice(&[0x81, len as u8]);
        } else {
            out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
        }
        out.extend_from_slice(content);
        out
    }

    fn spki(oid: &[u8], params: Option<&[u8]>, unused_bits: u8, key: &[u8]) -> Vec<u8> {
        let mut alg = der(TAG_OID, oid);
        if let Some(p) = params {
            alg.extend_from_slice(p);
        }
        let mut bits = vec![unused_bits];
        bits.extend_from_slice(key);
        let mut body = der(TAG_SEQUENCE, &alg);
        body.extend_from_slice(&der(TAG_BIT_STRING, &bits));
        der(TAG_SEQUENCE, &body)
    }

    /// Minimal Certificate DER around `spki`; `with_version` toggles the `[0]` field.
    fn fake_cert(spki: &[u8], with_version: bool) -> CertificateDer<'static> {
        let mut tbs = Vec::new();
        if with_version {
            tbs.extend_from_slice(&der(TAG_VERSION, &der(TAG_INTEGER, &[2])));
        }
        tbs.extend_from_slice(&der(TAG_INTEGER, &[1])); // serialNumber
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519))); // signature
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[])); // issuer
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[])); // validity
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[])); // subject
        tbs.extend_from_slice(spki);
        let mut cert = der(TAG_SEQUENCE, &tbs);
        cert.extend_from_slice(&der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519)));
        cert.extend_from_slice(&der(TAG_BIT_STRING, &[0u8; 65]));
        CertificateDer::from(der(TAG_SEQUENCE, &cert))
    }

    fn good_spki(key: &[u8]) -> Vec<u8> {
        spki(&OID_ED25519, None, 0, key)
    }

    #[test]
    fn spki_from_der_walk() {
        let id = Identity::from_seed([9u8; 32]);
        let (cert, _) = cert_from_identity(&id).unwrap();
        assert_eq!(spki_pubkey(&cert).unwrap(), pubkey(&id));
        assert_eq!(
            peer_fingerprint_from_cert(&cert).unwrap(),
            identity::fingerprint(&id.verifying_key())
        );
    }

    #[test]
    fn hand_built_certificate_walks_to_the_key() {
        let key = [0x42u8; 32];
        assert_eq!(
            spki_pubkey(&fake_cert(&good_spki(&key), true)).unwrap(),
            key
        );
    }

    #[test]
    fn long_form_lengths_are_walked() {
        // Padding the subject past 127 bytes forces long-form lengths on the enclosing
        // SEQUENCEs; the walker must decode them rather than misread the header.
        let key = [0x43u8; 32];
        let mut tbs = der(TAG_VERSION, &der(TAG_INTEGER, &[2]));
        tbs.extend_from_slice(&der(TAG_INTEGER, &[1]));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519)));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[]));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[]));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &der(0x0c, &[b'x'; 300])));
        tbs.extend_from_slice(&good_spki(&key));
        let mut cert = der(TAG_SEQUENCE, &tbs);
        cert.extend_from_slice(&der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519)));
        cert.extend_from_slice(&der(TAG_BIT_STRING, &[0u8; 65]));
        let cert = der(TAG_SEQUENCE, &cert);
        assert_eq!(cert[1], 0x82, "fixture must use a long-form length");
        assert_eq!(spki_pubkey(&CertificateDer::from(cert)).unwrap(), key);
    }

    #[test]
    fn embedded_victim_key_in_serial_is_rejected() {
        let (attacker, victim) = (Identity::from_seed([3u8; 32]), victim());
        let forged = forged_cert(&attacker, &victim, Forge::Serial);
        assert_forgery_detected(&forged, &attacker, &victim);
    }

    #[test]
    fn embedded_victim_key_in_common_name_is_rejected() {
        let (attacker, victim) = (Identity::from_seed([3u8; 32]), victim());
        let forged = forged_cert(&attacker, &victim, Forge::CommonName);
        assert_forgery_detected(&forged, &attacker, &victim);
    }

    #[test]
    fn pinned_server_verifier_rejects_forged_spki() {
        let (attacker, victim) = (Identity::from_seed([3u8; 32]), victim());
        let verifier = PinnedServerVerifier {
            expected: Some(pubkey(&victim)),
            provider: provider(),
        };
        let name = ServerName::try_from("owlpost").unwrap();
        let verify = |cert: &CertificateDer<'_>| {
            verifier.verify_server_cert(cert, &[], &name, &[], UnixTime::now())
        };
        for how in [Forge::Serial, Forge::CommonName] {
            let forged = forged_cert(&attacker, &victim, how);
            assert_eq!(old_scan(forged.as_ref()), Some(pubkey(&victim)));
            assert_eq!(verify(&forged).unwrap_err(), rejected());
        }
        let (genuine, _) = cert_from_identity(&victim).unwrap();
        assert!(verify(&genuine).is_ok());
    }

    #[test]
    fn pinned_client_verifier_rejects_forged_spki() {
        let (attacker, victim) = (Identity::from_seed([3u8; 32]), victim());
        let verifier = PinnedClientVerifier {
            allowed: [pubkey(&victim)].into(),
            allow_unpinned: true,
            provider: provider(),
        };
        for how in [Forge::Serial, Forge::CommonName] {
            let forged = forged_cert(&attacker, &victim, how);
            assert_eq!(
                verifier
                    .verify_client_cert(&forged, &[], UnixTime::now())
                    .unwrap_err(),
                rejected()
            );
        }
        let (genuine, _) = cert_from_identity(&victim).unwrap();
        assert!(
            verifier
                .verify_client_cert(&genuine, &[], UnixTime::now())
                .is_ok()
        );
    }

    // ---- every reject path, each differing from the good twin in one dimension ----

    fn err_of(cert: &CertificateDer<'_>) -> String {
        spki_pubkey(cert).unwrap_err().to_string()
    }

    #[test]
    fn rejects_x25519_oid() {
        let x25519 = spki(&[0x2b, 0x65, 0x6e], None, 0, &[1u8; 32]);
        let err = err_of(&fake_cert(&x25519, true));
        assert!(err.contains("no Ed25519 SubjectPublicKeyInfo"), "{err}");
    }

    #[test]
    fn rejects_spki_with_parameters() {
        let with_params = spki(&OID_ED25519, Some(&[0x05, 0x00]), 0, &[1u8; 32]);
        let err = err_of(&fake_cert(&with_params, true));
        assert!(err.contains("must not carry algorithm parameters"), "{err}");
    }

    #[test]
    fn rejects_nonzero_unused_bits() {
        let unused = spki(&OID_ED25519, None, 1, &[1u8; 32]);
        let err = err_of(&fake_cert(&unused, true));
        assert!(err.contains("1 unused bits"), "{err}");
    }

    #[test]
    fn rejects_key_length_31_and_33() {
        for n in [31usize, 33] {
            let err = err_of(&fake_cert(&good_spki(&vec![1u8; n]), true));
            assert!(err.contains(&format!("got {n}")), "{n}: {err}");
        }
    }

    #[test]
    fn rejects_empty_bit_string() {
        let mut alg = der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519));
        alg.extend_from_slice(&der(TAG_BIT_STRING, &[]));
        let err = err_of(&fake_cert(&der(TAG_SEQUENCE, &alg), true));
        assert!(err.contains("BIT STRING is empty"), "{err}");
    }

    #[test]
    fn rejects_missing_version_field() {
        let err = err_of(&fake_cert(&good_spki(&[1u8; 32]), false));
        assert!(err.contains("expected TBSCertificate [0] version"), "{err}");
    }

    #[test]
    fn rejects_truncated_certificate_at_every_length() {
        let id = Identity::from_seed([9u8; 32]);
        let (cert, _) = cert_from_identity(&id).unwrap();
        let full = cert.as_ref();
        for n in 0..full.len() {
            let cut = CertificateDer::from(full[..n].to_vec());
            assert!(
                spki_pubkey(&cut).is_err(),
                "prefix of {n} bytes must be rejected"
            );
        }
        // The same for the hand-built shape, which has a different layout.
        let fake = fake_cert(&good_spki(&[1u8; 32]), true);
        for n in 0..fake.as_ref().len() {
            let cut = CertificateDer::from(fake.as_ref()[..n].to_vec());
            assert!(
                spki_pubkey(&cut).is_err(),
                "prefix of {n} bytes must be rejected"
            );
        }
    }

    #[test]
    fn rejects_junk_and_bad_lengths() {
        let junk = CertificateDer::from(vec![0x30, 0x03, 0x02, 0x01, 0x05]);
        assert!(err_of(&junk).contains("expected TBSCertificate"));
        assert!(spki_pubkey(&CertificateDer::from(Vec::new())).is_err());
        assert!(spki_pubkey(&CertificateDer::from(vec![0x30])).is_err());
        // Indefinite length (0x80) and a 5-byte length are not DER.
        assert!(
            err_of(&CertificateDer::from(vec![0x30, 0x80, 0x00, 0x00])).contains("unsupported")
        );
        assert!(
            err_of(&CertificateDer::from(vec![0x30, 0x85, 0, 0, 0, 0, 1])).contains("unsupported")
        );
        // Long-form length claiming more than is present.
        assert!(
            err_of(&CertificateDer::from(vec![0x30, 0x82, 0xff, 0xff, 0x30])).contains("truncated")
        );
        assert!(err_of(&CertificateDer::from(vec![0x30, 0x82, 0xff])).contains("truncated length"));
        // A bare SPKI is not a certificate: the walker must not accept it standalone.
        assert!(spki_pubkey(&CertificateDer::from(good_spki(&[1u8; 32]))).is_err());
    }

    #[test]
    fn spki_planted_only_in_serial_is_not_found() {
        // The real SPKI is X25519; the only Ed25519 pattern sits in the serial. The old scan
        // returned it; the walker must reject the certificate outright.
        let victim = victim();
        let mut tbs = der(TAG_VERSION, &der(TAG_INTEGER, &[2]));
        tbs.extend_from_slice(&der(TAG_INTEGER, &victim_pattern(&victim)));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519)));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[]));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[]));
        tbs.extend_from_slice(&der(TAG_SEQUENCE, &[]));
        tbs.extend_from_slice(&spki(&[0x2b, 0x65, 0x6e], None, 0, &[1u8; 32]));
        let mut cert = der(TAG_SEQUENCE, &tbs);
        cert.extend_from_slice(&der(TAG_SEQUENCE, &der(TAG_OID, &OID_ED25519)));
        cert.extend_from_slice(&der(TAG_BIT_STRING, &[0u8; 65]));
        let cert = CertificateDer::from(der(TAG_SEQUENCE, &cert));
        assert_eq!(old_scan(cert.as_ref()), Some(pubkey(&victim)));
        assert!(spki_pubkey(&cert).is_err());
    }
}
