#!/usr/bin/env bash
# Drive scripts/check-body-privacy.sh directly, and pin the properties of the workflow
# that carries it to GitHub.
#
# The checking half is tested the way the Test Strategy on #130 asks for: through the
# script and never through GitHub, because a check whose only exercise is a live issue is
# one nobody can run. Three fixtures assembled at run time — the file scanner reads this
# file too, so a literal workstation path, private-network address or non-public email in
# here would fail the very check it is testing.
#
# The assertion that matters is not that a violating body is refused. It is that the
# refusal does not repeat the value: this check reports into a GitHub comment, which
# carries notifications and an audience, and a finding that quotes the leak has published
# it a second time somewhere the author cannot retract. Every refusal case below searches
# the combined output for the value it rejected.
#
# The workflow half is pinned statically because its hazard is static.
# `pull_request_target` runs with a writable token against the base repository, and the
# way that becomes a compromise is a checkout of the branch under review. Nothing catches
# that at run time — a malicious head only has to be checked out once — so the property is
# asserted against the file.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
CHECK="$REPO_ROOT/scripts/check-body-privacy.sh"
TEST_ROOT=$(mktemp -d)
trap 'rm -rf -- "$TEST_ROOT"' EXIT

# Assembled from pieces, exactly as the other two privacy tests do it.
internal_path="/""home/example-user/voxelheim"
other_internal_path="/""Users/example-user/voxelheim"
private_ip="10.""0.0.7"
unsafe_email="person""@""private.test"

run_check() {
  local script=$1 body=$2
  CHECK_STDOUT=$(bash "$script" < "$body" 2> "$TEST_ROOT/stderr.txt") && CHECK_STATUS=0 || CHECK_STATUS=$?
  CHECK_STDERR=$(cat "$TEST_ROOT/stderr.txt")
  CHECK_COMBINED="${CHECK_STDOUT}"$'\n'"${CHECK_STDERR}"
}

assert_absent() {
  if grep -Fq -- "$1" <<< "$CHECK_COMBINED"; then
    echo "privacy failure: $2 reached the check's output" >&2
    exit 1
  fi
}

# ------------------------------------------------------------------ a clean body passes
# Everything the repository sanctions in one body: a GitHub noreply identity, a reserved
# example domain, the documentation path token, and a public address.
cat > "$TEST_ROOT/clean.md" <<'BODY'
### What happened

The seam appeared after regenerating bindings under <worktree>/schemas.

Reported by fixture@example.invalid, co-authored with
Someone <1000+someone@users.noreply.github.com>.

The dedicated server answered on 203.0.113.9, which is a reserved documentation address.
BODY

run_check "$CHECK" "$TEST_ROOT/clean.md"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected a clean body to exit 0, got $CHECK_STATUS" >&2
  echo "$CHECK_COMBINED" >&2
  exit 1
fi
if [ -n "$CHECK_STDOUT" ]; then
  echo "expected a clean body to print no findings, got: $CHECK_STDOUT" >&2
  exit 1
fi

# An issue with no body at all is legitimate, not an error.
: > "$TEST_ROOT/empty.md"
run_check "$CHECK" "$TEST_ROOT/empty.md"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected an empty body to exit 0, got $CHECK_STATUS" >&2
  exit 1
fi

# ------------------------------------------------- all three categories, one per line
# Line numbers are load-bearing: "where in the body it is" is the half of the finding that
# survives the edit, and it is the only thing a report may say beyond the category.
{
  printf '### Steps to reproduce\n'
  printf 'Built under %s with the default profile.\n' "$internal_path"
  printf 'The daemon answered from %s during the run.\n' "$private_ip"
  printf 'Reported by %s.\n' "$unsafe_email"
} > "$TEST_ROOT/triple.md"

run_check "$CHECK" "$TEST_ROOT/triple.md"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a body carrying all three categories to exit 1, got $CHECK_STATUS" >&2
  echo "$CHECK_COMBINED" >&2
  exit 1
fi
grep -Fqx 'body privacy: line 2 contains a workstation-specific path' <<< "$CHECK_STDOUT"
grep -Fqx 'body privacy: line 3 contains a private-network address' <<< "$CHECK_STDOUT"
grep -Fqx 'body privacy: line 4 contains a non-public email address' <<< "$CHECK_STDOUT"
if [ "$(wc -l <<< "$CHECK_STDOUT")" -ne 3 ]; then
  echo "expected exactly three findings, got: $CHECK_STDOUT" >&2
  exit 1
fi
assert_absent "$internal_path" "the rejected path"
assert_absent "$private_ip" "the rejected address"
assert_absent "$unsafe_email" "the rejected email"

# The caveat is not decoration. A reader who takes a finding as "the leak was stopped" has
# the wrong model of what this check can do, and the only place to correct that is here.
grep -Fq 'already published when this check ran' <<< "$CHECK_STDERR"

# ------------------------------------------------------------ two categories, one line
# The dedupe key is (line, category) and not the line alone. Keyed on the line, the second
# category found on a line would be dropped — which is the one case where a body carries
# two different leaks and the reader has to be told about both.
{
  printf 'Notes\n'
  printf 'Built under %s against %s.\n' "$internal_path" "$private_ip"
} > "$TEST_ROOT/pair.md"

run_check "$CHECK" "$TEST_ROOT/pair.md"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected two categories on one line to exit 1, got $CHECK_STATUS" >&2
  exit 1
fi
grep -Fqx 'body privacy: line 2 contains a workstation-specific path' <<< "$CHECK_STDOUT"
grep -Fqx 'body privacy: line 2 contains a private-network address' <<< "$CHECK_STDOUT"
assert_absent "$internal_path" "the rejected path"
assert_absent "$private_ip" "the rejected address"

# ------------------------------------------- one category twice on a line is one finding
# Two prefixes matching the same line is still one location to look at, and a count of
# occurrences is itself a hint about the content.
{
  printf 'Notes\n'
  printf 'Copied %s to %s.\n' "$internal_path" "$other_internal_path"
} > "$TEST_ROOT/twice.md"

run_check "$CHECK" "$TEST_ROOT/twice.md"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a repeated category to exit 1, got $CHECK_STATUS" >&2
  exit 1
fi
if [ "$(grep -c 'workstation-specific path' <<< "$CHECK_STDOUT")" -ne 1 ]; then
  echo "expected one finding for one line, got: $CHECK_STDOUT" >&2
  exit 1
fi

# ------------------------------------------------------- the approved identities pass
# Same list as the other two scans, held identical by the pin in commit-privacy.test.sh.
# This is that claim from the outside, on the surface this check owns.
{
  printf 'Co-authored-by: Fixture <1000+fixture@users.noreply.github.com>\n'
  printf 'Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>\n'
  printf 'Reported-by: fixture@example.invalid\n'
} > "$TEST_ROOT/approved.md"

run_check "$CHECK" "$TEST_ROOT/approved.md"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected approved identities to pass, got $CHECK_STATUS" >&2
  echo "$CHECK_COMBINED" >&2
  exit 1
fi

# --------------------------------------------------- the misattributed account id
# A noreply address is resolved by the number in front of the plus and by nothing else, so
# one naming the right person with the wrong number is well-formed, carries no private data
# at all, and credits a stranger's account. Reported here even though it is not a leak:
# this surface is where a body transcribes a trailer somebody is about to paste.
#
# Assembled from pieces like every other fixture in this file. Written whole it would fail
# the file scanner on exactly the value it is testing — which is the same reason the
# workstation path above is spelled the way it is.
wrong_id="9999""999999999"
printf 'Co-authored-by: FabioSM46 <%s+FabioSM46''@users.noreply.github.com>\n' "$wrong_id" \
  > "$TEST_ROOT/misattributed.md"

run_check "$CHECK" "$TEST_ROOT/misattributed.md"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a misattributed account id to exit 1, got $CHECK_STATUS" >&2
  echo "$CHECK_COMBINED" >&2
  exit 1
fi
grep -Fqx 'body privacy: line 1 contains a misattributed GitHub account id' <<< "$CHECK_STDOUT"
assert_absent "$wrong_id" "the rejected account id"

# The login is unknown to the table, so the same wrong-looking number says nothing about
# it and the body passes. The rule claims only what it can check offline: for a login it
# names, the id must be that login's.
printf 'Co-authored-by: Stranger <%s+Stranger''@users.noreply.github.com>\n' "$wrong_id" \
  > "$TEST_ROOT/unknown-login.md"
run_check "$CHECK" "$TEST_ROOT/unknown-login.md"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected an unknown login to pass whatever its id, got $CHECK_STATUS" >&2
  echo "$CHECK_COMBINED" >&2
  exit 1
fi

# The bot login carries a bracket and `email_pattern` has none in its local-part class, so
# this address is never extracted by the address scan at all. The rule has its own pass over
# the body for that reason; without it the second table entry would be decorative here.
bracket_login="github-""actions[bot]"
printf 'Co-authored-by: %s <%s+%s''@users.noreply.github.com>\n' \
  "$bracket_login" "$wrong_id" "$bracket_login" > "$TEST_ROOT/bracket.md"
run_check "$CHECK" "$TEST_ROOT/bracket.md"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a bracketed login with another account's id to exit 1, got $CHECK_STATUS" >&2
  echo "$CHECK_COMBINED" >&2
  exit 1
fi
grep -Fqx 'body privacy: line 1 contains a misattributed GitHub account id' <<< "$CHECK_STDOUT"
assert_absent "$wrong_id" "the rejected account id"

# ---------------------------------------------------------------------- calling it wrong
# The body arrives on stdin and never in argv, so an argument is a mistake rather than an
# alternative spelling — and 2 rather than 1, because "the check could not run" and "the
# body is dirty" are answers the workflow acts on differently.
usage_status=0
bash "$CHECK" some-argument < "$TEST_ROOT/clean.md" >/dev/null 2>&1 || usage_status=$?
if [ "$usage_status" -ne 2 ]; then
  echo "expected a usage error to exit 2, got $usage_status" >&2
  exit 1
fi

# ----------------------------------------------------------------- the mutant control
# Every assertion above says a violating body is refused. None of them says the refusal
# came from the pattern rather than from something incidental, and a test that passes for
# the wrong reason is the failure mode this whole file exists to prevent elsewhere. Neuter
# one pattern and the finding it owns must disappear while the other two stay — which is
# the same fixture, run in the direction that fails.
sed "s|^private_network_pattern=.*|private_network_pattern='zz-this-pattern-matches-nothing-zz'|" \
  "$CHECK" > "$TEST_ROOT/mutant.sh"
if ! grep -Fq 'zz-this-pattern-matches-nothing-zz' "$TEST_ROOT/mutant.sh"; then
  echo "mutant control failure: the private-network pattern was not replaced" >&2
  exit 1
fi

run_check "$TEST_ROOT/mutant.sh" "$TEST_ROOT/triple.md"
if grep -Fq 'private-network address' <<< "$CHECK_STDOUT"; then
  echo "mutant control failure: the neutered pattern still reported an address" >&2
  exit 1
fi
grep -Fqx 'body privacy: line 2 contains a workstation-specific path' <<< "$CHECK_STDOUT"
grep -Fqx 'body privacy: line 4 contains a non-public email address' <<< "$CHECK_STDOUT"

# --------------------------------------------------------------------- the workflow half
python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
workflow_path = root / ".github/workflows/body-privacy.yml"
wf = workflow_path.read_text()

# ── The pull_request_target hazard ─────────────────────────────────────────────────────
# A writable token against the base repository plus anything taken from the branch under
# review is how a public repository is taken over. This workflow needs no checkout of the
# head at all — the body comes from the API — so the rule is absolute rather than
# conditional, and absolute rules are the ones a file can be held to.
assert "pull_request_target:" in wf, "the pull-request half must use pull_request_target"
for forbidden in (
    "head.sha",
    "head.ref",
    "head.repo",
    "head_sha",
    "head_ref",
    "refs/pull/",
    "merge_commit_sha",
):
    assert forbidden not in wf, (
        f"body-privacy.yml references {forbidden!r}. A pull_request_target workflow holds a "
        "writable token against the base repository; anything it takes from the branch "
        "under review is an arbitrary-code path into that token."
    )

# ── Nothing untrusted reaches an expression ────────────────────────────────────────────
# Two failures in one. `${{ ... body ... }}` in a `run:` script is shell injection from a
# field any stranger can write; in an `env:` block it is a log leak, because Actions
# renders env values into the run log — republishing the entire body, value included, to
# everyone who can read an Actions run. That is the exact harm this check reports on.
expressions = re.findall(r"\$\{\{(.*?)\}\}", wf, re.DOTALL)
assert expressions, "no workflow expressions found — the file shape changed"
for expression in expressions:
    for token in ("body", "title", "head", "login", "ref_name"):
        assert token not in expression, (
            f"body-privacy.yml interpolates {token!r} in `${{{{{expression.strip()}}}}}`. "
            "Attacker-controlled text must not reach a script or an env value; fetch it "
            "over the API instead."
        )

# The PAT is a strictly larger blast radius under pull_request_target and buys nothing:
# labels and comments are within GITHUB_TOKEN's reach, and GITHUB_TOKEN's own events
# cannot trigger another workflow, so the comment this job posts starts no loop.
assert "GH_PIPELINE_TOKEN" not in wf, (
    "body-privacy.yml must run on GITHUB_TOKEN, not the pipeline PAT"
)

# ── The fetched body is never printed ──────────────────────────────────────────────────
# The one copy of the body on the runner lives in a file. A single `cat` of it would put
# the whole body in a public log; the file is written, read once and removed.
printers = re.compile(r"\b(echo|printf|cat|tee|sed|awk|grep)\b")
sinks = ("GITHUB_OUTPUT", "GITHUB_ENV", "GITHUB_STEP_SUMMARY")
for number, line in enumerate(wf.splitlines(), start=1):
    if "body.txt" not in line and "BODY_FILE" not in line:
        continue
    assert not printers.search(line), (
        f"body-privacy.yml:{number} passes the fetched body to a command that prints it"
    )
    for sink in sinks:
        assert sink not in line, (
            f"body-privacy.yml:{number} writes the fetched body into {sink}"
        )
assert "set -x" not in wf, "tracing would print the fetched body into the run log"

# ── The shape the rest of the design rests on ──────────────────────────────────────────
assert "bash scripts/check-body-privacy.sh" in wf, (
    "the workflow must run the tested check rather than restating its patterns"
)
assert re.search(
    r"^on:\n"
    r"  issues:\n    types: \[opened, edited\]\n"
    r"  pull_request_target:\n    types: \[opened, edited\]\n",
    wf,
    re.MULTILINE,
), "both surfaces must be read on both opened and edited"

# A cancelled job resolves to CANCELLED, which pr-status-json counts among its FAILING
# conclusions and which only a manual re-run clears. Nothing here is worth owning that
# shape for; the superseded run queues and converges on the current body.
assert re.search(r"^  cancel-in-progress: false\s*$", wf, re.MULTILINE), (
    "body-privacy.yml must not cancel a superseded run"
)

# The two claims the acceptance criteria turn on, kept where a reader meets them. Neither
# is enforceable from outside the file, so the file has to say them.
assert "cannot prevent a leak" in wf, (
    "the workflow must state that it runs after the fact — a green run is not evidence "
    "that nothing was published"
)

print(
    f"body privacy — the check refuses each category without repeating it; "
    f"{workflow_path.name} takes nothing from the branch under review"
)
PY

# ------------------------------------------------------------------- the reporting half
# The run block executed VERBATIM — extracted from the YAML, never copied — against a
# stubbed `gh`, the same idiom and the same dependency-free extractor as
# pr-labeler-step.test.sh. A copied fixture would drift from the shipped workflow and keep
# passing while the real step broke.
#
# Three claims live only here. That a finding produces a label and a comment; that the
# comment names the category and not the value, which no static reading of the YAML can
# show; and that nothing is closed, edited or hidden — redacting text somebody else wrote
# is a bigger decision than a check should make, and the way that decision gets made by
# accident is a write nobody enumerated.

extract_run_block() {
  awk '
    !found && $0 ~ /^[[:space:]]*run: \|[[:space:]]*$/ {
      match($0, /^[[:space:]]*/); key = RLENGTH; found = 1; next
    }
    found {
      if ($0 ~ /^[[:space:]]*$/) { print ""; next }
      match($0, /^[[:space:]]*/); ind = RLENGTH
      if (ind <= key) exit
      if (!content) content = ind
      print substr($0, content + 1)
    }
  ' "$1"
}

extract_run_block "$REPO_ROOT/.github/workflows/body-privacy.yml" > "$TEST_ROOT/step.sh"
if ! grep -Fq 'bash scripts/check-body-privacy.sh' "$TEST_ROOT/step.sh"; then
  echo "the run block could not be extracted from body-privacy.yml" >&2
  exit 1
fi

cat > "$TEST_ROOT/gh-stub" <<'STUB'
#!/usr/bin/env bash
# Records every invocation and answers the four reads the step performs. Writes are
# recorded and acknowledged; the comment body is captured so the test can search it.
printf '%s\n' "$*" >> "${STUB_DIR}/calls"

for arg in "$@"; do
  case "$arg" in
    body=@*) cp "${arg#body=@}" "${STUB_DIR}/posted-comment.md" ;;
  esac
done

[ "${1:-}" = "label" ] && exit 0

if [ "${1:-}" != "api" ]; then
  echo "unexpected gh invocation: $*" >&2
  exit 64
fi

case "$*" in
  *--method*)
    exit 0
    ;;
  */labels*)
    [ -n "${STUB_LABELS_FAIL:-}" ] && exit 1
    cat "${STUB_DIR}/labels"
    ;;
  */comments*)
    cat "${STUB_DIR}/comment-ids"
    ;;
  *)
    [ -n "${STUB_BODY_FAIL:-}" ] && exit 1
    cat "${STUB_DIR}/body"
    ;;
esac
STUB

FIXTURE=""
new_fixture() {
  FIXTURE="$TEST_ROOT/fixture-$1"
  mkdir -p "$FIXTURE/bin" "$FIXTURE/runner"
  cp "$TEST_ROOT/gh-stub" "$FIXTURE/bin/gh"
  chmod +x "$FIXTURE/bin/gh"
  cp "$TEST_ROOT/step.sh" "$FIXTURE/step.sh"
  : > "$FIXTURE/calls"
  : > "$FIXTURE/labels"
  : > "$FIXTURE/comment-ids"
  cp "$2" "$FIXTURE/body"
}

run_block() {
  # `bash -e` is the shell GitHub Actions uses (`/usr/bin/bash -e {0}`), and reproducing
  # that flag is most of the point: the fail-closed branches below only end the step
  # because of it.
  BLOCK_OUT=$(cd "$REPO_ROOT" && PATH="$FIXTURE/bin:$PATH" \
    STUB_DIR="$FIXTURE" \
    STUB_LABELS_FAIL="${STUB_LABELS_FAIL:-}" \
    STUB_BODY_FAIL="${STUB_BODY_FAIL:-}" \
    RUNNER_TEMP="$FIXTURE/runner" \
    GH_TOKEN=stub \
    GITHUB_REPOSITORY="example/repository" \
    NUMBER=7 \
    MARKER='<!-- body-privacy -->' \
    LABEL=needs-privacy-review \
    bash -e "$FIXTURE/step.sh" 2>&1) && BLOCK_EXIT=0 || BLOCK_EXIT=$?
}

calls_have() {
  if ! grep -Eq -- "$1" "$FIXTURE/calls"; then
    echo "expected a call matching '$1'; the step made:" >&2
    cat "$FIXTURE/calls" >&2
    exit 1
  fi
}

calls_lack() {
  if grep -Eq -- "$1" "$FIXTURE/calls"; then
    echo "the step made a call matching '$1', which it must not:" >&2
    cat "$FIXTURE/calls" >&2
    exit 1
  fi
}

# Nothing is closed, edited or hidden. The issue or pull request itself is only ever READ:
# every write goes to its labels or to one comment. `/issues/7` with nothing after it is
# the resource that carries `state`, `title` and `body`.
assert_reports_only() {
  calls_lack '--method (PATCH|PUT|POST|DELETE) repos/example/repository/issues/7( |$)'
  calls_lack '(state=|state_reason=|graphql|minimizeComment|issue (close|edit))'
}

# ── a finding on a body nothing has reported on yet ────────────────────────────────────
new_fixture "new-finding" "$TEST_ROOT/triple.md"
run_block
if [ "$BLOCK_EXIT" -ne 0 ]; then
  echo "expected a reported finding to leave the job green, got $BLOCK_EXIT" >&2
  echo "$BLOCK_OUT" >&2
  exit 1
fi
calls_have '^label create needs-privacy-review'
calls_have '--method POST repos/example/repository/issues/7/labels'
calls_have '--method POST repos/example/repository/issues/7/comments'
calls_lack '--method PATCH repos/example/repository/issues/comments'
assert_reports_only

comment=$(cat "$FIXTURE/posted-comment.md")
grep -Fq 'line 2 contains a workstation-specific path' <<< "$comment"
grep -Fq 'line 3 contains a private-network address' <<< "$comment"
grep -Fq 'line 4 contains a non-public email address' <<< "$comment"
# The comment has to carry the caveat too. A reader who meets this report and takes it as
# "the leak was stopped" has the wrong model of what it can do, and the comment is where
# most readers will meet it.
grep -Fq 'shorten how long nobody knew' <<< "$comment"
CHECK_COMBINED="${comment}"$'\n'"${BLOCK_OUT}"
assert_absent "$internal_path" "the rejected path"
assert_absent "$private_ip" "the rejected address"
assert_absent "$unsafe_email" "the rejected email"

# ── a body with more findings than a comment can hold ──────────────────────────────────
# A pasted build log carries a workstation path on every line. Rendering all of them would
# push the comment past GitHub's 65,536-character limit, the POST would be refused, and a
# real finding would become a red run with no report — the one outcome worse than a noisy
# comment. The elided count is a total, which is what the other two checks already print.
for n in $(seq 1 60); do
  printf 'Step %s ran under %s/step-%s.\n' "$n" "$internal_path" "$n"
done > "$TEST_ROOT/many.md"

new_fixture "capped" "$TEST_ROOT/many.md"
run_block
if [ "$BLOCK_EXIT" -ne 0 ]; then
  echo "expected a body with many findings to leave the job green, got $BLOCK_EXIT" >&2
  echo "$BLOCK_OUT" >&2
  exit 1
fi
rendered=$(grep -c '^- line ' "$FIXTURE/posted-comment.md")
if [ "$rendered" -ne 50 ]; then
  echo "expected 50 rendered findings, got $rendered" >&2
  exit 1
fi
grep -Fq -- '…and 10 further finding(s)' "$FIXTURE/posted-comment.md"
CHECK_COMBINED=$(cat "$FIXTURE/posted-comment.md")
assert_absent "$internal_path" "the rejected path"

# ── the same body edited again: the standing report is updated, not duplicated ─────────
# One comment and one notification per body. A thread of stale reports is how a check
# people were meant to act on becomes one they filter out.
new_fixture "repeat-finding" "$TEST_ROOT/triple.md"
printf 'needs-privacy-review\n' > "$FIXTURE/labels"
printf '4242\n' > "$FIXTURE/comment-ids"
run_block
if [ "$BLOCK_EXIT" -ne 0 ]; then
  echo "expected a repeated finding to leave the job green, got $BLOCK_EXIT" >&2
  echo "$BLOCK_OUT" >&2
  exit 1
fi
calls_have '--method PATCH repos/example/repository/issues/comments/4242'
calls_lack '--method POST repos/example/repository/issues/7/comments'
assert_reports_only

# ── the body is fixed: the label comes off and the standing report is corrected ────────
new_fixture "cleared" "$TEST_ROOT/clean.md"
printf 'needs-privacy-review\n' > "$FIXTURE/labels"
printf '4242\n' > "$FIXTURE/comment-ids"
run_block
if [ "$BLOCK_EXIT" -ne 0 ]; then
  echo "expected a cleared body to leave the job green, got $BLOCK_EXIT" >&2
  echo "$BLOCK_OUT" >&2
  exit 1
fi
calls_have '--method DELETE repos/example/repository/issues/7/labels/needs-privacy-review'
calls_have '--method PATCH repos/example/repository/issues/comments/4242'
calls_lack '^label create'
assert_reports_only
grep -Fq 'cleared' "$FIXTURE/posted-comment.md"
grep -Fq 'before the edit' "$FIXTURE/posted-comment.md"

# ── a clean body nobody ever reported on is left entirely alone ────────────────────────
new_fixture "quiet" "$TEST_ROOT/clean.md"
run_block
if [ "$BLOCK_EXIT" -ne 0 ]; then
  echo "expected a clean body to leave the job green, got $BLOCK_EXIT" >&2
  echo "$BLOCK_OUT" >&2
  exit 1
fi
calls_lack '--method'
calls_lack '^label create'

# ── the write half fails closed too ────────────────────────────────────────────────────
# An unreadable label list is not an absent label, and a body that could not be fetched is
# not an empty one — an empty body scores clean, so failing open here is how a check that
# never ran returns a green verdict (#31, #134). Both end the run instead, and a red run
# on this workflow means exactly that: the alarm could not be raised.
new_fixture "labels-unreadable" "$TEST_ROOT/triple.md"
STUB_LABELS_FAIL=1 run_block
STUB_LABELS_FAIL=""
if [ "$BLOCK_EXIT" -eq 0 ]; then
  echo "expected an unreadable label list to end the run" >&2
  exit 1
fi
grep -Fq 'could not read the labels' <<< "$BLOCK_OUT"
calls_lack '--method'

new_fixture "body-unreadable" "$TEST_ROOT/triple.md"
STUB_BODY_FAIL=1 run_block
STUB_BODY_FAIL=""
if [ "$BLOCK_EXIT" -eq 0 ]; then
  echo "expected an unfetchable body to end the run" >&2
  exit 1
fi
calls_lack '--method'
calls_lack '^label create'

# An event carrying no number never reaches the API at all.
new_fixture "no-number" "$TEST_ROOT/clean.md"
BLOCK_OUT=$(cd "$REPO_ROOT" && PATH="$FIXTURE/bin:$PATH" STUB_DIR="$FIXTURE" \
  RUNNER_TEMP="$FIXTURE/runner" GH_TOKEN=stub GITHUB_REPOSITORY="example/repository" \
  NUMBER="" MARKER='<!-- body-privacy -->' LABEL=needs-privacy-review \
  bash -e "$FIXTURE/step.sh" 2>&1) && BLOCK_EXIT=0 || BLOCK_EXIT=$?
if [ "$BLOCK_EXIT" -eq 0 ]; then
  echo "expected an event with no number to end the run" >&2
  exit 1
fi
if [ -s "$FIXTURE/calls" ]; then
  echo "expected no API call for an event with no number, got:" >&2
  cat "$FIXTURE/calls" >&2
  exit 1
fi

echo "body privacy — clean bodies pass, each category is refused by location and category alone, and the reporting workflow labels and comments without repeating the value or touching the issue"
