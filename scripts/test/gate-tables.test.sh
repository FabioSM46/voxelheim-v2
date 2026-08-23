#!/usr/bin/env bash
# Pin every documented gate table against the gates ci.yml actually runs.
#
# The tables in AGENTS.md and in the two canonical skills each claimed to mirror
# ci.yml — "step for step", "exactly" — and nothing checked it. golangci-lint had been
# a blocking server gate since the Go scaffold (legacy PR 13) and appeared in none of them, so an
# agent following the skill table ran four gates where CI runs five, and found out on a
# red PR. `server/AGENTS.md` was the one copy that was right, which is the tell: the
# tables were not wrong because someone misread ci.yml, they were wrong because nothing
# made them follow it.
#
# This is the same pin the repository already uses for the pairs that must agree
# (deepseek-budget, client-ci-budget, the FULL_REVIEW_MARKER pair). GATES below is the
# canonical list, and it is checked in BOTH directions: every entry must be something
# ci.yml runs, and ci.yml's step list must hold no surprises. The second direction is
# the one that was missing — a gate added to CI could go undocumented indefinitely,
# which is exactly what happened.
#
# The generated adapters under .agents/ and .opencode/ are deliberately NOT re-checked
# here: agent-skills-sync.test.sh is the pin for that pair, and duplicating it would put
# the enumeration where the enforcement should be.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
ci = (root / ".github/workflows/ci.yml").read_text()
agents_md = (root / "AGENTS.md").read_text()
dev_issue = (root / ".claude/skills/dev-issue/SKILL.md").read_text()
process_pr = (root / ".claude/skills/process-pr/SKILL.md").read_text()

# Each gate is (token, ci_marker, doc_command).
#
#   ci_marker    the string that proves ci.yml runs it. A `uses:` step is a gate too:
#                golangci-lint is an action rather than a run line, which is part of why
#                every hand-written table missed it while listing the four `run:` gates
#                around it.
#   doc_command  what an executed table must carry VERBATIM. It defaults to ci_marker,
#                because for most gates CI's own string is already the command. Two are
#                overridden, and only because CI does not express them as one: gofmt runs
#                inside a shell conditional, and golangci-lint is an action with no command
#                line at all. Neither can be pasted, so the runnable spelling is named here.
#
# The token is not enough on its own, and that gap was real: a skill table could have said
# `go vet -newflag ./...`, kept the token `go vet`, and passed — an agent would then run a
# different gate from CI, which is the exact false local pass this file exists to stop.
# Found by the DeepSeek review on the pull request that added this test (legacy PR 125).
GATES = {
    "server": [
        ("gofmt", "gofmt -l .", 'test -z "$(gofmt -l .)"'),
        ("go vet", "go vet ./...", None),
        ("golangci-lint", "golangci/golangci-lint-action", "golangci-lint run"),
        ("go build", "go build ./...", None),
        ("GOARCH=386", "GOARCH=386 go build ./...", None),
        ("GOARCH=arm", "GOARCH=arm go build ./...", None),
        ("go test", "go test ./...", None),
    ],
    "client": [
        ("cargo fmt", "cargo fmt --all --check", None),
        ("cargo clippy", "cargo clippy --workspace --all-targets --locked -- -D warnings", None),
        ("cargo build", "cargo build --workspace --locked", None),
        ("cargo test", "cargo test --workspace --locked", None),
    ],
    "schemas": [
        ("check-schemas.sh", "bash scripts/check-schemas.sh", None),
    ],
}


def doc_command(gate):
    _token, ci_marker, override = gate
    return override or ci_marker

# The reverse direction. Setup steps are listed alongside the gates deliberately: the
# point is that ANY change to a workspace job's shape has to come past this test, so a
# new step cannot quietly be a gate nobody documented.
STEPS = {
    "server": [
        "uses:actions/checkout",
        "Detect workspace presence",
        "uses:actions/setup-go",
        "Format check (gofmt)",
        "Vet",
        "Lint (golangci-lint)",
        "Build",
        "Cross-compile (32-bit)",
        "Test",
    ],
    "client": [
        "uses:actions/checkout",
        "Detect workspace presence",
        "Prefer the canonical Ubuntu archive over the Azure mirror",
        "Install Bevy system dependencies",
        "uses:dtolnay/rust-toolchain",
        "uses:Swatinem/rust-cache",
        "Format check",
        "Clippy",
        "Build",
        "Test",
    ],
    "schemas": [
        "uses:actions/checkout",
        "Detect workspace presence",
        "Install pinned flatc",
        # Setup, not gates: check-schemas.sh's drift phase runs the documented
        # generation recipe, and the formatters are part of that recipe.
        "uses:actions/setup-go",
        "uses:dtolnay/rust-toolchain",
        "Validate contracts",
    ],
}


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


def step_names(job):
    steps = []
    for name, uses in re.findall(
        r"^      - (?:name: (.+)|uses: (\S+?))(?:\s+#.*)?\s*$", job, flags=re.MULTILINE
    ):
        # Action versions are enforced separately by actions-hardening.test.sh.
        # This test owns the job's shape, so updating a pinned SHA must
        # not require restating that SHA here too.
        steps.append(name or f"uses:{uses.split('@', 1)[0]}")
    return steps


def table_row(text, workspace, label):
    """The gate-table row for a workspace, in any of the three documented spellings
    (`server`, `server/`). Everything after the workspace cell is returned."""
    row = exactly_one(
        rf"^\|\s*`{re.escape(workspace)}/?`\s*\|(.+)$", text, f"{label} {workspace} row"
    )
    return row


def code_spans(text):
    return re.findall(r"`([^`]+)`", text)


def assert_documents(source_text, source_label, gates, exact):
    """Every gate must appear inside a code span, not merely somewhere in the prose.
    That distinction is what catches the original bug: the stale AGENTS.md row DID contain
    the string "golangci-lint" — in a parenthetical saying the gate had not arrived yet.

    `exact` separates the two kinds of table, and the split is the point rather than a
    convenience. The gate tables in the two skills are EXECUTED — an agent pastes that
    chain, so a flag there that CI does not run is a green local gate and a red PR, and
    they must carry the command verbatim. AGENTS.md's Definition of Done and "What CI
    enforces" DESCRIBE — the CI table deliberately abbreviates (`cargo fmt --check` for a
    row summarising five jobs), and nobody runs it. Requiring the full command there would
    buy nothing and cost the table its readability."""
    spans = code_spans(source_text)
    for gate in gates:
        needle = doc_command(gate) if exact else gate[0]
        assert any(needle in span for span in spans), (
            f"{source_label} does not document `{needle}` "
            f"{'as the command CI runs' if exact else 'as a command'}; "
            f"code spans present: {spans!r}"
        )


failures = []

# ── Direction 1: every documented gate is one ci.yml actually runs ───────────────
for workspace, gates in GATES.items():
    job = job_block(ci, workspace)
    for _token, marker, _override in gates:
        assert marker in job, (
            f"GATES lists {marker!r} for the {workspace} job, but ci.yml does not run it — "
            "if the gate was removed, remove it here and from every documented table"
        )

# ── Direction 2: ci.yml holds no gate this list has never heard of ──────────────
for workspace, expected in STEPS.items():
    found = step_names(job_block(ci, workspace))
    assert found == expected, (
        f"ci.yml's {workspace} job step list changed.\n"
        f"  expected: {expected!r}\n"
        f"  found:    {found!r}\n"
        "If the new step is a gate, add it to GATES and to EVERY documented table "
        "(AGENTS.md's Definition of Done and 'What CI enforces', and the gate tables in "
        ".claude/skills/dev-issue and .claude/skills/process-pr). If it is setup, add it here."
    )

# ── Direction 3: every gate is documented everywhere a table claims to mirror CI ─
dod = exactly_one(
    r"^## Definition of Done\s*$\n([\s\S]*?)(?=^### What CI enforces\s*$)",
    agents_md,
    "Definition of Done block",
)
ci_enforces = exactly_one(
    r"^### What CI enforces\s*$\n([\s\S]*?)(?=^#### )", agents_md, "What CI enforces block"
)

every_gate = [gate for gates in GATES.values() for gate in gates]
assert_documents(dod, "AGENTS.md Definition of Done", every_gate, exact=False)

for workspace, gates in GATES.items():
    for text, label, exact in (
        (ci_enforces, "AGENTS.md 'What CI enforces'", False),
        (dev_issue, ".claude/skills/dev-issue/SKILL.md", True),
        (process_pr, ".claude/skills/process-pr/SKILL.md", True),
    ):
        assert_documents(
            table_row(text, workspace, label), f"{label} ({workspace} row)", gates, exact=exact
        )

# ── Direction 4: this test runs ─────────────────────────────────────────────────
automation_job = job_block(ci, "automation")
invocation = "bash scripts/test/gate-tables.test.sh"
assert automation_job.count(invocation) == 1, (
    "automation job must execute the gate table test exactly once"
)

counts = " ".join(f"{w}={len(g)}" for w, g in GATES.items())
print(f"gate tables pinned to ci.yml — {counts} (4 documented tables)")
PY
