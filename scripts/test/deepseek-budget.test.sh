#!/usr/bin/env bash
# Pin DeepSeek's model, reasoning, output, request, job and manual-wait settings as one contract.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import ast
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
workflow = (root / ".github/workflows/deepseek-pr-review.yml").read_text()
reviewer_source = (root / ".github/scripts/deepseek_review.py").read_text()
process_skill = (root / ".claude/skills/process-pr/SKILL.md").read_text()
dev_skill = (root / ".claude/skills/dev-issue/SKILL.md").read_text()
agents_md = (root / "AGENTS.md").read_text()


def exactly_one(pattern, text, label):
    matches = re.findall(pattern, text, flags=re.MULTILINE | re.DOTALL)
    if len(matches) != 1:
        raise AssertionError(f"expected one {label}, found {len(matches)}")
    return matches[0]


def workflow_env(name):
    return int(
        exactly_one(
            rf"^\s+{re.escape(name)}:\s+\"(\d+)\"\s*$",
            workflow,
            f"{name} workflow value",
        )
    )


def workflow_string_env(name):
    return exactly_one(
        rf'^\s+{re.escape(name)}:\s+"([^"]+)"\s*$',
        workflow,
        f"{name} workflow value",
    )


def source_env_default(name):
    return exactly_one(
        rf'^{re.escape(name)}\s*=\s*os\.environ\.get\(\s*"{re.escape(name)}",\s*"([^"]+)"\s*\)',
        reviewer_source,
        f"{name} reviewer fallback",
    )


assignments = {}
for node in ast.parse(reviewer_source).body:
    if isinstance(node, ast.Assign) and len(node.targets) == 1:
        target = node.targets[0]
        if isinstance(target, ast.Name):
            try:
                assignments[target.id] = ast.literal_eval(node.value)
            except (ValueError, TypeError):
                pass

model = workflow_string_env("DEEPSEEK_MODEL")
reasoning_effort = workflow_string_env("DEEPSEEK_REASONING_EFFORT")
output_ceiling = workflow_env("DEEPSEEK_MAX_OUTPUT_TOKENS")
request_timeout = workflow_env("DEEPSEEK_REQUEST_TIMEOUT_SECONDS")
max_retries = workflow_env("DEEPSEEK_MAX_RETRIES")
job_minutes = int(
    exactly_one(
        r"^  review:\s*$.*?^    timeout-minutes:\s*(\d+)\s*$",
        workflow,
        "DeepSeek review job timeout",
    )
)
manual_wait = int(
    exactly_one(
        r"^DEEPSEEK_WAIT_SECONDS=(\d+)\s*$",
        process_skill,
        "process-pr DeepSeek wait",
    )
)

default_ceiling = assignments["DEEPSEEK_DEFAULT_MAX_OUTPUT_TOKENS"]
provider_ceiling = assignments["DEEPSEEK_PROVIDER_MAX_OUTPUT_TOKENS"]
default_model = source_env_default("DEEPSEEK_MODEL")
default_reasoning_effort = source_env_default("DEEPSEEK_REASONING_EFFORT")
request_budget = request_timeout * (max_retries + 1)
job_budget = job_minutes * 60
headroom = job_budget - request_budget

assert model == default_model == "deepseek-v4-flash", (
    "workflow and reviewer fallback must both select deepseek-v4-flash: "
    f"workflow={model} fallback={default_model}"
)
assert reasoning_effort == default_reasoning_effort == "high", (
    "workflow and reviewer fallback must both use high reasoning: "
    f"workflow={reasoning_effort} fallback={default_reasoning_effort}"
)
assert output_ceiling == default_ceiling, (
    "workflow output ceiling and reviewer fallback default diverged: "
    f"{output_ceiling} != {default_ceiling}"
)
assert 65_536 < output_ceiling <= provider_ceiling, (
    f"output ceiling {output_ceiling} must be above the exhausted 65,536-token value "
    f"and no greater than provider limit {provider_ceiling}"
)
assert request_timeout > 0 and max_retries >= 0
assert headroom == 600, (
    "review job must preserve its documented 10-minute setup/posting headroom: "
    f"budget={request_budget}s cap={job_budget}s headroom={headroom}s"
)
# The diff cap lives in four places: the constant the script actually applies, the
# paragraph in AGENTS.md people size a pull request against, the timing note in process-pr,
# and — since #167 — the step in dev-issue that measures a diff before opening a pull
# request around it. Nothing pinned it while it was 120_000, and it went on describing a
# context limit the model did not have until a truncated review on PR #158 made it visible;
# 600_000 then described the *next* model's context window, which is not what bounds a
# review either, and PR #164 paid for that with a 31-minute run and no verdict. A number
# readers make decisions from is an output, and outputs are pinned here.
#
# dev-issue is the one that matters most now: it is the only copy read *before* a pull
# request exists, which is the only moment splitting one is cheap.
diff_cap = assignments["DEEPSEEK_MAX_DIFF_CHARS"]
formatted_cap = f"{diff_cap:,}"
for text, label in (
    (agents_md, "AGENTS.md"),
    (process_skill, "process-pr SKILL.md"),
    (dev_skill, "dev-issue SKILL.md"),
):
    assert formatted_cap in text, (
        f"{label} does not document the diff cap the script applies ({formatted_cap} chars); "
        "change the constant and the prose together"
    )

# The job cap is a number two documents restate in prose, and legacy PR 160 moved it in the
# workflow while leaving process-pr telling a reader the cap was 70 minutes. Nothing
# caught it because the pins above read `timeout-minutes` out of the workflow and
# `DEEPSEEK_WAIT_SECONDS` out of the skill, and neither of those is the sentence an
# agent quotes when it reports a timeout. That sentence is a diagnostic somebody makes
# a decision from, so it is pinned like one.
#
# Matched on "N-minute cap" rather than on every minute figure, because process-pr
# carries an unrelated one: the 15-minute wait in its guardrails is about a PR that got
# merged mid-poll and has nothing to do with this budget.
for text, label in ((agents_md, "AGENTS.md"), (process_skill, "process-pr SKILL.md")):
    for spelling in re.findall(r"(\d+)-min(?:ute)?s?\s+cap", text):
        assert int(spelling) == job_minutes, (
            f"{label} describes a {spelling}-minute job cap; the workflow sets "
            f"timeout-minutes: {job_minutes}"
        )
    for restated in re.findall(r"timeout-minutes:\s*(\d+)", text):
        assert int(restated) == job_minutes, (
            f"{label} restates timeout-minutes: {restated}; the workflow sets {job_minutes}"
        )

assert request_budget <= manual_wait < job_budget, (
    "process-pr wait must cover the request budget without waiting through the job cap: "
    f"request={request_budget}s wait={manual_wait}s job={job_budget}s"
)

print(
    "deepseek budget — "
    f"model={model} effort={reasoning_effort} output={output_ceiling} diff_cap={formatted_cap} "
    f"request={request_budget}s "
    f"job={job_budget}s headroom={headroom}s wait={manual_wait}s"
)
PY
