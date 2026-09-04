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

echo "no skill refuses model invocation"
