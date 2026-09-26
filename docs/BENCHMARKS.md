# DropBridge Benchmarks

Methodology: [benchmarks/README.md](../benchmarks/README.md). Rule: data
beats hypothesis — every tuning decision below cites a measured number.

## 1. Loopback engine numbers (CI, GitHub runner)

Captured automatically by `dropbridge-bench --quick` on every push that
passes tests; latest successful table lives in the CI run summary.
Template:

| metric | size | result |
|---|---|---|
| disk write | 64 MB | … MiB/s |
| disk read | 64 MB | … MiB/s |
| blake3 hash | 64 MB | … MiB/s |
| loopback transfer (default streams) | 16 MB | … MiB/s |

> ⚠️ Loopback numbers overstate LAN throughput (no NIC, warm caches).
> They exist to catch engine regressions, not for marketing.

## 2. Real-LAN numbers (physical devices)

_Not yet measured — no physical hardware pair is available in the current
development environment. This is stated plainly rather than estimated
(project rule)._ Procedure when hardware is available:

1. Same AP, 5 GHz, 160 MHz, devices 1 m apart.
2. `iperf3` baseline on the same link.
3. `dropbridge send` of 1 GiB (single file), 10k×4 KiB (small-file batch),
   mixed 5 GB directory.
4. Record path (must show LAN direct), negotiated chunk size + stream count,
   CPU on both ends.

## 3. Tuning decisions from data

| Knob | Current | Why |
|---|---|---|
| chunk size | 1 MiB default (64 KiB–16 MiB negotiated) | large enough for throughput, small enough for resume granularity; matrix will re-validate |
| stream count | 4 default (≤16) | QUIC streams multiplex on one UDP socket; matrix job tests 1/2/4/8 |
| compression | off for JPEG/MP4/ZIP/APK; conditional rule otherwise | zstd-3 ratio sample in bench output; CPU cost must beat retransmit savings |
| path hysteresis | ×1.15 switch threshold | avoids route flapping; scored LAN=100/P2P=80/relay=40 |

## 4. History

| Date | Change | Before | After | Notes |
|---|---|---|---|---|
| — | engine landing | — | first CI numbers | pending first green bench run |
