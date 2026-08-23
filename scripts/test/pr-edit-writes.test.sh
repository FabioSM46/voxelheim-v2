#!/usr/bin/env bash
# Regression tests for pr-edit: title/body writes use REST, stay loud on API
# failure, and are not reported as successful until a read-back matches.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../gh-automation.sh
source "${SCRIPT_DIR}/gh-automation.sh"
# The sourced script sets -e; these tests deliberately drive failing paths.
set +e

pass=0
fail=0

assert_eq() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected '${expected}', got '${actual}'"
    fail=$((fail + 1))
  fi
}

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected to find '${needle}' in:"
    printf '           %s\n' "${haystack:-<empty>}"
    fail=$((fail + 1))
  fi
}

assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: did NOT expect '${needle}' in:"
    printf '           %s\n' "$haystack"
    fail=$((fail + 1))
  fi
}

assert_nonzero() {
  local name="$1" actual="$2"
  if [ "$actual" -ne 0 ]; then
    echo "  ok   — ${name} (exit ${actual})"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected a non-zero exit, got 0"
    fail=$((fail + 1))
  fi
}

CALL_LOG="$(mktemp)"
BODY_FILE="$(mktemp)"
trap 'rm -f "$CALL_LOG" "$BODY_FILE"' EXIT

FIXTURE_REPO="voxelheim-test/repo"
REPO="$FIXTURE_REPO"
GITHUB_REPOSITORY="$FIXTURE_REPO"
export GITHUB_REPOSITORY

GH_AUTH_STATUS=0
GH_WRITE_STATUS=0
GH_READ_STATUS=0
GH_READ_TITLE="Existing title"
GH_READ_BODY="Existing body"

gh() {
  printf 'gh' >>"$CALL_LOG"
  printf ' <%s>' "$@" >>"$CALL_LOG"
  printf '\n' >>"$CALL_LOG"

  if [ "${1:-}" = "auth" ] && [ "${2:-}" = "status" ]; then
    return "$GH_AUTH_STATUS"
  fi

  if [ "${1:-}" = "api" ] && [ "${2:-}" = "-X" ] && [ "${3:-}" = "PATCH" ]; then
    if [ "$GH_WRITE_STATUS" -ne 0 ]; then
      echo '{"message":"Resource not accessible by personal access token","status":"403"}'
      echo "gh: Resource not accessible by personal access token (HTTP 403)" >&2
      return "$GH_WRITE_STATUS"
    fi
    echo '{"number":279}'
    return 0
  fi

  if [ "${1:-}" = "api" ] && [ "${2:-}" = "repos/${FIXTURE_REPO}/pulls/279" ]; then
    if [ "$GH_READ_STATUS" -ne 0 ]; then
      echo '{"message":"Service unavailable","status":"503"}'
      echo "gh: Service unavailable (HTTP 503)" >&2
      return "$GH_READ_STATUS"
    fi
    jq -nc --arg title "$GH_READ_TITLE" --arg body "$GH_READ_BODY" \
      '{title:$title,body:$body}'
    return 0
  fi

  if [ "${1:-}" = "pr" ] && [ "${2:-}" = "edit" ]; then
    echo "gh pr edit must never be issued by this script — see issue 206" >&2
    return 65
  fi

  echo "unexpected gh invocation: $*" >&2
  return 64
}

reset_stub() {
  : >"$CALL_LOG"
  REPO="$FIXTURE_REPO"
  GITHUB_REPOSITORY="$FIXTURE_REPO"
  GH_AUTH_STATUS=0
  GH_WRITE_STATUS=0
  GH_READ_STATUS=0
  GH_READ_TITLE="Existing title"
  GH_READ_BODY="Existing body"
}

run_edit() {
  : >"$CALL_LOG"
  local out_file err_file
  out_file="$(mktemp)"
  err_file="$(mktemp)"
  cmd_pr_edit "$@" >"$out_file" 2>"$err_file"
  STATUS=$?
  OUT="$(cat "$out_file")"
  ERR="$(cat "$err_file")"
  CALLS="$(cat "$CALL_LOG")"
  rm -f "$out_file" "$err_file"
}

echo "pr-edit — a title write lands and is verified"

reset_stub
GH_READ_TITLE="Replacement title"
run_edit 279 --title "Replacement title"
assert_eq "a verified title edit exits 0" 0 "$STATUS"
assert_contains "a verified title edit says it was checked" "$OUT" \
  "PR #279 metadata updated and verified"
assert_contains "the write uses the pull-request REST endpoint" "$CALLS" \
  "gh <api> <-X> <PATCH> <repos/voxelheim-test/repo/pulls/279> <-f> <title=Replacement title>"
assert_contains "the helper reads the PR back" "$CALLS" \
  "gh <api> <repos/voxelheim-test/repo/pulls/279>"
assert_not_contains "the helper never reaches the broken porcelain command" "$CALLS" "<pr> <edit>"

echo
echo "pr-edit — a body file keeps its exact trailing newline"

reset_stub
printf 'First line\nSecond line\n' >"$BODY_FILE"
GH_READ_BODY=$'First line\nSecond line\n'
run_edit 279 --body-file "$BODY_FILE"
assert_eq "an exact body read-back exits 0" 0 "$STATUS"
assert_contains "a verified body edit says so" "$OUT" "metadata updated and verified"

echo
echo "pr-edit — API failure stays loud"

reset_stub
GH_WRITE_STATUS=1
run_edit 279 --title "Replacement title"
assert_nonzero "a failed PATCH exits non-zero" "$STATUS"
assert_not_contains "a failed PATCH prints no success" "$OUT" "updated and verified"
assert_contains "a failed PATCH keeps gh's reason" "$ERR" \
  "Resource not accessible by personal access token"
assert_contains "a failed PATCH re-emits the API body" "$ERR" '"status":"403"'
assert_not_contains "a failed PATCH never attempts read-back" "$CALLS" \
  "gh <api> <repos/voxelheim-test/repo/pulls/279>"

echo
echo "pr-edit — zero from PATCH is not the postcondition"

reset_stub
GH_READ_TITLE="Existing title"
run_edit 279 --title "Replacement title"
assert_nonzero "an unchanged title after a successful PATCH is failure" "$STATUS"
assert_not_contains "an unchanged title prints no success" "$OUT" "updated and verified"
assert_contains "an unchanged title names the failed postcondition" "$ERR" \
  "title did not match after the write"

reset_stub
GH_READ_STATUS=1
run_edit 279 --title "Replacement title"
assert_nonzero "an unreadable postcondition fails closed" "$STATUS"
assert_contains "an unreadable postcondition says verification failed" "$ERR" \
  "the read-back failed"
assert_contains "an unreadable postcondition re-emits the API body" "$ERR" '"status":"503"'

echo
echo "pr-edit — argument handling and CLI discovery"

out=$( (cmd_pr_edit 279) 2>&1 )
assert_nonzero "an edit with no field exits non-zero" $?
assert_contains "an edit with no field names the accepted flags" "$out" "--title, --body, or --body-file"

out=$( (cmd_pr_edit 279 --body one --body-file "$BODY_FILE") 2>&1 )
assert_nonzero "two body sources are refused" $?
assert_contains "two body sources explain the choice" "$out" "Use only one of --body and --body-file"

function_body="$(declare -f cmd_pr_edit)"
assert_not_contains "the implementation contains no gh pr edit call" "$function_body" "gh pr edit"

help_out="$(bash "${SCRIPT_DIR}/gh-automation.sh" --help)"
assert_contains "the CLI advertises the replacement command" "$help_out" "pr-edit <pr>"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
