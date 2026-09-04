#!/usr/bin/env bash
# Ensure the committed Codex and OpenCode adapters match .claude/skills.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

bash scripts/sync-agent-skills.sh --check

# A newly added canonical skill must exist in both generated harness trees. Checking only the
# names enumerated by the generator lets an omitted skill disappear from both adapters while the
# sync command still reports success.
canonical=$(find .claude/skills -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort)
codex=$(find .agents/skills -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort)
opencode=$(find .opencode/skills -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort)
[ "$canonical" = "$codex" ] || {
  echo "Codex skill set differs from canonical:" >&2
  diff <(printf '%s\n' "$canonical") <(printf '%s\n' "$codex") >&2 || true
  exit 1
}
[ "$canonical" = "$opencode" ] || {
  echo "OpenCode skill set differs from canonical:" >&2
  diff <(printf '%s\n' "$canonical") <(printf '%s\n' "$opencode") >&2 || true
  exit 1
}

# The pipeline skills must stay invocable by an agent, not only by a human typing the
# slash command (legacy PR 48). `disable-model-invocation: true` refuses every model invocation
# — and refusing it did not prevent the work, it routed three agents around the skill
# and into doing the same thing by hand, under a *wider* tool set than the skill's own
# `allowed-tools` list. The launch gate was never the gate that mattered; the frozen
# readiness rule and the `main` prohibition are the gates that matter.
#
# Pinned because this is exactly the line a copy-paste from another repository brings
# back, and its return would be silent: the skill would simply stop answering.
offenders=$(grep -rln "disable-model-invocation" .claude/skills || true)
if [ -n "$offenders" ]; then
  echo "FAIL: disable-model-invocation is back in:" >&2
  echo "$offenders" | sed 's/^/  - /' >&2
  echo "It reserves the skill for a human typist and refuses agent invocation; see legacy PR 48." >&2
  exit 1
fi

process_skill=.claude/skills/process-pr/SKILL.md
iteration_skill=.claude/skills/develop-iteration/SKILL.md
dev_skill=.claude/skills/dev-issue/SKILL.md

# The iteration loop must actively route a conflicting PR into remediation. Merely making the
# frozen rule fail closed leaves the orchestrator waiting forever because a conflicting PR has no
# merge ref and therefore no CI run that could wake it up.
grep -qF 'or `mergeable == CONFLICTING`' "$iteration_skill" || {
  echo "FAIL: develop-iteration no longer routes merge conflicts through process-pr" >&2
  exit 1
}
grep -qF 'A conflict is actionable remediation, not a wait' "$process_skill" || {
  echo "FAIL: process-pr no longer owns base-conflict remediation" >&2
  exit 1
}
grep -qF 'Do this on every remediation run.' "$process_skill" || {
  echo "FAIL: process-pr only reconciles a moved base after it becomes conflicting" >&2
  exit 1
}

# Reusing a worktree must never erase unfinished work. The safe paths are a clean no-op, a
# fast-forward, or a loud stop that preserves the checkout.
if grep -qF 'git reset --hard' "$process_skill"; then
  echo "FAIL: process-pr destructively resets a reused worktree" >&2
  exit 1
fi
grep -qF 'Existing worktree is dirty; preserving it' "$process_skill" || {
  echo "FAIL: process-pr does not preserve a dirty reused worktree" >&2
  exit 1
}

# Review dispositions can unlock a merge, so their mutating commands must live after the source
# push and remote-head verification, not behind a prose instruction to jump around the workflow.
python3 - <<'PY'
from pathlib import Path

text = Path('.claude/skills/process-pr/SKILL.md').read_text()
round_flow = text[text.index('#### 4c — Address the round') :]
push = round_flow.index('git push origin HEAD')
verify = round_flow.index('Confirm the PR is still open at REMOTE_HEAD')
publish = round_flow.index('#### 4f — Publish dispositions after the push')
reply = round_flow.index('comments/<comment-databaseId>/replies')
resolve = round_flow.index('resolveReviewThread')
ack = round_flow.index('add DEEPSEEK_REVIEW_READ')
assert push < verify < publish < reply < resolve < ack, (
    'process-pr can mutate review disposition state before source fixes are published and verified'
)
PY
grep -qF 'if git diff --cached --quiet; then' "$process_skill" || {
  echo "FAIL: process-pr still requires a source commit for a no-code disposition" >&2
  exit 1
}

# A feature base moving invalidates its children's evidence even when their own head SHA did not
# move. Both the orchestration instruction and the merge helper binding must remain visible.
grep -qF 'whose **head or base** is that branch' "$iteration_skill" || {
  echo "FAIL: develop-iteration does not refresh PRs whose feature base moved" >&2
  exit 1
}
grep -qF -- '--base-head "$OBSERVED_BASE_HEAD"' "$iteration_skill" || {
  echo "FAIL: develop-iteration does not bind the observed base head at merge time" >&2
  exit 1
}
if grep -qF -- '--json baseRefOid' "$iteration_skill"; then
  echo "FAIL: develop-iteration requests a gh JSON field unavailable in the pinned CLI" >&2
  exit 1
fi
grep -qF -- "--jq '.base.sha'" "$iteration_skill" || {
  echo "FAIL: develop-iteration does not read the observed base SHA from REST" >&2
  exit 1
}

# AI acknowledgement is evidence-gated, not human-only. NO_DEEPSEEK_REVIEW is the exemption that
# remains human-only.
if grep -qF 'blocks the pull request until a human' "$dev_skill"; then
  echo "FAIL: dev-issue still describes DeepSeek body acknowledgement as human-only" >&2
  exit 1
fi

echo "no skill refuses model invocation"
echo "iteration remediation ordering and worktree safety are pinned"
