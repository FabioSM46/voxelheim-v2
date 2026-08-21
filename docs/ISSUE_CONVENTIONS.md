# Issue Conventions — LLM-Optimized Format

Every issue must provide enough context for an AI agent to implement it without asking
clarifying questions. This document defines the required structure and the branch/worktree
workflow.

## Required Fields (LLM-Critical)

These fields **must** be filled in for every issue. The AI agent reads them literally — no
implicit knowledge, no assumed context.

| Field | Why the LLM needs it |
|-------|---------------------|
| **Workspace** | Determines which AGENTS.md to read and which directory to work in |
| **Type** | Maps to branch prefix: `feature/`, `fix/`, `refactor/` |
| **Priority** | Informs effort tuning; `high` gets more thorough review |
| **User Story** | The 3-line goal statement the implementation must satisfy |
| **Acceptance Criteria** | Verifiable checklist — LLM uses this as its test plan |
| **Technical Context** | The primary implementation guide: files, patterns, constraints |
| **Out of Scope** | Hard boundaries the LLM must NOT cross — prevents over-engineering |

## Field Writing Guidelines

### User Story
```
As a <specific role>
I want <concrete action>
So that <measurable outcome>
```
Bad: "As a player I want better combat" (vague)
Good: "As a tank wearing heavy armor I want nearby mobs to prefer attacking me so that my party's healer survives dungeon pulls"

### Acceptance Criteria
Each item must be **binary testable** (pass/fail). No subjective items.
```
- [ ] Mobs within 12 blocks re-target the player with the highest aggro score
- [ ] Heavy armor pieces each add their listed aggro multiplier
- [ ] Aggro decays to zero within 10 seconds of leaving combat
```
Bad: `- [ ] It works well` (subjective)
Bad: `- [ ] Combat feels better` (not testable)

### Technical Context
Use subsections. Be specific about file paths and line numbers.
```
### Files to Modify
- server/internal/combat/aggro.go (add gear-based multiplier)

### Patterns to Follow
- Stat aggregation pattern from server/internal/combat/stats.go:45

### Constraints
- MUST be computed server-side (client never decides targeting)
- MUST NOT add new Go modules
```

### Out of Scope
Be explicit about what NOT to do. This is the LLM's safety rail.
```
- Do NOT touch the healing threat rules (separate issue)
- Do NOT change the mob AI state machine
- Do NOT modify schemas/ (no wire format change needed)
```

### Code Pointers
Exact `file:line` references. The LLM reads these files first.
```
- server/internal/combat/aggro.go:15 (current target selection)
- server/internal/combat/stats.go:42 (gear stat aggregation)
- schemas/combat.fbs:10 (combat state tables)
```

## Branch Naming Convention

```
<type>/<issue-number>-<short-slug>
```

| Issue Type | Branch Prefix | Example |
|-----------|---------------|---------|
| `feature` | `feature/` | `feature/42-add-rune-key-portals` |
| `enhancement` | `feature/` | `feature/43-improve-chunk-streaming` |
| `refactor` | `refactor/` | `refactor/44-extract-mesh-builder` |
| `bugfix` | `fix/` | `fix/45-chunk-seam-lighting` |

**All branches are created from `develop`.** PRs target `develop`.

When authorized work has no GitHub issue, do not invent an issue number. Use
`<type>/<short-descriptive-slug>` instead, for example `refactor/graphify-gitignore`.

**Slug rules:**
- Derived from issue title
- Lowercase, hyphens only
- Maximum 5 words
- No special characters

## Worktree Workflow

The dev-issue skill creates isolated git worktrees for parallel issue work.
Issue-driven work uses `voxelheim-issue-<number>`. If no issue exists, use the same short,
descriptive slug as the branch: `voxelheim-issue-<short-descriptive-slug>`.

```
Directory layout:
  ~/repo/hub/
    voxelheim/                 ← Main repo (develop stays clean)
    voxelheim-issue-42/        ← Worktree for issue #42
    voxelheim-issue-43/        ← Worktree for issue #43
    voxelheim-issue-graphify/  ← Ad-hoc work with no issue
```

**Create:**
```bash
git worktree add -b <branch-name> ../voxelheim-issue-<number> origin/develop
cd ../voxelheim-issue-<number>
```

**Remove (after PR is open):**
```bash
cd ~/repo/hub/voxelheim
git worktree remove ../voxelheim-issue-<number>
git worktree prune
```

## Anti-Patterns (What NOT to Write)

| Anti-pattern | Why it fails | Fix |
|-------------|-------------|-----|
| "Fix the bug" | No context, no reproduction | Include steps + error logs |
| "Update the netcode" | No file path, no line number | Use `server/internal/net/session.go:42` |
| "Make it better" | Subjective, no goal | Define measurable acceptance criteria |
| "It doesn't work" | No reproduction steps | Step-by-step with exact inputs (seed, coordinates) |
| Missing Out of Scope | LLM adds features you didn't want | Always list what NOT to do |
| "See Discord thread" | LLM can't access Discord | Paste relevant context in the issue |
| Vague pronouns ("it", "this") | LLM can't resolve references | Use concrete nouns and exact names |
| Hiding a wire-format change in a server issue | The contract fan-out gets skipped | Name the `schemas/` impact explicitly |

## Quality Gates (Automated)

Every PR created from an issue must pass these gates before the PR is opened
(they mirror `.github/workflows/ci.yml` — see `AGENTS.md` for the full table):

- [ ] **Formatting** — `gofmt` / `cargo fmt --check` clean
- [ ] **Lint** — `go vet` / `cargo clippy -D warnings` pass
- [ ] **Build** — `go build` / `cargo build --locked` succeed
- [ ] **Tests** — `go test` / `cargo test --locked` green
- [ ] **Schemas** — `scripts/check-schemas.sh` passes and bindings are regenerated (when touched)
- [ ] **No secrets** — No API keys, tokens, or credentials committed
- [ ] **No debug prints** — Production code paths are clean
