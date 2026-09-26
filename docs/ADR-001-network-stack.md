# ADR-001: Network stack selection

- **Status:** Accepted (2026-09-23)
- **Deciders:** Principal Distributed Systems Architect, Network/QUIC/P2P Engineer, Applied Cryptography Engineer
- **Context:** DropBridge must move files between Android and Windows across every topology
  (same LAN, mixed bands, cross-network, 5G ↔ home NAT, hotel Wi-Fi) with no user networking
  knowledge. The stack must provide: encrypted transport, cryptographic device identity, NAT
  traversal, relay fallback, direct upgrade, connection migration/resume, Android + Windows
  support, self-hostable relay.

## Options evaluated

### Option A — Iroh 1.x (chosen)

QUIC transport + dial-by-Ed25519-key + integrated NAT traversal + stateless relays.
Verified state on 2026-09-23: **iroh 1.2.0** (2026-09-09); 1.0 GA on 2026-06-15 with
wire-protocol stability commitment; official Kotlin/Swift/Python/Node bindings; ~90%
hole-punch success rate reported; QUIC multipath + migration landed in 1.0; `iroh-relay`
self-hostable server crate with access control, metrics, rate limiting.

### Option B — Raw Quinn + in-house discovery/traversal/relay

quinn **0.11.11** (2026-06-22) is a superb QUIC implementation, but traversal/hole-punching,
relay design, migration and address discovery would all be greenfield. That is months of
distributed-systems work plus permanent maintenance of the highest-risk code in the product.

### Option C — libp2p (rust-libp2p)

Strong for swarm/DHT use cases we don't have. Circuit-relay v2 + Identify + AutoNAT assembly
is more moving parts than iroh's integrated stack; identity model is less ergonomic for
"my laptop" UX; Rust API still churning relative to iroh's post-1.0 stability promise.

### Option D — WebRTC DataChannels

ICE is battle-tested, but: no long-lived cryptographic device identity; SCTP channels are
inferior to QUIC streams for huge-file throughput and resume; Rust ecosystem (webrtc-rs)
notably less hardened than iroh in 2026; TURN self-hosting is heavier than iroh-relay.

## Scorecard

| Criterion (weight) | Iroh 1.2 | Quinn+own | libp2p | WebRTC |
|---|---|---|---|---|
| Throughput potential (QUIC streams) | ● high | ● high | ◐ med | ◐ med |
| Latency / setup | ● fast, 0-RTT capable | ● fast | ◐ slower | ◐ ICE setup |
| NAT success rate | ● ~90% + relay=100% fallback | ○ we must build it | ◐ partial | ● high (ICE+TURN) |
| Relay support | ● built-in, blind, self-hostable | ○ build it | ◐ circuit-v2 | ◐ TURN (not blind by default) |
| Android | ● Rust + official Kotlin bindings | ◐ Rust only | ◐ | ◐ heavy |
| Windows | ● | ● | ● | ◐ |
| API maturity | ● stable since 1.0 (2026-06) | ● (QUIC only) | ◐ | ◐ |
| Maintenance risk | ● n0-funded, weekly releases | ○ all ours | ◐ | ◐ |
| Complexity we own | ● low (transport solved) | ○ very high | ◐ high | ◐ high |
| License | MIT/Apache-2.0 | MIT/Apache | MIT | Apache |
| Security | ● Ed25519 identity, TLS1.3-in-QUIC, blind relays | ◐ DIY auth layer | ◐ | ◐ PKI mismatch |
| Battery | ● idle relays are cheap HTTPS keepalives | ◐ | ◐ | ○ ICE keepalives |
| Binary size | ◐ ~+4–8 MB (tokio/rustls) | ● smaller | ○ large | ○ large |
| Implementation risk | ● low-medium | ○ high | ◐ | ◐ |

● strong · ◐ acceptable · ○ weak/risky

## Decision

**Adopt Iroh 1.x (=1.2.0 pinned) as the network stack.** It eliminates precisely the parts
of the spec that are highest-risk to build in-house (hole punching, relay infra, migration)
while matching our privacy model (blind relays, identity-keyed connections) and our platforms
(Rust core + official Kotlin bindings for Android).

## Consequences & mitigations

- **+** We own the application protocol, transfer engine, pairing, and relay deployment.
- **+** LAN-only (offline) operation is guaranteed by injecting discovered LAN addresses as
  `TransportAddr::Ip` into `EndpointAddr` and tolerating relay absence.
- **−** We depend on n0's stack ⇒ mitigations: pin exact versions; all protocol state is ours;
  `RelayMode::custom` lets us run fully self-hosted; Quinn escape hatch remains because iroh
  exposes its QUIC layer.
- **−** Binary size grows ⇒ measured in `BENCHMARKS.md`; acceptable (<15 MB app delta).
- **Watch items:** iroh 1.3.x API drift (we pin `=1.2.0`), Android VPD/firewall quirks
  (physical test queue), multipath migration claims (**not** asserted until we test them —
  until then resume-at-app-layer is the guaranteed behavior, spec §6).
