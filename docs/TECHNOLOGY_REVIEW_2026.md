# DropBridge Technology Review — 2026

**Date:** 2026-09-23 · **Status:** Approved baseline · **Review owner:** Principal Distributed Systems Architect

This review evaluates the 2026 ecosystem state for every critical DropBridge dependency.
Policy (per product spec): no dependency is adopted on reputation alone — each was checked for
latest stable version, release recency, development activity, documentation quality, Android and
Windows support, license, security status, and production readiness.

---

## 1. Transport / P2P layer

### 1.1 Iroh — **SELECTED** (see `ADR-001-network-stack.md`)

| Property | Finding |
|---|---|
| Latest stable | **iroh 1.2.0**, released 2026-09-09 (crates.io/docs.rs). 1.0 GA shipped 2026-06-15 after 4 years / 65 pre-releases |
| Maintenance | Very active (n0-computer; weekly releases in 2026; wire-protocol stability commitment from 1.0) |
| License | MIT OR Apache-2.0 |
| Android | Rust core is Android-compatible; **official Kotlin bindings restored with 1.0** |
| Windows | First-class desktop target; used in n0's own desktop apps |
| Security | TLS 1.3 inside QUIC; peer identity = Ed25519 key; relays are **blind forwarders** (cannot decrypt) |
| Production readiness | 200M+ endpoints/month on public relay infra before 1.0 GA; ~90% hole-punch success reported for 1.0 |

What iroh 1.x gives DropBridge for free: QUIC transport (`noq`), dial-by-cryptographic-key,
NAT traversal + UDP hole punching, relay fallback, direct-connection upgrade, QUIC multipath and
connection migration (Wi-Fi → cellular), ALPN protocol routing, metrics, net-reports.

What DropBridge still owns: application protocol, transfer engine, pairing/trust, discovery UX,
journaling/resume, relay *deployment* (self-hosted `iroh-relay`, see §7).

### 1.2 Quinn

Latest: **0.11.11** (2026-06-22). Excellent, mature QUIC — but it is *only* QUIC: no discovery,
no NAT traversal, no relay. Choosing Quinn means re-implementing the hardest 40% of DropBridge
(hole punching, relay infra, migration). Rejected as primary; kept as the substrate iroh itself
builds on (`noq`, quinn-lineage API), which de-risks our stream-level code.

### 1.3 libp2p

Broad, but its strength (DHT/gossip/discovery for large swarms) is not our use case (2–10 paired
devices), and its relay (circuit-v2) + NAT story is more assembly work than iroh's integrated
solution. Rejected.

### 1.4 WebRTC DataChannels

Mature NAT traversal (ICE), but: no cryptographic peer identity tied to long-lived keys,
SCTP-over-DTLS channel model is worse for multi-TB file streaming than QUIC streams, and Rust
support (webrtc-rs) is markedly less production-hardened than iroh in 2026. Rejected.

---

## 2. Cryptography & identity

| Need | Choice | Notes |
|---|---|---|
| Device identity | **Ed25519 via iroh `SecretKey`** (ed25519-dalek 3.x under the hood) | Public key *is* the address; never hand-rolled |
| Transport encryption | TLS 1.3 inside QUIC (rustls **0.23.33+**) | Certificate pinning replaced by key-based auth |
| File integrity | **BLAKE3** (`blake3 ^1.8.3`, streaming, incremental) | Distinct from transport encryption (spec §41) |
| Pairing confirmation | Short auth code derived from both public keys + pairing nonce | Mitigates MITM/stolen-QR |
| QR payload | Signed invitation, one-time token, TTL ≤ 120 s | See `PROTOCOL.md` §Pairing |

### Key storage (2026 state)

- **Android:** Android Keystore (hardware-backed where available; StrongBox on eligible OPPO
  devices). DropBridge generates the Ed25519 key in Rust, then *wraps* it under a
  Keystore-resident AES-256-GCM key so the raw private key never touches disk in plaintext.
- **Windows:** **DPAPI** (`CryptProtectData`, current-user scope) for the at-rest key blob.
  Reviewed alternatives: Windows Credential Manager (size/type friction for key blobs), CNG
  persisted keys (good, more code; kept as Phase-6 upgrade path). No plaintext keys, ever.

---

## 3. Local discovery

- **mDNS/DNS-SD** implemented natively in Rust (`mdns-sd`, pure-Rust, actively maintained;
  verified against latest docs.rs during implementation). Works without NsdManager and gives
  identical behavior on Android/Windows/Linux/macOS.
- **DropBridge beacon** (signed UDP broadcast/unicast-probe on a fixed port) as a second,
  independent mechanism — survives APs that mangle multicast.
- **Android NsdManager** intentionally *not* used: under Android 16+ Local Network Protection,
  in-process sockets need `NEARBY_WIFI_DEVICES`/`ACCESS_LOCAL_NETWORK` anyway; NsdManager's
  exemption is out-of-process only and adds a second code path without benefit. (See §6.)
- Manual IP entry exists only as a hidden debug option (spec §16).

---

## 4. Application runtime & UI

| Layer | Choice | Version (2026-09-23) |
|---|---|---|
| Android + Windows UI | **Flutter** | stable channel; Windows desktop + Android mature |
| Dart ↔ Rust bridge | **flutter_rust_bridge** | **2.13.0** (2026-08-23); codegen stable, sync+async APIs |
| Windows shell (tray) | Rust `dropbridge-tray` (Shell_NotifyIconW via `windows` crate) + WinUI-free | Windows App SDK **2.4/2.5** is current stable, but a tray resident needs Win32 lifetime semantics; WinUI/WinAppSDK surfaces are used only if we add a full settings window later |
| Windows packaging | MSIX (sideload-friendly) + signed zip fallback | documented in `WINDOWS.md` |
| Android packaging | APK + AAB, Play signing docs | `ANDROID.md` |

Bridge rule (spec §26): **Dart never touches file bytes.** The FFI surface exchanges commands,
handles/paths, metadata, progress events only; bytes move disk → Rust buffer → QUIC inside Rust.

---

## 5. Persistence & integrity

| Need | Choice |
|---|---|
| Transfer journal | SQLite via **rusqlite** (`bundled`) — WAL mode, survives crashes/restarts |
| Hashing | BLAKE3 streamed *during* transfer (no double I/O, spec §42) |
| Config/trust store | SQLite + DPAPI/Keystore-wrapped secrets |

---

## 6. Android platform constraints (2026)

- **Local Network Protection**: rolled out 25Q2→26Q2. In-process local sockets (incl. UDP/multicast
  and outbound LAN TCP) require the runtime permission **`NEARBY_WIFI_DEVICES`** (API 33+) and,
  from API level 37, **`ACCESS_LOCAL_NETWORK`** (dangerous). DropBridge requests both with an
  explanatory UX string and *degrades gracefully* to internet P2P/relay when denied (spec §15).
- **Foreground services**: user-initiated sends use `dataSync` FGS with a visible notification;
  we do **not** run a permanent background listener (spec §56). Receiving on a stopped app is
  handled via rendezvous push-hint → FGS start rules, not socket abuse (spec §57).
- **Battery**: discovery is event-driven (on app open / share intent / network change), never a
  continuous scan; OPPO ColorOS aggressive kill behavior is mitigated by not needing background
  life at all for the common send path.

---

## 7. Server-side components

| Component | Stack | Role |
|---|---|---|
| Relay | **iroh-relay 1.2** server wrapped by `dropbridge-relay` (Rust) + Docker | Blind encrypted packet forwarder; health, metrics, rate limits; self-hosted (spec §21) |
| Rendezvous | **Axum 0.8** + SQLite | Signed presence + endpoint hints + push hints. *Not* a file server (spec §17) |

Cloud never sees plaintext files, filenames, or hashes (spec §8) — relay payload is opaque
QUIC packets; rendezvous stores only signed opaque blobs + online flags.

---

## 8. Tooling / hygiene

- `cargo audit` + `cargo deny` (licenses, advisories), Dependabot for cargo+github-actions.
- `cargo fuzz`/proptest on protocol parsers, path sanitizer, resume ranges (spec §81).
- CI: fmt, clippy (-D warnings), tests, audit, Windows cross-`check` of the tray crate.
- Benchmarks: `dropbridge-bench` (disk/hash/loopback/streams matrix) + iperf3 baseline on real LAN (spec §64–66).

---

## 9. Competitive scan (spec §107)

| Product | Take-away for DropBridge |
|---|---|
| AirDrop | The bar: zero-config, one tap. (Apple-only; no Windows.) |
| Quick Share | Cross-Android/Windows, but receive prompts + Google-account assumptions create friction |
| LocalSend | Great LAN UX, open source; no internet/relay path, no resume journal → our differentiation |
| PairDrop | Browser-based convenience; not a resident bridge |
| Syncthing | Sync semantics ≠ drop semantics; heavyweight for "send this once" |
| Tailscale | Proof that identity-based networking works for normal users; but requires account/VPN install |
| KDE Connect | Rich features, dated UX, fiddly pairing on some networks |

**DropBridge's wedge:** "Pair once. Drop anywhere." — a *permanent trusted bridge*, not a
pick-a-device-per-send tool, with automatic LAN/P2P/relay routing and resumable transfers.
