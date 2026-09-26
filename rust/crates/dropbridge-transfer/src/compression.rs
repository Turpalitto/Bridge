//! Conditional streaming compression for DropBridge transfers.
//!
//! Applies fast Zstandard compression to compressible file formats (text, logs,
//! json, source code, sqlite databases) while bypassing already-compressed media
//! formats (.mp4, .jpg, .zip, .apk) with zero CPU overhead.

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompressionError {
    #[error("decompression failed: {0}")]
    Decompress(String),
    #[error("decompressed length mismatch: expected {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
}

/// Extensions known to be already compressed, where zstd yields negligible or negative savings.
const PRECOMPRESSED_EXTENSIONS: &[&str] = &[
    // Video
    "mp4", "mkv", "avi", "mov", "webm", "flv", "m4v", "ts", // Audio
    "mp3", "aac", "flac", "ogg", "m4a", "opus", "wma", // Images
    "jpg", "jpeg", "png", "webp", "avif", "gif", "heic", "heif", "jxl",
    // Archives & Packages
    "zip", "gz", "xz", "bz2", "zst", "7z", "rar", "tgz", "apk", "aab", "jar", "ipa", "dmg", "iso",
    // Pre-compressed documents
    "pdf",
];

/// Checks if a file path is likely compressible based on its extension.
#[must_use]
pub fn is_compressible_path(path: &Path) -> bool {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_ascii_lowercase(),
        None => return true, // Files without extension (e.g. README, Makefile, unix bins) may be text
    };
    !PRECOMPRESSED_EXTENSIONS.contains(&ext.as_str())
}

/// Compress chunk data using zstd level 1 (fastest, high throughput).
/// Returns `None` if compressed data is not smaller than original raw bytes.
#[must_use]
pub fn compress_chunk(data: &[u8], level: i32) -> Option<Vec<u8>> {
    if data.len() < 64 {
        // Small chunks have frame header overhead and won't benefit from compression.
        return None;
    }
    match zstd::bulk::compress(data, level) {
        Ok(compressed) if compressed.len() < data.len() => Some(compressed),
        _ => None,
    }
}

/// Decompress chunk data, verifying the expected uncompressed length.
pub fn decompress_chunk(data: &[u8], expected_len: usize) -> Result<Vec<u8>, CompressionError> {
    let decompressed = zstd::bulk::decompress(data, expected_len)
        .map_err(|e| CompressionError::Decompress(e.to_string()))?;
    if decompressed.len() != expected_len {
        return Err(CompressionError::LengthMismatch {
            expected: expected_len,
            actual: decompressed.len(),
        });
    }
    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_compressible_path() {
        assert!(is_compressible_path(Path::new("document.txt")));
        assert!(is_compressible_path(Path::new("source.rs")));
        assert!(is_compressible_path(Path::new("database.sqlite")));
        assert!(is_compressible_path(Path::new("log.json")));
        assert!(is_compressible_path(Path::new("Makefile")));

        assert!(!is_compressible_path(Path::new("movie.mp4")));
        assert!(!is_compressible_path(Path::new("archive.zip")));
        assert!(!is_compressible_path(Path::new("photo.jpg")));
        assert!(!is_compressible_path(Path::new("photo.JPEG")));
        assert!(!is_compressible_path(Path::new("app.apk")));
        assert!(!is_compressible_path(Path::new("paper.pdf")));
    }

    #[test]
    fn test_compress_and_decompress_roundtrip() {
        let original = "Hello World! DropBridge 2026 state of the art fast transfer.\n".repeat(100);
        let raw_bytes = original.as_bytes();

        let compressed = compress_chunk(raw_bytes, 1).expect("compressible repetitive text");
        assert!(compressed.len() < raw_bytes.len());

        let decompressed =
            decompress_chunk(&compressed, raw_bytes.len()).expect("valid decompress");
        assert_eq!(decompressed, raw_bytes);
    }

    #[test]
    fn test_uncompressible_data_returns_none() {
        // High-entropy random bytes (from BLAKE3 XOF) do not compress
        let mut high_entropy = [0u8; 128];
        let mut xof = blake3::Hasher::new()
            .update(b"unique_salt_2026")
            .finalize_xof();
        xof.fill(&mut high_entropy);

        let result = compress_chunk(&high_entropy, 1);
        assert!(result.is_none());
    }

    #[test]
    fn test_decompress_invalid_data() {
        let junk = b"not a valid zstd frame payload";
        let err = decompress_chunk(junk, 100);
        assert!(matches!(err, Err(CompressionError::Decompress(_))));
    }
}
