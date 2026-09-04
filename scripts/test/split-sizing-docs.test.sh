#!/usr/bin/env bash
# Pin the sizing guidance to the cap it is about, and to itself across the two skills.
#
# Iteration 50 turned three issues into ten pull requests. `/dev-issue` Step 5 already said
# to decide the split before writing code, and all three agents implemented whole and split
# after measuring — so the instruction was not missing, it was unusable: it asked for an
# estimate and gave no method, while Step 7 gives an exact command. An instruction to guess
# loses to an instruction to measure.
#
# The fix put a calibration table in `/dev-issue` Step 5 and the same table in
# `/scrum-master`, which is the earlier and cheaper place to draw a seam — no code is sunk
# when an issue is being written. That is now three copies of one number and two copies of
# one table, which is the shape this repository pins rather than trusts: the same reasoning
# as `deepseek-budget`, `client-ci-budget`, `gate-tables` and the FULL_REVIEW_MARKER pair.
#
# What this does NOT check is whether anybody followed the guidance. Nothing can. What it
# checks is that the guidance still describes the cap that exists.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

DEV_ISSUE=".claude/skills/dev-issue/SKILL.md"
SCRUM=".claude/skills/scrum-master/SKILL.md"
REVIEWER=".github/scripts/deepseek_review.py"

for f in "$DEV_ISSUE" "$SCRUM" "$REVIEWER"; do
  [ -f "$f" ] || { echo "FAIL: $f is missing"; exit 1; }
done

python3 - "$DEV_ISSUE" "$SCRUM" "$REVIEWER" <<'PY'
import re
import sys

dev_issue = open(sys.argv[1]).read()
scrum = open(sys.argv[2]).read()
reviewer = open(sys.argv[3]).read()

failures = []

# 1. The cap the reviewer enforces, read from its own source rather than restated here.
# Anchored at the assignment, and tolerant of Python's digit separators: the value is a
# module-level literal (`DEEPSEEK_MAX_DIFF_CHARS = 45_000`), and a loose search would
# instead match the first number in one of the several comments that mention the name.
match = re.search(r"^DEEPSEEK_MAX_DIFF_CHARS\s*=\s*([\d_]+)\s*$", reviewer, re.M)
if match is None:
    failures.append("could not read DEEPSEEK_MAX_DIFF_CHARS out of the reviewer; this pin needs rewriting")
    cap = None
else:
    cap = int(match.group(1).replace("_", ""))

# 2. /scrum-master states the cap as a literal, because a ceremony has no code to read it
#    from. That literal must be the cap.
if cap is not None:
    stated = re.search(r"`DEEPSEEK_MAX_DIFF_CHARS` is \*\*([\d,]+)\*\* characters", scrum)
    if stated is None:
        failures.append("/scrum-master no longer states the cap; the Sizing section is what makes an issue sizable before code exists")
    elif int(stated.group(1).replace(",", "")) != cap:
        failures.append(
            f"/scrum-master says the cap is {stated.group(1)} but the reviewer enforces {cap}"
        )

# 3. Both skills carry the same calibration table. Compare the rows, not the prose around
#    them: a table that disagrees with itself sends the two ceremonies to different answers
#    about the same issue.
def rows(text, which):
    # Non-capturing, deliberately. `re.findall` with a capture group returns the *group*
    # and not the line, so a capturing version compared ["One workspace", ...] against
    # itself and passed whatever the numbers said — a drift check that could not detect
    # drift. Caught by mutating a row and watching this test stay green.
    found = re.findall(r"^\| (?:One workspace|Two or more workspaces)[^\n]*\|$", text, re.M)
    if not found:
        failures.append(f"{which} has no calibration table; the estimate is back to intuition")
    return found

dev_rows = rows(dev_issue, "/dev-issue Step 5")
scrum_rows = rows(scrum, "/scrum-master Sizing")
if dev_rows and scrum_rows and dev_rows != scrum_rows:
    failures.append(
        "the calibration tables have drifted apart:\n"
        + "\n".join(f"  /dev-issue : {r}" for r in dev_rows)
        + "\n"
        + "\n".join(f"  /scrum-master: {r}" for r in scrum_rows)
    )

# 4. Neither skill may instruct a split of at most two. The count is open — #851 needed
#    five, and its agent recorded that as a deviation because the skill had offered two.
#    The historical quotation in /dev-issue is exempt by being a quotation: it is the
#    sentence that names the defect, and deleting it would delete the reason.
for text, which in ((dev_issue, "/dev-issue"), (scrum, "/scrum-master")):
    for line in text.splitlines():
        if "one pull request or two" not in line:
            continue
        if "used to ask" in line:
            continue
        failures.append(f'{which} still instructs "one pull request or two": {line.strip()}')

# 5. /dev-issue must still ask for the decision before the code, in the count form.
if not re.search(r"\*\*Decide first how many pull requests this issue is — before any code exists, not after\.\*\*", dev_issue):
    failures.append("/dev-issue Step 5 no longer opens by asking how many pull requests the issue is")

if failures:
    print("FAIL: sizing guidance and the cap have drifted.")
    for f in failures:
        print(f"  - {f}")
    sys.exit(1)

print(f"PASS: both skills size against the reviewer's {cap}-character cap, with matching calibration tables")
PY
