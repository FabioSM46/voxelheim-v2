#!/usr/bin/env bash
# Pin the asymmetric acknowledgement policy and the order that makes an AI ack auditable.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import sys
from pathlib import Path

root = Path(sys.argv[1])
processes = tuple(
    (label, (root / path).read_text())
    for label, path in (
        ("Claude process-pr", ".claude/skills/process-pr/SKILL.md"),
        ("Codex process-pr", ".agents/skills/process-pr/SKILL.md"),
        ("OpenCode process-pr", ".opencode/skills/process-pr/SKILL.md"),
    )
)
agents = (root / "AGENTS.md").read_text()
workflow = (root / "docs/WORKFLOW.md").read_text()
protection = (root / ".github/branch-protection.md").read_text()

deprecated = (
    "Both labels are human-only",
    "Never apply `DEEPSEEK_REVIEW_READ` or `NO_DEEPSEEK_REVIEW`",
)
for label, text in processes + (
    ("AGENTS.md", agents),
    ("docs/WORKFLOW.md", workflow),
    (".github/branch-protection.md", protection),
):
    for phrase in deprecated:
        assert phrase not in text, f"{label} restored obsolete shared human-only policy: {phrase}"

for label, text in processes + (
    ("AGENTS.md", agents),
    (".github/branch-protection.md", protection),
):
    assert "NO_DEEPSEEK_REVIEW" in text and "human-only" in text, (
        f"{label} must keep NO_DEEPSEEK_REVIEW human-only"
    )

# The commands are the operational contract in every runtime adapter:
# privacy-check, publish, race-check, refresh, and verify the dated acknowledgement.
for label, process_text in processes:
    start = process_text.index("**Findings in the review body**")
    policy = process_text[start:]
    privacy = policy.index("bash scripts/check-body-privacy.sh")
    comment = policy.index("gh pr comment")
    pre_write = policy.index("LATEST_BEFORE_ACK=")
    remove = policy.index("remove DEEPSEEK_REVIEW_READ")
    verify_removed = policy.index("Stale acknowledgement label is still present")
    add = policy.index("add DEEPSEEK_REVIEW_READ")
    verify_added = policy.index("Could not verify the fresh acknowledgement label")
    post_latest = policy.index("LATEST_AFTER_ACK=")
    post_unread = policy.index("UNREAD_AFTER_ACK=")
    assert privacy < comment < pre_write < remove < verify_removed < add < verify_added < post_latest < post_unread, (
        f"{label} must publish, race-check, refresh, and verify every AI acknowledgement in order"
    )

    for required in (
        "read every",
        "concrete evidence",
        "public audit trail",
        "latest_review_id",
        ".deepseek_unread_findings",
        "repeat this step from the body fetch",
        "acknowledgement is blocked",
    ):
        assert required in policy, f"{label} AI acknowledgement is missing guard: {required}"

helper = (root / "scripts/gh-automation.sh").read_text()
rounds = helper[helper.index("cmd_pr_deepseek_rounds()") :]
assert "reviews(last: 100, states: [APPROVED, COMMENTED])" in rounds, (
    "pr-deepseek-rounds must anchor acknowledgement races to the newest review window"
)

print("DeepSeek acknowledgement policy keeps the exemption human-only and the AI ack audited")
PY
