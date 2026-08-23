#!/usr/bin/env bash
# =============================================================================
# Regression tests for pr-label — the write half of the labeler.
#
# The defect (legacy PR 134): `cmd_pr_label add` ran `gh pr edit … 2>/dev/null || true` and
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
# #206 moved both writes off `gh pr edit` and onto `gh api`, because `gh pr edit`
# pre-fetches `projectCards` and is therefore dead on the `gh` Ubuntu ships. This
# file moved with them and keeps asserting the same two guarantees — the loud
# failure, and the exact command line. The second matters more than it looks: a stub
# that matched the NEW shape while the script still issued the old one would pass
# green over a pipeline that could not write a label at all, so the `pr edit` case in
# the stub below now fails on sight rather than answering.
#
# The endpoint swap also removed a guard nobody had written down. `gh pr edit
# --add-label` refused a label the repository does not define; `POST
# issues/<n>/labels` creates it and returns 200. `repo_label_defined` is what stands
# there now, and the cases at the end of the `add` block are what keep it standing.
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
# Four knobs, because four things can independently fail: authentication, the read of
# the PR's own labels, the read of the labels the repository defines, and the write.
# Every invocation is logged, so a test can assert that a write was NOT attempted —
# which is the whole claim of both "could not determine" branches.
CALL_LOG="$(mktemp)"
trap 'rm -f "$CALL_LOG"' EXIT

# The fixture repository, injected through BOTH variables `resolve_repo` reads.
#
# `REPO` alone is not enough, and getting that wrong is what made this file pass
# locally and fail on the runner. `resolve_repo` reads
# `${GITHUB_REPOSITORY:-${REPO:-}}`, so the Actions event repository deliberately
# OUTRANKS a local `REPO` override — a rule `gh-automation-deepseek-rounds.test.sh`
# pins on purpose ("the Actions event repository wins"). Every GitHub Actions job
# exports `GITHUB_REPOSITORY`, so on the runner the ambient real slug beat the
# fixture and the `gh api repos/<slug>/…` assertions below saw
# `FabioSM46/voxelheim-v2`. On a workstation the variable is unset, `REPO` won, and
# the same assertions passed. A test whose verdict depends on where it is standing
# is not a test, and this one had that shape for exactly one push.
#
# `gh pr edit <n> --add-label` never named a repository — it inferred one from the
# working directory — so no slug reached this harness before the endpoint change and
# nothing here had to be right about it.
#
# Setting the higher-precedence variable is what actually fixes it; setting both
# says so unambiguously and leaves no ordering for a later edit to get wrong.
FIXTURE_REPO="voxelheim-test/repo"
REPO="$FIXTURE_REPO"
GITHUB_REPOSITORY="$FIXTURE_REPO"
export GITHUB_REPOSITORY

GH_AUTH_STATUS=0
GH_VIEW_STATUS=0
GH_VIEW_LABELS=""
GH_REPO_LABELS_STATUS=0
GH_REPO_LABELS=""
GH_WRITE_STATUS=0

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
    "api --paginate "*"/labels --jq"*)
      if [ "$GH_REPO_LABELS_STATUS" -ne 0 ]; then
        echo "gh: HTTP 502: Server Error (https://api.github.com/repos/voxelheim-test/repo/labels)" >&2
        return "$GH_REPO_LABELS_STATUS"
      fi
      [ -n "$GH_REPO_LABELS" ] && printf '%s\n' "$GH_REPO_LABELS"
      return 0
      ;;
    "api -X POST "*|"api -X DELETE "*)
      if [ "$GH_WRITE_STATUS" -ne 0 ]; then
        # A failed `gh api` splits itself across both streams: the API's own error
        # body on stdout, a one-line summary on stderr. `gh_label_api` captures the
        # first and must re-emit it rather than swallow it, so the stub produces
        # both and the assertions below look for both.
        echo '{"message":"Resource not accessible by personal access token","status":"403"}'
        echo "gh: Resource not accessible by personal access token (HTTP 403)" >&2
        return "$GH_WRITE_STATUS"
      fi
      echo '[{"name":"needs-review"}]'
      return 0
      ;;
    "pr edit "*)
      # #206: this is the command that cannot run on the `gh` Ubuntu ships, and
      # nothing in this script may reach it again. The stub refuses to answer rather
      # than making a reintroduction look like it works.
      echo "gh pr edit must never be issued by this script — see #206" >&2
      return 65
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
  # Re-asserted every time: a case that perturbs either variable must not be able
  # to hand the next one a different repository.
  REPO="$FIXTURE_REPO"
  GITHUB_REPOSITORY="$FIXTURE_REPO"
  GH_AUTH_STATUS=0
  GH_VIEW_STATUS=0
  GH_VIEW_LABELS=""
  GH_REPO_LABELS_STATUS=0
  # The labels this repository really defines, so an `add` in the happy path clears
  # `repo_label_defined` on its merits rather than because the guard was disabled.
  GH_REPO_LABELS=$'bug\nneeds-review\nneeds-work\nready-for-dev\nREADY TO MERGE\nDEEPSEEK_REVIEW_READ'
  GH_WRITE_STATUS=0
}

echo "the harness — the fixture repository reaches the code under test"

# This assertion exists because its absence cost a red CI run. Everything below
# asserts a `repos/<slug>/…` path, so all of it is worthless if the slug the code
# resolves is not the slug the harness injected. Pinning it directly means the next
# person sees "the fixture repository did not survive" instead of four confusing
# diffs against URLs that look almost right.
reset_stub
resolve_repo
assert_eq "the injected slug survives resolve_repo" "$FIXTURE_REPO" "$REPO"
# And specifically that it survives an Actions environment, which is the exact
# direction that broke: GITHUB_REPOSITORY is set on every runner and outranks REPO.
GITHUB_REPOSITORY="ambient-owner/ambient-repo" REPO="$FIXTURE_REPO"
( resolve_repo; [ "$REPO" = "ambient-owner/ambient-repo" ] )
assert_eq "  and an ambient GITHUB_REPOSITORY is what would have overridden it" 0 $?
reset_stub

echo
echo "pr-label add — a write that landed, and one that did not"

reset_stub
run_label 279 add "READY TO MERGE"
assert_eq "a successful add exits 0" 0 "$STATUS"
assert_contains "a successful add says so" "$OUT" "Label 'READY TO MERGE' added to PR #279"
# The exact command line, named rather than pattern-matched. A pull request is an
# issue, which is why the endpoint is `issues/…` and why this works on a `gh` where
# `gh pr edit` does not (#206).
assert_contains "a successful add issues the write" "$CALLS" \
  "gh api -X POST repos/voxelheim-test/repo/issues/279/labels -f labels[]=READY TO MERGE"
assert_contains "a successful add checks the label is defined first" "$CALLS" \
  "gh api --paginate repos/voxelheim-test/repo/labels --jq .[].name"
assert_not_contains "a successful add never touches gh pr edit" "$CALLS" "pr edit"
assert_not_contains "a successful add keeps the API payload off stdout" "$OUT" '"name"'

# The regression itself. Before legacy PR 134 this case exited 0 and printed the success line.
reset_stub
GH_WRITE_STATUS=1
run_label 131 add "ready-for-dev"
assert_nonzero "a failed add exits non-zero" "$STATUS"
assert_not_contains "a failed add prints no success line" "$OUT" "added to PR"
assert_contains "a failed add names the label and the PR" "$ERR" "failed to add label 'ready-for-dev' to PR #131"
assert_contains "a failed add lets gh's own reason through" "$ERR" "Resource not accessible by personal access token"
# `gh api` puts the API's own error body on STDOUT, which `gh_label_api` captures.
# Captured is not the same as discarded: it goes to stderr with the failure, or the
# endpoint change would have made the helper quieter than the one it replaced.
assert_contains "a failed add re-emits the API error body on stderr" "$ERR" '"status":"403"'
assert_not_contains "a failed add keeps the error body off stdout" "$OUT" '"status":"403"'

# The word that made the old line read as a deliberate design rather than an
# unchecked one. It described a property of the API call, not of the outcome.
reset_stub
GH_WRITE_STATUS=1
run_label 131 add "ready-for-dev"
assert_not_contains "no '(idempotent)' on a write that failed" "${OUT}${ERR}" "(idempotent)"

# Adding a label the PR already carries is not an error and must not become one:
# the REST endpoint accepts it, so the helper stays quiet about the distinction
# rather than growing a pre-check it cannot make race-free anyway.
reset_stub
GH_VIEW_LABELS="needs-review"
run_label 279 add "needs-review"
assert_eq "re-adding a present label still exits 0" 0 "$STATUS"
assert_not_contains "re-adding costs no read of the PR's own labels" "$CALLS" "gh pr view"

echo
echo "pr-label add — the guard the endpoint change would otherwise have removed"

# `gh pr edit --add-label` refused a label the repository does not define. `POST
# issues/<n>/labels` CREATES it and returns 200, so without `repo_label_defined` a
# typo would invent a label, attach it, and print the success line — #134's shape
# rebuilt by the fix for #206.
reset_stub
GH_REPO_LABELS=$'bug\nneeds-review'
run_label 279 add "reddy-for-dev"
assert_nonzero "a label the repository does not define is refused" "$STATUS"
assert_not_contains "an undefined label prints no success line" "$OUT" "added to PR"
assert_not_contains "an undefined label is never written" "$CALLS" "-X POST"
assert_contains "an undefined label says the endpoint would have created it" "$ERR" \
  "defines no such label, and this endpoint would create one rather than refuse"

# Third answer, failing closed: not readable is not the same as defined.
reset_stub
GH_REPO_LABELS_STATUS=1
run_label 279 add "needs-review"
assert_nonzero "an unreadable repository label list exits non-zero" "$STATUS"
assert_not_contains "an unreadable label list claims no add" "$OUT" "added to PR"
assert_not_contains "an unreadable label list attempts no write" "$CALLS" "-X POST"
assert_contains "an unreadable label list carries gh's reason" "$ERR" "HTTP 502"
assert_contains "an unreadable label list refuses to guess" "$ERR" \
  "is not the same as the label existing"

echo
echo "pr-label remove — removed, already absent, and could-not-determine"

reset_stub
GH_VIEW_LABELS=$'bug\nneeds-work'
run_label 279 remove "needs-work"
assert_eq "removing a present label exits 0" 0 "$STATUS"
assert_contains "removing a present label says so" "$OUT" "Label 'needs-work' removed from PR #279"
assert_contains "removing a present label issues the write" "$CALLS" \
  "gh api -X DELETE repos/voxelheim-test/repo/issues/279/labels/needs-work"
assert_not_contains "removing a present label never touches gh pr edit" "$CALLS" "pr edit"

# The label name is a path segment on this endpoint, and `READY TO MERGE` is a real
# label in this repository. A raw space in a URL path is not something to leave to
# whatever the HTTP client happens to do with it.
reset_stub
GH_VIEW_LABELS=$'bug\nREADY TO MERGE'
run_label 279 remove "READY TO MERGE"
assert_eq "removing a label with spaces exits 0" 0 "$STATUS"
assert_contains "a label with spaces is percent-encoded into the path" "$CALLS" \
  "gh api -X DELETE repos/voxelheim-test/repo/issues/279/labels/READY%20TO%20MERGE"

reset_stub
GH_VIEW_LABELS=$'bug\nneeds-review'
run_label 279 remove "READY TO MERGE"
assert_eq "an absent label is success, not failure" 0 "$STATUS"
assert_contains "an absent label is reported rather than silent" "$OUT" "not present on PR #279"
assert_not_contains "an absent label attempts no write" "$CALLS" "-X DELETE"

# The second half of the defect: this used to be indistinguishable from the case
# above — no write, no output, exit 0.
reset_stub
GH_VIEW_STATUS=1
run_label 279 remove "READY TO MERGE"
assert_nonzero "an unreadable label list exits non-zero" "$STATUS"
assert_not_contains "an unreadable label list claims no removal" "$OUT" "removed from PR"
assert_not_contains "an unreadable label list is not reported as absence" "$OUT" "not present"
assert_not_contains "an unreadable label list attempts no write" "$CALLS" "-X DELETE"
assert_contains "an unreadable label list carries gh's reason" "$ERR" "HTTP 502"
assert_contains "an unreadable label list refuses to guess" "$ERR" "is not the same as the label being absent"

reset_stub
GH_VIEW_LABELS="needs-work"
GH_WRITE_STATUS=1
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
# the sourced function: a `gh` on PATH that authenticates, answers the label-list
# read, and then fails the write. REPO is supplied so `resolve_repo` needs no lookup
# and the command that fails is the write itself rather than the repository probe.
STUB_BIN="$(mktemp -d)"
cat >"${STUB_BIN}/gh" <<'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "auth" ]; then exit 0; fi
if [ "${2:-}" = "--paginate" ]; then printf 'ready-for-dev\n'; exit 0; fi
echo "gh: Could not resolve to a PullRequest with the number of 99999." >&2
exit 1
STUB
chmod +x "${STUB_BIN}/gh"

# Both variables again, and for the reason spelled out at the top: under Actions an
# inherited GITHUB_REPOSITORY would outrank the REPO passed here.
run_cli() {
  PATH="${STUB_BIN}:${PATH}" REPO="$FIXTURE_REPO" GITHUB_REPOSITORY="$FIXTURE_REPO" \
    bash "${SCRIPT_DIR}/gh-automation.sh" "$@"
}

cli_out="$(run_cli pr-label 99999 add ready-for-dev 2>/dev/null)"
cli_status=$?
cli_err="$(run_cli pr-label 99999 add ready-for-dev 2>&1 >/dev/null)"
rm -rf "$STUB_BIN"

assert_nonzero "the CLI exits non-zero when the label write fails" "$cli_status"
assert_not_contains "the CLI prints no success line" "$cli_out" "added to PR"
assert_contains "the CLI surfaces gh's reason" "$cli_err" "Could not resolve to a PullRequest"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
