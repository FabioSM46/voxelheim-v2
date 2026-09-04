---
name: scrum-master
description: Scrum ceremony facilitator. Use for backlog refinement, iteration planning, and feature specification.
---


# scrum-master — Scrum Ceremony Skill

Triggers: `$scrum-master <ceremony>` where ceremony is one of:
- `backlog-refine` — Backlog refinement
- `iteration-plan` — Completion-driven iteration planning
- `feature-spec` — Feature → technical specification → issues

## Purpose

Manages the Agile development lifecycle for the repository. The user brings feature ideas to the table; this skill listens, drafts technical specifications, creates issues, and organizes the backlog and completion-driven iterations.

## Ceremonies

### 1. backlog-refine

Trigger: `$scrum-master backlog-refine` or run from a ceremony issue.

**Workflow:**

1. Fetch all open issues:
   ```bash
   gh issue list --state open --json number,title,labels,createdAt,updatedAt --limit 100
   ```

2. Filter out ceremony issues (labeled `ceremony`).

3. Categorize each issue against the same required-field contract `$dev-issue` enforces
   (Workspace, Type, Priority, User Story, Acceptance Criteria, Technical Context, Out of Scope),
   reading the **issue body only** — that is the whole of what `$dev-issue` sees, and step 4
   says why:
   - **Ready**: all seven required fields present and substantive in the body → ready for an iteration
   - **Needs Spec**: any required field missing from the body or placeholder-only → needs refinement
   - **Needs Triage**: New/uncategorized → needs initial assessment
   - **Stale**: No activity in 30+ days → flag for closure or re-scoping

4. For each `Needs Spec` issue:
   - Read the issue body
   - Draft acceptance criteria (if missing) based on the user story
   - Draft technical context (if missing) based on the workspace and existing code patterns
   - Draft **Out of Scope** (if missing) — name concrete files/modules/behaviours, never
     "unrelated changes". This is `$dev-issue`'s only defence against scope creep.
   - **Write every drafted field into the issue body. A comment does not deliver it.**
     `$dev-issue` reads the issue with `gh issue view <number> --json title,body,labels` and
     parses the required fields out of the body alone — it never fetches comments. A field
     drafted into a comment is written to a channel the agent it was written for cannot read,
     so the refinement never arrives and nothing turns red to say so (legacy PR 152).

     `gh issue edit --body-file` replaces the **entire** body, so the edit is surgical by
     discipline rather than by the tool: substitute the one named `### <Field>` section and
     reproduce every other byte exactly. Fetch, rewrite, replace, then read the body back and
     confirm the only difference is that section — GitHub appends a single trailing newline,
     and nothing else should differ.

     ```bash
     gh issue view <number> --json body --jq '.body' > /tmp/issue-<number>.md
     # Rewrite that file with the drafted section substituted in place — a
     # `cat > /tmp/issue-<number>.md <<'BODY' … BODY` heredoc, since this skill's
     # allowed-tools carry `cat` and no file-editing tool — then:
     gh issue edit <number> --body-file /tmp/issue-<number>.md
     gh issue view <number> --json body --jq '.body'   # read back and compare
     ```

   - Post a comment as well when a finding deserves to reach a human through their
     notifications — an ordering hazard, a dependency one issue creates for another. The
     comment is the audit trail; the body edit is the delivery. Never the comment alone.

5. Re-prioritize: suggest ordering changes based on:
   - Dependency chains (e.g., schemas before server endpoints before client consumption)
   - Business value (core survival loop before polish)
   - Effort (quick wins mixed with larger features)

6. Update labels as issues graduate: `needs-refinement` → `ready-for-dev` when all seven
   required fields are present and substantive **in the issue body**. A field that lives only
   in a comment graduates nothing: `iteration-plan` selects the next iteration's scope from
   `ready-for-dev`, so a label earned by a comment schedules work that `$dev-issue` stops on
   at its first step — the label says ready and the body is not.

7. Post a summary comment on the ceremony trigger issue (or as a new issue if invoked manually) with:
   - Total open issues
   - Breakdown by category
   - Suggested priority order for the next iteration
   - Issues flagged for closure

8. Close the ceremony issue when complete.

### 2. iteration-plan

Trigger: `$scrum-master iteration-plan` from the milestone-specific ceremony issue created by `iteration-lifecycle.yml`.

**Cold start (fresh repository)**: if NO iteration milestone exists at all — open or closed —
there is no completed milestone to verify and no lifecycle ceremony to wait for. Skip steps 1–4
below: create the undated `Iteration 1` milestone directly, populate it from `ready-for-dev`
(steps 5–8), and post the plan. This is the documented bootstrap path; everything after the
first iteration flows through the completion-driven state machine.

**Workflow:**

1. Resolve the active iteration: the open milestone with the **lowest** sequence, read from an
   `Iteration N` / `Sprint N` title and falling back to the milestone number — the same rule
   `iteration-advance` applies. Fail closed if no milestone is open (outside the cold start
   above), or if two open milestones tie at that **lowest** sequence, because that is the one
   place "lowest" has no answer. A collision higher up is left alone — the active iteration is
   still a single milestone — and it fails closed later, once the collision is itself the
   lowest. `iteration-advance` draws the line in exactly the same place.
   ```bash
   REPO=$(gh repo view --json nameWithOwner --jq '.nameWithOwner')
   gh api "repos/$REPO/milestones?state=open&per_page=100"
   ```

   **Several open milestones is a supported state, not a broken one.** Planning ahead is the
   point: a coherent batch can be committed to a future iteration while the current one is
   still being built, and every milestone above the active one is exactly that. What it
   changes is which half of this ceremony applies — creating and populating an iteration is
   always available; **closing** a completed milestone is what steps 2 and 3 gate.

2. **Closing the completed milestone requires it to hold zero open issues.** Iterations are
   completion-driven; never carry unfinished work automatically and never close a milestone
   that still has any. This is a precondition on step 9 alone. An active iteration with open
   issues does not stop you creating and populating a future one — it only means there is
   nothing to close this time.

3. **Closing it also requires its refinement.** Read the planning ceremony's
   `<!-- iteration-lifecycle:iteration-plan:milestone-N -->` marker and require exactly one
   closed backlog-refinement issue carrying the matching
   `<!-- iteration-lifecycle:backlog-refine:milestone-N -->` marker. Refuse to close if
   refinement is missing, open, or duplicated. A milestone planned ahead has no refinement
   ceremony by construction — nobody has worked it yet — so this gate never applies to
   creating one.

4. Resolve the target sequence — **the one the ceremony's title names: one above the active
   iteration.** Planning ahead means that milestone may already exist, so the target is not
   always something to create:

   - **It already exists and holds work.** The batch was chosen when it was planned ahead.
     Steps 5–7 have nothing left to select and step 8 creates nothing; this ceremony's work is
     step 9, closing the completed milestone. Assign into it only if you are adding to that
     batch.
   - **It does not exist, or exists empty.** This is the ordinary case: select the work and
     create it in step 8.

   Either way, **never create a second milestone on a sequence that already has one** — that
   is the collision `iteration-advance` fails closed on once it reaches the lowest position.
   To plan a further iteration beyond the target, take one above the **highest** existing
   sequence, open or closed, for the same reason. Do not mutate milestone state yet.

5. Fetch prioritized issues from the completed backlog refinement or by priority labels.

6. Select issues for the iteration:
   - Target a realistic work batch for one solo developer
   - Balance across workspaces when the backlog permits
   - Include at least one bug fix or tech debt item when any exists
   - Respect every blocking dependency (schemas → server → client ordering in particular)
   - Require at least one selected issue; if no issue is ready, explain the blocker and leave the planning ceremony and completed milestone open. This binds only when step 4 resolved a milestone to create or add to — a target that was planned ahead and already holds work has nothing left to select, and that is not a blocker

7. For each selected issue:
   - Assign an effort estimate: S (hours), M (1-2 days), L (3-5 days)
   - Check for blocking dependencies
   - Suggest an implementation order

8. Create the undated milestone for the sequence resolved in step 4 (or `Iteration 1` on cold start), assign every selected issue to it, and verify the assignments before closing the completed milestone. This ordering leaves the prior milestone recoverable if creation or assignment fails — and it is why the happy path itself spends a window with two milestones open. When the target milestone already exists because it was planned ahead, assign the selected work into it rather than creating a second milestone on the same sequence.
   ```bash
   for issue in <issue-numbers>; do
     gh issue edit "$issue" --milestone "Iteration <number>"
   done
   ```

9. Close the completed milestone only after the new iteration contains the selected work, and only when steps 2 and 3 are satisfied for it. If the active iteration still has open issues, leave it open: planning ahead closes nothing. (Cold start: nothing to close.)

10. Post the iteration plan as a comment with:
   - Iteration goal (one sentence)
   - Issue list with estimates and workspace
   - Total estimated effort
   - Dependencies and risks noted

11. Close the ceremony issue when complete. Its closure re-runs the lifecycle helper, which should see the newly populated active iteration and make no changes.

### 3. feature-spec

Trigger: `$scrum-master feature-spec <feature-description>`

**Workflow:**

1. Parse the user's feature description against the Game Design Document (`docs/GDD.md`).
   Ask clarifying questions if:
   - Workspace is not specified and not obvious
   - Scope is ambiguous
   - Dependencies are unclear
   - The feature contradicts a GDD pillar (say which, and ask)

2. Draft a technical specification (comment on the conversation):
   - **Summary**: One paragraph describing the feature
   - **Affected Workspaces**: Which parts of the monorepo are touched
   - **Network Contract** (if client↔server data moves): the FlatBuffers tables/messages in `schemas/`, versioning impact
   - **Server Design** (Go): authoritative logic, data flow, goroutine/tick considerations, persistence
   - **Client Design** (Rust/Bevy): ECS components/systems/plugins, rendering or UI impact, prediction/interpolation needs
   - **World & Data Changes** (if any): chunk format, save format, migration needs
   - **Constraints**: server-authoritative (the client never decides gameplay outcomes), determinism where required (world generation), performance budgets (tick rate, frame time), no new dependencies without discussion
   - **Risks**: What could go wrong, what needs extra care

3. Get user approval on the spec. Do NOT proceed to issue creation until the user confirms the spec looks correct.

4. After approval, create issues using the feature request template.

   **The issue body is a contract with `$dev-issue`.** That skill parses these headings and
   treats Workspace, Type, Priority, User Story, Acceptance Criteria, Technical Context and
   Out of Scope as required — matching `.github/ISSUE_TEMPLATE/feature_request.yml`. An issue
   missing any of them stalls the pipeline at the first step. Emit **all seven**, always.

   Dropdown values are fixed strings; use them verbatim:

   | Field | Allowed values |
   |-------|----------------|
   | Workspace | `server (Go Backend)` · `client (Rust Client)` · `schemas (FlatBuffers Contracts)` · `shared (Cross-cutting)` |
   | Type | `feature (New capability)` · `enhancement (Improve existing feature)` · `refactor (Restructure, no behavior change)` |
   | Priority | `high (Blocks other work)` · `medium (Normal priority)` · `low (Nice to have)` |

   For each logical unit of work:

   ```bash
   gh issue create \
     --title "[Feature]: <component description>" \
     --body "$(cat <<'EOF'
   ### Workspace

   <one of the four allowed values, verbatim>

   ### Type

   <one of the three allowed values, verbatim>

   ### Priority

   <one of the three allowed values, verbatim>

   ### User Story

   As a <role>, I want <goal>, so that <reason>

   ### Acceptance Criteria

   - [ ] <criterion 1>
   - [ ] <criterion 2>

   ### Technical Context

   <relevant files, patterns, constraints from the spec>

   ### Out of Scope

   - <what this issue must NOT touch>

   ### Code Pointers

   - `path/to/file.go:42` — <why it matters>

   ### Dependencies

   <blocking issues, or "None">

   ### Test Strategy

   <what to test and at which level>
   EOF
   )" \
     --label "feature" \
     --label "needs-refinement"
   ```

   **Out of Scope is the highest-leverage field you write.** It is the only boundary
   `$dev-issue` has against scope creep, and it is the one most often left vague. Name
   concrete files, modules, or behaviours — not "unrelated changes".

   Bug issues use `.github/ISSUE_TEMPLATE/bug_report.yml` instead, which requires
   Workspace, What happened?, What should have happened?, Steps to reproduce,
   Technical Context and Out of Scope — no Type/Priority/User Story.

5. Group issues logically:
   - **Foundational first**: FlatBuffers contracts, shared types, world/data formats
   - **Core second**: authoritative server logic, main client flows
   - **Polish last**: edge cases, error handling, visual feedback

6. Report to the user:
   - Number of issues created
   - Suggested implementation order
   - Total estimated effort
   - Link to the spec for future reference

## Sizing: an issue is written against the review cap, or it is written wrong

**This is the one place a seam can be chosen with no code sunk at all, and until Iteration 50 this
skill had never heard of the cap.**

`DEEPSEEK_MAX_DIFF_CHARS` is **45,000** characters. A pull request above it is truncated, every
unread file is injected as a finding, and the pull request blocks until somebody acknowledges the
gap. `$dev-issue` Step 5 therefore splits an oversized issue into parts — but splitting is damage
control. By the time `$dev-issue` reads an issue, its shape is already fixed: the acceptance
criteria are one list, and an agent cutting them apart is guessing at a seam somebody else should
have drawn.

The bill for that is measurable. **Iteration 50's first three issues became ten pull requests** —
#849 one, #850 four, #851 five. Each of those parts had then to be reviewed, merged and replayed in
order, seven times, because at the time only a pull request targeting `main` or `develop` got a CI
run at all. #903 removed that constraint and a later part can now be opened directly on the one
before it, so the *serialisation* is gone — but the review rounds, the ordering to keep straight and
the seams somebody had to invent under time pressure are not. Those are what sizing the issue up
front avoids, and they are the reason this section exists rather than a note that splitting is now
cheaper.

### Size every issue you draft or refine

Estimate from the same table `$dev-issue` Step 5 uses, calibrated on this repository's own
measurements:

| What the issue looks like | Measured | Pull requests |
| --- | --- | --- |
| One workspace, one or two new files, no UI | 15,000–35,000 | 1 |
| One workspace, a new module plus its tests | 45,000–60,000 | 2 |
| One workspace, a module *and* a settings/UI surface | 74,000–84,000 | 3+ |
| Two or more workspaces (`schemas` + `server` + `client`) | 95,000+ | 4+ |

Two multipliers the file count hides: changes here run about two-thirds tests, so a module with
real coverage is roughly three times its production code; and a field added to a type constructed
by literal costs every construction site — `SessionParams` has 45, which was 14,000 characters
before any behaviour existed.

**The Parts column is not the estimate divided by the cap.** 74,073 over 45,000 is 1.6 and #851
took five. Seams are discrete — you cut where the code already draws a boundary, and #851's parts
came out around 16k, 44k, 49k, 36k and 42k because that is where its module edges are — and an
estimate made before any code exists is systematically low: #851 was estimated whole at 74,073 and
its fifth part *alone* measured 80,534 once written. Treat the arithmetic as a floor on the count
and let the seams decide the number. `$dev-issue` Step 5 carries the same warning, for the agent
that reads the estimate you wrote.

### What to do when an issue is over

**Prefer splitting the issue.** Two issues, each one pull request, each with its own acceptance
criteria and its own `Dependencies` line, is strictly better than one issue and a note: the
milestone then counts the real work, `$dev-issue` runs each without a judgement call, and the seam
was chosen by whoever understood the feature.

**When the work genuinely is one issue** — a contract and both its consumers, say — keep it whole
and write the seam into the body under a heading `Suggested split`, naming each part, what it
contains, and the order they must merge in. `$dev-issue` will follow a stated seam rather than
invent one. Say how many parts: there is no limit of two, and #851 needed five.

**Never split on a character count.** The seam is a boundary the code already draws — the wire and
its consumer, the mechanism and its callers, a decision and the wiring that carries it. A split
made to hit a number leaves parts that each read as an excerpt of something else.

### Say the estimate out loud

Put the estimate and its reasoning in the issue body, or in the refinement comment for an issue you
did not author. **An estimate nobody records is one nobody can be shown to have skipped** — which
is exactly how `$dev-issue`'s identical instruction went unfollowed three times in Iteration 50
while being the first sentence of its Step 5.

## Guardrails

- Never create issues without user confirmation on the spec
- Never modify closed issues or closed milestones
- Respect workspace boundaries: don't create server issues for a client-only feature
- A feature that moves data between client and server ALWAYS has a schemas component — surface it explicitly rather than burying contract changes in a server issue
- **Size every issue against the 45,000-character review cap before it is committed to a milestone** — see "Sizing" above. A cross-workspace feature is never one pull request, and an issue that needs five parts should say five.
- When in doubt about scope, ask — don't assume
- Ceremony issues (labeled `ceremony`) are excluded from all iteration/backlog reports

## Reference

- Game design: `docs/GDD.md`
- Issue templates: `.github/ISSUE_TEMPLATE/`
- Issue conventions: `docs/ISSUE_CONVENTIONS.md`
- Pipeline: `AGENTS.md` (root)
- The review cap and what it costs: `AGENTS.md`, "Automated PR Review (DeepSeek)"; `$dev-issue` Step 5
- Shared helpers: `scripts/gh-automation.sh`
