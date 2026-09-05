#!/usr/bin/env bash
# Pin the documented local helper gate to the `helpers` selector that actually runs it.
#
# CI always runs the suite because helper tests read across workspace boundaries.
# Pin that selection to both local skill instructions, execute the workflow's shell
# with workspace-only diffs, and retain the gate-verdict and suite-completeness checks.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import os
import re
import subprocess
import sys
import tempfile
import textwrap
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
# Execute the actual classify step, not a copied selector. The gh function supplies
# synthetic changed paths; the bash function can simulate a classifier that has
# stopped recognising every workspace. Its output must never decide automation.
classify = textwrap.dedent(exactly_one(
    r"- name: Classify changed files\n.*?        run: \|\n(.*?)(?=\n  # Go backend gates)",
    workflow,
    "classify step shell",
))
job = exactly_one(
    r"\n  automation:\n(.*?)(?=\n  [a-z][\w-]*:\n)", workflow, "automation job block"
)
assert "if: ${{ !cancelled() && needs.detect.outputs.helpers != 'false' }}" in job, (
    "automation must run on true or missing helpers output, including detect failure"
)
assert "helpers: ${{ steps.classify.outputs.helpers }}" in workflow
assert not re.search(r"^\s*paths(?:-ignore)?:", workflow, re.MULTILINE), (
    "CI must not suppress workload results with path filters"
)

with tempfile.TemporaryDirectory() as tmp:
    output = Path(tmp) / "outputs"
    fixtures = (
        ("server/internal/world/chunk.go", "real"),
        ("client/src/world/palette.rs", "real"),
        ("docs/guide.md", "real"),
        (".claude/skills/dev-issue/SKILL.md", "real"),
        ("server/internal/world/future_input.go", "false"),
        ("client/src/future_input.rs", "crash"),
        ("", "real"),
    )
    for changed_path, mode in fixtures:
        output.write_text("")
        env = dict(os.environ, GITHUB_OUTPUT=str(output), BASE_REF="develop",
                   GITHUB_REPOSITORY="example/project", PR_NUMBER="1",
                   FIXTURE_PATH=changed_path, CLASSIFIER_MODE=mode)
        stub = '''
gh() { printf '%s\\n' "$FIXTURE_PATH"; }
bash() {
  if [ "$1" = scripts/changed-areas.sh ]; then
    case "$CLASSIFIER_MODE" in
      false) cat >/dev/null; printf 'server=false\\nclient=false\\nschemas=false\\n'; return 0 ;;
      crash) cat >/dev/null; return 1 ;;
    esac
  fi
  command bash "$@"
}
'''
        result = subprocess.run(["bash", "-e", "-c", stub + classify], cwd=root,
                                env=env, capture_output=True, text=True)
        selected = dict(line.split("=", 1) for line in output.read_text().splitlines())
        if mode == "crash":
            assert result.returncode != 0, "classifier crash must fail detect"
            assert selected.get("helpers") != "false", "crash must not skip automation"
        else:
            assert result.returncode == 0, result.stderr
            assert selected.get("helpers") == "true", (
                f"{changed_path!r} with {mode} classifier skipped automation: {selected}"
            )
        if changed_path == "docs/guide.md":
            assert all(selected[key] == "false" for key in ("server", "client", "schemas")), (
                "docs-only changes should still skip workspace builds"
            )

# ------------------------------------------------------- local gates match CI selection
for label, text in (("dev-issue", dev_issue), ("process-pr", process_pr)):
    assert "**Every PR runs the automation helper suite**" in text, (
        f"{label} must require the same unconditional helper gate as CI"
    )
    assert "PR has no gate to run" not in text
    assert "A diff that touches none of the above" not in text
assert "TOUCHES_SCRIPTS=" not in process_pr, "no local path selector may exempt automation"
agents = (root / "AGENTS.md").read_text()
assert "The `helpers` selector (which gates `automation`) is always true" in agents

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
    "unconditional helpers selection documented and enforced; "
    f"{len(on_disk)} helper tests, all enumerated in ci.yml"
)
PY
