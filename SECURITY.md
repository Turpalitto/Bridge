# DropBridge Security

Read together with docs/THREAT_MODEL.md. This file is the engineering view:
what protects what, and where the sharp edges are.

## 1. Trust anchors

| Asset | Storage | Protection |
|---|---|---|
| Device identity key (Ed25519) | per-platform keystore | Android Keystore/StrongBox; Windows DPAPI (`CryptProtectData`, UI-forbidden) |
| Trust registry | JSON in state dir | integrity = signatures everywhere else; treated as local-user data |
| Transfer journal | SQLite WAL | local-only; file paths + ranges (no content) |
| Received files | receive dir | OS user permissions |

The identity key is the root of everything: it authenticates QUIC
connections (TLS 1.3 certificates = long-term device keys), signs pairing
messages and presence blobs. Losing it = re-pair devices.

## 2. Transport security

* **QUIC/TLS 1.3** via iroh (rustls, FIPS-capable provider options). No
  custom handshake code in DropBridge — we use iroh's audited stack.
* **Certificate pinning by identity**: the peer's expected public key is
  pinned before dial (from the trust registry). A MITM with a valid public
  CA cert still fails because the key doesn't match the pinned DeviceId.
* **Relay blindness**: iroh relays forward opaque QUIC *packets* — they do
  not terminate the TLS session, so they cannot decrypt anything. A relay
  sees only metadata: endpoints' DeviceIds, connection timing, byte counts.
  Rendezvous stores only signed opaque blobs.
* **No plaintext fallback**: relay URLs must be `https://`; LAN connections
  are still TLS-encrypted (same keys), just unrouted.

## 3. Pairing security

* QR contains: enroller DeviceId, one-time token, enroller's current address
  hints. Token lifetime 120 s, single successful use.
* Joiner dials the **pinned enroller key** — a fake QR pointing at an
  attacker endpoint fails TLS pinning.
* Auth code = BLAKE3(token ‖ enroller_id ‖ joiner_id) truncated to 6
  digits, compared on both screens (human confirmation, spec §15).
* Rate limits: ≤ 8 pairing attempts/hour per device; invitations expire.
* After pairing, the one-time token is deleted on both sides.

## 4. Transfer integrity

* Every chunk carries (session, file_id, chunk_idx, offset, len); receiver
  writes only to the journaled offset — replays/reorders are idempotent.
* **BLAKE3 end-to-end**: sender hashes while streaming; receiver hashes
  while writing; `Msg::Verify{hash}` mismatch ⇒ file rejected, never
  renamed, sender notified (spec §37).
* **Atomic landing**: `.dropbridge-part` → rename only after verify.
  A power loss mid-transfer leaves no half-visible files.
* **Path traversal hardening**: manifest entries are normalized; any
  absolute path, `..` escape, symlink component, or non-portable character
  set rejects the whole transfer (tested).

## 5. Resource abuse defenses

* Frame caps (4 MiB control frames, 16 MiB chunks), manifest entry cap
  (200k), stream count cap (16), concurrent transfer cap (8).
* Disk-space precheck before accepting (receiver can refuse with reason).
* Relay rate limits (per-client Mbit/s) on self-hosted relays.
* Pairing and presence endpoints rate-limited.

## 6. What we deliberately do NOT do

* No account system, no cloud file storage (spec §8/§22).
* No custom crypto. Ed25519/BLAKE3/TLS from maintained libraries only.
* No "convenience" plaintext mode, no key export features.

## 7. Known residual risks (honest list)

1. **Local malware with the same OS user** can read received files and use
   the unwrapped key while it's in memory — out of scope for this layer
   (that's OS-level compromise).
2. **First-pairing MITM on the QR channel**: if an attacker can replace the
   displayed QR *and* the user ignores the auth-code comparison, impersonation
   is possible. Mitigation is the human check; keep it visible in UX.
3. **Relay metadata**: a hostile relay sees who talks to whom and how much.
   Content stays encrypted; use your own relay to minimize exposure.
4. **Android Keystore fallback**: devices without Keystore fall back to an
   encrypted-file protector — weaker at rest, flagged in-app (planned UX).
5. **Journal reveals filenames** to anyone with local file access.

## 8. Reporting issues

Open a GitHub issue marked `security`, or email the maintainer. Please
include: affected component (core/relay/rendezvous/app), reproduction, and
impact. We aim to triage within 72h.
