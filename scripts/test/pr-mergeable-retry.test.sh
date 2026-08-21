#!/usr/bin/env bash
# Tests for resolve_mergeable — waiting out GitHub's asynchronous mergeability computation.
#
# What this pins: `mergeable` is computed in a background job, so it reads UNKNOWN for a short while
# after a push. Any caller reading it immediately — `/process-pr`, or a human running `pr-status`
# seconds after pushing — lands inside that window and would otherwise get a misleading FAIL.
#
# The fix must retry UNKNOWN and *only* UNKNOWN. CONFLICTING is the "a conflicting PR runs zero
# checks, so nothing-red must never read as green" guard and an unreadable value is a permission or
# network failure; waiting on either would only delay the same answer.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

pass=0
fail=0

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "  ✓ ${label}"
    pass=$((pass + 1))
  else
    echo "  ✗ ${label}"
    echo "      expected: ${expected}"
    echo "      actual:   ${actual}"
    fail=$((fail + 1))
  fi
}

# shellcheck source=../gh-automation.sh
source "${SCRIPT_DIR}/gh-automation.sh"
# The sourced script sets -e; tests deliberately drive failing paths.
set +e

if ! declare -f resolve_mergeable >/dev/null; then
  echo "FATAL: resolve_mergeable not defined after sourcing the helper" >&2
  exit 1
fi

# resolve_mergeable is invoked through command substitution, so it runs in a subshell and any
# counter it increments dies with it. The call log therefore lives in a file — one line per call.
COUNT_FILE="$(mktemp)"
trap 'rm -f "$COUNT_FILE"' EXIT

# `gh` stub: replays MERGEABLE_SEQUENCE, one value per call, repeating the last one forever.
gh() {
  case "$*" in
    *"pr view"*)
      echo "call" >> "$COUNT_FILE"
      local n
      n=$(wc -l < "$COUNT_FILE" | tr -d ' ')
      local -a seq=($MERGEABLE_SEQUENCE)
      local idx=$((n - 1))
      [ "$idx" -ge "${#seq[@]}" ] && idx=$((${#seq[@]} - 1))
      local value="${seq[$idx]}"
      [ "$value" = "__EMPTY__" ] && return 1
      printf '%s\n' "$value"
      return 0
      ;;
  esac
  echo "unexpected gh invocation: $*" >&2
  return 64
}

# No real waiting: every case here would otherwise pay the production delay.
sleep() { :; }

run_case() {
  MERGEABLE_SEQUENCE="$1"
  : > "$COUNT_FILE"
  RESULT=$(resolve_mergeable 401 2>/dev/null)
  CALL_COUNT=$(wc -l < "$COUNT_FILE" | tr -d ' ')
}

echo "resolve_mergeable — UNKNOWN is a 'not yet', not a verdict"

export PR_MERGEABLE_RETRIES=4
export PR_MERGEABLE_RETRY_DELAY=0

run_case "UNKNOWN UNKNOWN MERGEABLE"
assert_eq "waits out UNKNOWN and returns the real answer" "MERGEABLE" "$RESULT"
assert_eq "  stops as soon as the answer arrives" "3" "$CALL_COUNT"

run_case "MERGEABLE"
assert_eq "returns immediately when already known" "MERGEABLE" "$RESULT"
assert_eq "  costs exactly one call in the normal case" "1" "$CALL_COUNT"

echo
echo "the fail-closed values are answers, and must not be retried"

run_case "CONFLICTING"
assert_eq "CONFLICTING is returned as-is" "CONFLICTING" "$RESULT"
assert_eq "  and is never retried (the no-checks guard stays immediate)" "1" "$CALL_COUNT"

run_case "__EMPTY__"
assert_eq "an unreadable value fails closed" "UNREADABLE" "$RESULT"
assert_eq "  and is never retried" "1" "$CALL_COUNT"

echo
echo "the budget is bounded and configurable"

run_case "UNKNOWN"
assert_eq "gives up after the budget rather than looping forever" "UNKNOWN" "$RESULT"
assert_eq "  making exactly retries+1 calls" "5" "$CALL_COUNT"

PR_MERGEABLE_RETRIES=0
run_case "UNKNOWN MERGEABLE"
assert_eq "retries=0 disables waiting entirely" "UNKNOWN" "$RESULT"
assert_eq "  taking the single sample and stopping" "1" "$CALL_COUNT"

PR_MERGEABLE_RETRIES=12
run_case "UNKNOWN UNKNOWN UNKNOWN UNKNOWN UNKNOWN UNKNOWN MERGEABLE"
assert_eq "a larger budget, as a CI caller could set, waits longer" "MERGEABLE" "$RESULT"
assert_eq "  and still stops on the first real answer" "7" "$CALL_COUNT"

echo
echo "malformed budgets fall back to defaults instead of breaking the read"

PR_MERGEABLE_RETRIES=abc
PR_MERGEABLE_RETRY_DELAY=0
run_case "UNKNOWN"
assert_eq "a non-numeric retry count falls back to the default of 4" "UNKNOWN" "$RESULT"
assert_eq "  so it still makes exactly retries+1 calls" "5" "$CALL_COUNT"

# A non-numeric delay reaches `sleep`, which fails. Whether that aborts depends on a set -e
# exemption for loop bodies; validating the input removes the question entirely.
PR_MERGEABLE_RETRIES=1
PR_MERGEABLE_RETRY_DELAY=abc
run_case "UNKNOWN MERGEABLE"
assert_eq "a non-numeric delay does not break the read" "MERGEABLE" "$RESULT"
assert_eq "  and the retry still happens" "2" "$CALL_COUNT"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
