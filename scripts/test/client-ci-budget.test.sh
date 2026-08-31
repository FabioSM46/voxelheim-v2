#!/usr/bin/env bash
# Pin the client dependency-step and job budgets as one fail-closed contract.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
workflow = (root / ".github/workflows/ci.yml").read_text()


def exactly_one(pattern, text, label):
    matches = re.findall(pattern, text, flags=re.MULTILINE)
    if len(matches) != 1:
        raise AssertionError(f"expected one {label}, found {len(matches)}")
    return matches[0]


def job_block(text, name):
    return exactly_one(
        rf"^  {re.escape(name)}:\s*$\n"
        r"([\s\S]*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)",
        text,
        f"{name} job",
    )


# Keep job extraction independent of the order of unrelated workflow jobs.
boundary_fixture = """jobs:
  client:
    timeout-minutes: 25
  inserted_job:
    timeout-minutes: 99
"""
assert "timeout-minutes: 99" not in job_block(boundary_fixture, "client"), (
    "client job extraction must stop at any following job key"
)

client_job = job_block(workflow, "client")
dependency_step = exactly_one(
    r"^      - name: Install Bevy system dependencies\s*$\n"
    r"([\s\S]*?)(?=^      - (?:name:|uses:))",
    client_job,
    "Bevy dependency step",
)

job_minutes = int(
    exactly_one(
        r"^    timeout-minutes:\s*(\d+)\s*$",
        client_job,
        "client job timeout",
    )
)
step_minutes = int(
    exactly_one(
        r"^        timeout-minutes:\s*(\d+)\s*$",
        dependency_step,
        "dependency step timeout",
    )
)

# 40 since #650. The client dev profile optimizes dependencies, and this PR's own
# cold CI run took 23m16s against the previous budget of 25 — a 104-second margin on
# a shared runner. The number is a measurement's to move and this test's to pin, so
# that it moves deliberately rather than drifting.
assert job_minutes == 40, f"client job timeout must be 40 minutes, got {job_minutes}"
assert step_minutes == 5, (
    f"dependency step timeout must be 5 minutes, got {step_minutes}"
)
assert step_minutes < job_minutes, (
    "dependency setup must leave a separate budget for Rust gates: "
    f"step={step_minutes}m job={job_minutes}m"
)

run_body = exactly_one(
    r"^        run: \|\s*$\n((?:^          .*\n?)+)",
    dependency_step,
    "dependency run block",
)

commands = []
current = ""
for raw_line in run_body.splitlines():
    line = raw_line.strip()
    if line.endswith("\\"):
        current += line[:-1].rstrip() + " "
        continue
    current += line
    commands.append(re.sub(r"\s+", " ", current).strip())
    current = ""

assert not current, "dependency run block ends with an unfinished continuation"
expected_options = (
    "-o Acquire::Retries=2 "
    "-o Acquire::http::Timeout=15 "
    "-o Acquire::https::Timeout=15"
)
expected_commands = [
    f"sudo apt-get {expected_options} update",
    (
        f"sudo apt-get {expected_options} install -y --no-install-recommends "
        "libasound2-dev libudev-dev pkg-config"
    ),
]
assert commands == expected_commands, (
    "APT update/install must keep the package list and use the bounded retry/timeout "
    f"contract; got {commands!r}"
)

for forbidden in ("continue-on-error:", "||", "set +e"):
    assert forbidden not in dependency_step, (
        f"dependency failures must remain fatal; found {forbidden!r}"
    )

rust_gates = [
    "cargo fmt --all --check",
    "cargo clippy --workspace --all-targets --locked -- -D warnings",
    "cargo build --workspace --locked",
    "cargo test --workspace --locked",
]
for command in rust_gates:
    count = client_job.count(f"run: {command}")
    assert count == 1, f"expected one unchanged Rust gate {command!r}, found {count}"

# The warm-up workflow runs the same commands and always runs them cold, so a budget
# that differs there is a cache that never gets built. Pinned rather than trusted:
# ci.yml is the source and client-cache.yml is what has to follow.
cache_workflow = (root / ".github/workflows/client-cache.yml").read_text()
cache_client = job_block(cache_workflow, "client")
cache_minutes = int(
    exactly_one(
        r"^    timeout-minutes:\s*(\d+)\s*$",
        cache_client,
        "client-cache.yml client job timeout",
    )
)
assert cache_minutes == job_minutes, (
    "the cache warm-up must carry the gate's budget: "
    f"gate={job_minutes}m warm-up={cache_minutes}m"
)

automation_job = job_block(workflow, "automation")
invocation = "bash scripts/test/client-ci-budget.test.sh"
assert automation_job.count(invocation) == 1, (
    "automation job must execute the focused client CI budget test exactly once"
)

print(
    "client CI budget — "
    f"dependencies={step_minutes}m job={job_minutes}m warm-up={cache_minutes}m "
    "apt-retries=2 apt-network-timeout=15s"
)
PY
