# Public Repository Policy

Voxelheim publishes its source for transparency, inspection and personal experimentation. External
users have no direct write access; proposed changes travel through pull requests to `develop`.

## Contribution boundary

- Keep direct write access limited to maintainers. Pull requests target `develop`; opening one
  never grants repository access, and maintainers retain the decision to accept or close it.
- Keep issues enabled because the project pipeline uses issue templates and milestone ceremonies.
- Public repositories remain cloneable and forkable. GitHub does not provide a switch to disable
  forks of a public repository. A fork does not grant write access to this repository.
- Security reports must follow [the private reporting policy](../.github/SECURITY.md), never a
  public issue.

The source is licensed under Apache-2.0. Redistributed copies and derivative works must retain a
readable copy of the attribution in [`NOTICE`](../NOTICE), as required by section 4(d) of the
license. Repository visibility, forking and pull-request access do not change those license rights
and do not require this repository to accept external contributions.

## Repository settings

The intended configuration is:

| Setting | Required value |
| --- | --- |
| Default branch | `develop` |
| Pull requests | enabled; base branch `develop` |
| Issues | enabled |
| Wiki | disabled |
| Default `GITHUB_TOKEN` permissions | read-only |
| Actions may approve pull requests | disabled |
| Allowed Actions | selected allowlist only |
| Require full-length commit SHA for Actions | enabled after the pinned workflows reach `develop` |
| Private vulnerability reporting | enabled when the repository is public |
| Dependabot alerts | enabled; version and security update PRs disabled |
| Secret scanning and push protection | enabled when available |
| Commit author and committer email | GitHub `noreply` address only |
| Personal data and internal paths | prohibited in Git, PRs, reviews, logs and artifacts |

The Actions workflows pin every third-party action to a full commit SHA and retain the release
name in a same-line comment for manual update auditing. Checkout credentials are not persisted in
the workspace. CI checks tracked content for personal email addresses and common workstation paths,
and checks every commit introduced by a pull request for author or committer addresses outside
GitHub's `noreply` domains. Diagnostics name only the location and category; they never echo the
rejected value into the public log.

## Visibility-change checklist

Changing a private repository to public can disable rules that GitHub Free does not support on a
private repository. Perform these checks immediately after changing visibility:

1. Confirm both rulesets documented in [branch-protection.md](../.github/branch-protection.md) are
   `active` and still target `develop` and `main`.
2. Confirm `ci-gate` is required and unresolved review threads block merges.
3. Reapply the repository settings in the table above and enable the public-only security
   features.
4. Confirm the repository exposes no Actions logs, artifacts, releases or Pages output containing
   credentials.
5. Run the secret scan against every reachable Git ref, not only the current working tree.

Never put a real token into a command line, issue, pull request, Actions input or configuration
file while performing these checks.
