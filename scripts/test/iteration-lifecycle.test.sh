#!/usr/bin/env bash
# =============================================================================
# Regression tests for completion-driven iteration ceremony transitions.
#
# Run: bash scripts/test/iteration-lifecycle.test.sh
# =============================================================================

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="${REPO_ROOT}/scripts/gh-automation.sh"
WORKFLOW="${REPO_ROOT}/.github/workflows/iteration-lifecycle.yml"

# shellcheck source=../gh-automation.sh
source "$SCRIPT"
# The sourced helper enables -e; failure paths below are assertions.
set +e
REPO="example/repository"

pass=0
fail=0
ADVANCE_OUT=""
ADVANCE_STATUS=0
MILESTONES_JSON='[]'
CEREMONIES_JSON='[]'
TMP_DIR="$(mktemp -d)"
TITLE_LOG="${TMP_DIR}/titles"
BODY_LOG="${TMP_DIR}/body"
trap 'rm -rf "$TMP_DIR"' EXIT

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

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: output did not contain '${needle}'"
    fail=$((fail + 1))
  fi
}

assert_file_contains() {
  local name="$1" file="$2" needle="$3"
  if [ -f "$file" ] && grep -Fq -- "$needle" "$file"; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: ${file} did not contain '${needle}'"
    fail=$((fail + 1))
  fi
}

assert_file_not_contains() {
  local name="$1" file="$2" needle="$3"
  if [ -f "$file" ] && ! grep -Fq -- "$needle" "$file"; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: ${file} unexpectedly contained '${needle}'"
    fail=$((fail + 1))
  fi
}

require_gh() { :; }

gh() {
  if [ "${1:-}" = "api" ]; then
    printf '%s\n' "$MILESTONES_JSON"
    return 0
  fi

  if [ "${1:-}" = "issue" ] && [ "${2:-}" = "list" ]; then
    printf '%s\n' "$CEREMONIES_JSON"
    return 0
  fi

  if [ "${1:-}" = "issue" ] && [ "${2:-}" = "create" ]; then
    shift 2
    local title="" body=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --title) title="$2"; shift 2 ;;
        --body) body="$2"; shift 2 ;;
        --label|--repo) shift 2 ;;
        *) echo "unexpected issue-create argument: $1" >&2; return 64 ;;
      esac
    done
    printf '%s\n' "$title" >> "$TITLE_LOG"
    printf '%s\n' "$body" > "$BODY_LOG"
    echo "https://example.invalid/issues/ceremony"
    return 0
  fi

  echo "unexpected gh invocation: $*" >&2
  return 64
}

# closed_issues defaults to a non-zero stand-in: every case that reaches the
# ceremony logic describes an iteration that actually committed work. The
# "never started" cases pass 0 explicitly.
milestone_entry() {
  local number="$1" title="$2" open_issues="$3" closed_issues="${4:-3}"
  jq -cn --argjson number "$number" --arg title "$title" \
    --argjson open "$open_issues" --argjson closed "$closed_issues" \
    '{number: $number, title: $title, open_issues: $open, closed_issues: $closed}'
}

# Several milestones can be open at once — planning ahead is a supported state —
# so the fixture takes as many entries as the case needs. The single-milestone
# helper is the one-entry spelling of it, unchanged for every case below.
milestones_json() {
  printf '%s\n' "$@" | jq -cs '.'
}

milestone_json() {
  milestones_json "$(milestone_entry "$@")"
}

many_ceremonies_json() {
  local count="$1"
  jq -cn --argjson count "$count" '[
    range($count) | {number: ., state: "CLOSED", body: "unrelated ceremony", url: "https://example.invalid/x"}
  ]'
}

ceremony_json() {
  local number="$1" state="$2" marker="$3"
  jq -cn --argjson number "$number" --arg state "$state" --arg marker "$marker" \
    '[{number: $number, state: $state, body: $marker, url: "https://example.invalid/ceremony"}]'
}

two_ceremonies_json() {
  local marker="$1"
  jq -cn --arg marker "$marker" '[
    {number: 1, state: "CLOSED", body: $marker, url: "https://example.invalid/1"},
    {number: 2, state: "OPEN", body: $marker, url: "https://example.invalid/2"}
  ]'
}

refinement_and_plan_json() {
  local refinement_state="$1" planning_state="$2" milestone_number="$3"
  local refinement planning
  refinement=$(iteration_marker backlog-refine "$milestone_number")
  planning=$(iteration_marker iteration-plan "$milestone_number")
  jq -cn \
    --arg refinement_state "$refinement_state" \
    --arg planning_state "$planning_state" \
    --arg refinement "$refinement" \
    --arg planning "$planning" \
    '[
      {number: 1, state: $refinement_state, body: $refinement, url: "https://example.invalid/1"},
      {number: 2, state: $planning_state, body: $planning, url: "https://example.invalid/2"}
    ]'
}

reset_case() {
  MILESTONES_JSON='[]'
  CEREMONIES_JSON='[]'
  : > "$TITLE_LOG"
  : > "$BODY_LOG"
}

run_advance() {
  ADVANCE_OUT=$(cmd_iteration_advance 2>&1)
  ADVANCE_STATUS=$?
}

creation_count() {
  if [ -s "$TITLE_LOG" ]; then
    wc -l < "$TITLE_LOG" | tr -d ' '
  else
    echo 0
  fi
}

echo "iteration lifecycle — completion gate"

reset_case
run_advance
assert_eq "no active milestone is a clean no-op" 0 "$ADVANCE_STATUS"
assert_eq "no active milestone creates nothing" 0 "$(creation_count)"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 2)
run_advance
assert_eq "active work is a clean no-op" 0 "$ADVANCE_STATUS"
assert_contains "remaining work is explained" "$ADVANCE_OUT" "still has 2 open issue(s)"
assert_eq "active work creates no ceremony" 0 "$(creation_count)"

# An empty milestone reads open_issues == 0 exactly like a completed one. Advancing
# on it would manufacture a ceremony for an iteration that never committed work —
# the repo's first milestone, or an Iteration N+1 whose assignment step failed.
reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0 0)
run_advance
assert_eq "an empty milestone is a clean no-op" 0 "$ADVANCE_STATUS"
assert_contains "an empty milestone reads as never started" "$ADVANCE_OUT" "no issues assigned"
assert_eq "an empty milestone creates no ceremony" 0 "$(creation_count)"

reset_case
MILESTONES_JSON='[{"number":5,"title":"Iteration 5","open_issues":0}]'
run_advance
assert_eq "an unreadable closed count fails closed" 1 "$ADVANCE_STATUS"
assert_contains "the unreadable closed count is named" "$ADVANCE_OUT" "Unreadable closed issue count"
assert_eq "an unreadable closed count creates nothing" 0 "$(creation_count)"

# The "exactly one ceremony" guarantee depends on the lookup being exhaustive.
# A page that fills the limit may be truncated, and a truncated page reads as
# "no ceremony exists" — which duplicates rather than fails closed.
reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(many_ceremonies_json 3)
CEREMONY_LOOKUP_LIMIT=3 run_advance
assert_eq "a full ceremony page fails closed" 1 "$ADVANCE_STATUS"
assert_contains "truncation is named as the risk" "$ADVANCE_OUT" "may be truncated"
assert_eq "a possibly truncated lookup creates nothing" 0 "$(creation_count)"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(many_ceremonies_json 2)
CEREMONY_LOOKUP_LIMIT=3 run_advance
assert_eq "a partial ceremony page still advances" 0 "$ADVANCE_STATUS"
assert_eq "a partial page creates the refinement ceremony" 1 "$(creation_count)"

echo
echo "iteration lifecycle — several iterations in flight"

# Planning ahead is supported: a coherent batch is committed to a future milestone
# while the current one is still being built. The active iteration is the open
# milestone with the lowest sequence; everything above it is nobody's work yet.
# Counting milestones was never a way to identify the active one — the sanctioned
# planning ceremony opens Iteration N+1 before it closes Iteration N.
reset_case
MILESTONES_JSON=$(milestones_json \
  "$(milestone_entry 12 "Iteration 9" 13)" \
  "$(milestone_entry 11 "Iteration 8" 4)" \
  "$(milestone_entry 13 "Iteration 10" 7)")
run_advance
assert_eq "several open milestones are not an error" 0 "$ADVANCE_STATUS"
assert_contains "the lowest sequence is the active iteration" "$ADVANCE_OUT" "Iteration 8 still has 4 open issue(s)"
assert_contains "the rest are reported as planned ahead" "$ADVANCE_OUT" "planned ahead"
assert_eq "an incomplete active iteration creates no ceremony" 0 "$(creation_count)"
assert_file_not_contains "the milestone count is no longer an error" "$SCRIPT" "Expected exactly one active iteration milestone"

# The ceremony belongs to the resolved milestone, not to whichever one the API
# listed first: Iteration 9 has 13 open issues here, so a run that evaluated it
# would have created nothing at all.
reset_case
MILESTONES_JSON=$(milestones_json \
  "$(milestone_entry 12 "Iteration 9" 13)" \
  "$(milestone_entry 11 "Iteration 8" 0)" \
  "$(milestone_entry 13 "Iteration 10" 7)")
run_advance
assert_eq "a completed lowest milestone still advances" 0 "$ADVANCE_STATUS"
assert_eq "advancing creates exactly one ceremony" 1 "$(creation_count)"
assert_file_contains "the ceremony names the active iteration" "$TITLE_LOG" "[Ceremony] Iteration 8 Backlog Refinement"
assert_file_contains "the ceremony carries the active milestone marker" "$BODY_LOG" "$(iteration_marker backlog-refine 11)"
assert_file_not_contains "no planned-ahead milestone is evaluated" "$BODY_LOG" "$(iteration_marker backlog-refine 12)"

# The case a string sort gets wrong: "Iteration 10" sorts before "Iteration 9".
# Iteration 10 is the complete one here, so a lexicographic winner would have
# opened refinement for an iteration nobody has started.
reset_case
MILESTONES_JSON=$(milestones_json \
  "$(milestone_entry 20 "Iteration 10" 0)" \
  "$(milestone_entry 21 "Iteration 9" 5)")
run_advance
assert_eq "the selection is numeric, not lexicographic" 0 "$ADVANCE_STATUS"
assert_contains "Iteration 9 outranks Iteration 10" "$ADVANCE_OUT" "Iteration 9 still has 5 open issue(s)"
assert_eq "the higher sequence is not advanced" 0 "$(creation_count)"

# The one ambiguity left: "lowest" has two answers, so it fails closed and names
# the milestones instead of advancing one of them at random.
reset_case
MILESTONES_JSON=$(milestones_json \
  "$(milestone_entry 11 "Iteration 8" 0)" \
  "$(milestone_entry 12 "Iteration 8 carry-over" 3)")
run_advance
assert_eq "equal sequences still fail closed" 1 "$ADVANCE_STATUS"
assert_contains "the first tied milestone is named" "$ADVANCE_OUT" "#11"
assert_contains "the second tied milestone is named" "$ADVANCE_OUT" "#12"
assert_contains "the shared sequence is named" "$ADVANCE_OUT" "sequence 8"
assert_eq "an ambiguous selection creates nothing" 0 "$(creation_count)"

# A collision ABOVE the active iteration leaves "lowest" a single answer, so it is
# not that ambiguity. Blocking there would stop the machine on a state it can read
# perfectly well — the mistake this rule replaces — and it fails closed later
# anyway, once the collision is itself the lowest.
reset_case
MILESTONES_JSON=$(milestones_json \
  "$(milestone_entry 11 "Iteration 8" 4)" \
  "$(milestone_entry 12 "Iteration 9" 2)" \
  "$(milestone_entry 13 "Iteration 9" 1)")
run_advance
assert_eq "a collision above the active iteration does not block" 0 "$ADVANCE_STATUS"
assert_contains "the unambiguous active iteration is still selected" "$ADVANCE_OUT" "Iteration 8 still has 4 open issue(s)"

# A title carrying no sequence falls back to the milestone number — the parser this
# selection reuses — and the fallback orders like any other sequence, so a
# low-numbered non-iteration milestone does become the active one. The selection
# line in the run log is where that shows up.
reset_case
MILESTONES_JSON=$(milestones_json \
  "$(milestone_entry 12 "Iteration 9" 6)" \
  "$(milestone_entry 4 "Hardening" 2)")
run_advance
assert_eq "an unparsed title falls back to its milestone number" 0 "$ADVANCE_STATUS"
assert_contains "the fallback sequence orders with the rest" "$ADVANCE_OUT" "Active iteration is #4"

echo
echo "iteration lifecycle — refinement creation and idempotency"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Sprint 5" 0)
run_advance
assert_eq "legacy-named completed milestone advances" 0 "$ADVANCE_STATUS"
assert_eq "completion creates one ceremony" 1 "$(creation_count)"
assert_file_contains "legacy name becomes Iteration in title" "$TITLE_LOG" "[Ceremony] Iteration 5 Backlog Refinement"
assert_file_contains "refinement body carries milestone marker" "$BODY_LOG" "$(iteration_marker backlog-refine 5)"
assert_file_contains "refinement body carries exact command" "$BODY_LOG" '/scrum-master backlog-refine'
assert_file_not_contains "refinement body has no weekly language" "$BODY_LOG" "Friday"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(ceremony_json 10 OPEN "$(iteration_marker backlog-refine 5)")
run_advance
assert_eq "open refinement is a clean wait" 0 "$ADVANCE_STATUS"
assert_contains "open refinement keeps planning locked" "$ADVANCE_OUT" "planning remains locked"
assert_eq "open refinement creates no duplicate" 0 "$(creation_count)"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(two_ceremonies_json "$(iteration_marker backlog-refine 5)")
run_advance
assert_eq "duplicate refinement markers fail closed" 1 "$ADVANCE_STATUS"
assert_contains "duplicate refinement error is explicit" "$ADVANCE_OUT" "Found 2 backlog-refinement ceremonies"

echo
echo "iteration lifecycle — planning unlock and idempotency"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(ceremony_json 10 CLOSED "$(iteration_marker backlog-refine 5)")
run_advance
assert_eq "closed refinement unlocks planning" 0 "$ADVANCE_STATUS"
assert_eq "planning unlock creates one ceremony" 1 "$(creation_count)"
assert_file_contains "next iteration number appears in title" "$TITLE_LOG" "[Ceremony] Iteration 6 Planning"
assert_file_contains "planning body carries milestone marker" "$BODY_LOG" "$(iteration_marker iteration-plan 5)"
assert_file_contains "planning body carries exact command" "$BODY_LOG" '/scrum-master iteration-plan'
assert_file_contains "planning explicitly omits a due date" "$BODY_LOG" "without a due date"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(refinement_and_plan_json CLOSED OPEN 5)
run_advance
assert_eq "open planning ceremony is a clean wait" 0 "$ADVANCE_STATUS"
assert_contains "open planning ceremony is recognized" "$ADVANCE_OUT" "already open"
assert_eq "open planning ceremony is not duplicated" 0 "$(creation_count)"

reset_case
MILESTONES_JSON=$(milestone_json 5 "Iteration 5" 0)
CEREMONIES_JSON=$(refinement_and_plan_json CLOSED CLOSED 5)
run_advance
assert_eq "closed planning with active old milestone fails" 1 "$ADVANCE_STATUS"
assert_contains "incomplete transition requests repair" "$ADVANCE_OUT" "Repair the milestone manually"

echo
echo "iteration lifecycle workflow — event-driven only"

assert_file_contains "workflow listens for issue closure" "$WORKFLOW" "types: [closed]"
assert_file_contains "workflow keeps manual recovery" "$WORKFLOW" "workflow_dispatch:"
assert_file_contains "workflow serializes transitions" "$WORKFLOW" 'group: iteration-lifecycle-${{ github.repository }}'
assert_file_contains "workflow calls state machine" "$WORKFLOW" "gh-automation.sh iteration-advance"
assert_file_not_contains "workflow has no schedule" "$WORKFLOW" "schedule:"
assert_file_not_contains "workflow has no cron" "$WORKFLOW" "cron:"

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
