#!/usr/bin/env bash
# =============================================================================
# gh-automation.sh — Mixed REST + GraphQL wrapper for GitHub pipeline ops
#
# Ported from the clinic-deck pipeline; clinic-deck PR numbers in comments refer
# to the incidents there that shaped these rules.
#
# Acceptance rule for READY TO MERGE:
#   Add label only when: CI is green AND the stable ci-gate check actually ran
#   successfully on the head commit, the PR is mergeable, unresolved review thread
#   count is zero, no DeepSeek review is holding unread findings in its body, and
#   DeepSeek review is definitively finished (approved, all rounds exhausted, or
#   exempt via NO_DEEPSEEK_REVIEW label).
#
# Commands:
#   pr-status <pr-number>         Comprehensive PR status (CI + threads)
#   pr-status-json <pr-number>    Same as pr-status but JSON output (for CI consumption)
#   pr-label <pr-number> <add|remove> <label>  Add or remove a label. Adding is
#                                 idempotent; a write that did not land exits
#                                 non-zero rather than printing success.
#   pr-deepseek-rounds <pr-number>  DeepSeek review round status (JSON). On failure
#                                 it prints {"error":...} and exits non-zero rather
#                                 than a zeroed success.
#   pr-deepseek-force-review <pr-number> [ref]
#                                 Dispatch a DeepSeek full review that bypasses the
#                                 MAX_ROUNDS cap.
#   pr-check-label <pr-number> <label>         Exit 0 present, 1 absent, 2 could
#                                 not determine — an unreadable lookup is not an
#                                 absent label.
#   is-ready-to-merge <pr-number>              Exit 0 if frozen rule met, 1 otherwise
#   iteration-advance             Advance the completion-driven iteration ceremony state
# =============================================================================

set -euo pipefail

# GitHub Actions supplies the canonical owner/name for the repository that fired
# the workflow. Local callers resolve it lazily through `gh repo view` instead of
# baking a name into this script: REST redirects some renamed-repository routes,
# while GraphQL and endpoints such as milestones do not. A stale literal therefore
# split this helper in two — some commands followed the checkout's current remote,
# while review status and iteration advancement queried the old repository.
REPO="${REPO:-}"

# The one stable check that must be PRESENT *and SUCCESSFUL* on the PR head.
# `ci-gate` owns the branch-aware policy: develop accepts only classifier-authorised
# skips, while main requires everything that exists at the ref. Keeping one public
# gate avoids mirroring every internal job name in this helper and in branch
# protection.
#
# Deliberately excludes `labeler` and `review`. Those are pipeline machinery from
# separate workflows, and `review` in particular is legitimately absent sometimes:
# DeepSeek skips a run once MAX_ROUNDS is spent, its concurrency group cancels
# superseded runs, and its job-level `if:` skips replies the bot itself authored.
# Requiring it would make READY TO MERGE unreachable exactly when those guards
# work as designed.
REQUIRED_CHECK="${REQUIRED_CHECK:-ci-gate}"

# How many ceremony-labelled issues `iteration-advance` reads when looking for the
# milestone-specific ceremonies. The "exactly one ceremony" guarantee is only as
# good as this lookup, and it fails in the dangerous direction: a truncated list
# reads as "no ceremony exists", which creates a DUPLICATE rather than failing
# closed. So a result that fills the limit is treated as truncated and refused.
# Each iteration spends exactly two ceremonies, and the active milestone's are
# always among the newest, so 500 is ~250 iterations of headroom.
CEREMONY_LOOKUP_LIMIT="${CEREMONY_LOOKUP_LIMIT:-500}"

# ── Helpers ──────────────────────────────────────────────────────────────────

die() { echo "ERROR: $*" >&2; exit 1; }

require_gh() {
  command -v gh &>/dev/null || die "gh CLI not found. Install: https://cli.github.com"
  gh auth status &>/dev/null || die "gh not authenticated. Run: gh auth login"
}

# Resolve the repository once for commands whose API shape needs an explicit
# owner/name. Prefer Actions' event repository, then a caller's local REPO
# override, then the current checkout as understood by gh. The last path is deliberately
# canonical rather than a parser for `git remote get-url`: gh follows a repository
# rename and reports its current name, whereas a stale local remote may not.
resolve_repo() {
  local candidate="${GITHUB_REPOSITORY:-${REPO:-}}"

  if [ -z "$candidate" ]; then
    candidate=$(gh repo view --json nameWithOwner --jq '.nameWithOwner') || {
      echo "ERROR: could not determine the current GitHub repository" >&2
      return 1
    }
  fi

  if [[ ! "$candidate" =~ ^[^/]+/[^/]+$ ]]; then
    echo "ERROR: GitHub repository must have the form owner/name" >&2
    return 1
  fi

  REPO="$candidate"
}

# Run `gh` under GH_CI_TOKEN when the caller supplied one, otherwise under whatever
# credential `gh` already has.
#
# Exists for exactly one caller: the `statusCheckRollup` read below. That projection
# includes CheckRun rows, and **a fine-grained PAT can never read those** — the Checks
# API is GitHub-App-only, so fine-grained tokens have no `Checks` permission to grant
# (they offer `Commit statuses`, which covers only the StatusContext half). No amount of
# reconfiguring `GH_PIPELINE_TOKEN` fixes it. `pr-labeler.yml` therefore hands this one
# call the workflow's own `GITHUB_TOKEN`, an App installation token whose `checks: read`
# comes from the job's `permissions:` block.
#
# Unset locally, so an interactive run is byte-for-byte what it was before: an empty
# GH_TOKEN is not the same as an unset one to `gh`, hence the branch rather than a
# `${GH_CI_TOKEN:-$GH_TOKEN}` default.
gh_ci() {
  if [ -n "${GH_CI_TOKEN:-}" ]; then
    GH_TOKEN="$GH_CI_TOKEN" gh "$@"
  else
    gh "$@"
  fi
}

# GraphQL query for review state, unresolved threads, and the acknowledgement
# label's own history.
#
# `last: 100` on reviews, not `first: 20`. Most counts derived from this payload are
# about the CURRENT state of the review conversation, and `first` returns the OLDEST
# page: on a PR busy enough to overflow it — Mode B thread replies each add an
# implicit COMMENTED review, so clinic-deck PR #260 held 6 for 2 real reviews — the
# newest review is exactly the one that falls off.
#
# The unread-findings count is the one consumer that reads the other way, and it is
# worth being explicit about which end of the window it can lose: an unacknowledged
# review keeps blocking until someone reads it, so the review that matters there is an
# OLD one, and truncation would silently stop it blocking — the clinic-deck #466 shape
# reappearing at the pagination boundary. 100 is the cap GitHub allows on a connection,
# so this is the widest that window goes without paginating, and paginating would put a
# loop on a path the labeler walks for every open PR on every CI completion. It is
# therefore a deliberate bound rather than a guarantee: ~100 reviews on one PR, with
# reply wrappers inflating the count, is where it gives out.
#
# `labels` and `timelineItems` are read here rather than through a second API call so
# the acknowledgement costs nothing extra. LABELED_EVENT carries the timestamp that
# ties an acknowledgement to the reviews it was applied after; 100 is the cap GitHub
# allows on a connection, and truncation drops the OLDEST events, so an ack that goes
# missing was outlived by 100 later label additions. It then reads as unacknowledged
# (fail closed) and re-applying the label produces a fresh event.
graphql_pr_review() {
  local pr="$1"
  resolve_repo || return 1
  gh api graphql -f query='
    query($owner: String!, $repo: String!, $pr: Int!) {
      repository(owner: $owner, name: $repo) {
        pullRequest(number: $pr) {
          reviews(last: 100, states: [APPROVED, CHANGES_REQUESTED, COMMENTED]) {
            nodes {
              author { login }
              state
              submittedAt
              body
            }
          }
          reviewThreads(first: 50) {
            totalCount
            nodes {
              isResolved
              isOutdated
              isCollapsed
            }
          }
          labels(first: 100) {
            nodes { name }
          }
          timelineItems(last: 100, itemTypes: [LABELED_EVENT]) {
            nodes {
              ... on LabeledEvent {
                createdAt
                label { name }
              }
            }
          }
        }
      }
    }
  ' -f owner="${REPO%%/*}" -f repo="${REPO##*/}" -F pr="$pr"
}

# ── pr-status — Human-readable PR status ─────────────────────────────────────

cmd_pr_status() {
  local pr="$1"
  require_gh

  echo "=== PR #${pr} Status ==="
  echo ""

  # CI status via REST
  echo "── CI Checks ──"
  gh pr checks "$pr" 2>/dev/null || echo "  (no checks found)"
  echo ""

  # Review state via GraphQL. The guarded assignment matters: under set -e an
  # unguarded failure here killed the whole command before the fallback branch.
  echo "── Reviews ──"
  local graphql_out
  graphql_out=$(graphql_pr_review "$pr" 2>/dev/null) || graphql_out=""
  if [ -z "$graphql_out" ]; then
    echo "  (unable to fetch review data)"
  else
    local unresolved total changes_requested
    total=$(echo "$graphql_out" | jq '.data.repository.pullRequest.reviewThreads.totalCount' 2>/dev/null) || total="?"
    unresolved=$(echo "$graphql_out" | jq '
      [.data.repository.pullRequest.reviewThreads.nodes[]
      | select(.isResolved == false and .isOutdated == false)] | length' 2>/dev/null) || unresolved="?"
    changes_requested=$(echo "$graphql_out" | jq '
      [.data.repository.pullRequest.reviews.nodes[]
      | select(.state == "CHANGES_REQUESTED")] | length' 2>/dev/null) || changes_requested="?"
    echo "  Review threads: ${unresolved:-?} unresolved / ${total:-?} total"
    echo "  Reviews requesting changes: ${changes_requested:-?}"
    # The line that would have made clinic-deck #464 obvious to anyone reading this
    # output: thread counts say nothing about findings DeepSeek wrote into a review
    # body.
    local body_findings
    body_findings=$(deepseek_unread_findings_from_graphql \
      "$graphql_out" "${DEEPSEEK_BOT_USER:-github-actions[bot]}" "$DEEPSEEK_REVIEW_READ_LABEL" 2>/dev/null) \
      || body_findings="?"
    echo "  DeepSeek reviews with unread body findings: ${body_findings:-?}"
  fi

  # Frozen rule evaluation — DELEGATED, never re-derived here.
  #
  # The verdict must come from cmd_pr_status_json, the same function the labeler
  # consumes, because the frozen rule needs exactly one implementation. When this
  # block evaluated the rule itself the two drifted, and this command was the one
  # that failed open (clinic-deck #279):
  #
  #   * CI was gated on `gh pr checks --required`, which filters to the contexts
  #     named in branch protection, so a red *non-required* check was invisible
  #     here. This command printed [PASS] on a PR the labeler was marking
  #     needs-work.
  #   * `--required` also exits 0 when the required set is empty, so a repo with
  #     no branch protection configured read green unconditionally.
  #   * The bot's round state was not consulted at all, so a PR with a review
  #     still outstanding could print [PASS].
  #
  # Individual fields below only *explain* the verdict; they never decide it.
  # `.ready_to_merge` also folds in the NO_DEEPSEEK_REVIEW/bot-branch exemption,
  # which is not exposed as a field and so cannot be recomputed from the outside.
  echo ""
  echo "── READY TO MERGE? ──"
  local status_json
  if ! status_json=$(cmd_pr_status_json "$pr" 2>/dev/null) || [ -z "$status_json" ]; then
    echo "  [FAIL] Could not evaluate readiness — status lookup failed"
    return 0
  fi

  local verdict
  verdict=$(echo "$status_json" | jq -r '.ready_to_merge' 2>/dev/null) || verdict="false"
  if [ "$verdict" = "true" ]; then
    echo "  [PASS] All conditions met — safe to add READY TO MERGE"
    return 0
  fi

  # Not ready: report every failing condition. A -1 is the helper's fail-closed
  # sentinel for a count it could not read, not a real tally.
  local explained=0 count
  local -a checks=(
    "unresolved_threads:unresolved review threads (must be 0)"
    "changes_requested:reviews requesting changes"
    "ci_failing:CI checks failing"
    "ci_pending:CI checks pending"
  )
  local entry field label
  for entry in "${checks[@]}"; do
    field="${entry%%:*}"
    label="${entry#*:}"
    count=$(echo "$status_json" | jq -r --arg f "$field" '.[$f]' 2>/dev/null) || count="?"
    if [ "$count" = "-1" ]; then
      echo "  [FAIL] ${label} — count unreadable, failing closed"
      explained=1
    elif [ "$count" != "0" ]; then
      echo "  [FAIL] ${count} ${label}"
      explained=1
    fi
  done

  # Unread DeepSeek findings sit outside the loop because the count alone is not
  # actionable: the reader needs to know where to look and how to clear it. A
  # condition whose remedy is undocumented is how a gate becomes a thing people
  # route around.
  local unread_findings
  unread_findings=$(echo "$status_json" | jq -r '.deepseek_unread_findings // "0"' 2>/dev/null) || unread_findings="0"
  if [ "$unread_findings" = "-1" ]; then
    echo "  [FAIL] DeepSeek body findings — count unreadable, failing closed"
    explained=1
  elif [ "$unread_findings" != "0" ]; then
    echo "  [FAIL] ${unread_findings} DeepSeek review(s) with unread findings in the review body"
    echo "         These create no review thread, so the thread count above cannot see them."
    echo "         Read them on the PR, then: gh pr edit ${pr} --add-label ${DEEPSEEK_REVIEW_READ_LABEL}"
    explained=1
  fi

  # Missing checks get their own line rather than a slot in the loop above: the
  # useful part is *which* ones are absent, and a bare count would not say.
  #
  # A field the payload does not carry at all defaults to the quiet value rather
  # than to a failure: this block only ever *explains* a verdict that has already
  # been decided upstream, so inventing a reason here would be a second, divergent
  # implementation of the rule — the exact drift this file exists to prevent.
  local missing_count missing_names
  missing_count=$(echo "$status_json" | jq -r '.checks_missing // "0"' 2>/dev/null) || missing_count="0"
  missing_names=$(echo "$status_json" | jq -r '.checks_missing_names // ""' 2>/dev/null) || missing_names=""
  if [ "$missing_count" = "-1" ]; then
    echo "  [FAIL] required CI checks — presence unreadable, failing closed"
    explained=1
  elif [ "$missing_count" != "0" ]; then
    echo "  [FAIL] required CI checks missing: ${missing_names//,/, } — CI did not run"
    explained=1
  fi

  # Presence alone is insufficient for the aggregate gate: a job-level SKIP is
  # a valid terminal CheckRun, but ci-gate is deliberately never optional.
  local required_state
  required_state=$(echo "$status_json" | jq -r '.required_check_state // ""' 2>/dev/null) || required_state=""
  case "$required_state" in
    "" | SUCCESS | MISSING) ;;
    SKIPPED)
      echo "  [FAIL] required CI gate was skipped — branch policy was not evaluated"
      explained=1
      ;;
    PENDING)
      echo "  [FAIL] required CI gate is still pending"
      explained=1
      ;;
    UNREADABLE)
      echo "  [FAIL] required CI gate state unreadable — failing closed"
      explained=1
      ;;
    *)
      echo "  [FAIL] required CI gate concluded ${required_state}"
      explained=1
      ;;
  esac

  # Mergeability is a state, not a count, so it also sits outside the loop.
  local mergeable_state
  mergeable_state=$(echo "$status_json" | jq -r '.mergeable // ""' 2>/dev/null) || mergeable_state=""
  case "$mergeable_state" in
    "" | MERGEABLE) ;;
    CONFLICTING)
      echo "  [FAIL] PR has merge conflicts — no pull_request workflow can run until they are resolved"
      explained=1
      ;;
    UNKNOWN)
      echo "  [FAIL] mergeability still being computed by GitHub — failing closed, re-checked next poll"
      explained=1
      ;;
    *)
      echo "  [FAIL] mergeability unreadable (${mergeable_state}) — failing closed"
      explained=1
      ;;
  esac

  # Every count clean but still not ready means DeepSeek has not finished: not
  # approved, rounds not exhausted, and not exempt.
  if [ "$explained" -eq 0 ]; then
    echo "  [FAIL] DeepSeek review not finished — not approved, rounds not exhausted, not exempt"
  fi
}

# ── resolve_mergeable — read GitHub's mergeability, waiting out UNKNOWN ──────
#
# Prints exactly one of MERGEABLE | CONFLICTING | UNKNOWN | UNREADABLE on stdout.
#
# UNKNOWN is retried, and it is the ONLY value that is. GitHub computes mergeability in a background
# job, so UNKNOWN means "not yet", where CONFLICTING and an unreadable value both mean "no" and are
# returned immediately.
#
# Scope, stated honestly: the labeler's own startup usually outlasts GitHub's computation, so the
# path this actually helps is an *immediate* read — `/process-pr` and a human at a terminal both
# call `pr-status` seconds after a push, which is inside the window.
#
# Retrying costs nothing when the answer is already known: the loop returns on the first response
# that is not UNKNOWN. `PR_MERGEABLE_RETRIES=0` disables waiting entirely, which is what the tests
# use. Do NOT raise this on a caller that loops over many PRs — the budget is paid per PR.
#
# Plain `gh`, not gh_ci: `mergeable` is not a checks field, so it needs no App token, and gh_ci
# exists for exactly one caller by design.
resolve_mergeable() {
  local pr="$1"
  local retries="${PR_MERGEABLE_RETRIES:-4}"
  local delay="${PR_MERGEABLE_RETRY_DELAY:-5}"
  local attempt=0
  local value

  # Both budgets are validated rather than trusted. A non-numeric delay makes `sleep` fail, and
  # whether that aborts the function depends on a `set -e` exemption for commands inside a loop
  # body — "fails closed because of a subtlety in how set -e treats loop bodies" is not a property
  # this helper should rest on. Two lines make it moot.
  case "$retries" in '' | *[!0-9]*) retries=4 ;; esac
  case "$delay" in '' | *[!0-9]*) delay=5 ;; esac

  while :; do
    value=$(gh pr view "$pr" --json mergeable --jq '.mergeable' 2>/dev/null) || value=""
    case "$value" in
      MERGEABLE | CONFLICTING | UNKNOWN) ;;
      *)
        echo "[WARN] Could not determine mergeable for PR #${pr} — failing closed" >&2
        value="UNREADABLE"
        ;;
    esac

    [ "$value" = "UNKNOWN" ] || break
    [ "$attempt" -lt "$retries" ] || break
    attempt=$((attempt + 1))
    echo "[INFO] mergeable=UNKNOWN on PR #${pr} — GitHub still computing; retry ${attempt}/${retries} in ${delay}s" >&2
    sleep "$delay"
  done

  printf '%s\n' "$value"
}

# ── pr-status-json — Machine-readable JSON status ────────────────────────────

cmd_pr_status_json() {
  local pr="$1"
  require_gh

  local graphql_out unresolved ci_failing ci_pending changes_requested
  local checks_missing checks_missing_names required_check_state mergeable
  graphql_out=$(graphql_pr_review "$pr" 2>/dev/null) || graphql_out=""

  unresolved=$(echo "$graphql_out" | jq '
    [.data.repository.pullRequest.reviewThreads.nodes[]
    | select(.isResolved == false and .isOutdated == false)] | length' 2>/dev/null) || unresolved=""

  changes_requested=$(echo "$graphql_out" | jq '
    [.data.repository.pullRequest.reviews.nodes[]
    | select(.state == "CHANGES_REQUESTED")] | length' 2>/dev/null) || changes_requested=""

  # DeepSeek findings delivered as review-body prose, which creates no thread and so
  # is invisible to `unresolved` above. Clinic-deck PR #464 merged with three real
  # ones unread while every gate printed green. Same payload, no extra API call.
  local deepseek_unread_findings
  deepseek_unread_findings=$(deepseek_unread_findings_from_graphql \
    "$graphql_out" "${DEEPSEEK_BOT_USER:-github-actions[bot]}" "$DEEPSEEK_REVIEW_READ_LABEL" 2>/dev/null) \
    || deepseek_unread_findings=""

  # CI via the statusCheckRollup projection — `gh pr checks --json` does not exist on
  # every gh in the field. The rollup mixes two row shapes: CheckRun rows carry
  # status/conclusion, StatusContext rows carry state; cover both.
  #
  # Fetched ONCE and counted locally. This used to be two `gh pr view` calls, which
  # doubled the API cost and — worse — discarded gh's stderr into /dev/null, so a
  # rollup that could not be read produced a bare "[WARN] Could not determine
  # ci_failing" with no way to tell a permissions failure from a transient 502.
  # `statusCheckRollup` needs a credential that can read check runs, which no
  # fine-grained PAT can be given (see `gh_ci` above) — hence the separate token.
  # Without it every count pins to -1 and READY TO MERGE is unreachable repo-wide;
  # surfacing the reason is what makes that distinguishable from the outside.
  local rollup rollup_err
  rollup_err=$(mktemp)
  if rollup=$(gh_ci pr view "$pr" --json statusCheckRollup 2>"$rollup_err"); then
    ci_failing=$(echo "$rollup" | jq '
      [.statusCheckRollup[] | (.conclusion // .state // "") as $c
       | select($c == "FAILURE" or $c == "TIMED_OUT" or $c == "STARTUP_FAILURE"
                or $c == "ACTION_REQUIRED" or $c == "CANCELLED" or $c == "ERROR")]
      | length' 2>/dev/null) || ci_failing=""

    ci_pending=$(echo "$rollup" | jq '
      [.statusCheckRollup[]
       | select(((.status // "COMPLETED") != "COMPLETED")
                or (.state == "PENDING") or (.state == "EXPECTED"))]
      | length' 2>/dev/null) || ci_pending=""

    # gh exited 0 but the projection was unusable — report what came back instead
    # of only that it was unreadable. A null rollup is the signature of a token
    # that cannot see checks.
    if [ -z "$ci_failing" ] || [ -z "$ci_pending" ]; then
      echo "[WARN] statusCheckRollup unusable for PR #${pr}: $(printf '%s' "$rollup" | head -c 200)" >&2
    fi

    # Successful presence, not just verdicts. Both counts above are 0 for a rollup
    # containing nothing at all — and that is a state the pipeline actually reaches.
    # A PR with merge conflicts has no computable refs/pull/<n>/merge, so every
    # workflow that would check that ref out is never created (not queued, not
    # failed — absent), leaving only push-driven external contexts. Two green
    # external rows then satisfied "nothing failing, nothing pending" and this
    # helper called a PR whose test suite never ran safe to merge (clinic-deck
    # #315/#317). A check that does not exist is not a passing check — the same
    # principle as the fail-closed block below.
    if required_check_state=$(echo "$rollup" | jq -r --arg required "$REQUIRED_CHECK" '
        [.statusCheckRollup[]?
         | select((.name // .context // "") == $required)
         | (.conclusion // .state // "") as $conclusion
         | if ((.status // "COMPLETED") != "COMPLETED")
              or $conclusion == "PENDING" or $conclusion == "EXPECTED"
           then "PENDING"
           elif $conclusion == "" then "UNREADABLE"
           else ($conclusion | ascii_upcase)
           end] as $states
        | if ($states | length) == 0 then "MISSING"
          elif any($states[]; . == "FAILURE" or . == "TIMED_OUT"
               or . == "STARTUP_FAILURE" or . == "ACTION_REQUIRED"
               or . == "CANCELLED" or . == "ERROR")
            then ($states | map(select(. == "FAILURE" or . == "TIMED_OUT"
                 or . == "STARTUP_FAILURE" or . == "ACTION_REQUIRED"
                 or . == "CANCELLED" or . == "ERROR"))[0])
          elif any($states[]; . == "PENDING") then "PENDING"
          elif all($states[]; . == "SUCCESS") then "SUCCESS"
          elif any($states[]; . == "SKIPPED") then "SKIPPED"
          else "UNREADABLE"
          end' 2>/dev/null); then
      if [ "$required_check_state" = "MISSING" ]; then
        checks_missing=1
        checks_missing_names="$REQUIRED_CHECK"
      else
        checks_missing=0
        checks_missing_names=""
      fi
    else
      checks_missing=""
      checks_missing_names=""
      required_check_state=""
      echo "[WARN] could not read ${REQUIRED_CHECK} state from rollup for PR #${pr}" >&2
    fi
  else
    ci_failing=""
    ci_pending=""
    checks_missing=""
    checks_missing_names=""
    required_check_state=""
    echo "[WARN] statusCheckRollup lookup failed for PR #${pr}: $(head -c 300 "$rollup_err")" >&2
  fi
  rm -f "$rollup_err"

  # Mergeability — the root cause of the empty rollup above, and independently
  # disqualifying: a conflicting PR cannot be merged and will not run a single
  # `pull_request` workflow until it is fixed. UNKNOWN means GitHub is still
  # computing the merge commit, which is normal for the first seconds after a push;
  # it fails closed like any other value we cannot act on, and the next poll re-reads.
  #
  # Plain `gh`, not gh_ci: `mergeable` is not a checks field, so it needs no App
  # token, and gh_ci exists for exactly one caller by design.
  # `|| mergeable="UNREADABLE"` is belt-and-braces — resolve_mergeable always prints
  # a value and returns 0 — but the entire contract of this function is to emit
  # fail-closed JSON rather than die, and that should not depend on a helper never
  # regressing.
  mergeable=$(resolve_mergeable "$pr") || mergeable="UNREADABLE"

  # Fail closed: a count we could not read is not a zero. -1 keeps the JSON valid,
  # can never satisfy the readiness equalities below, and routes the labeler into
  # its needs-review branch instead of silently passing — mirroring the DeepSeek
  # error contract. This replaces the old `|| echo "0"` fallbacks, which reported
  # every extraction failure as "nothing outstanding".
  local count_var
  for count_var in unresolved changes_requested ci_failing ci_pending checks_missing deepseek_unread_findings; do
    if [[ ! "${!count_var}" =~ ^[0-9]+$ ]]; then
      echo "[WARN] Could not determine ${count_var} for PR #${pr} — failing closed" >&2
      printf -v "$count_var" '%s' "-1"
    fi
  done

  case "$required_check_state" in
    SUCCESS | MISSING | SKIPPED | PENDING | FAILURE | TIMED_OUT | STARTUP_FAILURE | ACTION_REQUIRED | CANCELLED | ERROR) ;;
    *)
      echo "[WARN] Could not determine ${REQUIRED_CHECK} state for PR #${pr} — failing closed" >&2
      required_check_state="UNREADABLE"
      ;;
  esac

  # DeepSeek review round status — fails closed on error (no bypass)
  local ds_json ds_complete ds_exhausted ds_participated bot_review_count ds_satisfied
  if ! ds_json=$(cmd_pr_deepseek_rounds "$pr" 2>/dev/null) || [ -z "$ds_json" ]; then
    echo "[WARN] Failed to fetch DeepSeek status for PR $pr — treating as unsatisfied" >&2
    ds_complete="false"
    ds_exhausted="false"
    ds_participated="true"
    bot_review_count=0
  else
    ds_complete=$(echo "$ds_json" | jq -r '.review_complete // false' 2>/dev/null || echo "false")
    ds_exhausted=$(echo "$ds_json" | jq -r '.review_rounds_exhausted // false' 2>/dev/null || echo "false")
    bot_review_count=$(echo "$ds_json" | jq -r '.bot_review_count // 0' 2>/dev/null || echo "0")
    if [ -z "$bot_review_count" ] || [ "$bot_review_count" = "null" ]; then
      bot_review_count=0
    fi
    ds_participated="false"
    if [ "$ds_complete" = "true" ] || [ "$ds_exhausted" = "true" ]; then
      ds_participated="true"
    elif [[ "$bot_review_count" =~ ^[0-9]+$ ]] && [ "$bot_review_count" -gt 0 ]; then
      ds_participated="true"
    fi
  fi

  # Check if PR is exempt from DeepSeek review (label or branch pattern)
  #
  # `cmd_pr_check_label` answers 0 present / 1 absent / 2 could-not-determine, and
  # this `if` deliberately treats the last two alike: an unreadable lookup leaves the
  # PR unexempt, which is the strict direction and matches every other count here.
  # stderr is silenced at the call site because stdout below is JSON someone parses.
  local ds_exempt="false"
  if cmd_pr_check_label "$pr" "NO_DEEPSEEK_REVIEW" 2>/dev/null; then
    ds_exempt="true"
  else
    local branch
    branch=$(gh pr view "$pr" --json headRefName --jq '.headRefName' 2>/dev/null || echo "")
    case "$branch" in
      dependabot/*|bot/*|renovate/*) ds_exempt="true" ;;
    esac
  fi

  # Ready when DeepSeek has definitively finished: approved, all rounds exhausted, or exempt
  ds_satisfied="false"
  if [ "$ds_complete" = "true" ] || [ "$ds_exhausted" = "true" ] || [ "$ds_exempt" = "true" ]; then
    ds_satisfied="true"
  fi

  # `deepseek_unread_findings` is deliberately NOT waived by ds_exempt. The exemption
  # answers "should DeepSeek review this PR at all"; findings that already exist were
  # written either way, and nobody has read them. Labelling a PR NO_DEEPSEEK_REVIEW
  # after the review landed would otherwise retire the findings silently — the shape
  # of the defect, not a fix for it.
  local ready="false"
  [ "$unresolved" = "0" ] && [ "$changes_requested" = "0" ] && [ "$ci_failing" = "0" ] && [ "$ci_pending" = "0" ] && [ "$checks_missing" = "0" ] && [ "$required_check_state" = "SUCCESS" ] && [ "$deepseek_unread_findings" = "0" ] && [ "$mergeable" = "MERGEABLE" ] && [ "$ds_satisfied" = "true" ] && ready="true"

  printf '{"pr":%s,"unresolved_threads":%s,"changes_requested":%s,"ci_failing":%s,"ci_pending":%s,"checks_missing":%s,"checks_missing_names":"%s","required_check_state":"%s","mergeable":"%s","deepseek_review_complete":%s,"deepseek_rounds_exhausted":%s,"deepseek_has_participated":%s,"deepseek_unread_findings":%s,"ready_to_merge":%s}\n' \
    "$pr" "$unresolved" "$changes_requested" "$ci_failing" "$ci_pending" "$checks_missing" "$checks_missing_names" "$required_check_state" "$mergeable" "$ds_complete" "$ds_exhausted" "$ds_participated" "$deepseek_unread_findings" "$ready"
}

# ── pr-comments — List bot review comments ───────────────────────────────────

cmd_pr_comments() {
  local pr="$1"
  require_gh
  resolve_repo || die "Could not resolve the repository for PR comments"

  echo "=== Review Comments for PR #${pr} ==="
  gh api "repos/${REPO}/pulls/${pr}/comments" \
    --jq '.[] | "[" + .user.login + "] [" + .path + ":" + (.line // "?" | tostring) + "] " + .body' 2>/dev/null \
    || echo "  (no comments or API error)"
}

# ── pr-label — Label writes that report what actually happened ───────────────
#
# This helper writes every label the pipeline applies: `pr-labeler.yml` calls it
# nine times across the three verdict branches, `READY TO MERGE` among them.
#
# It used to say `2>/dev/null || true` and then print its success line
# unconditionally, so the one thing it could never report was a write that did not
# land — the reason was discarded, the exit status was discarded, and the word
# "(idempotent)" made the line read as "it was already there" when it meant
# "nobody checked". Found live: `pr-label 131 add ready-for-dev` printed success,
# exited 0, and applied nothing (#134).
#
# The reading half of this script has always failed closed — an unreadable count
# is -1 in `pr-status-json`, never 0. The writing half now does too: an operation
# that did not demonstrably happen exits non-zero and says why on stderr.
#
# **The exit status reaches the labeler step, and that is the point.** A `run:`
# block is `bash -e`, so a failing label write ends the step and the run goes red
# instead of logging a success nobody performed. It costs the rest of that pass —
# but every firing processes all open PRs and the six-hour sweep re-runs
# unconditionally, so the labels come back on the next one, and `labeler` is
# deliberately outside `REQUIRED_CHECK`, so a red run cannot make READY TO MERGE
# unreachable. A silent no-op costs the same labels and leaves nothing to notice.

cmd_pr_label() {
  local pr="$1" action="$2" label="$3"
  require_gh

  case "$action" in
    add)
      # No pre-check: GitHub's addLabels mutation accepts a label the PR already
      # carries, so `--add-label` is genuinely idempotent and re-adding is not an
      # error. What was missing is the exit status, not a guard. gh's stderr is
      # left unredirected so the reason lands in the log with the failure.
      if gh pr edit "$pr" --add-label "$label"; then
        echo "Label '${label}' added to PR #${pr}"
      else
        echo "ERROR: failed to add label '${label}' to PR #${pr} — see gh's output above" >&2
        return 1
      fi
      ;;
    remove)
      # `gh --remove-label` errors when the label is absent, so presence is read
      # first. That read has three answers, not two: "absent" is a fact worth
      # acting on, "could not determine" is not, and taking the second for the
      # first is how a failed lookup became a skipped removal with no line printed
      # at all.
      local present=0
      cmd_pr_check_label "$pr" "$label" || present=$?
      case "$present" in
        0)
          if gh pr edit "$pr" --remove-label "$label"; then
            echo "Label '${label}' removed from PR #${pr}"
          else
            echo "ERROR: failed to remove label '${label}' from PR #${pr} — see gh's output above" >&2
            return 1
          fi
          ;;
        1)
          echo "Label '${label}' not present on PR #${pr} — nothing to remove"
          ;;
        *)
          # The reason is already on stderr from the read itself; this line says
          # what was decided on the strength of it.
          echo "ERROR: refusing to remove label '${label}' from PR #${pr} — an unreadable label list is not the same as the label being absent" >&2
          return 1
          ;;
      esac
      ;;
    *)
      die "Unknown action: $action (use add or remove)"
      ;;
  esac
}

# ── pr-deepseek-rounds — DeepSeek review round status ────────────────────────

# Mode A stamps this into every full-review body. Counting COMMENTED reviews
# without it counts things that are not review rounds: GitHub wraps a standalone
# review-comment reply in an implicit COMMENTED review, and the "review paused"
# notice used to be posted as one too (clinic-deck #260 inflated 2 real reviews
# into 6 counted rounds). Keep in sync with FULL_REVIEW_MARKER in
# .github/scripts/deepseek_review.py.
DEEPSEEK_FULL_REVIEW_MARKER="<!-- deepseek:full-review -->"

# Stamped into the one review body that is prose and yet reports nothing: the clean
# approve ("no substantive issues found"). Everything else DeepSeek writes into a
# body is a finding. Keep in sync with NO_FINDINGS_MARKER in
# .github/scripts/deepseek_review.py — an approve stamped there and unrecognised
# here costs a pointless acknowledgement click, which is the safe direction to be
# wrong in.
DEEPSEEK_NO_FINDINGS_MARKER="<!-- deepseek:no-findings -->"

# The acknowledgement. A human adds it to say they have read what DeepSeek left in
# a review body; the frozen rule refuses READY TO MERGE until they have.
#
# It is a label, not a reaction or a magic comment, for two reasons: it is one click
# in the UI on a control humans already use here (NO_DEEPSEEK_REVIEW is the same
# shape), and — because it can always be applied — it can never make READY TO MERGE
# unreachable, which is the failure every condition on that list has to answer for.
DEEPSEEK_REVIEW_READ_LABEL="${DEEPSEEK_REVIEW_READ_LABEL:-DEEPSEEK_REVIEW_READ}"

# Emit the error shape consumed by callers that gate automation on this helper.
# Printed to stdout so a caller reading stdout can tell "misconfigured" apart
# from "no reviews yet"; the command exits non-zero alongside it.
deepseek_rounds_error() {
  local message="$1" max_rounds="$2"
  jq -cn --arg error "$message" --argjson max_rounds "$max_rounds" \
    '{error: $error, max_rounds: $max_rounds}'
}

# Reduce a GraphQL reviews payload to the round-status JSON.
# Pure: takes the payload, bot login and round cap as arguments so it can be
# exercised by scripts/test/gh-automation-deepseek-rounds.test.sh without network.
#
# Bot logins come in two spellings: REST reports "github-actions[bot]" while
# GraphQL reports "github-actions". Both sides are normalised by stripping a
# trailing "[bot]" so either form of DEEPSEEK_BOT_USER matches either payload.
deepseek_rounds_from_graphql() {
  local graphql_out="$1" bot_user="$2" max_rounds="$3"

  local counts
  counts=$(echo "$graphql_out" | jq -c --arg bot "$bot_user" \
    --arg marker "$DEEPSEEK_FULL_REVIEW_MARKER" \
    --arg none_marker "$DEEPSEEK_NO_FINDINGS_MARKER" '
    def canon: sub("\\[bot\\]$"; "");
    def is_full_review: .state == "COMMENTED" and ((.body // "") | contains($marker));
    # A clean verdict. GitHub forbids Actions from approving, so the script records
    # "nothing found" as a COMMENT whose body begins with the no-findings marker and
    # carries no full-review marker (#22). Both halves are required: a review with
    # findings always carries the full-review marker, so it can never be mistaken for
    # this shape even if its header order ever changed.
    def is_clean_verdict:
      ((.body // "") | startswith($none_marker)) and
      (((.body // "") | contains($marker)) | not);
    [.data.repository.pullRequest.reviews.nodes[]?
     | select((.author.login // "" | canon) == ($bot | canon))]
    | sort_by(.submittedAt)
    | {
        commented: [.[] | select(is_full_review)] | length,
        approved: [.[] | select(.state == "APPROVED")] | length,
        clean: [.[] | select(is_clean_verdict)] | length,
        latest_review_id: (
          [.[] | select(is_full_review or .state == "APPROVED" or is_clean_verdict)][-1].databaseId // 0
        ),
      }
  ') || return 1

  local bot_review_count approved_count clean_count latest_review_id
  bot_review_count=$(echo "$counts" | jq '.commented')
  approved_count=$(echo "$counts" | jq '.approved')
  clean_count=$(echo "$counts" | jq '.clean')
  # latest_review_id tracks the chronologically latest bot review of ANY state.
  # Used by the polling loop to detect new review arrival by ID comparison.
  # Terminal state (review_complete=true) is handled separately.
  latest_review_id=$(echo "$counts" | jq '.latest_review_id')

  local review_complete="false"
  local review_rounds_exhausted="false"

  # An APPROVE is still honoured — it is a terminal verdict and older PRs carry them —
  # but it can no longer be produced here, so a clean COMMENT verdict counts too.
  # Neither spends the round budget: only `commented` (full-review marker) does, so a
  # clean pass on an early commit leaves a later push reviewable.
  if [ "${approved_count}" -gt 0 ] || [ "${clean_count}" -gt 0 ]; then
    review_complete="true"
  fi

  if [ "${bot_review_count}" -ge "${max_rounds}" ]; then
    review_rounds_exhausted="true"
  fi

  printf '{"bot_review_count":%s,"max_rounds":%s,"review_complete":%s,"latest_review_id":%s,"review_rounds_exhausted":%s}\n' \
    "$bot_review_count" "$max_rounds" "$review_complete" "$latest_review_id" "$review_rounds_exhausted"
}

# Count the bot reviews whose body is holding findings nobody has acknowledged.
# Pure: takes the payload, bot login and ack label as arguments, so
# scripts/test/pr-status-frozen-rule.test.sh can exercise it without network.
#
# "Carries findings" is decided structurally — body minus markers minus whitespace is
# non-empty — rather than from a marker the model has to remember to emit. Three shapes
# have to come out right, and only the structural rule gets all three:
#
#   * an inline-only full review: body is the round marker alone, so it strips to
#     nothing. Its findings ARE threads; `unresolved` already counts them.
#   * a full review with general comments: "## General Comments …" survives the strip.
#     This is clinic-deck #464, which merged with three of them unread.
#   * an APPROVE carrying general comments — deepseek_review.py posts that shape
#     unstamped when the model sets review_complete=true and still returns comments.
#     This is clinic-deck #478, and a rule keyed on the round marker would miss it
#     entirely.
#
# GitHub's implicit COMMENTED wrapper around a Mode B thread reply has an empty body, so
# it strips to nothing and is not a finding — its content is a thread, and counted as one.
#
# The acknowledgement is dated, not sticky: only reviews submitted BEFORE the label was
# applied count as read. A forced second review therefore blocks again on its own, and
# pre-applying the label acknowledges nothing, which is the honest reading of a click
# that happened before the words existed.
deepseek_unread_findings_from_graphql() {
  local graphql_out="$1" bot_user="$2" ack_label="$3"

  # `.reviews.nodes[]` is deliberately un-`?`-ed: a payload without it is unreadable,
  # jq exits non-zero, and the caller's fail-closed sentinel takes over. The ack fields
  # are the opposite — absent means "never acknowledged", which is already the safe
  # answer — so they tolerate a payload that predates them.
  echo "$graphql_out" | jq \
    --arg bot "$bot_user" \
    --arg label "$ack_label" \
    --arg full_marker "$DEEPSEEK_FULL_REVIEW_MARKER" \
    --arg none_marker "$DEEPSEEK_NO_FINDINGS_MARKER" '
    def canon: sub("\\[bot\\]$"; "");
    # split/join, not gsub: the markers are stripped as literal text, so a future
    # marker containing a regex metacharacter cannot quietly change what is stripped.
    def strip($m): split($m) | join("");
    .data.repository.pullRequest as $pr
    | (if ([$pr.labels.nodes[]? | .name] | index($label)) == null then null
       else ([$pr.timelineItems.nodes[]? | select(.label.name == $label) | .createdAt] | max)
       end) as $acked_at
    | [ $pr.reviews.nodes[]
        | select((.author.login // "" | canon) == ($bot | canon))
        # The no-findings marker exempts a review only in the exact shape the script
        # posts it: a body that BEGINS with the marker and carries no full-review
        # marker. `contains` was the first attempt and it is a fail-open — the marker
        # is a string in this repository, DeepSeek reviews this repository, and a
        # general comment that merely quotes it would exempt however many real
        # findings sat beside it. Model prose cannot begin a body, because the
        # general-comments composer always puts its own header first.
        #
        # The state used to be half of this test (APPROVED only). GitHub forbids
        # Actions from approving, so the clean verdict is now a COMMENT (#22) and the
        # state can no longer discriminate. The full-review marker replaces it, and is
        # strictly stronger: a review carrying findings always has that marker, so it
        # cannot be exempted regardless of what its body starts with.
        | select(
            (((.body // "") | startswith($none_marker)) and
             (((.body // "") | contains($full_marker)) | not)) | not
          )
        | select(((.body // "") | strip($full_marker) | gsub("^\\s+|\\s+$"; "")) != "")
        | select($acked_at == null or ((.submittedAt // "9999") > $acked_at))
      ] | length
  '
}

cmd_pr_deepseek_rounds() {
  local pr="$1"

  local bot_user="${DEEPSEEK_BOT_USER:-github-actions[bot]}"
  local max_rounds="${MAX_ROUNDS:-1}"

  # Guard: validate max_rounds is numeric (prevents invalid JSON output)
  if [[ ! "$max_rounds" =~ ^[0-9]+$ ]]; then
    echo "WARNING: MAX_ROUNDS is not numeric, defaulting to 1" >&2
    max_rounds=1
  fi

  require_gh
  if ! resolve_repo; then
    deepseek_rounds_error "Could not determine the current GitHub repository" "$max_rounds"
    return 1
  fi

  local graphql_out exit_code
  graphql_out=$(gh api graphql -f query='
    query($owner: String!, $repo: String!, $pr: Int!) {
      repository(owner: $owner, name: $repo) {
        pullRequest(number: $pr) {
          reviews(first: 50, states: [APPROVED, COMMENTED]) {
            nodes {
              databaseId
              author { login }
              state
              submittedAt
              body
            }
          }
        }
      }
    }
  ' -f owner="${REPO%%/*}" -f repo="${REPO##*/}" -F pr="$pr" 2>&1)
  exit_code=$?

  if [ $exit_code -ne 0 ]; then
    echo "ERROR: DeepSeek rounds GraphQL query failed (exit=$exit_code)" >&2
    [ -n "$graphql_out" ] && echo "Stderr: ${graphql_out:0:500}" >&2
    deepseek_rounds_error "GraphQL query failed (exit=$exit_code)" "$max_rounds"
    return 1
  fi

  if ! echo "$graphql_out" | jq -e '.data' >/dev/null 2>&1; then
    echo "ERROR: DeepSeek rounds GraphQL response is not valid JSON or missing .data" >&2
    [ -n "$graphql_out" ] && echo "Response: ${graphql_out:0:500}" >&2
    deepseek_rounds_error "GraphQL response is not valid JSON or missing .data" "$max_rounds"
    return 1
  fi

  if ! deepseek_rounds_from_graphql "$graphql_out" "$bot_user" "$max_rounds"; then
    echo "ERROR: Could not derive DeepSeek round status from the GraphQL response" >&2
    deepseek_rounds_error "Could not derive round status from the GraphQL response" "$max_rounds"
    return 1
  fi
}

# ── pr-deepseek-force-review — Dispatch a review that ignores the round cap ──

# The automatic cap (MAX_ROUNDS=1) deliberately stops re-reviewing after the
# first pass; this is the way back in when a second opinion is wanted. The run
# executes the workflow definition from <ref>, so the ref must be a branch that
# carries the FORCE_REVIEW bypass — hence develop rather than the default branch.
cmd_pr_deepseek_force_review() {
  local pr="${1:-}" ref="${2:-develop}"
  [ -n "$pr" ] || die "usage: pr-deepseek-force-review <pr-number> [ref]"
  require_gh
  resolve_repo || die "Could not resolve the repository for review dispatch"

  gh workflow run deepseek-pr-review.yml \
    --repo "$REPO" \
    --ref "$ref" \
    -f pr_number="$pr" \
    -f event_name=pull_request \
    || die "Failed to dispatch deepseek-pr-review.yml on ref '$ref'"

  echo "Dispatched a forced DeepSeek review of PR #${pr} (workflow ref: ${ref})."
  echo "Watch it: gh run list --workflow=deepseek-pr-review.yml --limit 3"
}

# ── pr-check-label — 0 present, 1 absent, 2 could not determine ──────────────
#
# The third code is the whole point. Written as `gh pr view … 2>/dev/null | grep
# -qxF`, a failed lookup produces no output, grep exits non-zero, and the answer is
# indistinguishable from "the label is not there" — with the reason in /dev/null.
# The two call for opposite responses, and `cmd_pr_label remove` was taking the
# second for the first (#134).
#
# Callers that only branch on "present" keep working unchanged: 2 is non-zero, so
# an `if cmd_pr_check_label …` still falls to its else, which is the fail-closed
# direction everywhere this is used.
CHECK_LABEL_UNDETERMINED=2

cmd_pr_check_label() {
  local pr="$1" label="$2"
  local labels
  # Not `2>/dev/null`: the reason belongs in the caller's log. `pr-status-json`
  # silences it at its own call site, where stdout is JSON someone has to parse.
  if ! labels=$(gh pr view "$pr" --json labels --jq '.labels[].name'); then
    echo "ERROR: could not read the labels on PR #${pr}" >&2
    return "$CHECK_LABEL_UNDETERMINED"
  fi
  # A here-string rather than a pipe: under `pipefail`, `grep -q` exits on the first
  # match and can hand the writer a SIGPIPE, which would surface as a failed lookup.
  grep -qxF -e "$label" <<<"$labels"
}

# ── is-ready-to-merge — Exit 0 if frozen rule met ───────────────────────────

cmd_is_ready_to_merge() {
  local pr="$1"
  local json
  json=$(cmd_pr_status_json "$pr" 2>/dev/null)
  local ready
  ready=$(echo "$json" | python3 -c "import sys,json; print(str(json.load(sys.stdin)['ready_to_merge']).lower())" 2>/dev/null || echo "false")
  if [ "$ready" = "true" ]; then
    echo "PR #${pr} is READY TO MERGE"
    exit 0
  else
    echo "PR #${pr} is NOT ready to merge"
    echo "$json" | python3 -c "
import sys, json
d = json.load(sys.stdin)
reasons = []
if int(d['unresolved_threads']) > 0: reasons.append(str(d['unresolved_threads']) + ' unresolved review threads')
if int(d.get('deepseek_unread_findings', 0)) > 0: reasons.append(str(d['deepseek_unread_findings']) + ' DeepSeek review(s) with unread body findings')
if int(d['changes_requested']) > 0: reasons.append(str(d['changes_requested']) + ' reviews requesting changes')
if int(d['ci_failing']) > 0: reasons.append(str(d['ci_failing']) + ' CI checks failing')
if int(d['ci_pending']) > 0: reasons.append(str(d['ci_pending']) + ' CI checks pending')
for r in reasons: print('  [FAIL] ' + r)
" 2>/dev/null
    exit 1
  fi
}

# ── iteration lifecycle — Completion-driven ceremony state machine ───────

iteration_marker() {
  local ceremony="$1" milestone_number="$2"
  printf '<!-- iteration-lifecycle:%s:milestone-%s -->' "$ceremony" "$milestone_number"
}

iteration_sequence() {
  local milestone_title="$1" milestone_number="$2"
  if [[ "$milestone_title" =~ (Sprint|Iteration)[[:space:]]+([0-9]+) ]]; then
    printf '%s\n' "${BASH_REMATCH[2]}"
  else
    printf '%s\n' "$milestone_number"
  fi
}

create_iteration_ceremony() {
  local ceremony="$1" milestone_number="$2" current_sequence="$3"
  local title body marker issue_url issue_number label_status

  marker=$(iteration_marker "$ceremony" "$milestone_number")
  case "$ceremony" in
    backlog-refine)
      title="[Ceremony] Iteration ${current_sequence} Backlog Refinement"
      body=$(cat <<EOF
## Iteration ${current_sequence} Backlog Refinement

${marker}

> Run: \`/scrum-master backlog-refine\`

All committed issues in iteration milestone #${milestone_number} are closed. Complete refinement before planning the next iteration.

### Instructions for the agent
1. Scan all open issues in the repo (excluding ceremony issues)
2. For each uncategorized issue, draft a technical specification
3. Re-prioritize the backlog based on dependencies and business value
4. Post a summary as a comment on this issue
5. Close this ceremony issue when refinement is complete
EOF
)
      ;;
    iteration-plan)
      local next_sequence=$((current_sequence + 1))
      title="[Ceremony] Iteration ${next_sequence} Planning"
      body=$(cat <<EOF
## Iteration ${next_sequence} Planning

${marker}

> Run: \`/scrum-master iteration-plan\`

Backlog refinement for completed iteration milestone #${milestone_number} is closed. Planning may now create the next completion-driven iteration.

### Instructions for the agent
1. Verify the current iteration milestone has zero open issues
2. Verify its milestone-specific backlog-refinement ceremony is closed
3. Select a non-empty, realistic solo-developer scope from \`ready-for-dev\`
4. Create \`Iteration ${next_sequence}\` without a due date, assign the selected work, and verify every assignment
5. Close the completed milestone only after the new iteration is populated
6. Assign effort estimates (S/M/L), dependencies, and an iteration goal
7. Post the iteration plan as a comment on this issue
8. Close this ceremony issue when planning is complete
EOF
)
      ;;
    *)
      die "Unknown iteration ceremony: $ceremony"
      ;;
  esac

  issue_url=$(gh issue create \
    --title "$title" \
    --body "$body" \
    --label "ceremony" \
    --repo "$REPO") || die "Could not create the ${ceremony} ceremony for milestone #${milestone_number}."

  issue_number="${issue_url%/}"
  issue_number="${issue_number##*/}"
  if ! [[ "$issue_number" =~ ^[0-9]+$ ]]; then
    die "Created the ${ceremony} ceremony but could not read its issue number from '${issue_url}'."
  fi

  # `gh issue create --label` is a two-write operation: it creates the issue and
  # then applies the label. The first half can land while the second disappears,
  # and gh still exits zero. That happened live on #119: the workflow printed the
  # new issue URL and went green, but the issue had no `ceremony` label. The next
  # close would therefore miss the workflow's job condition, while a recovery
  # dispatch would miss the issue in the label-filtered idempotency lookup and
  # create a duplicate.
  #
  # Verify the postcondition, repair one missing write explicitly, then verify
  # again. An unreadable lookup is not an absent label and never authorizes a
  # blind repair. A repair that errors or silently does nothing makes the run red
  # with the already-created issue number in the log.
  if iteration_issue_has_label "$issue_number" ceremony; then
    label_status=0
  else
    label_status=$?
  fi

  if [ "$label_status" -eq 1 ]; then
    echo "WARNING: Ceremony issue #${issue_number} was created without label 'ceremony'; retrying the label write." >&2
    if ! gh issue edit "$issue_number" --add-label ceremony --repo "$REPO" >/dev/null; then
      die "Ceremony issue #${issue_number} was created, but adding label 'ceremony' failed. Apply it manually before dispatching recovery."
    fi

    if iteration_issue_has_label "$issue_number" ceremony; then
      label_status=0
    else
      label_status=$?
    fi
  fi

  if [ "$label_status" -ne 0 ]; then
    if [ "$label_status" -eq 1 ]; then
      die "Ceremony issue #${issue_number} was created, but label 'ceremony' is still missing after the retry. Apply it manually before dispatching recovery."
    fi
    die "Ceremony issue #${issue_number} was created, but its labels could not be verified. Check it manually before dispatching recovery."
  fi

  printf '%s\n' "$issue_url"
}

iteration_issue_has_label() {
  local issue_number="$1" expected_label="$2" labels label

  if ! labels=$(gh issue view "$issue_number" --repo "$REPO" --json labels --jq '.labels[].name'); then
    echo "ERROR: could not read labels on ceremony issue #${issue_number}" >&2
    return 2
  fi

  while IFS= read -r label; do
    if [ "$label" = "$expected_label" ]; then
      return 0
    fi
  done <<< "$labels"
  return 1
}

ceremonies_for_marker() {
  local ceremonies_json="$1" marker="$2"
  printf '%s\n' "$ceremonies_json" | jq -c --arg marker "$marker" '
    [.[] | select(.body != null and (.body | contains($marker)))]
  '
}

cmd_iteration_advance() {
  require_gh
  resolve_repo || die "Could not resolve the repository for iteration advancement"

  local milestones milestone_count
  milestones=$(gh api "repos/$REPO/milestones?state=open&per_page=100")
  milestone_count=$(printf '%s\n' "$milestones" | jq 'length')

  if [ "$milestone_count" -eq 0 ]; then
    echo "No active iteration milestone; nothing to advance."
    return 0
  fi

  # More than one open milestone is a supported state, not a broken one: the user
  # plans a coherent batch into a future iteration while the current one is still
  # being built. Counting milestones was never a way to identify the active one
  # anyway — the sanctioned planning ceremony opens Iteration N+1 (step 8) before
  # closing Iteration N (step 9), so the happy path itself spends a window with two
  # open milestones, and any issue closing inside it used to kill the run.
  #
  # The active iteration is the open milestone with the LOWEST sequence, read
  # through iteration_sequence (an `Iteration N` / `Sprint N` title, falling back to
  # the milestone number). Everything above it is planned ahead and nobody has
  # started it, so it is never evaluated. The comparison is numeric on purpose:
  # sorted as strings, "Iteration 10" precedes "Iteration 9".
  local -a milestone_numbers=() milestone_sequences=()
  local i entry_number entry_title entry_sequence
  for ((i = 0; i < milestone_count; i++)); do
    entry_number=$(printf '%s\n' "$milestones" | jq -r --argjson i "$i" '.[$i].number')
    entry_title=$(printf '%s\n' "$milestones" | jq -r --argjson i "$i" '.[$i].title')
    entry_sequence=$(iteration_sequence "$entry_title" "$entry_number")
    # A milestone whose title does not parse falls back to its number, so an
    # unreadable sequence means an unreadable number: comparing it would fail the
    # test silently and hand the state machine a milestone chosen at random.
    if ! [[ "$entry_sequence" =~ ^[0-9]+$ ]]; then
      die "Unreadable iteration sequence for open milestone '${entry_title}' (#${entry_number})."
    fi
    milestone_numbers+=("$entry_number")
    milestone_sequences+=("$entry_sequence")
  done

  local active_index=0
  for ((i = 1; i < milestone_count; i++)); do
    if [ "${milestone_sequences[$i]}" -lt "${milestone_sequences[$active_index]}" ]; then
      active_index=$i
    fi
  done

  # The one ambiguity that survives: two open milestones on the same sequence.
  # "Lowest" has two answers there and picking either advances an iteration at
  # random, so it fails closed and names them. A collision ABOVE the active
  # iteration is left alone — the active one is still unambiguous, blocking on it
  # would repeat the mistake this rule replaces, and when the collision becomes the
  # lowest it fails closed then, with this same message.
  local tied_count=0 tied_list=""
  for ((i = 0; i < milestone_count; i++)); do
    if [ "${milestone_sequences[$i]}" -eq "${milestone_sequences[$active_index]}" ]; then
      tied_count=$((tied_count + 1))
      tied_list="${tied_list:+${tied_list}, }#${milestone_numbers[$i]}"
    fi
  done
  if [ "$tied_count" -ne 1 ]; then
    die "Open milestones ${tied_list} all resolve to iteration sequence ${milestone_sequences[$active_index]}; cannot tell which iteration is active. Rename or close one, then dispatch recovery."
  fi

  local milestone_number open_issues closed_issues current_sequence
  milestone_number="${milestone_numbers[$active_index]}"
  current_sequence="${milestone_sequences[$active_index]}"
  open_issues=$(printf '%s\n' "$milestones" | jq -r --argjson i "$active_index" '.[$i].open_issues')
  closed_issues=$(printf '%s\n' "$milestones" | jq -r --argjson i "$active_index" '.[$i].closed_issues')

  if [ "$milestone_count" -gt 1 ]; then
    echo "Active iteration is #${milestone_number} (Iteration ${current_sequence}), the lowest of ${milestone_count} open milestones; the rest are planned ahead and are not evaluated."
  fi

  if ! [[ "$open_issues" =~ ^[0-9]+$ ]]; then
    die "Unreadable open issue count for milestone #${milestone_number}."
  fi
  if ! [[ "$closed_issues" =~ ^[0-9]+$ ]]; then
    die "Unreadable closed issue count for milestone #${milestone_number}."
  fi
  if [ "$open_issues" -gt 0 ]; then
    echo "Iteration ${current_sequence} still has ${open_issues} open issue(s); no ceremony created."
    return 0
  fi
  # "Never started" reads identically to "complete" on open_issues alone. A milestone
  # with no issues at all is the former: the repo's first milestone before planning
  # populates it, or an Iteration N+1 whose assignment step failed after the previous
  # milestone was already closed. Advancing on that manufactures a refinement ceremony
  # for an iteration that committed nothing.
  if [ "$closed_issues" -eq 0 ]; then
    echo "Iteration ${current_sequence} has no issues assigned; nothing to advance."
    return 0
  fi

  local ceremonies ceremony_count refinement_marker refinement_matches refinement_count refinement_state
  ceremonies=$(gh issue list \
    --repo "$REPO" \
    --state all \
    --label ceremony \
    --limit "$CEREMONY_LOOKUP_LIMIT" \
    --json number,state,body,url)
  ceremony_count=$(printf '%s\n' "$ceremonies" | jq 'length')

  # See CEREMONY_LOOKUP_LIMIT: a full page may be a truncated page, and truncation
  # here silently creates duplicate ceremonies instead of refusing to guess.
  if [ "$ceremony_count" -ge "$CEREMONY_LOOKUP_LIMIT" ]; then
    die "Ceremony lookup returned ${ceremony_count} issues at its ${CEREMONY_LOOKUP_LIMIT} limit and may be truncated; raise CEREMONY_LOOKUP_LIMIT before advancing."
  fi

  refinement_marker=$(iteration_marker backlog-refine "$milestone_number")
  refinement_matches=$(ceremonies_for_marker "$ceremonies" "$refinement_marker")
  refinement_count=$(printf '%s\n' "$refinement_matches" | jq 'length')

  if [ "$refinement_count" -eq 0 ]; then
    echo "Iteration ${current_sequence} is complete; creating backlog-refinement ceremony."
    create_iteration_ceremony backlog-refine "$milestone_number" "$current_sequence"
    return 0
  fi
  if [ "$refinement_count" -ne 1 ]; then
    die "Found ${refinement_count} backlog-refinement ceremonies for milestone #${milestone_number}; refusing to guess."
  fi

  refinement_state=$(printf '%s\n' "$refinement_matches" | jq -r '.[0].state' | tr '[:lower:]' '[:upper:]')
  if [ "$refinement_state" = "OPEN" ]; then
    echo "Backlog refinement for Iteration ${current_sequence} is still open; planning remains locked."
    return 0
  fi
  if [ "$refinement_state" != "CLOSED" ]; then
    die "Unknown backlog-refinement state '${refinement_state}' for milestone #${milestone_number}."
  fi

  local planning_marker planning_matches planning_count planning_state
  planning_marker=$(iteration_marker iteration-plan "$milestone_number")
  planning_matches=$(ceremonies_for_marker "$ceremonies" "$planning_marker")
  planning_count=$(printf '%s\n' "$planning_matches" | jq 'length')

  if [ "$planning_count" -eq 0 ]; then
    echo "Backlog refinement is complete; creating Iteration $((current_sequence + 1)) planning ceremony."
    create_iteration_ceremony iteration-plan "$milestone_number" "$current_sequence"
    return 0
  fi
  if [ "$planning_count" -ne 1 ]; then
    die "Found ${planning_count} iteration-planning ceremonies for milestone #${milestone_number}; refusing to guess."
  fi

  planning_state=$(printf '%s\n' "$planning_matches" | jq -r '.[0].state' | tr '[:lower:]' '[:upper:]')
  if [ "$planning_state" = "OPEN" ]; then
    echo "Iteration $((current_sequence + 1)) planning ceremony is already open; no duplicate created."
    return 0
  fi

  die "Iteration planning ceremony is closed but milestone #${milestone_number} remains active. Repair the milestone manually, then dispatch recovery."
}

# ── Dispatch ─────────────────────────────────────────────────────────────────

# Skipped when the script is sourced (tests source it to call functions directly).
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

case "${1:-}" in
  pr-status)
    shift; cmd_pr_status "$@"
    ;;
  pr-status-json)
    shift; cmd_pr_status_json "$@"
    ;;
  pr-comments)
    shift; cmd_pr_comments "$@"
    ;;
  pr-label)
    shift; cmd_pr_label "$@"
    ;;
  pr-deepseek-rounds)
    shift; cmd_pr_deepseek_rounds "$@"
    ;;
  pr-deepseek-force-review)
    shift; cmd_pr_deepseek_force_review "$@"
    ;;
  pr-check-label)
    shift; cmd_pr_check_label "$@"
    ;;
  is-ready-to-merge)
    shift; cmd_is_ready_to_merge "$@"
    ;;
  iteration-advance)
    shift; cmd_iteration_advance "$@"
    ;;
  *)
    cat <<EOF
Usage: gh-automation.sh <command> [args]

Commands:
  pr-status <pr>         Human-readable PR status (CI + threads)
  pr-status-json <pr>    Machine-readable JSON PR status
  pr-comments <pr>       List review comments on PR
  pr-label <pr> <add|remove> <label>   Add or remove a label; adding is idempotent
                                       and a failed write exits non-zero
  pr-deepseek-rounds <pr>              DeepSeek review round status (JSON)
  pr-deepseek-force-review <pr> [ref]  Dispatch a DeepSeek review that ignores the
                                       round cap (ref defaults to develop)
  pr-check-label <pr> <label>          Exit 0 present, 1 absent, 2 undetermined
  is-ready-to-merge <pr>              Exit 0 if frozen rule met
  iteration-advance                   Advance completion-driven iteration ceremonies

Frozen acceptance rule for READY TO MERGE:
  Add label only when: CI is green, unresolved review thread count is zero,
  no DeepSeek review is holding unread findings in its body (clear with the
  ${DEEPSEEK_REVIEW_READ_LABEL} label), and DeepSeek review is definitively
  finished (approved, all rounds exhausted, or exempt via NO_DEEPSEEK_REVIEW
  label).
EOF
    exit 0
    ;;
esac
