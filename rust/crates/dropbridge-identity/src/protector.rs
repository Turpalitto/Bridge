//! Secret protection (spec §11).
//!
//! Private keys are never stored in plaintext. Shipped apps provide a
//! platform protector:
//!
//! * **Android** — Android Keystore AES-256-GCM wrapping (implemented in the
//!   Kotlin layer of the Flutter app; see docs/ANDROID.md),
//! * **Windows** — DPAPI `CryptProtectData` (current user scope; implemented
//!   in `dropbridge-tray`, see docs/WINDOWS.md).
//!
//! The CLI/development fallback is [`FileProtector`]: the key blob is written
//! to a chmod-0600 file inside the app's private config dir. It is explicitly
//! **not** encryption — it exists so headless servers/CI can run, and it is
//! refused by default when a platform protector is available (the app wiring
//! decides).
use std::path::{Path, PathBuf};

use crate::IdentityError;

/// Wraps/unwraps secret material at rest.
pub trait SecretProtector: Send + Sync {
    /// Human-readable mechanism name (for diagnostics, never the secret).
    fn name(&self) -> &'static str;
    /// Wrap secret bytes for storage.
    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, IdentityError>;
    /// Unwrap previously sealed bytes.
    fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, IdentityError>;
}

/// No-op protector for tests only.
#[derive(Default)]
pub struct NullProtector;

impl SecretProtector for NullProtector {
    fn name(&self) -> &'static str {
        "none(test)"
    }
    fn seal(&self, p: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(p.to_vec())
    }
    fn unseal(&self, s: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(s.to_vec())
    }
}

/// In-memory direct seed protector.
///
/// Used when a host platform (e.g. Android Keystore / TEE / StrongBox)
/// unseals the 32-byte private seed in hardware memory and passes it directly
/// to Rust without saving any plaintext seed to disk.
pub struct DirectSeedProtector {
    seed: [u8; 32],
}

impl DirectSeedProtector {
    #[must_use]
    pub fn new(seed: [u8; 32]) -> Self {
        Self { seed }
    }
}

impl SecretProtector for DirectSeedProtector {
    fn name(&self) -> &'static str {
        "hardware-keystore-direct"
    }

    fn seal(&self, _plaintext: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(b"hardware-keystore-backed".to_vec())
    }

    fn unseal(&self, _sealed: &[u8]) -> Result<Vec<u8>, IdentityError> {
        Ok(self.seed.to_vec())
    }
}

/// chmod-0600 file protector (CLI/dev fallback; see module docs).
pub struct FileProtector {
    path: PathBuf,
}

impl FileProtector {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn set_owner_only(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }
}

impl SecretProtector for FileProtector {
    fn name(&self) -> &'static str {
        "file-0600"
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, IdentityError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, plaintext)?;
        Self::set_owner_only(&self.path);
        Ok(self.path.to_string_lossy().as_bytes().to_vec())
    }

    fn unseal(&self, _sealed: &[u8]) -> Result<Vec<u8>, IdentityError> {
        std::fs::read(&self.path).map_err(IdentityError::from)
    }
}

/// Load-or-create an identity using a protector.
///
/// `marker` is the small persisted blob returned by [`SecretProtector::seal`]
/// (e.g. a path or DPAPI descriptor); it is stored by the caller next to the
/// config and passed back on startup.
pub fn load_or_create(
    protector: &dyn SecretProtector,
    existing_marker: Option<&[u8]>,
) -> Result<(crate::DeviceIdentity, Vec<u8>), IdentityError> {
    if let Some(marker) = existing_marker {
        let raw = protector.unseal(marker)?;
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::InvalidKey)?;
        return Ok((crate::DeviceIdentity::from_bytes(bytes)?, marker.to_vec()));
    }
    let id = crate::DeviceIdentity::generate();
    let marker = protector.seal(&id.to_bytes())?;
    Ok((id, marker))
}

/// Windows DPAPI protector (spec §11): `CryptProtectData` with the current
/// user's credentials, UI forbidden. The secret never exists on disk as
/// plaintext; the sealed blob is machine+user bound by the OS.
#[cfg(windows)]
pub struct DpapiProtector;

#[cfg(windows)]
impl SecretProtector for DpapiProtector {
    fn name(&self) -> &'static str {
        "windows-dpapi"
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, IdentityError> {
        dpapi(plaintext, true)
    }

    fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, IdentityError> {
        dpapi(sealed, false)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>, IdentityError> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
        } else {
            CryptUnprotectData(
                &in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out_blob,
            )
        }
    };
    if ok == 0 {
        return Err(IdentityError::Protector("DPAPI call failed".into()));
    }
    let out =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }.to_vec();
    unsafe { LocalFree(out_blob.pbData as _) };
    Ok(out)
}

/// Pick the best protector available on this platform (spec §11).
pub fn platform_protector(fallback_path: std::path::PathBuf) -> Box<dyn SecretProtector> {
    #[cfg(windows)]
    {
        let _ = fallback_path;
        Box::new(DpapiProtector)
    }
    #[cfg(not(windows))]
    {
        Box::new(FileProtector::new(fallback_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_protector_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = FileProtector::new(dir.path().join("keys/device.key"));
        let (id, marker) = load_or_create(&p, None).unwrap();
        let (id2, _) = load_or_create(&p, Some(&marker)).unwrap();
        assert_eq!(id.device_id(), id2.device_id());
    }
}
