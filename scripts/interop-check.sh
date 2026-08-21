#!/usr/bin/env bash
# Drives the real client against the real server over TLS, and checks the things
# neither side's own tests can see.
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
# **What it checks got narrower when trust on first use was removed, and the reason is
# worth stating rather than leaving as a shorter script.** The client used to pin the
# first certificate it saw into a file, so this script could read that file, delete it,
# swap the server's key and watch the refusal happen — all of it on disk, all of it
# reachable from bash. The expected fingerprint now comes from an account service's
# server list, and standing one of those up needs a Discord application and a browser.
# So the *refusal* is asserted in `client/src/net/tls.rs`'s own tests, where the
# expectation is a value and the check is the one line under test; what stays here is
# the half no unit test on either side can reach — a real handshake between the two
# real stacks — plus the property that survived the pin file: an address in no list is
# never shown the identity this client holds.
#
# The client is run with `--server`, which is the development path: no list, no
# expectation, and therefore no identity presented. That is the behaviour under test in
# check 2, not a limitation of the script.
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

run_client() {
  XDG_DATA_HOME="$WORK/clientdata" timeout 15 \
    "$REPO_ROOT/client/target/debug/voxelheim-client" \
    --server "127.0.0.1:$PORT" --name Eivor >"$1" 2>&1 || true
}

echo "building both sides..."
(cd "$REPO_ROOT/server" && go build -o "$WORK/voxelheimd" ./cmd/voxelheimd)
(cd "$REPO_ROOT/client" && cargo build --workspace --locked --quiet)

mkdir -p "$WORK/world" "$WORK/clientdata"

# ---- 1. a connection is encrypted, admitted, and speaks the protocol ----
#
# The check that found the record-layer bug, and the reason this script exists. A
# session that establishes and then keeps running is the whole assertion: the world
# starts streaming immediately afterwards, which is what fills one socket read with
# several TLS records.
start_server "$WORK/server1.log"
FINGERPRINT=$(grep -o 'certificate_sha256=[a-f0-9]*' "$WORK/server1.log" | head -1 | cut -d= -f2)
[ -n "$FINGERPRINT" ] || fail "the server logged no certificate fingerprint"

run_client "$WORK/client1.log"
grep -q "session established" "$WORK/client1.log" \
  || fail "the client never established a session; see $WORK/client1.log"
grep -qi "decrypt\|protocol error" "$WORK/client1.log" \
  && fail "the session established and then broke; see $WORK/client1.log"
pass "a connection is established over TLS and the record stream survives it"

# ---- 2. nothing was written down, and nothing was presented ----
#
# **The property that replaced the pin file.** `--server` names an address that is in
# no list, so nothing states which certificate to expect there — and the client
# therefore presents no identity and keeps none. Checked on disk, because the identity
# file is where a token would have to be kept for a later launch to present it, and
# checked for a pin file too: that path is removed, not merely unused, so anything
# writing one back is a regression this script should catch.
IDENTITY_DIR="$WORK/clientdata/voxelheim/identity"
if [ -d "$IDENTITY_DIR" ]; then
  STRAY=$(find "$IDENTITY_DIR" -type f | head -5)
  [ -z "$STRAY" ] \
    || fail "an unlisted session wrote credentials to disk: $STRAY"
fi
grep -q "a returning character" "$WORK/client1.log" \
  && fail "an unlisted session presented a stored identity"
pass "an address in no list is shown no identity and leaves nothing behind"

# ---- 3. a second launch is a new character, not a remembered one ----
#
# The same statement from the other side, and the one that would have caught a pin
# file quietly coming back: with nothing written down, the second launch cannot be a
# returning session however the first one went.
run_client "$WORK/client2.log"
grep -q "session established" "$WORK/client2.log" \
  || fail "the second launch never established a session; see $WORK/client2.log"
grep -q "a returning character" "$WORK/client2.log" \
  && fail "a second unlisted launch came back as the same character"
pass "a second launch on the development path is a new character"

# ---- 4. the fingerprint the server announces is the one a list would carry ----
#
# Not a client assertion at all — it is the join between the two halves. The number in
# the server's startup line is what an operator registers with the account service, and
# what `client/src/net/tls.rs` then compares a certificate against. A digest of the
# wrong shape here would be a list nothing could verify against, refused whole by
# `net/servers.rs`.
[ "${#FINGERPRINT}" -eq 64 ] && [ -z "${FINGERPRINT//[0-9a-f]/}" ] \
  || fail "the server announced a fingerprint that is not 64 lowercase hex characters"
pass "the server announces a fingerprint of the shape the registry and the client agree on"

echo
echo "interop: 4/4"
