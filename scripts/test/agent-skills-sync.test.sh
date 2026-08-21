#!/usr/bin/env bash
# Ensure the committed Codex and OpenCode adapters match .claude/skills.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

bash scripts/sync-agent-skills.sh --check

# The pipeline skills must stay invocable by an agent, not only by a human typing the
# slash command (#48). `disable-model-invocation: true` refuses every model invocation
# — and refusing it did not prevent the work, it routed three agents around the skill
# and into doing the same thing by hand, under a *wider* tool set than the skill's own
# `allowed-tools` list. The launch gate was never the gate that mattered; the human
# merge gate is, and that one is untouched.
#
# Pinned because this is exactly the line a copy-paste from another repository brings
# back, and its return would be silent: the skill would simply stop answering.
offenders=$(grep -rln "disable-model-invocation" .claude/skills || true)
if [ -n "$offenders" ]; then
  echo "FAIL: disable-model-invocation is back in:" >&2
  echo "$offenders" | sed 's/^/  - /' >&2
  echo "It reserves the skill for a human typist and refuses agent invocation; see #48." >&2
  exit 1
fi

echo "no skill refuses model invocation"
