#!/usr/bin/env bash
# Bring up the self-hosted relay stack (Docker + Caddy TLS).
set -euo pipefail
cd "$(dirname "$0")/../deploy/relay"
docker compose up -d --build
echo "relay: see Caddyfile domain; metrics on 127.0.0.1:9090"
