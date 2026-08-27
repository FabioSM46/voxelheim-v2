#!/usr/bin/env bash
# Pin post-merge verification of `develop`: the workflow's shape, its gate, its
# fail-closed commit range, and the alarm it files.
#
# `.github/workflows/integration.yml` is a second copy of ci.yml's workload, and the
# repository already knows what that costs — client-cache.yml is the other one, and
# client-cache-parity.test.sh exists because a comment claiming two files agreed was a
# guarantee nothing checked. So nothing here restates what ci.yml runs. The expected job
# bodies are DERIVED from ci.yml and compared line for line; a gate added there and not
# here reddens the `automation` job that runs this file.
#
# Four traps were named when this was specified, and three of them are structural — the
# pins below are what make them stay fixed rather than fixed once:
#
#   1. `concurrency` keyed on `github.event.pull_request.number` is empty on a push, so
#      every run would share the group `ci-`. There is no concurrency group here at all;
#      the assertion is broader and simpler — the pull-request context appears nowhere in
#      this workflow.
#   2. `detect` cannot run outside a pull request (it reads `base.sha`, `head.sha`,
#      `.number` and `pulls/<n>/files`). Same assertion covers it, plus: no classifier.
#   3. `check-commit-privacy.sh <base> <head>` needs `before`/`after` on a push, and
#      `before` is forty zeroes on a branch creation. The step's own shell block is
#      extracted and EXECUTED here against fixtures, because "fails closed" is a claim
#      about behaviour and this repository pins those by running them.
#   4. No `paths:` filters. A path-filtered job creates no result for a gate to audit,
#      and `verdict` audits every job in this workflow.
#
# And one acceptance criterion that is about what this must NOT touch: `READY TO MERGE`
# has to stay reachable while develop is red, so `REQUIRED_CHECK` and the frozen rule are
# checked to be exactly what they were.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

pass=0
fail=0

assert_eq() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected '${expected}', got '${actual}'"
    fail=$((fail + 1))
  fi
}

assert_nonzero() {
  local name="$1" actual="$2"
  if [ "$actual" -ne 0 ]; then
    echo "  ok   — ${name} (exit ${actual})"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected a non-zero exit, got 0"
    fail=$((fail + 1))
  fi
}

assert_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: expected to find '${needle}' in:"
    printf '           %s\n' "$haystack"
    fail=$((fail + 1))
  fi
}

assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}: did NOT expect '${needle}' in:"
    printf '           %s\n' "$haystack"
    fail=$((fail + 1))
  fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# ── 1. The workflow's shape, derived from ci.yml ─────────────────────────────
echo
echo "integration.yml — shape, derived from ci.yml"
if python3 - "$ROOT" "$WORK" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
work = Path(sys.argv[2])
ci = (root / ".github/workflows/ci.yml").read_text()
integration_raw = (root / ".github/workflows/integration.yml").read_text()
automation_sh = (root / "scripts/gh-automation.sh").read_text()


def strip_comments(text):
    """The workflow with comment-only lines removed.

    Every assertion below is about what the file EXECUTES. integration.yml's comments
    quote the very expressions it must not evaluate — `github.event.pull_request.number`
    is named there precisely to explain why it is absent — so matching against the raw
    text would fail on the explanation rather than on the mistake. Trailing comments on
    a `uses:` line stay: `# v4` is the release label actions-hardening.test.sh requires.
    """
    return "\n".join(
        line for line in text.splitlines()
        if not line.strip().startswith("#")
    ) + "\n"


integration = strip_comments(integration_raw)


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


# The same boundary guard client-cache-parity.test.sh carries: extraction must stop at
# the next job key, so an unrelated job inserted after one being compared can never be
# read as part of it.
boundary_fixture = "jobs:\n  server:\n    timeout-minutes: 20\n  inserted_job:\n    timeout-minutes: 99\n"
assert "timeout-minutes: 99" not in job_block(boundary_fixture, "server"), (
    "job extraction must stop at any following job key"
)


def executable_lines(job):
    """The job's steps with comments and blank lines removed.

    Comments are dropped on purpose: integration.yml's point to ci.yml rather than
    repeating its post-mortems, and a copied paragraph is not what has to stay in step.
    Everything that EXECUTES is compared exactly — step names, action refs, `if:`
    guards, `with:` inputs and every line of every `run:` block."""
    steps = exactly_one(r"^    steps:\s*$\n([\s\S]*)\Z", job, "steps block")
    return [
        line for line in steps.splitlines()
        if line.strip() and not line.strip().startswith("#")
    ]


# ── the workload jobs are ci.yml's, minus their selectors ────────────────────
for name in ("server", "client", "schemas"):
    ci_job = job_block(ci, name)
    integration_job = job_block(integration, name)
    assert executable_lines(ci_job) == executable_lines(integration_job), (
        f"integration.yml's {name} job no longer runs what ci.yml's does.\n"
        "Post-merge verification is a second copy of the same workload; when a gate "
        "changes in ci.yml it has to change here too, in the same words."
    )
    # The selector is the ONLY thing that may differ, and it must be gone rather than
    # rewritten: a post-merge run classifies nothing.
    assert "needs: detect" in ci_job, f"ci.yml's {name} job no longer needs detect"
    assert "needs:" not in integration_job, (
        f"integration.yml's {name} job must not depend on a classifier"
    )

# ── no pull-request context anywhere (traps 1 and 2) ─────────────────────────
assert "pull_request" not in integration, (
    "integration.yml names the pull-request context. On a push "
    "`github.event.pull_request.*` is empty: a concurrency group keyed on `.number` "
    "collapses every run into one group under cancel-in-progress, and a classifier "
    "reading `.base.sha`/`.head.sha`/`pulls/<n>/files` has nothing to read."
)
assert "changed-areas.sh" not in integration, (
    "integration.yml must not classify a diff — the point of a post-merge run is the "
    "combination, and a combination is not describable as a diff"
)

# ── no paths: filters (trap 4) ───────────────────────────────────────────────
assert not re.search(r"^\s*paths(-ignore)?:\s*$", integration, re.MULTILINE), (
    "integration.yml must carry no `paths:` filter — a path-filtered job creates no "
    "result for the verdict job to audit"
)

# ── the trigger ──────────────────────────────────────────────────────────────
on_block = exactly_one(r"^on:\s*$\n([\s\S]*?)(?=^\S)", integration, "on: block")
assert re.search(r"^  push:\s*$", on_block, re.MULTILINE), (
    "integration.yml must run on push"
)
assert re.search(r"^    branches:\s*\[develop\]\s*$", on_block, re.MULTILINE), (
    "integration.yml must run on pushes to develop"
)
# `main` is human-only and nothing here may touch it.
assert "main" not in on_block, "integration.yml must not trigger on main"

# ── the automation job globs the suite ───────────────────────────────────────
integration_automation = job_block(integration, "automation")
assert "scripts/test/*.test.sh" in integration_automation, (
    "integration.yml's automation job must glob the helper suite rather than enumerate "
    "it: this run has no selector to exempt anything from, so the superset is the whole "
    "directory"
)
assert 'python3 .github/scripts/test_deepseek_review.py' in integration_automation, (
    "integration.yml's automation job must run the reviewer's own test suite"
)
assert '[ "$failed" -eq 0 ]' in integration_automation, (
    "the helper loop's verdict line is missing — without it the block's exit status is "
    "the last echo's, which is a gate that prints FAILED and exits 0"
)
enumerated = set(
    re.findall(r"^\s*bash (scripts/test/[\w.-]+\.test\.sh)", job_block(ci, "automation"), re.MULTILINE)
)
on_disk = {f"scripts/test/{p.name}" for p in (root / "scripts/test").glob("*.test.sh")}
assert enumerated <= on_disk, (
    "ci.yml enumerates helper tests that do not exist, so the glob here cannot cover them"
)

# ── the verdict job ──────────────────────────────────────────────────────────
verdict = job_block(integration, "verdict")
assert re.search(r"^    name: integration-verdict\s*$", verdict, re.MULTILINE), (
    "the verdict job must carry a name of its own"
)
assert "ci-gate" not in verdict, (
    "the verdict job must not be named or aliased `ci-gate`: that name is the frozen "
    "rule's REQUIRED_CHECK, and a red develop must never make an open pull request "
    "unmergeable"
)
needs = exactly_one(r"^    needs: \[([^\]]+)\]\s*$", verdict, "verdict needs list")
declared = {n.strip() for n in needs.split(",")}
# Scoped to the jobs section: `on:` carries a `  push:` key at the same indent, and a
# verdict that "audits every job" must not be asked to audit a trigger.
jobs_section = exactly_one(r"^jobs:\s*$\n([\s\S]*)\Z", integration, "jobs section")
workload = set(re.findall(r"^  ([a-z][\w-]*):\s*$", jobs_section, re.MULTILINE)) - {"verdict"}
assert declared == workload, (
    f"the verdict must audit every job in this workflow: needs={sorted(declared)} "
    f"jobs={sorted(workload)}"
)
assert re.search(r"^    if: \$\{\{ always\(\) \}\}\s*$", verdict, re.MULTILINE), (
    "the verdict job must run with always(): its whole purpose is to speak when "
    "something upstream did not"
)
assert re.search(r"^      issues: write\s*$", verdict, re.MULTILINE), (
    "the verdict job needs issues: write to file the alarm"
)
assert "secrets.GITHUB_TOKEN" in verdict and "GH_PIPELINE_TOKEN" not in integration, (
    "the report runs on GITHUB_TOKEN with job-scoped permissions — no new secret"
)
assert "bash scripts/integration-gate.sh" in verdict, "the verdict must run the gate"
assert "bash scripts/gh-automation.sh integration-report" in verdict, (
    "the verdict must file a report when the gate rejects develop"
)
assert "|| status=$?" in verdict, (
    "the gate must be run with `|| status=$?`: a bare failing command under `set -e` "
    "ends the step where it stands, and the report below would never run"
)
# Every job the verdict audits must hand the gate its result.
for job in sorted(workload):
    assert f"{job.upper()}_RESULT: ${{{{ needs.{job}.result }}}}" in verdict, (
        f"the verdict does not pass {job}'s result to the gate, so the gate cannot read it"
    )

# ── the gate and the report audit exactly the jobs that exist ────────────────
# A job added to the workflow and not to these two lists would run, fail, and be read by
# nobody: the gate would never look at its result and the report would never name it.
# Both lists are therefore derived from the workflow rather than trusted.
gate_sh = (root / "scripts/integration-gate.sh").read_text()
audited = set(re.findall(r'^expect_success "([a-z][\w-]*)"', gate_sh, re.MULTILINE))
assert audited == workload, (
    f"scripts/integration-gate.sh audits {sorted(audited)} but the workflow runs "
    f"{sorted(workload)} — an unaudited job is one whose failure nothing reads"
)
for job in sorted(workload):
    assert f"${{{job.upper()}_RESULT:-}}" in gate_sh, (
        f"the gate reads no {job.upper()}_RESULT, so the verdict's env var reaches nothing"
    )
reported = exactly_one(r'^INTEGRATION_JOBS="([^"]+)"\s*$', automation_sh, "INTEGRATION_JOBS")
assert set(reported.split()) == workload, (
    f"gh-automation.sh reports on {sorted(reported.split())} but the workflow runs "
    f"{sorted(workload)} — the alarm would not name the job that failed"
)

# ── ci.yml's pull-request contract is untouched ──────────────────────────────
ci_on = exactly_one(r"^on:\s*$\n([\s\S]*?)(?=^\S)", ci, "ci.yml on: block")
assert ci_on.strip() == "pull_request:\n    branches: [main, develop]".strip(), (
    "ci.yml's trigger changed. Post-merge verification lives in its own workflow "
    f"precisely so this stays as it was; got:\n{ci_on}"
)
assert "integration" not in job_block(ci, "ci_gate"), (
    "ci-gate must know nothing about post-merge verification"
)

# ── the frozen rule is untouched ─────────────────────────────────────────────
required = exactly_one(
    r'^REQUIRED_CHECK="\$\{REQUIRED_CHECK:-([^}"]+)\}"\s*$', automation_sh, "REQUIRED_CHECK"
)
assert required == "ci-gate", (
    f"REQUIRED_CHECK must stay `ci-gate`, got {required!r} — the new check must never "
    "enter the frozen acceptance rule"
)
status_json = exactly_one(
    r"^cmd_pr_status_json\(\) \{\n([\s\S]*?)^\}\s*$", automation_sh, "cmd_pr_status_json"
)
assert "integration" not in status_json.lower(), (
    "cmd_pr_status_json must be unaffected by post-merge verification"
)

# ── AGENTS.md records the change ─────────────────────────────────────────────
agents = (root / "AGENTS.md").read_text()
assert "integration.yml" in agents, (
    "AGENTS.md must name the workflow that now verifies develop after a merge"
)

# ── this test runs ───────────────────────────────────────────────────────────
invocation = "bash scripts/test/integration-verify.test.sh"
assert job_block(ci, "automation").count(invocation) == 1, (
    "ci.yml's automation job must execute this test exactly once"
)

# ── hand the shell block under test to the bash half ─────────────────────────
step = exactly_one(
    r"^      - name: Reject commit privacy leaks\n([\s\S]*?)(?=^      - |^  [a-z])",
    integration,
    "commit privacy step",
)  # comment-free, so the extracted block is exactly what the runner executes
block = exactly_one(r"^        run: \|\n([\s\S]*)\Z", step, "commit privacy run block")
(work / "privacy-step.sh").write_text(
    "".join(line[10:] if line.startswith(" " * 10) else line
            for line in block.splitlines(keepends=True))
)
print("  ok   — integration.yml mirrors ci.yml's workload and touches nothing it must not")
PY
then
  pass=$((pass + 1))
else
  echo "  FAIL — static pins (see the traceback above)"
  fail=$((fail + 1))
fi

# ── 2. The gate: every job must reach exactly `success` ──────────────────────
echo
echo "integration-gate.sh — anything other than success is rejected"
run_gate() {
  OUT=$(PRIVACY_RESULT="${1}" SERVER_RESULT="${2}" CLIENT_RESULT="${3}" \
        SCHEMAS_RESULT="${4}" AUTOMATION_RESULT="${5}" INTEGRATION_SHA=deadbeef \
        bash "$ROOT/scripts/integration-gate.sh" 2>&1)
  RC=$?
}

run_gate success success success success success
assert_eq "an all-green matrix is accepted" "0" "$RC"
assert_contains "and says which commit it accepted" "$OUT" "accepted develop at deadbeef"

run_gate success failure success success success
assert_nonzero "a failed job is rejected" "$RC"
assert_contains "and is named" "$OUT" "server: expected success, got failure"

run_gate success success skipped success success
assert_nonzero "a SKIPPED job is rejected, not read as authorised" "$RC"
assert_contains "and is named" "$OUT" "client: expected success, got skipped"

run_gate success success success cancelled success
assert_nonzero "a cancelled job is rejected" "$RC"

run_gate success success success success ""
assert_nonzero "an empty result is rejected — 'cannot tell' is never 'fine'" "$RC"
assert_contains "and says why it is empty" "$OUT" "missing from the verdict"

run_gate "" success success success success
assert_nonzero "the privacy job is audited too" "$RC"

# ── 3. The commit range fails closed (trap 3) ────────────────────────────────
echo
echo "integration.yml — the commit privacy range fails closed"
FIXTURE="$WORK/repo"
mkdir -p "$FIXTURE/scripts"
git -C "$FIXTURE" init -q 2>/dev/null || git init -q "$FIXTURE"
git -C "$FIXTURE" config user.email "fixture@example.invalid"
git -C "$FIXTURE" config user.name "Fixture"
echo one > "$FIXTURE/a.txt"
git -C "$FIXTURE" add -A && git -C "$FIXTURE" commit -qm "first"
BEFORE_REAL=$(git -C "$FIXTURE" rev-parse HEAD)
echo two > "$FIXTURE/a.txt"
git -C "$FIXTURE" add -A && git -C "$FIXTURE" commit -qm "second"
AFTER_REAL=$(git -C "$FIXTURE" rev-parse HEAD)

# The stub stands in for the real scanner and records that it was reached at all: every
# refusal below claims the scan was NOT skipped, and that claim is only worth something
# if something is watching the call.
SCAN_LOG="$WORK/scan.log"
cat > "$FIXTURE/scripts/check-commit-privacy.sh" <<'STUB'
#!/usr/bin/env bash
echo "scanned $1 $2" >> "$SCAN_LOG"
exit 0
STUB
export SCAN_LOG

run_privacy() {
  : > "$SCAN_LOG"
  OUT=$(cd "$FIXTURE" && BEFORE_SHA="$1" AFTER_SHA="$2" bash "$WORK/privacy-step.sh" 2>&1)
  RC=$?
  LOG=$(cat "$SCAN_LOG")
}

ZERO=0000000000000000000000000000000000000000

run_privacy "" "$AFTER_REAL"
assert_nonzero "an empty 'before' is refused" "$RC"
assert_contains "and says the range cannot be read" "$OUT" "the range cannot be read"
assert_not_contains "the scan is not skipped silently" "$LOG" "scanned"

run_privacy "$ZERO" "$AFTER_REAL"
assert_nonzero "a branch-creation 'before' (forty zeroes) is refused" "$RC"
assert_not_contains "no scan is attempted on a zero range" "$LOG" "scanned"

run_privacy "$BEFORE_REAL" ""
assert_nonzero "an empty 'after' is refused" "$RC"
assert_not_contains "no scan is attempted without a head" "$LOG" "scanned"

run_privacy "1234567890123456789012345678901234567890" "$AFTER_REAL"
assert_nonzero "a 'before' this checkout does not hold is refused" "$RC"
assert_contains "and says the previous tip is missing" "$OUT" "previous tip is not present"
assert_not_contains "no scan is attempted on an unreachable base" "$LOG" "scanned"

run_privacy "$BEFORE_REAL" "$AFTER_REAL"
assert_eq "a real push range is scanned" "0" "$RC"
assert_contains "with both ends handed to the scanner" "$LOG" "scanned ${BEFORE_REAL} ${AFTER_REAL}"

# ── 4. The alarm: what a person actually sees ────────────────────────────────
echo
echo "integration-report — the alarm is filed, and never silently lost"
STUB_DIR="$WORK/bin"
CALL_LOG="$WORK/gh.log"
mkdir -p "$STUB_DIR"
cat > "$STUB_DIR/gh" <<'STUB'
#!/usr/bin/env bash
echo "gh $*" >> "$CALL_LOG"
BODY=""
for arg in "$@"; do
  if [ "$prev" = "--body" ] 2>/dev/null; then BODY="$arg"; fi
  prev="$arg"
done
[ -n "$BODY" ] && printf '%s\n' "$BODY" >> "$CALL_LOG.body"
case "$1 ${2:-}" in
  "auth status") exit 0 ;;
  "issue list")
    if [ "${GH_LIST_OK:-1}" != "1" ]; then
      echo "API rate limit exceeded" >&2
      exit 1
    fi
    printf '%s' "${GH_OPEN_ALARMS:-}"
    [ -n "${GH_OPEN_ALARMS:-}" ] && echo
    exit 0
    ;;
  "issue create")
    if [ "${GH_CREATE_OK:-1}" != "1" ]; then
      echo "could not create issue" >&2
      exit 1
    fi
    echo "https://github.com/o/r/issues/77"
    exit 0
    ;;
  "issue comment") exit 0 ;;
esac
exit 0
STUB
chmod +x "$STUB_DIR/gh"
export CALL_LOG

run_report() {
  : > "$CALL_LOG"
  : > "$CALL_LOG.body"
  # `-` and not `:-`: an EMPTY value is one of the cases under test, and `:-` would
  # substitute the default for it — the fixture deciding the verdict instead of the guard.
  OUT=$(PATH="$STUB_DIR:$PATH" GITHUB_REPOSITORY=owner/repo \
        INTEGRATION_SHA="${SHA_IN-abc123}" \
        INTEGRATION_RUN_URL="${URL_IN-https://example.invalid/run/1}" \
        PRIVACY_RESULT="${1}" SERVER_RESULT="${2}" CLIENT_RESULT="${3}" \
        SCHEMAS_RESULT="${4}" AUTOMATION_RESULT="${5}" \
        bash "$ROOT/scripts/gh-automation.sh" integration-report 2>&1)
  RC=$?
  LOG=$(cat "$CALL_LOG")
  BODY=$(cat "$CALL_LOG.body")
}

run_report success failure success success success
assert_eq "a first failure files an issue" "0" "$RC"
assert_contains "through gh issue create" "$LOG" "gh issue create"
assert_contains "the body carries the dedup marker" "$BODY" "<!-- integration-failure -->"
assert_contains "the body names the commit" "$BODY" "abc123"
assert_contains "the body names the run" "$BODY" "https://example.invalid/run/1"
assert_contains "the body names the failing job" "$BODY" "server"
assert_contains "the body says it blocks no pull request" "$BODY" "READY TO MERGE"

GH_OPEN_ALARMS="12
41" run_report success success failure success success
assert_eq "a repeat failure comments instead of duplicating" "0" "$RC"
assert_contains "on the newest open alarm" "$LOG" "gh issue comment 41"
assert_not_contains "and opens no second issue" "$LOG" "gh issue create"

GH_LIST_OK=0 run_report success success success failure success
assert_eq "an unreadable lookup still raises the alarm" "0" "$RC"
assert_contains "by creating an issue anyway" "$LOG" "gh issue create"
assert_contains "and says the lookup failed" "$OUT" "may duplicate"

GH_CREATE_OK=0 run_report success success success success failure
assert_nonzero "a report that could not be filed exits non-zero" "$RC"
assert_contains "and names the commit that failed" "$OUT" "abc123"

run_report success success success success success
assert_nonzero "an all-green matrix files no alarm" "$RC"
assert_not_contains "and writes nothing" "$LOG" "gh issue"

SHA_IN="" run_report success failure success success success
assert_nonzero "a report that names no commit is refused" "$RC"
assert_not_contains "and writes nothing" "$LOG" "gh issue"

URL_IN="" run_report success failure success success success
assert_nonzero "a report that names no run is refused" "$RC"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
