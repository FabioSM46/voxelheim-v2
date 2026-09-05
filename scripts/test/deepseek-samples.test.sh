#!/usr/bin/env bash
#
# The seventeen #925 samples exist twice, and this is what keeps the copies one thing.
#
# `_HIGH_SAMPLES` in .github/scripts/test_deepseek_review.py is the machine-readable
# copy: the cap is derived from it, so it is the one that decides. The table in AGENTS.md
# is the human copy and carries what the tuple deliberately does not — files, completion
# tokens, wall-clock, outcome — which is why it is not simply deleted in favour of the
# tuple.
#
# Two copies of one measurement is the shape this repository pins rather than trusts
# (gate-tables.test.sh, split-sizing-docs.test.sh, client-cache-parity.test.sh). The
# standing rule says an eighteenth sample brings the cap down; whoever takes it will hit
# `assertEqual(17, len(_HIGH_SAMPLES))` and update the tuple, and nothing would have
# noticed the table staying at seventeen. The drift had already started: the table said
# 6.0 where the tuple gives 6.027, and the cap is derived from the second.
#
# Also pinned: the heading itself. Both the constant's comment and the tuple's comment
# send a reader to "Taken, on #925" for the provenance of numbers they do not carry, and
# a pointer nothing checks is a pointer that rots.
#
# Run: bash scripts/test/deepseek-samples.test.sh

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

python3 - "$ROOT/AGENTS.md" "$ROOT/.github/scripts/test_deepseek_review.py" \
         "$ROOT/.github/scripts/deepseek_review.py" <<'PY'
import ast
import re
import sys

agents = open(sys.argv[1]).read()
suite = open(sys.argv[2]).read()
reviewer = open(sys.argv[3]).read()

HEADING = "Taken, on #925"
failures = []

# 1. The tuple, read from the suite's AST rather than by regex: it is a literal, and a
#    literal is what ast.literal_eval exists for.
samples = None
for node in ast.walk(ast.parse(suite)):
    if isinstance(node, ast.Assign) and any(
        isinstance(t, ast.Name) and t.id == "_HIGH_SAMPLES" for t in node.targets
    ):
        samples = list(ast.literal_eval(node.value))
if samples is None:
    failures.append("could not read _HIGH_SAMPLES out of the suite; this pin needs rewriting")

# 2. The heading both code comments point at.
if HEADING not in agents:
    failures.append(
        f'AGENTS.md has no "{HEADING}" section, and two comments in the reviewer send a '
        "reader to it for the provenance of the samples"
    )
for path, text in ((sys.argv[3], reviewer), (sys.argv[2], suite)):
    if HEADING not in text:
        failures.append(f"{path} no longer names the section holding the samples")

# 3. The table rows, from that section to the end of the table.
rows = []
if HEADING in agents:
    section = agents[agents.index(HEADING) :]
    for line in section.splitlines():
        m = re.match(
            r"^\|\s*\*{0,2}#(\d+)[^|]*\|"      # PR, optionally "(again)"
            r"\s*\*{0,2}([\d,]+)\*{0,2}\s*\|"  # diff chars
            r"[^|]*\|"                          # files
            r"\s*\*{0,2}([\d,]+)\*{0,2}\s*\|"  # reasoning chars
            r"[^|]*\|"                          # completion tokens
            r"\s*\*{0,2}([\d.]+)\*{0,2}\s*\|", # ratio
            line,
        )
        if m:
            pr, diff, reasoning, ratio = m.groups()
            rows.append(
                (int(pr), int(diff.replace(",", "")), int(reasoning.replace(",", "")), float(ratio))
            )

if samples is not None:
    if not rows:
        failures.append(f'no sample rows parsed under "{HEADING}"; the table or this pin has changed shape')
    elif sorted((d, r) for _, d, r, _ in rows) != sorted(samples):
        failures.append(
            f"AGENTS.md lists {len(rows)} sample rows and the suite derives the cap from "
            f"{len(samples)}; the (diff, reasoning) pairs must be the same measurement"
        )

# 4. Every stated ratio is the one its own row implies. The table rounds to one or two
#    places, so the tolerance is a rounding step and not a licence.
for pr, diff, reasoning, stated in rows:
    if abs(stated - reasoning / diff) >= 0.05:
        failures.append(
            f"#{pr}: the table states a ratio of {stated} where {reasoning}/{diff} is "
            f"{reasoning / diff:.2f}"
        )

# 5. The worst ratio the prose names is the worst the samples hold. This is the number the
#    whole derivation hangs from, so a stale restatement of it is the expensive one.
if rows:
    worst = max(r / d for _, d, r, _ in rows)
    stated_worst = re.search(r"The worst ratio observed is ([\d.]+)", agents)
    if stated_worst is None:
        failures.append("AGENTS.md no longer names the worst observed ratio the cap is derived from")
    elif abs(float(stated_worst.group(1)) - worst) >= 0.005:
        failures.append(
            f"AGENTS.md says the worst observed ratio is {stated_worst.group(1)} while the "
            f"samples give {worst:.3f}"
        )

if failures:
    print("FAIL: the #925 samples have drifted between AGENTS.md and the suite.")
    for f in failures:
        print(f"  - {f}")
    raise SystemExit(1)

print(f"PASS: AGENTS.md and DiffCapTests hold the same {len(rows)} #925 samples")
PY
