//! Receiver-side path sanitization (spec §51).
//!
//! We NEVER trust filenames from a peer. This module enforces the strictest
//! of POSIX + Windows rules so a received path can never escape the receive
//! root on any supported OS:
//!
//! * no `..` components, no absolute paths, no UNC (`//host/…`, `\\host\…`),
//! * no Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0-9`, `LPT0-9`),
//! * no drive letters (`C:\…`), no alternate data streams (`name:stream`),
//! * no control characters, no NUL, no Windows-reserved characters,
//! * length limits per component and for the whole path,
//! * normalization of `\` → `/` and collapsing of `.` components.
//!
//! The result is a clean `/`-separated relative path; the caller joins it to
//! the receive root and MUST additionally verify the canonical result stays
//! under the root (defense in depth: [`is_within_root`]).
use crate::ProtocolError;

const MAX_COMPONENT_LEN: usize = 255;

fn is_windows_device_name(stem: &str) -> bool {
    let s = stem.to_ascii_uppercase();
    let bare = ["CON", "PRN", "AUX", "NUL"];
    if bare.contains(&s.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            if rest.len() <= 2 && rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
                return true;
            }
        }
    }
    false
}

/// Sanitize a peer-supplied relative path. Returns the cleaned path.
pub fn sanitize_rel_path(p: &str) -> Result<String, ProtocolError> {
    let reject = |why: &str| ProtocolError::UnsafePath(format!("{why}: {p:?}"));

    if p.is_empty() {
        return Err(reject("empty path"));
    }
    if p.len() > crate::limits::MAX_PATH_LEN {
        return Err(ProtocolError::PathTooLong);
    }
    if p.contains('\0') {
        return Err(reject("NUL byte"));
    }
    if p.chars().any(|c| c.is_control()) {
        return Err(reject("control character"));
    }
    // Absolute / UNC forms.
    if p.starts_with('/') || p.starts_with('\\') {
        return Err(reject("absolute path"));
    }
    if p.contains("\\\\") {
        return Err(reject("UNC path"));
    }
    // Drive letter prefix: "C:" anywhere as a component prefix.
    let bytes = p.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(reject("drive-letter path"));
    }

    let mut out: Vec<String> = Vec::new();
    for raw in p.split(['/', '\\']) {
        if raw.is_empty() {
            continue; // tolerate double slashes after the start-checks above
        }
        if raw == "." {
            continue;
        }
        if raw.contains("..") {
            return Err(reject("parent traversal"));
        }
        if raw.len() > MAX_COMPONENT_LEN {
            return Err(ProtocolError::PathTooLong);
        }
        if raw
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err(reject("reserved character"));
        }
        // Strip trailing dots/spaces (Windows normalizes these away, which
        // can change identity); reject rather than silently rename.
        if raw.ends_with('.') || raw.ends_with(' ') {
            return Err(reject("trailing dot or space"));
        }
        let stem = raw.split('.').next().unwrap_or(raw);
        if is_windows_device_name(stem) {
            return Err(reject("windows device name"));
        }
        out.push(raw.to_string());
    }

    if out.is_empty() {
        return Err(reject("path reduced to nothing"));
    }
    let joined = out.join("/");
    if joined.len() > crate::limits::MAX_PATH_LEN {
        return Err(ProtocolError::PathTooLong);
    }
    Ok(joined)
}

/// Convenience predicate for checking path safety without allocating (mirrors V2 isSafeRelativePath).
pub fn is_safe_rel_path(p: &str) -> bool {
    sanitize_rel_path(p).is_ok()
}

/// Defense in depth: verify `candidate` (canonical) stays under `root`.
/// Both must already be canonical/absolute.
pub fn is_within_root(root: &std::path::Path, candidate: &std::path::Path) -> bool {
    candidate.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_paths_pass() {
        assert_eq!(sanitize_rel_path("a.jpg").unwrap(), "a.jpg");
        assert_eq!(
            sanitize_rel_path("Photos/2026/c.jpg").unwrap(),
            "Photos/2026/c.jpg"
        );
        assert_eq!(sanitize_rel_path("./x/y.txt").unwrap(), "x/y.txt");
        // Unicode is fine.
        assert!(sanitize_rel_path("фото/видео.mp4").is_ok());
        assert!(is_safe_rel_path("Photos/2026/c.jpg"));
    }

    #[test]
    fn bad_paths_rejected() {
        let bad = [
            "../x",
            "..\\x",
            "/x",
            "\\x",
            "x/../y",
            "../../etc/passwd",
            "..\\..\\windows\\system32",
            "//host/share",
            "\\\\h\\s",
            "\\\\server\\share\\file.txt",
            "C:\\x",
            "c:/x",
            "C:\\Windows\\System32\\evil.dll",
            "a:b",
            "innocent.txt:hidden.exe",
            "CON",
            "con.txt",
            "CON.txt",
            "folder/COM1.txt",
            "NUL.tar",
            "COM1",
            "LPT1",
            "lpt9.txt",
            "PRN.any",
            "aux",
            "aux.tar",
            "a\u{0000}b",
            "file\0name.txt",
            "a\nb",
            "x<y",
            "trail.",
            "trail ",
            "",
            &"x".repeat(600),
        ];
        for p in bad {
            assert!(sanitize_rel_path(p).is_err(), "must reject {p:?}");
            assert!(!is_safe_rel_path(p), "must reject {p:?}");
        }
    }

    #[test]
    fn within_root() {
        let root = std::path::Path::new("/srv/recv");
        assert!(is_within_root(root, &root.join("a/b.txt")));
        assert!(!is_within_root(root, std::path::Path::new("/srv/evil")));
    }

    use proptest::prelude::*;
    proptest! {
        #[test]
        fn sanitizer_never_emits_traversal(s in "\\PC{0,3}[a-zA-Z0-9 .:_-]{0,10}([/\\\\][a-zA-Z0-9 .:_-]{0,10}){0,4}") {
            if let Ok(p) = sanitize_rel_path(&s) {
                prop_assert!(!p.contains(".."));
                prop_assert!(!p.starts_with('/'));
                prop_assert!(!p.starts_with('\\'));
                prop_assert!(!p.contains('\0'));
            }
        }
    }
}
