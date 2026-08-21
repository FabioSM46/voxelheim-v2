#!/usr/bin/env bash
# Drives the real client against the real server over TLS, and checks the three
# things neither side's own tests can see.
#
# **This is not in CI, and the reason is worth knowing before anyone tries to put it
# there.** The client is a Bevy application and opens a window, so it needs a display;
# and CI runs the Go and Rust gates in separate jobs with separate toolchains, so no
# job has both binaries. What the two workspaces test on their own is each half of the
# encryption. What only this can test is the two halves *meeting* — and the first time
# it ran it found a real bug that every unit test on both sides had passed over: the
# client fed rustls a socket read and discarded whatever one `read_tls` call did not
# take, which desynchronised the record stream the moment a read carried more than one
# TLS record.
#
# Run it after touching internal/transport, internal/certs, or client/src/net/tls.rs.
#
#   bash scripts/interop-check.sh
#
# Needs: a Go toolchain, a Rust toolchain, and a display for the client.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
WORK=$(mktemp -d)
PORT=${PORT:-7799}
SERVER_PID=""

cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() { echo "[FAIL] $*" >&2; exit 1; }
pass() { echo "[PASS] $*"; }

start_server() {
  "$WORK/voxelheimd" -listen "127.0.0.1:$PORT" -world-dir "$WORK/world" >"$1" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 40); do
    grep -q "voxelheimd listening" "$1" 2>/dev/null && return 0
    sleep 0.25
  done
  fail "the server did not start; see $1"
}

stop_server() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}

run_client() {
  XDG_DATA_HOME="$WORK/clientdata" timeout 15 \
    "$REPO_ROOT/client/target/debug/voxelheim-client" \
    --server "127.0.0.1:$PORT" --name Eivor >"$1" 2>&1 || true
}

echo "building both sides..."
(cd "$REPO_ROOT/server" && go build -o "$WORK/voxelheimd" ./cmd/voxelheimd)
(cd "$REPO_ROOT/client" && cargo build --workspace --locked --quiet)

mkdir -p "$WORK/world" "$WORK/clientdata"

# ---- 1. a first connection is encrypted, admitted, and pins what it saw ----
start_server "$WORK/server1.log"
FINGERPRINT=$(grep -o 'certificate_sha256=[a-f0-9]*' "$WORK/server1.log" | head -1 | cut -d= -f2)
[ -n "$FINGERPRINT" ] || fail "the server logged no certificate fingerprint"

run_client "$WORK/client1.log"
grep -q "session established" "$WORK/client1.log" \
  || fail "the client never established a session; see $WORK/client1.log"
grep -qi "decrypt\|protocol error" "$WORK/client1.log" \
  && fail "the session established and then broke; see $WORK/client1.log"
pass "a first connection is established over TLS"

PIN_FILE="$WORK/clientdata/voxelheim/identity/127.0.0.1_$PORT.pin"
[ -f "$PIN_FILE" ] || fail "no pin was written to $PIN_FILE"
PINNED=$(tr -d '[:space:]' <"$PIN_FILE")
[ "$PINNED" = "$FINGERPRINT" ] \
  || fail "the client pinned $PINNED but the server announced $FINGERPRINT"
pass "the client pinned the fingerprint the server logged"

# ---- 2. the identity survives, under encryption ----
run_client "$WORK/client2.log"
grep -q "a returning character" "$WORK/client2.log" \
  || fail "a reconnect did not come back as the same player; see $WORK/client2.log"
pass "a reconnect returns as the same character"

# ---- 3. a substituted certificate is refused, and no token is presented ----
stop_server
rm -f "$WORK/world/server-cert.pem" "$WORK/world/server-key.pem"
start_server "$WORK/server2.log"
NEW_FINGERPRINT=$(grep -o 'certificate_sha256=[a-f0-9]*' "$WORK/server2.log" | head -1 | cut -d= -f2)
[ "$NEW_FINGERPRINT" != "$FINGERPRINT" ] \
  || fail "the server presented the same certificate after its key was deleted"

run_client "$WORK/client3.log"
grep -q "different certificate than the one pinned" "$WORK/client3.log" \
  || fail "a substituted certificate was not refused; see $WORK/client3.log"
grep -q "session established" "$WORK/client3.log" \
  && fail "a session was established against a substituted certificate"
grep -q "tls: handshake failure" "$WORK/server2.log" \
  || fail "the server did not see the handshake refused, so the client may have said something first"
pass "a substituted certificate is refused before anything is sent"

# ---- 4. an identity with no pin is refused, and nothing is sent ----
# The upgrade case: every player carried over from the plaintext transport has an
# identity file and no pin. Simulated by deleting the pin and keeping the identity,
# which is exactly the state such a player is in on their first connection.
stop_server
start_server "$WORK/server3.log"
rm -f "$PIN_FILE"

run_client "$WORK/client4.log"
grep -q "has never verified its certificate" "$WORK/client4.log" \
  || fail "an identity was presented to a server that had never been pinned; see $WORK/client4.log"
grep -q "session established" "$WORK/client4.log" \
  && fail "a session was established while holding an unverified identity"
# The TCP connection is made before the pin is read — the socket is what the handshake
# runs over — so "connection accepted" is expected and proves nothing either way. What
# must never appear is a session that got past the handshake, because that is the only
# point at which a token would have crossed the wire.
grep -q "session admitted" "$WORK/server3.log" \
  && fail "the server admitted a session the client should have refused to open"
[ -f "$PIN_FILE" ] \
  && fail "a refused connection pinned the certificate anyway"
pass "an identity is never presented to a server that was never pinned"

echo
echo "interop: 5/5"
