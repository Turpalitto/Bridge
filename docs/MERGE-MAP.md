# DropBridge Component Merge Map

This document maps every major subsystem across the three source repositories (V1, V2, and V3) to its final, unified production implementation in DropBridge.

---

## 1. Feature Matrix Overview

| Subsystem / Feature | V1 (`_v_archive1`) | V2 (`_v_archive2`) | V3 (`_v3_meetmaker/dropbridge`) | Final Implementation |
|---|---|---|---|---|
| **Primary Transport** | WebRTC DataChannel (browser) | WebRTC DataChannel (browser) | Iroh 1.2 / QUIC (Native Rust) | **V3 (Iroh 1.2 / QUIC)** |
| **Relay Transport** | Postgres signaling mailbox + WebRTC turn | Next.js blind ciphertext relay | Self-hosted `iroh-relay` | **V3 (`dropbridge-relay` / iroh-relay)** |
| **Path Manager & Scoring** | Candidate categorization (`direct-lan`, `internet-p2p`, `relay`) | Score function (RTT, loss, throughput) + 15% hysteresis | Latency probe scoring + hysteresis in Rust | **V2 logic ported to V3 (`dropbridge-network/src/paths.rs`)** |
| **Identity & Cryptography** | Ed25519 keypair in memory | Ed25519 (identity) + X25519 (ECDH session) + AES-GCM | Ed25519 keypair + Hardware protector hooks (DPAPI/Keystore) | **V3 Ed25519 + V2 X25519 ECDH / session security (`dropbridge-identity`)** |
| **Pairing & Trust Gate** | Postgres `pair_sessions` + QR code | QR token + 6-digit challenge + signed Ed25519 challenge + user confirm | Iroh QUIC `ALPN_PAIRING` + one-time token + 6-digit PIN | **V3 Iroh pairing + V2 signed challenge verification + QUIC lifecycle fix** |
| **Device Revocation** | Database row deletion | Signed `/api/devices/:id/revoke` route | `TrustRegistry::revoke(&DeviceId)` in SQLite | **V3 SQLite registry + V2 immediate session termination** |
| **Protocol Framing** | JSON control frames + raw binary chunks | JSON control frames + encrypted chunks | Postcard binary framing + 4-byte length prefix | **V3 Postcard binary protocol (`dropbridge-protocol`)** |
| **Manifest Format** | JSON `{ transferId, items: [{relPath, size}], totalSize, resumeFrom }` | JSON `{ transferId, items: [{relPath, size, blake3}] }` | Postcard binary `Manifest { session, total_bytes, entries }` | **V3 binary `Manifest` with file IDs, mtime, BLAKE3** |
| **Transfer Engine & Chunks** | Browser WebRTC send/receive loops, 256KB chunks | Adaptive chunking (64KB–1MB) | Native Rust multi-stream pipelined engine | **V3 Rust engine (`dropbridge-transfer`) with 256KB default chunks** |
| **Resume Semantics** | Receiver `resume-sync` + sender rewind | Range tracking in Postgres `transfer_chunks` | Receiver `have_ranges` + SQLite range journal | **V1 receiver-authoritative rewind + V3 interval `RangeSet`** |
| **Backpressure & Memory** | `bufferedAmountLow` event listener | WASM chunk stream | Tokio async channel + bounded sinks (4–32 MiB) | **V3 bounded async streams with backpressure** |
| **Integrity (BLAKE3)** | Noble-hashes BLAKE3 in JS | Noble-hashes BLAKE3 in WASM | Native SIMD `blake3` crate (AVX2/NEON) | **V3 native SIMD `blake3` inline streaming** |
| **Atomic File Write** | Final download / FSA stream | `.dropbridge-part` file | `*.dropbridge-part` + atomic rename | **V3 `*.dropbridge-part` with atomic filesystem rename** |
| **Path Traversal Safety** | Basic regex/prefix checks | Exhaustive Windows/Unix sanitizer + virtual root containment | `sanitize_rel_path` + canonical root containment | **V2 exhaustive rules ported to V3 (`dropbridge-protocol/src/path_safety.rs`)** |
| **Collision Policy** | Browser numbered downloads | `nextCollisionName`: `photo (1).jpg` | `find_unused_name`: `photo (1).jpg` | **V2/V3 `find_unused_name` with `Rename` default policy** |
| **Persistence Layer** | PostgreSQL (Drizzle) | PostgreSQL (Drizzle) | Embedded SQLite (`rusqlite` bundled) | **V3 Embedded SQLite (`dropbridge-transfer/src/journal.rs`)** |
| **Android Integration** | Specification in docs | Specification in docs | Flutter shell + native Android Share Target + FFI | **V3 Flutter/Kotlin shell + native FD streaming (`from_raw_fd`)** |
| **Windows 11 Integration** | Specification in docs | Specification in docs | Win32 System Tray + Folder Watcher + Toast UX | **V3 `dropbridge-tray` + stable folder watcher** |
| **LAN Discovery** | WebRTC signaling polling | WebRTC signaling polling | mDNS / DNS-SD (`mdns-sd`) + signed UDP beacon | **V3 `dropbridge-discovery` (mDNS + signed beacon)** |
| **CI Workflow** | GitHub Actions Next.js build | Vitest CI pipeline | GitHub Actions Linux & Windows cross-check | **V3 CI cleaned: removed log auto-commit, artifact upload only** |

---

## 2. Detailed Component Breakdown

### 2.1 Network & Transport Layer
- **Source**: V3 (`dropbridge-network`)
- **Rationale**: QUIC via Iroh 1.2 is the definitive foundation specified in the product architecture. It provides automatic NAT traversal, cryptographic endpoint addressing, multiplexed streams, connection migration, and encrypted wire transport. WebRTC from V1/V2 is strictly a browser fallback and not suited for a high-performance native desktop/mobile transfer daemon.
- **Modifications**:
  1. Fix endpoint address hints generation: ensure local listening socket addresses are included even when running offline with `RelayConfig::Disabled`.
  2. Integrate V2's path manager scoring formula and 15% hysteresis margin into `dropbridge-network/src/paths.rs`.
- **Target Location**: `rust/crates/dropbridge-network/`

### 2.2 Protocol & Framing Layer
- **Source**: V3 (`dropbridge-protocol`)
- **Rationale**: V3's Postcard binary serialization provides compact wire representation, zero allocations during streaming header checks, and strict validation.
- **Modifications**:
  1. Add capability negotiation inspired by V2's `negotiateCapabilities`.
  2. Implement V2's exhaustive path safety rules into `dropbridge-protocol/src/path_safety.rs`.
  3. Ensure pairing messages support signed challenge verification.
- **Target Location**: `rust/crates/dropbridge-protocol/`

### 2.3 Identity, Cryptography & Security Layer
- **Source**: V2 security design merged into V3 (`dropbridge-identity`)
- **Rationale**: V2 provides a hardened identity model with dual keypairs (Ed25519 for identity proof + X25519 for session encryption), signed challenge-response authentication, replay windows, and explicit device revocation. V3 provides the native Rust integration and hardware secret protector abstractions.
- **Modifications**:
  1. Enhance `dropbridge-identity` with X25519 ECDH key generation and HKDF-derived session keys for end-to-end payload confidentiality over relays.
  2. Integrate Ed25519 challenge signing into the pairing handshake.
  3. Enforce immediate active session termination upon device revocation.
- **Target Location**: `rust/crates/dropbridge-identity/`

### 2.4 Pairing Engine
- **Source**: V3 (`dropbridge-core/src/pairing.rs`) + V2 signed challenge logic
- **Rationale**: V3's direct Iroh ALPN connection avoids third-party rendezvous servers for LAN pairing.
- **Modifications**:
  1. **Bugfix (Critical)**: Eliminate the race condition where the enroller calls `conn.close()` immediately upon sending `PairMsg::Done`. Replace with graceful stream finish (`send.finish()`) and clean shutdown coordination, resolving `Other("connection lost")`.
  2. Store address hints mutually on both enroller and joiner so subsequent transfers can dial immediately.
  3. Verify cryptographic Ed25519 signature of the challenge PIN.
- **Target Location**: `rust/crates/dropbridge-core/src/pairing.rs`

### 2.5 Transfer Engine, Resume & Progress
- **Source**: V1 transfer semantics merged into V3 (`dropbridge-transfer`)
- **Rationale**: V1 has the superior transfer state machine, receiver-authoritative rewind semantics, smoothed speed/ETA calculation, and backpressure handling. V3 provides the high-performance native async disk I/O, `RangeSet` interval tracking, and SIMD BLAKE3 verification.
- **Modifications**:
  1. Enforce receiver-authoritative resume: sender always clamps its starting position to the receiver's acknowledged range. If sender claims progress ahead of receiver, sender must rewind.
  2. Stream chunking with bounded in-flight memory (4–32 MiB).
  3. Atomic file writes: stream data into `.dropbridge-part`, verify BLAKE3 hash, atomic rename to final filename.
  4. Fix collision handling so collisions rename files sequentially: `photo.jpg` -> `photo (1).jpg`.
- **Target Location**: `rust/crates/dropbridge-transfer/`

### 2.6 Persistence Layer
- **Source**: V3 (`dropbridge-transfer/src/journal.rs` & `dropbridge-identity/src/trust.rs`)
- **Rationale**: Embedded SQLite (`rusqlite` bundled) is the only suitable persistence layer for cross-platform desktop and mobile clients. It requires no external database daemon and survives app restarts.
- **Modifications**:
  1. Unify database schema into clean, consistent tables: `transfers`, `transfer_files`, `transfer_ranges`, `trusted_devices`.
  2. Ensure all in-flight transfer state can be fully reconstructed after crash/restart.
- **Target Location**: `rust/crates/dropbridge-transfer/src/journal.rs`

### 2.7 Android Integration & Streaming
- **Source**: V3 (`dropbridge/app/flutter/android` & `dropbridge-ffi`)
- **Rationale**: Real Android file transfers cannot copy multi-gigabyte files into temporary app cache before sending.
- **Modifications**:
  1. Implement native file descriptor streaming: `ContentResolver.openFileDescriptor` -> `ParcelFileDescriptor.detachFd()` -> Rust `tokio::fs::File::from_raw_fd`.
  2. Stream directly from Android content provider into QUIC sink with zero staging copies.
  3. Support `ACTION_SEND` and `ACTION_SEND_MULTIPLE`.
- **Target Location**: `dropbridge/app/flutter/android/` and `rust/crates/dropbridge-ffi/`

### 2.8 Windows 11 Integration
- **Source**: V3 (`dropbridge-tray` & `dropbridge-core/src/watcher.rs`)
- **Rationale**: Windows 11 users require a native tray app, startup integration, and drop-zone folder watching.
- **Modifications**:
  1. File stability detector in folder watcher: verify file size is unchanged and file handle is exclusively accessible over a 2-second stability window before beginning transfer.
  2. Toast notifications for incoming files and completed transfers.
- **Target Location**: `rust/crates/dropbridge-tray/`

### 2.9 Continuous Integration
- **Source**: V3 (`.github/workflows/dropbridge.yml`)
- **Rationale**: Automated CI must test, verify, and report without altering git history.
- **Modifications**:
  1. Remove git auto-commit / auto-push step on failure (`git commit -m "ci: publish logs..."`).
  2. Replace with standard GitHub Actions artifact upload (`actions/upload-artifact`) for test logs.
- **Target Location**: `.github/workflows/dropbridge.yml`
