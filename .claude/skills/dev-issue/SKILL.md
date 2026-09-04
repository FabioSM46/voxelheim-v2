---
name: dev-issue
description: Implements GitHub issues end-to-end. Use to turn an issue into a merge-ready PR.
argument-hint: <issue-number> [--base <branch>]
allowed-tools: Bash(gh *) Bash(git *) Bash(go *) Bash(gofmt *) Bash(cargo *) Bash(flatc *) Bash(bash *) Bash(cd *) Bash(mkdir *) Bash(mktemp *) Bash(ls *) Bash(rm *) Bash(cp *) Bash(mv *) Bash(cat *) Bash(xargs *) Bash(sed *) Bash(awk *) Bash(tr *) Bash(head *) Bash(tail *) Bash(paste *) Bash(find *) Bash(rg *) Bash(date *) Bash(dirname *) Bash(echo *) Bash(set *) Bash(source *) Bash(export *) Bash(jq *)
---

# dev-issue — Issue to Implementation Skill

Triggers: `/dev-issue <issue-number>` or `/dev-issue <issue-url>`, optionally followed by
`--base <branch>` when an orchestrator is assembling work on a non-main feature branch.

## Purpose

Takes a GitHub issue and drives it from requirements to PR — statelessly. Exits after opening the
PR and cleaning up. Does NOT monitor the PR; passive monitoring is handled by `pr-labeler.yml`
(event-driven plus a six-hour sweep), while `/process-pr` owns active remediation.

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

### Step 3a: Resolve the PR Base

Set `BASE_BRANCH=develop` unless the invocation explicitly supplies `--base <branch>`. A base
override is used by `/develop-iteration` for feature branches and their sub-branches; it changes
both the branch point and the pull request target.

Verify that the selected remote branch exists before creating the worktree. Refuse an empty base
and refuse `main`: this skill never opens or retargets a pull request to `main`. When reusing an
existing branch or PR, verify its actual base matches the requested base rather than silently
retargeting it.

### Step 4: Create Worktree and Branch

Branches are created from the selected `BASE_BRANCH` (`develop` by default).

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
BASE_BRANCH="<develop-or-explicit-base>"
[ -n "$BASE_BRANCH" ] && [ "$BASE_BRANCH" != "main" ] || { echo "Refusing PR base: $BASE_BRANCH"; exit 1; }
git fetch origin "$BASE_BRANCH"

# Reuse an existing worktree for this branch if one is already checked out.
EXISTING=$(git worktree list --porcelain | awk -v b="refs/heads/$BRANCH" '
  /^worktree /{wt=substr($0,10)}
  /^branch /{if (substr($0,8)==b) {print wt; exit}}')

if [ -n "$EXISTING" ]; then
  echo "Reusing existing worktree: $EXISTING"
  WORKTREE_DIR="$EXISTING"
  cd "$WORKTREE_DIR"
else
  # Three states, not two: the branch can exist with no worktree. Step 10's
  # `git worktree remove` deletes the worktree and keeps the branch (it holds the
  # PR's commits), so any re-run or retry lands here. `worktree add -b` would die
  # with "A branch named '<branch>' already exists" and take the whole run with it
  # under `set -e`.
  if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
    echo "Branch $BRANCH exists with no worktree — checking it out"
    git worktree add "$WORKTREE_DIR" "$BRANCH"
  else
    git worktree add -b "$BRANCH" "$WORKTREE_DIR" "origin/$BASE_BRANCH"
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

**Decide first how many pull requests this issue is — before any code exists, not after.**
Step 7 measures the finished diff against `DEEPSEEK_MAX_DIFF_CHARS`, and a measurement is a check
rather than a decision: an issue implemented whole and split afterwards is split at its most
expensive moment, when every seam has to be cut back out of finished work.

**The answer is a count, not a coin flip.** This step used to ask "one pull request or two", and
that framing was itself a defect: Iteration 50 produced a four-part issue and a five-part one, and
both agents wrote them up as deviations from the skill because the skill had given them two. An
issue that needs five parts needs five. Say the number, say where each seam is, and open them in
order.

### Estimate before you write, with the table rather than with intuition

The instruction above is not new and it did not work: in Iteration 50 all three issues were
implemented whole and split after measuring — 95,000, 74,073 and 84,185 characters — by three
agents who had each read this paragraph. **The reason is worth stating, because it is a property of
instructions and not of those agents**: Step 5 asked for an estimate and gave no method, while Step
7 gives an exact command. An instruction to guess loses to an instruction to measure, every time,
and it loses *silently* — the guess is never written down, so nothing looks skipped.

So estimate from the issue, using what this repository has actually measured:

| What the issue looks like | Measured | Parts |
| --- | --- | --- |
| One workspace, one or two new files, no UI | 15,000–35,000 | 1 |
| One workspace, a new module plus its tests | 45,000–60,000 | 2 |
| One workspace, a module *and* a settings/UI surface | 74,000–84,000 | 3+ |
| Two or more workspaces (`schemas` + `server` + `client`) | 95,000+ | 4+ |

Two calibration points behind the bottom rows, both from Iteration 50: **#851** named eight new
files in one workspace and came to 74,073 — five parts; **#850** touched `schemas`, `server` and
`client` and came to about 95,000 — four parts.

**The Parts column is not the estimate divided by the cap, and reading it that way is the mistake
this paragraph exists to stop.** 74,073 over 45,000 is 1.6, and #851 took five. Two reasons, both
structural rather than accidental:

- **Seams are discrete.** You cut where the code already draws a boundary, and those boundaries fall
  where they fall — not at even fractions of a total. #851's parts came out at roughly 16k, 44k,
  49k, 36k and 42k because that is where its module edges are. A part well under the cap is normal
  and is not waste.
- **An estimate made before writing is systematically low**, and by more than a rounding. #851 was
  estimated whole at 74,073; its fifth part *alone* measured 80,534 once written. Implementation
  finds work the acceptance criteria did not name, and review fixes then grow a branch again (#901
  went 44,973 → 48,682 answering three findings).

So the arithmetic is a floor on the count, never the count: **estimate ÷ 45,000, rounded up, then
add one for what you have not thought of, then let the seams decide the final number.** Aim each
part at roughly 40,000 rather than 45,000, because the cap is a truncation point and not a target.

**Two multipliers the file count hides.** Test-heavy changes run about two-thirds tests in this
repository, so a module with real coverage is roughly three times its production code. And a field
added to a type that is constructed by literal costs every construction site: `SessionParams` has
45 of them, which was 14,000 characters before any behaviour was written.

**The seam is a boundary the code already draws**: the wire and its consumer, the mechanism and its
callers, the description and its renderer, a decision and the wiring that carries it. Never a
character count — a split made to hit a number leaves parts that each read as an excerpt of
something else. Each part must build, pass its gates and stand as a reviewable change on its own;
Step 7 says what to do when they cannot, and the answer there is to ask rather than to open pieces
that do not compile.

**In a binary crate, "stands on its own" has a compiler meaning**: a `pub` item reachable only from
`#[cfg(test)]` is `dead_code` under `-D warnings`, so a seam that leaves a producer without its
consumer does not build. Either move the seam, or carry `#[allow(dead_code)]` with a comment naming
the part that consumes it — house style here, as `net/codec.rs` does for encoders that ship before
their callers. Say which you did and why.

**The rule that actually catches people**: acceptance criteria that touch both a server workspace
and a client one, or that name more than a handful of new files, are not one pull request at this
cap. Iteration 30 was the first iteration run entirely under 45,000 and four of its seven pull
requests went over it anyway; Iteration 50 turned three issues into ten.

**Know what the ordering costs — and know that it is much smaller than it was.** A later part used
to be unopenable until the one before it had merged, because `ci.yml` only ran for pull requests
targeting `main` or `develop`: based on an earlier part's branch it got no CI at all, and based on
`develop` it carried that part's commits and went straight back over the cap. Since #903 CI and the
stable `ci-gate` run for a pull request targeting **any** base, and `/dev-issue` takes an explicit
`--base`, so a later part is branched from the earlier one and opened against it. Its three-dot diff
then measures that part alone, which is the number the cap is about, and nothing waits.

What remains is an ordering, not a blockage: **say in every part's description which part it is,
how many there are, its exact base, and the direction in which the stack must collapse.** A PR
targeting an earlier part normally merges *into that part before the earlier part merges upward*;
the deepest ready leaf lands first. Saying merely "after part N" is ambiguous about whether it
means implementation dependency or GitHub merge direction, and the frozen rule cannot infer either.

The replay is still there only for a deliberately parent-first stack: a part branched from a
*merged* earlier part, or one you choose to base on `BASE_BRANCH` directly, needs
`git rebase --onto "origin/$BASE_BRANCH" <old-base>` after that squash merge before it will push a
clean diff. That rewrites history and therefore requires explicit user authorization; the
iteration orchestrator defaults to leaf-first non-main merges instead. Iteration 30 paid that cost
twice, on #455 and #457; Iteration 50 paid it seven times — and that iteration is why the
feature-base support exists.

**Write the estimate down in the pull request body**, next to the measurement Step 7 produces. An
estimate nobody records is an estimate nobody can be shown to have skipped — which is exactly how
this instruction went unfollowed three times while being the first sentence of this step.

Then implement:

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
source (legacy PR 178). **Read the paths before you believe the findings** — the tell is that they sit
outside the tree you pointed the linter at, in a package your diff never touched.

This is the more dangerous of the two traps, and the direction each fails in is why it is worth
the paragraph. A binary that will not start looks broken, so nobody acts on it; this looks like
a genuine lint failure in code you did not write, which is the state most likely to send you
"fixing" `errcheck` in a file you cannot open. Clear the cache and re-run — the fix is to make
the verdict true, never to quieten it, and not to paste a `cache clean` into the table below
either, since the cache is what keeps the gate cheap enough to run every time. CI never sees
this: every run starts on a clean runner holding no other checkout's cache.

Run from within the worktree, for **every** workspace the diff touches (determine them from
`git diff --name-only "origin/$BASE_BRANCH"...HEAD`):

| Workspace | Gate command |
| --------- | ------------ |
| `server/` | `cd server && test -z "$(gofmt -l .)" && go vet ./... && golangci-lint run && go build ./... && GOARCH=386 go build ./... && GOARCH=arm go build ./... && go test ./...` |
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
in one session, to two agents who had not read each other's transcripts (legacy PR 195). It is silent
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

### Step 7: Commit, Then Measure What the Reviewer Will Be Asked to Read

**A pull request can be too big to review, and the reviewer cannot tell you so in advance.**
DeepSeek emits its chain of thought into the same output budget the verdict has to fit in, so a
diff that reasons hard enough runs the budget out and returns nothing — half an hour, a full API
spend, a red `review` job, and no statement anywhere that the size was the problem. **That is not
monotonic in size**: 72,350 characters came back with a verdict and 60,863 did not (#491), because
what binds is how hard the model reasons about *that* content. Those measurements used `max`;
the current `high` effort trades some reasoning depth for lower latency and a lower risk of
exhausting the shared reasoning/verdict budget, without changing its 384,000-token ceiling.
`DEEPSEEK_MAX_DIFF_CHARS` is
**45,000** characters, set from the worst ratio yet measured; over it the diff is truncated and
every unread file is injected as a finding, which blocks the pull request until every gap has been
read and publicly disposed of. A human may acknowledge that audit; `/process-pr` may do so only
through its evidence-backed, dated acknowledgement sequence. Neither outcome is one to open a PR
into deliberately.

**This step verifies the decision Step 5 already took; it is not where that decision belongs.** If
the estimate there was that this issue is a single pull request, this is where the estimate meets
the actual bytes.

**Commit first, then measure.** `git diff "origin/$BASE_BRANCH"...HEAD` compares *commits*: run it on an
uncommitted tree and it answers for a branch that has not changed, which is `0` however much work
is sitting in the working directory. A guard that reports `0` is not a guard, and it is silent —
exactly the shape of failure this step exists to remove. Committing costs nothing here, because
what must not have happened yet is the **pull request**, not the commit.

```bash
git add -A
git commit -m "<conventional-commit-type>: <concise description>

Implements #<issue-number>

- <bullet point of key change>
- <bullet point of key change>"

# What the reviewer actually sees: generated code and lockfiles are excluded by name,
# so exclude them here too or the number is not the one that matters. `wc -m` and not
# `wc -c`, because the cap is characters and the reviewer measures `len(diff)` in code
# points — bytes overcount every em-dash in this repository's prose.
#
# Two pathspecs per lockfile, and that is not redundancy. A git pathspec without
# `:(glob)` matches the path from the root, so a bare `Cargo.lock` matches ONLY a
# top-level one — and neither lockfile in this repository is top-level. The reviewer
# matches on the *basename* at any depth (`is_generated_path` in
# .github/scripts/deepseek_review.py), so the pair is what mirrors it.
# `scripts/test/diff-measure-parity.test.sh` pins this list against that function.
REVIEWABLE=$(git diff "origin/$BASE_BRANCH"...HEAD -- . \
  ':(exclude)gen/*' ':(exclude)*/gen/*' ':(exclude)*_generated.*' \
  ':(exclude)Cargo.lock' ':(exclude)*/Cargo.lock' \
  ':(exclude)go.sum' ':(exclude)*/go.sum' | wc -m)
echo "reviewable diff: ${REVIEWABLE} characters (cap 45,000)"
```

**That list was wrong for as long as it existed, and the way it was wrong is the reason
it is now pinned by a test.** It excluded `Cargo.lock` and `go.sum` as bare pathspecs,
which match a top-level file and nothing else — while this repository keeps them at
`client/Cargo.lock` and `server/go.sum`. So the recipe excluded **neither**, and every
measurement of a pull request that touched a lockfile was too large: `client/Cargo.lock`
alone was 5264 lines on legacy PR 15. A number that is too large is the quiet direction
to be wrong in — it never opens an oversized pull request, it splits a change that did
not need splitting, and the split costs the serialisation Step 5 describes. It was found
on #851, where a 16,140-character part measured 30,000 and a reviewer had to check the
pathspec by hand to find out why.

The lesson is the one this repository keeps relearning: **a number somebody makes a
decision from is an output, and outputs are pinned by tests here.** This one could not
look wrong, because a diff size has no expected value to compare against.

**When Step 5 split the issue, mind what the three-dot diff measures against.** It measures against
whatever base the branch was created from, so measure against the base this part is actually opened
on: `git diff "origin/$BASE_BRANCH"...HEAD` where `BASE_BRANCH` is that base. A part branched from an
earlier part and opened against it measures that part alone — which is the number the cap is about.
The trap is measuring a later part against `develop` while the earlier part is unmerged: the merge
base is then the earlier part's base, the diff carries its commits, and it reads as over the cap
however correctly sized the part is. That number is not the one
the cap is about, and the paragraph that follows does not apply to it.

**And if you ever do need to change a pull request's base, change it before you push.** A base
change fires `pull_request` with action `edited`, which is not among the default activity types, so
retargeting on its own re-triggers nothing: the pull request sits with whatever checks the old base
produced. Retarget **first**, then push, so the push is the event that runs CI. Recovering after the
fact takes a close-and-reopen to emit `reopened` — which is what #902 cost, back when a feature base
got no CI run at all and retargeting to `develop` was the only way to obtain `ci-gate`. #903 removed
that reason; the ordering rule outlived it, because it is about GitHub's events rather than about
which bases are allowed.

**If it is over the cap, the estimate was wrong and the work still has to be split — before
opening anything.** Not after: a PR that exists is a PR whose review has already been attempted,
and unpicking one costs more than staging the parts in the first place. Split along the seam Step 5
describes — a boundary the code already draws, never a character count — so each part is a change
somebody can review as a whole and each stands on its own. **There is no limit of two**: take as
many parts as the work needs, and say which part is which and how many there are in every
description. Open each on the part before it rather than waiting for that one to merge — Step 5 says
why that works now and what it still costs — and state that leaf PRs merge inward before their base
branch's PR merges upward. If the intended topology is parent-first instead, stop for the explicit
history-rewrite authorization that topology requires.

Also record what Step 5 estimated beside what this measured. Two numbers make the estimate
falsifiable; one makes it decorative, and this table is calibrated from the pairs.

If the parts cannot each stand alone, say so and ask the user rather than splitting into pieces
that do not compile. A branch that does not build is worse than a review that has to be split
across two rounds.

Verify each part before opening it — the same measurement, on the branch you are about to push.

### Step 8: Push and Open PR

```bash
# From within the worktree, with Step 7's commit already made:
bash scripts/check-publication-privacy.sh
bash scripts/check-commit-privacy.sh "origin/$BASE_BRANCH" HEAD
git push -u origin HEAD

PR_BODY=$(mktemp)
trap 'rm -f "$PR_BODY"' EXIT
cat >"$PR_BODY" <<'EOF'
## Summary

Closes #<issue-number>

### Changes
- <list key changes>

### Review Size
- Estimated before implementation: <estimate and reasoning>
- Measured reviewable diff: <REVIEWABLE> characters

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

bash scripts/check-body-privacy.sh <"$PR_BODY"
gh pr create \
  --base "$BASE_BRANCH" \
  --title "<issue-title>" \
  --body-file "$PR_BODY"
```

**If the pull request's title or body must be corrected, never use `gh pr edit`.** On `gh`
2.45.0 its Projects-classic pre-fetch fails before any flag is applied (#206). Use
`bash scripts/gh-automation.sh pr-edit <pr> --title "<title>" --body-file <path>`; the
helper writes through REST and reads both requested fields back before reporting success.

Only tick a box you actually ran. An unticked box with a one-line reason is useful; a ticked box that is not true poisons every later review.

**PR target**: Default is `develop`; an explicit `--base` may target another feature branch.
Never open or retarget a PR to `main`, never push directly to `main`, and never merge a pull request
into `main`. Merging into any non-main base is authorized, but **not from this skill**:
`/dev-issue` is stateless and exits at PR creation, before CI has reported anything. Merging is a
decision for a context that has read the result.

### Step 9: Exit

Report to the user:

- Branch name
- PR number and URL
- Worktree directory location
- Confirmation that the DeepSeek review will be triggered automatically by `deepseek-pr-review.yml`

**DO NOT** wait for CI, poll for reviews, or attempt to merge. The `/dev-issue` skill is stateless — it exits after PR creation.

### Step 10: Cleanup — Remove Worktree

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
