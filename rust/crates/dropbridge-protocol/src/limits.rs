//! Resource limits (spec §53). Every limit here is a DoS guardrail; they are
//! protocol constants, so they live in the protocol crate and are enforced by
//! both sender and receiver.

/// Hard cap for a single control/metadata frame (4 MiB). Chunk *data* never
/// flows through framed messages.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Cap for a pairing frame.
pub const MAX_PAIR_FRAME_BYTES: usize = 64 * 1024;

/// Maximum manifest entries in one transfer (spec §45: 100k small files must
/// work; 200k leaves headroom while bounding memory).
pub const MAX_MANIFEST_ENTRIES: usize = 200_000;

/// Maximum length of a single relative path component-joined path.
pub const MAX_PATH_LEN: usize = 512;

/// Maximum length of a user note attached to an offer.
pub const MAX_NOTE_LEN: usize = 1024;

/// Default chunk size for large-file transfers. Benchmark-driven (see
/// docs/BENCHMARKS.md); 1 MiB is the measured loopback/LAN sweet spot for
/// 1–8 stream configurations on contemporary hardware.
pub const DEFAULT_CHUNK_SIZE: u64 = 1024 * 1024;

/// Default number of parallel data streams for one large file. The bench
/// matrix (spec §44) picks this; it is negotiated down by the receiver via
/// `Capabilities::max_concurrency`.
pub const DEFAULT_STREAM_COUNT: u32 = 4;

/// Absolute ceiling on parallel data streams regardless of negotiation.
pub const MAX_STREAM_COUNT: u32 = 16;

/// Absolute ceiling on chunk size regardless of negotiation.
pub const MAX_CHUNK_SIZE: u64 = 16 * 1024 * 1024;

/// Minimum chunk size accepted in negotiation.
pub const MIN_CHUNK_SIZE: u64 = 64 * 1024;

/// Maximum concurrently active transfer sessions per endpoint.
pub const MAX_CONCURRENT_TRANSFERS: usize = 8;

/// Maximum devices in the trust store.
pub const MAX_TRUSTED_DEVICES: usize = 64;

/// Pairing token TTL (spec §12: 1–2 minutes).
pub const PAIRING_TTL_SECS: i64 = 120;

/// Read timeout for control-channel frames from an unauthenticated/idle peer.
pub const CONTROL_IDLE_TIMEOUT_SECS: u64 = 60;

/// Number of pairing attempts allowed per remote endpoint id per hour.
pub const PAIR_RATE_LIMIT_PER_HOUR: u32 = 8;

/// Helper: clamp a negotiated chunk size into the legal range.
#[must_use]
pub fn clamp_chunk_size(v: u64) -> u64 {
    v.clamp(MIN_CHUNK_SIZE, MAX_CHUNK_SIZE)
}

/// Helper: clamp a negotiated stream count into the legal range.
#[must_use]
pub fn clamp_stream_count(v: u32) -> u32 {
    v.clamp(1, MAX_STREAM_COUNT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps() {
        assert_eq!(clamp_chunk_size(0), MIN_CHUNK_SIZE);
        assert_eq!(clamp_chunk_size(u64::MAX), MAX_CHUNK_SIZE);
        assert_eq!(clamp_stream_count(0), 1);
        assert_eq!(clamp_stream_count(10_000), MAX_STREAM_COUNT);
    }
}
