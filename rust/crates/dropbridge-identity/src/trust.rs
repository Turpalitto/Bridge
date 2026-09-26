//! Trusted device registry (spec §13).
//!
//! After pairing, a device becomes trusted with a set of permission flags.
//! Revocation removes the entry; because transport connections are
//! authenticated by device id, a revoked device can no longer pass the
//! trust check on any subsequent connection. Old pairing tokens are one-time
//! and expired, so revocation is effective immediately.
/// Minimal bitflags replacement (avoids an extra dependency).
#[macro_export]
macro_rules! bitflags_like {
    (
        $(#[$outer:meta])*
        pub struct $name:ident: $ty:ty {
            $(const $flag:ident = $val:expr;)*
        }
    ) => {
        $(#[$outer])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name($ty);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $flag: Self = Self($val);)*

            #[must_use]
            pub const fn bits(&self) -> $ty { self.0 }

            #[must_use]
            pub const fn from_bits_truncate(b: $ty) -> Self { Self(b) }

            #[must_use]
            pub const fn contains(&self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            #[must_use]
            pub const fn union(self, other: Self) -> Self { Self(self.0 | other.0) }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }
    };
}

use std::path::{Path, PathBuf};

use crate::{DeviceId, IdentityError};
use serde::{Deserialize, Serialize};

bitflags_like! {
    /// Permission flags for a trusted device.
    pub struct Permission: u32 {
        const RECEIVE_FILES   = 0b0000_0001;
        const SEND_FILES      = 0b0000_0010;
        const AUTO_RECEIVE    = 0b0000_0100;
        const FOLDER_SYNC     = 0b0000_1000;
        const CLIPBOARD       = 0b0001_0000;
        const REMOTE_WAKE     = 0b0010_0000;
    }
}

/// Default permissions granted at pairing time (spec §13).
impl Permission {
    #[must_use]
    pub fn default_pairing() -> Self {
        Self::RECEIVE_FILES | Self::SEND_FILES | Self::AUTO_RECEIVE
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedDevice {
    pub device_id: DeviceId,
    pub name: String,
    pub kind: String,
    pub permissions: u32,
    /// Unix seconds when paired.
    pub paired_at: i64,
    /// Unix seconds when last seen (best-effort, local only).
    pub last_seen: i64,
}

impl TrustedDevice {
    pub fn has(&self, p: Permission) -> bool {
        self.permissions & p.bits() == p.bits()
    }
}

/// File-backed trust registry (atomic JSON rewrite).
pub struct TrustRegistry {
    path: PathBuf,
    devices: Vec<TrustedDevice>,
}

impl TrustRegistry {
    /// Load (or create) the registry at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let path = path.as_ref().to_path_buf();
        let devices = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| IdentityError::InvalidKey)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { path, devices })
    }

    #[must_use]
    pub fn devices(&self) -> &[TrustedDevice] {
        &self.devices
    }

    pub fn get(&self, id: &DeviceId) -> Option<&TrustedDevice> {
        self.devices.iter().find(|d| &d.device_id == id)
    }

    pub fn is_trusted(&self, id: &DeviceId, p: Permission) -> bool {
        self.get(id).is_some_and(|d| d.has(p))
    }

    /// Add or update a trusted device, then persist.
    pub fn upsert(&mut self, device: TrustedDevice) -> Result<(), IdentityError> {
        if self.devices.len() >= dropbridge_protocol::limits::MAX_TRUSTED_DEVICES
            && !self.devices.iter().any(|d| d.device_id == device.device_id)
        {
            return Err(IdentityError::Protector("trust store full".into()));
        }
        if let Some(existing) = self
            .devices
            .iter_mut()
            .find(|d| d.device_id == device.device_id)
        {
            *existing = device;
        } else {
            self.devices.push(device);
        }
        self.save()
    }

    /// Revoke a device. Returns true if it was present.
    pub fn revoke(&mut self, id: &DeviceId) -> Result<bool, IdentityError> {
        let before = self.devices.len();
        self.devices.retain(|d| &d.device_id != id);
        self.save()?;
        Ok(self.devices.len() < before)
    }

    /// Update last-seen timestamp (persisted lazily on next save).
    pub fn touch(&mut self, id: &DeviceId, now: i64) {
        if let Some(d) = self.devices.iter_mut().find(|d| &d.device_id == id) {
            d.last_seen = now;
        }
    }

    pub fn save(&self) -> Result<(), IdentityError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.devices)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl From<serde_json::Error> for IdentityError {
    fn from(e: serde_json::Error) -> Self {
        IdentityError::Protector(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td(id_byte: u8, name: &str) -> TrustedDevice {
        TrustedDevice {
            device_id: [id_byte; 32],
            name: name.into(),
            kind: "phone".into(),
            permissions: Permission::default_pairing().bits(),
            paired_at: 1,
            last_seen: 1,
        }
    }

    #[test]
    fn registry_roundtrip_and_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        let mut reg = TrustRegistry::open(&path).unwrap();
        reg.upsert(td(1, "OPPO")).unwrap();
        reg.upsert(td(2, "Laptop")).unwrap();

        let mut reg2 = TrustRegistry::open(&path).unwrap();
        assert_eq!(reg2.devices().len(), 2);
        assert!(reg2.is_trusted(&[1; 32], Permission::SEND_FILES));
        assert!(!reg2.is_trusted(&[1; 32], Permission::CLIPBOARD));

        assert!(reg2.revoke(&[1; 32]).unwrap());
        let reg3 = TrustRegistry::open(&path).unwrap();
        assert_eq!(reg3.devices().len(), 1);
        assert!(!reg3.is_trusted(&[1; 32], Permission::RECEIVE_FILES));
    }

    #[test]
    fn permission_bits() {
        let p = Permission::RECEIVE_FILES | Permission::AUTO_RECEIVE;
        assert!(p.contains(Permission::RECEIVE_FILES));
        assert!(!p.contains(Permission::CLIPBOARD));
    }
}
