#!/usr/bin/env bash
# =============================================================================
# Regression tests for scripts/changed-areas.sh — the classifier behind ci.yml's
# `detect` job.
#
# These flags decide which workload jobs ci-gate permits to report SKIPPED. A
# wrong `false` is therefore a silently untested PR — so every rule is pinned
# here, and the fail-open behaviours (unrecognised paths, empty input) are
# pinned hardest: they are what stands between a classifier bug and a green
# label on unverified code.
#
# Each assertion compares the classifier's ENTIRE stdout, not one flag. The
# detect job appends that stdout verbatim to $GITHUB_OUTPUT, so "all three keys,
# once each, in stable order" is as much a part of the contract as the values —
# a missing key would read as empty in `if:` expressions, which the workflow's
# `!= 'false'` polarity turns into "run the job" (fail open, by design; see
# ci.yml).
#
# Run: bash scripts/test/changed-areas.test.sh
# =============================================================================

set -uo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/changed-areas.sh"

pass=0
fail=0

# expect <name> <server> <client> <schemas> <<'EOF'
#   one changed path per line
# EOF
expect() {
  local name="$1" out expected
  expected="server=$2
client=$3
schemas=$4"
  out=$(bash "$SCRIPT" 2>/dev/null)
  if [ "$out" = "$expected" ]; then
    echo "  ok   — ${name}"
    pass=$((pass + 1))
  else
    echo "  FAIL — ${name}"
    echo "         expected: $(echo "$expected" | tr '\n' ' ')"
    echo "         got:      $(echo "$out" | tr '\n' ' ')"
    fail=$((fail + 1))
  fi
}

echo "changed-areas — one workspace runs its own job"

expect "a server file runs the server job only" \
  true false false <<'EOF'
server/internal/world/chunk.go
EOF

expect "a client file runs the client job only" \
  false true false <<'EOF'
client/src/render/greedy_mesh.rs
EOF

echo
echo "changed-areas — the contract fans out to both consumers"

expect "a schema change runs schemas AND both sides of the contract" \
  true true true <<'EOF'
schemas/world.fbs
EOF

expect ".flatc-version is a contract change (it pins the code generator)" \
  true true true <<'EOF'
.flatc-version
EOF

echo
echo "changed-areas — markdown is inert only OUTSIDE the workspaces"

expect "root AGENTS.md runs nothing" \
  false false false <<'EOF'
AGENTS.md
EOF

expect "docs / agent skills / README bundle runs nothing" \
  false false false <<'EOF'
docs/WORKFLOW.md
.claude/skills/dev-issue/SKILL.md
.agents/skills/dev-issue/SKILL.md
.opencode/skills/dev-issue/SKILL.md
README.md
EOF

expect "markdown inside a workspace still runs that workspace" \
  true false false <<'EOF'
server/docs/NETCODE.md
EOF

echo
echo "changed-areas — automation helpers are the automation job's problem, not a workspace's"

# The `helpers` selector is computed in the workflow from the raw path list —
# never by this script — so the classifier cannot exempt itself from its own
# tests. Here these paths must simply not trigger any workspace build.
expect "gh-automation.sh and its tests trigger no workspace job" \
  false false false <<'EOF'
scripts/gh-automation.sh
scripts/test/pr-status-frozen-rule.test.sh
EOF

expect "deepseek_review.py triggers no workspace job" \
  false false false <<'EOF'
.github/scripts/deepseek_review.py
EOF

echo
echo "changed-areas — global paths run everything"

expect "ci.yml runs everything (the jobs' own meaning changed)" \
  true true true <<'EOF'
.github/workflows/ci.yml
EOF

expect "a workflow that is not ci.yml runs nothing" \
  false false false <<'EOF'
.github/workflows/pr-labeler.yml
EOF

expect ".gitignore runs nothing" \
  false false false <<'EOF'
.gitignore
EOF

echo
echo "changed-areas — fail open"

expect "an unrecognised path runs everything" \
  true true true <<'EOF'
tools/new-top-level-thing.sh
EOF

expect "empty input runs everything" \
  true true true <<'EOF'
EOF

expect "one unrecognised path among inert ones still runs everything" \
  true true true <<'EOF'
AGENTS.md
tools/new-top-level-thing.sh
EOF

echo
echo "changed-areas — mixes and renames"

expect "server + root docs runs only the server side" \
  true false false <<'EOF'
server/internal/world/gen.go
AGENTS.md
EOF

# The detect job feeds BOTH sides of a rename. A file moved out of a workspace
# must rebuild the workspace it left — its imports just broke.
expect "a cross-workspace rename runs both workspaces" \
  true true false <<'EOF'
client/src/net/moved.rs
server/internal/net/moved.rs
EOF

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
