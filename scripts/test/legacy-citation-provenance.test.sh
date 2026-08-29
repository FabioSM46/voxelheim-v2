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
# boundary answers that per line, and that is what drove the sweep. This test asks the same
# question of the snapshot's own text instead, for the reasons set out below.
#
# The boundary is *derived*, not typed in: it is the repository's root commit. A list of
# offending lines typed into a test is a list that goes stale the first time somebody adds a
# legitimate citation, which is the failure mode this repository has hit with hand-kept
# counts more than once.
#
# ---------------------------------------------------------------------------------------
# Three properties this guard has to hold, each of which it failed at some point in review.
# They are recorded because every one of them is a way for the test to keep passing while
# the thing it protects rots, and that is the only failure mode a guard really has.
#
#   1. **It reads the snapshot's text, not `git blame`.** Blame classified the sweep and
#      cannot guard it: a line the sweep rewrote blames to the sweep, a line somebody
#      reverts blames to the revert, so the answer is never "the boundary". A blame-driven
#      guard therefore catches a citation the sweep *missed* and never one that comes
#      *back* — blind on exactly the lines it exists to watch. Found because a negative
#      case passed when it should have failed.
#
#   2. **Attribution must be attached to the citation, and must itself be pre-import.**
#      `clinic-deck #464` resolves; a loose `clinic-deck` somewhere on the line does not —
#      "clinic-deck is separate; see #147" would have exempted `#147` for no reason. Worse,
#      the attribution was accepted from the preceding line without asking where that line
#      came from, so a bare citation on an untouched boundary line could be laundered by
#      adding a sentence above it today. The attributing line must be in the snapshot too.
#
#   3. **It matches lines across the whole snapshot, not per path.** Keyed by path, a
#      `git mv` since the import removed a file from the guard entirely, citations and all.
#      See "why global" below.
# ---------------------------------------------------------------------------------------
#
# Two things are deliberately not citations and are handled without an allowance:
#
#   * A `#` followed by digits and then more alphanumerics is not a citation — `#78787D` is
#     a colour, `%#08x` is a format verb. That is settled by the token grammar rather than
#     by an allowance, because a grammar cannot go stale and cannot be widened by accident.
#   * A citation that names the repository it belongs to already resolves. `clinic-deck #464`
#     says which tree it means; so does `legacy PR 147`, which carries no `#` at all.
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


def git(*args, check=True):
    return subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True, check=check
    ).stdout


# ------------------------------------------------------------------ the boundary itself
# Read, not hard-coded. The import is the root commit: it holds the snapshot, and every
# line written in this repository came after it. Fail closed on anything else — a shallow
# clone answers nothing here, and a test that cannot see the boundary must say so rather
# than pass. The `automation` job checks out with fetch-depth: 0 for exactly this.
roots = git("rev-list", "--max-parents=0", "HEAD").split()
if len(roots) != 1:
    raise AssertionError(
        f"expected exactly one root commit to serve as the import boundary, found "
        f"{len(roots)}: {roots}. Without an unambiguous boundary this property cannot be "
        "evaluated, and passing would assert something nothing checked."
    )
BOUNDARY = roots[0]

# ------------------------------------------------------------------- what counts as one
# Digits terminated by anything that is not alphanumeric. This is what separates `#131`
# from `#78787D` and `%#08x` without an exemption list standing between them.
CITATION = re.compile(r"#(\d{1,4})(?![0-9A-Za-z])")

# Attribution, *attached* to the citation it explains: the repository name, an optional
# "PR"/"issue", then the number — and a `/`-separated chain, because `#315/#317` is one
# reference to two changes. A bare `clinic-deck` elsewhere on the line explains nothing.
ATTACHED = re.compile(
    r"clinic-deck\s+(?:PR|PRs|issue)?\s*#\d{1,4}(?:\s*/\s*#\d{1,4})*", re.IGNORECASE
)
# Prose wraps, so an attribution may sit at the tail of the previous line with its citation
# at the head of this one. Both halves are required: the tail must *end* with the name, and
# the citation must be in the leading run of `#N`s after nothing but comment punctuation.
WRAP_TAIL = re.compile(r"clinic-deck\s*$", re.IGNORECASE)
WRAP_HEAD = re.compile(r"^[\s#/*>|+-]*(#\d{1,4}(?:\s*/\s*#\d{1,4})*)")


def attached_tokens(line):
    """Tokens on `line` that an attribution on the same line already explains."""
    found = set()
    for m in ATTACHED.finditer(line):
        found.update("#" + t for t in CITATION.findall(m.group(0)))
    return found


def wrapped_tokens(line):
    """Tokens at the head of `line`, i.e. the ones a wrapped attribution could reach."""
    m = WRAP_HEAD.match(line)
    return {"#" + t for t in CITATION.findall(m.group(1))} if m else set()


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

# The citations that reach the tree only through the wrapped-attribution path. Pinned by
# name: both tightenings this guard has already been through would have silently dropped
# them, and a guard that quietly stops exempting is a guard somebody will "fix" by deleting
# the wrap path. The reviewer script's former wrapped #466 now carries `clinic-deck` on the
# citation line itself after its acknowledgement policy was reworded, so it no longer belongs
# to this set. If this set changes again, say why.
WRAPPED_PIN = {
    ("scripts/gh-automation.sh", "#315"),
    ("scripts/gh-automation.sh", "#317"),
}


# ------------------------------------------------------------------------------ the scan
def scan(files, boundary_citations, boundary_attributions, allowed):
    """Pure over its inputs so the fixtures below can drive it. `files` is {path: [lines]}."""
    failures, used, wrapped = [], set(), set()
    for path, lines in sorted(files.items()):
        for lineno, line in enumerate(lines, start=1):
            if line not in boundary_citations:
                continue  # written after the import — this repository's own numbering
            tokens = ["#" + m.group(1) for m in CITATION.finditer(line)]
            if not tokens:
                continue
            same = attached_tokens(line)
            prev = lines[lineno - 2] if lineno >= 2 else None
            # The attributing line must be pre-import too, or a sentence added today
            # launders a citation that was here all along.
            reachable = (
                wrapped_tokens(line)
                if prev is not None
                and prev in boundary_attributions
                and WRAP_TAIL.search(prev)
                else set()
            )
            for token in tokens:
                if (path, token) in allowed:
                    used.add((path, token))
                elif token in same:
                    pass
                elif token in reachable:
                    wrapped.add((path, token))
                else:
                    failures.append((path, lineno, token, line.strip()))
    return failures, used, wrapped


# -------------------------------------------------------------------------- why global
# The snapshot's lines are collected across every blob at the boundary and matched against
# every tracked line, regardless of path. Keyed by path instead, a `git mv` since the import
# silently removed a file from the guard together with all its citations — and a rename is
# an ordinary refactor here, while the guard's whole job is to survive ordinary work.
#
# The price is a wider over-approximation: a line written today that is byte-identical to a
# snapshot line carrying a bare `#N` is flagged wherever it lives. That is somebody copying
# an old comment forward, which is the thing this test exists to stop, so the direction is
# the safe one — and it is not free of false positives. The realistic one is the shortest
# snapshot citation line, `  (#43);`: a genuine new reference to *this* repository's #43,
# written with that exact indentation, would be flagged. The cost of that is a red run
# naming the file and line, and a one-line allowance or a reworded comment; the cost of the
# per-path form was a silent hole. Loud and occasionally wrong beats quiet and wrong.
def boundary_lines(pattern):
    out = git("grep", "-h", "-I", "-E", pattern, BOUNDARY, "--", ".", check=False)
    return set(out.split("\n"))


boundary_citations = {l for l in boundary_lines(r"#[0-9]{1,4}") if CITATION.search(l)}
boundary_attributions = {
    l for l in boundary_lines(r"[Cc]linic-deck") if re.search("clinic-deck", l, re.I)
}
if not boundary_citations:
    raise AssertionError(
        f"no citation lines found at the boundary {BOUNDARY[:7]} — the snapshot could not "
        "be read, and an empty comparison set would pass this test against anything."
    )

tracked = {}
for path in git("ls-files", "-z").split("\0"):
    if not path or "/gen/" in f"/{path}" or "_generated." in Path(path).name:
        continue
    try:
        tracked[path] = (root / path).read_text(encoding="utf-8").split("\n")
    except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
        continue

failures, used, wrapped = scan(
    tracked, boundary_citations, boundary_attributions, ALLOWED
)

if failures:
    print(
        f"{len(failures)} bare legacy citation(s): the line was in the tree at the import "
        f"boundary {BOUNDARY[:7]}, so the number names the previous repository's change, "
        "not this one's. Write it as `legacy PR N` (or `legacy issue N`), attach the "
        "repository it means (`clinic-deck #N`), or add a named allowance if it is not a "
        "citation at all:"
    )
    for path, lineno, token, line in failures:
        print(f"  {path}:{lineno}: {token} — {line}")
    sys.exit(1)

stale = sorted(set(ALLOWED) - used)
if stale:
    raise AssertionError(
        "these allowances no longer match a boundary line and must be removed — a stale "
        f"exemption is how a real citation slips back in under cover of a fixture: {stale}"
    )
if wrapped != WRAPPED_PIN:
    raise AssertionError(
        "the set of citations exempted by a wrapped attribution changed.\n"
        f"  pinned:  {sorted(WRAPPED_PIN)}\n"
        f"  found:   {sorted(wrapped)}\n"
        "Gained one: attach the attribution to the citation instead if you can. Lost one: a "
        "tightening dropped a case that used to be covered — confirm it is now attributed "
        "some other way rather than simply unguarded, then update the pin."
    )

# ------------------------------------------------------- the guard's own negative cases
# Every hole below was a real one this file shipped with or was reviewed for. They run
# against fixtures rather than the tree so they keep testing the rule after the tree stops
# containing an example of it — which is the same reason the tree is checked against the
# snapshot rather than against a list.
# Every fixture line here stands in for a line the snapshot contains. Nothing is skipped
# conditionally and nothing is counted by hand: the cases are a table, the table is walked,
# and the summary reports its length. A case that quietly does not run is the same defect as
# an attribution that quietly does not attribute.
CITE = "// the fallback answered them (#131)."
LOOSE = "// clinic-deck is separate; see #147."
TAIL = "// crowded the budget (clinic-deck"
HEAD = "#317). A check that does not exist is not a passing check"
FIX_CITATIONS = {CITE, LOOSE, HEAD}
FIX_ATTRIBUTIONS = {TAIL}

CASES = [
    # label, files, expect_failure, expected "path:line" of the first failure
    ("a bare citation on a snapshot line",
     {"a.rs": [CITE]}, True, "a.rs:1"),

    # An attribution-shaped sentence above a citation exempts nothing on its own. Only
    # `clinic-deck` at the tail of the preceding line can reach across the break, and
    # `legacy PR` never attributes anything — a rewritten citation carries no `#` at all.
    ("a sentence above the citation that only looks like an attribution",
     {"a.rs": ["// legacy PR 999 explains everything below", CITE]}, True, "a.rs:2"),

    # Finding 3. The other repository is named, but not as this citation's attribution.
    ("an unrelated clinic-deck sentence on the same line",
     {"a.rs": [LOOSE]}, True, "a.rs:1"),

    # Finding 2. The path never existed at the boundary; the line did.
    ("a legacy citation carried through a rename",
     {"moved/elsewhere.rs": [CITE]}, True, "moved/elsewhere.rs:1"),

    # The wrap must survive both tightenings above: tail attribution, head citation, and an
    # attributing line that is itself pre-import.
    ("a wrapped attribution", {"a.sh": [TAIL, HEAD]}, False, None),

    # Finding 1, and the case that fails if the snapshot-membership test on the preceding
    # line is ever dropped: the tail is attribution-shaped but was written today, so a
    # citation that has been here since the import cannot be laundered by it.
    ("a wrapped attribution forged after the import",
     {"a.sh": ["// something (clinic-deck", HEAD]}, True, "a.sh:2"),
]

for label, files, expect_fail, expect_where in CASES:
    got, _, _ = scan(files, FIX_CITATIONS, FIX_ATTRIBUTIONS, {})
    if expect_fail:
        if not got:
            raise AssertionError(f"negative case '{label}': expected a failure, got none")
        where = f"{got[0][0]}:{got[0][1]}"
        if where != expect_where:
            raise AssertionError(
                f"negative case '{label}': expected the failure at {expect_where}, got {where}"
            )
    elif got:
        raise AssertionError(f"case '{label}': expected no failure, got {got}")

print(
    f"legacy citations resolved — boundary {BOUNDARY[:7]}, {len(boundary_citations)} "
    f"snapshot citation line(s) vs {len(tracked)} tracked file(s), {len(ALLOWED)} named "
    f"allowance(s) all matching, {len(wrapped)} wrapped attribution(s) pinned, "
    f"{len(CASES)} negative cases green"
)
PY
