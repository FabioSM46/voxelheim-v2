#!/usr/bin/env bash
# =============================================================================
# check-schemas.sh — validate every FlatBuffers contract in schemas/
#
# The single source of truth for the schemas gate: CI's `schemas` job runs this
# script, and /dev-issue runs it locally before opening a PR. It answers two
# questions, and they are not the same one.
#
# **Phase 1 — can flatc generate this contract for both consumers?** Every .fbs
# file is compiled for Go and Rust into a throwaway directory, so a schema that
# parses but cannot generate code for either side fails here rather than at the
# next scaffold build.
#
# **Phase 2 — are the committed bindings still the ones this contract produces?**
# Phase 1 throws its output away, so for a long time nothing compared `gen/`
# against `schemas/`: editing a contract and forgetting to regenerate passed every
# gate in the repository. It was not hypothetical. PR #139 changed one comment in
# `RepairRequest` and CI went green with `server/gen/.../RepairRequest.go` and
# `client/src/gen/.../repair_request_generated.rs` still carrying the text the
# change removed — flatc propagates `///` documentation into both consumers, so
# the contract disagreed with its own bindings and no check could see it. A field
# or a type would have been the same silence with worse consequences.
#
# Phase 2 runs the recipe `schemas/AGENTS.md` documents, verbatim and in place,
# and then asks git whether anything moved. That file already tells a human
# "regenerating and reformatting must produce no diff — check that before
# committing"; this is that sentence, executed.
#
# **On drift the regenerated files are left in the working tree, not reverted.**
# The regeneration *is* the fix, and throwing it away to report a failure would
# make the reader do the work twice. In CI the checkout is disposable, so nothing
# is lost either way.
#
# flatc is pinned by .flatc-version at the repo root. CI installs exactly that
# release; locally a version mismatch is a warning, a missing flatc is an error.
# The formatters are part of generation, not a tidy-up — see schemas/AGENTS.md —
# so a scaffolded workspace whose formatter is missing is an error too, never a
# skipped phase. A skipped check that reports success is the failure this whole
# phase exists to end.
# =============================================================================

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCHEMAS_DIR="${REPO_ROOT}/schemas"

if [ ! -d "$SCHEMAS_DIR" ]; then
  echo "check-schemas: no schemas/ directory at this ref — nothing to validate."
  exit 0
fi

mapfile -t fbs_files < <(find "$SCHEMAS_DIR" -name '*.fbs' -type f | sort)
if [ "${#fbs_files[@]}" -eq 0 ]; then
  echo "check-schemas: schemas/ exists but holds no .fbs files — nothing to validate."
  exit 0
fi

command -v flatc >/dev/null 2>&1 || {
  echo "ERROR: flatc not found. Install the release pinned in .flatc-version:" >&2
  echo "  https://github.com/google/flatbuffers/releases" >&2
  exit 1
}

if [ -f "${REPO_ROOT}/.flatc-version" ]; then
  pinned="$(tr -d '[:space:]' < "${REPO_ROOT}/.flatc-version")"
  actual="$(flatc --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  if [ -n "$pinned" ] && [ "$actual" != "$pinned" ]; then
    echo "WARNING: local flatc ${actual:-<unknown>} != pinned ${pinned}. CI uses the pin; generated output may differ." >&2
  fi
fi

out_dir="$(mktemp -d)"
trap 'rm -rf "$out_dir"' EXIT

status=0
for fbs in "${fbs_files[@]}"; do
  rel="${fbs#"$REPO_ROOT"/}"
  # Both consumers, deliberately: --go and --rust exercise different generator
  # paths, and a contract only one side can consume is a broken contract.
  if flatc --go -o "${out_dir}/go" -I "$SCHEMAS_DIR" "$fbs" \
     && flatc --rust -o "${out_dir}/rust" -I "$SCHEMAS_DIR" "$fbs"; then
    echo "[PASS] ${rel}"
  else
    echo "[FAIL] ${rel}" >&2
    status=1
  fi
done

if [ "$status" -ne 0 ]; then
  exit "$status"
fi

# ── Phase 2 — the committed bindings must be the ones this contract produces ──

# Both consumers are optional: the workspaces are scaffolded through the pipeline
# itself, and an absent one has nothing to regenerate. Presence is read from the
# marker file, exactly as ci.yml and ci-gate.sh read it.
server_present=false; client_present=false
[ -f "${REPO_ROOT}/server/go.mod" ] && server_present=true
[ -f "${REPO_ROOT}/client/Cargo.toml" ] && client_present=true

if [ "$server_present" = false ] && [ "$client_present" = false ]; then
  echo "check-schemas: no consumer is scaffolded — no bindings to compare."
  exit 0
fi

if ! git -C "$REPO_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  echo "ERROR: not a git repository — phase 2 compares against the committed tree." >&2
  exit 1
fi

gen_paths=()
[ "$server_present" = true ] && gen_paths+=("server/gen")
[ "$client_present" = true ] && gen_paths+=("client/src/gen")

# Refuse rather than clobber. Phase 2 writes into the working tree, so an already
# dirty gen/ makes drift and a local edit indistinguishable — and gen/ is never
# hand-edited here, so a dirty one is a surprise worth stopping for.
if [ -n "$(git -C "$REPO_ROOT" status --porcelain -- "${gen_paths[@]}")" ]; then
  echo "ERROR: generated bindings have uncommitted changes:" >&2
  git -C "$REPO_ROOT" status --short -- "${gen_paths[@]}" >&2
  echo "Commit or discard them first — this phase regenerates in place and would overwrite them." >&2
  exit 1
fi

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "ERROR: $1 not found, and $2 is scaffolded. $3" >&2
    exit 1
  }
}

if [ "$server_present" = true ]; then
  require gofmt "server/" "Generation includes gofmt; see schemas/AGENTS.md."
  flatc --go --go-module-name github.com/FabioSM46/voxelheim-v2/server/gen \
    -o "${REPO_ROOT}/server/gen" -I "$SCHEMAS_DIR" "${fbs_files[@]}"
  gofmt -w "${REPO_ROOT}/server/gen"
fi

if [ "$client_present" = true ]; then
  require cargo "client/" "Generation includes cargo fmt; see schemas/AGENTS.md."
  # Two passes with the root schema last: flatc rewrites mod.rs per input file
  # instead of accumulating it, so a single pass leaves a root declaring only the
  # modules reachable from whichever schema it processed last.
  flatc --rust --rust-module-root-file \
    -o "${REPO_ROOT}/client/src/gen" -I "$SCHEMAS_DIR" "${fbs_files[@]}"
  flatc --rust --rust-module-root-file \
    -o "${REPO_ROOT}/client/src/gen" -I "$SCHEMAS_DIR" "${SCHEMAS_DIR}/envelope.fbs"
  (cd "${REPO_ROOT}/client" && cargo fmt --all)
fi

if git -C "$REPO_ROOT" diff --quiet -- "${gen_paths[@]}"; then
  echo "[PASS] committed bindings match the contract"
  exit 0
fi

echo "[FAIL] committed bindings are stale — the contract has moved and gen/ has not:" >&2
git -C "$REPO_ROOT" diff --stat -- "${gen_paths[@]}" >&2
echo >&2
echo "The regenerated files are in your working tree now; review and commit them." >&2
echo "Never hand-edit gen/ — the recipe lives in schemas/AGENTS.md." >&2
exit 1
