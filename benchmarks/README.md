# DropBridge benchmarks

Numbers live in [docs/BENCHMARKS.md](../docs/BENCHMARKS.md). This directory
holds raw CSV captures and the methodology so results are reproducible.

## Method

### Loopback (CI, every push)

`dropbridge-bench --quick` on GitHub runners:

* disk write / read: 1 MiB buffered streaming, deterministic pseudo-content
* BLAKE3: streamed hash during the same read pass (matches production)
* full loopback transfer: two in-process endpoints, relay disabled —
  pair → offer → N chunk streams → BLAKE3 verify → atomic rename
* measures the *engine*, not a NIC: same-machine loopback overstates real
  LAN throughput. Treat as a regression detector.

### Stream-count & chunk-size matrix

`dropbridge-bench --matrix` repeats the loopback transfer with 1/2/4/8
streams. Tuning knobs are negotiated per-session via capabilities
(`max_chunk_size`, `max_concurrency`); overrides exist in `NodeConfig`
(`override_chunk_size`, `override_stream_count`) exactly so this matrix can
be run without protocol changes.

### Real LAN (manual, gold standard)

Two physical devices, `iperf3` reference on the same link, then:

```bash
dropbridge-bench --dir /tmp/dbbench --matrix    # sender side equivalent:
# or with the CLI:
dropbridge send <peer> 1GiB.bin   # timed with progress events
```

Record: hardware, NIC driver, AP model, distance, channel width, OS power
plan. Append to docs/BENCHMARKS.md with date.

### Compression gate

`dropbridge-bench` prints zstd-3 ratios for a compressible sample and a
media-like sample. Policy: **never auto-compress JPEG/MP4/ZIP/APK** — the
matrix only informs the *conditional* rule (compress iff extension unknown
AND sample ratio > 1.15), which is off by default.

## Files

* `results.csv` — appended rows: date, platform, test, size, MiB/s, notes
* run `dropbridge-bench --help` for all flags
