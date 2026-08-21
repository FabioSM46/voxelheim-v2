# CLAUDE.md

Please refer to and follow the instructions listed in the @AGENTS.md file at the project root.

This is a **public repository**. Never put a real email address, personal data, machine username,
internal filesystem path, private hostname, secret or credential in a tracked file, commit, branch,
PR, review, CI log or artifact. Use GitHub `noreply`, reserved example values and placeholders such
as `<repo-root>`. The only approved public identity is the attribution handle `@FabioSM46` in
`NOTICE`. Before every push run `bash scripts/check-publication-privacy.sh` over the tracked
tree and `bash scripts/check-commit-privacy.sh <base> <head>` over the commits the branch adds —
a commit message is a published surface too, and no `Claude-Session:` trailer may reach one.

Quick reference — the pipeline skills:

- `/dev-issue <number>` — implement a GitHub issue end-to-end (worktree → gates → PR)
- `/process-pr [number]` — force-cycle an open PR (DeepSeek feedback + CI fixes)
- `/scrum-master feature-spec | backlog-refine | iteration-plan` — scrum ceremonies

Hard rules that override everything else: work only in git worktrees, branch from `develop`,
never push or merge to `main`, never read `.env` files, never hand-edit `gen/` code, and the
server is authoritative — the client never decides gameplay outcomes. Privacy failures block a
push even when every build and test is green.
