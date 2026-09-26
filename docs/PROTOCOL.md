# DropBridge Protocol v1

Status: implemented in `rust/crates/dropbridge-protocol`. Transport: QUIC via
iroh 1.2 (TLS 1.3 inside QUIC, authenticated by Ed25519 device keys).

## ALPNs

| ALPN | Purpose |
|---|---|
| `dropbridge/transfer/1` | file transfer sessions |
| `dropbridge/pair/1` | accountless pairing |
| `dropbridge/probe/1` | path probing (RTT/bandwidth), reserved |

## Control channel

One bidirectional QUIC stream per connection. Frames:

```
[u32 BE length][postcard payload]        (length ≤ 4 MiB)
```

### Messages (`Msg`)

```
Hello { version, caps }                    → immediately after connect
HelloAck { version, caps, ok, reason }     → accept/refuse the connection
TransferOffer { session, manifest, note }  → propose a transfer
TransferAccept { session, chunk_size, stream_count, have_ranges }
TransferReject { session, reason }
SessionSync { session, have_ranges }       → mid-transfer re-sync (resume)
Verify { session, hash }                   → sender: all chunk streams done
Complete { session, ok, detail }           → receiver: verification result
Cancel { session, reason }
Ping { nonce } / Pong { nonce }
Error { code, msg }
```

`session` is a u64 chosen by the sender and **stable across retries of the
same logical transfer** — this is what makes retries resume instead of
restart: the receiver journal is keyed by session and answers with
`have_ranges` (completed byte ranges per file).

### Capabilities (spec §49)

```
Capabilities {
  protocol_versions: [u32],      // highest first
  supports_resume: bool,
  supports_folders: bool,
  supports_parallel_streams: bool,
  supports_compression: bool,
  max_chunk_size: u64,
  max_concurrency: u32,
  device_name: String,           // display only
  device_kind: DeviceKind,
  app_version: String,
}
```

### Manifest

```
Manifest { entries: [FileEntry], total_bytes }
FileEntry { rel_path, size, mtime_secs, file_id }
```

Limits (DoS guardrails, enforced by both sides): ≤ 200 000 entries,
path ≤ 512 chars, every path sanitized per *Path safety* below.

## Data streams

The sender opens `stream_count` unidirectional streams (default 4, max 16 —
benchmark-driven, see BENCHMARKS.md). Each stream carries concatenated chunks:

```
[u32 BE header_len][postcard ChunkHeader][raw bytes] …
ChunkHeader { session, file_id, chunk_idx, offset, len }
```

Chunk size negotiated in `TransferAccept` (default 1 MiB, 64 KiB–16 MiB).
QUIC provides reliability and per-stream flow control; there are **no
application-level per-chunk ACKs** — resume state comes from the receiver's
persisted ranges.

## Integrity digest

`hash` in `Verify` is BLAKE3 over, for each file in manifest order:
`rel_path bytes || 0x1F || file bytes`.
Integrity hashing is separate from transport encryption (spec §41).

## Pairing channel

QR payload `dropbridge://pair/<base32(postcard PairInvitation)>`:

```
PairInvitation {
  magic "DBPQ", version, device_id, device_name, device_kind,
  pairing_token: [u8;32], expires_at, relay_hints[], direct_hints[]
}
```

Messages on `dropbridge/pair/1`:

```
Request { token, device_name, device_kind }   joiner → enroller
Challenge { auth_code }                       enroller → joiner
Confirm { auth_code }                         joiner → enroller
Done | Fail { reason }                        enroller → joiner
```

`auth_code = BLAKE3(token || enroller_id || joiner_id) mod 10^6`.
Token: CSPRNG, one-time, TTL ≤ 120 s. See THREAT_MODEL.md.

## Path safety (receiver MUST enforce)

Rejected: `..` components, absolute/UNC paths, drive letters, `name:stream`
(ADS), Windows device names (CON/PRN/AUX/NUL/COM*/LPT*), control chars,
reserved characters `< > : " | ? *`, trailing dots/spaces, > 255-char
components, > 512-char paths. After sanitizing, the receiver additionally
verifies the canonical target stays under the receive root.

## Versioning (spec §50)

* Unknown optional fields appended to messages are tolerated by serde
  evolution rules; unknown capabilities are ignored.
* Breaking changes require a new ALPN (`dropbridge/transfer/2`) and version
  negotiation in Hello/HelloAck; 1.0 refuses politely, never crashes.
