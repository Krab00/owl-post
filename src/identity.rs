//! ed25519 keypair at `$OWLPOST_HOME/key`, `ed25519:<b64>` pubkey encoding,
//! `owl:<16 base32>` fingerprint, sign/verify (§4).

use std::path::Path;

use anyhow::{Context, bail};
use data_encoding::{BASE32_NOPAD, BASE64_NOPAD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

const KEY_FILE: &str = "key";
const PREFIX: &str = "ed25519:";

pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    pub fn generate() -> Identity {
        Identity {
            signing: SigningKey::generate(&mut rand_core::OsRng),
        }
    }

    pub fn from_seed(seed: [u8; 32]) -> Identity {
        Identity {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    pub fn load(home: &Path) -> anyhow::Result<Identity> {
        let path = home.join(KEY_FILE);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let seed: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!("{}: expected 32 bytes, got {}", path.display(), bytes.len())
        })?;
        Ok(Identity::from_seed(seed))
    }

    /// Writes the 32-byte seed with mode 0600; refuses to overwrite.
    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let path = home.join(KEY_FILE);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow::anyhow!("key already exists at {}", path.display())
            } else {
                anyhow::Error::from(e).context(format!("creating {}", path.display()))
            }
        })?;
        f.write_all(&self.signing.to_bytes())
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// Raw 32-byte seed (for building the TLS key; never log it).
    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.signing.sign(msg)
    }
}

pub fn verify(pubkey: &VerifyingKey, msg: &[u8], sig: &Signature) -> bool {
    pubkey.verify_strict(msg, sig).is_ok()
}

pub fn pubkey_string(pubkey: &VerifyingKey) -> String {
    format!("{PREFIX}{}", BASE64_NOPAD.encode(pubkey.as_bytes()))
}

pub fn parse_pubkey(s: &str) -> anyhow::Result<VerifyingKey> {
    let bytes = decode_prefixed(s)?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("pubkey must be 32 bytes, got {}", bytes.len()))?;
    VerifyingKey::from_bytes(&arr).context("invalid ed25519 public key")
}

pub fn sig_string(sig: &Signature) -> String {
    format!("{PREFIX}{}", BASE64_NOPAD.encode(&sig.to_bytes()))
}

pub fn parse_sig(s: &str) -> anyhow::Result<Signature> {
    let bytes = decode_prefixed(s)?;
    let arr: [u8; 64] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes, got {}", bytes.len()))?;
    Ok(Signature::from_bytes(&arr))
}

fn decode_prefixed(s: &str) -> anyhow::Result<Vec<u8>> {
    let Some(b64) = s.strip_prefix(PREFIX) else {
        bail!("expected `{PREFIX}` prefix in {s:?}");
    };
    BASE64_NOPAD
        .decode(b64.as_bytes())
        .with_context(|| format!("decoding base64 in {s:?}"))
}

/// `owl:` + first 16 chars of lowercase base32 (no padding) of SHA-256(raw pubkey).
pub fn fingerprint(pubkey: &VerifyingKey) -> String {
    let digest = Sha256::digest(pubkey.as_bytes());
    let b32 = BASE32_NOPAD.encode(&digest).to_lowercase();
    format!("owl:{}", &b32[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed() -> Identity {
        Identity::from_seed([7u8; 32])
    }

    #[test]
    fn fingerprint_is_stable() {
        let fp = fingerprint(&fixed().verifying_key());
        assert_eq!(fp, "owl:72asyextvngonlc5");
        assert_eq!(fp.len(), 20);
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = fixed();
        let pk = id.verifying_key();
        let msg = b"arbitrary bytes \x00\xff";
        let sig = id.sign(msg);
        assert!(verify(&pk, msg, &sig));
        let mut bad_msg = msg.to_vec();
        bad_msg[3] ^= 0x01;
        assert!(!verify(&pk, &bad_msg, &sig));
        let mut bad_sig = sig.to_bytes();
        bad_sig[5] ^= 0x01;
        assert!(!verify(&pk, msg, &Signature::from_bytes(&bad_sig)));
        let other = Identity::from_seed([8u8; 32]).verifying_key();
        assert!(!verify(&other, msg, &sig), "different pubkey must reject");
    }

    #[test]
    fn pubkey_encoding_roundtrip() {
        let pk = fixed().verifying_key();
        let s = pubkey_string(&pk);
        assert!(s.starts_with("ed25519:"));
        assert!(!s.contains('='), "no padding: {s}");
        assert_eq!(s.len(), 8 + 43);
        assert_eq!(parse_pubkey(&s).unwrap(), pk);
        let wrong_prefix = s.replacen("ed25519:", "foo:", 1);
        assert!(parse_pubkey(&wrong_prefix).is_err());
        let same_len_prefix = s.replacen("ed25519:", "ed25518:", 1);
        assert!(
            parse_pubkey(&same_len_prefix).is_err(),
            "prefix must be checked literally"
        );
        assert!(parse_pubkey("ed25519:not*base64").is_err());
        assert!(
            parse_pubkey(&s[8..]).is_err(),
            "bare base64 must be rejected"
        );
        assert!(parse_pubkey("ed25519:AAAA").is_err(), "wrong length");
    }

    #[test]
    fn sig_encoding_roundtrip() {
        let sig = fixed().sign(b"x");
        let s = sig_string(&sig);
        assert!(s.starts_with("ed25519:"));
        assert_eq!(parse_sig(&s).unwrap(), sig);
        assert!(parse_sig("foo:AAAA").is_err());
        assert!(parse_sig(&s[8..]).is_err(), "bare base64 must be rejected");
        assert!(parse_sig(&s.replacen("ed25519:", "ed25518:", 1)).is_err());
        assert!(parse_sig("ed25519:AAAA").is_err());
    }

    #[test]
    fn save_load_roundtrip_and_refuses_overwrite() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().join("nested");
        let id = fixed();
        id.save(&home).unwrap();
        let bytes = std::fs::read(home.join("key")).unwrap();
        assert_eq!(bytes, [7u8; 32]);
        let loaded = Identity::load(&home).unwrap();
        assert_eq!(loaded.verifying_key(), id.verifying_key());
        let err = Identity::generate().save(&home).err().unwrap().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(home.join("key")).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn key_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        fixed().save(home.path()).unwrap();
        let mode = std::fs::metadata(home.path().join("key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn load_rejects_wrong_length() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("key"), [1u8; 31]).unwrap();
        let err = Identity::load(home.path()).err().unwrap().to_string();
        assert!(err.contains("32 bytes"), "{err}");
        std::fs::write(home.path().join("key"), [1u8; 33]).unwrap();
        let err = Identity::load(home.path()).err().unwrap().to_string();
        assert!(err.contains("got 33"), "{err}");
        assert!(Identity::load(&home.path().join("missing")).is_err());
    }

    #[test]
    fn generate_is_random() {
        assert_ne!(
            Identity::generate().verifying_key(),
            Identity::generate().verifying_key()
        );
    }
}
