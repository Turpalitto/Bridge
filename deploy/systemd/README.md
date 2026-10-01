# DropBridge Systemd Services

Production systemd unit files for running DropBridge server components on Linux hosts.

## Services

* `dropbridge-relay.service`: Blind QUIC packet forwarder (`/usr/local/bin/dropbridge-relay`).
* `dropbridge-rendezvous.service`: Presence and wake-hint rendezvous server (`/usr/local/bin/dropbridge-rendezvous`).

## Setup

1. Copy binaries to `/usr/local/bin/`:
   ```bash
   sudo cp target/release/dropbridge-relay /usr/local/bin/
   sudo cp target/release/dropbridge-rendezvous /usr/local/bin/
   ```

2. Create system user:
   ```bash
   sudo useradd -r -s /usr/sbin/nologin -d /var/lib/dropbridge dropbridge
   ```

3. Install unit files:
   ```bash
   sudo cp deploy/systemd/*.service /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now dropbridge-relay dropbridge-rendezvous
   ```

4. Verify status:
   ```bash
   systemctl status dropbridge-relay
   systemctl status dropbridge-rendezvous
   ```
