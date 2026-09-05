---
name: process-pr
description: Use ONLY when the user explicitly requests /process-pr. Manages PR feedback loops. Use to resolve CI failures and review feedback on open PRs.
compatibility: opencode
metadata:
  opencode/autoinvoke: "false"
---


# process-pr — PR Remediation Skill

Triggers: `/process-pr <pr-number>` or `/process-pr` (uses current branch PR)

## Purpose

A remediation cycle for conflicts, CI failures, and review feedback. It may be invoked by a user
or by an iteration orchestrator.

The passive monitoring path (`pr-labeler.yml`) handles the normal case. This skill is the escape hatch for when you want results now.

**All work is done in a `git worktree`** — never operate on the main working directory. If an existing worktree for this PR's branch already exists (e.g., from a prior `/dev-issue` or `/process-pr` run that wasn't cleaned up), reuse it.

## Timing reality — read before touching the polling code

DeepSeek's review job is budgeted at `DEEPSEEK_REQUEST_TIMEOUT_SECONDS` (2700) × (`DEEPSEEK_MAX_RETRIES` (1) + 1) = 90 minutes, under a `timeout-minutes: 100` job cap with 10 minutes reserved for setup and posting. Real runs vary with diff size, and the diff cap is 90,000 characters (measured at `high` on #925: the heaviest of seventeen replays used under 19% of the output ceiling) — a pull request at the cap genuinely can use a good part of the wall-clock budget, and one over it is truncated before the call rather than allowed to exhaust the model's output budget with no verdict to show (#167). The current `high` reasoning effort trades some review depth for lower latency and a lower risk of exhausting the shared reasoning/verdict budget; it does not shorten the request budget. **Any wait shorter than the request budget does not "poll" — it just times out and reports stale state.** `DEEPSEEK_WAIT_SECONDS` below is 5700 (95 min), covering the request budget while still ending before the job cap; do not lower it without changing the workflow first.

The review round budget is `MAX_ROUNDS: "1"` (set in `.github/workflows/deepseek-pr-review.yml`). Exactly one Mode A full review is automatic; thread replies (Mode B) do not spend the budget and continue indefinitely. So the normal execution of this skill is **a single pass**, not a loop. Step 4 is written that way.

Because `pull_request` runs check out the merge ref, the workflow that actually executes is the
**base branch's** copy. Read the PR's actual base branch version of
`deepseek-pr-review.yml` when diagnosing review behaviour; do not assume the base is `develop`.

## Workflow

### Step 0: Common Setup

```bash
set -euo pipefail
REPO=$(gh repo view --json nameWithOwner --jq '.nameWithOwner')
REPO_ROOT=$(git rev-parse --show-toplevel)
DEEPSEEK_WAIT_SECONDS=5700
POLL_INTERVAL=30
```

### Step 1: Determine PR Number

If the user provided a PR number, use it. Otherwise, find the PR for the current branch:

```bash
gh pr list --head "$(git branch --show-current)" --json number --jq '.[0].number'
```

Exit with an error if no PR is found.

### Step 2: Create/Reuse Worktree and Detect Workspaces

All file edits and git operations happen **inside a worktree**. Never modify the main working directory.

```bash
PR_META=$(gh pr view <pr-number> --json headRefName,baseRefName,state)
BRANCH=$(echo "$PR_META" | jq -er '.headRefName | select(length > 0)')
BASE_BRANCH=$(echo "$PR_META" | jq -er '.baseRefName | select(length > 0)')
[ "$(echo "$PR_META" | jq -r '.state')" = "OPEN" ] || exit 1
git fetch origin "$BRANCH" "$BASE_BRANCH"
REMOTE_HEAD=$(git rev-parse "origin/$BRANCH")

# Reuse an existing worktree for this branch if one is checked out.
# substr() rather than $2 — worktree paths can contain spaces.
WORKTREE_DIR=$(git worktree list --porcelain | awk -v b="refs/heads/$BRANCH" '
  /^worktree /{wt=substr($0,10)}
  /^branch /{if (substr($0,8)==b) {print wt; exit}}')

if [ -n "$WORKTREE_DIR" ]; then
  echo "Reusing existing worktree: $WORKTREE_DIR"
  cd "$WORKTREE_DIR"
  [ -z "$(git status --porcelain)" ] || {
    echo "Existing worktree is dirty; preserving it and stopping"
    exit 1
  }
  LOCAL_HEAD=$(git rev-parse HEAD)
  if [ "$LOCAL_HEAD" = "$REMOTE_HEAD" ]; then
    :
  elif git merge-base --is-ancestor "$LOCAL_HEAD" "$REMOTE_HEAD"; then
    git merge --ff-only "origin/$BRANCH"
  else
    echo "Existing worktree has local commits or diverged history; preserving it and stopping"
    exit 1
  fi
else
  WORKTREE_DIR="$(dirname "$REPO_ROOT")/voxelheim-pr-<pr-number>"
  if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
    [ "$(git rev-parse "refs/heads/$BRANCH")" = "$REMOTE_HEAD" ] || {
      echo "Local branch differs from origin; preserving it and stopping"
      exit 1
    }
    git worktree add "$WORKTREE_DIR" "$BRANCH"
  else
    git worktree add -b "$BRANCH" "$WORKTREE_DIR" "origin/$BRANCH"
  fi
  cd "$WORKTREE_DIR"
  [ -f server/go.mod ] && (cd server && go mod download)
  [ -f client/Cargo.toml ] && (cd client && cargo fetch)
fi

[ "$(git rev-parse --show-toplevel)" = "$WORKTREE_DIR" ] || { echo "NOT in worktree — abort"; exit 1; }
```

**Detect every touched workspace, not just one.** A cross-cutting PR must be gated everywhere it lands:

```bash
WORKSPACES=$(gh pr view <pr-number> --json files --jq '
  [.files[].path | split("/")[0]]
  | unique
  | map(select(. == "server" or . == "client" or . == "schemas"))
  | join(" ")')

echo "Workspaces: ${WORKSPACES:-<none>} | Worktree: $WORKTREE_DIR"
```

**Every PR runs the automation helper suite**, including workspace-only and docs-only changes.
An empty `WORKSPACES` means run the helper suite alone. Helper tests read across workspace
boundaries, so no path selector may exempt them; this matches CI's unconditional automation
selection. Skill-directory edits also require regenerating the adapters.

**Gate commands — these mirror `.github/workflows/ci.yml` exactly, and `scripts/test/gate-tables.test.sh` is what makes that a fact rather than a claim. Run one per detected workspace:**

Run each chain as it is written and take its exit status as the verdict. Piping one into
`head` or `tail` reports the pager's success instead of the gate's, and a failing gate then
reads as a passing one — `/dev-issue` Step 6 carries the reasoning, and this table is the
other place the mistake is available.

| Workspace | Gate command (from `$WORKTREE_DIR`) |
| --------- | ----------------------------------- |
| `server` | `cd server && test -z "$(gofmt -l .)" && go vet ./... && golangci-lint run && go build ./... && GOARCH=386 go build ./... && GOARCH=arm go build ./... && go test ./...` |
| `client` | `cd client && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo build --workspace --locked && cargo test --workspace --locked` |
| `schemas` | `bash scripts/check-schemas.sh` — and because a contract change rebuilds both consumers, run the `server` and `client` gates too |
| Every PR | the full `scripts/test/*.test.sh` suite (glob it — never retype the list) plus `python3 .github/scripts/test_deepseek_review.py`; run them under a failure flag so a red test is the block's exit status, as `/dev-issue` Step 6 spells out |

Formatting is the gate most often skipped and the one that most often reddens CI. It is not optional.

#### Step 2b: Reconcile the current base

Do this on every remediation run. If the current base tip is not an ancestor of the PR head, merge
it into the head without rebasing or force-pushing, resolve any conflicts, run all affected Step 2
gates, and push. This also invalidates stale CI on a technically mergeable child after its feature
base advances.

```bash
BASE_HEAD=$(git rev-parse "origin/$BASE_BRANCH")
if ! git merge-base --is-ancestor "$BASE_HEAD" HEAD; then
  [ "$(git rev-parse "origin/$BRANCH")" = "$REMOTE_HEAD" ] || exit 1
  git merge --no-ff --no-commit "origin/$BASE_BRANCH"
  # Resolve conflicts; if their intent is ambiguous, abort and stop without publishing.
  # Run every affected gate, then:
  git add -A
  git commit -m "fix: reconcile ${BASE_BRANCH} on PR #<pr-number>"
  bash scripts/check-publication-privacy.sh
  bash scripts/check-commit-privacy.sh "origin/$BASE_BRANCH" HEAD
  git push origin HEAD
  REMOTE_HEAD=$(git rev-parse HEAD)
fi
```

After this push, require fresh CI. The base commits were reviewed on their own PRs, so do not wait
for an automatic review that cannot start after the findings-round cap is spent; an aggregate
parent still needs the explicit assembled-head review required by `/develop-iteration`.

### Step 3: Check PR Status

```bash
bash scripts/gh-automation.sh pr-status <pr-number>
```

This evaluates the full frozen READY TO MERGE rule and prints a `[FAIL]` line for each unmet condition. The rule has **seven** conditions, all of which must hold:

1. Unresolved (non-outdated) review threads = 0
2. No review requesting changes
3. No failing CI checks
4. No pending CI checks
5. No *missing* required checks — the stable `ci-gate` check must have actually run and succeeded
6. `mergeable == MERGEABLE`
7. DeepSeek definitively finished (approved, rounds exhausted, or `NO_DEEPSEEK_REVIEW` exempt), with no unread findings left in a review body (cleared with the `DEEPSEEK_REVIEW_READ` label)

Conditions 5 and 6 exist because a **conflicting PR runs zero Actions checks** — with nothing red,
a naive "is CI failing?" read calls that green. A conflict is actionable remediation, not a wait;
Step 2b merges the current base into the PR head so CI can run again.

The helper fails closed: an unreadable value counts as failure, never as pass.

### Step 4: Process the DeepSeek Review Round (in worktree)

With `MAX_ROUNDS: "1"` this is a **single pass**. Repeat 4a–4e only if `max_rounds` is raised in the workflow, or after an explicit `pr-deepseek-force-review` dispatch. Safety cap either way: **5 iterations**.

#### 4a — Read DeepSeek state

```bash
ROUNDS=$(bash scripts/gh-automation.sh pr-deepseek-rounds <pr-number>) || true
if ! echo "$ROUNDS" | jq -e '.' >/dev/null 2>&1; then
  echo "ERROR: pr-deepseek-rounds returned invalid JSON. Raw output: ${ROUNDS:0:200}"
  exit 1
fi
# The helper reports configuration/API failures as {"error":...} + non-zero exit,
# so a zeroed round count always means "no reviews yet", never "lookup failed".
if echo "$ROUNDS" | jq -e '.error' >/dev/null 2>&1; then
  echo "ERROR: pr-deepseek-rounds failed: $(echo "$ROUNDS" | jq -r '.error')"
  exit 1
fi
REVIEW_COMPLETE=$(echo "$ROUNDS" | jq -r '.review_complete')
ROUNDS_EXHAUSTED=$(echo "$ROUNDS" | jq -r '.review_rounds_exhausted')
BOT_REVIEW_COUNT=$(echo "$ROUNDS" | jq -r '.bot_review_count')
PREV_REVIEW_ID=$(echo "$ROUNDS" | jq -r '.latest_review_id')
ROUND_CAP=$(echo "$ROUNDS" | jq -r '.max_rounds')
echo "DeepSeek state: rounds=$BOT_REVIEW_COUNT/$ROUND_CAP complete=$REVIEW_COMPLETE exhausted=$ROUNDS_EXHAUSTED"
```

**Guard**: if `review_complete=true` or `review_rounds_exhausted=true`, there is no round to wait for. Skip 4b entirely and go to 4c — a round that already landed still has threads worth addressing.

#### 4b — Wait for the review (only if none has landed yet)

Run this only when `PREV_REVIEW_ID` is `0`.

```bash
START_TIME=$(date +%s)
while true; do
  sleep "$POLL_INTERVAL"

  # Deadline check goes FIRST, before any guard that can `continue`. A persistently
  # failing helper (broken gh auth, network partition, helper regression) otherwise
  # loops forever through the retry paths below and never reaches a check at the
  # bottom — hanging silently in exactly the failure mode this wait exists to report.
  ELAPSED=$(($(date +%s) - START_TIME))
  if [ "$ELAPSED" -ge "$DEEPSEEK_WAIT_SECONDS" ]; then
    echo "Timeout (${DEEPSEEK_WAIT_SECONDS}s) waiting for DeepSeek — the job may be approaching its own 100-min cap."
    echo "Check: gh run list --workflow=deepseek-pr-review.yml --limit 3"
    break
  fi

  CURRENT=$(bash scripts/gh-automation.sh pr-deepseek-rounds <pr-number>) || true
  if ! echo "$CURRENT" | jq -e '.' >/dev/null 2>&1; then
    echo "WARNING: pr-deepseek-rounds returned invalid JSON, retrying... (${CURRENT:0:200})"
    continue
  fi
  if echo "$CURRENT" | jq -e '.error' >/dev/null 2>&1; then
    echo "WARNING: pr-deepseek-rounds failed ($(echo "$CURRENT" | jq -r '.error')), retrying..."
    continue
  fi
  NEW_ID=$(echo "$CURRENT" | jq -r '.latest_review_id')

  # Check for a new review ID FIRST — a review that also sets review_complete=true
  # still carries feedback worth processing.
  if [ "$NEW_ID" != "0" ] && [ "$NEW_ID" != "$PREV_REVIEW_ID" ]; then
    echo "DeepSeek review arrived (id=$NEW_ID)"
    PREV_REVIEW_ID="$NEW_ID"
    BOT_REVIEW_COUNT=$(echo "$CURRENT" | jq -r '.bot_review_count')
    break
  fi

  if [ "$(echo "$CURRENT" | jq -r '.review_complete')" = "true" ]; then
    echo "DeepSeek approved with no issues found"
    break
  fi

  echo "Waiting for DeepSeek review... (elapsed: ${ELAPSED}s / ${DEEPSEEK_WAIT_SECONDS}s)"
done
```

A timeout here is a real signal, not noise. A job cancelled by the 100-minute cap produces **no output at all**, and `pr-status-json` counts `CANCELLED` as failing — so the PR sticks at `needs-work` with nothing in the log explaining why. If that happened, say so explicitly rather than continuing silently.

#### 4c — Address the round's feedback

1. **Fetch unresolved, non-outdated review threads**:

   ```bash
   bash scripts/gh-automation.sh pr-comments <pr-number>

   gh api graphql --raw-field 'query=query($owner:String!,$repo:String!,$pr:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$pr){reviewThreads(first:50){nodes{id isResolved isOutdated comments(first:2){nodes{databaseId body}}}}}}}' \
     -f owner="${REPO%%/*}" -f repo="${REPO##*/}" -F pr=<pr-number> \
     --jq '.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved == false and .isOutdated == false) | {id, comments: [.comments.nodes[] | {databaseId, body}]}'
   ```

2. **Implement fixes for each unresolved thread** (files under `$WORKTREE_DIR/`):
   - Valid bug or suggestion → implement it
   - Question → answer it in a reply
   - Nitpick under ~5 minutes → just fix it
   - Out of scope → reply explaining why; do not silently ignore

   **Publication order:** prepare the dispositions in steps 3–5, but do not send replies, resolve
   threads, post the body audit, or write `DEEPSEEK_REVIEW_READ` until Step 4e has pushed every
   source fix and verified the remote head. A fix that exists only in the worktree has not landed.

3. **Prepare a reply for every thread**, including ones you did not act on. Record the comment ID,
   thread ID, and response text for Step 4f; make no GitHub write yet.

4. **Prepare to resolve** only threads where the fix will have landed or the point is genuinely
   addressed. Record that decision for Step 4f; make no GitHub write yet.

   A thread whose suggestion you **rejected** may be resolved too (#217) — but only after a reply
   that states the evidence, because that reply is the whole of the audit trail once no human is
   required to read it. Never resolve a thread you have not answered.

5. **Findings in the review body** (general comments) create no thread and cannot be resolved.
   Fetch the complete recent DeepSeek review bodies, not just the thread list, and read every
   finding before deciding whether to acknowledge them:

   ```bash
   ACK_STATE=$(bash scripts/gh-automation.sh pr-deepseek-rounds <pr-number>)
   ACK_REVIEW_ID=$(echo "$ACK_STATE" | jq -er '.latest_review_id | select(. != 0)')
   if ! BODY_REVIEWS=$(gh api graphql \
     --raw-field 'query=query($owner:String!,$repo:String!,$pr:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$pr){reviews(last:100,states:[APPROVED,CHANGES_REQUESTED,COMMENTED]){nodes{databaseId submittedAt state author{login} body}}}}}' \
     -f owner="${REPO%%/*}" -f repo="${REPO##*/}" -F pr=<pr-number>); then
     echo "Could not fetch DeepSeek review bodies; acknowledgement is blocked"
     exit 1
   fi
   echo "$BODY_REVIEWS" | jq --arg bot "${DEEPSEEK_BOT_USER:-github-actions[bot]}" '
     def canon: sub("\\[bot\\]$"; "");
     .data.repository.pullRequest.reviews.nodes[]
     | select((.author.login // "" | canon) == ($bot | canon))
     | select((.body // "") != "")
     | {databaseId, submittedAt, state, body}'
   ```

   Address each valid point in code. A point you reject needs concrete evidence from the diff,
   a test, or a measured behavior; disagreement without evidence is not a disposition. If any
   finding is unclear or unsupported by evidence either way, leave it unacknowledged and report
   the block.

   Record one disposition per body finding for Step 4f. Never use `NO_DEEPSEEK_REVIEW`; that
   exemption remains human-only.

#### 4d — Fix CI failures

```bash
bash scripts/gh-automation.sh pr-status <pr-number>
```

If CI is failing, read the run logs (`gh run view <run-id> --log-failed`), fix the code, then run the Step 2 gate command for **every** affected workspace before pushing.

If the failure is infrastructure (runner outage, registry timeout, expired token), report it and stop. Do not paper over it with code changes.

#### 4e — Commit and Push

If feedback requires correcting the pull request's title or body, **never use `gh pr edit`**.
On `gh` 2.45.0 its Projects-classic pre-fetch fails before any flag is applied (#206). Use
`bash scripts/gh-automation.sh pr-edit <pr> --title "<title>" --body-file <path>`; the
helper writes through REST and verifies every requested field by reading the pull request back.

```bash
cd "$WORKTREE_DIR"
git add -A
if git diff --cached --quiet; then
  echo "No source changes to commit"
else
  git commit -m "fix: address feedback on PR #<pr-number>"
  bash scripts/check-publication-privacy.sh
  bash scripts/check-commit-privacy.sh "origin/$BASE_BRANCH" HEAD
  git fetch origin "$BRANCH"
  [ "$(git rev-parse "origin/$BRANCH")" = "$REMOTE_HEAD" ] || {
    echo "PR head moved; preserve local commits and reconstruct state"
    exit 1
  }
  git push origin HEAD
  REMOTE_HEAD=$(git rev-parse HEAD)
fi

# Confirm the PR is still open at REMOTE_HEAD. Only then execute the prepared
# replies/resolutions and body acknowledgement sequence from steps 3–5.
CURRENT_PR=$(gh pr view <pr-number> --json state,headRefOid)
[ "$(echo "$CURRENT_PR" | jq -r '.state')" = "OPEN" ] || exit 1
[ "$(echo "$CURRENT_PR" | jq -r '.headRefOid')" = "$REMOTE_HEAD" ] || exit 1
```

**NEVER run `git add` / `git commit` / `git push` from the main repo directory.** Always from `$WORKTREE_DIR`.

With the round budget spent, this push does **not** trigger another review — pushes are skipped once the cap is reached, and a one-time notice is posted as an issue comment. If another pass is genuinely wanted:

```bash
bash scripts/gh-automation.sh pr-deepseek-force-review <pr-number> [ref]
```

`ref` defaults to `develop` and must be a branch carrying the bypass, because the dispatched run executes that ref's workflow definition.

#### 4f — Publish dispositions after the push

Only after Step 4e verifies the remote head, publish every prepared reply, then resolve its thread:

```bash
gh api "repos/$REPO/pulls/<pr-number>/comments/<comment-databaseId>/replies" \
  --field body="<response naming the pushed fix or rejection evidence>" || exit 1
gh api graphql \
  -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{id isResolved}}}' \
  -f id="<THREAD_ID>" || exit 1
```

For body findings, post one public disposition per finding (fixed with file/test, or rejected with
evidence) and scan the exact comment before posting. This is the public audit trail. Then re-read
`latest_review_id`; it must still equal `ACK_REVIEW_ID`, or repeat this step from the body fetch.
Refresh `DEEPSEEK_REVIEW_READ` only after the audit exists, treating label lookup as three-state
(present/absent/unreadable), and verify both the fresh label and postconditions:

```bash
ACK_COMMENT='<one bullet per DeepSeek body finding, including review ID and evidence>'
printf '%s\n' "$ACK_COMMENT" | bash scripts/check-body-privacy.sh || exit 1
gh pr comment <pr-number> --body "$ACK_COMMENT" || exit 1
LATEST_BEFORE_ACK=$(bash scripts/gh-automation.sh pr-deepseek-rounds <pr-number> \
  | jq -er '.latest_review_id')
[ "$LATEST_BEFORE_ACK" = "$ACK_REVIEW_ID" ] || exit 1
if bash scripts/gh-automation.sh pr-check-label <pr-number> DEEPSEEK_REVIEW_READ; then
  bash scripts/gh-automation.sh pr-label <pr-number> remove DEEPSEEK_REVIEW_READ || exit $?
  if bash scripts/gh-automation.sh pr-check-label <pr-number> DEEPSEEK_REVIEW_READ; then
    echo "Stale acknowledgement label is still present"
    exit 1
  else
    LABEL_RC=$?
    [ "$LABEL_RC" -eq 1 ] || exit "$LABEL_RC"
  fi
else
  LABEL_RC=$?
  [ "$LABEL_RC" -eq 1 ] || exit "$LABEL_RC"
fi
bash scripts/gh-automation.sh pr-label <pr-number> add DEEPSEEK_REVIEW_READ || exit $?
if ! bash scripts/gh-automation.sh pr-check-label <pr-number> DEEPSEEK_REVIEW_READ; then
  echo "Could not verify the fresh acknowledgement label"
  exit 1
fi
LATEST_AFTER_ACK=$(bash scripts/gh-automation.sh pr-deepseek-rounds <pr-number> \
  | jq -er '.latest_review_id')
UNREAD_AFTER_ACK=$(bash scripts/gh-automation.sh pr-status-json <pr-number> \
  | jq -er '.deepseek_unread_findings')
if [ "$LATEST_AFTER_ACK" != "$ACK_REVIEW_ID" ] || [ "$UNREAD_AFTER_ACK" != "0" ]; then
  bash scripts/gh-automation.sh pr-label <pr-number> remove DEEPSEEK_REVIEW_READ
  exit 1
fi
```

A newer review remains unread because its timestamp follows the label. If any disposition is not
understood or evidence-backed, leave the label absent. Never apply `NO_DEEPSEEK_REVIEW`.

### Step 5: Final Verification

Re-run the status check and confirm all frozen-rule conditions from Step 3 are met:

```bash
bash scripts/gh-automation.sh pr-status <pr-number>
bash scripts/gh-automation.sh pr-check-label <pr-number> "READY TO MERGE"
```

`pr-labeler.yml` applies the label — on CI completion, on the six-hour sweep, and on manual dispatch. If `pr-status` shows the rule satisfied but the label is absent, the labeler simply has not run yet.

### Step 6: Cleanup Worktree

```bash
MAIN_REPO=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
cd "$MAIN_REPO"

git worktree remove "$WORKTREE_DIR"
git worktree prune
```

If `git worktree remove` fails (uncommitted changes, or the directory is busy), warn the user and provide the manual cleanup command.

### Step 7: Report

```
PR #<number>: <title>
├── Mergeable:   ✅ no conflicts
├── CI:          ✅ ci-gate ran and passed
├── DeepSeek:    ✅ approved / rounds exhausted
├── Threads:     ✅ 0 unresolved
├── Body finds:  ✅ none unread (or: N left unacknowledged with the blocking reason)
├── Label:       READY TO MERGE
└── Status:      Ready for an autonomous non-main merge
```

Report each line from what `pr-status` actually printed. A standalone `/process-pr` invocation
returns after remediation; an orchestrator such as `/develop-iteration` may now perform the fresh
readiness check and merge.

**Do not merge from this remediation skill.** This is a separation of responsibilities, not a
human gate: autonomous merge decisions for non-main bases belong to the caller that reads the
finished result and the PR's ordering constraints.

## Guardrails

- **Worktree only**: all file edits, git operations, and quality gates run inside the worktree. Never operate on the main repo's working directory.
- **Verify worktree**: before any file edit, confirm `git rev-parse --show-toplevel` equals `$WORKTREE_DIR`.
- Single pass by default (`MAX_ROUNDS: "1"`); hard cap 5 iterations if the budget is ever raised.
- Gate **every** touched workspace, not just the first one detected.
- If CI failures are infrastructure-related (not code), report to the user and stop.
- **Never poll `mergeable`, and never block on it.** It is the one frozen-rule input with no
  liveness guarantee: a **merged** PR reports it as `null` forever, so `until mergeable != UNKNOWN`
  is not slow — it is unsatisfiable, and burns its whole timeout. It is also not yours to wait for.
  `pr-status` retries it internally (`resolve_mergeable`), the labeler re-reads it on every pass,
  and Step 5 already says an absent label with a satisfied rule just means the labeler has not run
  yet. Report what you measured and exit.
- **Before waiting on anything about a PR, check the PR is still open.** A merge or close between
  your last read and your next one silently invalidates every condition you are polling — this is
  how a 15-minute wait gets spent on a PR that landed two minutes in.
- If DeepSeek does not arrive within `DEEPSEEK_WAIT_SECONDS` (5700), report it as a probable job-cap timeout and exit the loop — do not treat it as "no findings".
- **GraphQL rate limits**: one wait is at most 190 polls (5700s ÷ 30s) at ~3–5 points each — under 950 points. GitHub allows 5000 points/hour. Avoid concurrent force-cycles across multiple PRs.
- NEVER push directly to `main` (`git push origin main`), and never merge a pull request into
  `main` — human-only. Merging into any non-main base is authorized through
  **`bash scripts/gh-automation.sh pr-merge <pr> --head <observed-sha> --base-head
  <observed-base-sha>`**: it refuses a `main` base by name, fails closed on one it cannot read
  (#218), and rejects a head or base that moved after the readiness read. Read the pull-request
  body before you merge — an ordering
  stated against another PR binds whoever merges, and the frozen rule cannot see it (#214 and #215
  were each `ready_to_merge: true`, and merging one alone broke `develop` at runtime with nothing
  turning red).
- Never force-push or rebase without explicit user instruction. Conflict remediation merges the
  current base into the PR head and preserves history.
- Always run quality gates locally before pushing fixes.
- Apply `DEEPSEEK_REVIEW_READ` only through Step 4c's read/dispose/public-audit/fresh-write
  sequence. Never apply `NO_DEEPSEEK_REVIEW`; that exemption remains human-only.

## Reference

- Shared helpers: `scripts/gh-automation.sh` (`pr-status`, `pr-deepseek-rounds`, `pr-deepseek-force-review`)
- DeepSeek reviewer: `.github/workflows/deepseek-pr-review.yml` / `.github/scripts/deepseek_review.py`
- Passive monitor: `.github/workflows/pr-labeler.yml`
- CI definition (source of truth for gates): `.github/workflows/ci.yml`
- Branch protection: `.github/branch-protection.md`
