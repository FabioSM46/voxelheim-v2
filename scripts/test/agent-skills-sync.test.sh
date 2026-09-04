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

process_skill=.claude/skills/process-pr/SKILL.md
grep -qF '[ "$(echo "$CURRENT_PR" | jq -r '\''.state'\'')" = "OPEN" ] || exit 1' "$process_skill" \
  && grep -qF '[ "$(echo "$CURRENT_PR" | jq -r '\''.headRefOid'\'')" = "$REMOTE_HEAD" ] || exit 1' "$process_skill" || {
  echo "FAIL: process-pr publication guard does not fail closed without ambient set -e" >&2
  exit 1
}

echo "no skill refuses model invocation"
