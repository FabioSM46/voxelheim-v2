#!/usr/bin/env bash
# A citation that came in with the import cannot be written as a bare `#N`.
#
# This repository renumbered from scratch at the sanitized snapshot, so every `#N` that
# was already in the tree at that commit names a change in the *previous* repository.
# Thirty-six of them resolve to a real and unrelated change here, which is the worse
# outcome rather than the better one: a dangling `#593` announces itself, a citation that
# resolves to the wrong thing does not. `AGENTS.md` cited `#13` for "the Go scaffold" while
# this repository's #13 is "anchor review findings to lines"; an agent implementing #102
# followed `#147` in good faith and propagated it into four new comments and a PR body.
#
# **The classifier is line provenance, never the number.** `#131` appears in this tree both
# as a legacy reference and as a correct reference to this repository's own #131 — the same
# number, in the same tree, legacy in one file and current in another. A number-driven sweep
# would break the working citations to fix the broken ones. `git blame` against the import
# boundary answers it per line, and that is what drove the sweep. This test asks the same
# question of the snapshot's own text instead, for the reason set out where it does so.
#
# The boundary is *derived*, not typed in: it is the repository's root commit. A list of
# offending lines typed into a test is a list that goes stale the first time somebody adds a
# legitimate citation, which is the failure mode this repository has hit with hand-kept
# counts more than once.
#
# Two things are deliberately not citations and are handled differently:
#
#   * A `#` followed by digits and then more alphanumerics is not a citation — `#78787D` is
#     a colour, `%#08x` is a format verb. That is settled by the token grammar rather than
#     by an allowance, because a grammar cannot go stale and cannot be widened by accident.
#   * A citation that names the repository it belongs to already resolves. `clinic-deck #464`
#     says which tree it means; so does `legacy PR 147`. The attribution is accepted on the
#     citing line or the one above it, because prose wraps.
#
# Everything else needs a named allowance with a reason, and an allowance that no longer
# matches anything is itself a failure — a stale exemption is how a real citation slips
# back in under cover of a fixture.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

python3 - "$REPO_ROOT" <<'PY'
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])


def git(*args):
    return subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True, check=True
    ).stdout


# ------------------------------------------------------------------ the boundary itself
# Read, not hard-coded. The import is the root commit: it holds the snapshot, and every line
# written in this repository came after it. Fail closed on anything else —
# a shallow clone answers nothing here, and a test that cannot see the boundary must say so
# rather than pass. The `automation` job checks out with fetch-depth: 0 for this.
roots = git("rev-list", "--max-parents=0", "HEAD").split()
assert len(roots) == 1, (
    f"expected exactly one root commit to serve as the import boundary, found {len(roots)}: "
    f"{roots}. Without an unambiguous boundary this property cannot be evaluated."
)
BOUNDARY = roots[0]

# ------------------------------------------------------------------- what counts as one
# Digits terminated by anything that is not alphanumeric. This is what separates `#131`
# from `#78787D` and `%#08x` without an exemption list standing between them.
CITATION = re.compile(r"#(\d{1,4})(?![0-9A-Za-z])")

# A citation that names its repository resolves, so it is not bare. `legacy PR N` is the
# convention the imported issue bodies already use; `clinic-deck #N` names the other tree
# the pipeline came from.
ATTRIBUTED = re.compile(r"clinic-deck|\blegacy (?:PR|PRs|issue)\b", re.IGNORECASE)

# ------------------------------------------------------------------- the named allowance
# (path, token) -> why this one is not a citation. Keyed on the token rather than the line
# number so it survives the file moving underneath it, and scoped to one file so a fixture
# number cannot exempt the same number somewhere it really is a citation — `#131` is a
# fixture PR number in pr-label-writes.test.sh and a real reference everywhere else.
ALLOWED = {
    (".claude/skills/dev-issue/SKILL.md", "#42"):
        "worked example of a worktree path for a hypothetical issue",
    (".agents/skills/dev-issue/SKILL.md", "#42"):
        "generated adapter of the .claude worked example",
    (".opencode/skills/dev-issue/SKILL.md", "#42"):
        "generated adapter of the .claude worked example",
    (".agents/skills/dev-issue/agents/openai.yaml", "#42"):
        "placeholder issue number in the Codex default prompt",
    (".agents/skills/process-pr/agents/openai.yaml", "#42"):
        "placeholder PR number in the Codex default prompt",
    ("scripts/sync-agent-skills.sh", "#42"):
        "the generator source of the two Codex default prompts above",
    ("docs/ISSUE_CONVENTIONS.md", "#42"):
        "worked example of two parallel worktrees",
    ("docs/ISSUE_CONVENTIONS.md", "#43"):
        "worked example of two parallel worktrees",
    (".github/ISSUE_TEMPLATE/feature_request.yml", "#12"):
        "placeholder text shown in the Dependencies field of the form",
    ("scripts/test/iteration-lifecycle.test.sh", "#4"):
        "fixture milestone number in an asserted string",
    ("scripts/test/iteration-lifecycle.test.sh", "#11"):
        "fixture milestone number in an asserted string",
    ("scripts/test/iteration-lifecycle.test.sh", "#12"):
        "fixture milestone number in an asserted string",
    ("scripts/test/pr-label-writes.test.sh", "#131"):
        "fixture PR number inside a string the test asserts exactly; rewriting it breaks the test",
    ("scripts/test/pr-label-writes.test.sh", "#279"):
        "fixture PR number inside a string the test asserts exactly",
    ("scripts/test/pr-labeler-step.test.sh", "#279"):
        "fixture PR number inside a string the test asserts exactly",
}

# ------------------------------------------------- what "present at the boundary" means
# **The snapshot's text, not `git blame`, and the difference is the whole regression story.**
# Blame is what drove the sweep — it is how each citation was classified, one line at a time.
# It cannot be what guards the result. A line the sweep rewrote no longer blames to the
# boundary, it blames to the sweep; revert that line and it blames to the revert. Either way
# the answer is "not the boundary", so a blame-based test goes blind on exactly the lines it
# was added to keep watching: it catches a citation the sweep *missed* and never one that
# comes *back*. That is the opposite of holding after the sweep rather than only during it.
#
# Reading the boundary snapshot's own text fixes it in both directions. A line counts as
# present at the boundary when its exact text appears in that commit's version of the same
# file — a superset of what blame answers, since an untouched boundary line's text is in the
# snapshot by definition, and indifferent to how many commits have passed since. Restoring an
# old citation restores its text, so it is caught, and caught in the working tree before it
# is ever committed rather than one commit too late.
#
# The residual over-approximation is a new line written byte-identical to a snapshot line
# carrying a bare `#N`. That is somebody copying an old comment forward, which is the thing
# this test exists to stop — so the one direction it errs in is the safe one.
tracked = [
    p for p in git("ls-files", "-z").split("\0")
    if p and "/gen/" not in f"/{p}" and "_generated." not in Path(p).name
]
at_boundary = set(git("ls-tree", "-r", "--name-only", "-z", BOUNDARY).split("\0"))
candidates = []
for path in tracked:
    if path not in at_boundary:
        continue  # written after the import; it has no snapshot text to match against
    try:
        text = (root / path).read_text(encoding="utf-8")
    except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
        continue
    if CITATION.search(text):
        candidates.append(path)

failures = []
used = set()
for path in candidates:
    try:
        snapshot = set(git("show", f"{BOUNDARY}:{path}").split("\n"))
    except subprocess.CalledProcessError:
        continue  # unreadable at the boundary (a symlink, or a mode-only entry)
    lines = (root / path).read_text(encoding="utf-8").split("\n")
    for lineno, line in enumerate(lines, start=1):
        if line not in snapshot:
            continue  # written after the import — this repository's own numbering
        tokens = ["#" + m.group(1) for m in CITATION.finditer(line)]
        if not tokens:
            continue
        context = line + "\n" + (lines[lineno - 2] if lineno >= 2 else "")
        attributed = bool(ATTRIBUTED.search(context))
        for token in tokens:
            if (path, token) in ALLOWED:
                used.add((path, token))
                continue
            if attributed:
                continue
            failures.append((path, lineno, token, line.strip()))

# ------------------------------------------------------------------------- the two verdicts
if failures:
    print(
        f"{len(failures)} bare legacy citation(s): the line was in the tree at the import "
        f"boundary {BOUNDARY[:7]}, so the number names the previous repository's change, "
        "not this one's. Write it as `legacy PR N` (or `legacy issue N`), or add a named "
        "allowance if it is not a citation at all:"
    )
    for path, lineno, token, line in failures:
        print(f"  {path}:{lineno}: {token} — {line}")
    sys.exit(1)

stale = sorted(set(ALLOWED) - used)
assert not stale, (
    "these allowances no longer match a boundary line and must be removed — a stale "
    f"exemption is how a real citation slips back in under cover of a fixture: {stale}"
)

print(
    f"legacy citations resolved — boundary {BOUNDARY[:7]}, {len(candidates)} file(s) checked "
    f"against its snapshot, "
    f"{len(ALLOWED)} named allowance(s), all still matching"
)
PY
