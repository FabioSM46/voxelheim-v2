#!/usr/bin/env bash
# Evaluate the post-merge verification matrix for `develop`.
#
# The sibling of scripts/ci-gate.sh, and deliberately much simpler than it. ci-gate
# audits a matrix whose jobs may legitimately be skipped, so it has to know which
# selector authorised each skip. This workflow has no selectors at all — the point of a
# post-merge run is the combination, and a combination is not describable as a diff — so
# there is exactly one acceptable result per job.
#
# **Every other result is rejected, `skipped` included.** A skip here is not an
# authorised narrowing; it is a job that stopped running while the workflow kept
# reporting success, which is the one way a green integration run could mean less than
# it says. An empty result is rejected for the same reason: a job removed from `needs`
# reads as empty, and "cannot tell" is never "fine".
#
# An absent workspace is NOT a skip. Each job runs unconditionally and no-ops with a
# notice when its marker file is missing (server/go.mod, client/Cargo.toml,
# schemas/*.fbs), exactly as ci.yml's `present` guard does — so a pre-scaffold ref
# reaches `success` here without anything having to authorise it.

set -uo pipefail

fail=0

expect_success() {
  local label="$1" result="$2"
  case "$result" in
    success)
      echo "[PASS] ${label}: success"
      ;;
    "")
      echo "::error::${label}: no result — the job is missing from the verdict's \`needs\` list" >&2
      fail=1
      ;;
    *)
      echo "::error::${label}: expected success, got ${result}" >&2
      fail=1
      ;;
  esac
}

expect_success "privacy" "${PRIVACY_RESULT:-}"
expect_success "server" "${SERVER_RESULT:-}"
expect_success "client" "${CLIENT_RESULT:-}"
expect_success "schemas" "${SCHEMAS_RESULT:-}"
expect_success "automation" "${AUTOMATION_RESULT:-}"

if [ "$fail" -ne 0 ]; then
  echo "Integration gate rejected develop at ${INTEGRATION_SHA:-<unknown commit>}" >&2
  exit 1
fi

echo "Integration gate accepted develop at ${INTEGRATION_SHA:-<unknown commit>}"
