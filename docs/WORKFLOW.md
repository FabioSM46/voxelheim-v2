# Workflow

Solo-developer agile process powered by Claude Code, Codex, or OpenCode. All state lives
in GitHub — no external PM tool. `AGENTS.md` is authoritative for every rule summarized
here.

## Roles

| Role | Who | What |
|------|-----|------|
| Product Owner | **You** | Ideas, priorities, approve specs, and perform `main` promotions |
| Scrum Master | Claude/OpenCode (`/scrum-master`) or Codex (`$scrum-master`) | Runs ceremonies, drafts specs, creates issues |
| Developer | Claude/OpenCode (`/dev-issue`) or Codex (`$dev-issue`) | Implements issues, opens PRs, resolves feedback |
| Iteration Orchestrator | Claude/OpenCode (`/develop-iteration`) or Codex (`$develop-iteration`) | Parallelizes committed issues, remediates PRs, merges non-main bases |
| Reviewer | DeepSeek `deepseek-v4-flash` with high reasoning (auto) + You | Automated code review on every PR |

## The Completion-Driven Cycle

### 1. Iteration Execution

The daily dev cycle. Run `/dev-issue <number>` in Claude/OpenCode or
`$dev-issue <number>` in Codex to implement issues. Each run:

```
1. Fetch issue from GitHub → parse workspace, AC, code pointers
2. Read AGENTS.md conventions (root + workspace)
3. Create isolated git worktree, branch from the planned non-main base (`develop` by default)
4. Implement (surgical edits, server-authoritative, no new deps)
5. Run quality gates (fmt, lint, build, test, schemas) — loop until green
6. Commit, push, open PR targeting that planned base
7. Cleanup worktree, exit (stateless)
```

**Auto-review**: DeepSeek reviews the PR automatically on open (one automatic round;
replies in threads continue indefinitely).

**Passive monitoring**: `pr-labeler.yml` fires when a PR's CI run completes (plus a
six-hour sweep and manual dispatch), evaluates the frozen rule, and labels the PR:
- `READY TO MERGE` — the full frozen acceptance rule holds (see below)
- `needs-work` — CI failing or changes requested
- `needs-review` — otherwise (pending, unresolved threads, unread body findings, conflicts)

**Force-cycle**: `/process-pr <number>` resolves base conflicts, reads review feedback, implements
fixes, re-runs quality gates, pushes, then resolves or acknowledges findings against the published
head.

**Autonomous path**: `/develop-iteration` (or `$develop-iteration` in Codex) assigns one agent per
independent issue, runs safe waves in parallel, routes actionable PRs through `/process-pr`, and
merges every freshly ready PR into its planned non-main base. You may still run the individual
skills directly. You may acknowledge body-level findings with `DEEPSEEK_REVIEW_READ` after reading
them; `/process-pr` may do so too, but only after it has addressed or evidence-backed rejected every
finding and posted that disposition publicly on the PR.

There is no calendar deadline. If you stop work for a month, the active iteration simply
remains open. When its final committed issue closes, `iteration-lifecycle.yml` creates one
milestone-specific backlog-refinement ceremony.

### 2. Backlog Refinement

**`/scrum-master backlog-refine`** does:
1. Scans all open issues (excluding ceremony issues)
2. Categorizes each: Ready / Needs Spec / Needs Triage / Stale
3. Drafts missing specs as comments on issues
4. Updates labels (`needs-refinement` → `ready-for-dev` when complete)
5. Re-prioritizes based on dependencies (schemas → server → client), value, and effort
6. Flags stale issues (30+ days inactive) for closure
7. Posts a summary comment, closes the ceremony issue

**You**: Review the summary, close or keep stale issues, adjust priorities, add manual specs.

Closing the milestone-specific refinement issue automatically creates one planning
ceremony for the next iteration.

### 3. Iteration Planning

**`/scrum-master iteration-plan`** does:
1. Resolves the active iteration — the open milestone with the lowest sequence, so several
   iterations may be planned ahead without the ceremony refusing to run
2. Verifies zero open issues and a complete milestone-specific refinement ceremony before
   *closing* that milestone; creating and populating a new one is not gated on either
3. Selects a non-empty, realistic solo-developer work batch from `ready-for-dev`
4. Creates and populates the next undated `Iteration N` milestone, then closes the completed milestone
5. Assigns effort estimates (S = hours, M = 1-2 days, L = 3-5 days)
6. Posts an iteration plan with a clear goal and closes the ceremony issue

**You**: Review the plan, remove or add issues, and adjust the iteration goal.
`workflow_dispatch` on `iteration-lifecycle.yml` is the recovery mechanism if an
issue-close event is missed.

### Bootstrapping the first iteration

Fresh repository, no milestones yet: `iteration-advance` no-ops by design. The path in is:

1. `/scrum-master feature-spec "<first epic>"` → issues are created
2. `/scrum-master backlog-refine` (manual invocation) → issues graduate to `ready-for-dev`
3. `/scrum-master iteration-plan` → detects the cold start, creates `Iteration 1`
   directly and populates it

From then on the completion-driven state machine owns the transitions.

## Issue Lifecycle

```
needs-triage → needs-refinement → ready-for-dev → in-progress → in-review → done
```

| Label | Meaning | Set By |
|-------|---------|--------|
| `needs-triage` | Just created, not assessed | Issue template |
| `needs-refinement` | Needs AC, technical context, out-of-scope | Issue template or user |
| `ready-for-dev` | Fully specified, iteration-ready | `backlog-refine` |
| `in-progress` | Being implemented | `/dev-issue` (auto) |
| `in-review` | PR open, under review | `/dev-issue` (auto) |
| `done` | Merged to develop | PR merge (normally the iteration orchestrator) |

## Issue Labels

| Label | Meaning |
|-------|---------|
| `feature` | New capability |
| `bug` | Defect or unexpected behavior |
| `enhancement` | Improvement to existing feature |
| `refactor` | Code restructuring, no behavior change |
| `ceremony` | Scrum ceremony issue (excluded from backlog scans) |

## PR Labels

| Label | Meaning | Set By |
|-------|---------|--------|
| `needs-review` | Waiting for CI / review | `pr-labeler.yml` |
| `needs-work` | CI failing or changes requested | `pr-labeler.yml` |
| `READY TO MERGE` | Frozen acceptance rule satisfied | `pr-labeler.yml` |
| `DEEPSEEK_REVIEW_READ` | Every DeepSeek review-body finding has been read and publicly disposed of; acknowledgement must postdate the review | You, or `/process-pr` after its audited review |
| `NO_DEEPSEEK_REVIEW` | PR exempt from DeepSeek review (bot branches, trivial changes) | You, by hand |

## Frozen Acceptance Rule

> Add `READY TO MERGE` only when: the stable `ci-gate` check ran and succeeded, no CI
> check is failing or pending, the PR is mergeable, no reviews request changes,
> unresolved review thread count is zero, no DeepSeek review is holding unread findings
> in its body (cleared with the `DEEPSEEK_REVIEW_READ` label), and DeepSeek review is
> definitively finished (approved, rounds exhausted, or exempt via `NO_DEEPSEEK_REVIEW`).

The enforced list lives in `scripts/gh-automation.sh` (`cmd_pr_status_json`) — one
implementation, consumed by both the labeler and the human-facing `pr-status`. See
`AGENTS.md` for the reasoning behind every condition.

## Feature Specs (large features only)

**`/scrum-master feature-spec "description"`** drafts a full technical spec (network
contract, server design, client design, constraints, risks), asks for your approval, then
creates breakdown issues with suggested order and effort estimates.

## Commands Quick Reference

| Command | Purpose |
|---------|---------|
| `/dev-issue <number>` | Implement end-to-end: worktree → code → gates → PR |
| `/process-pr <number>` | Force-cycle: resolve bot feedback + fix CI |
| `/develop-iteration [iteration]` | Parallelize committed issues → remediate → merge non-main PRs |
| `/scrum-master feature-spec "..."` | Draft spec → create breakdown issues (after approval) |
| `/scrum-master backlog-refine` | Scan, categorize, draft specs, re-prioritize |
| `/scrum-master iteration-plan` | Close completed milestone, create next iteration, select issues |
| `bash scripts/gh-automation.sh pr-status <n>` | Full frozen-rule readout for a PR |
| `bash scripts/gh-automation.sh pr-deepseek-force-review <n>` | Force another review round |

## Infrastructure

| Workflow File | Trigger | Purpose |
|---------------|---------|---------|
| `.github/workflows/ci.yml` | Every PR | Change-gated non-main validation; existence-complete main promotion; stable `ci-gate` verdict |
| `.github/workflows/pr-labeler.yml` | CI run completed + 6h sweep + manual | Read CI + reviews + threads, manage labels |
| `.github/workflows/deepseek-pr-review.yml` | PR open/sync + review replies + manual dispatch | Post inline review comments via DeepSeek |
| `.github/workflows/iteration-lifecycle.yml` | Issue closed + manual recovery | Sequence completion-driven ceremonies |
