#!/usr/bin/env bash
# =============================================================================
# graphify-clean.sh — strip graphify's go_pkg_* dangling-edge artifact from an
# extraction before the graph health diagnostic runs.
#
# graphify's AST extractor emits `imports_from` edges from Go files to package
# nodes (`go_pkg_math`, `go_pkg_testing`, `go_pkg_.../internal/world`) that it
# never emits as nodes, so `diagnose_extraction` reports ~1,600 dangling-endpoint
# edges on this repository and warns the graph "may be incomplete/corrupt". The
# built graph.json is unaffected — `build_from_json` silently drops unresolvable
# edges — so the warning is a diagnostic artifact, not graph corruption. But it
# masks a genuinely dangling edge from an LLM id mismatch, which is exactly what
# the diagnostic exists to surface (see issue #737).
#
# This wrapper drops the artifact edges in place and re-runs the health
# diagnostic on the cleaned extraction, so what it reports afterwards is real.
# The extraction sidecar is a transient build artifact — graphify's own Step 9
# deletes it — so editing it in place is safe.
#
# Usage:
#   bash scripts/graphify-clean.sh [extraction.json] [scan-root]
#
#   extraction.json   default: <repo>/graphify-out/.graphify_extract.json
#   scan-root         the base the extraction's source_file paths are relative
#                     to; default: <repo>. Only used for the health diagnostic.
#
# Exit status: 0 when the filter ran (or there was nothing to clean). The health
# diagnostic is best-effort: it runs only when graphify is importable, and a
# "Graph health: OK" verdict under it means the remaining extraction is clean.
# =============================================================================

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXTRACT="${1:-${REPO_ROOT}/graphify-out/.graphify_extract.json}"
SCAN_ROOT="${2:-${REPO_ROOT}}"

if [ ! -f "$EXTRACT" ]; then
  echo "graphify-clean: no extraction at ${EXTRACT} — nothing to clean."
  exit 0
fi

# The filter is pure stdlib JSON, so it runs on any host with python3 and needs
# no graphify install. Kept as one heredoc so the shell renders no encoding
# drift (same reason .graphify_detect.json is written from Python).
python3 - "$EXTRACT" <<'PYEOF'
import json, sys

path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    data = json.load(f)

edges = data.get("edges", [])
before = len(edges)
kept = [
    e for e in edges
    if not (
        e.get("source", "").startswith("go_pkg_")
        or e.get("target", "").startswith("go_pkg_")
    )
]
data["edges"] = kept

with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, ensure_ascii=False, indent=2)

print(f"graphify-clean: dropped {before - len(kept)} go_pkg_* edges "
      f"({before} -> {len(kept)})")
PYEOF

# Best-effort health diagnostic on the cleaned extraction. graphify is a local
# tool; the interpreter that produced the extraction is recorded in
# graphify-out/.graphify_python when the pipeline ran.
GFY_PYTHON=""
_GFY_FILE="$(dirname "$EXTRACT")/.graphify_python"
if [ -f "$_GFY_FILE" ]; then
  _FROM_FILE=$(tr -d '[:space:]' < "$_GFY_FILE")
  case "$_FROM_FILE" in
    *[!a-zA-Z0-9/_.@:\\-]*) ;;
    *) if [ -n "$_FROM_FILE" ] && [ -x "$_FROM_FILE" ]; then
         GFY_PYTHON="$_FROM_FILE"
       fi ;;
  esac
fi
if [ -z "$GFY_PYTHON" ] && command -v python3 >/dev/null 2>&1; then
  GFY_PYTHON="python3"
fi
if [ -z "$GFY_PYTHON" ] || ! "$GFY_PYTHON" -c "import graphify" >/dev/null 2>&1; then
  echo "graphify-clean: graphify not importable — filter applied, health diagnostic skipped."
  exit 0
fi

"$GFY_PYTHON" - "$EXTRACT" "$SCAN_ROOT" <<'PYEOF'
import json, sys
from graphify.diagnostics import diagnose_extraction, format_diagnostic_report

extract = sys.argv[1]
root = sys.argv[2]
with open(extract, encoding="utf-8") as f:
    data = json.load(f)
summary = diagnose_extraction(data, directed=False, root=root)
print(format_diagnostic_report(summary))
flags = [f'{summary[k]} {label}' for k, label in (
    ('dangling_endpoint_edges', 'dangling-endpoint edges'),
    ('missing_endpoint_edges', 'missing-endpoint edges'),
    ('self_loop_edges', 'self-loop edges'),
) if summary.get(k, 0)]
if flags:
    print('GRAPH HEALTH WARNING: ' + '; '.join(flags))
else:
    print('Graph health: OK (no dangling/missing/collapsed edges).')
PYEOF
