//! Accountless QR pairing (spec §9, §12).
//!
//! Flow:
//! 1. Enroller (e.g. Windows "Add phone") generates a one-time random
//!    `pairing_token` (32 bytes) + expiry ≤ 120 s and shows a QR containing a
//!    [`PairInvitation`] (device id, name, endpoint hints, token, expiry).
//! 2. Joiner (phone) scans, dials the enroller's pairing ALPN using the hints,
//!    sends [`PairMsg::Request`] containing the token and its own identity.
//!    The QUIC layer already authenticates the enroller's identity key
//!    (MITM-safe transport); the token proves QR possession; the short TTL
//!    limits replay.
//! 3. Enroller shows the joiner's device name + a 6-digit auth code derived
//!    from `BLAKE3(token || enroller_id || joiner_id)`; user confirms.
//! 4. On confirm both sides persist each other as trusted devices.
//!
//! A stolen token within TTL still cannot MITM (TLS keys authenticate both
//! endpoints) — worst case an attacker pairs *as itself*, which the human
//! sees as an unknown device name in step 3 and rejects.
use crate::limits::PAIRING_TTL_SECS;
use crate::{DeviceId, DeviceKind};
use serde::{Deserialize, Serialize};

/// Versioned QR payload. Serialized with postcard, then base32 (QR-friendly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairInvitation {
    /// Magic, for scanners rejecting foreign QRs fast.
    pub magic: [u8; 4],
    pub version: u32,
    /// Enroller's device identity (Ed25519 public key).
    pub device_id: DeviceId,
    pub device_name: String,
    pub device_kind: DeviceKind,
    /// One-time random token, 32 bytes.
    pub pairing_token: [u8; 32],
    /// Unix seconds after which the invitation is void.
    pub expires_at: i64,
    /// Endpoint hints: relay URLs (as strings) the enroller is reachable on.
    pub relay_hints: Vec<String>,
    /// Direct socket hints (LAN), "ip:port" strings. May be empty.
    pub direct_hints: Vec<String>,
}

pub const PAIR_MAGIC: [u8; 4] = *b"DBPQ";

impl PairInvitation {
    pub fn new(
        device_id: DeviceId,
        device_name: String,
        device_kind: DeviceKind,
        pairing_token: [u8; 32],
        now_unix: i64,
        relay_hints: Vec<String>,
        direct_hints: Vec<String>,
    ) -> Self {
        Self {
            magic: PAIR_MAGIC,
            version: 1,
            device_id,
            device_name,
            device_kind,
            pairing_token,
            expires_at: now_unix + PAIRING_TTL_SECS,
            relay_hints,
            direct_hints,
        }
    }

    pub fn expired(&self, now_unix: i64) -> bool {
        now_unix > self.expires_at
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        postcard::to_allocvec(self).map_err(|e| e.to_string())
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self, String> {
        let v: Self = postcard::from_bytes(b).map_err(|e| e.to_string())?;
        if v.magic != PAIR_MAGIC {
            return Err("not a DropBridge pairing QR".into());
        }
        Ok(v)
    }

    /// Canonical QR string: `dropbridge://pair/<base32>`.
    pub fn to_qr_string(&self) -> Result<String, String> {
        let b = self.to_bytes()?;
        Ok(format!(
            "dropbridge://pair/{}",
            data_encoding::BASE32_NOPAD.encode(&b)
        ))
    }

    pub fn from_qr_string(s: &str) -> Result<Self, String> {
        let b32 = s
            .trim()
            .strip_prefix("dropbridge://pair/")
            .ok_or("not a dropbridge pairing QR")?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(b32.to_ascii_uppercase().as_bytes())
            .map_err(|e| e.to_string())?;
        Self::from_bytes(&bytes)
    }

    /// Short human-comparable auth code (6 decimal digits).
    pub fn auth_code(&self, joiner_id: &DeviceId) -> u32 {
        let mut h = blake3::Hasher::new();
        h.update(&self.pairing_token);
        h.update(&self.device_id);
        h.update(joiner_id);
        let digest = h.finalize();
        let v = u32::from_be_bytes(digest.as_bytes()[0..4].try_into().unwrap());
        v % 1_000_000
    }
}

/// Messages on the pairing ALPN control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairMsg {
    /// Joiner → enroller.
    Request {
        token: [u8; 32],
        device_name: String,
        device_kind: DeviceKind,
    },
    /// Enroller → joiner: show the code, wait for user.
    Challenge { auth_code: u32 },
    /// Joiner confirms its user saw/entered the code.
    Confirm { auth_code: u32 },
    /// Enroller → joiner: pairing finalized.
    Done,
    /// Either side: pairing failed (bad token, expired, rejected, replayed).
    Fail { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv() -> PairInvitation {
        PairInvitation::new(
            [7u8; 32],
            "Home Laptop".into(),
            DeviceKind::Laptop,
            [9u8; 32],
            1_000_000,
            vec!["https://relay.example/".into()],
            vec!["192.168.1.20:4242".into()],
        )
    }

    #[test]
    fn qr_roundtrip() {
        let i = inv();
        let s = i.to_qr_string().unwrap();
        assert!(s.starts_with("dropbridge://pair/"));
        let back = PairInvitation::from_qr_string(&s).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn expiry() {
        let i = inv();
        assert!(!i.expired(1_000_000));
        assert!(!i.expired(1_000_000 + PAIRING_TTL_SECS));
        assert!(i.expired(1_000_000 + PAIRING_TTL_SECS + 1));
    }

    #[test]
    fn auth_code_stable_and_bounded() {
        let i = inv();
        let j: DeviceId = [3u8; 32];
        let c1 = i.auth_code(&j);
        let c2 = i.auth_code(&j);
        assert_eq!(c1, c2);
        assert!(c1 < 1_000_000);
        let other: DeviceId = [4u8; 32];
        // Different joiner ⇒ (overwhelmingly likely) different code.
        assert_ne!(i.auth_code(&other), c1);
    }

    #[test]
    fn foreign_qr_rejected() {
        assert!(PairInvitation::from_qr_string("https://example.com").is_err());
        assert!(PairInvitation::from_qr_string("dropbridge://pair/!!!!").is_err());
    }
}
