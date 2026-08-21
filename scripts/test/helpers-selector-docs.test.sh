#!/usr/bin/env bash
# Pin the documented local helper gate to the `helpers` selector that actually runs it.
#
# Two claims live in two places and must agree. `.github/workflows/ci.yml` decides which
# changed paths select the `automation` job; the skills tell an agent which changed paths
# oblige it to run that job's suite locally. When the workflow grew the three skill
# directories, both skills kept saying `scripts/` and `.github/` — and one of them still
# listed `.claude/` as a path with no gate at all, which is the exact opposite of what CI
# had just started doing. Nothing failed, because nothing compared them.
#
# The third check is about the gate's verdict rather than its trigger: the documented block
# is a script, and a script that prints FAILED and exits 0 is not a gate. It is run here, in
# both directions, against fixtures.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1])
workflow = (root / ".github/workflows/ci.yml").read_text()
dev_issue = (root / ".claude/skills/dev-issue/SKILL.md").read_text()
process_pr = (root / ".claude/skills/process-pr/SKILL.md").read_text()


def exactly_one(pattern, text, label):
    matches = re.findall(pattern, text, flags=re.MULTILINE | re.DOTALL)
    if len(matches) != 1:
        raise AssertionError(f"expected one {label}, found {len(matches)}")
    return matches[0]


# ---------------------------------------------------------------- the selector itself
# The grep in detect's `helpers` step is the source of truth. Read the alternation out of
# it rather than restating the list here: a copy in this file would be one more thing to
# keep in step by hand, which is the failure being tested for.
alternation = exactly_one(
    r"grep -Eq '\^\(([^)]+)\)/'", workflow, "helpers selector grep in ci.yml"
)
prefixes = [alt.replace("\\.", ".") for alt in alternation.split("|")]

assert prefixes, "helpers selector matched nothing"
for skill_dir in (".claude", ".agents", ".opencode"):
    assert skill_dir in prefixes, (
        f"{skill_dir}/ must select the automation job — agent-skills-sync.test.sh is the only "
        "test that catches a stale adapter, and it runs in that job"
    )

# ------------------------------------------------------- /dev-issue documents every one
# Scoped to the sentence that states the condition, not to the file: the whole point is
# that a mention somewhere else in the document does not tell an agent when to run this.
condition = exactly_one(
    r"\*\*If the diff touches(.*?)```bash", dev_issue, "dev-issue local helper-gate condition"
)
missing = [p for p in prefixes if f"`{p}/`" not in condition]
assert not missing, (
    "/dev-issue Step 6 does not name every path that selects the automation job: "
    f"missing {missing}. ci.yml selects {prefixes}."
)

# The same sentence's mirror image: the "nothing to run here" list must not claim a path
# that CI gates. This is the regression that shipped — `.claude/` sat in both roles.
no_gate = exactly_one(
    r"A diff that touches none of the above \(([^)]*)\)", dev_issue, "dev-issue no-gate list"
)
contradictions = [p for p in prefixes if p in no_gate]
assert not contradictions, (
    f"/dev-issue calls {contradictions} gate-free while ci.yml selects the automation job "
    "for those paths"
)

# ------------------------------------------------- /process-pr's detector matches exactly
# This one is executable, so hold it to equality in both directions rather than
# containment: an extra prefix here would send an agent to run a suite CI never selected.
selector = exactly_one(
    r"TOUCHES_SCRIPTS=\$\(gh pr view <pr-number> --json files --jq '(.*?)'\)",
    process_pr,
    "process-pr TOUCHES_SCRIPTS selector",
)
documented = set(re.findall(r'startswith\("([^"]+)/"\)', selector))
assert documented == set(prefixes), (
    "/process-pr Step 2 detects a different path set than ci.yml selects: "
    f"skill={sorted(documented)} ci.yml={sorted(prefixes)}"
)

# ------------------------------------------------------------- the block reports failure
# Extract the documented gate and run it. Both concrete commands are swapped for fixtures:
# the suite glob points at a temp directory, and the python line becomes `true` or `false`
# so the trailing command's own failure is covered too.
block = exactly_one(
    r"```bash\n(failed=0\n.*?)```", dev_issue, "dev-issue helper-suite gate block"
)
assert "scripts/test/*.test.sh" in block, "gate block must glob the suite, not enumerate it"


def run_gate(fixture_dir, python_stub):
    script = block.replace("scripts/test/*.test.sh", f"{fixture_dir}/*.test.sh").replace(
        "python3 .github/scripts/test_deepseek_review.py", python_stub
    )
    return subprocess.run(
        ["bash", "-c", script], capture_output=True, text=True
    ).returncode


with tempfile.TemporaryDirectory() as tmp:
    (Path(tmp) / "a.test.sh").write_text("exit 0\n")
    (Path(tmp) / "b.test.sh").write_text("exit 0\n")
    assert run_gate(tmp, "true") == 0, "gate must pass when every test and the python suite pass"

    (Path(tmp) / "b.test.sh").write_text("echo broken; exit 1\n")
    assert run_gate(tmp, "true") != 0, (
        "gate exited 0 with a failing helper test — this is the `|| { echo FAILED; break; }` "
        "masking bug: the loop's status is the break's, and the trailing command overwrites it"
    )
    assert run_gate(tmp, "false") != 0, "gate must fail when the python suite fails"

    # A failing test must not stop the ones after it: CI reports every helper test, so a
    # local gate that stops at the first sends you back for a round it could have saved.
    (Path(tmp) / "a.test.sh").write_text("echo FIRST-RAN; exit 1\n")
    (Path(tmp) / "b.test.sh").write_text("echo LAST-RAN; exit 0\n")
    script = block.replace("scripts/test/*.test.sh", f"{tmp}/*.test.sh").replace(
        "python3 .github/scripts/test_deepseek_review.py", "true"
    )
    done = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
    assert "LAST-RAN" in done.stdout, (
        "gate stopped at the first failing test; it must run the whole suite and still fail"
    )

# ------------------------------------------------- the enumerated list vs. the directory
# The local gate globs `scripts/test/`; the automation job enumerates it. That asymmetry is
# deliberate — a test that cares about being run says so itself — but it only holds while
# the list is complete, and nothing checked completeness. The drift this PR fixes ran one
# way (a documented list shorter than the directory, so a test never ran locally); this is
# the same drift running the other way, where a test file lands in the directory, passes the
# globbed local gate, and is never invoked by CI at all. This very file was almost that.
job = exactly_one(
    r"\n  automation:\n(.*?)(?=\n  [a-z][\w-]*:\n)", workflow, "automation job block"
)
# Anchored to the start of the command line, so a commented-out invocation does not read as
# a test CI runs. Unanchored, `# bash scripts/test/x.test.sh` matched, and a test somebody
# had deliberately disabled would have satisfied the completeness check below — the guard
# failing in precisely the direction it exists to prevent.
enumerated = set(
    re.findall(r"^\s*bash (scripts/test/[\w.-]+\.test\.sh)", job, re.MULTILINE)
)
on_disk = {
    f"scripts/test/{p.name}" for p in (root / "scripts/test").glob("*.test.sh")
}
assert on_disk, "no helper tests found on disk"
unrun = sorted(on_disk - enumerated)
assert not unrun, (
    f"these helper tests exist but the automation job never runs them: {unrun}. "
    "Add them to .github/workflows/ci.yml — the local gate's glob will not catch this."
)
missing_files = sorted(enumerated - on_disk)
assert not missing_files, (
    f"the automation job invokes helper tests that do not exist: {missing_files}"
)

print(
    f"helpers selector documented and enforced — prefixes={prefixes}; "
    f"{len(on_disk)} helper tests, all enumerated in ci.yml"
)
PY
