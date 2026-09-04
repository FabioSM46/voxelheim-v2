#!/usr/bin/env bash
#
# pr-merge — the base guard, and what it does NOT claim.
#
# #217 authorized develop merges; the owner later authorized stacked non-main feature-base
# merges too, while leaving `main` human-only. A deny rule
# in `.claude/settings.json` cannot express that split: `gh pr merge <n>` names no branch,
# because the base is a property of the pull request. Removing the merge deny for one
# target therefore removed it for both, and the DeepSeek review on #218 asked for the
# machine-checkable check that a deny list cannot provide.
#
# What is pinned here is deliberately narrow:
#
#   * a PR based on `main` is refused, and the refusal names the base;
#   * a base that cannot be read is refused too — fail closed, the same rule every count
#     in `cmd_pr_status_json` follows, because an unreadable base is not evidence of
#     a permitted non-main base;
#   * in both refusals **no merge is attempted**, which is the only claim the guard
#     actually makes and the one a call log can prove;
#   * PRs targeting `develop` or a feature branch merge, with `--squash` as the default.
#
# What is NOT pinned, because it is not true: that an agent cannot merge `main`. `gh pr
# merge` and `gh api -X PUT …/merge` bypass this function entirely. The guard stops an
# accident on the designated path, exactly as the `git push origin main` deny entries do,
# and AGENTS.md says so in those words.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/gh-automation.sh"

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

# ── The `gh` stub ────────────────────────────────────────────────────────────
#
# Two knobs: what `gh pr view --json baseRefName` answers, and whether it can answer at
# all. Every invocation is logged so a test can assert that `gh pr merge` was never
# reached — which is the entire claim of both refusal paths.

STUB_DIR="$(mktemp -d)"
CALL_LOG="$(mktemp)"
trap 'rm -rf "$STUB_DIR" "$CALL_LOG"' EXIT

cat > "$STUB_DIR/gh" <<'STUB'
#!/usr/bin/env bash
echo "gh $*" >> "$CALL_LOG"
case "$1 ${2:-}" in
  "auth status")
    exit 0
    ;;
  "pr view")
    if [ "${GH_BASE_READ_OK:-1}" != "1" ]; then
      echo "could not resolve to a PullRequest" >&2
      exit 1
    fi
    # `-` and not `:-`: an empty answer is one of the cases under test, and `:-` would
    # quietly substitute `develop` for it — the stub deciding the verdict instead of the
    # guard. Only an *unset* GH_BASE means "the ordinary develop PR".
    printf '%s\n' "${GH_BASE-develop}"
    exit 0
    ;;
  "repo view")
    printf '%s\n' 'owner/repo'
    exit 0
    ;;
  "api repos/owner/repo/pulls/218")
    printf '%s\n' "${GH_BASE_HEAD-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}"
    exit 0
    ;;
  "pr merge")
    if [ "${GH_HEAD_MATCH_OK:-1}" != "1" ] && [[ " $* " == *" --match-head-commit "* ]]; then
      echo "head branch was modified" >&2
      exit 1
    fi
    if [ "${GH_MERGE_OK:-1}" != "1" ]; then
      echo "Pull request is not mergeable" >&2
      exit 1
    fi
    echo "Merged"
    exit 0
    ;;
esac
exit 0
STUB
chmod +x "$STUB_DIR/gh"
export CALL_LOG
PATH="$STUB_DIR:$PATH"
export PATH

run_merge() {
  : > "$CALL_LOG"
  OUT=$(bash "$SCRIPT" pr-merge "$@" 2>&1)
  RC=$?
  LOG=$(cat "$CALL_LOG")
}

echo
echo "pr-merge — a PR based on main is refused"
GH_BASE=main run_merge 218
assert_nonzero "merging into main exits non-zero" "$RC"
assert_contains "the refusal names the base" "$OUT" "targets 'main'"
assert_contains "the refusal says who may do it" "$OUT" "human-only"
assert_not_contains "no merge is attempted" "$LOG" "gh pr merge"

echo
echo "pr-merge — an unreadable base fails closed"
GH_BASE_READ_OK=0 run_merge 218
assert_nonzero "an unreadable base exits non-zero" "$RC"
assert_contains "the refusal says the base could not be read" "$OUT" "could not read the base branch"
assert_contains "it carries gh's reason" "$OUT" "could not resolve to a PullRequest"
assert_not_contains "no merge is attempted on an unreadable base" "$LOG" "gh pr merge"

echo
echo "pr-merge — an empty base is not 'develop' either"
GH_BASE="" run_merge 218
assert_nonzero "an empty base exits non-zero" "$RC"
assert_contains "the refusal names the emptiness" "$OUT" "empty base branch"
assert_not_contains "no merge is attempted on an empty base" "$LOG" "gh pr merge"

echo
echo "pr-merge — a develop PR merges, squash by default"
GH_BASE=develop run_merge 218
assert_eq "merging into develop exits 0" "0" "$RC"
assert_contains "it reports the base it merged into" "$OUT" "merged into develop"
assert_contains "the default method is squash" "$LOG" "gh pr merge 218 --squash"

echo
echo "pr-merge — a feature-base PR also merges"
GH_BASE=feature/parent-branch run_merge 218
assert_eq "merging into a feature branch exits 0" "0" "$RC"
assert_contains "it reports the feature base" "$OUT" "merged into feature/parent-branch"
assert_contains "the feature merge is squash by default" "$LOG" "gh pr merge 218 --squash"

echo
echo "pr-merge — an observed head can be bound to the merge"
HEAD_SHA=0123456789abcdef0123456789abcdef01234567
GH_BASE=feature/parent-branch run_merge 218 --head "$HEAD_SHA"
assert_eq "a matching expected head merges" "0" "$RC"
assert_contains "gh receives the expected head" "$LOG" "--match-head-commit $HEAD_SHA"

GH_BASE=feature/parent-branch GH_HEAD_MATCH_OK=0 run_merge 218 --head "$HEAD_SHA"
assert_nonzero "a moved head refuses the merge" "$RC"
assert_contains "the moved-head failure is visible" "$OUT" "head branch was modified"

GH_BASE=feature/parent-branch run_merge 218 --head not-a-sha
assert_nonzero "an invalid expected head is refused" "$RC"
assert_contains "the invalid SHA is explained" "$OUT" "invalid expected head SHA"
assert_not_contains "an invalid SHA attempts no merge" "$LOG" "gh pr merge"

echo
echo "pr-merge — an observed base head can be bound to the readiness read"
BASE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
GH_BASE=feature/parent-branch GH_BASE_HEAD="$BASE_SHA" run_merge 218 --base-head "$BASE_SHA"
assert_eq "a matching expected base head merges" "0" "$RC"
assert_contains "the base SHA comes from the stable REST shape" "$LOG" "gh api repos/owner/repo/pulls/218 --jq .base.sha"
assert_not_contains "the guard does not request an unsupported gh field" "$LOG" "baseRefOid"

GH_BASE=feature/parent-branch GH_BASE_HEAD=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb run_merge 218 --base-head "$BASE_SHA"
assert_nonzero "a moved base refuses the merge" "$RC"
assert_contains "the moved-base failure requests fresh readiness" "$OUT" "repeat readiness checks"
assert_not_contains "a moved base attempts no merge" "$LOG" "gh pr merge"

GH_BASE=feature/parent-branch run_merge 218 --base-head not-a-sha
assert_nonzero "an invalid expected base SHA is refused" "$RC"
assert_contains "the invalid base SHA is explained" "$OUT" "invalid expected base head SHA"
assert_not_contains "an invalid base SHA attempts no merge" "$LOG" "gh pr merge"

echo
echo "pr-merge — the method is explicit and validated"
GH_BASE=develop run_merge 218 --merge
assert_eq "an explicit --merge is accepted" "0" "$RC"
assert_contains "and is what gh receives" "$LOG" "gh pr merge 218 --merge"

GH_BASE=develop run_merge 218 --yolo
assert_nonzero "an unknown method exits non-zero" "$RC"
assert_contains "and says which methods exist" "$OUT" "expected --head <sha>, --base-head <sha>, --squash, --merge or --rebase"
assert_not_contains "an unknown method attempts no merge" "$LOG" "gh pr merge"

echo
echo "pr-merge — a failed merge is loud, never a silent success"
GH_BASE=develop GH_MERGE_OK=0 run_merge 218
assert_nonzero "a failed merge exits non-zero" "$RC"
assert_contains "it names the PR and the base" "$OUT" "merge of PR #218 into 'develop' failed"
assert_contains "it carries gh's reason" "$OUT" "not mergeable"
assert_not_contains "it prints no success line" "$OUT" "merged into develop ("

echo
echo "pr-merge — argument handling"
run_merge
assert_nonzero "no PR number exits non-zero" "$RC"
assert_contains "and prints usage" "$OUT" "usage: pr-merge"

echo
echo "the guard does not claim to be a sandbox"
DOC=$(cat "$ROOT/AGENTS.md")
assert_contains "AGENTS.md still says gh api reaches a merge another way" "$DOC" \
  'gh api -X PUT repos/OWNER/REPO/pulls/N/merge'
assert_contains "AGENTS.md still says the allowlist is not a sandbox" "$DOC" \
  "The allowlist is not a sandbox"

echo
echo "── ${pass} passed, ${fail} failed ──"
[ "$fail" -eq 0 ] || exit 1
