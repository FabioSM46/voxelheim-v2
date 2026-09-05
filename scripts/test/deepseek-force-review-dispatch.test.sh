#!/usr/bin/env bash
#
# pr-deepseek-force-review — the measure-only dispatch, and the cap override it carries.
#
# AGENTS.md names `measure_only: true` as the way to sample the diff cap, and the helper
# could not send it: it passed `pr_number` and `event_name` and nothing else, so every
# "measurement" a reader of that paragraph dispatched through the designated helper was
# a forced review that posted (#925). Worse, the cap truncates *before* the API call, so
# a replay of a 65,000-character pull request measured 45,000 whichever way it was
# dispatched — the upper band could not be sampled at all.
#
# Pinned here:
#
#   * the default dispatch is unchanged — no measure input reaches the workflow;
#   * `--measure-only` sends `measure_only=true`;
#   * `--measure-cap N` is sent only beside `--measure-only`, and without it the helper
#     refuses before `gh workflow run` is reached — a raised cap must never reach a review
#     that posts, and the script refuses the same shape a second time;
#   * the workflow declares both inputs and hands the cap to the script as
#     DEEPSEEK_MEASURE_CAP, so a dispatch on a ref carrying this definition measures what
#     it says it measures.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/gh-automation.sh"
WORKFLOW="$ROOT/.github/workflows/deepseek-pr-review.yml"

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

STUB_DIR="$(mktemp -d)"
CALL_LOG="$(mktemp)"
trap 'rm -rf "$STUB_DIR" "$CALL_LOG"' EXIT

cat > "$STUB_DIR/gh" <<'STUB'
#!/usr/bin/env bash
echo "gh $*" >> "$CALL_LOG"
case "$1 ${2:-}" in
  "auth status") exit 0 ;;
  "repo view") printf '%s\n' 'owner/repo'; exit 0 ;;
  "workflow run") exit 0 ;;
esac
exit 0
STUB
chmod +x "$STUB_DIR/gh"
export CALL_LOG

run_helper() {
  : > "$CALL_LOG"
  PATH="$STUB_DIR:$PATH" bash "$SCRIPT" pr-deepseek-force-review "$@" 2>&1
}

echo "the default dispatch is the forced review it always was"
out=$(run_helper 925)
status=$?
calls=$(<"$CALL_LOG")
assert_eq "exit 0" 0 "$status"
assert_contains "dispatches the workflow" "$calls" "gh workflow run deepseek-pr-review.yml"
assert_contains "on develop by default" "$calls" "--ref develop"
assert_contains "names the PR" "$calls" "pr_number=925"
assert_not_contains "sends no measure_only input" "$calls" "measure_only"
assert_not_contains "sends no measure_cap input" "$calls" "measure_cap"
assert_contains "says it forced a review" "$out" "forced DeepSeek review of PR #925"

echo
echo "--measure-only is sent as the workflow input"
out=$(run_helper 925 feature/some-branch --measure-only)
status=$?
calls=$(<"$CALL_LOG")
assert_eq "exit 0" 0 "$status"
assert_contains "honours the ref" "$calls" "--ref feature/some-branch"
assert_contains "sends measure_only=true" "$calls" "measure_only=true"
assert_not_contains "sends no cap when none was asked for" "$calls" "measure_cap"
assert_contains "says nothing will be posted" "$out" "nothing will be posted"

echo
echo "--measure-cap rides only beside --measure-only"
out=$(run_helper 925 --measure-only --measure-cap 70000)
status=$?
calls=$(<"$CALL_LOG")
assert_eq "exit 0" 0 "$status"
assert_contains "sends measure_only=true" "$calls" "measure_only=true"
assert_contains "sends the cap" "$calls" "measure_cap=70000"
assert_contains "reports the cap" "$out" "cap 70000"

out=$(run_helper 925 --measure-cap 70000)
status=$?
calls=$(<"$CALL_LOG")
assert_eq "a cap without --measure-only is refused" 1 "$status"
assert_contains "the refusal says why" "$out" "only valid with --measure-only"
assert_not_contains "and no dispatch is attempted" "$calls" "workflow run"

out=$(run_helper 925 --measure-only --measure-cap lots)
status=$?
calls=$(<"$CALL_LOG")
assert_eq "a non-numeric cap is refused" 1 "$status"
assert_not_contains "and no dispatch is attempted" "$calls" "workflow run"

out=$(run_helper 925 --bogus)
status=$?
assert_eq "an unknown option is refused" 1 "$status"

out=$(run_helper)
status=$?
assert_eq "a missing PR number is refused" 1 "$status"

echo
echo "the workflow declares what the helper sends"
workflow=$(<"$WORKFLOW")
assert_contains "declares the measure_only input" "$workflow" "      measure_only:"
assert_contains "declares the measure_cap input" "$workflow" "      measure_cap:"
assert_contains "hands the cap to the script under the name it reads" "$workflow" \
  "DEEPSEEK_MEASURE_CAP: \${{ github.event_name == 'workflow_dispatch' && inputs.measure_cap"
reviewer=$(<"$ROOT/.github/scripts/deepseek_review.py")
assert_contains "the script reads that name" "$reviewer" 'os.environ.get("DEEPSEEK_MEASURE_CAP"'

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
