#!/usr/bin/env bash
# Single-machine LAN smoke test for the DropBridge CLI.
# Flow: pair (QR string roundtrip) -> receiver daemon -> send directory ->
# byte-compare the received tree. Exit 0 = pass.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${DROPBRIDGE_BIN:-}
if [[ -z "$BIN" ]]; then
  echo ">> building dropbridge-cli (release)"
  cargo build --release -p dropbridge-cli
  BIN=target/release/dropbridge
fi

WORK=$(mktemp -d /tmp/dropbridge-smoke.XXXXXX)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT

mkdir -p "$WORK/payload/sub"
head -c 1048576 /dev/urandom > "$WORK/payload/big.bin"
echo "hello dropbridge" > "$WORK/payload/note.txt"
head -c 65536 /dev/urandom > "$WORK/payload/sub/inner.bin"

COMMON_RECV_ARGS=(--relay disabled)

echo ">> step 1: pairing (receiver node shows QR; sender joins)"
"$BIN" --state "$WORK/recv-state" --name smoke-laptop --kind laptop --port 47501 \
  --receive "$WORK/recv" "${COMMON_RECV_ARGS[@]}" pair --auto-confirm \
  > "$WORK/pair.log" 2>&1 &
PAIR_PID=$!
QR=""
for _ in $(seq 1 30); do
  QR=$(grep -o 'dropbridge://[^ ]*' "$WORK/pair.log" | head -1 || true)
  [[ -n "$QR" ]] && break
  sleep 0.5
done
if [[ -z "$QR" ]]; then echo "FAIL: no invitation"; cat "$WORK/pair.log"; exit 1; fi

"$BIN" --state "$WORK/send-state" --name smoke-phone --kind phone \
  --receive "$WORK/send-recv" "${COMMON_RECV_ARGS[@]}" join "$QR"
# pairing node may stay alive; give it up to 30 s then stop it
for _ in $(seq 1 30); do
  kill -0 "$PAIR_PID" 2>/dev/null || break
  sleep 1
done
kill "$PAIR_PID" 2>/dev/null || true
wait "$PAIR_PID" 2>/dev/null || true
sleep 1

echo ">> step 2: receiver daemon (same state dir -> same identity + trust)"
"$BIN" --state "$WORK/recv-state" --name smoke-laptop --kind laptop \
  --receive "$WORK/recv" "${COMMON_RECV_ARGS[@]}" daemon --port 47501 \
  > "$WORK/recv.log" 2>&1 &
sleep 4

echo ">> step 3: send payload"
"$BIN" --state "$WORK/send-state" --name smoke-phone --kind phone \
  --receive "$WORK/send-recv" "${COMMON_RECV_ARGS[@]}" \
  send smoke-laptop "$WORK/payload"

echo ">> step 4: verify received tree byte-for-byte"
for rel in big.bin note.txt sub/inner.bin; do
  DST=$(find "$WORK/recv" -path "*$rel" -type f | head -1)
  if [[ -z "$DST" ]]; then
    echo "FAIL: missing $rel"; ls -R "$WORK/recv"; exit 1
  fi
  cmp -s "$WORK/payload/$rel" "$DST" || { echo "FAIL: mismatch $rel"; exit 1; }
done

echo "PASS: LAN smoke (pair -> daemon -> send -> verify)"
