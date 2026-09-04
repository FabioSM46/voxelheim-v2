#!/usr/bin/env bash
# Deterministic result-matrix tests for scripts/ci-gate.sh.
#
# The voxelheim-specific surface pinned hardest here: workspaces are scaffolded
# through the pipeline itself, so on `main` a selector may be false ONLY because
# the workspace does not exist at the ref — a false selector for an existing
# workspace is a silently narrowed release matrix and must be rejected.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GATE="${ROOT}/scripts/ci-gate.sh"
pass=0
fail=0

run_case() {
  local name="$1" expected="$2"
  shift 2
  local output actual
  output=$(env -i PATH="$PATH" "$@" bash "$GATE" 2>&1)
  actual=$?
  if [ "$actual" -eq "$expected" ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected exit ${expected}, got ${actual}"
    echo "$output" | sed 's/^/         /'
    fail=$((fail + 1))
  fi
}

non_main_common=(
  BASE_REF=develop DETECT_RESULT=success
  SERVER_SELECTED=true SERVER_RESULT=success
  CLIENT_SELECTED=false CLIENT_RESULT=skipped
  SCHEMAS_SELECTED=false SCHEMAS_RESULT=skipped
  HELPERS_SELECTED=false AUTOMATION_RESULT=skipped
)

main_common=(
  BASE_REF=main DETECT_RESULT=success
  SERVER_SELECTED=true SERVER_EXISTS=true SERVER_RESULT=success
  CLIENT_SELECTED=true CLIENT_EXISTS=true CLIENT_RESULT=success
  SCHEMAS_SELECTED=true SCHEMAS_EXIST=true SCHEMAS_RESULT=success
  HELPERS_SELECTED=true AUTOMATION_RESULT=success
)

echo "ci-gate — non-main policy"
run_case "develop accepts selected work while unrelated jobs skip" 0 "${non_main_common[@]}"
run_case "a feature base uses the same selected-work policy" 0 \
  "${non_main_common[@]/BASE_REF=develop/BASE_REF=feature/parent-branch}"
run_case "docs-only PR accepts an entirely skipped workload" 0 \
  BASE_REF=develop DETECT_RESULT=success \
  SERVER_SELECTED=false SERVER_RESULT=skipped \
  CLIENT_SELECTED=false CLIENT_RESULT=skipped \
  SCHEMAS_SELECTED=false SCHEMAS_RESULT=skipped \
  HELPERS_SELECTED=false AUTOMATION_RESULT=skipped
run_case "selected job may not skip" 1 "${non_main_common[@]/SERVER_RESULT=success/SERVER_RESULT=skipped}"
run_case "unselected job may not disappear" 1 "${non_main_common[@]/CLIENT_RESULT=skipped/CLIENT_RESULT=missing}"
run_case "cancelled selected job fails closed" 1 "${non_main_common[@]/SERVER_RESULT=success/SERVER_RESULT=cancelled}"
run_case "failed detect cannot be hidden" 1 "${non_main_common[@]/DETECT_RESULT=success/DETECT_RESULT=failure}"
run_case "missing selector fails closed" 1 "${non_main_common[@]/SCHEMAS_SELECTED=false/SCHEMAS_SELECTED=}"
run_case "malformed selector is not read as false" 1 "${non_main_common[@]/SCHEMAS_SELECTED=false/SCHEMAS_SELECTED=maybe}"
run_case "an empty base still fails closed" 1 "${non_main_common[@]/BASE_REF=develop/BASE_REF=}"

echo
echo "ci-gate — the helper-scope selector the classifier cannot compute for itself"
# The whole point of the workflow-computed `helpers` flag: a diff touching
# scripts/** while changed-areas.sh — the very thing that diff may have broken —
# says nothing about it. The automation job hosts the classifier's own tests, so
# a SKIPPED automation here would be precisely the silent self-exemption.
helpers_only=("${non_main_common[@]/HELPERS_SELECTED=false/HELPERS_SELECTED=true}")
run_case "helper-scope diffs cannot skip the job hosting the helper tests" 1 \
  "${helpers_only[@]}"
run_case "helper scope alone is enough for automation to have run" 0 \
  "${helpers_only[@]/AUTOMATION_RESULT=skipped/AUTOMATION_RESULT=success}"
run_case "missing helper-scope selector fails closed" 1 \
  "${non_main_common[@]/HELPERS_SELECTED=false/HELPERS_SELECTED=}"
run_case "a malformed helper-scope selector is not read as false" 1 \
  "${non_main_common[@]/HELPERS_SELECTED=false/HELPERS_SELECTED=maybe}"

echo
echo "ci-gate — main release policy validates everything that exists"
run_case "complete release matrix succeeds" 0 "${main_common[@]}"
# The pre-scaffold promotion: a workspace that does not exist yet is the ONE
# legitimate skip on main. This is what keeps the pipeline usable from day zero.
pre_scaffold=("${main_common[@]/CLIENT_SELECTED=true/CLIENT_SELECTED=false}")
pre_scaffold=("${pre_scaffold[@]/CLIENT_EXISTS=true/CLIENT_EXISTS=false}")
pre_scaffold=("${pre_scaffold[@]/CLIENT_RESULT=success/CLIENT_RESULT=skipped}")
run_case "an absent workspace may skip on main" 0 "${pre_scaffold[@]}"
run_case "release job for an existing workspace may not skip" 1 \
  "${main_common[@]/SERVER_RESULT=success/SERVER_RESULT=skipped}"
# The narrowing guard itself: selector false while the workspace exists.
narrowed=("${main_common[@]/CLIENT_SELECTED=true/CLIENT_SELECTED=false}")
narrowed=("${narrowed[@]/CLIENT_RESULT=success/CLIENT_RESULT=skipped}")
run_case "release selector may not narrow the matrix for an existing workspace" 1 \
  "${narrowed[@]}"
# The reverse mismatch: a selector claiming work for a workspace the gate's own
# checkout says is absent means detect and ci-gate saw different trees.
phantom=("${main_common[@]/CLIENT_EXISTS=true/CLIENT_EXISTS=false}")
run_case "release selector may not claim an absent workspace" 1 "${phantom[@]}"
run_case "release helper scope may not narrow the matrix" 1 \
  "${main_common[@]/HELPERS_SELECTED=true/HELPERS_SELECTED=false}"
run_case "release automation may not skip" 1 \
  "${main_common[@]/AUTOMATION_RESULT=success/AUTOMATION_RESULT=skipped}"
run_case "pending release job fails closed" 1 \
  "${main_common[@]/CLIENT_RESULT=success/CLIENT_RESULT=pending}"
run_case "malformed existence flag fails closed" 1 \
  "${main_common[@]/SERVER_EXISTS=true/SERVER_EXISTS=maybe}"
run_case "missing existence flag fails closed" 1 \
  "${main_common[@]/SERVER_EXISTS=true/SERVER_EXISTS=}"

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
