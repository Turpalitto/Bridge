//! Cryptographic device identity, secret protection, and the trust registry.
//!
//! Every device owns one permanent Ed25519 keypair (spec §10). The public
//! key is the device id and the network address (iroh's dial-by-key model).
//! The private key never leaves the device; at rest it is wrapped by a
//! platform [`SecretProtector`] (Android Keystore / Windows DPAPI in shipped
//! apps, chmod-0600 file fallback for CLI/development).
#![forbid(unsafe_code)]

pub mod protector;
pub mod trust;

pub use protector::{DirectSeedProtector, FileProtector, SecretProtector};
pub use trust::{Permission, TrustRegistry, TrustedDevice};

use iroh_base::{PublicKey, SecretKey, Signature};
use thiserror::Error;

/// 32-byte device identity = Ed25519 public key.
pub type DeviceId = [u8; 32];

/// Replay-prevention window for authentication challenges (spec §9, §13).
pub const AUTH_TIMESTAMP_WINDOW_SECS: i64 = 60;

/// Canonical format for authentication challenges (spec §9, §13).
pub fn canonical_auth_challenge(peer_id: &DeviceId, nonce: &[u8], timestamp: i64) -> Vec<u8> {
    let mut m = Vec::with_capacity(32 + nonce.len() + 32);
    m.extend_from_slice(b"dropbridge-auth/1/");
    m.extend_from_slice(peer_id);
    m.extend_from_slice(b"/");
    m.extend_from_slice(&timestamp.to_be_bytes());
    m.extend_from_slice(b"/");
    m.extend_from_slice(nonce);
    m
}

/// Derive a session key from a shared secret and context string using BLAKE3 KDF (spec §10, §22).
pub fn derive_session_key(context: &str, material: &[u8]) -> [u8; 32] {
    blake3::derive_key(context, material)
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("invalid key material")]
    InvalidKey,
    #[error("signature verification failed")]
    BadSignature,
    #[error("timestamp outside allowed replay window")]
    TimestampWindow,
    #[error("protector error: {0}")]
    Protector(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A device's permanent identity keypair.
#[derive(Clone)]
pub struct DeviceIdentity {
    secret: SecretKey,
}

impl DeviceIdentity {
    /// Generate a fresh random identity.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            secret: SecretKey::generate(),
        }
    }

    /// Restore from raw 32-byte seed (already unwrapped).
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, IdentityError> {
        Ok(Self {
            secret: SecretKey::from_bytes(&bytes),
        })
    }

    /// Raw 32-byte seed — wrap with a [`SecretProtector`] before persisting.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    /// This device's id (public key).
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        *self.secret.public().as_bytes()
    }

    /// z-base-32 rendering of the device id (for QR / diagnostics).
    #[must_use]
    pub fn device_id_z32(&self) -> String {
        self.secret.public().to_z32()
    }

    /// Parse a z-base-32 device id.
    pub fn parse_z32(s: &str) -> Result<DeviceId, IdentityError> {
        PublicKey::from_z32(s)
            .map(|k| *k.as_bytes())
            .map_err(|_| IdentityError::InvalidKey)
    }

    /// Sign an arbitrary message.
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.secret.sign(msg).to_bytes()
    }

    /// Verify a signature against a remote device id.
    pub fn verify(device: &DeviceId, msg: &[u8], sig: &[u8; 64]) -> Result<(), IdentityError> {
        let pk = PublicKey::from_bytes(device).map_err(|_| IdentityError::InvalidKey)?;
        let sig = Signature::from_bytes(sig);
        pk.verify(msg, &sig)
            .map_err(|_| IdentityError::BadSignature)
    }

    /// Sign an authentication challenge bound to the remote peer id, a nonce, and timestamp.
    pub fn sign_auth_challenge(
        &self,
        peer_id: &DeviceId,
        nonce: &[u8],
        timestamp: i64,
    ) -> [u8; 64] {
        let msg = canonical_auth_challenge(peer_id, nonce, timestamp);
        self.sign(&msg)
    }

    /// Verify an authentication challenge ensuring it falls within the replay window.
    pub fn verify_auth_challenge(
        device: &DeviceId,
        my_id: &DeviceId,
        nonce: &[u8],
        timestamp: i64,
        now: i64,
        sig: &[u8; 64],
    ) -> Result<(), IdentityError> {
        if (now - timestamp).abs() > AUTH_TIMESTAMP_WINDOW_SECS {
            return Err(IdentityError::TimestampWindow);
        }
        let msg = canonical_auth_challenge(my_id, nonce, timestamp);
        Self::verify(device, &msg, sig)
    }

    /// Borrow the underlying iroh secret key (for endpoint construction).
    #[must_use]
    pub fn secret_key(&self) -> &SecretKey {
        &self.secret
    }
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id_z32())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let a = DeviceIdentity::generate();
        let msg = b"dropbridge test message";
        let sig = a.sign(msg);
        DeviceIdentity::verify(&a.device_id(), msg, &sig).unwrap();
        // tampered message fails
        assert!(DeviceIdentity::verify(&a.device_id(), b"other", &sig).is_err());
    }

    #[test]
    fn auth_challenge_roundtrip_and_replay_protection() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let nonce = [42u8; 16];
        let now = 1_700_000_000i64;

        // Valid signature within window
        let sig = a.sign_auth_challenge(&b.device_id(), &nonce, now);
        DeviceIdentity::verify_auth_challenge(
            &a.device_id(),
            &b.device_id(),
            &nonce,
            now,
            now + 10,
            &sig,
        )
        .unwrap();

        // Expired signature outside replay window (past)
        let err = DeviceIdentity::verify_auth_challenge(
            &a.device_id(),
            &b.device_id(),
            &nonce,
            now,
            now + AUTH_TIMESTAMP_WINDOW_SECS + 5,
            &sig,
        );
        assert!(matches!(err, Err(IdentityError::TimestampWindow)));

        // Skewed signature outside replay window (future)
        let err = DeviceIdentity::verify_auth_challenge(
            &a.device_id(),
            &b.device_id(),
            &nonce,
            now + AUTH_TIMESTAMP_WINDOW_SECS + 10,
            now,
            &sig,
        );
        assert!(matches!(err, Err(IdentityError::TimestampWindow)));

        // Tampered peer id
        let c = DeviceIdentity::generate();
        assert!(DeviceIdentity::verify_auth_challenge(
            &a.device_id(),
            &c.device_id(),
            &nonce,
            now,
            now,
            &sig
        )
        .is_err());
    }

    #[test]
    fn key_derivation() {
        let secret = b"super-secret-shared-material";
        let k1 = derive_session_key("dropbridge-v1-session-key", secret);
        let k2 = derive_session_key("dropbridge-v1-session-key", secret);
        let k3 = derive_session_key("dropbridge-v1-other-context", secret);
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn key_roundtrip() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::from_bytes(a.to_bytes()).unwrap();
        assert_eq!(a.device_id(), b.device_id());
    }

    #[test]
    fn z32_roundtrip() {
        let a = DeviceIdentity::generate();
        let s = a.device_id_z32();
        assert_eq!(DeviceIdentity::parse_z32(&s).unwrap(), a.device_id());
    }
}
