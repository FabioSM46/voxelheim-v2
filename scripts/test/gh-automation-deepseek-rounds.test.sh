#!/usr/bin/env bash
# =============================================================================
# Regression tests for pr-deepseek-rounds round accounting.
#
# The original defect (inherited knowledge from clinic-deck): the rounds command
# compared GraphQL's author.login ("github-actions") against the REST spelling
# ("github-actions[bot]"), so the filter never matched and every field collapsed
# to zero — silently.
#
# Run: bash scripts/test/gh-automation-deepseek-rounds.test.sh
# =============================================================================

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../gh-automation.sh
source "${SCRIPT_DIR}/gh-automation.sh"
# The sourced script sets -e; tests deliberately invoke failing commands.
set +e

pass=0
fail=0

# assert_field <test-name> <json> <jq-path> <expected>
assert_field() {
  local name="$1" json="$2" path="$3" expected="$4" actual
  actual=$(echo "$json" | jq -r "$path" 2>/dev/null)
  if [ "$actual" = "$expected" ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: ${path} expected '${expected}', got '${actual}'"
    fail=$((fail + 1))
  fi
}

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

# reviews <login:state:id:timestamp>... → GraphQL-shaped payload
#
# state doubles as the body selector, because submittedAt contains colons and so
# has to stay the last field of the spec:
#   COMMENTED → a Mode A full review; body carries the round-accounting marker
#   REPLY     → the implicit empty-body COMMENTED review GitHub wraps around a
#               thread reply
#   LEGACY    → a COMMENTED review with an unmarked body, e.g. a "review paused"
#               notice from before the notice became an issue comment
#   CLEAN     → the clean verdict: a COMMENTED review that leads with the
#               no-findings marker and carries no round-accounting marker
#   APPROVED  → an approve (no longer producible: GitHub forbids Actions from
#               approving, which is why CLEAN exists — but old PRs carry them)
reviews() {
  local nodes=""
  local spec login state id ts body
  for spec in "$@"; do
    IFS=':' read -r login state id ts <<<"$spec"
    case "$state" in
      COMMENTED) body="${DEEPSEEK_FULL_REVIEW_MARKER}"$'\n\n## General Comments' ;;
      REPLY)     state="COMMENTED"; body="" ;;
      LEGACY)    state="COMMENTED"; body="DeepSeek has reviewed this PR **3 times** (limit: 3)." ;;
      CLEAN)     state="COMMENTED"; body="${DEEPSEEK_NO_FINDINGS_MARKER}"$'\n\nDeepSeek review complete: no substantive issues found.' ;;
      *)         body="" ;;
    esac
    [ -n "$nodes" ] && nodes+=","
    nodes+=$(jq -cn --arg l "$login" --arg s "$state" --argjson d "$id" --arg t "$ts" --arg b "$body" \
      '{databaseId: $d, author: {login: $l}, state: $s, submittedAt: $t, body: $b}')
  done
  echo "{\"data\":{\"repository\":{\"pullRequest\":{\"reviews\":{\"nodes\":[${nodes}]}}}}}"
}

echo "pr-deepseek-rounds — login form matching"

# The regression itself: GraphQL omits the [bot] suffix that the default
# DEEPSEEK_BOT_USER carries. Both spellings must resolve to the same bot.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:COMMENTED:222:2026-01-01T11:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "GraphQL login matches REST-form bot_user" "$out" '.bot_review_count' 2
assert_field "latest_review_id is the newest review" "$out" '.latest_review_id' 222

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions[bot]:COMMENTED:111:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "REST-form login matches REST-form bot_user" "$out" '.bot_review_count' 1

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions[bot]:COMMENTED:111:2026-01-01T10:00:00Z")" \
  "github-actions" 3)
assert_field "REST-form login matches suffix-less bot_user" "$out" '.bot_review_count' 1

out=$(deepseek_rounds_from_graphql \
  "$(reviews "deepseek-reviewer:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:COMMENTED:222:2026-01-01T11:00:00Z")" \
  "deepseek-reviewer[bot]" 3)
assert_field "custom DEEPSEEK_BOT_USER is honoured" "$out" '.bot_review_count' 1
assert_field "custom DEEPSEEK_BOT_USER excludes other authors" "$out" '.latest_review_id' 111

out=$(deepseek_rounds_from_graphql \
  "$(reviews "FabioSM46:COMMENTED:111:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "human reviews are not counted" "$out" '.bot_review_count' 0
assert_field "no bot review leaves latest_review_id at 0" "$out" '.latest_review_id' 0

echo "pr-deepseek-rounds — derived state"

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:APPROVED:222:2026-01-01T11:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "APPROVED sets review_complete" "$out" '.review_complete' true
assert_field "APPROVED is not counted as a round" "$out" '.bot_review_count' 1
assert_field "latest_review_id includes the APPROVED review" "$out" '.latest_review_id' 222

# The clean verdict replaces the APPROVE that GitHub will not accept from an Action.
# It is terminal, and it must not spend the round budget: a clean pass on an early
# commit has to leave a later push reviewable.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:CLEAN:333:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 1)
assert_field "a clean COMMENT verdict sets review_complete" "$out" '.review_complete' true
assert_field "a clean verdict is not counted as a round" "$out" '.bot_review_count' 0
assert_field "a clean verdict does not exhaust the cap" "$out" '.review_rounds_exhausted' false
assert_field "latest_review_id includes the clean verdict" "$out" '.latest_review_id' 333

# A full review is not a clean verdict, however its body begins: the round-accounting
# marker is what tells them apart, so findings can never present as a clean pass.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:444:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "a full review does not set review_complete" "$out" '.review_complete' false

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:COMMENTED:222:2026-01-01T11:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "below MAX_ROUNDS is not exhausted" "$out" '.review_rounds_exhausted' false
assert_field "review_complete false without APPROVED" "$out" '.review_complete' false

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:COMMENTED:222:2026-01-01T11:00:00Z" \
             "github-actions:COMMENTED:333:2026-01-01T12:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "MAX_ROUNDS COMMENT reviews exhausts rounds" "$out" '.review_rounds_exhausted' true
assert_field "max_rounds is echoed back" "$out" '.max_rounds' 3

# The shipped cap (MAX_ROUNDS=1 in deepseek-pr-review.yml): a single COMMENT
# review is the whole budget, so the very first round must flip the exhausted flag.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 1)
assert_field "cap of 1 exhausts after one COMMENT review" "$out" '.review_rounds_exhausted' true
assert_field "cap of 1 is echoed back" "$out" '.max_rounds' 1

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:APPROVED:111:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 1)
assert_field "cap of 1 is not spent by an APPROVED review" "$out" '.review_rounds_exhausted' false

# submittedAt, not array order, decides which review is latest.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:222:2026-01-01T11:00:00Z" \
             "github-actions:COMMENTED:111:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "latest_review_id sorts by submittedAt" "$out" '.latest_review_id' 222

echo "pr-deepseek-rounds — only full reviews spend the round budget"

# The inflation shape: real reviews mixed with thread replies and legacy paused
# notices. At a cap of 1 that inflation is terminal, so the marker filter is what
# makes the cap mean "one review".
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:REPLY:222:2026-01-01T11:00:00Z" \
             "github-actions:COMMENTED:333:2026-01-01T12:00:00Z" \
             "github-actions:LEGACY:444:2026-01-01T13:00:00Z" \
             "github-actions:LEGACY:555:2026-01-01T14:00:00Z" \
             "github-actions:REPLY:666:2026-01-01T15:00:00Z")" \
  "github-actions[bot]" 3)
assert_field "reply/notice inflation counts 2 rounds, not 6" "$out" '.bot_review_count' 2
assert_field "reply/notice inflation is not exhausted at cap 3" "$out" '.review_rounds_exhausted' false

# A reply must never be mistaken for the review it replies to: at cap 1 that
# would spend the budget before any review had run.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:REPLY:111:2026-01-01T10:00:00Z")" \
  "github-actions[bot]" 1)
assert_field "a thread reply alone counts as zero rounds" "$out" '.bot_review_count' 0
assert_field "a thread reply alone does not exhaust the cap" "$out" '.review_rounds_exhausted' false
assert_field "a thread reply does not move latest_review_id" "$out" '.latest_review_id' 0

out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:REPLY:222:2026-01-01T11:00:00Z")" \
  "github-actions[bot]" 1)
assert_field "latest_review_id ignores a later reply wrapper" "$out" '.latest_review_id' 111

# ...but an APPROVE is a real terminal state and must still be seen.
out=$(deepseek_rounds_from_graphql \
  "$(reviews "github-actions:COMMENTED:111:2026-01-01T10:00:00Z" \
             "github-actions:APPROVED:222:2026-01-01T11:00:00Z")" \
  "github-actions[bot]" 1)
assert_field "latest_review_id still tracks an APPROVE" "$out" '.latest_review_id' 222
assert_field "APPROVE after an exhausted cap still completes" "$out" '.review_complete' true

echo "pr-deepseek-rounds — failure modes are distinguishable"

out=$(deepseek_rounds_from_graphql '{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}' \
  "github-actions[bot]" 3)
assert_field "empty review list yields a real zero" "$out" '.bot_review_count' 0
assert_field "empty review list has no error field" "$out" '.error' null

out=$(deepseek_rounds_from_graphql 'not json at all' "github-actions[bot]" 3 2>/dev/null)
assert_eq "unparseable payload exits non-zero" 1 $?
assert_eq "unparseable payload emits no zeroed success" "" "$out"

out=$(deepseek_rounds_error "REPO variable is not set" 3)
assert_field "error shape carries the message" "$out" '.error' "REPO variable is not set"
assert_field "error shape has no bot_review_count" "$out" '.bot_review_count' null

out=$(REPO="" cmd_pr_deepseek_rounds 199 2>/dev/null)
assert_eq "missing REPO exits non-zero" 1 $?
assert_field "missing REPO reports an error" "$out" '.error' "REPO variable is not set"

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
