---
name: process-pr
description: Use ONLY when the user explicitly requests /process-pr. Manages PR feedback loops. Use to resolve CI failures and review feedback on open PRs.
compatibility: opencode
metadata:
  opencode/autoinvoke: "false"
---


# process-pr — Manual PR Force-Cycle Skill

Triggers: `/process-pr <pr-number>` or `/process-pr` (uses current branch PR)

## Purpose

A manual force-cycle for PR monitoring. Use this when you want immediate feedback processing without waiting for the `pr-labeler.yml` sweep. This skill reads DeepSeek review comments, resolves them, fixes CI failures, and pushes updates.

The passive monitoring path (`pr-labeler.yml`) handles the normal case. This skill is the escape hatch for when you want results now.

**All work is done in a `git worktree`** — never operate on the main working directory. If an existing worktree for this PR's branch already exists (e.g., from a prior `/dev-issue` or `/process-pr` run that wasn't cleaned up), reuse it.

## Timing reality — read before touching the polling code

DeepSeek's review job is budgeted at `DEEPSEEK_REQUEST_TIMEOUT_SECONDS` (2700) × (`DEEPSEEK_MAX_RETRIES` (1) + 1) = 90 minutes, under a `timeout-minutes: 100` job cap with 10 minutes reserved for setup and posting. Real runs vary with diff size, and the diff cap is 90,000 characters — a pull request at the cap genuinely can use much of that budget, and one over it is truncated before the call rather than allowed to exhaust the model's output budget with no verdict to show (#167). **Any wait shorter than the request budget does not "poll" — it just times out and reports stale state.** `DEEPSEEK_WAIT_SECONDS` below is 5700 (95 min), covering the request budget while still ending before the job cap; do not lower it without changing the workflow first.

The review round budget is `MAX_ROUNDS: "1"` (set in `.github/workflows/deepseek-pr-review.yml`). Exactly one Mode A full review is automatic; thread replies (Mode B) do not spend the budget and continue indefinitely. So the normal execution of this skill is **a single pass**, not a loop. Step 4 is written that way.

Because `pull_request` runs check out the merge ref, the workflow that actually executes is the **base branch's** copy. Read `develop`'s `deepseek-pr-review.yml` when diagnosing review behaviour, never the feature branch's.

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
BRANCH=$(gh pr view <pr-number> --json headRefName --jq '.headRefName')

# Reuse an existing worktree for this branch if one is checked out.
# substr() rather than $2 — worktree paths can contain spaces.
WORKTREE_DIR=$(git worktree list --porcelain | awk -v b="refs/heads/$BRANCH" '
  /^worktree /{wt=substr($0,10)}
  /^branch /{if (substr($0,8)==b) {print wt; exit}}')

if [ -n "$WORKTREE_DIR" ]; then
  echo "Reusing existing worktree: $WORKTREE_DIR"
  cd "$WORKTREE_DIR"
  git fetch origin "$BRANCH"
  git reset --hard "origin/$BRANCH"
else
  WORKTREE_DIR="$(dirname "$REPO_ROOT")/voxelheim-pr-<pr-number>"
  git fetch origin "$BRANCH"
  git worktree add "$WORKTREE_DIR" "origin/$BRANCH"
  cd "$WORKTREE_DIR"
  git checkout -B "$BRANCH" "origin/$BRANCH"
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

# Does the PR touch pipeline code? CI runs a separate job for that. The five prefixes
# mirror the `helpers` grep in ci.yml's detect job — the skill directories are in it
# because agent-skills-sync.test.sh guards them and lives in the job it selects.
TOUCHES_SCRIPTS=$(gh pr view <pr-number> --json files --jq '
  [.files[].path | select(startswith("scripts/") or startswith(".github/")
    or startswith(".claude/") or startswith(".agents/") or startswith(".opencode/"))] | length')

echo "Workspaces: ${WORKSPACES:-<none>} | pipeline files: $TOUCHES_SCRIPTS | Worktree: $WORKTREE_DIR"
```

Guard: an empty `WORKSPACES` with a non-zero `TOUCHES_SCRIPTS` is normal for pipeline PRs — run only the helper suite. If **both** are empty the PR has no gate to run, which is legitimate for `docs/` or root-markdown-only changes; say so and move on rather than inventing a gate. A skill-directory edit is **not** in that category: `.claude/`, `.agents/` and `.opencode/` select the helper suite, because `agent-skills-sync.test.sh` is the one test that catches an adapter left stale. Anything else with no detected workspace is suspicious — report it and ask the user before proceeding.

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
| `scripts/`, `.github/`, `.claude/`, `.agents/` or `.opencode/` touched | the full `scripts/test/*.test.sh` suite (glob it — never retype the list) plus `python3 .github/scripts/test_deepseek_review.py`; run them under a failure flag so a red test is the block's exit status, as `/dev-issue` Step 6 spells out |

Formatting is the gate most often skipped and the one that most often reddens CI. It is not optional.

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

Conditions 5 and 6 exist because a **conflicting PR runs zero Actions checks** — with nothing red, a naive "is CI failing?" read calls that green. If `pr-status` reports a conflict, stop and rebase before anything else; no amount of pushing will produce a check run until the conflict is gone.

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

3. **Reply to every thread**, including ones you did not act on:

   ```bash
   gh api "repos/$REPO/pulls/<pr-number>/comments/<comment-databaseId>/replies" \
     --field body="<response text>"
   ```

4. **Resolve** only threads where the fix landed or the point is genuinely addressed:

   ```bash
   gh api graphql -f query='mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{id isResolved}}}' -f id="<THREAD_ID>"
   ```

   A thread whose suggestion you **rejected** may be resolved too (#217) — but only after a reply
   that states the evidence, because that reply is the whole of the audit trail once no human is
   required to read it. Never resolve a thread you have not answered.

5. **Findings in the review body** (general comments) create no thread and cannot be resolved. Address them in code where valid, then tell the user to read the review and apply the `DEEPSEEK_REVIEW_READ` label — that acknowledgement is a human-only action; never apply it yourself.

#### 4d — Fix CI failures

```bash
bash scripts/gh-automation.sh pr-status <pr-number>
```

If CI is failing, read the run logs (`gh run view <run-id> --log-failed`), fix the code, then run the Step 2 gate command for **every** affected workspace before pushing.

If the failure is infrastructure (runner outage, registry timeout, expired token), report it and stop. Do not paper over it with code changes.

#### 4e — Commit and Push

```bash
cd "$WORKTREE_DIR"
git add -A
git commit -m "fix: address DeepSeek review round $(( ${BOT_REVIEW_COUNT:-0} + 1 )) on PR #<pr-number>"
git pull --rebase origin "$BRANCH"
git push origin HEAD
```

**NEVER run `git add` / `git commit` / `git push` from the main repo directory.** Always from `$WORKTREE_DIR`.

With the round budget spent, this push does **not** trigger another review — pushes are skipped once the cap is reached, and a one-time notice is posted as an issue comment. If another pass is genuinely wanted:

```bash
bash scripts/gh-automation.sh pr-deepseek-force-review <pr-number> [ref]
```

`ref` defaults to `develop` and must be a branch carrying the bypass, because the dispatched run executes that ref's workflow definition.

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
├── Body finds:  ✅ none unread (or: N awaiting your DEEPSEEK_REVIEW_READ)
├── Label:       READY TO MERGE
└── Status:      Ready for manual review and merge
```

Report each line from what `pr-status` actually printed. Remind the user: "PR is ready for your review. Merge when satisfied."

**DO NOT auto-merge.** The pipeline only labels; the human is the merge gate.

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
  `main` — human-only. Merging into `develop` is authorized (#217). Read the pull-request body
  before you do: an ordering stated against another PR binds whoever merges, and the frozen rule
  cannot see it (#214 and #215 were each `ready_to_merge: true`, and merging one alone broke
  `develop` at runtime with nothing turning red).
- Never force-push or rebase without explicit user instruction (the `git pull --rebase` of your own feature branch in 4e is the sanctioned exception).
- Always run quality gates locally before pushing fixes.
- Never apply `DEEPSEEK_REVIEW_READ` or `NO_DEEPSEEK_REVIEW` yourself — both are human-only acknowledgements.

## Reference

- Shared helpers: `scripts/gh-automation.sh` (`pr-status`, `pr-deepseek-rounds`, `pr-deepseek-force-review`)
- DeepSeek reviewer: `.github/workflows/deepseek-pr-review.yml` / `.github/scripts/deepseek_review.py`
- Passive monitor: `.github/workflows/pr-labeler.yml`
- CI definition (source of truth for gates): `.github/workflows/ci.yml`
- Branch protection: `.github/branch-protection.md`
