#!/usr/bin/env bash
# Pin /dev-issue's diff-measurement pathspecs against the reviewer's own exclusion rule.
#
# Step 7 of the dev-issue skill measures a branch's diff and compares it to
# DEEPSEEK_MAX_DIFF_CHARS. For that number to mean anything it has to exclude exactly
# what the reviewer excludes — `is_generated_path` in .github/scripts/deepseek_review.py,
# which drops anything under a `gen/` path segment, anything whose basename carries the
# `_generated.` infix, and the two dependency lockfiles by basename.
#
# It did not. The recipe carried `':(exclude)Cargo.lock'` and `':(exclude)go.sum'`, and a
# git pathspec without `:(glob)` magic is matched from the repository root — so a bare
# `Cargo.lock` matches a top-level one and nothing else, while this repository keeps its
# lockfiles at `client/Cargo.lock` and `server/go.sum`. Neither was ever excluded. Every
# measurement of a branch touching a lockfile came back too large, and on #851 a
# 16,140-character part measured 30,000 and was split for it.
#
# **Too large is the quiet direction.** It never opens an oversized pull request, so
# nothing turns red; it splits changes that did not need splitting, and each split
# serialises work that was planned as parallel. The defect survived because a diff size
# has no expected value to compare it against — which is precisely the shape this
# repository pins with tests (`gate-tables`, `deepseek-budget`, `client-ci-budget`, the
# FULL_REVIEW_MARKER pair).
#
# The pin is behavioural, not textual. It runs the skill's own pathspec list over the
# real tracked tree and asks whether the set of files it keeps is the set the reviewer
# would read. A pathspec that is merely *spelled* differently is fine; one that admits or
# drops a file the reviewer does not is not.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

SKILL=".claude/skills/dev-issue/SKILL.md"
REVIEWER=".github/scripts/deepseek_review.py"

for f in "$SKILL" "$REVIEWER"; do
  [ -f "$f" ] || { echo "FAIL: $f is missing"; exit 1; }
done

# The pathspec list, read out of the skill rather than restated here. Restating it would
# put the enumeration where the enforcement should be, and the copy in this file would be
# the one that stayed right while the skill drifted.
mapfile -t PATHSPECS < <(
  python3 - "$SKILL" <<'PY'
import re
import sys

text = open(sys.argv[1]).read()
match = re.search(r"^REVIEWABLE=\$\(git diff .*?\| wc -m\)$", text, re.S | re.M)
if not match:
    sys.exit("FAIL: no REVIEWABLE=$(git diff ... | wc -m) recipe found in the skill")
found = re.findall(r"':\(exclude\)([^']+)'", match.group(0))
if not found:
    sys.exit("FAIL: the recipe excludes nothing at all")
print("\n".join(found))
PY
)

echo "pathspecs read from the skill: ${#PATHSPECS[@]}"

SPECS=()
for spec in "${PATHSPECS[@]}"; do
  SPECS+=(":(exclude)${spec}")
done

# What the skill's recipe keeps, over the real tree.
KEPT=$(git ls-files -- . "${SPECS[@]}" | sort)

# What the reviewer would read, from its own function rather than a paraphrase of it.
#
# The reader is written to a temp file rather than fed in as a heredoc: a heredoc *is*
# stdin, so `git ls-files | python3 - <<PY` hands the script the heredoc and leaves the
# pipe with no reader, which git reports as SIGPIPE and a shell reports as exit 141.
READER=$(mktemp)
trap 'rm -f "$READER"' EXIT
cat > "$READER" <<'PYEOF'
import importlib.util
import sys
from pathlib import Path

# The reviewer imports PyGithub and the OpenAI SDK at module scope, so it cannot be
# loaded bare. `test_deepseek_review.py` already stubs both and loads it, and its own
# comment says a missing stub takes the whole suite down silently — so this test goes
# through that loader rather than keeping a second copy of the stub list, which would
# rot out of step with the imports it stands in for.
harness_path = Path(sys.argv[1]).with_name("test_deepseek_review.py")
spec = importlib.util.spec_from_file_location("test_deepseek_review", harness_path)
harness = importlib.util.module_from_spec(spec)
try:
    spec.loader.exec_module(harness)
except Exception as exc:  # pragma: no cover - reported, never swallowed
    sys.exit(f"FAIL: cannot load the reviewer to read its rule: {exc}")

module = getattr(harness, "deepseek_review", None)
if module is None:
    sys.exit("FAIL: the reviewer test harness no longer exposes the module it loads")
is_generated = getattr(module, "is_generated_path", None)
if is_generated is None:
    sys.exit("FAIL: deepseek_review.is_generated_path is gone; this pin needs rewriting")

for line in sys.stdin.read().splitlines():
    if line and not is_generated(line):
        print(line)
PYEOF

WOULD_READ=$(git ls-files | python3 "$READER" "$REVIEWER")
WOULD_READ=$(printf '%s\n' "$WOULD_READ" | sort)

if [ "$KEPT" != "$WOULD_READ" ]; then
  echo "FAIL: the skill's measurement does not read what the reviewer reads."
  echo "Measured but NOT reviewed (inflates the number, splits changes needlessly):"
  comm -23 <(printf '%s\n' "$KEPT") <(printf '%s\n' "$WOULD_READ") | sed 's/^/  + /'
  echo "Reviewed but NOT measured (deflates the number, lets an oversized PR through):"
  comm -13 <(printf '%s\n' "$KEPT") <(printf '%s\n' "$WOULD_READ") | sed 's/^/  - /'
  exit 1
fi

echo "PASS: the skill's pathspecs keep exactly the $(printf '%s\n' "$KEPT" | grep -c . ) files the reviewer reads"

# The negative control. A pin that cannot fail is not one, and this is the exact defect
# that motivated the file: the bare, root-anchored spelling of the lockfile exclusions.
# If the tree ever moves both lockfiles to the root, this control stops discriminating —
# so it asserts that it actually caught something rather than assuming it did.
BROKEN=$(git ls-files -- . ':(exclude)*/gen/*' ':(exclude)*_generated.*' ':(exclude)Cargo.lock' ':(exclude)go.sum' | sort)
if [ "$BROKEN" = "$WOULD_READ" ]; then
  echo "FAIL: the negative control no longer distinguishes anything — the root-anchored"
  echo "      lockfile pathspecs now give the right answer, so this test would pass over"
  echo "      the very defect it exists to catch. Rewrite the control."
  exit 1
fi

echo "PASS: the negative control still fails as it should ($(comm -23 <(printf '%s\n' "$BROKEN") <(printf '%s\n' "$WOULD_READ") | grep -c .) file(s) it would wrongly measure)"
