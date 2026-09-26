# DropBridge Threat Model

Assets: private identity keys; file contents in transit/at rest; the trust
relationship itself; user attention (no surprise executions).

Trust anchors: each device's Ed25519 key (hardware-wrapped at rest), and the
human pairing confirmation.

## 1. Adversary classes & mitigations

### MITM on any network path
Every transport connection is TLS 1.3 inside QUIC authenticated by the peer's
Ed25519 device key, which we pin at pairing time. An attacker who controls the
network (hotel Wi-Fi, rogue AP, relay operator) can observe timing/size but
cannot decrypt or substitute content. **Status: mitigated by construction.**

### Spoofed device ("fake laptop")
Beacons/discovery records are *signed* by the device key and only used as
dialing hints — trust decisions never come from discovery. A spoofer can make
us dial them, but the QUIC handshake fails against the pinned key. Pairing QR
contains the enroller key; the joiner asserts `conn.remote_id == QR key`.
**Status: mitigated.**

### QR replay / stolen pairing token
Token is one-time, CSPRNG, TTL ≤ 120 s, consumed on success. If stolen within
TTL, the attacker can only pair *as themselves*: the enroller sees an unknown
device name + auth code and denies (human gate). Replays after consumption
fail. **Status: mitigated; residual risk = shoulder-surfing the QR within 2
minutes, accepted for v1, documented.**

### Compromised/malicious relay
The relay forwards opaque QUIC packets addressed by EndpointId; it holds no
keys, sees no filenames/hashes/content (spec §22). A compromised relay can
delay/drop (availability) or attempt downgrade of *routing*, but not of
*content*. We pin protocol versions in Hello; unknown versions refuse.
**Status: content confidentiality intact; availability attack surface noted.**

### Malicious LAN peer / untrusted device
All session entry points check the trust registry by authenticated device id
BEFORE any processing; untrusted peers get `HelloAck{ok:false}` and nothing
else. Pairing attempts are rate-limited (8/hour/device). **Status: mitigated.**

### Malicious filenames / path traversal
Receiver sanitizes every path (strictest of POSIX+Windows rules) and
re-verifies canonical containment under the receive root (defense in depth).
Symlinks are not followed during planning; the receive tree is created fresh.
**Status: mitigated; proptest coverage in `dropbridge-protocol`.**

### Oversized metadata / manifest bombs
Hard protocol limits: 4 MiB frames, 200k manifest entries, 512-char paths,
16 streams, 16 MiB chunks, 8 concurrent transfers, lying length prefixes
rejected by the frame decoder. **Status: mitigated.**

### Disk exhaustion
Free-space check before any part file is created; transfer refused with
`NotEnoughDiskSpace` (spec §54). **Status: mitigated (unix statvfs; Windows
check ships with the tray layer — tracked).**

### DoS via protocol fuzzing
Parsers are serde/postcard with strict size caps; malformed frames close the
stream, never panic (property tests cover framing and paths; `cargo fuzz`
targets are the next hardening step — tracked in Phase 6). **Status: largely
mitigated; fuzz campaign pending.**

### Credential theft at rest
Private keys: DPAPI (Windows), Android Keystore wrapping (app layer),
0600-file fallback only where nothing better exists (dev/CI). Keys never log,
never leave the device, never appear in rendezvous payloads. **Status:
mitigated on shipped platforms.**

### Downgrade attacks
Protocol version is exchanged in Hello and pinned per ALPN; relay cannot alter
handshake messages (inside TLS). Older clients refusing newer versions fail
closed, not open. **Status: mitigated.**

### Malicious file execution (transfer ≠ execution)
DropBridge never auto-opens/executes received files; Windows SmartScreen and
file-type policies remain in charge. Documented in WINDOWS.md. **Status:
mitigated by policy.**

## 2. Privacy (spec §8)

* Relay: sees packet sizes/timing/EndpointIds only.
* Rendezvous: stores signed opaque blobs + timestamps; cannot forge (no key),
  cannot read (blob content is up to the client; we ship AddrHints which are
  public reachability info by nature).
* Logging: device ids, sizes, timings. No filenames in core logs; no file
  contents anywhere; no clipboard content (clipboard bridge is Phase 2 and
  default-off).

## 3. Residual risks (accepted for v1, tracked)

1. Shoulder-surfing a pairing QR within its 2-minute window.
2. A trusted-but-compromised device can exfiltrate what it is trusted for
   (trust revocation is the remedy; per-file confirmation mode is Phase 6).
3. Availability attacks against n0 public relays — mitigated by self-hosting
   (`deploy/relay`) and LAN-first behavior.
4. Android Keystore wrapping lives in the app layer; CLI keys on Android
   developer devices use the file fallback.
