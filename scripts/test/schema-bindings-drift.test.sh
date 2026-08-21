#!/usr/bin/env bash
# Pin check-schemas.sh's regeneration recipe against the one schemas/AGENTS.md documents.
#
# The drift phase exists because nothing compared `gen/` to `schemas/`, so a contract could
# move and its bindings stay put (#139). Closing that put the generation recipe in a second
# place: the prose in schemas/AGENTS.md that a human follows, and the script that now follows
# it for them. Two copies of one procedure is the shape this repository keeps having to fix,
# so it is pinned rather than trusted.
#
# **This test cannot run the phase**, and that is worth stating rather than hiding: the
# `automation` job has neither flatc nor either consumer's toolchain, so what runs the recipe
# for real is the `schemas` job. What this pins is that the two copies say the same thing —
# a recipe quietly simplified in one place is caught here, and a recipe that is wrong in both
# is caught by the bindings failing to match in CI.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
script = (root / "scripts/check-schemas.sh").read_text()
doc = (root / "schemas/AGENTS.md").read_text()


def canonical(text):
    """Reduce a shell line to the tokens that decide what flatc writes.

    The doc spells paths literally and the script spells them through variables, so a
    textual diff would report a difference that is not one. Everything below normalises
    the spelling and nothing else — a changed flag, a dropped pass or a different output
    directory all survive."""
    text = text.replace('"${REPO_ROOT}/', '"').replace("${REPO_ROOT}/", "")
    text = text.replace('"$SCHEMAS_DIR"', "schemas").replace("$SCHEMAS_DIR", "schemas")
    text = text.replace('"${fbs_files[@]}"', "schemas/*.fbs")
    text = text.replace('"${SCHEMAS_DIR}/envelope.fbs"', "schemas/envelope.fbs")
    text = text.replace('"', "")
    return re.sub(r"\s+", " ", text).strip()


# Anchored at the start of the command, so a line that merely *mentions* one — the
# `require cargo "client/" "...cargo fmt..."` guard, a comment — is not a recipe step.
INVOCATION = re.compile(r"^\(?\s*(?:cd\s+\S+\s*&&\s*)?(flatc|gofmt|cargo\s+fmt)\b")


def recipe_lines(text):
    """Every flatc / gofmt / cargo-fmt invocation, in order. Blocks holding none
    contribute none — schemas/AGENTS.md has fenced blocks that are not the recipe.

    Continuations are joined before splitting: the script wraps its flatc calls and the
    doc does not, and a difference in line width is not a difference in recipe."""
    joined = re.sub(r"\\\n\s*", " ", text)
    found = []
    for raw in joined.splitlines():
        line = raw.strip()
        if line.startswith("#") or line.startswith("//"):
            continue
        if INVOCATION.match(line):
            found.append(canonical(line))
    return found


# The documented recipe: the two fenced blocks under the consumer headings.
doc_blocks = re.findall(r"```bash\n([\s\S]*?)```", doc)
documented = [c for block in doc_blocks for c in recipe_lines(block)]

# The script's phase 2 — from the phase banner to the end.
phase_two = script.split("# ── Phase 2")[-1]
assert phase_two != script, "check-schemas.sh has no phase 2 banner"
implemented = recipe_lines(phase_two)

assert documented, "schemas/AGENTS.md documents no generation recipe"
assert implemented, "check-schemas.sh phase 2 runs no generation commands"
assert implemented == documented, (
    "the recipe check-schemas.sh runs and the one schemas/AGENTS.md documents have drifted.\n"
    f"  documented:  {documented!r}\n"
    f"  implemented: {implemented!r}\n"
    "Change both, or neither — the script is the doc executed."
)

# The three properties the phase needs to be worth anything. Each was a deliberate choice
# and each is easy to undo by accident.
assert "git" in phase_two and "--quiet" in phase_two, (
    "phase 2 must compare against the committed tree with git, not just regenerate"
)
assert "status --porcelain" in phase_two, (
    "phase 2 must refuse to run on a dirty gen/ — it regenerates in place and would "
    "otherwise overwrite uncommitted work"
)
for marker, why in (
    ("go.mod", "server presence must be read from its marker file"),
    ("Cargo.toml", "client presence must be read from its marker file"),
):
    assert marker in phase_two, why

# A missing formatter must be an error, never a skipped phase: a check that reports success
# because it did not run is the failure this whole phase exists to end.
assert re.search(r"require\s+gofmt", phase_two) and re.search(r"require\s+cargo", phase_two), (
    "phase 2 must fail closed when a scaffolded workspace's formatter is absent"
)

# And the job that actually runs it must install what it needs.
ci = (root / ".github/workflows/ci.yml").read_text()
schemas_job = re.search(
    r"^  schemas:\s*$\n([\s\S]*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)", ci, re.M
).group(1)
for needed in ("actions/setup-go@", "dtolnay/rust-toolchain@", "Install pinned flatc"):
    assert needed in schemas_job, (
        f"the schemas job must install {needed} — phase 2 runs the formatters, and a job "
        "without them fails on every run rather than checking anything"
    )

print(f"schema binding drift — recipe pinned, {len(documented)} commands, doc == script")
PY
