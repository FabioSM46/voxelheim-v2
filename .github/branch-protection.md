# Merge Protection Configuration

The merge contract for `main` and `develop` is enforced by two GitHub **rulesets**, both `active`.
This file records what they enforce, why there are two of them, and how to recreate them.

> **Rulesets, not classic branch protection.** The two were created while the repository was
> private and classic branch protection was unavailable on its plan: the classic API answered
> `404 Branch not protected`, while the rulesets API accepted both configurations. If you are
> looking for these rules in the UI, they live under **Settings → Rules → Rulesets**. A repository
> visibility change can alter rule enforcement; follow the checks in
> [`docs/PUBLIC_REPOSITORY.md`](../docs/PUBLIC_REPOSITORY.md) immediately after publication.

## What is enforced

| Ruleset | Rule | Who can bypass |
| ------- | ---- | -------------- |
| `PR required — unresolved review threads block merge` | `pull_request`: `required_review_thread_resolution: true`, `required_approving_review_count: 0` | **nobody** |
| `ci-gate must pass (admin can bypass)` | `required_status_checks`: `ci-gate` | repository admin (`bypass_mode: always`) |

Both target `refs/heads/develop` and `refs/heads/main`.

In practice:

- **An unresolved review thread makes the merge button unavailable to everyone**, including the
  repository owner. Read the finding, fix it or reject it, resolve the thread — there is no
  override.
- **A red, pending or missing `ci-gate` blocks the merge, but the admin sees a bypass option** and
  can merge anyway with an explicit acknowledgement. This is the escape hatch for the cases where
  CI cannot be green for reasons outside the diff.

## Why two rulesets

**A bypass list belongs to a ruleset, not to a rule.** One ruleset containing both rules would
make both blocks bypassable or neither — the asymmetry above is only expressible by splitting
them, so the strict rule sits in a ruleset with an empty `bypass_actors` and the overridable one
sits in a ruleset that lists the admin role. The API reports this back per ruleset as
`current_user_can_bypass`: `never` for the first, `always` for the second.

## Consequences worth knowing before you hit them

- **The `pull_request` rule also blocks direct pushes to `develop` and `main`, for everyone, with
  no bypass.** That matches the pipeline's existing hard rule ("all work targets `develop`
  through a PR"), but it now fails at the remote instead of relying on discipline. A local commit
  straight onto `develop` will be rejected on push.
- **`required_approving_review_count` is 0 on purpose.** GitHub does not let an author approve
  their own pull request, so on a solo repository requiring one approval would make every PR
  unmergeable except by bypass — turning the review requirement into a rubber stamp on the bypass
  checkbox. The human merge action *is* the review here.
- **`strict_required_status_checks_policy` is false** (branches need not be up to date with the
  base). With several PRs open in parallel, requiring it would mean re-pushing every branch after
  each merge for a check that a red `ci-gate` would catch anyway. Turn it on if the parallel waves
  ever start producing semantic conflicts that CI misses.
- **Only `ci-gate` is required.** `labeler` and `review` stay out deliberately: DeepSeek
  legitimately skips runs once its round cap is spent, so requiring `review` would deadlock any PR
  whose second push produced no review. `ci-gate` is the one stable check, and it owns the
  branch-aware workload rule:
  - `develop`: changed-area-selected workspace jobs and the automation job must succeed;
    explicitly unselected jobs may report `SKIPPED`.
  - `main`: every workspace that exists at the ref, plus the automation job, must run and succeed.
    A selector may be false only because the workspace is not scaffolded yet; `SKIPPED` is
    otherwise not accepted.

## Why `READY TO MERGE` is still not redundant

The ruleset is a floor, not a replacement for the frozen acceptance rule. GitHub's native
unresolved-conversation rule counts review **threads**, and that is only half of a DeepSeek
review: findings delivered as general comments live in the review **body** and create no thread at
all. A review made entirely of them scores zero unresolved threads while holding real findings,
and GitHub would let it merge.

`pr-status-json` counts those separately as `deepseek_unread_findings`, acknowledged by the dated
`DEEPSEEK_REVIEW_READ` label. A human may apply it after reading the findings; the AI may apply it
only after addressing or evidence-backed rejecting every body finding and publishing the resulting
audit trail on the PR. A stale acknowledgement must be removed and re-applied after the review.
`NO_DEEPSEEK_REVIEW` remains a human-only exemption. So:

- the **ruleset** guarantees nobody merges over an unresolved thread or a red `ci-gate` by accident
- the **`READY TO MERGE` label** additionally guarantees the review was read, the round is
  finished, and the PR is genuinely mergeable

Merge on the label; the ruleset is what catches you when you don't.

## Recreating the configuration

Both rulesets were created through the API and can be recreated on a fresh repository with the
payloads below. Verify afterwards that the reported `current_user_can_bypass` is `never` for the
first and `always` for the second — that field is the asymmetry, and it is the one thing worth
checking by hand.

```bash
gh api repos/OWNER/REPO/rulesets -X POST --input - <<'JSON'
{
  "name": "PR required — unresolved review threads block merge",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [],
  "conditions": { "ref_name": { "include": ["refs/heads/develop", "refs/heads/main"], "exclude": [] } },
  "rules": [
    { "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": false,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true
      }
    }
  ]
}
JSON

gh api repos/OWNER/REPO/rulesets -X POST --input - <<'JSON'
{
  "name": "ci-gate must pass (admin can bypass)",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [ { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" } ],
  "conditions": { "ref_name": { "include": ["refs/heads/develop", "refs/heads/main"], "exclude": [] } },
  "rules": [
    { "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": false,
        "required_status_checks": [ { "context": "ci-gate" } ]
      }
    }
  ]
}
JSON
```

`actor_id: 5` with `actor_type: RepositoryRole` is the repository **admin** role.

## Safe rollout

Do not require `ci-gate` before it has reported on a real PR head — a required check that has
never run pins every PR at "expected, waiting". The safe order, already followed here, is:

1. Merge the pipeline bootstrap and confirm a PR to `develop` reports a successful `ci-gate`.
2. Only then create the second ruleset.

The workload jobs (`server`, `client`, `schemas`, `automation`) stay visible for diagnostics; the
enforced contract is the single `ci-gate` check.

## Verifying enforcement

Enforcement is worth confirming rather than assuming, and a live PR is the only honest test:

```bash
gh api repos/OWNER/REPO/rulesets --jq '.[] | "\(.name) → \(.enforcement)"'
gh pr view <number> --json mergeable,mergeStateStatus
```

`mergeable: MERGEABLE` together with `mergeStateStatus: BLOCKED` is the signature of a rule
holding the merge back — the PR itself is clean, the policy is what says no. That is exactly the
state legacy PR 9 was in with `ci-gate` green and three unresolved review threads.
