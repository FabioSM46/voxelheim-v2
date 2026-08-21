#!/usr/bin/env bash
# =============================================================================
# changed-areas.sh — classify a changed-file list into the CI jobs it can affect
#
# Reads newline-separated repository paths on stdin (for renames, feed BOTH the
# new and the previous path — a file moved out of a workspace must still rebuild
# the workspace it left: its imports there just broke). Prints exactly three
# `key=value` lines on stdout, in the shape `$GITHUB_OUTPUT` consumes:
#
#   server=…    run the `server` job (Go backend gates)
#   client=…    run the `client` job (Rust client gates)
#   schemas=…   run the `schemas` job (FlatBuffers contract validation)
#
# A schema change sets ALL THREE flags: the .fbs files are the network contract,
# and a contract change must rebuild and re-test both sides that consume its
# generated code. That is the "granite contracts" promise of FlatBuffers made
# executable.
#
# The `helpers` selector (which gates the `automation` job) is deliberately NOT
# computed here. The automation job runs this script's own tests, so gating it
# on this script's output would let a classifier bug exempt itself from the
# tests that would have caught it. ci.yml derives `helpers` from the raw
# changed-path list with a plain grep — see the detect job.
#
# ci.yml consumes these through `if: … != 'false'` — a job skips only on a
# positive, well-formed "false". That polarity is half of the safety story; this
# script is the other half, and it FAILS OPEN:
#
#   * a path no rule recognises sets every flag true, so a new top-level
#     directory is tested until someone classifies it here, and
#   * empty input sets every flag true, because "nothing changed" is never why
#     CI was asked to run — an empty list means the caller could not produce
#     one, and an unreadable diff must run everything, not nothing.
#
# The dangerous direction is a silent false: flags decide which work `ci-gate`
# permits to skip. scripts/test/changed-areas.test.sh pins every rule below,
# including the fail-open ones, and runs in the automation job.
# =============================================================================

set -euo pipefail

ws_server=false   # server/**  — the Go backend workspace
ws_client=false   # client/**  — the Rust client workspace
ws_schemas=false  # schemas/** and .flatc-version — the network contract
global=false      # affects every workspace, or unrecognised (fail open)

saw_any=false

while IFS= read -r path; do
  [ -n "$path" ] || continue
  saw_any=true
  # First match wins, top to bottom. Workspace prefixes come BEFORE the inert
  # arm on purpose: markdown INSIDE a workspace is that workspace's problem
  # (a future doc-embedding build may read it); only root/docs markdown is
  # known inert to every job.
  case "$path" in
    server/*) ws_server=true ;;
    client/*) ws_client=true ;;
    # The network contract. Generated code on both sides derives from these, so
    # the schemas flag fans out to server and client in the flag section below.
    # `.flatc-version` pins the compiler that produces that generated code —
    # changing it can change the output, so it is a contract change too.
    schemas/* | .flatc-version) ws_schemas=true ;;
    # Changes every job's meaning: the workflow itself.
    .github/workflows/ci.yml) global=true ;;
    # Automation helpers. Inert HERE by design: the `automation` job that tests
    # them is selected by detect's classifier-independent `helpers` grep, never
    # by this script (see the header). Routing them to a workspace would run
    # builds they cannot affect.
    scripts/* | .github/scripts/*) : ;;
    # Inert for CI: nothing any job builds, lints or tests reads these. The
    # `.github/*` arm is reached only for files the two .github arms above did
    # not claim — other workflows run themselves; changing them cannot change
    # what THIS run must verify. `*.md` here is root- and docs-level markdown
    # only (AGENTS.md, README.md, docs/**) — see the workspace note above.
    *.md | docs/* | .github/* | .claude/* | .agents/* | .opencode/* | .githooks/* | \
      .gitignore | .gitattributes | LICENSE*) : ;;
    # Fail open. Anything unrecognised runs everything; the fix for the cost is
    # to add a rule here WITH a test, never to guess from the workflow.
    *) global=true ;;
  esac
done

if [ "$saw_any" = false ]; then
  echo "changed-areas: empty input — failing open, all jobs run" >&2
  ws_server=true ws_client=true ws_schemas=true global=true
fi

flag() { # flag <name> <cond>... — true if ANY listed bucket is true
  local name="$1" value=false bucket
  shift
  for bucket in "$@"; do
    [ "$bucket" = true ] && value=true
  done
  printf '%s=%s\n' "$name" "$value"
}

echo "changed-areas: ws_server=${ws_server} ws_client=${ws_client} ws_schemas=${ws_schemas} global=${global}" >&2

# A schema change rebuilds both consumers of its generated code.
flag server "$ws_server" "$ws_schemas" "$global"
flag client "$ws_client" "$ws_schemas" "$global"
flag schemas "$ws_schemas" "$global"
