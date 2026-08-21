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
assert_contains "the remedy names the label" "$out" "gh pr edit 464 --add-label DEEPSEEK_REVIEW_READ"
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
# leads with the marker (#22). The state can no longer be half of the test; the
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
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
