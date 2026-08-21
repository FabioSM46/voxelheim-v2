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
# **It found a second one by not being run** (#154). The server grew a rule that a hello
# must present a signed ticket, the client grew a development path that presents none,
# and each half's tests stayed green against its own fakes while the two together could
# not open a session at all. This script did not catch it because it had itself stopped
# working: it started `voxelheimd` with no `-world-name`, which that server now refuses.
# So the checks below now cover the *join* rather than only the transport, and the one
# that would have caught #154 on the day it landed is check 2 — the documented
# development command, asserted to reach a world.
#
# **It could not pass at all between #104 and #108, and what fixed it is `--name`.** The
# server answers a hello with `ServerCharacterList` and waits for a character to be chosen,
# and the screen that chooses one waits for a person — which no unattended check can be.
# `--name Eivor` is the client asking for that character by name and having one created
# under it when the account holds none, which is what the server itself did with a hello's
# display name before V7 moved the choice onto the wire. It is a request like any other:
# the server admits or refuses it on its own terms, and check 2 asserts a world on the far
# side of a real one. Every `run_client` below passes it, and that is load-bearing rather
# than decoration — drop it and the client sits on the character screen until `timeout`
# kills it.
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
# check 4, not a limitation of the script.
#
# **Why the ticket is minted here instead of signed in for.** A ticket comes from
# `POST /v1/signin/discord/finish`, which redeems a Discord authorization code — a third
# party, a browser and a registered application, none of which a check can stand up. So
# this signs one with a key it generated and hands the *public* half to the server as
# `-ticket-key`, which is a documented way to run it (see the README). Everything after
# that is real: the ticket is the wire format the contract fixes, the signature is the
# one `internal/ticket` verifies, and the server admits or refuses it on its own terms.
# What is skipped is the account service, and only the account service.
#
# The ticket *body* layout is `internal/ticket`'s and this is the one place outside the
# Go module that knows it. It is not guessing: a change there makes every check below
# fail loudly with the server's own refusal in the log, which is the failure mode a
# duplicated layout should have.
#
# Run it after touching internal/transport, internal/certs, internal/session,
# internal/ticket, or client/src/net/.
#
#   bash scripts/interop-check.sh
#
# Needs: a Go toolchain, a Rust toolchain, openssl, and a display for the client.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
WORK=$(mktemp -d)
PORT=${PORT:-7799}
WORLD=midgard
SERVER_PID=""

# An account service that nothing listens on, and that is deliberate rather than lazy:
# with a live ticket already cached, the development path never asks one for anything —
# it signs in only when it has nothing to present. The URL has to parse; nobody has to
# answer it. A check that could not say that would be a check that had quietly acquired
# a dependency on the sign-in flow.
UNUSED_ACCOUNT_SERVICE="http://127.0.0.1:7798"
TICKET_AUTHORITY="127.0.0.1_7798"

# **A failure keeps its evidence.** Every message below names a log inside $WORK, and
# the trap used to delete the directory on the way out — so the one run anybody needed
# to read pointed at a path that was already gone.
KEEP_WORK=0

cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  if [ "$KEEP_WORK" = 1 ]; then
    echo "logs kept in $WORK" >&2
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

fail() { KEEP_WORK=1; echo "[FAIL] $*" >&2; exit 1; }
pass() { echo "[PASS] $*"; }

# Generates the pair the account service would have, and prints the public half as the
# hex `-ticket-key` takes.
make_ticket_key() {
  openssl genpkey -algorithm ed25519 -out "$WORK/ticket.pem" 2>/dev/null \
    || fail "openssl cannot generate an ed25519 key; this check needs openssl 1.1.1 or newer"
  # A DER SubjectPublicKeyInfo for Ed25519 is a 12-byte prefix and the 32-byte key.
  openssl pkey -in "$WORK/ticket.pem" -pubout -outform DER \
    | tail -c 32 | xxd -p -c 32
}

# Writes a ticket for $1 into the client's cache file $2, in the record shape
# `client/src/net/tickets.rs` reads back: the 96-byte ticket, then the expiry as an
# i64 of Unix seconds, little-endian.
mint_ticket() {
  local world=$1 cache=$2 expires
  expires=$(( $(date +%s) + 3600 ))

  # body = account_id[16] world_id[12] expires_at:u32, little-endian.
  # The account is any non-zero sixteen bytes: the server digests it into a player id,
  # so which one it is decides only which character comes back.
  printf '\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10' > "$WORK/body.bin"
  # world_id = the first 12 bytes of SHA-256(domain ‖ name). The domain separates this
  # digest from every other use of SHA-256 in the repository.
  { printf 'voxelheim/world-id/v1\x00'; printf '%s' "$world"; } \
    | sha256sum | cut -c1-24 | xxd -r -p >> "$WORK/body.bin"
  printf "$(printf '\\x%02x\\x%02x\\x%02x\\x%02x' \
    $((expires & 255)) $((expires >> 8 & 255)) $((expires >> 16 & 255)) $((expires >> 24 & 255)))" \
    >> "$WORK/body.bin"
  [ "$(stat -c%s "$WORK/body.bin")" = 32 ] || fail "the ticket body is not 32 bytes"

  # The signature is over SHA-256(domain ‖ body) rather than over the body: Ed25519
  # hashes its own input, so the digest buys no cryptography — it is where the domain
  # goes, since the body's width is fixed by the wire format.
  { printf 'voxelheim/ticket-body/v1\x00'; cat "$WORK/body.bin"; } \
    | sha256sum | cut -d' ' -f1 | xxd -r -p > "$WORK/digest.bin"
  openssl pkeyutl -sign -rawin -inkey "$WORK/ticket.pem" \
    -in "$WORK/digest.bin" -out "$WORK/sig.bin"
  [ "$(stat -c%s "$WORK/sig.bin")" = 64 ] || fail "the signature is not 64 bytes"

  mkdir -p "$(dirname "$cache")"
  cat "$WORK/body.bin" "$WORK/sig.bin" > "$cache"
  # The expiry the client caches beside the ticket. Eight bytes, little-endian, and
  # positive for another hour — a i64 whose top four bytes are zero for the next
  # eighty years.
  printf "$(printf '\\x%02x\\x%02x\\x%02x\\x%02x\\x00\\x00\\x00\\x00' \
    $((expires & 255)) $((expires >> 8 & 255)) $((expires >> 16 & 255)) $((expires >> 24 & 255)))" \
    >> "$cache"
  chmod 600 "$cache"
  [ "$(stat -c%s "$cache")" = 104 ] || fail "the cached record is not 104 bytes"
}

start_server() {
  "$WORK/voxelheimd" -listen "127.0.0.1:$PORT" -world-dir "$WORK/world" \
    -world-name "$WORLD" -ticket-key "$TICKET_KEY" >"$1" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 40); do
    grep -q "voxelheimd listening" "$1" 2>/dev/null && return 0
    sleep 0.25
  done
  fail "the server did not start; see $1"
}

# Runs the client for a few seconds and returns whatever it said. Never fails the
# script itself: every check below reads the log, and a client killed by `timeout`
# exits non-zero by construction.
#
# `--name Eivor` is what answers the character phase without a person at the keyboard;
# see the header. It is passed on every run, including the two that are refused before
# the phase is reached, because a flag that appeared only where it was needed would be
# read as part of what those checks are asserting.
run_client() {
  local log=$1; shift
  # `world settled` is a debug line and it is the signal that chunks arrived and meshed,
  # which is the whole of "reached a world". Without this the check below can only see
  # that a session opened.
  XDG_DATA_HOME="$WORK/clientdata" RUST_LOG=info,voxelheim_client=debug timeout 15 \
    "$REPO_ROOT/client/target/debug/voxelheim-client" \
    --server "127.0.0.1:$PORT" --name Eivor "$@" >"$log" 2>&1 || true
}

echo "building both sides..."
(cd "$REPO_ROOT/server" && go build -o "$WORK/voxelheimd" ./cmd/voxelheimd)
(cd "$REPO_ROOT/client" && cargo build --workspace --locked --quiet)

mkdir -p "$WORK/world" "$WORK/clientdata"
TICKET_KEY=$(make_ticket_key)
[ "${#TICKET_KEY}" -eq 64 ] || fail "the generated ticket key is not 64 hex characters"

# ---- 1. a hello with no account is refused, by the server, in words ----
#
# **The check #154 did not have.** The client's own tests said an absent ticket was a
# legal hello and the server's own tests said a ticket was required, and both were
# right; what nobody ran was the pair. Asserted before the working path so that a
# regression here reads as what it is — the server's admission rule — rather than as a
# ticket problem.
start_server "$WORK/server1.log"
FINGERPRINT=$(grep -o 'certificate_sha256=[a-f0-9]*' "$WORK/server1.log" | head -1 | cut -d= -f2)
[ -n "$FINGERPRINT" ] || fail "the server logged no certificate fingerprint"

run_client "$WORK/client-no-account.log"
grep -q "session established" "$WORK/client-no-account.log" \
  && fail "a hello presenting no ticket was admitted; see $WORK/client-no-account.log"
grep -q "the session ticket was not accepted" "$WORK/client-no-account.log" \
  || fail "the refusal did not reach the client in the server's own words; see $WORK/client-no-account.log"
grep -q "the hello presents no session ticket" "$WORK/server1.log" \
  || fail "the server refused for some reason other than the absent ticket"
pass "a hello presenting no account is refused, and the refusal reaches the player"

# ---- 2. the documented development launch reaches a world ----
#
# The check this script exists for after #154, and the one that also carries what check
# 1 used to: a session that establishes and then keeps streaming is what fills one
# socket read with several TLS records, which is the record-layer bug this script found
# the first time it ran.
mint_ticket "$WORLD" "$WORK/clientdata/voxelheim/world-ticket/$TICKET_AUTHORITY/$WORLD"
run_client "$WORK/client-join.log" --account-service "$UNUSED_ACCOUNT_SERVICE" --world "$WORLD"
grep -q "session established" "$WORK/client-join.log" \
  || fail "the documented development launch never established a session; see $WORK/client-join.log"
grep -qi "decrypt\|protocol error" "$WORK/client-join.log" \
  && fail "the session established and then broke; see $WORK/client-join.log"
grep -q "world settled" "$WORK/client-join.log" \
  || fail "the session established but no world arrived; see $WORK/client-join.log"
grep -q "session admitted" "$WORK/server1.log" \
  || fail "the server did not record admitting anybody"
pass "a signed-in development client reaches a world over TLS and the record stream survives it"

# ---- 3. a ticket for another world is still refused ----
#
# **The property that makes check 2 an acceptable trade rather than a hole.** Presenting
# a ticket to an address in no list is bounded because the ticket is bounded: it names
# one world, and a server running another refuses it. Widen check 2 without this one and
# the trade stops being the one that was argued for.
mint_ticket "asgard" "$WORK/clientdata/voxelheim/world-ticket/$TICKET_AUTHORITY/asgard"
run_client "$WORK/client-wrong-world.log" --account-service "$UNUSED_ACCOUNT_SERVICE" --world asgard
grep -q "session established" "$WORK/client-wrong-world.log" \
  && fail "a ticket for another world was admitted; see $WORK/client-wrong-world.log"
grep -q "the ticket names another world" "$WORK/server1.log" \
  || fail "the server did not refuse the ticket for naming another world"
pass "a ticket for one world is refused by a server running another"

# ---- 4. nothing was written down, and no identity was presented ----
#
# **The property that replaced the pin file, and it survives the ticket.** `--server`
# names an address that is in no list, so nothing states which certificate to expect
# there — and the client therefore presents no *identity* and keeps none, whatever
# account it signed in as. Checked on disk, because the identity file is where a token
# would have to be kept for a later launch to present it, and checked for a pin file
# too: that path is removed, not merely unused, so anything writing one back is a
# regression this script should catch.
IDENTITY_DIR="$WORK/clientdata/voxelheim/identity"
if [ -d "$IDENTITY_DIR" ]; then
  STRAY=$(find "$IDENTITY_DIR" -type f | head -5)
  [ -z "$STRAY" ] \
    || fail "an unlisted session wrote credentials to disk: $STRAY"
fi
grep -q "a returning character" "$WORK/client-join.log" \
  && fail "an unlisted session claimed a stored identity came back"
pass "an address in no list is shown no identity and leaves nothing behind"

# ---- 5. the account decides the character, and the client says so ----
#
# The other half of check 4, and the correction #154 had to make to it. With no identity
# file this path used to report every session as a new character; the ticket names an
# account and the server restores that account's character, so the second launch is the
# same player as the first — and the client, which is told neither, must claim neither.
run_client "$WORK/client-again.log" --account-service "$UNUSED_ACCOUNT_SERVICE" --world "$WORLD"
grep -q "session established" "$WORK/client-again.log" \
  || fail "the second launch never established a session; see $WORK/client-again.log"
grep -q "a new character" "$WORK/client-again.log" \
  && fail "a client with no identity file claimed to know this was a new character"
[ "$(grep -c 'returning=true' "$WORK/server1.log")" -ge 1 ] \
  || fail "the server did not restore the account's character on the second launch"
pass "a second launch is the same account's character, and the client claims nothing about it"

# ---- 6. the character phase happened, and the second launch found the first's ----
#
# **The one check that reads the phase itself rather than the world on the far side of
# it.** Check 2 proves a session established; this proves how. The account starts with no
# characters here, so the first launch is offered an empty list and asks for a creation;
# the second is offered the character that creation made and asks to play it. Both halves
# are read from the client's own log, and the counts are the server's own answer — an
# `--name` that quietly created a second character every launch would pass every other
# check on this page and fail this one.
grep -q "the server is waiting for a character: 0 of at most" "$WORK/client-join.log" \
  || fail "the first launch was not offered an empty character list; see $WORK/client-join.log"
grep -q "asking to create a character" "$WORK/client-join.log" \
  || fail "the first launch did not ask to create one; see $WORK/client-join.log"
grep -q "the server is waiting for a character: 1 of at most" "$WORK/client-again.log" \
  || fail "the second launch was not offered the character the first made; see $WORK/client-again.log"
grep -q "asking to play a character this account already has" "$WORK/client-again.log" \
  || fail "the second launch did not ask to play it; see $WORK/client-again.log"
grep -q "asking to create a character" "$WORK/client-again.log" \
  && fail "the second launch created another character instead of playing the first"
pass "a character is created on the first launch and played on the second"

# ---- 7. the fingerprint the server announces is the one a list would carry ----
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
echo "interop: 7/7"
