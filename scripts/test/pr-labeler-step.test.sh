#!/usr/bin/env bash
# =============================================================================
# Regression tests for the PR Labeler's "Process open PRs" step.
#
# The inherited defect (clinic-deck #279): the step captured `pr-status-json`
# with `2>&1`, so the helper's `[WARN] … failing closed` diagnostics were
# prefixed onto the JSON on stdout. Every `json.load` then raised, python3
# exited non-zero, and because the step runs under `shell: bash -e` — where
# `VAR=$(cmd)` inherits the substitution's exit status — a failing assignment
# killed the whole step. Symptom: the labeler died right after printing
# `Status:`, before any label was applied, and every open PR went unlabelled.
#
# These tests execute the workflow's run block VERBATIM — extracted from the
# YAML, not copied — against stubbed `gh` and `gh-automation.sh`. That is the
# point: a copied-out fixture would drift from the shipped workflow and keep
# passing while the real step broke again.
#
# Run: bash scripts/test/pr-labeler-step.test.sh
# =============================================================================

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/pr-labeler.yml"

pass=0
fail=0
STEP_OUT=""
STEP_EXIT=0

# assert_eq <test-name> <expected> <actual>
assert_eq() {
  local name="$1" expected="$2" actual="$3"
  if [ "$actual" = "$expected" ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected '${expected}', got '${actual}'"
    fail=$((fail + 1))
  fi
}

# assert_contains <test-name> <haystack> <needle>
assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: output did not contain '${needle}'"
    echo "         ---- output ----"
    echo "$haystack" | sed 's/^/         /'
    fail=$((fail + 1))
  fi
}

# assert_not_contains <test-name> <haystack> <needle>
assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: output unexpectedly contained '${needle}'"
    fail=$((fail + 1))
  fi
}

# extract_run_block <yaml-file>
#
# Pulls the first `run: |` block scalar out of the workflow and strips its
# indentation, per the YAML rule that the block's indent is set by its first
# non-empty line. Deliberately dependency-free — no PyYAML — so this test has
# nothing to install on a runner.
extract_run_block() {
  awk '
    !found && $0 ~ /^[[:space:]]*run: \|[[:space:]]*$/ {
      match($0, /^[[:space:]]*/); key = RLENGTH; found = 1; next
    }
    found {
      if ($0 ~ /^[[:space:]]*$/) { print ""; next }
      match($0, /^[[:space:]]*/); ind = RLENGTH
      if (ind <= key) exit
      if (!content) content = ind
      print substr($0, content + 1)
    }
  ' "$1"
}

# run_step <stdout> <stderr> <exit-code> → sets STEP_OUT and STEP_EXIT
#
# Builds a throwaway tree holding stubbed `gh` and `scripts/gh-automation.sh`,
# then runs the extracted block under `bash -e` — the exact shell GitHub Actions
# uses (`shell: /usr/bin/bash -e {0}`). Reproducing that flag is the whole test:
# under plain `bash` the original defect does not reproduce at all.
#
# Results come back through globals rather than stdout because the step's exit
# status IS the assertion; `$(run_step …)` would run it in a subshell and lose it.
run_step() {
  local stub_stdout="$1" stub_stderr="$2" stub_exit="$3"
  local tmp
  tmp="$(mktemp -d)"

  mkdir -p "${tmp}/scripts" "${tmp}/bin"

  cat > "${tmp}/bin/gh" <<'STUB'
#!/usr/bin/env bash
# Only `gh pr list` is reachable from the step; anything else is a test bug.
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "list" ]; then
  echo "279"
  exit 0
fi
echo "unexpected gh invocation: $*" >&2
exit 64
STUB

  cat > "${tmp}/scripts/gh-automation.sh" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  pr-status-json)
    [ -n "${STUB_STDERR:-}" ] && printf '%s\n' "${STUB_STDERR}" >&2
    [ -n "${STUB_STDOUT:-}" ] && printf '%s\n' "${STUB_STDOUT}"
    exit "${STUB_EXIT:-0}"
    ;;
  pr-label)
    echo "[LABEL] $*"
    exit 0
    ;;
esac
echo "unexpected helper subcommand: $*" >&2
exit 64
STUB

  chmod +x "${tmp}/bin/gh" "${tmp}/scripts/gh-automation.sh"
  extract_run_block "$WORKFLOW" > "${tmp}/step.sh"

  STEP_OUT=$(cd "$tmp" && PATH="${tmp}/bin:${PATH}" \
    STUB_STDOUT="$stub_stdout" STUB_STDERR="$stub_stderr" STUB_EXIT="$stub_exit" \
    GH_TOKEN="stub" DEEPSEEK_BOT_USER="github-actions[bot]" \
    bash -e ./step.sh 2>&1)
  STEP_EXIT=$?

  rm -rf "$tmp"
}

PRESENT='"checks_missing":0,"checks_missing_names":"","required_check_state":"SUCCESS","mergeable":"MERGEABLE"'
READY_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,'"$PRESENT"',"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":0,"ready_to_merge":true}'
FAILCLOSED_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":-1,"ci_pending":-1,'"$PRESENT"',"deepseek_review_complete":false,"deepseek_rounds_exhausted":false,"deepseek_has_participated":false,"deepseek_unread_findings":0,"ready_to_merge":false}'
PENDING_JSON='{"pr":279,"unresolved_threads":2,"changes_requested":0,"ci_failing":0,"ci_pending":1,'"$PRESENT"',"deepseek_review_complete":false,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":0,"ready_to_merge":false}'
# Every count the step used to branch on is 0, and the PR still is not ready —
# DeepSeek left findings in a review body, where no thread counts them.
UNREAD_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,'"$PRESENT"',"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":3,"ready_to_merge":false}'
SKIPPED_GATE_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":0,"checks_missing_names":"","required_check_state":"SKIPPED","mergeable":"MERGEABLE","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":0,"ready_to_merge":false}'

# Nothing failing, nothing pending, nothing there.
NO_CI_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":1,"checks_missing_names":"ci-gate","required_check_state":"MISSING","mergeable":"CONFLICTING","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":0,"ready_to_merge":false}'
CONFLICT_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":0,"checks_missing_names":"","required_check_state":"SUCCESS","mergeable":"CONFLICTING","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":0,"ready_to_merge":false}'
LEGACY_JSON='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":false}'

echo "pr-labeler step — the run block is extractable"

block=$(extract_run_block "$WORKFLOW")
workflow_text=$(<"$WORKFLOW")
assert_contains "labeler reruns after CI or DeepSeek completes" "$workflow_text" \
  "workflows: [CI, DeepSeek PR Review]"
assert_contains "run block contains the status call" "$block" "pr-status-json"
assert_contains "run block contains the label call" "$block" "pr-label"
assert_not_contains "run block is dedented" "$block" "          PR_LIST="

echo
echo "pr-labeler step — stderr never contaminates the parsed JSON"

# The regression: helper warns on stderr while returning perfectly good JSON.
# Before the fix this aborted the step; the warning must stay out of $STATUS.
run_step "$FAILCLOSED_JSON" "[WARN] Could not determine ci_failing for PR #279 — failing closed" 0
assert_eq "warn-on-stderr does not abort the step" 0 "$STEP_EXIT"
assert_contains "warn-on-stderr still reaches the log" "$STEP_OUT" "[WARN] Could not determine ci_failing"
assert_contains "fail-closed counts route to needs-work" "$STEP_OUT" "[LABEL] pr-label 279 add needs-work"
assert_contains "fail-closed counts parse as -1" "$STEP_OUT" "ci_failing=-1"

# Stderr noise alongside a clean, ready payload must not cost the label.
run_step "$READY_JSON" "gh: a deprecation notice on stderr" 0
assert_eq "stderr noise does not abort a ready PR" 0 "$STEP_EXIT"
assert_contains "ready PR still gets READY TO MERGE" "$STEP_OUT" "[LABEL] pr-label 279 add READY TO MERGE"
assert_contains "ready PR drops needs-review" "$STEP_OUT" "[LABEL] pr-label 279 remove needs-review"

echo
echo "pr-labeler step — malformed input degrades, never aborts"

run_step "not json at all" "" 0
assert_eq "unparseable stdout does not abort the step" 0 "$STEP_EXIT"
assert_contains "unparseable stdout fails closed to needs-work" "$STEP_OUT" "[LABEL] pr-label 279 add needs-work"
assert_contains "unparseable fields read back empty" "$STEP_OUT" "ci_failing= "

run_step "" "boom" 1
assert_eq "helper failure does not abort the step" 0 "$STEP_EXIT"
assert_contains "helper failure skips the PR" "$STEP_OUT" "WARNING: Failed to get status for PR #279"
assert_not_contains "skipped PR gets no labels" "$STEP_OUT" "[LABEL]"

echo
echo "pr-labeler step — the frozen rule still routes correctly"

run_step "$PENDING_JSON" "" 0
assert_eq "pending state does not abort the step" 0 "$STEP_EXIT"
assert_contains "pending checks or threads route to needs-review" "$STEP_OUT" "[LABEL] pr-label 279 add needs-review"
assert_not_contains "pending state never earns READY TO MERGE" "$STEP_OUT" "add READY TO MERGE"

run_step "$SKIPPED_GATE_JSON" "" 0
assert_contains "a skipped aggregate gate routes to needs-review" "$STEP_OUT" "[LABEL] pr-label 279 add needs-review"
assert_contains "a skipped aggregate gate removes READY TO MERGE" "$STEP_OUT" "[LABEL] pr-label 279 remove READY TO MERGE"
assert_not_contains "a skipped aggregate gate is waiting, not failing" "$STEP_OUT" "add needs-work"

echo
echo "pr-labeler step — a PR whose CI never ran is routed, not dropped"

# Every count is 0 because nothing ran: a conflicting PR has no computable merge
# ref, so no pull_request workflow is ever created. Without the presence and
# mergeable conditions this matches neither the needs-work branch (nothing
# failing) nor the needs-review branch (nothing pending) and falls through to
# "Unknown state — no label changes", leaving whatever stale label the PR
# already carried.
run_step "$NO_CI_JSON" "" 0
assert_eq "a PR with no CI does not abort the step" 0 "$STEP_EXIT"
assert_contains "absent CI routes to needs-review" "$STEP_OUT" "[LABEL] pr-label 279 add needs-review"
assert_not_contains "absent CI never earns READY TO MERGE" "$STEP_OUT" "add READY TO MERGE"
assert_not_contains "absent CI is waiting, not failing" "$STEP_OUT" "add needs-work"
assert_not_contains "absent CI is never an unknown state" "$STEP_OUT" "Unknown state"
assert_contains "the new signals reach the job log" "$STEP_OUT" "checks_missing=1"
assert_contains "the missing gate state reaches the job log" "$STEP_OUT" "required_check_state=MISSING"

# Conflicts with every required check present must still route, on the
# mergeability signal alone.
run_step "$CONFLICT_JSON" "" 0
assert_contains "a conflicting PR routes to needs-review" "$STEP_OUT" "[LABEL] pr-label 279 add needs-review"
assert_not_contains "a conflicting PR never earns READY TO MERGE" "$STEP_OUT" "add READY TO MERGE"
assert_not_contains "a conflicting PR is never an unknown state" "$STEP_OUT" "Unknown state"

# A payload predating the presence/mergeable/unread fields carries none of them,
# so python3 raises KeyError and the `||` fallbacks fire. All sentinels must fail
# closed rather than read as "nothing missing, and mergeable".
run_step "$LEGACY_JSON" "" 0
assert_contains "a legacy payload fails closed to needs-review" "$STEP_OUT" "[LABEL] pr-label 279 add needs-review"
assert_contains "an absent checks_missing falls back to -1" "$STEP_OUT" "checks_missing=-1"
assert_contains "an absent required gate state fails closed" "$STEP_OUT" "required_check_state=UNREADABLE"
assert_contains "an absent mergeable falls back to UNREADABLE" "$STEP_OUT" "mergeable=UNREADABLE"
assert_contains "an absent deepseek_unread_findings falls back to -1" "$STEP_OUT" "deepseek_unread=-1"

echo
echo "pr-labeler step — findings nobody read take the label back off"

# The routing that matters: unlike every other DeepSeek signal this one is not
# monotonic, so a forced second review can put findings on a PR that already
# earned the label. Left out of the branch conditions, this shape scores 0
# everywhere the step looks and falls through to "Unknown state — no label
# changes", keeping a READY TO MERGE the frozen rule no longer supports.
run_step "$UNREAD_JSON" "" 0
assert_eq "unread findings do not abort the step" 0 "$STEP_EXIT"
assert_contains "unread findings route to needs-review" "$STEP_OUT" "[LABEL] pr-label 279 add needs-review"
assert_contains "unread findings take READY TO MERGE back off" "$STEP_OUT" "[LABEL] pr-label 279 remove READY TO MERGE"
assert_not_contains "unread findings never earn READY TO MERGE" "$STEP_OUT" "add READY TO MERGE"
assert_not_contains "unread findings are waiting, not failing" "$STEP_OUT" "add needs-work"
assert_not_contains "unread findings are never an unknown state" "$STEP_OUT" "Unknown state"
assert_contains "the count reaches the job log" "$STEP_OUT" "deepseek_unread=3"

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
