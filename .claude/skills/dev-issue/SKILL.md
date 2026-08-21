---
name: dev-issue
description: Implements GitHub issues end-to-end. Use to turn an issue into a merge-ready PR.
argument-hint: <issue-number>
allowed-tools: Bash(gh *) Bash(git *) Bash(go *) Bash(gofmt *) Bash(cargo *) Bash(flatc *) Bash(bash *) Bash(cd *) Bash(mkdir *) Bash(ls *) Bash(rm *) Bash(cp *) Bash(mv *) Bash(cat *) Bash(xargs *) Bash(sed *) Bash(awk *) Bash(tr *) Bash(head *) Bash(tail *) Bash(paste *) Bash(find *) Bash(rg *) Bash(date *) Bash(dirname *) Bash(echo *) Bash(set *) Bash(source *) Bash(export *) Bash(jq *)
---

# dev-issue — Issue to Implementation Skill

Triggers: `/dev-issue <issue-number>` or `/dev-issue <issue-url>`

## Purpose

Takes a GitHub issue and drives it from requirements to PR — statelessly. Exits after opening the PR and cleaning up. Does NOT monitor the PR; monitoring is handled by `pr-labeler.yml` (event-driven: fires when a PR's CI run completes, plus a six-hour sweep) or `/process-pr` (manual force-cycle).

Uses `git worktree` for branch isolation, enabling parallel issue work without blocking the main working directory.

## Workflow

### Step 1: Fetch the Issue

Use `gh issue view <number> --json title,body,labels` to read the issue. Parse these fields from the issue body:

| Field | Source | Required | Maps To |
|-------|--------|----------|---------|
| Workspace | `### Workspace` → dropdown | Yes | Working directory |
| Type | `### Type` → dropdown | Yes* | Branch prefix (*bug reports have no Type field; derive from the `bug` label) |
| Priority | `### Priority` → dropdown | Yes* | Effort tuning (*same exception) |
| User Story | `### User Story` | Yes* | Implementation goal (*same exception) |
| Acceptance Criteria | `### Acceptance Criteria` | Yes* | Verification checklist (*same exception) |
| Technical Context | `### Technical Context` | Yes | Implementation guide |
| Out of Scope | `### Out of Scope` | Yes | Implementation boundaries |
| Code Pointers | `### Code Pointers` | No | Files to read first |
| Dependencies | `### Dependencies` | No | Blocking items |
| Test Strategy | `### Test Strategy` | No | What to test |

Required-ness mirrors `.github/ISSUE_TEMPLATE/feature_request.yml`. Bug reports (`bug_report.yml`) carry `What happened? / What should have happened? / Steps to reproduce` instead of Type/Priority/User Story/Acceptance Criteria — treat the reproduction steps as the acceptance criteria.

If a required field is missing, do not guess. Say which field is absent and ask the user whether to proceed anyway.

### Step 2: Validate Workspace

Map the workspace value to the correct working directory:

| Issue Value | Directory | Local AGENTS.md |
|-------------|-----------|-----------------|
| `server (Go Backend)` | `server/` | `server/AGENTS.md` |
| `client (Rust Client)` | `client/` | `client/AGENTS.md` |
| `schemas (FlatBuffers Contracts)` | `schemas/` | `schemas/AGENTS.md` |
| `shared (Cross-cutting)` | root | every workspace AGENTS.md that exists |

Local AGENTS.md files are created with each workspace's scaffold; read each one that exists, and do not treat its absence as an error before the workspace is scaffolded.

**Defensive validation**: If the workspace value does not match one of the values above, DO NOT proceed. Prompt the user: "I don't recognize the workspace '[value]'. Accepted values: server (Go Backend), client (Rust Client), schemas (FlatBuffers Contracts), shared (Cross-cutting). Which workspace should I use?"

### Step 3: Read Context

Before writing any code:

1. Read the root `AGENTS.md` for shared conventions and the pipeline contract
2. Read the relevant local `AGENTS.md` for the workspace (if it exists)
3. Read the **Code Pointers** from the issue — these are exact `file:line` references the issue author deemed critical. Open each one.
4. Read the **Out of Scope** section — commit it to memory. Do not touch anything listed there.
5. If the change touches `schemas/*.fbs`, remember the contract rule: a schema change rebuilds BOTH sides. Generated bindings live under `gen/` directories (plus flatc's `*_generated.rs` naming on the Rust side) and are committed — regenerate them, never hand-edit them, and expect the review bot to exclude them from the diff it reads.
6. Use `rg --files` and `rg` to find existing patterns relevant to the change

### Step 4: Create Worktree and Branch

All branches are created from `develop`.

**Branch naming:**

```
Type mapping (from the Type dropdown, or from labels if absent):
  feature     → feature/
  enhancement → feature/
  refactor    → refactor/
  bug label   → fix/

Slug: issue title → lowercase → replace non-alphanumeric with hyphens
      → collapse consecutive hyphens → strip leading/trailing hyphens
      → take first 5 words max

Result: <prefix><issue-number>-<short-slug>

Examples:
  feature/42-add-rune-key-portals
  fix/43-fix-chunk-seam-lighting
  refactor/44-extract-mesh-builder
```

**Worktree workflow:**

The worktree path is derived from the repo root, never from `$PWD`. A relative `../voxelheim-v2-issue-N` resolves against the current directory, so a single `cd server` earlier in the run silently creates the worktree *inside* the repo.

```bash
set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

SLUG=$(echo "<issue-title>" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]/-/g' | sed 's/-\+/-/g' | sed 's/^-//;s/-$//' | tr '-' '\n' | head -5 | paste -sd '-')
BRANCH="<type-prefix><issue-number>-${SLUG}"
WORKTREE_DIR="$(dirname "$REPO_ROOT")/voxelheim-v2-issue-<issue-number>"

# Reuse an existing worktree for this branch if one is already checked out.
EXISTING=$(git worktree list --porcelain | awk -v b="refs/heads/$BRANCH" '
  /^worktree /{wt=substr($0,10)}
  /^branch /{if (substr($0,8)==b) {print wt; exit}}')

if [ -n "$EXISTING" ]; then
  echo "Reusing existing worktree: $EXISTING"
  WORKTREE_DIR="$EXISTING"
  cd "$WORKTREE_DIR"
else
  git fetch origin develop
  # Three states, not two: the branch can exist with no worktree. Step 9's
  # `git worktree remove` deletes the worktree and keeps the branch (it holds the
  # PR's commits), so any re-run or retry lands here. `worktree add -b` would die
  # with "A branch named '<branch>' already exists" and take the whole run with it
  # under `set -e`.
  if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
    echo "Branch $BRANCH exists with no worktree — checking it out"
    git worktree add "$WORKTREE_DIR" "$BRANCH"
  else
    git worktree add -b "$BRANCH" "$WORKTREE_DIR" origin/develop
  fi
  cd "$WORKTREE_DIR"
fi

# Fetch dependencies for the workspaces that exist at this ref.
[ -f server/go.mod ] && (cd server && go mod download)
[ -f client/Cargo.toml ] && (cd client && cargo fetch)
```

The worktree directory is `<parent-of-repo-root>/voxelheim-v2-issue-<number>`:

- Main repo: `<workspace>/voxelheim-v2`
- Worktree for issue #42: `<workspace>/voxelheim-v2-issue-42`

**Verify before editing anything:**

```bash
[ "$(git rev-parse --show-toplevel)" = "$WORKTREE_DIR" ] || { echo "NOT in worktree — abort"; exit 1; }
```

Every file edit and every git command from here on runs inside `$WORKTREE_DIR`. If this check fails, stop and re-enter the worktree — do not edit the main working directory.

### Step 5: Implement

1. **Honor Out of Scope strictly** — if a change touches anything listed in Out of Scope, stop and reassess
2. Follow existing code patterns in the workspace
3. Honor all workspace-specific rules from the local AGENTS.md
4. **The server is authoritative** — the client renders and predicts, it never decides. Any gameplay rule implemented client-side only is a bug.
5. Never introduce new dependencies (Go modules, crates) without explicit instruction
6. No hardcoded secrets, no debug prints left in production paths (`println!`/`fmt.Println` in hot loops, `dbg!`), no player data in fixtures
7. Keep changes surgical — edit existing files when possible, create new files only when necessary
8. Never hand-edit anything under a `gen/` directory or a `*_generated.rs` file — regenerate from `schemas/` instead

### Step 6: Quality Gates (MANDATORY — Do Not Skip)

These commands mirror `.github/workflows/ci.yml` step for step, and `scripts/test/gate-tables.test.sh` is what keeps that true — the claim used to be prose, and it was wrong for every server PR between the Go scaffold and the test. Anything CI runs that you skip here becomes a red PR and a wasted DeepSeek review round — the round budget is 1.

`golangci-lint` is a real gate, not a nice-to-have: ci.yml pins both the action and the linter version, so read the version from there rather than trusting whatever is on your PATH. An older local binary can fail to start on a newer Go toolchain, which looks like a broken repository and is not one.

**A finding whose path is not inside your worktree is a cache artifact, not a lint failure.**
golangci-lint caches to a single per-user directory — `golangci-lint cache status` prints it —
shared by every worktree and every checkout on the machine, while this pipeline creates and
destroys worktrees of the same Go module constantly: step 9 below, and `/process-pr`'s own
cleanup. Entries keyed to a path that no longer exists outlive it, so a run in a brand-new
worktree can report ordinary `errcheck` findings against `../../voxelheim-pr-165/server/...`
and exit 1, with the linter's own `no such file or directory` warnings sitting above them.
`golangci-lint cache clean` and a re-run in the same worktree answers `0 issues` on identical
source (#178). **Read the paths before you believe the findings** — the tell is that they sit
outside the tree you pointed the linter at, in a package your diff never touched.

This is the more dangerous of the two traps, and the direction each fails in is why it is worth
the paragraph. A binary that will not start looks broken, so nobody acts on it; this looks like
a genuine lint failure in code you did not write, which is the state most likely to send you
"fixing" `errcheck` in a file you cannot open. Clear the cache and re-run — the fix is to make
the verdict true, never to quieten it, and not to paste a `cache clean` into the table below
either, since the cache is what keeps the gate cheap enough to run every time. CI never sees
this: every run starts on a clean runner holding no other checkout's cache.

Run from within the worktree, for **every** workspace the diff touches (determine them from `git diff --name-only origin/develop...HEAD`):

| Workspace | Gate command |
| --------- | ------------ |
| `server/` | `cd server && test -z "$(gofmt -l .)" && go vet ./... && golangci-lint run && go build ./... && go test ./...` |
| `client/` | `cd client && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo build --workspace --locked && cargo test --workspace --locked` |
| `schemas/` | `bash scripts/check-schemas.sh`, then regenerate the committed bindings and run the **server and client gates too** — a contract change rebuilds both consumers (this mirrors `scripts/changed-areas.sh`, which fans a schemas change out to all three CI jobs) |

**A gate's verdict is its exit status, and a pipeline's exit status belongs to its last
command.** `golangci-lint run 2>&1 | tail -5 && echo OK` prints `OK` over a failing run: the
linter exits 1, `tail` exits 0, and `&&` reads the pager. The chains above are written with
`&&` throughout for that reason — run them as written. When the output is too noisy to read,
keep the status and trim the text separately:

```bash
status=0
golangci-lint run > /tmp/lint.txt 2>&1 || status=$?
tail -20 /tmp/lint.txt
[ "$status" -eq 0 ]   # ← the verdict, read from the gate rather than from the pager
```

**`|| status=$?` rather than `; status=$?`, and the difference only shows under `set -e`.**
A bare failing command under `set -e` ends the shell where it stands, so the `tail` never runs
and the failure is reported with none of the output that would explain it — the one thing this
form exists to keep. Putting the command in an `||` list is what exempts it, which is the same
mechanism the helper loop below relies on. Step 4's worktree recipe opens with
`set -euo pipefail` and, as that loop's own note says, `set -e` lingers in an interactive
shell besides, so assume it is on rather than checking.

This is the helper loop's defect one layer up, and it is not hypothetical: it happened twice
in one session, to two agents who had not read each other's transcripts (#195). It is silent
by construction, which is what makes a paragraph the only defence — a piped gate that passed
prints exactly what a piped gate that failed prints, so nothing about the transcript looks
wrong. `head` and `tail` stay in `allowed-tools` because step 4's slug recipe needs them; what
must not happen is a gate command on the left of that pipe.

**If the diff touches `scripts/`, `.github/`, or any of the three skill directories
(`.claude/`, `.agents/`, `.opencode/`)**, CI also runs the automation helper suite. Run it too:

```bash
failed=0
for t in scripts/test/*.test.sh; do
  printf '== %s\n' "$t"
  bash "$t" || { echo "FAILED: $t"; failed=1; }
done
python3 .github/scripts/test_deepseek_review.py || failed=1
[ "$failed" -eq 0 ]  # ← the block's exit status IS the gate's verdict; never drop this line
```

**The glob is the instruction, and enumerating the files here instead is the mistake it
replaces.** This block used to name a fixed set of scripts and `scripts/test/` had already
grown past it. The two it never gained — `client-ci-budget.test.sh` and
`deepseek-budget.test.sh` — are precisely the
ones a CI change is most likely to break, and the drift failed in the worst direction
available: the documented set passed locally, a PR was pushed on the strength of that, and
CI went red on a test this gate had never run. A list kept in step with a directory by hand
falls behind it, and nothing here notices when it has.

`.github/workflows/ci.yml` does enumerate them, deliberately — two of those tests assert
their own presence in that list, so a test that cares about being run says so itself. That
is a claim about what CI must execute, not a set for this gate to copy: the glob is always
a superset of it, which is the only relationship this gate needs.

**The last line is load-bearing, and the flag is why the loop can report at all.** Written as
`bash "$t" || { echo "FAILED: $t"; break; }`, the loop exits 0 on a failing test — `echo` and
`break` both succeed, so the failure is printed and then thrown away, and the trailing
`python3` line becomes the block's exit status. A gate that prints FAILED and exits 0 is the
same defect as the enumerated list above, one layer down: it looks like it ran, and its
verdict is not what it saw. The flag also drops the `break`, deliberately — CI runs every
helper test in `scripts/test/` and reports every failure, so a local gate that stops at the
first one sends you back for a second round it could have saved. `set -e` would fix the exit
status and reintroduce the early stop, and it lingers in an interactive shell besides.

**No count of that directory appears above, and that is not an oversight.** Writing one down
is the same hand-kept mirror of a directory this block replaced, one layer further out: the
number would have been wrong the moment this pull request added its own guard test, which is
exactly how long a hand-kept count survives.

**Why the skill directories are in that list**: `agent-skills-sync.test.sh` is what keeps the
Codex and OpenCode adapters in step with `.claude/skills/`, and it lives in the `automation`
job. A PR that edits a canonical skill and forgets `scripts/sync-agent-skills.sh` therefore
has exactly one test standing between it and a stale adapter — the one that never runs unless
those prefixes select the job. `.github/workflows/ci.yml` holds the same five prefixes in its
`helpers` grep; `scripts/test/helpers-selector-docs.test.sh` pins this sentence to it, so the
two cannot drift apart silently.

A diff that touches none of the above (docs, root markdown) has no gate to run; say so rather
than inventing one. Skill-directory edits are **not** in that category — they run the helper
suite, and the sync script must be re-run and all three copies committed together.

Formatting (`gofmt`, `cargo fmt`) is the gate most often skipped and the one that most often reddens CI. It is not optional. Fix formatting with `gofmt -w .` / `cargo fmt --all`, lint with what clippy's messages say — then re-run the full gate.

**Definition of Done (verified before PR):**

- [ ] Formatting clean (`gofmt -l` empty / `cargo fmt --check`)
- [ ] Lint passes (`go vet` / `cargo clippy -D warnings`)
- [ ] Build succeeds (`go build` / `cargo build --locked`)
- [ ] All tests green (`go test` / `cargo test --locked`) — this repository starts with a clean baseline; keep it that way
- [ ] Schemas validated and bindings regenerated (when `schemas/` is touched)
- [ ] No debug prints left in production code
- [ ] No hardcoded secrets, tokens, or keys
- [ ] Server-authoritative rule honored — no gameplay decisions client-side
- [ ] Workspace-specific rules honored (local AGENTS.md)

### Step 7: Commit and Open PR

```bash
# From within the worktree:
git add -A
git commit -m "<conventional-commit-type>: <concise description>

Implements #<issue-number>

- <bullet point of key change>
- <bullet point of key change>"

git push -u origin HEAD

gh pr create \
  --base develop \
  --title "<issue-title>" \
  --body "$(cat <<'EOF'
## Summary

Closes #<issue-number>

### Changes
- <list key changes>

### Gates Run
- [x] format check (gofmt / cargo fmt)
- [x] lint (go vet / clippy)
- [x] build
- [x] tests — all green
- [x] schemas validated (if touched)

### Acceptance Criteria Verification
<copy each AC item and mark as checked or leave unchecked with explanation>

### Out of Scope (Verified)
<confirm nothing from the Out of Scope section was touched>
EOF
)"
```

Only tick a box you actually ran. An unticked box with a one-line reason is useful; a ticked box that is not true poisons every later review.

**PR target**: Default is `develop`. PRs targeting `main` are allowed for hotfixes. However, NEVER push directly to `main` and NEVER merge any PR. Merging is a human-only operation.

### Step 8: Exit

Report to the user:

- Branch name
- PR number and URL
- Worktree directory location
- Confirmation that the DeepSeek review will be triggered automatically by `deepseek-pr-review.yml`

**DO NOT** wait for CI, poll for reviews, or attempt to merge. The `/dev-issue` skill is stateless — it exits after PR creation.

### Step 9: Cleanup — Remove Worktree

After the PR is created and the user has been informed:

```bash
# Return to the main repo (derived, not hardcoded — this skill runs on more than one machine)
MAIN_REPO=$(git rev-parse --path-format=absolute --git-common-dir)
MAIN_REPO=$(dirname "$MAIN_REPO")
cd "$MAIN_REPO"

git worktree remove "$WORKTREE_DIR"
git worktree prune
```

Confirm: "Worktree removed: `$WORKTREE_DIR`"

If `git worktree remove` fails (e.g., uncommitted changes), warn the user and provide the manual cleanup command. `git worktree remove` leaves nothing behind on success — if a `voxelheim-v2-issue-*` directory survives, it was created outside the worktree machinery and should be deleted by hand.

## Reference

- Issue conventions: `docs/ISSUE_CONVENTIONS.md`
- Issue templates: `.github/ISSUE_TEMPLATE/`
- Pipeline docs: `AGENTS.md` (root)
- CI definition (source of truth for gates): `.github/workflows/ci.yml`
- Branch protection: `.github/branch-protection.md`
- Shared helpers: `scripts/gh-automation.sh`
