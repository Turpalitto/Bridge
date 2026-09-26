# Manual cross-device acceptance plan

For physical sign-off (Android phone + Windows PC). Each item maps to a spec
requirement; record results per run in a table at the bottom.

## 0. Environment

- Android 14+ device (one Android 16 device for LNP checks)
- Windows 11 PC, non-admin user account
- Wi-Fi AP both devices can join; separate network for P2P/relay tests
- `dropbridge` CLI + tray build on PC; APK on phone

## 1. Pairing

| # | Step | Expected |
|---|---|---|
| 1.1 | PC: tray → Pair; phone scans QR | code shown both sides, confirm → trusted |
| 1.2 | Wrong-code entry | pairing rejected, rate-limit message after 8 tries |
| 1.3 | Expired QR (>120 s) | join fails with clear error |
| 1.4 | Re-pair same devices | new entry replaces/updates, no duplicates |

## 2. LAN transfers

| # | Step | Expected |
|---|---|---|
| 2.1 | Share 5 photos from Gallery | arrive in `DropBridge\From Phone` < 5 s |
| 2.2 | Share 4 GB video | constant memory on both sides; progress true |
| 2.3 | Drop folder with 1000 small files into To Phone | batched transfer; structure kept on phone |
| 2.4 | Wi-Fi off mid-transfer (both sides) | resumes automatically when Wi-Fi back |
| 2.5 | PC sleep mid-transfer | resumes after wake |
| 2.6 | Kill daemon mid-transfer, restart | journal resume, no duplicate bytes |
| 2.7 | Name collision (same filename twice) | `-1` suffix, both files kept |
| 2.8 | Corrupt simulate (edit .part mid-flight) | verify fails → file rejected, retry offered |

## 3. Internet paths

| # | Step | Expected |
|---|---|---|
| 3.1 | Phone on 5G, PC on home Wi-Fi | P2P direct (check path badge) |
| 3.2 | Double-NAT (symmetric) fallback | relay path used; transfer completes |
| 3.3 | Self-hosted relay only (`--relay urls=…`) | works with no n0 servers |
| 3.4 | Relay killed mid-transfer | path migrates or resumes |

## 4. Android specifics

| # | Step | Expected |
|---|---|---|
| 4.1 | Deny NEARBY_WIFI_DEVICES | graceful: LAN off, internet paths on |
| 4.2 | Android 16: deny ACCESS_LOCAL_NETWORK | same degradation + explainer |
| 4.3 | Battery 10% | discovery paused; manual send still works |
| 4.4 | App swiped away, then send from PC | wake path: notification → FGS → transfer |
| 4.5 | Share text + URL | arrive as .txt / .url stubs |

## 5. Windows specifics

| # | Step | Expected |
|---|---|---|
| 5.1 | Fresh install, autostart toggle | Run key written; survives reboot |
| 5.2 | Copy half-written file into To Phone | picked up only after stability |
| 5.3 | Firewall: public profile | mDNS blocked → manual pairing path works |
| 5.4 | Tray Quit during transfer | transfer completes before exit |
| 5.5 | Notification actions | Open folder / Approve work |

## Results log

| Date | Tester | Build | Section | Pass/Fail | Notes |
|---|---|---|---|---|---|
| — | — | — | — | — | no physical runs yet (see docs/TESTING_STATUS.md) |
