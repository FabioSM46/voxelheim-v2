#!/usr/bin/env bash
# Evaluate the tiered CI matrix after every workload job has reached a terminal
# result. GitHub branch protection and READY TO MERGE require this one stable
# check; branch-specific policy stays here instead of being duplicated in settings.
#
# Voxelheim divergence from the clinic-deck original this is ported from: the
# workspaces (server/, client/, schemas/) may not exist yet — they are scaffolded
# by issues that flow through this very pipeline. The gate therefore also reads
# *_EXISTS flags, computed by the ci-gate job from its own checkout of the same
# merge ref the workload jobs ran on:
#
#   * develop: selectors come from the diff classifier; existence is not
#     consulted. A selected job must succeed, an unselected one must be skipped.
#   * main: a release promotion validates everything that exists. The selector
#     must EQUAL existence — a false selector for an existing workspace is a
#     narrowed release matrix and is rejected, while a false selector for a
#     workspace that does not exist yet is the only legitimate skip.
#
# The automation job (helper-suite host) answers to detect's classifier-
# independent `helpers` selector; on main it is always required.

set -uo pipefail

fail=0

reject() {
  echo "::error::$*" >&2
  fail=1
}

expect_exact() {
  local label="$1" actual="$2" expected="$3"
  if [ "$actual" != "$expected" ]; then
    reject "${label}: expected ${expected}, got ${actual:-<empty>}"
  else
    echo "[PASS] ${label}: ${actual}"
  fi
}

expect_selected() {
  local label="$1" selected="$2" result="$3"
  case "$selected" in
    true) expect_exact "$label" "$result" success ;;
    false) expect_exact "$label" "$result" skipped ;;
    *) reject "${label}: selector must be true or false, got ${selected:-<empty>}" ;;
  esac
}

# main only: the selector may be false ONLY because the workspace is absent.
expect_release_workspace() {
  local label="$1" selected="$2" exists="$3" result="$4"
  case "$exists" in
    true | false) ;;
    *) reject "${label}: existence flag must be true or false, got ${exists:-<empty>}"; return ;;
  esac
  if [ "$selected" != "$exists" ]; then
    reject "${label}: release selector (${selected:-<empty>}) must equal workspace existence (${exists}) — a false selector for an existing workspace narrows production validation"
    return
  fi
  expect_selected "$label" "$selected" "$result"
}

base_ref="${BASE_REF:-}"
detect_result="${DETECT_RESULT:-}"
server_result="${SERVER_RESULT:-}"
client_result="${CLIENT_RESULT:-}"
schemas_result="${SCHEMAS_RESULT:-}"
automation_result="${AUTOMATION_RESULT:-}"

expect_exact "detect" "$detect_result" success

case "$base_ref" in
  develop)
    expect_selected "server" "${SERVER_SELECTED:-}" "$server_result"
    expect_selected "client" "${CLIENT_SELECTED:-}" "$client_result"
    expect_selected "schemas" "${SCHEMAS_SELECTED:-}" "$schemas_result"
    expect_selected "automation" "${HELPERS_SELECTED:-}" "$automation_result"
    ;;
  main)
    # Checking both the selectors and the results catches a future workflow
    # edit that silently narrows production validation while all executed jobs
    # remain green.
    expect_release_workspace "server" "${SERVER_SELECTED:-}" "${SERVER_EXISTS:-}" "$server_result"
    expect_release_workspace "client" "${CLIENT_SELECTED:-}" "${CLIENT_EXISTS:-}" "$client_result"
    expect_release_workspace "schemas" "${SCHEMAS_SELECTED:-}" "${SCHEMAS_EXIST:-}" "$schemas_result"
    expect_exact "helper-scope selector" "${HELPERS_SELECTED:-}" true
    expect_exact "automation" "$automation_result" success
    ;;
  *) reject "BASE_REF must be develop or main, got ${base_ref:-<empty>}" ;;
esac

if [ "$fail" -ne 0 ]; then
  echo "CI gate rejected the ${base_ref:-unknown} matrix" >&2
  exit 1
fi

echo "CI gate accepted the ${base_ref} matrix"
