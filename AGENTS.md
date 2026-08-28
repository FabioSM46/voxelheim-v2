# Voxelheim — Go + Rust Monorepo

Cooperative voxel survival RPG (Minecraft × World of Warcraft) set in a dark Norse world.
Authoritative **Go** server, **Rust/Bevy** client, **FlatBuffers** network contracts — one
repository, no submodules. The full game design lives in `docs/GDD.md`.

## Repository Structure

```
voxelheim-v2/
├── AGENTS.md             # This file — global rules (authoritative for the pipeline)
├── CLAUDE.md             # Pointer here for Claude Code
├── README.md             # Project overview
├── .flatc-version        # Pinned FlatBuffers compiler release
├── server/               # Go backend (netcode, chunk generation, authoritative logic)
├── client/               # Rust client (Bevy: ECS + wgpu rendering, greedy meshing)
├── schemas/              # FlatBuffers .fbs contracts between client and server
├── docs/                 # GDD, workflow, issue conventions
├── scripts/              # gh-automation.sh, CI classifiers, schema validation + tests
├── .github/              # CI, DeepSeek review bot, PR labeler, iteration lifecycle
├── .claude/skills/       # Canonical skills for Claude Code (/dev-issue, /process-pr, /scrum-master)
├── .agents/skills/       # Generated Codex adapters ($dev-issue, $process-pr, $scrum-master)
└── .opencode/skills/     # Generated OpenCode adapters (/dev-issue, /process-pr, /scrum-master)
```

**The workspaces are scaffolded through the pipeline itself.** `server/`, `client/` and
`schemas/` may not exist yet at a given ref; every script and CI job in this repository
treats an absent workspace as "nothing to verify", never as an error. Do not "fix" that by
creating placeholder files.

## Workspace Purposes

| Workspace  | Tech Stack                    | Purpose                                                          |
| ---------- | ----------------------------- | ---------------------------------------------------------------- |
| `server/`  | Go (single module)            | Authoritative simulation: netcode, chunk generation, world state, combat resolution, persistence |
| `client/`  | Rust + Bevy (cargo workspace) | Rendering (greedy meshing on wgpu), input, prediction/interpolation, UI |
| `schemas/` | FlatBuffers (.fbs)            | The network contract. Single source of truth for every byte that crosses the wire |

### The server is authoritative — always

The client renders, predicts, and requests; it never decides. Movement validation, combat
outcomes, loot rolls, placement legality, durability loss, respawn resolution — all of it is
computed server-side. A gameplay rule that exists only client-side is a bug even when it
"works", because it is a cheat vector by construction. Client-side prediction is welcome for
feel; the server's answer always wins.

### The contract is granite

`schemas/*.fbs` is the only definition of the wire format. Both sides consume **committed,
generated bindings** — flatc output lives under `gen/` directories (Rust files additionally
carry flatc's `*_generated.rs` naming) and is regenerated, never hand-edited. A schema change
therefore fans out: `scripts/changed-areas.sh` runs the `schemas`, `server` AND `client` CI
jobs for any `schemas/**` or `.flatc-version` diff, and `/dev-issue` mirrors that locally.
The flatc release is pinned in `.flatc-version`; `scripts/check-schemas.sh` is the single
source of truth for schema validation (CI and skills both run it).

## Agent Rules — Global vs Local

Read this file when starting work anywhere in the repository. Before editing a specific
workspace, also read its local `AGENTS.md` (`server/AGENTS.md`, `client/AGENTS.md`,
`schemas/AGENTS.md`) — each is created with its workspace's scaffold and is authoritative
for workspace-specific conventions. Absence of a local AGENTS.md means the workspace is not
scaffolded yet.

### Cross-Cutting Changes

When a change touches multiple workspaces:

1. Start with the contract (`schemas/`) — types first, consumers second
2. Regenerate bindings for both sides
3. Implement the server (authoritative) half
4. Implement the client half against the server's actual behavior
5. Gate every touched workspace before opening the PR

**A new item is the cross-cutting change this repository makes most often, and it has its own
path**: `docs/ADDING_AN_ITEM.md` walks it server first, names the rule behind each step and
points at the copy that enforces it. Two things it settles before you start: item ids are
appended and never inserted, and adding an item is **not** a schema change — the registry and
the recipe table are never sent, so no `.fbs` is touched unless the item needs a genuinely new
message.

## Shared Conventions

These apply across all workspaces:

### Public repository privacy boundary

**This repository is public. Treat every tracked byte and every GitHub surface as globally
readable forever.** That includes source and documentation, commit author/committer metadata,
branch names, commit and PR messages, review comments, CI logs, artifacts and release assets.

- **Never publish a real email address.** Commits must use the GitHub-provided `noreply` address.
  Source, fixtures and commit messages may contain only GitHub `noreply` identities, addresses
  under reserved example domains, and the one vendor no-reply an agent harness appends to its
  co-author trailer by construction; never copy a real address into a test. That vendor address is
  approved **by literal address, not by domain** — a no-reply under a private host would publish an
  internal hostname, which is the thing these scans exist to catch. `is_approved_email` is the list,
  it lives in every privacy script, and `scripts/test/commit-privacy.test.sh` pins those copies to
  each other: widen what an address may be in one and the pin fails until the others agree.
- **Never publish internal machine paths.** Do not commit workstation usernames, home directories,
  mount points, checkout locations or other environment-specific paths. Documentation uses tokens
  such as `<repo-root>`, `<worktree>` and `<workspace>`. Tests use clearly synthetic fixture paths.
- **Never publish personal data.** Real names, private usernames, phone numbers, locations, account
  identifiers and contact details stay out of the repository and all GitHub discussions. The sole
  approved public identity is the project attribution handle `@FabioSM46` recorded in `NOTICE`.
- **Never publish secrets or operational identifiers.** API keys, tokens, credentials, private
  certificates, internal hostnames and non-public service endpoints belong in GitHub Secrets or a
  local ignored file, never in Git, logs, issues or PRs.
- **Never publish a generated agent session reference.** A `Claude-Session:` trailer, or a
  `claude.ai/code/session_…` URL anywhere in a message, is an account-scoped endpoint. It is the one
  non-public endpoint a machine can recognise, and it recurs *because* it is generated rather than
  because anybody was careless — three of them reached `develop` before anything looked (#128).
- **A commit message is a published surface, and it is checked like one.** Two scripts divide the
  work: `bash scripts/check-publication-privacy.sh` scans tracked file content, and
  `bash scripts/check-commit-privacy.sh <base> <head>` walks the commits a branch adds and reads
  their author and committer fields *and* their message. Run both before any push. A suspected leak
  blocks publication; redact it before continuing rather than printing it into another diagnostic.
- **So is an issue or pull-request body — but that one can only be checked afterwards.** The three
  categories reach it through `scripts/check-body-privacy.sh`, driven by
  `.github/workflows/body-privacy.yml` on `issues` and `pull_request_target` (`opened`, `edited`).
  A finding gets the `needs-privacy-review` label and one comment naming the category and the line;
  nothing is closed, edited or hidden. **This check cannot prevent a leak — a body is public from
  the moment it is posted — it can only shorten how long nobody knew**, so a green run means
  "nothing is published now", never "nothing was published". It is the surface a machine writes
  most often and was the last one nothing read (#130): an agent writing a PR body is transcribing
  what it just did, and what it just did happened in a working directory with a name. Run it on a
  body before posting one:
  `gh issue view <n> --json body --jq .body | bash scripts/check-body-privacy.sh`.

- **Commits**: Never commit unless explicitly requested; conventional-commit style (`feat:`, `fix:`, `refactor:`, `docs:`, `ci:`)
- **Read Before Write**: Find existing patterns before implementing
- **No new dependencies** (Go modules, crates) without explicit instruction
- **No git submodules**: Everything is in one repository
- **Never read `.env` files** — AI agents MUST NOT read, open, or inspect any `.env` file. Use `.env.example` exclusively for understanding required environment variables. `.env` files contain real secrets and credentials.
- **NEVER push or merge to `main`** — `main` is the release branch and it stays human-only. The AI
  must never `git push origin main` and must never merge a pull request into `main`. PRs targeting
  `main` are allowed; the human performs the merge.
- **Merging into `develop` is the AI's to do** — since #217 it merges on its own judgement, and
  `READY TO MERGE` is a signal it should read rather than a precondition it must satisfy. Merge
  through **`bash scripts/gh-automation.sh pr-merge <pr>`**, which refuses a `main` base by name and
  fails closed on one it cannot read (#218). **This is an instruction, and it is worth knowing
  exactly what backs it** — see "What actually stops a merge" below, because for `develop` the
  answer is now "nothing, deliberately", and for `main` it is the instruction plus that one check on
  the path an agent is told to use.
- **Git worktree isolation** — All file edits and git operations MUST happen inside an isolated `git worktree`, never on the main working directory. Use `<parent-directory>/voxelheim-v2-issue-<number>` when the work is tied to an issue; if no issue exists, do not invent a number — use `<parent-directory>/voxelheim-v2-issue-<short-descriptive-slug>` instead (for example, `voxelheim-v2-issue-graphify`). `/process-pr` worktrees remain `voxelheim-v2-pr-<number>`. Verify you are inside the worktree before making any changes (`git rev-parse --show-toplevel`). After the PR is opened, clean up with `git worktree remove` and `git worktree prune`. Reuse an existing worktree if one already exists for the branch.
- **Never hand-edit generated code** — anything under a `gen/` directory or matching `*_generated.rs` is flatc output. Regenerate from `schemas/`; the review bot excludes those paths from the diff it reads for the same reason.

## Branch Strategy

- `main` — production-ready branch (releases)
- `develop` — shared integration branch and the default branch; **all branches start here**
- `feature/<issue>-<slug>` — features and enhancements
- `fix/<issue>-<slug>` — bug fixes
- `refactor/<issue>-<slug>` — restructuring without behavior change

For authorized work that has no issue, omit the issue number rather than inventing one and use
`<type>/<short-descriptive-slug>` (for example, `refactor/graphify-gitignore`).

## AI-Driven Development Pipeline

The repository includes an automated pipeline that drives issues from creation to
merge-ready PRs. It uses three layers:

| Layer         | Technology                            | Responsibility                                             |
| ------------- | ------------------------------------- | ---------------------------------------------------------- |
| **Skills**    | Claude, Codex and OpenCode agent skills | LLM-driven implementation, PR management, scrum ceremonies |
| **Workflows** | GitHub Actions (`.github/workflows/`) | CI checks, PR review, PR monitoring, iteration transitions |
| **Helpers**   | `scripts/gh-automation.sh`            | Shared REST + GraphQL API wrapper                          |

### Cross-Runtime Skill Synchronization

`.claude/skills/` is the canonical source for the three pipeline workflows. Codex consumes
the generated adapters in `.agents/skills/`; OpenCode consumes the generated adapters in
`.opencode/skills/`. The adapters remove runtime-specific Claude frontmatter, preserve the
same workflow, and map explicit invocations to each runtime's syntax.

When any canonical skill changes:

1. Edit only `.claude/skills/<skill>/SKILL.md`.
2. Run `bash scripts/sync-agent-skills.sh` from the repository root.
3. Run `bash scripts/test/agent-skills-sync.test.sh` and commit all three versions together.

Never hand-edit the generated `.agents/skills/*/SKILL.md` or
`.opencode/skills/*/SKILL.md` files. CI fails when a committed adapter is stale. Codex UI
metadata lives in `.agents/skills/*/agents/openai.yaml` and is generated by the same script.

### Pipeline Flow

```
Issue opened via template
        │
        ▼
/dev-issue <number> in Claude/OpenCode, or $dev-issue <number> in Codex
(the user, or an agent the user asked to run several issues in parallel)
        │
        ▼
skill: reads issue → validates workspace → reads AGENTS.md
        │
        ▼
skill: creates worktree + branch from develop → implements → gates (fmt, lint, build, test)
        │
        ▼
skill: opens PR targeting develop with structured description → EXITS (stateless)
        │
        ▼
deepseek-pr-review.yml: posts inline review comments on the PR diff ───┐
        │                                                              │
        ▼                                                              ▼ (parallel)
pr-labeler.yml (on CI completion + 6h sweep): reads ci-gate + threads ─┘
        │
    ┌───┴──────────┐
    │ Frozen rule?  │──met──→ adds READY TO MERGE label
    │ CI red?       │──yes──→ adds needs-work label
    │ otherwise     │───────→ needs-review
    └───────────────┘
        │
        ▼
User optionally runs: /process-pr (Claude/OpenCode) or $process-pr (Codex)
        │
        ▼
User reviews → merges (manual gate)
```

### Skills Reference

| Skill          | Claude / OpenCode           | Codex                       | Behavior                                         |
| -------------- | --------------------------- | --------------------------- | ------------------------------------------------ |
| `dev-issue`    | `/dev-issue <number>`       | `$dev-issue <number>`       | Stateless: implements issue → opens PR → exits   |
| `process-pr`   | `/process-pr <pr-number>`   | `$process-pr <pr-number>`   | Manual force-cycle: fix CI issues + re-run gates |
| `scrum-master` | `/scrum-master <ceremony>`  | `$scrum-master <ceremony>`  | backlog-refine, iteration-plan, feature-spec     |

#### Who may start a skill, and which gates are the real ones

All three skills are invocable by an agent as well as by a human typist. They carried
`disable-model-invocation: true` until legacy PR 48, and removing it changed less than it looks like:
three agents had already hit that refusal and done the work by hand anyway, from the issue
body and this file. The flag was not preventing the work — it was routing it through a path
with a **wider** tool set, since a skill runs under its `allowed-tools` list and a
general-purpose subagent does not. Keep those lists narrow; they are now the reason to prefer
a skill invocation over a hand-rolled agent.

**The merge side has now moved, and #217 is where.** CI still runs on the pull request whichever
hand opened it, `READY TO MERGE` is still computed by the one frozen rule, the rulesets still
reject direct pushes at the remote, and `DEEPSEEK_REVIEW_READ` and `NO_DEEPSEEK_REVIEW` are still
human-only. What changed is the merge itself: into `develop` it is the AI's call, and a thread the
AI has answered is the AI's to resolve. **The human gate was the merge, it was the gate that was
ever load-bearing, and it is gone by decision rather than by oversight** — read the residual risk
closing the next section before assuming otherwise.

Iteration ceremonies are the deliberate exception and stay human-in-the-loop — not because a
model cannot run them, but because choosing which issues go into the next iteration is the
user's call, not a mechanical step.

#### What actually stops a merge

Since #217 the answer depends on the target, and for `develop` the answer is "nothing".

**Into `develop`: nothing, and that is the decision.** The AI merges on its own judgement. The
frozen acceptance rule still computes `READY TO MERGE` and the AI should read it, but it is a
signal, not a gate — a merge that ignores it is a bad call rather than a blocked one. The branch
ruleset still applies to everyone: a pull request is required, review threads must be resolved,
and `ci-gate` must be green, so nothing lands on a pull request that is not already in a mergeable
state. That is the whole of what remains, and every condition in it is machine-checkable.

**Into `main`: the instruction, plus one check on the designated path.** This is worth stating
precisely, because the shape is the one this file warns about everywhere else. A deny rule can match
`git push origin main` because the command names its ref. **`gh pr merge <n>` names no branch** —
the base is a property of the pull request — so no `Bash(...)` prefix can distinguish a merge into
`develop` from one into `main`. Removing the merge deny for `develop` therefore removed it for
`main` too.

What replaces it is not a deny but a check. **`bash scripts/gh-automation.sh pr-merge <pr>` is the
designated way for an agent to merge**: it reads `baseRefName` and refuses `main` by name. It fails
closed — an unreadable base and an empty one are refused too, because neither is evidence of
`develop` — and in every refusal it attempts no merge at all.
`scripts/test/pr-merge-guard.test.sh` pins each of those, the negative included (#218).

**It stops an accident, not a decision, and that difference is the whole of what may be claimed for
it.** `gh pr merge` and `gh api -X PUT …/merge` bypass the helper entirely, and nothing stops an
agent that means to use them. The ruleset on `main` requires a pull request, resolved threads and a
green `ci-gate`, and it does not distinguish a human from an agent; required approvals are 0 because
GitHub forbids approving your own pull request and this is a solo repository. **So the instruction
is still what holds `main`. The check is a courtesy to a distracted agent on the one path it is told
to use — the same bargain as the `git push origin main` entries, worth the same amount.**

**What is deliberately not claimed.** The allowlist is not a sandbox and never was. The skills carry
`Bash(gh *)` and `Bash(bash *)`, and both are load-bearing — `gh api` is how half the pipeline talks
to GitHub, and every skill runs `bash scripts/gh-automation.sh`. Both reach a merge by another
spelling: `gh api -X PUT repos/OWNER/REPO/pulls/N/merge`, or `bash -c` running anything at all.
Enumerating gh subcommands would remove the one spelling a reviewer thinks to check and leave the
others in place, which is worse than an honest description because it reads as enforcement.

**The `git push origin main` entries stay, and their reasoning is unchanged.** The review on #52
found `git push --force origin main` denied while `git push -f origin main` was not, and the
shorthands were added: `-f` is the same accident as `--force`, and an accident stopped locally beats
one stopped at the remote. What keeps that from being the theatre described above is what sits
underneath it — a push to `main` has a complete backstop, ref-matched and unbypassable — so those
entries are convenience and nothing rests on them. **Enumerate where something else is holding the
line; never where the enumeration is the line.** A merge into `main` has no equivalent backstop,
which is why the answer there is a base check on one named path rather than a deny entry — an entry
would have blocked the single spelling a reviewer thinks to check while `gh api -X PUT …/merge`
stayed open, and read as enforcement while doing it.

This section used to describe three layers and close on a residual risk: *an agent could merge a
pull request that is genuinely ready but that the human has not read; `READY TO MERGE` and a human
reviewer are what stand there.* #217 deleted the second of those two, deliberately, at the
repository owner's instruction.

**The residual risk now, stated so nobody has to rediscover it.** Three things changed, and they
are not the same thing:

1. **Nothing reads a pull request before it lands on `develop` unless the AI does.** `READY TO
   MERGE` is computed from CI state, mergeability, thread counts and DeepSeek state. Not one of
   those is a reader.
2. **`unresolved_threads == 0` no longer means a person looked.** The AI may resolve a thread it has
   replied to, including one whose suggestion it rejected (#217). The reply is mandatory and is the
   whole of the audit trail; it is not a second opinion. Two machines agreeing is not verification —
   on #209 the review's premise was false, the reply refuted it with a measurement, and the review
   conceded; that exchange would have read identically had the measurement been wrong.
3. **Cross-pull-request ordering is invisible to the frozen rule, and merging is now the AI's
   call.** #214 and #215 were each `ready_to_merge: true` on their own, and merging #214 alone left
   sign-in broken on `develop` at runtime with nothing turning red, because
   `scripts/interop-check.sh` is not a CI step. An ordering stated in a pull-request body binds
   whoever merges. **Read the bodies.**

   **Since #409 something turns red afterwards, and that is a smaller claim than it sounds.**
   `.github/workflows/integration.yml` runs the whole workload on `develop` after every merge and
   files a GitHub issue when it fails, so a combination that is broken only together no longer
   waits for somebody to happen to build it. What it does **not** do is prevent the merge: it
   reports after the fact, deliberately, because the alternative shapes cost more than the report
   is worth (the reasoning is under "What CI enforces"). So the ordering in a pull-request body
   still binds whoever merges — what changed is only how long a mistake stays invisible.

What still stands: `ci-gate` present and green, the rulesets, `DEEPSEEK_REVIEW_READ` as the one
acknowledgement that a *person* read a finding, `NO_DEEPSEEK_REVIEW` beside it, and iteration
planning as a human decision.

### Frozen Acceptance Rule

**Add the `READY TO MERGE` label only when: the stable `ci-gate` check is present and
successful on the head commit, no CI check is failing or pending, the PR is mergeable, no
review requests changes, unresolved review thread count is zero, no DeepSeek review is
holding unread findings in its body (cleared by the `DEEPSEEK_REVIEW_READ` label), and
DeepSeek review is definitively finished (a clean verdict, rounds exhausted, or exempt via
`NO_DEEPSEEK_REVIEW`).**

This rule has exactly one implementation — `cmd_pr_status_json` in
`scripts/gh-automation.sh` — consumed by `pr-labeler.yml` and by the human-facing
`pr-status`. Every condition fails closed: an unreadable count is `-1`, never `0`.

"Green" is not the same as "not red". A PR with **no checks at all** satisfies "zero
failing, zero pending" perfectly — and that state is reachable: a PR with merge conflicts
has no computable merge ref, so no `pull_request` workflow is ever created. The helper
therefore requires `ci-gate` to be **present and successful** and `mergeable == MERGEABLE`.
`ci-gate` owns the branch-aware workload rule (see "What CI enforces" below); a skipped or
unreadable aggregate gate is not green. `labeler` and `review` stay outside this check
because DeepSeek legitimately skips runs once its round cap is spent.

The labeler is event-driven: it fires on `workflow_run` when CI or the DeepSeek review
completes. Whichever finishes second produces the final verdict, so a clean review that
outlives CI does not remain `needs-review`. A six-hour sweep cron covers the transitions
GitHub emits no event for (resolving the last review thread has no webhook; a recomputed
mergeable state after a base merge).
`workflow_dispatch` or `/process-pr` forces an immediate pass. Every firing processes
**all** open PRs, so any single run repairs every stale label. Do not add a `pull_request`
trigger to the labeler: it would be the only mode that attaches a `labeler` check to PR
head SHAs, and mid-CI it could only ever conclude "pending".

#### The write half fails closed too, so a labeler run can go red

Reading state fails closed and always has. Writing the label did not: `pr-label` ran
`gh pr edit … 2>/dev/null || true` and then printed `Label 'X' added to PR #N (idempotent)`
whatever happened — reason discarded, status discarded, success line unconditional. It was
found the only way it could be, by hand: a `pr-label … add` printed that line, exited 0, and
applied nothing (legacy PR 134). A systematic failure — a token that lost `pull_requests: write` —
would have looked exactly like a working pipeline.

`pr-label` now exits non-zero when a write does not land, and `pr-check-label` answers
**0 present / 1 absent / 2 could not determine**, because a failed lookup is not an absent
label and skipping a removal on the strength of one is the same defect one layer down. A
`run:` block is `bash -e`, so a failed write now ends the labeler step and the run goes red.
That costs the rest of that pass, and it is the right trade: the next firing relabels every
open PR anyway, `labeler` is deliberately outside `REQUIRED_CHECK` so a red run cannot make
`READY TO MERGE` unreachable, and a silent no-op loses the same labels while leaving nothing
to notice. **A red labeler run means a label write failed — read the step log rather than
re-running it blind.**

`pr-labeler-step.test.sh` stubs this script wholesale and is right to: it pins which labels
the workflow *asks for* in each state. But its stub's `exit 0` was the assumption under
test, so the helper it stands in for needs its own coverage —
`scripts/test/pr-label-writes.test.sh`, which drives `gh` failing.

#### The thread count only sees half of a review

DeepSeek delivers findings in two shapes. Inline comments create **review threads**, which
the rule counts. General comments live in the **review body** and create no thread at all,
so a review made entirely of them scores `0 unresolved / 0 total` while holding real
findings. `pr-status-json` therefore also counts `deepseek_unread_findings`: bot reviews
whose body still says something once the markers are stripped. The test is **structural,
not marker-driven** — an inline-only review is body-less (its findings are threads) and a
review with general comments is not. The one body that is prose and yet reports nothing —
the clean verdict — says so with `<!-- deepseek:no-findings -->`, trusted only in the exact
shape the script posts: a body that *begins* with that marker and carries no
`<!-- deepseek:full-review -->`. A body that merely quotes the marker is not exempt, and
neither is one that leads with it while carrying findings, because every review with
findings is stamped.

**The acknowledgement is the `DEEPSEEK_REVIEW_READ` label**, and it is *dated*, not
sticky: only reviews submitted before it was applied count as read. A forced second review
blocks again on its own; removing the label un-acknowledges everything; pre-applying it
acknowledges nothing. `NO_DEEPSEEK_REVIEW` does **not** waive it — that label answers
"should DeepSeek review this PR", where findings that already exist were written either way
and nobody has read them. Both labels are human-only actions.

### Completion-Driven Iteration Lifecycle

There is no weekly ceremony schedule and iteration milestones have no due date. The
`iteration-lifecycle.yml` workflow runs when a milestoned or `ceremony`-labelled issue
closes (no other close can move the state machine; manual `workflow_dispatch` recovery
stays unconditional), resolves the active milestone, and evaluates that one:

1. While the active iteration has open issues, it does nothing.
2. When the final committed issue closes, it creates exactly one milestone-specific
   backlog-refinement ceremony.
3. Closing that completed ceremony creates exactly one iteration-planning ceremony.
4. `/scrum-master iteration-plan` selects a non-empty work batch, creates and populates
   the next undated `Iteration N` milestone, then closes the completed milestone.

**More than one iteration may be in flight, and the lifecycle picks the active one instead of
counting them.** Planning ahead is supported: a coherent batch can be committed to a future
milestone while the current one is still being built. The active iteration is the open
milestone with the **lowest** sequence — parsed from an `Iteration N` / `Sprint N` title,
falling back to the milestone number — and every milestone above it is a planned iteration
nobody has started, which is never evaluated. The comparison is numeric on purpose: sorted as
strings, `Iteration 10` precedes `Iteration 9`.

Counting was never a safe way to identify the active milestone, and the sanctioned happy path
is the proof. Step 8 of the planning ceremony creates `Iteration N+1` before step 9 closes
`Iteration N` — deliberately in that order, so a failed assignment leaves the previous
milestone recoverable — and every issue that closed inside that window killed the run with
`Expected exactly one active iteration milestone`. `workflow_dispatch` recovered a different
failure than this one: it is unconditional at the job level only, and the step ran the same
helper and died on the same line (legacy PR 201).

**One ambiguity still fails closed**: two open milestones resolving to the same sequence.
"Lowest" has two answers there, so the helper names both milestone numbers and refuses rather
than advancing an iteration at random. A collision *above* the active one is left alone — the
active iteration is still unambiguous — and it fails closed later, once the collision is
itself the lowest.

**Ceremony creation has a postcondition, not merely a zero exit status.** `gh issue create
--label ceremony` creates the issue first and applies its label afterward; on #119 the first
write landed, the second did not, and the command still returned the new URL successfully.
That unlabeled ceremony was invisible both to the close-event job condition and to the
label-filtered idempotency lookup. The helper now reads the created issue back, retries a
missing label once with an explicit edit, and verifies again. An unreadable lookup or a retry
that does not demonstrably land makes the workflow red and names the issue that needs repair.

Hidden milestone markers and workflow-level concurrency keep the transitions ordered and
idempotent. Ceremonies remain human-in-the-loop: Actions creates the self-describing issue;
the user runs the exact command in its body. Nothing is closed or promoted automatically —
when the active iteration completes while a successor milestone already exists, the machine
still creates its refinement and then its planning ceremony, and the human decides there
whether to populate a further iteration or simply close the completed one.

**Cold start**: with no milestone at all, `iteration-advance` is a clean no-op. The first
`/scrum-master iteration-plan` creates `Iteration 1` directly — the skill documents this
bootstrap path.

### Automated PR Review (DeepSeek)

`deepseek-pr-review.yml` provides automated code review via the DeepSeek API
(`deepseek-v4-flash`, thinking enabled, `reasoning_effort=max`):

| Mode | Trigger | Behavior |
|------|---------|----------|
| **A — Full Review** | PR opened or new commit pushed | Fetches the full diff, sends to DeepSeek, posts inline review comments with code suggestions. **Any finding that names a file must be anchored to it**; the review body is reserved for observations that belong to no single file. A body comment creates no review thread, so it can only be cleared by the `DEEPSEEK_REVIEW_READ` label — anchoring is what keeps that click rare instead of routine. |
| **B — Reply** | Developer replies to a DeepSeek review comment | Fetches full context (diff + all threads), sends the developer's reply as the focal question, posts the response in the same thread. Never touches code. |

**Mode A asks the API for a JSON object; Mode B must not.** `response_format` of type
`json_object` is refused with a 400 unless the prompt contains the word "json". Mode A's system
prompt carries it by construction ("Always respond with a JSON object"); Mode B's does not. While
that flag lived in the shared `call_deepseek` helper, a reply succeeded or failed on whether the
word happened to turn up in the diff — failing as a 400 on a `pull_request_review_comment` run,
which attaches no check to the head commit, so nothing on the PR turned red and the developer
simply never got an answer (legacy PR 54); succeeding as prose wrapped in an object, which is what both
bot replies on legacy PR 34 still are. The contract now belongs to the caller and `json_mode` has no
default for a third caller to inherit. **The finding was filed on legacy PR 16 and dismissed because
seventeen reviews had worked** — seventeen successes were not evidence that the call was legal,
only that the word kept appearing by luck (legacy PR 57).

**Review Completeness**: the model returns a `review_complete` flag. No substantive issues →
the verdict is recorded as a COMMENT review whose body begins with
`<!-- deepseek:no-findings -->` and carries no round marker, so it is terminal without
spending the round budget. Substantive issues → a stamped COMMENT that does spend it.

**Nothing is ever posted as an APPROVE.** GitHub forbids Actions from approving pull
requests, and a PAT is no way around it either, because nobody may approve their own PR and
one human authors them all here. Attempting an APPROVE therefore did not produce a stricter
verdict, it produced a failed job — on the one kind of PR that deserved it least, the
flawless one (legacy PR 22). The marker carries what the review state used to.

**Round Limit**: automatic full reviews are capped at **1 COMMENT round** per PR
(`MAX_ROUNDS`, default 1). One pass surfaces the substantive issues; later rounds tend to
manufacture marginal findings to justify another COMMENT. After the cap, pushes no longer
trigger a review and a one-time notice is posted as an issue comment. Mode B (threaded
replies) is unaffected and continues indefinitely.

**What counts as a round**: only Mode A full reviews, identified by the
`<!-- deepseek:full-review -->` marker stamped into every review body. The marker is
load-bearing — GitHub records a standalone review-comment reply as an implicit empty-body
COMMENTED review, so a state-only filter would count thread replies as review rounds. The
clean verdict never counts either: it is deliberately unstamped, so a clean pass on an early
commit leaves a later push reviewable. Historical APPROVEs, from before GitHub's restriction
was hit, are still honoured as terminal. Definitions live in `.github/scripts/deepseek_review.py`
(`FULL_REVIEW_MARKER`) and `scripts/gh-automation.sh` (`DEEPSEEK_FULL_REVIEW_MARKER`) and
must stay in sync — `test_deepseek_review.py` pins the pair.

**Forcing another pass**: `bash scripts/gh-automation.sh pr-deepseek-force-review <pr> [ref]`
dispatches a review with `FORCE_REVIEW=true`, which bypasses the cap. The dispatched run
executes the workflow definition from `ref` (default `develop`).

**Which copy of the config applies**: `pull_request` runs check out the *merge ref*, so the
workflow and script that execute are the **base branch's**. Diagnose review behaviour by
reading develop's copy, never the branch's.

**Safety**: DeepSeek never creates commits, pushes code, or modifies files — it is
review-only. The bot ignores its own comments (anti-loop guard, enforced at job level
before a runner boots). Diffs over 45,000 characters are truncated with every dropped file
named in the log **and in the review itself**: a truncated pass cannot come back clean, because
the skipped files are injected as a finding, so the pull request blocks until a human has
acknowledged what nobody read (legacy PR 32 — on legacy PR 30 the budget ran out after the client files and the
entire server half was reported as having no issues). Splitting the pull request is still the
reliable fix when a diff exceeds it.

**And it never claims to have done any of it.** Both system prompts forbade *suggesting* an
action; neither forbade *asserting* one, so on PR #209 a Mode B reply announced "I'm closing this
thread as resolved" — 72 seconds after a human had already resolved it. The script's entire write
surface is `create_issue_comment`, `create_review` and `create_review_comment_reply`; resolving a
thread is not among them. Both prompts now forbid the claim as well as the suggestion, and both
carry the worked example that keeps it from widening into a muzzle: *"I checked the callers"* is a
reviewer doing its job, *"I closed the thread"* is a reviewer reporting something that did not
happen. Making the claim true was the wrong fix — a bot that can clear its own findings is the
opposite of what `unresolved_threads == 0` exists to mean. Third instance of the #134 / #46 family,
same resolution: **when an output asserts something no machine checks, stop asserting it** (#212).

**The cap used to be 120,000, and the reason given for keeping it there was wrong.** This file
said it was not raised "because the model's context is what the cap describes". V4 documents a
**1M-token context**, flash and pro alike; 120,000 characters is roughly 35K tokens, about 3.5%
of that window. The cap described a limit the model did not have. It cost legacy PR 158 five unread
files — `session.go` and `world/store.go` among them, the two the change actually turned on —
and the block did its job: nothing came back clean, and a human had to acknowledge the gap.

**Then it was 600,000, and that was the same mistake with a bigger number.** It described the
*new* model's context window, and the context window is not what bounds a review: the chain of
thought is emitted into the same output budget the verdict has to fit in, so a diff can sit
comfortably inside a 1M-token context and still leave nothing to answer with. The cap therefore
never fired in the band where the model actually runs out — anything between roughly 124,000 and
600,000 characters reached the API, spent the whole ceiling reasoning, and exited on a missing
verdict with nothing anywhere saying the size was the problem. PR #164 is where that was paid for:
124,711 characters, 1,481,442 characters of reasoning, `finish_reason=length`, 31 minutes, no
review (#167).

**Then it was 90,000, and that number was measured — from the only full-ceiling sample there was
of a quantity that varies by a factor of two.** From #164, the model emitted 1,481,442 characters
for 384,000 tokens — 3.86 characters per token, so the output budget is about 1,481,000
characters — and that run reasoned about **11.9** characters per character of diff. The diff
which exactly fills the budget at that ratio is about 124,000 characters, which is where #164
landed; 90,000 was set at 72% of it and left roughly 107,000 tokens over. Then PR #488 reasoned
at **23.8** — twice as hard, on a diff half the size — and 60,863 characters exhausted the
whole ceiling in 33 minutes with no verdict (#491).

**45,000 is set from the worst observed ratio, not the average.** 23.8 against a
1,481,000-character budget puts the fill point at about 62,300 — but that is an estimate carrying
#164's characters-per-token, and the number to anchor on is the one #488 measured: it emitted
1,448,213 characters at 60,863 and had nothing left for a verdict, so **60,863 is an observed fill
point** and 62,300 corroborates it to within 2.4% rather than replacing it. The margin is the one
90,000 already used, taken against the observed point: 90,000 was 72% of the 124,711 #164 measured,
and **45,000 is 74% of the 60,863 #488 measured** — a point *thinner* than that precedent rather
than as generous (the precedent's exact 72.2% of 60,863 is 43,900, a flat 72% is 43,800), which is
said out loud because the gap is inside the noise of a two-sample ratio. It spends about 277,400
tokens reasoning at the bad ratio and about 138,500 at the good one. **The outcome is not
monotonic in diff size, and that is the finding** — 72,350 (#169) succeeded and 60,863 failed,
so size is a proxy for the binding variable and the binding variable is how hard the model
reasons about *that* content.

| PR | Diff chars | Reasoning chars | Ratio | Outcome |
|---|---|---|---|---|
| #501 | 45,415 | — | — | verdict, two findings |
| #80 (at 262,144) | 50,963 | — | ~2.7, from 35,966 completion tokens | verdict in 8m58s |
| #80 (at 131,072) | 50,963 | 530,226 | ≥10.4 — a floor; the ceiling stopped it | no verdict |
| #488 | **60,863** | **1,448,213** | **23.8** | **no verdict, 33 min** |
| #168 | 64,167 | — | — | verdict in 7m38s |
| #169 | 72,350 | — | — | verdict |
| #164 | 124,711 | 1,481,442 | 11.9 | no verdict, 31 min |

**Set it from the tail, because the two failure directions are not symmetric.** Too high costs a
33-minute run that produces nothing, a failing `review` check and a pull request nobody can merge
until it is split by hand — and nothing says the size was the problem until you open the job. Too
low is loud: the diff is truncated, every dropped file is named in the log and injected into the
review as a finding, and the pull request blocks until a human acknowledges the gap. One costs
half an hour and a manual split, the other costs a `DEEPSEEK_REVIEW_READ` click.

**It costs review latency, and that was chosen rather than inherited.** Five of Iteration 29's
seven issues already needed splitting at 90,000; at 45,000 most changes become two or three pull
requests. What bounds the tightening is #501: 45,415 characters came back with a verdict and two
substantive findings, so the safe ceiling is bracketed between a measured success at 45,415 and a
measured failure at 60,863, and the cap sits just under the success.

**Raising it again is what needs more samples; lowering it did not.** Two rows measure the ratio
*at exhaustion* — #164 at 11.9 and #488 at 23.8 — and those two are what the cap is set from,
because in each the 384,000-token ceiling this cap sits under is what stopped the reasoning. The
row for #80's replay stopped against a lower ceiling that has since been raised, which is a
different thing. The rest of the table bounds the ratio without
sampling it: `reasoning_chars` is printed only where a run returns no content, so a success reports
`completion_tokens` instead, and #80's replay printed 530,226 characters against a 131,072-token
ceiling it *hit*, which makes 10.4 a floor rather than a value. **What #80 does measure is worth
more than a third point on the curve: its two rows are the same 50,963-character diff**, once
reasoning past 131,072 tokens and once finishing inside 35,966 — so the ratio varies run to run on
identical input and not only between diffs, which is the strongest argument available that a cap
set from one observation was never going to hold. `measure_only: true` on the dispatch replays a
real diff without posting a review or spending a round, and a third and fourth sample would say
whether 23.8 is the tail or the new middle. Until somebody takes them, the asymmetry above decides
the direction on its own.

**The cap is a truncation threshold, not a promise.** A review that still exhausts the budget
under it is a new measurement, and this number is what comes down. The ratio belongs to the model
and to `DEEPSEEK_REASONING_EFFORT`; change either and it has to be measured again. The lesson is
narrower than "caps should be generous": **a number defended by a claim about the world has to be
re-checked when the world changes** — twice the claim was about the context window when the
binding constraint was somewhere else entirely, and the third time it was a real measurement of a
quantity that had been observed once.

**An unreadable diff fails the run**, and "unreadable" covers three shapes:

- a fetch that **errors** — not an empty diff, and reporting it as one is how a review that never
  happened got a green check (legacy PR 31);
- a fetch that **answers incompletely** — files returned with no patch. GitHub reports zero changed
  lines for a genuine binary, so a file with changes and no patch is a withheld patch, not an image
  (legacy PR 43);
- a fetch that **fails below PyGithub** — urllib3 exhausting its retries raises `RequestException`,
  which escaped as a stack trace until it was caught and named (legacy PR 43, and this one had genuinely
  happened: a 503 storm on `/pulls/42/reviews`).

**An unreadable round count is not zero either.** Zero means "no rounds spent" and lets another
review run, so a failed lookup during an outage could bypass the one-round cap indefinitely, each
run blind to the ones before it. It raises instead.

**A correction worth keeping, because the mistake was expensive** (legacy PR 46). The withheld-patch case
above was written as a post-mortem: a 636-line pull request had supposedly arrived as a
three-character diff, on the strength of which the model improvised findings for code it had never
seen. **That never happened.** `get_diff` returned a string until legacy PR 34 and a 3-field tuple after, and
the log line kept calling `len()` on it — so `Diff: 3 chars` was the tuple's field count, printed for
every pull request regardless of size. The model had the full diff the whole time.

The guard is still right, and stays: a withheld patch *is* distinguishable from a binary and *should*
fail the run. What was wrong was the belief that it had already fired. The rule that follows is not
"test your logs" but something narrower: **a diagnostic someone makes a decision from is an output,
and outputs are pinned by tests here.** Two agents read that line and reached the same false
conclusion — one filed an issue and wrote a pull request against a bug that did not exist, the other
told the user a clean review was worthless. Neither was careless; the number simply could not be
questioned, because a number that cannot vary cannot look wrong.

Generated code (`gen/` directories, `*_generated.rs`) and dependency
lockfiles (`Cargo.lock`, `go.sum`, matched by exact basename) are excluded from the diff by
name — announced, never reviewed. The manifests beside them (`Cargo.toml`, `go.mod`) are not
excluded: a new dependency is exactly what a reviewer should see, and the lockfile is only
its mechanical consequence. The measurement that motivated the lockfile half: on legacy PR 15,
`client/Cargo.lock` was 5264 of 8319 non-generated lines — 63% of a one-round review budget
spent on a resolved version graph.

**Output budget, cost and time are one configuration.** Mode A and Mode B explicitly send
`DEEPSEEK_MAX_OUTPUT_TOKENS=384000`. Legacy PR 80 exhausted 65,536 tokens while reasoning over a
measured 50,963-character / 13-file diff; a faithful measure-only replay then exhausted 131,072
tokens too (run 32171858677: 530,226 reasoning characters, no final content). With 262,144,
run 32175108406 reviewed that same diff on its first attempt in 8m58s and returned 1,060 final
characters / 35,966 completion tokens; the JSON verdict parsed successfully and measure-only
posted nothing to GitHub. 262,144 held until the diff cap was raised; a larger diff reasons for
longer before it has a verdict, so the configured value now *is*
[V4's documented 384K maximum](https://api-docs.deepseek.com/quick_start/pricing) rather than a
step below it — and because there is nothing above it, the diff cap is the number that has to
absorb every later surprise. Startup validation rejects zero, non-numeric and oversized values before
an API call, and its executable ceiling was already 384,000 — this change spends headroom that
existed rather than creating any. **There is none left above it**: a review that exhausts 384K
needs a smaller diff, not a larger budget. At the V4 Flash rate of $0.28 per million output
tokens, one attempt that consumes the full ceiling costs about $0.108; with the single retry the
worst-case two-attempt output envelope is about $0.215, excluding input tokens.

**Timeouts — two of them, and the order matters**: the request budget is
`DEEPSEEK_REQUEST_TIMEOUT_SECONDS` × (`DEEPSEEK_MAX_RETRIES` + 1) — currently 2700s × 2 =
90 min — and the job gets `timeout-minutes: 100`, leaving an explicit 10 minutes for checkout,
setup and posting. **The budget must stay below the job cap**, so change one and re-check the
other. `scripts/test/deepseek-budget.test.sh` pins this relationship. When the SDK's deadline
fires, the script
prints a diagnostic naming the diff size and exits 1; when the job cap wins instead, GitHub
reports the step as `cancelled` with no output — and `pr-status-json` counts `CANCELLED` as
a *failing* check, so the PR sticks at `needs-work` with nothing in the log explaining why.
`max_retries` is deliberately **1, not 0**: one retry buys resilience against transient
connection errors (they fail fast) while keeping the worst case bounded.

### Setup Prerequisites

0. **Local tools**: `gh` and `jq`. `scripts/gh-automation.sh` needs both, and the helper
   test suite needs `jq` as well — the full list lives in the Toolchain table in
   `README.md`. `jq` is the one that went undeclared for a long time, because GitHub's
   `ubuntu-latest` image ships it and CI therefore never noticed: on a workstation without
   it, `pr-status` reported `[FAIL] ? unresolved review threads (must be 0)` and exited 0,
   a sentence about GitHub describing a fact about the workstation (#211). Every standalone
   `jq` call in that script is deliberately `2>/dev/null` — a working jq can still be handed
   an unparseable payload, and the fail-closed sentinel is what must answer that — so the
   same redirection swallowed `jq: command not found` at all of them. `require_jq` sits
   beside `require_gh` and is reached the same way, from the command dispatch rather than
   file scope, so `--help` and the subcommands that need neither tool stay usable. **gh's
   built-in `--jq` is not the same thing**: that expression is evaluated inside `gh` and
   needs no binary, which is exactly what makes the two easy to confuse when reading the
   script.
1. **GitHub Token**: Create a fine-grained PAT with `contents: read/write`,
   `pull_requests: read/write`, `issues: read/write` on this repository. Store as
   `GH_PIPELINE_TOKEN` in repository secrets.

   **This token cannot read CI status, and no permission fixes that.** `pr-status-json`
   reads CI state through the `statusCheckRollup` projection, which includes CheckRun rows,
   and the Checks API is **GitHub-App-only** — the fine-grained PAT permission list has no
   `Checks` entry to grant (it offers `Commit statuses`, which covers only the
   StatusContext half). The fix is in the workflow, not in settings: `pr-labeler.yml`
   passes `GH_CI_TOKEN: ${{ secrets.GITHUB_TOKEN }}` and declares `checks: read` +
   `statuses: read` + `actions: read`, and `gh_ci()` in `scripts/gh-automation.sh` routes
   that one call through it. All three permissions are load-bearing (`gh`'s projection
   reaches `checkSuite.workflowRun`; without `actions: read` the rollup dies on that nested
   field and fails closed to `-1`). The PAT keeps everything else, including the
   cross-workflow dispatches `GITHUB_TOKEN` deliberately cannot perform. **Any new caller
   that reads checks must use `gh_ci`, not `gh`.**
2. **DeepSeek API Key**: Create an API key at
   [platform.deepseek.com](https://platform.deepseek.com/api_keys) and store it as
   `DEEPSEEK_API_KEY` in repository secrets (`Settings → Secrets and variables → Actions`).
3. **Merge protection**: enforced by two GitHub **rulesets**, both active on `develop` and `main`
   — see `.github/branch-protection.md` for the payloads and the reasoning. Classic branch
   protection is not used (it is plan-gated on a private repository; rulesets are not).

   **An unresolved review thread blocks the merge for everyone, with no bypass. A red, pending or
   missing `ci-gate` blocks it too, but a repository admin can override that one explicitly.**
   The asymmetry needs two rulesets because a bypass list belongs to a ruleset, not to a rule.

   Two consequences that bite before you read the file: the `pull_request` rule also rejects
   **direct pushes** to `develop` and `main` — the existing hard rule now fails at the remote
   rather than relying on discipline — and required approvals are deliberately **0**, because
   GitHub forbids approving your own PR and a solo repository would otherwise be unmergeable
   except by bypass. Require `ci-gate` only after it has reported on a real PR.

   The rulesets are a floor under the frozen acceptance rule, not a replacement for it: GitHub's
   native unresolved-conversation check counts review *threads*, so it is blind to DeepSeek
   findings that live in a review *body*. `READY TO MERGE` remains the stricter signal — merge on
   the label, and let the ruleset catch the mistakes.

## Definition of Done

**Every PR opened by the pipeline MUST verify ALL of the following before the PR is created:**

- [ ] **Formatting clean** — `gofmt -l` empty (server) / `cargo fmt --all --check` (client)
- [ ] **Lint passes** — `go vet ./...` and `golangci-lint run` (server) / `cargo clippy --workspace --all-targets --locked -- -D warnings` (client)
- [ ] **Build succeeds** — `go build ./...` / `cargo build --workspace --locked`
- [ ] **Cross-compiles for 32-bit** (server) — `GOARCH=386 go build ./...` and `GOARCH=arm go build ./...`; the runners are amd64 only, so an untyped constant that overflows `int` is invisible to every other gate
- [ ] **Tests green** — `go test ./...` / `cargo test --workspace --locked`; this repository starts with a clean baseline — keep it that way
- [ ] **Schemas validated** — `bash scripts/check-schemas.sh` and regenerated bindings, when `schemas/` is touched
- [ ] **No debug prints** in production code paths
- [ ] **No hardcoded secrets** — no API keys, tokens, passwords, or credentials in code or fixtures
- [ ] **Publication privacy clean** — no real email, personal data, internal path, hostname or endpoint; `scripts/check-publication-privacy.sh` passes over the tree and `scripts/check-commit-privacy.sh` passes over the commits the branch adds
- [ ] **Server-authoritative rule honored** — no gameplay decisions client-side
- [ ] **Workspace rules honored** — local AGENTS.md conventions

If any gate fails, the implementation is incomplete. Fix and re-run gates before creating the PR.

### What CI enforces

`.github/workflows/ci.yml` runs the following per workspace, so the first five DoD bullets
are machine-checked and the rest remain review responsibilities. **This table, the DoD
bullets above and the gate tables in both canonical skills are pinned to ci.yml by
`scripts/test/gate-tables.test.sh`** — they are not kept in step by hand. They were, once:
golangci-lint had been a blocking server gate since the Go scaffold (legacy PR 13) and appeared in
none of them, so an agent following the skills ran four server gates where CI runs five.

| Job          | Gates                                                                            |
| ------------ | -------------------------------------------------------------------------------- |
| `server`     | `gofmt` check, `go vet`, `golangci-lint` (version pinned in ci.yml), `go build`, `GOARCH=386 go build ./...` + `GOARCH=arm go build ./...`, `go test` |
| `client`     | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo build --locked`, `cargo test --locked` (with Bevy's Linux system deps) |
| `schemas`    | `scripts/check-schemas.sh`: every contract generates for both consumers, **and the committed `gen/` bindings are the ones it produces** — flatc pinned in `.flatc-version` |
| `automation` | The full `scripts/test/*.test.sh` suite plus `test_deepseek_review.py` — the pipeline's own regression tests |
| `ci-gate`    | The one stable check: audits that every selected job succeeded and every skip was authorized |

That is the pull-request half. `.github/workflows/integration.yml` runs the same workload again
on `develop` after every merge — see "Nothing verified `develop` after a merge" below.

#### CI runs only the jobs the diff can affect (the `detect` job)

Every `ci.yml` run starts with `detect`. On a PR targeting `develop`, it classifies changed
files through `scripts/changed-areas.sh` and lets unaffected jobs skip via job-level `if:`.
A server-only PR runs `server`; a schema PR runs `schemas` + `server` + `client` (the
contract fan-out); a docs-only PR runs nothing but `detect`, possibly `automation`, and
`ci-gate`. A skipped job still reports a terminal `SKIPPED` result, which `ci-gate` accepts
only when the corresponding selector is exactly `false`.

On a PR targeting `main`, `detect` bypasses diff classification and selects **everything
that exists at the merge ref** (existence read from marker files: `server/go.mod`,
`client/Cargo.toml`, any `schemas/*.fbs`). `ci-gate` re-derives existence from its own
checkout of the same ref and requires selector == existence, so a promotion can never
narrow its matrix. Pre-scaffold, an absent workspace is the one legitimate skip on main.

Three failure directions are deliberately open: the classifier answers all-true for
unrecognised paths and empty input (pinned by `changed-areas.test.sh`); the detect step
falls back to all-true when it cannot enumerate the diff (API error or the pulls/files
3000-file cap); and the consumers test `!= 'false'`, so a crashed `detect` runs the full
matrix while `ci-gate` also rejects the failed classifier.

The `helpers` selector (which gates `automation`) is the one output the classifier does NOT
produce: it is a plain grep over the raw changed-path list in the workflow. The automation
job runs `changed-areas.sh`'s own tests, so gating it on that classifier's output would let
a classifier bug exempt itself from the tests that would have caught it.

When a new top-level path appears, the fallback costs a few extra minutes per run until
someone classifies it: fix that by adding a rule to `changed-areas.sh` **with a test**,
never by reaching for `paths:` filters.

Two traps to know before trying to trim CI further. Both are ways of making
`READY TO MERGE` unreachable while every log looks green:

1. **Never add workflow- or job-level `paths:` filters to workload jobs.** A path-filtered
   workflow creates no result for `ci-gate` to audit. Job-level `if:` is the safe tool.
2. **`cancel-in-progress` resolves cancelled jobs to CANCELLED, which `pr-status-json`
   counts as FAILING.** `ci.yml` scopes cancellation to `pull_request` events only: a
   `synchronize` always carries a new head SHA, so the cancelled run belongs to a commit
   that is no longer the head and whose checks are never read. A cancellation landing on
   the *current* head SHA pins the PR at `needs-work` until a manual re-run.

#### Nothing verified `develop` after a merge — `integration.yml` does

`ci.yml` runs on a pull request's merge ref **and nowhere else**. Every pull request is therefore
validated against the `develop` that existed when its run started, and two pull requests branched
from the same base are each validated against a tree that does not contain the other. A
combination that is broken only together turns nothing red. #214 and #215 were the first bill for
that; in Iteration 25, #402 and #403 were verified together only because somebody made a worktree
by hand and ran the client gates in it. **A process that depends on somebody remembering is not
one**, and #409 replaced it.

`.github/workflows/integration.yml` runs on `push: branches: [develop]`. It has **no `detect` job
and no classifier**: the point of a post-merge run is the combination, and a combination is not
describable as a diff, so every job runs and each no-ops with a notice when its workspace is not
scaffolded — the same `present` guard, and the same rule a `main`-targeted pull request already
follows. Alongside the four workloads it re-runs both privacy scans, which is the one thing here
that is new rather than repeated: a merge commit's own message and identity fields were never
scanned by anything, because they do not exist yet while the pull request is open.

**Three shapes were weighed and two were rejected.** Adding `push` to `ci.yml` needs one
definition and no drift, and costs conditionals through `detect`, `concurrency` and the privacy
step in the file where a mistake is most expensive — the one file whose pull-request behaviour must
not move. A merge queue (`merge_group`) is strictly better in kind, because it is the only shape
that *prevents* the broken combination rather than reporting it, and it costs the most: it changes
how merges happen, it interacts with both rulesets, and `pr-merge` would have to understand it. A
separate workflow keeps `ci.yml`'s contract untouched by construction and costs drift — which this
repository already knows how to pay, because `client-cache.yml` is the other copy of that same
workload and `client-cache-parity.test.sh` is what keeps it honest.
`scripts/test/integration-verify.test.sh` does the same job here: it **derives** the expected job
bodies from `ci.yml` and compares them line for line, so the two files can only drift through a red
`automation` job.

**What a failure looks like.** `integration-verdict` audits every job — a `skipped` or empty result
is rejected, since a skip here authorises nothing — and on failure runs
`gh-automation.sh integration-report`, which opens a GitHub issue naming the commit, the run and
the failing jobs, or comments on the one already open. A red run in a list nobody opens is not a
notification, and a workflow-run email depends on a per-account setting this repository cannot see;
an issue is a durable, watchable artifact that stays until a person closes it. Only machine-generated
facts reach that body: an issue opened by `GITHUB_TOKEN` triggers no workflow, so `body-privacy.yml`
never scans it. Nothing reverts, rolls back or fixes anything — reporting is the whole of it.

**It cannot make `READY TO MERGE` unreachable, and that is a requirement rather than a
side effect.** The run is on `develop`'s tip, so its check attaches to a commit that is no pull
request's head — the only SHA `pr-status-json` reads. It is not `ci-gate`, so it is not
`REQUIRED_CHECK`. This is the same reasoning that keeps `labeler` outside the frozen rule: a broken
integration branch must not also make every open pull request unmergeable, because **a merge is
sometimes how it gets fixed**.

**There is no `concurrency` group, deliberately.** Keyed on `github.event.pull_request.number` —
`ci.yml`'s group — every push would share the group `ci-`, because that context is empty outside a
pull request. Keyed on the ref with `cancel-in-progress: true`, an intermediate merge would be
cancelled and never verified, which is the hole this closes. And `cancel-in-progress: false` is the
same hole with an extra step: GitHub cancels a **pending** run when a newer one queues behind it, so
a burst of four merges would verify the first and the last and drop the two between. Every merge
gets its own run. There is no `workflow_dispatch` either — a dispatch carries no `github.event.before`,
and the commit-privacy range fails closed on a range it cannot read (a zero `before`, an absent tip),
so a dispatch could only ever reach that refusal. Re-run a failed run from the UI: a re-run replays
the original push payload.
