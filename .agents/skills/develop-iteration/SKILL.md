---
name: develop-iteration
description: Develops every committed issue in an iteration, parallelizes independent work, processes PR feedback, and autonomously merges ready PRs into non-main bases.
---


# develop-iteration — Iteration Delivery Orchestrator

Triggers: `$develop-iteration <iteration-number-or-milestone>` or `$develop-iteration` (uses the active iteration)

## Purpose

Deliver the issues already committed to one iteration. This skill does not choose iteration scope:
`$scrum-master iteration-plan` and the user own that decision. It coordinates one issue agent per
issue, uses `$dev-issue` for implementation and `$process-pr` for remediation, and merges every PR
that satisfies the repository's frozen rule.

The run is resumable. GitHub milestones, issues, branches, pull requests, checks, reviews and
comments are the state store; never rely on an in-memory queue being the only record of progress.
On restart, reconstruct the plan from GitHub and continue without duplicating branches or PRs.

## Non-negotiable boundaries

- Never create, retarget or merge a pull request whose base is `main`.
- Never push directly to `main` or `develop`.
- A merge may target `develop` or any other non-`main` branch. Feature branches and their
  sub-branches are first-class topology, not exceptional cleanup.
- Use `bash scripts/gh-automation.sh pr-merge <pr> --head <observed-sha>` for every merge. The
  helper refuses `main` and rejects a concurrently changed head, but it does not decide readiness;
  the fresh readiness check below is mandatory.
- `NO_DEEPSEEK_REVIEW` remains human-only. Body findings may be acknowledged only through
  `$process-pr`'s read/dispose/public-audit/fresh-label sequence.
- Iteration ceremonies remain human-in-the-loop. Stop after delivery; do not run backlog
  refinement or iteration planning automatically.

## 1. Resolve the iteration and reconstruct state

If an argument is supplied, resolve it to exactly one open milestone. Accept a milestone number,
`Iteration N`, or `Sprint N`. Without an argument, use the same active-iteration rule as
`iteration-advance`: the open iteration milestone with the lowest numeric sequence, falling back
to milestone number. If two open milestones resolve to the same lowest sequence, stop and name
both; choosing between them is a product decision.

Read every non-ceremony issue assigned to the milestone, including closed issues, and find any PR
that references it. For each issue record:

- issue state and required `$dev-issue` fields;
- declared dependencies and ordering notes in the issue body;
- existing branch and PR, including the PR's actual base branch;
- CI, DeepSeek, review-thread and merge state;
- likely files/workspaces from Code Pointers and technical context.

Treat already-merged work as complete. Reuse open PRs and existing worktrees instead of opening
duplicates. A closed, unmerged PR or a branch whose relationship to the issue is ambiguous is a
stop condition for that issue, not permission to guess.

## 2. Build the dependency and branch graph before dispatch

Build a directed acyclic graph from explicit issue dependencies, cross-cutting order
(`schemas` before server before client), PR bodies, and any existing PR base relationships.
A PR targeting another feature branch depends on that branch and must merge before the parent
branch's final PR can merge upward.

Choose a base for each new issue:

- default to `develop` for an independent issue;
- use the relevant feature branch when the issue is a child of work intentionally assembled there;
- preserve the actual base of every existing PR;
- reject `main` at planning time and verify it again immediately before PR creation and merge.

Pass a non-default base explicitly to the issue agent as
`$dev-issue <issue> --base <branch>`. The base is part of the plan: do not silently retarget an
existing PR to make the graph easier.

When several child PRs need a new aggregate feature base, bootstrap it without opening a parent PR:

1. Choose a non-main `feature/<descriptive-slug>` name and verify it does not already exist.
2. In a temporary worktree named
   `<parent-of-repo>/voxelheim-v2-issue-iteration-<N>-aggregate`, create that branch from the
   current `origin/develop`, verify the worktree root, push the branch, then remove the worktree.
3. Target the child issue agents at that feature base.
4. After every intended child PR has landed, create the aggregate PR from the feature branch to
   `develop`. Scan its complete body with `scripts/check-body-privacy.sh` before posting it, include
   `Closes #N` for every contained issue, then verify its base and body through GitHub.

Use an aggregate branch only when the final combined PR is plausibly below the review cap and the
feature is meaningfully reviewable as a whole. Otherwise use dependency-ordered PRs directly.

Opening the parent only after assembly gives CI and the automatic DeepSeek round the final combined
diff. If a parent PR was already open when a child merge changed its head, wait for fresh CI and
explicitly request one final DeepSeek review of the assembled head before considering the parent
ready.

## 3. Decide the parallel wave

Parallelism is the default, but only for work that is actually independent. Issues may share a
wave when all of these are true:

- neither reaches the other through the dependency graph;
- neither produces a schema or public contract consumed by the other;
- their Code Pointers and expected edit surfaces do not materially overlap;
- merging either first cannot invalidate the other's acceptance criteria;
- their planned PR bases already exist and are not waiting on another wave.

When the evidence is insufficient, serialize the issues and state why. Do not convert uncertainty
into parallel work. Recompute the graph after every merge because a newly discovered implementation
seam or PR body may add an ordering constraint.

Use the **Codex collaboration sub-agent tools** to start exactly one owning agent for each issue in the ready wave, up to
the harness's available concurrency slots. Dispatch that maximal set together so it runs
concurrently; keep the remaining safe issues for the next wave. Give every agent the issue number,
planned base, relevant dependencies, and this instruction:

> Invoke `$dev-issue <issue> --base <planned-base>` and follow it completely. Report every PR
> created (including split or stacked PRs), each PR's base, branch and URL, and any new dependency.
> Do not merge. Remain the owning agent for follow-up `$process-pr` work on those PRs.

Never assign two agents to the same issue. If an issue must be split into several PRs, its owning
agent keeps ownership while the orchestrator adds those PRs and their base edges to the graph.

## 4. Monitor and remediate the open PR set

After an implementation wave returns, monitor all of its open PRs as a set. Before waiting on a
PR, confirm it is still open. Pending CI or an in-progress first DeepSeek review is a wait state,
not a reason to invoke `$process-pr`.

When a PR has actionable CI failures, unresolved DeepSeek threads, or unread DeepSeek body
findings, send its owning agent a follow-up task to invoke `$process-pr <pr>`. Preserve one agent
per issue even when that issue owns multiple PRs. Do not run concurrent force-cycles for multiple
PRs from the same issue, and avoid unnecessary concurrent DeepSeek polling because of GraphQL rate
limits.

Persist the remediation count in public PR comments so a restart cannot reset the safety cap. Before
each force-cycle, count comments carrying
`<!-- develop-iteration:remediation:pr-N:attempt-K -->`, refuse attempt four, scan the new marker
comment with `scripts/check-body-privacy.sh`, and post it before dispatching the owning agent.
Re-read state after every pushed fix. Stop that PR earlier when `$process-pr` reports infrastructure
failure, an unclear finding without an evidence-backed disposition, an unsafe rebase, or another
condition requiring user judgment. Never force an additional DeepSeek round merely to make the
loop move.

## 5. Merge ready leaves in dependency order

Only leaf PRs whose dependencies have landed are candidates. Immediately before every merge:

1. Re-read the PR state, `headRefOid`, base branch and complete body; retain that SHA as
   `OBSERVED_HEAD`.
2. Confirm it is still open and the base is non-empty and not `main`.
3. Enforce every ordering statement in its issue and PR body.
4. Run `bash scripts/gh-automation.sh is-ready-to-merge <pr>` against the current head.
5. If the command succeeds, run
   `bash scripts/gh-automation.sh pr-merge <pr> --head "$OBSERVED_HEAD"`.

`READY TO MERGE` is useful evidence but may lag the current state; never substitute a stale label
for step 4. Conversely, a fresh successful frozen-rule evaluation is sufficient even if the
event-driven labeler has not applied the label yet.

Feature-base branches are not protected by the `develop`/`main` rulesets. For them, the fresh
frozen-rule check is the only required-check guard the orchestrator controls, so skipping it is
never permitted.

After a merge into a feature branch, refresh every open PR whose head is that branch. After a
merge into `develop`, wait for that exact merge commit's `Integration` workflow and require its
`integration-verdict` to succeed before merging another PR into `develop`. A red integration run
is a stop condition: report the generated integration issue and do not compound the broken base.

A child PR merged only into a feature branch does not reliably close its linked issue, and its
`Closes #N` text is not inherited by the eventual parent PR. Keep those issues pending until the
feature branch reaches `develop`, and ensure the parent PR body carries `Closes #N` for every issue
whose commits it contains. For an existing parent PR, write the complete corrected body to a
temporary file, run `scripts/check-body-privacy.sh` on it, use
`scripts/gh-automation.sh pr-edit <pr> --body-file <path>`, then re-read the complete body and all
ordering constraints. Close an issue explicitly only after verifying `develop` contains its commits.

## 6. Continue until the iteration is delivered

After each merge, rebuild the issue/PR graph and dispatch the next maximal safe wave. Continue
until every non-ceremony issue in the selected milestone is closed by a merged PR and no planned
child or parent PR remains open.

Do not close the milestone and do not run the ceremonies created by `iteration-lifecycle.yml`.
Those are product decisions outside delivery.

## Stop conditions

Stop only the affected branch of work when possible; stop the whole run when continuing could
corrupt ordering or the shared base. Report the exact state and safe resume command when any of
these occurs:

- missing required issue fields or ambiguous iteration/dependency data;
- a requested or existing PR base of `main`;
- a dependency outside the iteration that has not landed;
- an infrastructure failure or unavailable GitHub state;
- an unsplittable change above the review cap;
- three unsuccessful remediation cycles for one PR;
- a red post-merge `Integration` run on `develop`;
- a decision that changes iteration scope or requires a new dependency.

## Final report

Report the milestone, issue-to-agent ownership, branch/PR topology, merges in order, CI and
DeepSeek dispositions, the final `Integration` result on `develop`, and any stopped work. If all
issues landed, say that delivery is complete and that the completion-driven ceremony workflow now
owns the next transition.

## References

- Issue implementation: `$dev-issue`
- PR remediation: `$process-pr`
- Frozen status and guarded merge: `scripts/gh-automation.sh`
- Iteration selection and lifecycle: root `AGENTS.md`
- CI and post-merge verification: `.github/workflows/ci.yml`, `.github/workflows/integration.yml`
