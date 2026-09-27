//! Secret protection (spec §11).
//!
//! Private keys are never stored in plaintext. Shipped apps provide a
//! platform protector:
//!
//! * **Android** — Android Keystore AES-256-GCM wrapping (implemented in the
//!   Kotlin layer of the Flutter app; see docs/ANDROID.md),
//! * **Windows** — DPAPI `CryptProtectData` in *this* crate (current user +
//!   machine scope); the engine binary (`dropbridge.exe`) uses it via
//!   [`platform_protector`], and the tray only supervises that engine — it
//!   never touches keys. See docs/WINDOWS.md.
//!
//! The CLI/development fallback is [`FileProtector`]: the key blob is written
//! to a chmod-0600 file inside the app's private config dir. It is explicitly
//! **not** encryption — it exists so headless servers/CI can run.
//!
//! On Windows it is *never* used to write: [`platform_protector`] returns a
//! [`ChainedProtector`] whose primary is DPAPI, so a fresh install can only
//! ever produce a DPAPI-sealed marker. The file protector is reachable there
//! exclusively as a read fallback, which is what keeps a state directory that
//! was created by another platform (or by an older build) bootable instead of
//! making the engine die with an opaque DPAPI error.
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

/// A protector that prefers `primary` but can still *read* a blob produced by
/// `fallback`.
///
/// Sealing always goes to `primary`, so a fallback protector can never widen
/// the on-disk exposure of a freshly created identity. Unsealing tries
/// `primary` first and only reaches for `fallback` when the primary refuses
/// the blob — which on Windows is the "DPAPI blob created by a different user,
/// machine or elevation level" case. This turns an unbootable engine into a
/// bootable one without ever silently downgrading a new install.
pub struct ChainedProtector {
    primary: Box<dyn SecretProtector>,
    fallback: Box<dyn SecretProtector>,
}

impl ChainedProtector {
    /// Chain two protectors; `fallback` is read-only in practice.
    pub fn new(primary: Box<dyn SecretProtector>, fallback: Box<dyn SecretProtector>) -> Self {
        Self { primary, fallback }
    }
}

impl SecretProtector for ChainedProtector {
    fn name(&self) -> &'static str {
        self.primary.name()
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, IdentityError> {
        self.primary.seal(plaintext)
    }

    fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, IdentityError> {
        match self.primary.unseal(sealed) {
            Ok(plaintext) => Ok(plaintext),
            Err(primary_err) => match self.fallback.unseal(sealed) {
                Ok(plaintext) => Ok(plaintext),
                Err(_) => Err(IdentityError::Protector(format!(
                    "the identity could not be unwrapped by {} ({primary_err}) and the fallback \
                     ({}) has no matching blob either. The state directory most likely belongs to \
                     another Windows user or another machine (DPAPI blobs are bound to both). Delete \
                     the state directory to start over, then pair again.",
                    self.primary.name(),
                    self.fallback.name()
                ))),
            },
        }
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
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
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
        // The code is the only actionable part of a DPAPI failure, so surface
        // it: 0x80090005 (-2146893005) is the classic "this blob was sealed by
        // a different user or machine".
        let code = unsafe { GetLastError() };
        return Err(IdentityError::Protector(format!(
            "Crypt{}Data failed (0x{:08X})",
            if protect { "Protect" } else { "Unprotect" },
            code
        )));
    }
    let out =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) }.to_vec();
    unsafe { LocalFree(out_blob.pbData as _) };
    Ok(out)
}

/// Pick the best protector available on this platform (spec §11).
///
/// On Windows the file protector is kept as a *read-only* fallback so a state
/// directory written by another platform still boots; new identities are only
/// ever sealed with DPAPI.
#[must_use]
pub fn platform_protector(fallback_path: std::path::PathBuf) -> Box<dyn SecretProtector> {
    #[cfg(windows)]
    {
        Box::new(ChainedProtector::new(
            Box::new(DpapiProtector),
            Box::new(FileProtector::new(fallback_path)),
        ))
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

    /// A primary that refuses everything, standing in for DPAPI on a machine
    /// that cannot unwrap a blob sealed elsewhere.
    struct Refusing(&'static str);

    impl SecretProtector for Refusing {
        fn name(&self) -> &'static str {
            self.0
        }
        fn seal(&self, _p: &[u8]) -> Result<Vec<u8>, IdentityError> {
            Err(IdentityError::Protector("refusing to seal".into()))
        }
        fn unseal(&self, _s: &[u8]) -> Result<Vec<u8>, IdentityError> {
            Err(IdentityError::Protector("refusing to unseal".into()))
        }
    }

    #[test]
    fn chain_reads_a_blob_only_the_fallback_understands() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("keys/device.key");

        // Written by the file protector, i.e. a state directory that predates
        // the platform protector being wired in.
        let (id, marker) = load_or_create(&FileProtector::new(key.clone()), None).unwrap();
        assert!(!marker.is_empty());

        let chain = ChainedProtector::new(
            Box::new(Refusing("primary")),
            Box::new(FileProtector::new(key)),
        );
        let (id2, marker2) = load_or_create(&chain, Some(&marker)).unwrap();
        assert_eq!(id.device_id(), id2.device_id());
        assert_eq!(marker, marker2, "an existing marker must not be rewritten");
    }

    #[test]
    fn chain_prefers_the_primary() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("keys/device.key");
        let (id, marker) = load_or_create(&FileProtector::new(key), None).unwrap();

        let chain = ChainedProtector::new(
            Box::new(DirectSeedProtector::new(id.to_bytes())),
            Box::new(Refusing("fallback")),
        );
        let (id2, _) = load_or_create(&chain, Some(&marker)).unwrap();
        assert_eq!(id.device_id(), id2.device_id());
    }

    #[test]
    fn chain_never_seals_with_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let fallback_key = dir.path().join("keys/device.key");
        let chain = ChainedProtector::new(
            Box::new(DirectSeedProtector::new([7u8; 32])),
            Box::new(FileProtector::new(fallback_key.clone())),
        );
        let (_id, marker) = load_or_create(&chain, None).unwrap();
        assert_eq!(marker, b"hardware-keystore-backed".to_vec());
        assert!(!fallback_key.exists(), "the fallback must stay read-only");
    }

    #[test]
    fn chain_explains_an_unrecoverable_identity() {
        let dir = tempfile::tempdir().unwrap();
        let chain = ChainedProtector::new(
            Box::new(Refusing("windows-dpapi")),
            Box::new(FileProtector::new(dir.path().join("keys/device.key"))),
        );
        let err = load_or_create(&chain, Some(b"sealed-by-someone-else")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("windows-dpapi"), "{msg}");
        assert!(msg.contains("state directory"), "{msg}");
    }

    #[test]
    fn platform_protector_roundtrips_on_this_host() {
        let dir = tempfile::tempdir().unwrap();
        let p = platform_protector(dir.path().join("keys/device.key"));
        let (id, marker) = load_or_create(p.as_ref(), None).unwrap();
        let (id2, _) = load_or_create(p.as_ref(), Some(&marker)).unwrap();
        assert_eq!(id.device_id(), id2.device_id());
    }
}
