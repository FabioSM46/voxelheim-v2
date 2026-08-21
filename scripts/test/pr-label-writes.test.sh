#!/usr/bin/env bash
# =============================================================================
# Regression tests for pr-label — the write half of the labeler.
#
# The defect (#134): `cmd_pr_label add` ran `gh pr edit … 2>/dev/null || true` and
# then printed its success line unconditionally. The reason went to /dev/null, the
# exit status went to `true`, and the line went out regardless — so the one thing
# the helper could never report was a label it had not applied. Found live during a
# ceremony: `pr-label 131 add ready-for-dev` printed success, exited 0, and the
# label was not on the PR.
#
# `remove` carried the same shape plus a second one. Its guard, `cmd_pr_check_label`,
# was `gh pr view … 2>/dev/null | grep -qxF`: a failed lookup prints nothing, grep
# exits non-zero, and "could not read the labels" became "the label is absent" —
# skipping the removal with no line printed at all.
#
# Why nothing caught it: `pr-labeler-step.test.sh` replaces this whole script with a
# stub whose `pr-label` case is `echo "[LABEL] $*"; exit 0`. That test is correct and
# stays — it pins which labels the workflow *asks for* in each state. But it never
# executes `cmd_pr_label`, and its unconditional `exit 0` encodes the very assumption
# that turned out to be false. This file tests the helper underneath it, and drives
# the direction the suite had never exercised: `gh` failing.
#
# Run: bash scripts/test/pr-label-writes.test.sh
# =============================================================================

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

# ── The `gh` stub ────────────────────────────────────────────────────────────
#
# Three knobs, because three things can independently fail: authentication, the
# label read, and the label write. Every invocation is logged, so a test can assert
# that a write was NOT attempted — which is the whole claim of the "could not
# determine" branch.
CALL_LOG="$(mktemp)"
trap 'rm -f "$CALL_LOG"' EXIT

GH_AUTH_STATUS=0
GH_VIEW_STATUS=0
GH_VIEW_LABELS=""
GH_EDIT_STATUS=0

gh() {
  printf 'gh %s\n' "$*" >>"$CALL_LOG"
  case "$*" in
    "auth status"*)
      return "$GH_AUTH_STATUS"
      ;;
    "pr view "*)
      if [ "$GH_VIEW_STATUS" -ne 0 ]; then
        echo "gh: HTTP 502: Server Error (https://api.github.com/graphql)" >&2
        return "$GH_VIEW_STATUS"
      fi
      [ -n "$GH_VIEW_LABELS" ] && printf '%s\n' "$GH_VIEW_LABELS"
      return 0
      ;;
    "pr edit "*)
      if [ "$GH_EDIT_STATUS" -ne 0 ]; then
        echo "gh: 'ready-for-dev' not found in the repository" >&2
        return "$GH_EDIT_STATUS"
      fi
      echo "https://github.com/FabioSM46/voxelheim-v2/pull/279"
      return 0
      ;;
  esac
  echo "unexpected gh invocation: $*" >&2
  return 64
}

# run_label <pr> <action> <label> → STATUS, OUT (stdout), ERR (stderr), CALLS
#
# stdout and stderr are captured apart on purpose. "Printed a success line" and
# "explained the failure" are separate claims about separate streams, and folding
# them together would let an ERROR on stderr satisfy an assertion about stdout.
run_label() {
  : >"$CALL_LOG"
  local out_file err_file
  out_file="$(mktemp)"
  err_file="$(mktemp)"
  cmd_pr_label "$@" >"$out_file" 2>"$err_file"
  STATUS=$?
  OUT="$(cat "$out_file")"
  ERR="$(cat "$err_file")"
  CALLS="$(cat "$CALL_LOG")"
  rm -f "$out_file" "$err_file"
}

reset_stub() {
  GH_AUTH_STATUS=0
  GH_VIEW_STATUS=0
  GH_VIEW_LABELS=""
  GH_EDIT_STATUS=0
}

echo "pr-label add — a write that landed, and one that did not"

reset_stub
run_label 279 add "READY TO MERGE"
assert_eq "a successful add exits 0" 0 "$STATUS"
assert_contains "a successful add says so" "$OUT" "Label 'READY TO MERGE' added to PR #279"
assert_contains "a successful add issues the write" "$CALLS" "gh pr edit 279 --add-label READY TO MERGE"

# The regression itself. Before #134 this case exited 0 and printed the success line.
reset_stub
GH_EDIT_STATUS=1
run_label 131 add "ready-for-dev"
assert_nonzero "a failed add exits non-zero" "$STATUS"
assert_not_contains "a failed add prints no success line" "$OUT" "added to PR"
assert_contains "a failed add names the label and the PR" "$ERR" "failed to add label 'ready-for-dev' to PR #131"
assert_contains "a failed add lets gh's own reason through" "$ERR" "not found in the repository"

# The word that made the old line read as a deliberate design rather than an
# unchecked one. It described a property of the API call, not of the outcome.
reset_stub
GH_EDIT_STATUS=1
run_label 131 add "ready-for-dev"
assert_not_contains "no '(idempotent)' on a write that failed" "${OUT}${ERR}" "(idempotent)"

# Adding a label the PR already carries is not an error and must not become one:
# GitHub's addLabels mutation accepts it, so the helper stays quiet about the
# distinction rather than growing a pre-check it cannot make race-free anyway.
reset_stub
GH_VIEW_LABELS="needs-review"
run_label 279 add "needs-review"
assert_eq "re-adding a present label still exits 0" 0 "$STATUS"
assert_not_contains "re-adding costs no extra read" "$CALLS" "gh pr view"

echo
echo "pr-label remove — removed, already absent, and could-not-determine"

reset_stub
GH_VIEW_LABELS=$'bug\nneeds-work'
run_label 279 remove "needs-work"
assert_eq "removing a present label exits 0" 0 "$STATUS"
assert_contains "removing a present label says so" "$OUT" "Label 'needs-work' removed from PR #279"
assert_contains "removing a present label issues the write" "$CALLS" "gh pr edit 279 --remove-label needs-work"

reset_stub
GH_VIEW_LABELS=$'bug\nneeds-review'
run_label 279 remove "READY TO MERGE"
assert_eq "an absent label is success, not failure" 0 "$STATUS"
assert_contains "an absent label is reported rather than silent" "$OUT" "not present on PR #279"
assert_not_contains "an absent label attempts no write" "$CALLS" "--remove-label"

# The second half of the defect: this used to be indistinguishable from the case
# above — no write, no output, exit 0.
reset_stub
GH_VIEW_STATUS=1
run_label 279 remove "READY TO MERGE"
assert_nonzero "an unreadable label list exits non-zero" "$STATUS"
assert_not_contains "an unreadable label list claims no removal" "$OUT" "removed from PR"
assert_not_contains "an unreadable label list is not reported as absence" "$OUT" "not present"
assert_not_contains "an unreadable label list attempts no write" "$CALLS" "--remove-label"
assert_contains "an unreadable label list carries gh's reason" "$ERR" "HTTP 502"
assert_contains "an unreadable label list refuses to guess" "$ERR" "is not the same as the label being absent"

reset_stub
GH_VIEW_LABELS="needs-work"
GH_EDIT_STATUS=1
run_label 279 remove "needs-work"
assert_nonzero "a failed removal exits non-zero" "$STATUS"
assert_not_contains "a failed removal prints no success line" "$OUT" "removed from PR"
assert_contains "a failed removal names the label and the PR" "$ERR" "failed to remove label 'needs-work' from PR #279"

echo
echo "pr-check-label — three answers, not two"

reset_stub
GH_VIEW_LABELS=$'bug\nREADY TO MERGE'
cmd_pr_check_label 279 "READY TO MERGE" 2>/dev/null
assert_eq "a present label exits 0" 0 $?

reset_stub
GH_VIEW_LABELS=$'bug\nREADY TO MERGE'
cmd_pr_check_label 279 "needs-work" 2>/dev/null
assert_eq "an absent label exits 1" 1 $?

reset_stub
GH_VIEW_STATUS=1
cmd_pr_check_label 279 "needs-work" 2>/dev/null
undetermined=$?
assert_eq "an unreadable lookup exits 2" 2 "$undetermined"
assert_eq "  and 2 is the documented sentinel" "$CHECK_LABEL_UNDETERMINED" "$undetermined"
[ "$undetermined" -ne 1 ]
assert_eq "  which is not the code for 'absent'" 0 $?

# A label whose name is a prefix of another must not match it: `grep -x` is what
# keeps `needs-work` from answering for `needs-work-later`.
reset_stub
GH_VIEW_LABELS="needs-work-later"
cmd_pr_check_label 279 "needs-work" 2>/dev/null
assert_eq "a prefix of another label is still absent" 1 $?

# Every caller branches on "present", so the undetermined code must fall to the
# else — the strict direction. `cmd_pr_status_json` reads NO_DEEPSEEK_REVIEW this
# way, where "could not tell" has to mean "not exempt".
reset_stub
GH_VIEW_STATUS=1
exempt="false"
if cmd_pr_check_label 279 "NO_DEEPSEEK_REVIEW" 2>/dev/null; then exempt="true"; fi
assert_eq "an unreadable exemption read fails closed" "false" "$exempt"

echo
echo "pr-label — argument handling"

# `die` exits the shell, so this one runs in a subshell.
out=$( (cmd_pr_label 279 frobnicate "needs-work") 2>&1 )
assert_nonzero "an unknown action exits non-zero" $?
assert_contains "an unknown action says which actions exist" "$out" "use add or remove"

echo
echo "pr-label — end to end through the CLI, with gh failing"

# The reproduction from the issue, driven through the real entry point rather than
# the sourced function: a `gh` on PATH that authenticates and then fails the write.
STUB_BIN="$(mktemp -d)"
cat >"${STUB_BIN}/gh" <<'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "auth" ]; then exit 0; fi
echo "gh: Could not resolve to a PullRequest with the number of 99999." >&2
exit 1
STUB
chmod +x "${STUB_BIN}/gh"

cli_out="$(PATH="${STUB_BIN}:${PATH}" bash "${SCRIPT_DIR}/gh-automation.sh" pr-label 99999 add ready-for-dev 2>/dev/null)"
cli_status=$?
cli_err="$(PATH="${STUB_BIN}:${PATH}" bash "${SCRIPT_DIR}/gh-automation.sh" pr-label 99999 add ready-for-dev 2>&1 >/dev/null)"
rm -rf "$STUB_BIN"

assert_nonzero "the CLI exits non-zero when the label write fails" "$cli_status"
assert_not_contains "the CLI prints no success line" "$cli_out" "added to PR"
assert_contains "the CLI surfaces gh's reason" "$cli_err" "Could not resolve to a PullRequest"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
