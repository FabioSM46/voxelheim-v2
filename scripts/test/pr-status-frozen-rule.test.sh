#!/usr/bin/env bash
# =============================================================================
# Regression tests for the frozen rule having exactly ONE implementation.
#
# The inherited defect (clinic-deck #279): cmd_pr_status evaluated the rule
# itself instead of delegating, and gated CI on `gh pr checks "$pr" --required`.
# That flag filters to the contexts named in branch protection, so a red,
# non-required check was invisible to it. The two status commands then disagreed
# about the same PR at the same moment:
#
#   pr-status      → [PASS] All conditions met — safe to add READY TO MERGE
#   pr-status-json → {"ci_failing":1,"ready_to_merge":false}
#
# The human-facing command was the one failing OPEN, and /process-pr trusts it.
# `--required` also exits 0 when the required set is empty, so a repo with no
# branch protection read green unconditionally; and the bot's round state was
# never consulted at all.
#
# cmd_pr_status now takes its verdict from cmd_pr_status_json verbatim. These
# tests stub that function to pin the delegation.
#
# The second half of the file is the same failure one layer down (#211). The verdict
# was delegated correctly and still read out as `[FAIL] ? unresolved review threads
# (must be 0)` with an exit status of 0 and an empty stderr, because `jq` was not
# installed. Every standalone `jq` call in gh-automation.sh carries `2>/dev/null` --
# rightly, since a jq that runs can still be handed an unparseable payload and the
# fail-closed sentinel is what must answer that -- and the same redirection swallowed
# `jq: command not found` at each of them. `require_jq` is the preflight; these cases
# drive the reproduction from the issue and pin that the machine-facing half did not
# change with it.
#
# Run: bash scripts/test/pr-status-frozen-rule.test.sh
# =============================================================================

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../gh-automation.sh
source "${SCRIPT_DIR}/gh-automation.sh"
# The sourced script sets -e; tests deliberately drive failing paths.
set +e

pass=0
fail=0

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: output did not contain '${needle}'"
    echo "$haystack" | sed 's/^/         /'
    fail=$((fail + 1))
  fi
}

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

assert_nonzero() {
  local name="$1" status="$2"
  if [ "$status" -ne 0 ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected a non-zero exit, got 0"
    fail=$((fail + 1))
  fi
}

assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: output unexpectedly contained '${needle}'"
    echo "$haystack" | sed 's/^/         /'
    fail=$((fail + 1))
  fi
}

# ── Stubs ────────────────────────────────────────────────────────────────────
# Only the display path may touch the network; the verdict must come from the
# stubbed cmd_pr_status_json alone.

require_gh() { :; }

gh() {
  case "$*" in
    *"pr checks"*) echo "labeler	fail	12s	https://example.invalid/job/1"; return 1 ;;
  esac
  echo "unexpected gh invocation: $*" >&2
  return 64
}

graphql_pr_review() {
  echo '{"data":{"repository":{"pullRequest":{"reviewThreads":{"totalCount":2,"nodes":[]},"reviews":{"nodes":[]}}}}}'
}

# status_stub <json> — make cmd_pr_status_json return exactly this
status_stub() {
  local json="$1"
  eval "cmd_pr_status_json() { printf '%s\n' '${json}'; }"
}

OK_PRESENCE='"checks_missing":0,"checks_missing_names":"","required_check_state":"SUCCESS","mergeable":"MERGEABLE"'
CLEAN='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,'"$OK_PRESENCE"',"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":true}'
CI_RED='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":1,"ci_pending":0,'"$OK_PRESENCE"',"deepseek_review_complete":false,"deepseek_rounds_exhausted":true,"deepseek_has_participated":true,"ready_to_merge":false}'
UNREADABLE='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":-1,"ci_pending":-1,'"$OK_PRESENCE"',"deepseek_review_complete":false,"deepseek_rounds_exhausted":true,"deepseek_has_participated":true,"ready_to_merge":false}'
DEEPSEEK_OPEN='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,'"$OK_PRESENCE"',"deepseek_review_complete":false,"deepseek_rounds_exhausted":false,"deepseek_has_participated":false,"ready_to_merge":false}'
THREADS='{"pr":279,"unresolved_threads":2,"changes_requested":1,"ci_failing":0,"ci_pending":0,'"$OK_PRESENCE"',"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":false}'

# "Nothing failing, nothing pending — because nothing ran" shapes.
NO_CI='{"pr":315,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":1,"checks_missing_names":"ci-gate","required_check_state":"MISSING","mergeable":"CONFLICTING","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":false}'
CONFLICTED='{"pr":315,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":0,"checks_missing_names":"","required_check_state":"SUCCESS","mergeable":"CONFLICTING","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":false}'
MERGE_UNKNOWN='{"pr":315,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":0,"checks_missing_names":"","required_check_state":"SUCCESS","mergeable":"UNKNOWN","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":false}'
PRESENCE_UNREADABLE='{"pr":315,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"checks_missing":-1,"checks_missing_names":"","required_check_state":"UNREADABLE","mergeable":"MERGEABLE","deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"ready_to_merge":false}'
# A payload predating the presence/mergeable/unread fields — the display path must
# stay quiet about fields it lacks rather than manufacture a reason the producer
# never gave.
LEGACY_SHAPE='{"pr":279,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,"deepseek_review_complete":false,"deepseek_rounds_exhausted":false,"deepseek_has_participated":false,"ready_to_merge":false}'

# Unread-findings shapes: every count clean, but a DeepSeek review is holding
# findings in its body.
UNREAD='{"pr":464,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,'"$OK_PRESENCE"',"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":3,"ready_to_merge":false}'
UNREAD_UNREADABLE='{"pr":464,"unresolved_threads":0,"changes_requested":0,"ci_failing":0,"ci_pending":0,'"$OK_PRESENCE"',"deepseek_review_complete":true,"deepseek_rounds_exhausted":false,"deepseek_has_participated":true,"deepseek_unread_findings":-1,"ready_to_merge":false}'

echo "pr-status — the verdict is delegated, not re-derived"

# Structural: the `--required` gate must be gone from the function body. This is
# the regression itself, so pin it directly rather than only through behaviour.
body=$(declare -f cmd_pr_status)
assert_not_contains "cmd_pr_status no longer gates on --required" "$body" "--required"
assert_contains "cmd_pr_status delegates to cmd_pr_status_json" "$body" "cmd_pr_status_json"

echo
echo "pr-status — a red non-required check can no longer read green"

status_stub "$CI_RED"
out=$(cmd_pr_status 279 2>&1)
assert_contains "red non-required check is reported" "$out" "[FAIL] 1 CI checks failing"
assert_not_contains "red non-required check never prints PASS" "$out" "[PASS]"

status_stub "$CLEAN"
out=$(cmd_pr_status 279 2>&1)
assert_contains "a genuinely ready PR still passes" "$out" "[PASS] All conditions met"
assert_not_contains "a ready PR reports no failures" "$out" "[FAIL]"

echo
echo "pr-status — fail-closed sentinels are surfaced as such"

status_stub "$UNREADABLE"
out=$(cmd_pr_status 279 2>&1)
assert_contains "unreadable CI failing count is explained" "$out" "CI checks failing — count unreadable, failing closed"
assert_contains "unreadable CI pending count is explained" "$out" "CI checks pending — count unreadable, failing closed"
assert_not_contains "a -1 is never printed as a tally" "$out" "-1 CI checks"
assert_not_contains "unreadable counts never print PASS" "$out" "[PASS]"

echo
echo "pr-status — every blocking condition is explained"

status_stub "$THREADS"
out=$(cmd_pr_status 279 2>&1)
assert_contains "unresolved threads are reported" "$out" "[FAIL] 2 unresolved review threads"
assert_contains "changes-requested reviews are reported" "$out" "[FAIL] 1 reviews requesting changes"

# All counts clean but still not ready ⇒ DeepSeek is the only remaining explanation.
status_stub "$DEEPSEEK_OPEN"
out=$(cmd_pr_status 279 2>&1)
assert_contains "an unfinished DeepSeek review is explained" "$out" "[FAIL] DeepSeek review not finished"
assert_not_contains "an unfinished DeepSeek review never prints PASS" "$out" "[PASS]"

# Findings in a review body block, and the reader is told how to clear them —
# a gate whose remedy is undocumented is one people route around.
status_stub "$UNREAD"
out=$(cmd_pr_status 464 2>&1)
assert_contains "unread body findings are reported" "$out" "[FAIL] 3 DeepSeek review(s) with unread findings in the review body"
# The remedy has to be a command the reader can actually run: `gh pr edit` is the
# one #206 found is dead on the `gh` Ubuntu ships, so the line names this script's
# own label helper instead — which is also the only implementation of the write.
assert_contains "the remedy names the label" "$out" "pr-label 464 add DEEPSEEK_REVIEW_READ"
assert_not_contains "the remedy does not send the reader to gh pr edit" "$out" "gh pr edit"
assert_contains "the report says why threads did not catch it" "$out" "These create no review thread"
assert_not_contains "unread body findings never print PASS" "$out" "[PASS]"
assert_not_contains "unread findings are not blamed on DeepSeek being unfinished" "$out" "DeepSeek review not finished"

status_stub "$UNREAD_UNREADABLE"
out=$(cmd_pr_status 464 2>&1)
assert_contains "an unreadable findings count is explained" "$out" "DeepSeek body findings — count unreadable, failing closed"
assert_not_contains "a -1 is never printed as a tally" "$out" "-1 DeepSeek review(s)"
assert_not_contains "an unreadable findings count never prints PASS" "$out" "[PASS]"

echo
echo "pr-status — a failed lookup fails closed"

cmd_pr_status_json() { return 1; }
out=$(cmd_pr_status 279 2>&1)
assert_contains "a failed status lookup is reported" "$out" "[FAIL] Could not evaluate readiness"
assert_not_contains "a failed status lookup never prints PASS" "$out" "[PASS]"

cmd_pr_status_json() { echo ""; }
out=$(cmd_pr_status 279 2>&1)
assert_contains "an empty status payload is reported" "$out" "[FAIL] Could not evaluate readiness"

echo
echo "pr-status — absence of CI is reported as absence, not as green"

status_stub "$NO_CI"
out=$(cmd_pr_status 315 2>&1)
assert_contains "missing checks are named" "$out" "[FAIL] required CI checks missing: ci-gate"
assert_contains "the reason CI is missing is named too" "$out" "[FAIL] PR has merge conflicts"
assert_not_contains "a PR with no CI never prints PASS" "$out" "[PASS]"

status_stub "$CONFLICTED"
out=$(cmd_pr_status 315 2>&1)
assert_contains "conflicts alone block readiness" "$out" "[FAIL] PR has merge conflicts"
assert_not_contains "conflicts never print PASS" "$out" "[PASS]"
assert_not_contains "present checks are not reported missing" "$out" "required CI checks missing"

status_stub "$MERGE_UNKNOWN"
out=$(cmd_pr_status 315 2>&1)
assert_contains "an uncomputed merge state fails closed" "$out" "mergeability still being computed"
assert_not_contains "an uncomputed merge state never prints PASS" "$out" "[PASS]"

status_stub "$PRESENCE_UNREADABLE"
out=$(cmd_pr_status 315 2>&1)
assert_contains "unreadable check presence is explained" "$out" "presence unreadable, failing closed"
assert_not_contains "unreadable presence is never printed as a tally" "$out" "-1 required"

# The DeepSeek fallback fires only when nothing else explained the verdict. A
# spurious missing-checks line would consume that slot and hide the real reason.
status_stub "$LEGACY_SHAPE"
out=$(cmd_pr_status 279 2>&1)
assert_contains "a legacy payload still reaches the DeepSeek explanation" "$out" "[FAIL] DeepSeek review not finished"
assert_not_contains "a legacy payload invents no missing-check reason" "$out" "required CI checks missing"
assert_not_contains "a legacy payload invents no mergeability reason" "$out" "mergeability"
assert_not_contains "a legacy payload invents no unread-findings reason" "$out" "unread findings in the review body"

echo
echo "pr-status-json — the frozen rule itself"

# Restore the real implementations, then stub only the network primitives beneath
# them. Everything above tested the display path against a stubbed verdict; this
# section tests the verdict.
# shellcheck source=../gh-automation.sh
source "${SCRIPT_DIR}/gh-automation.sh"
set +e
require_gh() { :; }

ROLLUP='{}'
MERGE_STATE='MERGEABLE'

gh_ci() {
  case "$*" in
    *"statusCheckRollup"*) printf '%s\n' "$ROLLUP"; return 0 ;;
  esac
  echo "unexpected gh_ci invocation: $*" >&2
  return 64
}

gh() {
  case "$*" in
    *"--json mergeable"*) printf '%s\n' "$MERGE_STATE"; return 0 ;;
    *"--json headRefName"*) printf 'fix/317-presence\n'; return 0 ;;
    *"auth status"*) return 0 ;;
  esac
  echo "unexpected gh invocation: $*" >&2
  return 64
}

graphql_pr_review() {
  echo '{"data":{"repository":{"pullRequest":{"reviewThreads":{"totalCount":0,"nodes":[]},"reviews":{"nodes":[]}}}}}'
}
cmd_pr_deepseek_rounds() {
  echo '{"bot_review_count":0,"max_rounds":1,"review_complete":true,"latest_review_id":1,"review_rounds_exhausted":false}'
}
cmd_pr_check_label() { return 1; }

# Row helper: the rollup mixes CheckRun (name) and StatusContext (context) shapes.
check_row() { printf '{"__typename":"CheckRun","name":"%s","status":"COMPLETED","conclusion":"%s"}' "$1" "$2"; }
pending_check_row() { printf '{"__typename":"CheckRun","name":"%s","status":"IN_PROGRESS","conclusion":null}' "$1"; }

ALL_GREEN="{\"statusCheckRollup\":[$(check_row detect SUCCESS),$(check_row server SUCCESS),$(check_row client SUCCESS),$(check_row schemas SUCCESS),$(check_row automation SUCCESS),$(check_row ci-gate SUCCESS),$(check_row review SUCCESS)]}"
# The detect-gated matrix: ci.yml skips jobs whose inputs did not change via
# job-level `if:`, and a skipped job reports its check as SKIPPED. The entire
# design rests on SKIPPED reading as present-and-green — in none of the failing
# conclusions, not pending, and satisfying the presence gate. If this case ever
# fails, ci.yml's detect job is silently blocking (or worse, passing) PRs.
DETECT_SKIPS="{\"statusCheckRollup\":[$(check_row detect SUCCESS),$(check_row server SUCCESS),$(check_row client SKIPPED),$(check_row schemas SKIPPED),$(check_row automation SKIPPED),$(check_row ci-gate SUCCESS)]}"
# A conflicting PR runs zero pull_request workflows; only push-driven external
# contexts report. Nothing red, nothing pending — and nothing there.
EXTERNAL_ONLY="{\"statusCheckRollup\":[$(check_row 'External: pages build' SUCCESS),$(check_row 'External: mirror sync' SUCCESS)]}"
ONE_ABSENT="{\"statusCheckRollup\":[$(check_row server SUCCESS),$(check_row client SUCCESS),$(check_row schemas SUCCESS)]}"
ONE_RED="{\"statusCheckRollup\":[$(check_row server SUCCESS),$(check_row client SUCCESS),$(check_row schemas SUCCESS),$(check_row ci-gate FAILURE)]}"
GATE_SKIPPED="{\"statusCheckRollup\":[$(check_row server SKIPPED),$(check_row client SKIPPED),$(check_row schemas SKIPPED),$(check_row automation SKIPPED),$(check_row ci-gate SKIPPED)]}"
GATE_PENDING="{\"statusCheckRollup\":[$(pending_check_row ci-gate)]}"
EMPTY_ROLLUP='{"statusCheckRollup":[]}'

# The rule must stay satisfiable. Without this case every assertion below would
# also pass on a helper hard-wired to answer "no".
ROLLUP="$ALL_GREEN" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "a genuinely green PR is still ready" "$out" '"ready_to_merge":true'
assert_contains "no checks are reported missing" "$out" '"checks_missing":0'
assert_contains "the aggregate gate succeeded" "$out" '"required_check_state":"SUCCESS"'

ROLLUP="$DETECT_SKIPS" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "skipped workload checks are accepted behind a successful gate" "$out" '"checks_missing":0'
assert_contains "skipped checks are not failing" "$out" '"ci_failing":0'
assert_contains "skipped checks are not pending" "$out" '"ci_pending":0'
assert_contains "a detect-gated matrix is still ready" "$out" '"ready_to_merge":true'

ROLLUP="$EXTERNAL_ONLY" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "external contexts alone are not CI" "$out" '"ready_to_merge":false'
assert_contains "the stable gate is counted absent" "$out" '"checks_missing":1'
assert_contains "the absent gate is named" "$out" '"checks_missing_names":"ci-gate"'

ROLLUP="$EMPTY_ROLLUP" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "an empty rollup is never green" "$out" '"ready_to_merge":false'

ROLLUP="$ONE_ABSENT" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "a single absent check blocks readiness" "$out" '"ready_to_merge":false'
assert_contains "only the absent gate is named" "$out" '"checks_missing_names":"ci-gate"'

# The presence check must not shadow the verdict check it sits beside.
ROLLUP="$ONE_RED" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "a red check still fails" "$out" '"ci_failing":1'
assert_contains "a red check is not reported as absent" "$out" '"checks_missing":0'
assert_contains "a red check blocks readiness" "$out" '"ready_to_merge":false'
assert_contains "the failed aggregate state is explicit" "$out" '"required_check_state":"FAILURE"'

ROLLUP="$GATE_SKIPPED" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "a skipped aggregate gate is not successful" "$out" '"required_check_state":"SKIPPED"'
assert_contains "a skipped aggregate gate blocks readiness" "$out" '"ready_to_merge":false'

ROLLUP="$GATE_PENDING" MERGE_STATE='MERGEABLE'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "a pending aggregate gate is explicit" "$out" '"required_check_state":"PENDING"'
assert_contains "a pending aggregate gate blocks readiness" "$out" '"ready_to_merge":false'

ROLLUP="$ALL_GREEN" MERGE_STATE='CONFLICTING'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "a conflicting PR is never ready" "$out" '"ready_to_merge":false'
assert_contains "the merge state is reported" "$out" '"mergeable":"CONFLICTING"'

ROLLUP="$ALL_GREEN" MERGE_STATE='UNKNOWN'
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "an uncomputed merge state is never ready" "$out" '"ready_to_merge":false'

# gh exiting non-zero for mergeable must fail closed, not read as mergeable.
gh() {
  case "$*" in
    *"--json mergeable"*) return 1 ;;
    *"--json headRefName"*) printf 'fix/317-presence\n'; return 0 ;;
    *"auth status"*) return 0 ;;
  esac
  return 64
}
ROLLUP="$ALL_GREEN"
out=$(cmd_pr_status_json 315 2>/dev/null)
assert_contains "an unreadable merge state fails closed" "$out" '"mergeable":"UNREADABLE"'
assert_contains "an unreadable merge state is never ready" "$out" '"ready_to_merge":false'

echo
echo "unread DeepSeek findings — the review shape no thread counts"

# General comments live in the review BODY, and a body creates no review thread,
# so the thread count is a proxy for "the review has been dealt with" that only
# holds for inline reviews. Clinic-deck merged a PR with three substantive body
# findings unread while every gate printed green; this section is why that cannot
# happen here.
#
# These exercise deepseek_unread_findings_from_graphql directly — it takes the
# payload, the bot login and the ack label, so it needs no network and no stubs.

MARK='<!-- deepseek:full-review -->'
NONE_MARK='<!-- deepseek:no-findings -->'
BOT='github-actions[bot]'
ACK='DEEPSEEK_REVIEW_READ'
FINDINGS=$'\n\n## General Comments\n\n*1.* `close()` leaks a session — including an open socket — on the *common* failure path.'

# review <login> <state> <submittedAt> <body> → one reviews.nodes entry. Built with jq
# so a body carrying quotes, backticks or newlines cannot break the fixture instead of
# the code.
review() {
  jq -cn --arg login "$1" --arg state "$2" --arg ts "$3" --arg body "$4" \
    '{author:{login:$login},state:$state,submittedAt:$ts,body:$body}'
}

# payload <reviews> <labels> <label-events> — each argument a comma-separated node list.
payload() {
  printf '{"data":{"repository":{"pullRequest":{"reviewThreads":{"totalCount":0,"nodes":[]},"reviews":{"nodes":[%s]},"labels":{"nodes":[%s]},"timelineItems":{"nodes":[%s]}}}}}' \
    "$1" "$2" "$3"
}

label_node() { printf '{"name":"%s"}' "$1"; }
label_event() { printf '{"createdAt":"%s","label":{"name":"%s"}}' "$1" "$2"; }

# assert_unread <name> <payload> <expected-count>
assert_unread() {
  local name="$1" pl="$2" expected="$3" actual
  actual=$(deepseek_unread_findings_from_graphql "$pl" "$BOT" "$ACK" 2>/dev/null)
  assert_eq "$name" "$expected" "$actual"
}

# The three shapes the structural rule has to separate, and it is the only rule that
# gets all three right — see the reasoning above the function.
assert_unread "an inline-only full review carries no body findings" \
  "$(payload "$(review "$BOT" COMMENTED '2026-08-10T10:00:00Z' "$MARK")" '' '')" 0
assert_unread "a full review with general comments does" \
  "$(payload "$(review "$BOT" COMMENTED '2026-08-10T10:00:00Z' "${MARK}${FINDINGS}")" '' '')" 1
assert_unread "an APPROVE carrying general comments does too" \
  "$(payload "$(review "$BOT" APPROVED '2026-08-10T10:00:00Z' "${FINDINGS}")" '' '')" 1
assert_unread "a marked clean approve does not" \
  "$(payload "$(review "$BOT" APPROVED '2026-08-10T10:00:00Z' "${NONE_MARK}"$'\n\nDeepSeek review complete: no substantive issues found. Approving.')" '' '')" 0

# The exemption is the one place a marker is trusted, so it is trusted only in the exact
# shape the script posts: an APPROVE whose body BEGINS with the marker. DeepSeek reviews
# this repository, where every marker is a string in the diff — a body that merely
# quotes it must not wave through the findings beside it.
assert_unread "a body that merely quotes the marker is not exempt" \
  "$(payload "$(review "$BOT" COMMENTED '2026-08-10T10:00:00Z' "${MARK}"$'\n\n## General Comments\n\n*1.* The `'"${NONE_MARK}"$'` marker is load-bearing; keep it in sync.')" '' '')" 1
assert_unread "an approve quoting it mid-body is not exempt either" \
  "$(payload "$(review "$BOT" APPROVED '2026-08-10T10:00:00Z' $'## General Comments\n\n*1.* Worth noting `'"${NONE_MARK}"$'` here.')" '' '')" 1
# GitHub forbids Actions from approving, so the clean verdict is a COMMENTED review that
# leads with the marker (legacy PR 22). The state can no longer be half of the test; the
# full-review marker is, and it is strictly stronger — a review carrying findings always
# has that marker, so it cannot be exempted whatever its body starts with.
assert_unread "a marked clean COMMENT verdict is exempt" \
  "$(payload "$(review "$BOT" COMMENTED '2026-08-10T10:00:00Z' "${NONE_MARK}"$'\n\nDeepSeek review complete: no substantive issues found.')" '' '')" 0
assert_unread "leading with the marker does not exempt a review that carries findings" \
  "$(payload "$(review "$BOT" COMMENTED '2026-08-10T10:00:00Z' "${NONE_MARK}${MARK}"$'\n\n## General Comments\n\n*1.* Something real.')" '' '')" 1

# An approve posted before this marker existed reads as findings. Fail-closed by
# design: one click clears it, where the opposite default silently retires findings.
assert_unread "an unmarked clean approve fails closed" \
  "$(payload "$(review "$BOT" APPROVED '2026-08-10T10:00:00Z' 'DeepSeek review complete: no substantive issues found. Approving.')" '' '')" 1

# GitHub wraps a Mode B thread reply in an implicit, empty-bodied COMMENTED review.
# Its content IS a thread, and `unresolved` counts it; counting it here as well would
# block on feedback that is already blocking.
assert_unread "an implicit reply review is not a finding" \
  "$(payload "$(review github-actions COMMENTED '2026-08-10T10:00:00Z' '')" '' '')" 0
assert_unread "another author's review body is not DeepSeek's" \
  "$(payload "$(review some-human COMMENTED '2026-08-10T10:00:00Z' 'General thoughts, no thread.')" '' '')" 0
# GraphQL says "github-actions", REST says "github-actions[bot]". A spelling mismatch
# here counts zero findings on every PR — silently, which is how the round counter
# broke before it.
assert_unread "the bot login is matched in either spelling" \
  "$(payload "$(review github-actions COMMENTED '2026-08-10T10:00:00Z' "${MARK}${FINDINGS}")" '' '')" 1

echo
echo "unread DeepSeek findings — the acknowledgement is dated, not sticky"

REVIEW_AT_10="$(review "$BOT" COMMENTED '2026-08-10T10:00:00Z' "${MARK}${FINDINGS}")"
REVIEW_AT_12="$(review "$BOT" COMMENTED '2026-08-10T12:00:00Z' "${MARK}${FINDINGS}")"

assert_unread "the label applied after the review clears it" \
  "$(payload "$REVIEW_AT_10" "$(label_node "$ACK")" "$(label_event '2026-08-10T11:00:00Z' "$ACK")")" 0
# Pre-acknowledging is acknowledging nothing: the words did not exist yet.
assert_unread "the label applied before the review does not" \
  "$(payload "$REVIEW_AT_10" "$(label_node "$ACK")" "$(label_event '2026-08-10T09:00:00Z' "$ACK")")" 1
# The case that makes this dated rather than sticky: a forced second review lands
# after an acknowledgement and blocks again on its own.
assert_unread "a review newer than the acknowledgement blocks again" \
  "$(payload "${REVIEW_AT_10},${REVIEW_AT_12}" "$(label_node "$ACK")" "$(label_event '2026-08-10T11:00:00Z' "$ACK")")" 1
assert_unread "a removed label leaves its old event powerless" \
  "$(payload "$REVIEW_AT_10" "$(label_node needs-review)" "$(label_event '2026-08-10T11:00:00Z' "$ACK")")" 1
# Truncation drops the OLDEST label events, so a present label with no dated event is
# possible in principle. It reads as unacknowledged; re-applying the label fixes it.
assert_unread "a present label with no dated event fails closed" \
  "$(payload "$REVIEW_AT_10" "$(label_node "$ACK")" '')" 1
assert_unread "another label's event is not an acknowledgement" \
  "$(payload "$REVIEW_AT_10" "$(label_node "$ACK")" "$(label_event '2026-08-10T11:00:00Z' needs-review)")" 1

# A payload that cannot be read must never answer zero — that reads as "nothing
# outstanding". Both unreadable shapes stop short of an answer, by different routes:
# a JSON document with no reviews list makes jq exit non-zero, while empty input makes
# it produce nothing at all. The caller's `-1` guard covers both (wire test below).
if deepseek_unread_findings_from_graphql '{}' "$BOT" "$ACK" >/dev/null 2>&1; then
  outcome="answered"
else
  outcome="failed"
fi
assert_eq "a payload with no reviews list fails rather than answering zero" "failed" "$outcome"
assert_eq "no payload at all yields no count either" "" \
  "$(deepseek_unread_findings_from_graphql '' "$BOT" "$ACK" 2>/dev/null)"
# The ack fields are the opposite: absent means "never acknowledged", which is already
# the safe answer, so a payload predating them still reads.
assert_unread "a payload with no ack fields still counts findings" \
  '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":['"$REVIEW_AT_10"']}}}}}' 1

# The query has to keep asking for what the acknowledgement is derived from. Read from
# a fresh subshell because this file stubs graphql_pr_review for its own tests.
REAL_QUERY=$(bash -c "source '${SCRIPT_DIR}/gh-automation.sh'; declare -f graphql_pr_review")
assert_contains "the query asks for review bodies" "$REAL_QUERY" "body"
assert_contains "the query asks for the PR's labels" "$REAL_QUERY" "labels(first:"
assert_contains "the query asks for label events" "$REAL_QUERY" "LABELED_EVENT"
# `first` returns the OLDEST page, and the newest review is the one that matters.
# The findings count reads the other way round — an old unacknowledged review is the one
# still blocking — so the window is also the widest a connection allows. Narrowing it
# silently shortens how long a finding can keep blocking.
assert_contains "reviews are read from the newest end, at the connection cap" "$REAL_QUERY" "reviews(last: 100,"

echo
echo "unread DeepSeek findings — the condition as the frozen rule sees it"

gh() {
  case "$*" in
    *"--json mergeable"*) printf '%s\n' "$MERGE_STATE"; return 0 ;;
    *"--json headRefName"*) printf 'fix/466-deepseek-general-findings-unread\n'; return 0 ;;
    *"auth status"*) return 0 ;;
  esac
  echo "unexpected gh invocation: $*" >&2
  return 64
}
graphql_pr_review() { printf '%s\n' "$WIRE_PAYLOAD"; }
ROLLUP="$ALL_GREEN"
MERGE_STATE='MERGEABLE'

# Everything else green — the shape that used to print [PASS].
WIRE_PAYLOAD="$(payload "$REVIEW_AT_10" '' '')"
out=$(cmd_pr_status_json 464 2>/dev/null)
assert_contains "unread body findings are counted" "$out" '"deepseek_unread_findings":1'
assert_contains "unread body findings block readiness" "$out" '"ready_to_merge":false'
assert_contains "and they do not masquerade as threads" "$out" '"unresolved_threads":0'

# The rule must stay satisfiable, or the label is unreachable and the gate is worse
# than the hole it closed.
WIRE_PAYLOAD="$(payload "$REVIEW_AT_10" "$(label_node "$ACK")" "$(label_event '2026-08-10T11:00:00Z' "$ACK")")"
out=$(cmd_pr_status_json 464 2>/dev/null)
assert_contains "acknowledging them clears the count" "$out" '"deepseek_unread_findings":0'
assert_contains "acknowledging them earns the label" "$out" '"ready_to_merge":true'

# NO_DEEPSEEK_REVIEW answers "should DeepSeek review this PR", not "were these words read".
cmd_pr_check_label() { return 0; }
WIRE_PAYLOAD="$(payload "$REVIEW_AT_10" '' '')"
out=$(cmd_pr_status_json 464 2>/dev/null)
assert_contains "the DeepSeek exemption does not retire existing findings" "$out" '"ready_to_merge":false'
cmd_pr_check_label() { return 1; }

WIRE_PAYLOAD=''
out=$(cmd_pr_status_json 464 2>/dev/null)
assert_contains "an unreadable payload fails closed to -1" "$out" '"deepseek_unread_findings":-1'
assert_contains "an unreadable payload is never ready" "$out" '"ready_to_merge":false'

echo
echo "jq preflight — an absent jq announces itself instead of rendering as '?'"

# Driven through the real entry point as a subprocess, which is the only place a PATH
# without jq on it means anything: this file has already sourced the script, and the
# shell running these lines has a working jq by definition.
#
# PATH is REPLACED rather than prefixed, so nothing else on the machine can supply the
# binary. That is also why `$BASH` is named absolutely — resolving `bash` would itself
# need a PATH entry, and the stub directory deliberately has exactly one.

JQ_ABSENT_DIR="$(mktemp -d)"
JQ_BROKEN_DIR="$(mktemp -d)"
API_FAIL_DIR="$(mktemp -d)"
trap 'rm -rf -- "$JQ_ABSENT_DIR" "$JQ_BROKEN_DIR" "$API_FAIL_DIR"' EXIT

# A gh that authenticates and then refuses everything else with a line naming itself.
# Nothing in this section should reach an API call; if the preflight regresses, this
# is what makes that visible instead of letting a test run touch the network.
#
# `#!/bin/sh` and not the suite's usual `#!/usr/bin/env bash`: an absolute interpreter
# is the whole point when PATH holds one directory. `env` would resolve — it is named
# absolutely too — and then fail to find `bash`, so the stub would exit 127 and the
# run would die on "gh not authenticated" with nothing about jq in it.
#
# It answers `--version` with the minimum, because `require_gh` reads the version before
# `auth status`, and a gh it cannot read would be refused before jq is ever examined.
cat >"${JQ_ABSENT_DIR}/gh" <<'STUB'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then echo "gh version 2.18.0"; exit 0; fi
if [ "${1:-}" = "auth" ]; then exit 0; fi
echo "gh: the stub was reached — the jq preflight did not run first" >&2
exit 1
STUB
chmod +x "${JQ_ABSENT_DIR}/gh"

# The issue's reproduction, verbatim in shape: nothing is installed or removed, a jq
# that exits 127 is simply put where PATH finds it first. `command -v jq` succeeds on
# this file, which is precisely why the preflight cannot be a PATH lookup alone — and
# why `require_jq` runs a real filter and checks the answer.
cp "${JQ_ABSENT_DIR}/gh" "${JQ_BROKEN_DIR}/gh"
printf '#!/bin/sh\necho "jq: command not found" >&2\nexit 127\n' >"${JQ_BROKEN_DIR}/jq"
chmod +x "${JQ_BROKEN_DIR}/jq"

INSTALL_HINT="https://jqlang.github.io/jq/download/"

# Every subcommand that reaches the standalone binary — directly, or through a callee
# whose stderr it discards. `pr-comments`, `pr-check-label` and `pr-deepseek-force-review`
# are deliberately absent: they use gh's built-in `--jq`, which is evaluated inside gh
# and needs no binary, so a preflight there would refuse work that would have succeeded.
for jq_case in "pr-status 279" \
               "pr-status-json 279" \
               "is-ready-to-merge 279" \
               "pr-label 279 add ready-for-dev" \
               "pr-deepseek-rounds 279" \
               "iteration-advance"; do
  jq_name="${jq_case%% *}"
  # Word splitting is the point: the case string carries the argv.
  # shellcheck disable=SC2086
  jq_out=$(PATH="$JQ_ABSENT_DIR" "$BASH" "${SCRIPT_DIR}/gh-automation.sh" $jq_case 2>/dev/null)
  jq_status=$?
  # shellcheck disable=SC2086
  jq_err=$(PATH="$JQ_ABSENT_DIR" "$BASH" "${SCRIPT_DIR}/gh-automation.sh" $jq_case 2>&1 >/dev/null)

  assert_nonzero "${jq_name} exits non-zero with no jq" "$jq_status"
  assert_contains "${jq_name} names the missing tool" "$jq_err" "jq"
  assert_contains "${jq_name} says how to install it" "$jq_err" "$INSTALL_HINT"
  assert_eq "${jq_name} writes nothing to stdout" "" "$jq_out"
done

echo
echo "jq preflight — the reproduction from the issue"

repro_out=$(PATH="$JQ_BROKEN_DIR" "$BASH" "${SCRIPT_DIR}/gh-automation.sh" pr-status 279 2>/dev/null)
repro_status=$?
repro_err=$(PATH="$JQ_BROKEN_DIR" "$BASH" "${SCRIPT_DIR}/gh-automation.sh" pr-status 279 2>&1 >/dev/null)

assert_nonzero "a jq on PATH that does not run is still a missing jq" "$repro_status"
assert_contains "the diagnostic reaches stderr" "$repro_err" "jq"
assert_contains "and carries the install hint" "$repro_err" "$INSTALL_HINT"
assert_not_contains "the '?' verdict is gone" "$repro_out" "[FAIL] ?"
assert_not_contains "and so is the sentence about GitHub" "$repro_out" "unresolved review threads (must be 0)"

echo
echo "jq preflight — what it must NOT block"

# `require_jq` is called from the command dispatch, not at file scope, for the same
# reason `require_gh` is not: a path that needs neither tool must stay usable without
# them. The usage text is that path, and hoisting the check to the top of the file is
# the change these three cases exist to catch.
#
# Run under the broken shim on a full PATH rather than the one-entry directory above:
# the usage block is a heredoc into `cat`, so a PATH with no coreutils would fail it
# for a reason that has nothing to do with the preflight. What matters here is that
# jq is unusable, and the shim is exactly that.
help_out=$(PATH="${JQ_BROKEN_DIR}:${PATH}" "$BASH" "${SCRIPT_DIR}/gh-automation.sh" --help 2>&1)
help_status=$?
assert_eq "the usage path still exits 0 with no jq" "0" "$help_status"
assert_contains "the usage path still prints usage" "$help_out" "Usage: gh-automation.sh"
assert_not_contains "and says nothing about jq" "$help_out" "jq"

echo
echo "jq preflight — the fail-closed contract survives it"

# The half of #211 that was already correct, and the half the fix must not disturb: a
# preflight that dies on an absent tool and a count that could not be read at runtime
# are different things. With jq present and the API refusing every call, pr-status-json
# must still answer -1 — never 0 — and still say so on stderr.
cat >"${API_FAIL_DIR}/gh" <<'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "--version" ]; then echo "gh version 2.18.0"; exit 0; fi
if [ "${1:-}" = "auth" ]; then exit 0; fi
echo "gh: API rate limit exceeded" >&2
exit 1
STUB
chmod +x "${API_FAIL_DIR}/gh"

FIXTURE_REPO="voxelheim-test/repo"
run_api_fail() {
  PATH="${API_FAIL_DIR}:${PATH}" REPO="$FIXTURE_REPO" GITHUB_REPOSITORY="$FIXTURE_REPO" \
    "$BASH" "${SCRIPT_DIR}/gh-automation.sh" pr-status-json 279
}
closed_out=$(run_api_fail 2>/dev/null)
closed_err=$(run_api_fail 2>&1 >/dev/null)

assert_contains "an unreadable thread count is still -1" "$closed_out" '"unresolved_threads":-1'
assert_contains "an unreadable ci_failing is still -1" "$closed_out" '"ci_failing":-1'
assert_contains "an unreadable ci_pending is still -1" "$closed_out" '"ci_pending":-1'
assert_contains "the required check is still UNREADABLE" "$closed_out" '"required_check_state":"UNREADABLE"'
assert_contains "and the PR is still not ready" "$closed_out" '"ready_to_merge":false'
assert_contains "a WARN per unreadable field still reaches stderr" "$closed_err" "failing closed"
assert_not_contains "the preflight adds no line when jq works" "$closed_err" "jq"

echo
echo "gh version preflight — a gh too old for the pipeline is refused before any API call"

# #1281, the #211 fix one tool over. A gh older than the pipeline needs used to pass
# `require_gh` — it was on PATH and authenticated — and the missing feature surfaced
# only when a command needed it: `pr-merge --head` as `unknown flag:
# --match-head-commit`, right after `is-ready-to-merge` had passed.
#
# The stub logs every invocation, and a refusal must leave exactly one line in that
# log: `--version`. No `auth status`, and no API call.
GH_VERSION_DIR="$(mktemp -d)"
GH_CALL_LOG="$(mktemp)"
GH_ERR_FILE="$(mktemp)"
trap 'rm -rf -- "$JQ_ABSENT_DIR" "$JQ_BROKEN_DIR" "$API_FAIL_DIR" "$GH_VERSION_DIR" "$GH_CALL_LOG" "$GH_ERR_FILE"' EXIT

cat >"${GH_VERSION_DIR}/gh" <<'STUB'
#!/bin/sh
printf '%s\n' "$*" >>"$GH_CALL_LOG"
if [ "${1:-}" = "--version" ]; then
  [ -n "$GH_STUB_VERSION" ] && printf '%s\n' "$GH_STUB_VERSION"
  exit "${GH_STUB_VERSION_STATUS:-0}"
fi
if [ "${1:-}" = "auth" ]; then exit 0; fi
echo "gh: no API is reachable from this test" >&2
exit 1
STUB
chmod +x "${GH_VERSION_DIR}/gh"

# run_gh_version <gh --version output> <helper argv...>  → GH_OUT GH_ERR GH_STATUS GH_CALLS
run_gh_version() {
  local version="$1"
  shift
  : >"$GH_CALL_LOG"
  GH_OUT=$(PATH="${GH_VERSION_DIR}:${PATH}" GH_CALL_LOG="$GH_CALL_LOG" GH_STUB_VERSION="$version" \
    REPO="$FIXTURE_REPO" GITHUB_REPOSITORY="$FIXTURE_REPO" \
    INTEGRATION_SHA="abc123" INTEGRATION_RUN_URL="https://example.invalid/run/1" \
    "$BASH" "${SCRIPT_DIR}/gh-automation.sh" "$@" 2>"$GH_ERR_FILE")
  GH_STATUS=$?
  GH_ERR=$(<"$GH_ERR_FILE")
  GH_CALLS=$(<"$GH_CALL_LOG")
}

GH_INSTALL_HINT="https://github.com/cli/cli#installation"
OLD_UBUNTU_GH="gh version 2.4.0+dfsg1 (2022-03-23 Ubuntu 2.4.0+dfsg1-2)"

assert_eq "the minimum is the one the audit set" "2.18.0" "$GH_MIN_VERSION"
assert_contains "and README's Toolchain row states the same number" \
  "$(<"${SCRIPT_DIR}/../README.md")" "| gh    | ${GH_MIN_VERSION} or newer"

# The reproduction from the issue, through every subcommand that calls `require_gh`.
# `is-ready-to-merge` is the one that matters most: it reaches gh inside a subshell
# whose stderr it discards, so a refusal there alone would print "NOT ready" instead.
for gh_case in "pr-status 279" \
               "pr-status-json 279" \
               "pr-comments 279" \
               "pr-edit 279 --title retitled" \
               "pr-label 279 add ready-for-dev" \
               "pr-deepseek-rounds 279" \
               "pr-deepseek-force-review 279" \
               "is-ready-to-merge 279" \
               "pr-merge 279" \
               "iteration-advance" \
               "integration-report"; do
  gh_name="${gh_case%% *}"
  # Word splitting is the point: the case string carries the argv.
  # shellcheck disable=SC2086
  run_gh_version "$OLD_UBUNTU_GH" $gh_case
  assert_nonzero "${gh_name} refuses gh 2.4.0+dfsg1" "$GH_STATUS"
  assert_contains "${gh_name} names the installed version" "$GH_ERR" "gh 2.4.0 is too old"
  assert_contains "${gh_name} names the required minimum" "$GH_ERR" "requires gh ${GH_MIN_VERSION} or newer"
  assert_contains "${gh_name} says how to install a current release" "$GH_ERR" "$GH_INSTALL_HINT"
  assert_eq "${gh_name} writes nothing to stdout" "" "$GH_OUT"
  assert_eq "${gh_name} makes no call but the version probe — no auth, no API" "--version" "$GH_CALLS"
done

echo
echo "gh version preflight — the comparison is numeric, per component"

for old in "gh version 2.17.9" "gh version 2.9.0" "gh version 1.100.0"; do
  base="${old#gh version }"
  run_gh_version "$old" pr-status-json 279
  assert_nonzero "${base} is refused" "$GH_STATUS"
  assert_contains "${base} is named in the refusal" "$GH_ERR" "gh ${base} is too old"
  assert_eq "${base} is refused before any other call" "--version" "$GH_CALLS"
done

# `2.100.0` is the case a string compare gets wrong: as text it sorts before `2.18.0`.
# The multi-line shape is what a real gh prints.
for good in "gh version ${GH_MIN_VERSION}" \
            $'gh version 2.100.0 (2026-09-03)\nhttps://github.com/cli/cli/releases/tag/v2.100.0' \
            "gh version 3.0.0"; do
  base="${good#gh version }"
  base="${base%% *}"
  base="${base%%$'\n'*}"
  run_gh_version "$good" pr-comments 279
  assert_eq "${base} passes the preflight" "0" "$GH_STATUS"
  assert_not_contains "${base} draws no version refusal" "$GH_ERR" "too old"
  assert_contains "${base} goes on to gh auth status" "$GH_CALLS" "auth status"
  assert_contains "${base} goes on to the API" "$GH_CALLS" "api repos/${FIXTURE_REPO}/pulls/279/comments"
done

echo
echo "gh version preflight — output it cannot read fails closed"

for unreadable in "gh version DEV" "gh version 2.18" "hub version 2.14.2"; do
  run_gh_version "$unreadable" pr-status-json 279
  assert_nonzero "'${unreadable}' is refused" "$GH_STATUS"
  assert_contains "'${unreadable}' is reported as unreadable" "$GH_ERR" "could not read the gh version"
  assert_contains "'${unreadable}' is quoted in the refusal" "$GH_ERR" "printed '${unreadable}'"
  assert_contains "'${unreadable}' still names the minimum" "$GH_ERR" "requires gh ${GH_MIN_VERSION} or newer"
  assert_eq "'${unreadable}' is refused before any other call" "--version" "$GH_CALLS"
done

run_gh_version "" pr-status-json 279
assert_nonzero "an empty version is refused" "$GH_STATUS"
assert_contains "and says nothing was printed" "$GH_ERR" "printed 'nothing'"

# A supported version string from a probe that failed is not a supported gh.
export GH_STUB_VERSION_STATUS=3
run_gh_version "gh version 2.45.0" pr-status-json 279
unset GH_STUB_VERSION_STATUS
assert_nonzero "a failing version probe is refused whatever it printed" "$GH_STATUS"
assert_contains "and the exit status is named" "$GH_ERR" "exited 3"
assert_eq "and nothing else is called" "--version" "$GH_CALLS"

echo
echo "gh version preflight — once per process, and not skippable"

# `is-ready-to-merge` calls `require_gh` itself, again inside cmd_pr_status_json's
# subshell, and again in the DeepSeek rounds read nested inside that. Counting the
# `auth status` lines proves the repeat calls happened — at least two, without pinning
# how deep the nesting runs today; one `--version` line is the claim.
run_gh_version "gh version ${GH_MIN_VERSION}" is-ready-to-merge 279
probes=$(printf '%s\n' "$GH_CALLS" | grep -cx -- '--version')
auths=$(printf '%s\n' "$GH_CALLS" | grep -cx -- 'auth status')
if [ "$auths" -ge 2 ]; then
  echo "  ok   — require_gh ran more than once in one process (${auths} times)"
  pass=$((pass + 1))
else
  echo "  FAIL — require_gh ran more than once in one process: ran ${auths} time(s), so the next case proves nothing"
  fail=$((fail + 1))
fi
assert_eq "and gh --version ran once" "1" "$probes"

export GH_VERSION_OK="2.45.0"
run_gh_version "$OLD_UBUNTU_GH" pr-status-json 279
unset GH_VERSION_OK
assert_nonzero "an inherited GH_VERSION_OK does not skip the check" "$GH_STATUS"
assert_contains "and the old gh is still named" "$GH_ERR" "gh 2.4.0 is too old"

run_gh_version "$OLD_UBUNTU_GH" --help
assert_eq "the usage path still exits 0 on an old gh" "0" "$GH_STATUS"
assert_contains "the usage path still prints usage" "$GH_OUT" "Usage: gh-automation.sh"
assert_eq "and never runs gh at all" "" "$GH_CALLS"

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
