#!/usr/bin/env bash
# Pin client-cache.yml against ci.yml's client job as one fail-closed contract.
#
# The warm-up workflow only pays for itself if the gate can read what it wrote and the
# gate finds its work already done. Both halves used to live as a comment claiming the
# two files were configured identically — a guarantee nothing checked. This derives the
# expectation from ci.yml instead of restating it, so the gate is the source of truth
# and the cache is what has to follow.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
ci = (root / ".github/workflows/ci.yml").read_text()
cache = (root / ".github/workflows/client-cache.yml").read_text()


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


# Same guard client-ci-budget.test.sh carries: extraction must stop at the next job
# key, so an unrelated job inserted after `client` can never be read as part of it.
boundary_fixture = """jobs:
  client:
    timeout-minutes: 25
  inserted_job:
    timeout-minutes: 99
"""
assert "timeout-minutes: 99" not in job_block(boundary_fixture, "client"), (
    "client job extraction must stop at any following job key"
)

ci_client = job_block(ci, "client")
cache_client = job_block(cache, "client")


# ── 1. The cache key ────────────────────────────────────────────────────────────
# rust-cache composes its key from the workspace, the shared key and the toolchain it
# finds installed. Any of the three differing makes the warm entry unreachable from the
# gate, which fails silently: the gate simply rebuilds, exactly as it did before this
# workflow existed. Nothing turns red, so nothing but this test would notice.
def cache_inputs(job, label):
    step = exactly_one(
        r"^      - uses: (Swatinem/rust-cache@[^\s]+)\s*(?:#.*)?$\n([\s\S]*?)"
        r"(?=^      - (?:name:|uses:)|\Z)",
        job,
        f"{label} rust-cache step",
    )
    action, body = step
    return {
        "action": action,
        "workspaces": exactly_one(
            r"^          workspaces:\s*(\S+)\s*$", body, f"{label} workspaces"
        ),
        "shared-key": exactly_one(
            r"^          shared-key:\s*(\S+)\s*$", body, f"{label} shared-key"
        ),
        "toolchain": exactly_one(
            r"^      - uses: (dtolnay/rust-toolchain@[^\s]+)\s*(?:#.*)?$",
            job,
            f"{label} toolchain",
        ),
    }


gate_key = cache_inputs(ci_client, "ci.yml client")
warm_key = cache_inputs(cache_client, "client-cache.yml client")

assert gate_key == warm_key, (
    "client-cache.yml must write the cache key ci.yml reads; "
    f"gate={gate_key!r} warm={warm_key!r}"
)
assert gate_key["shared-key"] == "client-gates", (
    "the shared key must stay named rather than derived from the job identity, "
    f"got {gate_key['shared-key']!r}"
)
assert gate_key["workspaces"] == "client", (
    f"the cached workspace must be client/, got {gate_key['workspaces']!r}"
)


# ── 2. The cargo commands ───────────────────────────────────────────────────────
# Derived from ci.yml, never restated: whatever the gate compiles is what the warm-up
# owes. `cargo fmt` is the one exemption and it is not a judgement call — it compiles
# nothing, so there is no artifact for a cache to carry.
def cargo_commands(job):
    return [
        re.sub(r"\s+", " ", match).strip()
        for match in re.findall(r"^\s*(?:run: )?(cargo\b[^\n]*)$", job, flags=re.MULTILINE)
    ]


NON_COMPILING = ("cargo fmt",)

gate_cargo = cargo_commands(ci_client)
warm_cargo = cargo_commands(cache_client)

assert gate_cargo, "found no cargo gates in ci.yml's client job"
owed = [c for c in gate_cargo if not c.startswith(NON_COMPILING)]

for gate_cmd in owed:
    verb = gate_cmd.split()[1]
    # `cargo test` is warmed with --no-run: the binaries are the expensive half and
    # ci.yml is what decides whether they pass. Running the suite twice would buy a
    # slower merge and no second answer.
    expected = gate_cmd + (" --no-run" if verb == "test" else "")
    matching = [c for c in warm_cargo if c.split()[1] == verb]
    assert len(matching) == 1, (
        f"expected exactly one `cargo {verb}` warm-up for ci.yml's {gate_cmd!r}, "
        f"found {matching!r}"
    )
    assert matching[0] == expected, (
        f"warm-up must compile what the gate compiles: expected {expected!r}, "
        f"got {matching[0]!r}"
    )

assert len(warm_cargo) == len(owed), (
    "the warm-up must run the gate's compiling commands and nothing else; "
    f"gate owes {owed!r}, warm-up runs {warm_cargo!r}"
)


# ── 3. The triggers ─────────────────────────────────────────────────────────────
push_paths = exactly_one(
    r"^    paths:\s*$\n((?:^      (?:-|#).*\n)+)", cache, "push paths list"
)
paths = re.findall(r"^      - (\S+)\s*$", push_paths, flags=re.MULTILINE)
for required in ("client/**", ".github/workflows/client-cache.yml", ".github/workflows/ci.yml"):
    assert required in paths, (
        f"{required!r} must trigger a rebuild of the cache; got {paths!r}"
    )

# A cache GitHub has not served in 7 days is deleted, and a schedule is the only
# trigger that does not wait on someone happening to merge client code. The bound
# below is the reason the cron reads 1,3,5 rather than a single weekly fire.
crons = re.findall(r"^    - cron:\s*['\"]([^'\"]+)['\"]\s*$", cache, flags=re.MULTILINE)
assert crons, "client-cache.yml must carry a schedule; the 7-day eviction needs one"

days = set()
for cron in crons:
    fields = cron.split()
    assert len(fields) == 5, f"malformed cron {cron!r}"
    _minute, _hour, dom, month, dow = fields
    assert dom == "*" and month == "*", (
        f"the gap bound below only reasons about day-of-week schedules, got {cron!r}"
    )
    for part in dow.split(","):
        assert part.isdigit(), f"unsupported day-of-week field {dow!r} in {cron!r}"
        days.add(int(part) % 7)

ordered = sorted(days)
gaps = [
    ((ordered[(i + 1) % len(ordered)] - ordered[i]) % 7) or 7 for i in range(len(ordered))
]

EVICTION_DAYS = 7
assert max(gaps) < EVICTION_DAYS, (
    f"a {max(gaps)}-day gap between warm-ups does not beat a {EVICTION_DAYS}-day "
    f"eviction window; crons={crons!r}"
)
# GitHub drops scheduled runs under load, so the schedule has to survive losing one.
worst_after_a_drop = max(gaps[i] + gaps[(i + 1) % len(gaps)] for i in range(len(gaps)))
assert worst_after_a_drop < EVICTION_DAYS, (
    f"one dropped run would open a {worst_after_a_drop}-day gap against a "
    f"{EVICTION_DAYS}-day eviction window; crons={crons!r}"
)


# ── 4. This test runs ───────────────────────────────────────────────────────────
automation_job = job_block(ci, "automation")
invocation = "bash scripts/test/client-cache-parity.test.sh"
assert automation_job.count(invocation) == 1, (
    "automation job must execute the client cache parity test exactly once"
)

print(
    "client cache parity — "
    f"key={gate_key['shared-key']}/{gate_key['workspaces']} "
    f"warmed={len(owed)} gates "
    f"max-gap={max(gaps)}d worst-after-a-drop={worst_after_a_drop}d"
)
PY
