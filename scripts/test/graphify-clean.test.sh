#!/usr/bin/env bash
# =============================================================================
# Regression tests for scripts/graphify-clean.sh — the wrapper that strips
# graphify's go_pkg_* dangling-edge artifact out of an extraction.
#
# The AST extractor emits `imports_from` edges to `go_pkg_*` package nodes it
# never creates (issue #737), so diagnose_extraction flags ~1,600 phantom
# dangling edges and warns the graph is corrupt when it is not. The built
# graph.json is unaffected — build_from_json drops unresolvable edges — so the
# danger is a false alarm masking a real dangling edge from an LLM id mismatch.
#
# These tests pin the filter itself (pure stdlib, runs anywhere python3 does)
# and the missing-file no-op. The health diagnostic half is best-effort by
# design and is not asserted here: it needs graphify installed, which CI's
# `automation` job does not have.
#
# Run: bash scripts/test/graphify-clean.test.sh
# =============================================================================

set -uo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/graphify-clean.sh"

pass=0
fail=0

fail_test() {
  local name="$1" detail="$2"
  echo "  FAIL — ${name}"
  echo "         ${detail}"
  fail=$((fail + 1))
}

ok() {
  local name="$1"
  echo "  ok   — ${name}"
  pass=$((pass + 1))
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ── fixture: one real edge, three go_pkg_* dangling edges ────────────────
cat > "$TMP/extract.json" <<'JSON'
{
  "nodes": [
    {"id": "server_internal_game_collide", "label": "collide.go", "file_type": "code"}
  ],
  "edges": [
    {"source": "server_internal_game_collide", "target": "go_pkg_math", "relation": "imports_from", "confidence": "EXTRACTED", "confidence_score": 1.0},
    {"source": "server_internal_game_collide", "target": "go_pkg_testing", "relation": "imports_from", "confidence": "EXTRACTED", "confidence_score": 1.0},
    {"source": "go_pkg_github_com_owner_repo_server_internal_world", "target": "server_internal_game_collide", "relation": "imports_from", "confidence": "EXTRACTED", "confidence_score": 1.0},
    {"source": "server_internal_game_collide", "target": "server_internal_game_collidebox", "relation": "references", "confidence": "EXTRACTED", "confidence_score": 1.0}
  ],
  "hyperedges": []
}
JSON

echo "graphify-clean — drops the go_pkg_* artifact edges only"

out=$(bash "$SCRIPT" "$TMP/extract.json" "$TMP" 2>&1)
status=$?
if [ $status -ne 0 ]; then
  fail_test "exit status is 0 after filtering" "got $status"
else
  ok "exit status is 0 after filtering"
fi
if [[ "$out" != *"dropped 3 go_pkg_* edges"* ]]; then
  fail_test "reports the drop count" "got: $out"
else
  ok "reports the drop count"
fi

kept=$(python3 -c '
import json, sys
data = json.load(open(sys.argv[1]))
go = [e for e in data["edges"] if e["source"].startswith("go_pkg_") or e["target"].startswith("go_pkg_")]
print(len(data["edges"]), len(go))
' "$TMP/extract.json")
if [ "$kept" != "1 0" ]; then
  fail_test "keeps the real edge and removes all go_pkg_*" "got: $kept (want: 1 edge, 0 go_pkg_)"
else
  ok "keeps the real edge and removes all go_pkg_*"
fi

echo
echo "graphify-clean — a go_pkg_* node name is left untouched"

cat > "$TMP/nonedge.json" <<'JSON'
{
  "nodes": [{"id": "go_pkg_math", "label": "math", "file_type": "code"}],
  "edges": [{"source": "go_pkg_math", "target": "server_a_b", "relation": "references", "confidence": "EXTRACTED", "confidence_score": 1.0}]
}
JSON
out=$(bash "$SCRIPT" "$TMP/nonedge.json" "$TMP" 2>&1)
if [[ "$out" != *"dropped 1 go_pkg_* edges"* ]]; then
  fail_test "an edge FROM a go_pkg_ node is dropped too" "got: $out"
else
  ok "an edge FROM a go_pkg_ node is dropped too"
fi

echo
echo "graphify-clean — missing extraction is a clean no-op"

out=$(bash "$SCRIPT" "$TMP/nonexistent.json" "$TMP" 2>&1)
status=$?
if [ $status -ne 0 ]; then
  fail_test "exit status is 0 when there is nothing to clean" "got $status"
else
  ok "exit status is 0 when there is nothing to clean"
fi
if [[ "$out" != *"nothing to clean"* ]]; then
  fail_test "says nothing to clean" "got: $out"
else
  ok "says nothing to clean"
fi

echo
echo "${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
