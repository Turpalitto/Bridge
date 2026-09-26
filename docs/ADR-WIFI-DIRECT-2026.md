# ADR-WIFI-DIRECT-2026: Fast Direct Mode spike verdict

- **Status:** Deferred past MVP (spike verdict: **do not ship automatically**; re-evaluate after Phase 4)
- **Date:** 2026-09-23

## Question

Should DropBridge use Wi-Fi Direct (P2P) as a transfer path between Android and Windows to gain
throughput or to work without a common network?

## Findings (2026 state)

**Android side.** Wi-Fi Direct is mature (`WifiP2pManager`), needs `NEARBY_WIFI_DEVICES`
(API 33+) plus fine/coarse location on older releases. Persistent groups + vendor quirks on
OPPO/ColorOS (group-owner negotiation failures, MAC randomization breaking saved groups) are
well-documented. On many devices, forming a P2P group *drops or degrades the station Wi-Fi*,
i.e. it can kill the phone's Internet for the duration of the transfer — spec §24 forbids
doing this automatically.

**Windows side.** Wi-Fi Direct on Windows 11 desktop is the weak link: `WiFiDirectDevice`
(WinRT) and the legacy `WFD_*` APIs exist, but desktop adapter/driver support is inconsistent;
Microsoft's own Quick Share chose BLE-advertised Wi-Fi Direct only with careful pairing UX;
many office laptops have drivers where P2P GO formation fails outright. Reconnect UX after
sleep is unreliable without OS-level pairing state the user must manage.

**Throughput.** Real-world WFD tops out well below a normal 5 GHz infrastructure link
(single-stream GO, power-save quirks). Against DropBridge's LAN-direct path (already
line-rate on the same router), WFD usually *loses* — its only structural win is the
"no common network at all" case.

**That case is already covered** by iroh's internet P2P + relay fallback with E2E encryption —
which works even across networks, not just adjacent devices.

## Decision

1. MVP ships **without** Wi-Fi Direct. The path manager's candidate set is:
   `LAN direct → Internet direct P2P → self-hosted/n0 relay`.
2. Architecture keeps a `PathCandidate` abstraction so a `WifiDirect` candidate can be added
   without redesign (its hooks are stubbed + unit-tested, but never scored above LAN in MVP).
3. Re-open only if all gates pass: (a) Windows 11 26H1 driver telemetry shows ≥95% GO
   formation success in our test matrix; (b) station-Wi-Fi survival confirmed on OPPO test
   devices; (c) reconnect-after-sleep works without user action; (d) measured throughput beats
   LAN-direct on the same hardware. Data over hope (spec §102).

## Consequences

- Fewer Android permissions (no mandatory location-era prompts for P2P).
- No risk of breaking the user's Internet mid-transfer (spec §24 honored).
- "No common network" scenarios still work via internet P2P/relay — the product promise holds.
