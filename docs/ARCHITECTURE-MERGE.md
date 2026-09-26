# DropBridge: Unified Architecture Merge Document

## 1. Overview and Executive Summary

DropBridge is a high-speed, secure, cross-platform file transfer application connecting Android and Windows 11 devices across diverse network topologies:
- Direct Local Area Network (Wi-Fi / Ethernet)
- Direct Peer-to-Peer (NAT traversal via STUN/hole-punching)
- Blind Encrypted Relay fallback (when direct connectivity is obstructed)

This document formalizes the synthesis of three predecessor codebases into a single unified production architecture:
1. **V1 (`_v_archive1` / `dropbridge-file-transfer-system.zip`)**: Web reference prototype emphasizing transfer semantics, stream chunking, progress tracking, and receiver-authoritative resume synchronization.
2. **V2 (`_v_archive2` / `dropbridge-file-transfer-system (1).zip`)**: Security and control-plane reference implementation emphasizing Ed25519/X25519 identity, authenticated challenge-response pairing, session key derivation, device revocation, path traversal protection, and route scoring with hysteresis.
3. **V3 (`_v3_meetmaker/dropbridge` / GitHub Foundation)**: Native Rust workspace with Iroh 1.2 QUIC networking, SQLite persistence, native BLAKE3 hashing, FFI surface for Flutter/Android, Windows tray app, and integration test harness.

---

## 2. High-Level Architecture Diagram

```
┌────────────────────────────────────────────────────────────────────────┐
│                          USER INTERFACE LAYER                          │
│                                                                        │
│   Android (Flutter / Native Kotlin)        Windows 11 (WinUI / Tray)   │
│   • Share Target (ACTION_SEND / MULTIPLE)  • System Tray Menu          │
│   • SAF Content URI / FD resolution        • Folder Watcher (drop zone)│
│   • Device list, pairing UX, notifications • Toast notifications       │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Commands / Events / FDs (JSON FFI)
                                    │ (Zero bulk file bytes across FFI)
┌───────────────────────────────────▼────────────────────────────────────┐
│                        DROPBRIDGE RUST CORE ENGINE                     │
│                                                                        │
│  ┌───────────────────────┐ ┌──────────────────────┐ ┌────────────────┐ │
│  │ dropbridge-identity   │ │ dropbridge-pairing   │ │dropbridge-proto│ │
│  │ • Ed25519 node key    │ │ • QR token exchange  │ │• Postcard v1   │ │
│  │ • X25519 E2E key      │ │ • Signed challenges  │ │• Path safety   │ │
│  │ • Hardware protector  │ │ • Replay protection  │ │• Caps negot.   │ │
│  └───────────────────────┘ └──────────────────────┘ └────────────────┘ │
│  ┌───────────────────────┐ ┌──────────────────────┐ ┌────────────────┐ │
│  │ dropbridge-transfer   │ │ dropbridge-storage   │ │dropbridge-disc.│ │
│  │ • Sender streaming    │ │ • SQLite journal     │ │• mDNS / DNS-SD │ │
│  │ • Receiver engine     │ │ • Range tracking     │ │• Signed UDP    │ │
│  │ • Bounded RAM (4-32MB)│ │ • Trusted devices    │ │• Hints cache   │ │
│  │ • BLAKE3 SIMD inline  │ │ • Atomic rename/part │ │                │ │
│  └───────────────────────┘ └──────────────────────┘ └────────────────┘ │
│  ┌───────────────────────────────────────────────────────────────────┐ │
│  │ dropbridge-network                                                │ │
│  │ • Iroh 1.2 Endpoint (QUIC, dial-by-key)                           │ │
│  │ • Path Manager (RTT, loss, throughput scoring + 15% hysteresis)   │ │
│  │ • Graceful stream/connection lifecycle management                 │ │
│  └───────────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ QUIC Streams (Encrypted / Multiplexed)
                                    ▼
       ┌────────────────────────────┼───────────────────────────┐
       ▼                            ▼                           ▼
  Direct LAN (QUIC)       Direct P2P (Hole Punch)     Blind Relay (Encrypted)
```

---

## 3. Core Subsystems and Architectural Unification

### 3.1 Network Transport (Foundation: V3 Iroh / QUIC)
- **Primary Transport**: `iroh` v1.2 over QUIC. Devices dial cryptographic public keys rather than volatile IP addresses.
- **ALPN Multiplexing**:
  - `dropbridge/transfer/1`: Bidirectional control stream + unidirectional data chunk streams.
  - `dropbridge/pairing/1`: Bidirectional pairing handshake.
  - `dropbridge/probe/1`: Latency and path capability measurement.
- **Routing Hierarchy**:
  1. Direct LAN (highest priority, zero cloud contact).
  2. Direct Internet P2P via STUN/ICE hole-punching.
  3. Self-hosted / public relay fallback (iroh-relay).
- **Relay Privacy Guarantee**: The relay only forwards encrypted QUIC packets or E2E ciphertext blobs; plaintext file content is never visible to intermediate servers.

### 3.2 Security, Identity and Pairing (Merged from V2 into V3)
- **Device Identity**:
  - Primary Identity: Ed25519 signing keypair.
  - Secondary Privacy Key: X25519 Diffie-Hellman keypair for end-to-end session encryption across untrusted relays.
  - Storage: Backed by OS hardware stores (Android Keystore via FFI, Windows DPAPI via `dropbridge-identity::protector`).
- **Pairing Flow**:
  1. Enroller generates one-time QR invitation with random 256-bit token (TTL <= 120s) and connection hints.
  2. Joiner dials enroller over `ALPN_PAIRING`.
  3. Enroller issues 6-digit numeric challenge + random nonce.
  4. Joiner signs challenge using Ed25519 private key and returns signature.
  5. Enroller verifies signature, displays prompt to user (or auto-approves if headless), and confirms.
  6. Mutual exchange of connection hints; both devices upsert each other into SQLite `trusted_devices`.
  7. Graceful stream completion (`finish()`) before connection teardown to prevent connection-lost errors.
- **Trust & Revocation**:
  - Discovery != Trust. Discovered devices cannot initiate transfers without prior pairing.
  - Revoking a device removes it from SQLite and immediately aborts any active sessions.

### 3.3 Transfer Engine, Streaming & Resume (Merged from V1 & V3)
- **State Machine**:
  `Created` -> `Negotiating` -> `Transferring` -> `Verifying` -> `Completed`
  Fault/Interruption states: `Paused`, `Interrupted`, `Failed`, `Canceled`.
- **Receiver-Authoritative Resume**:
  - Receiver maintains authoritative state of written byte ranges in SQLite (`transfer_ranges`).
  - Upon reconnection or transfer initiation, receiver responds to `TransferOffer` with `TransferAccept { have_ranges }`.
  - Sender rewinds file cursor to match receiver's confirmed boundary. Sender never pushes bytes past what receiver acknowledges.
- **Memory Boundedness**:
  - Direct disk-to-network streaming using 256 KiB chunks.
  - In-flight buffer bounded to 4–32 MiB regardless of file size (1 MB, 1 GB, or 100 GB).
  - No buffering of entire files in RAM. Zero byte passing through UI or JS/Dart bridges.
- **Integrity Verification**:
  - Inline streaming BLAKE3 hashing computed concurrently with disk read/write.
  - Receiver compares full transfer digest before finalizing.
  - Data initially written to temporary files: `<filename>.dropbridge-part`.
  - On hash verification match: atomic rename `<filename>.dropbridge-part` -> `<filename>`.
  - On hash mismatch: transfer marked `Failed`, part file quarantined or purged.

### 3.4 Path Safety and Filesystem Containment (Imported from V2)
- Manifest relative paths are strictly sanitized:
  - Rejection of null bytes, leading slashes, backslashes normalized to forward slashes.
  - Rejection of Windows drive letters (`C:`), UNC paths (`\\server\share`, `//server/share`).
  - Rejection of Alternate Data Streams (`file:stream:$DATA`).
  - Rejection of path traversal segments (`.` and `..`).
  - Rejection of Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`), stripping trailing dots and spaces before validation.
  - Rejection of control characters `\x00-\x1f` and invalid filesystem characters `<>:"|?*`.
  - Canonical containment assertion: destination path must remain strictly within configured `receive_root`.

### 3.5 Collision Policy (Formalized from V1/V2/V3)
- When a destination file already exists:
  - Default policy: `Rename`.
  - Numbering scheme: `file.ext` -> `file (1).ext` -> `file (2).ext`.
  - Alternative policies: `Replace` (explicit overwrite) and `Skip`.
  - Atomic rename guarantees that existing files are never silently overwritten or truncated during partial transfers.

---

## 4. Platform Integration Targets

### Android
- Integration via `dropbridge-ffi` C-ABI.
- Intent filters: `android.intent.action.SEND` and `SEND_MULTIPLE` with MIME type `*/*`.
- `ContentResolver` -> `openFileDescriptor(uri, "r")` -> native file descriptor passed directly to Rust core (`from_raw_fd`).
- Zero full-file staging copies to app-internal cache.

### Windows 11
- Native system tray application (`dropbridge-tray`).
- Auto-start on boot via registry / Startup folder.
- Background folder watcher: monitors drop directory, enforces file write stability (checks unchanged size and exclusive access over time window) before queuing transfer.
- Windows Toast notifications for incoming pairing requests, progress, and completed transfers.
