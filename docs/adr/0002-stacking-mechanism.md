# ADR 0002 — Retain hand-built stacks pending a bottom-first experiment

- **Status**: accepted — retain the existing workflow; neither alternative adopted
- **Date**: 2026-09-05
- **Issue**: #926
- **Prerequisite**: #925, completed; review cap now 90,000 characters at `high`

## Decision

Keep the current hand-built, leaf-first collapse and its guarded, one-PR-at-a-time merges.
Do not install, require or link PRs with native `gh stack`. Evaluate the dependency-free
alternative first: hand-built stacks merged bottom-first with **merge commits**, through
the existing `pr-merge --merge` path. That is the candidate for a future declared experiment,
not permission to change the direction of an existing stack.

This is a decision to retain the status quo with a known cost, not a finding that leaf-first
is cheaper. The evidence for its diff inflation is direct. The alternative's ancestry
argument is sound, but its complete GitHub delivery sequence has not been exercised here on
a small real issue. #926 requires that experiment before adoption spreads. No alternative is
adopted by this ADR, so no real-issue experiment is claimed and no unrelated issue is used as
one. A later ADR can replace this decision with the result of that experiment.

## Evidence and scope of the comparison

The [#852 stack's final leaf](https://github.com/FabioSM46/voxelheim-v2/pull/924) names ten PRs,
#915–#924, and explicitly orders them leaf-first. The
[#920 disposition](https://github.com/FabioSM46/voxelheim-v2/pull/920) records its own opening
diff at **11,302 characters**, then **140,957** after its descendants were folded in. The
assembled review omitted nine files and required a public `DEEPSEEK_REVIEW_READ` audit.
The #924 remediation comment separately records a fresh pre-merge check catching an unresolved
thread after an earlier green read. #926 records unread body findings on #920 as the other
pre-merge stop. Neither a remembered label nor an earlier clean review would have been enough.

These are different measurements from the later #925 replay table in `AGENTS.md`: that table
uses the files API and records **139,626** characters for the assembled #920 replay. Do not
replace the 140,957-character merge-time observation with that later API measurement, or call
either measurement the size of #920's original layer. Likewise, the claim that bottom-first
would have kept #920 at exactly 11,302 is a counterfactual: fixes and conflict resolutions can
change a layer. The justified claim is that descendants would not be folded into its diff.

#925 is the root-cause work and precedes this mechanism decision. Seventeen replays at `high`
supported raising the cap from 45,000 to 90,000. That reduces pressure to split ordinary issues;
it does not establish a measured future stack frequency, and 140,957 still exceeds 90,000.
The cap, truncation behavior and acknowledgement rule are unchanged here.

## Option (a), evaluated first: hand-built stacks, bottom-first

Define bottom as the PR nearest `develop`. For `develop <- A <- B <- C`, bottom-first means
merge A into `develop`, then B into `develop`, then C into `develop`. Retargeting and refreshing
the next PR are part of the operation; changing only which merge button is pressed is not enough.

### Preserve ancestry with the existing merge-commit option

`cmd_pr_merge` already accepts `--merge` as well as its default `--squash`. Repository settings
read for this decision allow merge commits, squash and rebase merges. There is no new binary,
dependency or helper contract needed to request a merge commit.

If A is merged with a merge commit, A's actual commits remain ancestors of `develop` and B.
After B is retargeted to `develop` and that base is merged into B, the three-dot diff excludes
A and contains B's own layer. C continues to target B; merge B's refreshed head into C before
using C's checks again. When B later lands in `develop`, repeat the retarget-and-refresh for C.
No descendant is merged into its parent PR, so there is no structural accumulation of upper
layers in a waiting lower layer. This is a Git ancestry argument, not a measured GitHub trial.

The cost is merge commits in integration history, serial base refreshes and fresh checks.
Conflict resolution still costs work, but it happens while merging the base, in the direction
`process-pr` already prescribes; it does not require rewriting published commits. Existing
worktree ownership must be respected when refreshing each branch. Each lower part must be safe
to release to `develop` on its own: compiling is insufficient if it temporarily breaks a
runtime contract that a later part repairs. An aggregate feature intentionally delivered as
one unit cannot be converted by merely reversing the order in its PR descriptions.

The existing guards remain applicable: read both SHAs, run `is-ready-to-merge`, then use
`pr-merge <pr> --merge --head <observed-sha> --base-head <observed-base-sha>`. Retarget through
`pr-edit` before the refresh push, so the new head triggers CI against the intended base.
Re-read the actual base, review bodies and threads after each refresh. Wait for the exact
`Integration` result after every merge into `develop`, as the orchestrator already requires.
The no-`main` refusal is preserved because every merge still passes through the helper.

### Keeping squash instead is a different option

A squash of A creates a new commit that B does not contain. Simply retargeting B does not make
the old A commits ancestors of the new base; a three-dot diff can still include A. A controlled
replay must exclude the old parent history, for example by rebasing B's own commits from its
recorded old parent tip onto the new `develop` tip, then restacking C using its recorded old
parent boundary. Review fixes and base-merge commits make those boundaries worth recording
explicitly. A blind rebase onto the current base is not a defined replay.

Publishing that replay rewrites branch history and needs the explicit authorization currently
required by `dev-issue` and `process-pr`, plus leases bound to the old remote tips. All prior
readiness observations are invalidated. This is the manual replay cost recorded for #455 and
#457 in Iteration 30. Choose **merge commits** for the first bottom-first experiment; retaining
squash gives up the strongest reason to try option (a).

### Verdict on (a)

Best candidate to evaluate next: it fixes structural inflation without an extension, preserves
the designated guard and can avoid force-pushes. It is not blocked by the helper or the current
repository merge settings. Adoption is withheld pending the required real-issue experiment,
including the cost of serial checks and evidence that each layer is safe on `develop` alone.
The orchestrator's current child-before-parent rule would need an explicit change after that
experiment; this ADR does not silently override it.

## Option (b): native GitHub stacks and `gh stack`

GitHub documents native stacks as a public preview. Layers must share a repository, which this
project already does. Native stacks propagate trunk merge requirements to their layers and
automatically rebase remaining branches after bottom-first merges; merge commits, squash and
rebase merging are supported. Programmatic stack merging uses an asynchronous API. Those are
useful features, but native branch protection does not implement this repository's unread
DeepSeek-body rule. [GitHub's stack model](https://docs.github.com/en/pull-requests/get-started/about-stacked-prs)

The CLI reference specifies `gh >= 2.0`; the installed 2.45.0 satisfies that documented minimum.
`gh extension list` returned no extensions. The missing command is therefore not evidence that
2.45.0 is too old. The reference says stack pushes use per-branch leases with potentially partial
success, while stack merge checks basic PR state and relies on GitHub for merge requirements.
[CLI reference](https://docs.github.com/en/pull-requests/reference/stacked-prs-cli-commands)

Automatic restacking reduces manual replay work and avoids folding descendants into waiting
parents. It still incurs conflict recovery and revalidation of changed heads. Local lease
checks protect branch updates, not the freshness of a review verdict. An automatically changed
child must never inherit authorization to merge from its old SHA.

Replacing `pr-merge` with `gh stack merge` would bypass the designated non-`main` refusal and
the current head/base binding. A multi-PR operation would also bypass the orchestrator's pause
for each `develop` integration result. Trunk rules cannot substitute for the custom frozen rule
that caught #920's unread findings. Calling the stack merge command after checking the whole
stack once is therefore not an equivalent integration.

A future integration would need a guarded, single-bottom-PR operation, a verified non-`main`
trunk, explicit merge method, observed-head/base checks, and recovery that stops on a changed
ref or a partial restack. It must re-evaluate CI, threads and review bodies at the new state
before the next merge. No claim is made here that the preview API can enforce all of those
conditions atomically; that is work required before recommending it.

**Verdict: do not recommend or adopt `gh stack`.** Option (a) captures the inflation fix with
less integration work. Preview behavior and the new merge path add costs with no measured
benefit over that candidate in this repository. No version is selected or installed and the
README Toolchain table stays unchanged. A future recommendation must pin a release of the
[official extension](https://github.com/github/gh-stack), record its verified `gh` requirement,
and exercise the guarded integration on one declared issue before wider use. Pinning a CLI
release alone cannot freeze GitHub's server-side preview behavior.

## Option (c): status quo, hand-built stacks collapsed leaf-first

The leaves merge inward through `pr-merge`, squash by default. This avoids published-history
replays and lets an incomplete feature reach `develop` only as an assembled unit. It keeps the
existing no-`main` guard, head/base checks, review-body audit and base-refresh workflow.

The price is structural inflation: merging C into B makes B's waiting PR contain B plus C;
merging that into A makes A contain the full feature. Fresh CI and the final assembled-head
review remain necessary. A reviewer having read each small part is not equivalent to having
read their combination. #920 is direct evidence of that price, and the higher cap cannot
eliminate it for an arbitrarily large aggregate. The orchestrator already permits aggregate
bases only when their final combined PR is plausibly within the review cap; retaining this
workflow does not grant an exception to that rule.

**Verdict: retain for now**, because it is the exercised delivery path and the alternative
has not passed #926's real-issue experiment. This accepts refresh and aggregate-review cost;
it does not justify repeating a ten-part stack whose final combined diff is known to exceed
the cap. When a combined change cannot be reviewed within that bound, choose independently
deliverable work or report the planning constraint rather than promising that tiny leaves
will keep the parent tiny.

## Comparison against Iteration 50

| Option | Diff inflation | Replay and verification cost | Existing guards disturbed |
| --- | --- | --- | --- |
| (a) Bottom-first, hand-built, merge commits | Descendants do not accumulate in waiting parents; fixes may still grow a layer | No published-history replay; retarget, merge base, run fresh checks and wait for Integration at each step | Helper and no-force-push remediation fit; orchestrator ordering must change; standalone runtime safety must be verified |
| (a), retaining squash | Requires deliberate removal of old parent history to isolate each remaining layer | Manual cascading replay, conflicts, leased force-pushes and fresh checks; Iteration 30 records this cost | Explicit rewrite authorization and replay policy needed; helper remains the merge path |
| (b) Native stacks | Automatic restacking keeps layers separate | Tool/server automate replay; conflicts, changed-head validation and partial local pushes still need recovery | New merge API, auto-rewrites and multi-PR merges need integration with custom readiness, SHA checks, no-`main` refusal and Integration sequencing |
| (c) Current leaf-first | Observed #920: 11,302 to 140,957; nine unread files | No rebase replay; repeated base refreshes and final aggregate review/audit | No contract change; already caught late thread and body findings |

## Explicit disposition of the three conflicts

1. **Force-push versus fresh readiness:** (a) with merge commits avoids the rewrite; (a) with
   squash needs explicit rewrite authorization; (b) is blocked on guarded restack/merge
   integration. For all candidates, changed head or base invalidates prior readiness. Preserve
   the frozen rule and both SHA observations for each merge. The existing head match is checked
   by GitHub; the base comparison is a preflight read, with a remaining race before the merge
   endpoint. Neither this ADR nor the preview is evidence that this race has disappeared.
2. **Squash versus child history:** retain squash for the current leaf-first workflow; choose
   merge commits for the proposed manual bottom-first experiment. Native squash support is
   documented, but no local trial establishes its recovery and guard behavior here. Do not
   pretend a squash is the original parent commit or quietly change the helper's default.
3. **Tooling and stability:** no new tool now. Installed `gh` meets the documented extension
   minimum; lack of an installed extension and public-preview integration risk are separate
   facts. Same-repository eligibility is satisfied. A future native recommendation owes a
   pinned extension, verified toolchain entry and experiment result, not an unpinned install.

## Experiment required to revisit this decision

This is a protocol for future evaluation, **not an experiment result or an adoption**. Use one
small real issue that naturally has two independently deliverable parts, and declare the
experiment in its issue and both PR bodies before merging. Do not repurpose an active stack
with different ordering promises or split this comparison merely to manufacture a trial.

For option (a), record the exact branches and PRs, original parent boundaries, method `--merge`,
bottom-first direction and owner. Keep the child branch intact when its parent lands. Retarget
the child before its refresh push, merge the new base into its head, and retain the helper's
fresh frozen-rule and observed-SHA sequence. Record reviewable sizes at opening, after refresh
and immediately before merge, conflict work, number/duration of checks, review dispositions
and both exact `Integration` verdicts. Verify the refreshed child's diff contains its own work
and deliberate conflict resolutions, with no duplicated parent or folded-in descendant work.

Stop the experiment on an unexpected rewrite, stale or unreadable guard input, ambiguous
conflict, wrong base, or failed integration. Preserve the existing refs and record the failure;
do not recover by bypassing the helper. Write up success or failure on that issue before any
further experimental use. Only then update the global rule and the canonical `dev-issue`,
`develop-iteration` and `process-pr` instructions as needed, regenerate both runtime adapters,
and run their gates together. A successful ancestry check alone does not satisfy this protocol.

## Verification for this decision

This PR changes documentation only. `scripts/test/pr-merge-guard.test.sh` verifies the existing
positive and negative contract; `scripts/test/agent-skills-sync.test.sh` verifies the unchanged
runtime adapters. Publication and commit/body privacy checks apply before publication. No
gameplay workspace, schema, dependency, merge helper or review configuration is modified.
