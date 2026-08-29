#!/usr/bin/env bash
# Pin the asymmetric acknowledgement policy and the order that makes an AI ack auditable.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
process = (root / ".claude/skills/process-pr/SKILL.md").read_text()
agents = (root / "AGENTS.md").read_text()
workflow = (root / "docs/WORKFLOW.md").read_text()
protection = (root / ".github/branch-protection.md").read_text()

deprecated = (
    "Both labels are human-only",
    "Never apply `DEEPSEEK_REVIEW_READ` or `NO_DEEPSEEK_REVIEW`",
)
for text, label in (
    (process, "process-pr"),
    (agents, "AGENTS.md"),
    (workflow, "docs/WORKFLOW.md"),
    (protection, ".github/branch-protection.md"),
):
    for phrase in deprecated:
        assert phrase not in text, f"{label} restored obsolete shared human-only policy: {phrase}"

for text, label in (
    (process, "process-pr"),
    (agents, "AGENTS.md"),
    (protection, ".github/branch-protection.md"),
):
    assert "NO_DEEPSEEK_REVIEW" in text and "human-only" in text, (
        f"{label} must keep NO_DEEPSEEK_REVIEW human-only"
    )

# The commands are the operational contract: privacy-check the public disposition,
# publish it, refresh any stale ack, then add the new dated event. Reordering these
# would let the label claim work whose audit trail did not yet exist.
start = process.index("**Findings in the review body**")
policy = process[start:]
privacy = policy.index("bash scripts/check-body-privacy.sh")
comment = policy.index("gh pr comment")
remove = policy.index("remove DEEPSEEK_REVIEW_READ")
add = policy.index("add DEEPSEEK_REVIEW_READ")
assert privacy < comment < remove < add, (
    "process-pr must privacy-check and publish every disposition before refreshing the ack"
)

for required in (
    "read every",
    "concrete evidence",
    "public audit trail",
    "latest_review_id",
    "deepseek_unread_findings == 0",
):
    assert required in policy, f"process-pr AI acknowledgement is missing guard: {required}"

print("DeepSeek acknowledgement policy keeps the exemption human-only and the AI ack audited")
PY
