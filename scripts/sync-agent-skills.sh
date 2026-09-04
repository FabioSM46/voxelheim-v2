#!/usr/bin/env bash
# Keep the Codex and OpenCode skill adapters in sync with the canonical Claude skills.

set -euo pipefail

MODE=write
case "${1:-}" in
  "") ;;
  --check) MODE=check ;;
  *)
    echo "usage: bash scripts/sync-agent-skills.sh [--check]" >&2
    exit 2
    ;;
esac

REPO_ROOT=$(git rev-parse --show-toplevel)
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

SKILLS=(dev-issue process-pr scrum-master)
status=0

frontmatter_value() {
  local key="$1" source="$2"
  sed -n "s/^${key}:[[:space:]]*//p" "$source" | head -1
}

skill_body() {
  local source="$1"
  awk '
    NR == 1 && $0 == "---" { in_frontmatter = 1; next }
    in_frontmatter && $0 == "---" { in_frontmatter = 0; next }
    !in_frontmatter { print }
  ' "$source"
}

interface_metadata() {
  case "$1" in
    dev-issue)
      DISPLAY_NAME="Develop GitHub Issue"
      SHORT_DESCRIPTION="Implement a GitHub issue into a reviewed PR"
      DEFAULT_PROMPT='Use $dev-issue to implement GitHub issue #42 and open a PR.'
      ;;
    process-pr)
      DISPLAY_NAME="Process Pull Request"
      SHORT_DESCRIPTION="Resolve CI and review feedback on an open PR"
      DEFAULT_PROMPT='Use $process-pr to address CI and review feedback on PR #42.'
      ;;
    scrum-master)
      DISPLAY_NAME="Scrum Master"
      SHORT_DESCRIPTION="Run backlog, iteration, and feature ceremonies"
      DEFAULT_PROMPT='Use $scrum-master to run backlog refinement for this repository.'
      ;;
    *)
      echo "sync-agent-skills: add Codex interface metadata for '$1'" >&2
      exit 1
      ;;
  esac
}

render_codex_skill() {
  local source="$1" output="$2" name description
  name=$(frontmatter_value name "$source")
  description=$(frontmatter_value description "$source")
  if [ -z "$name" ] || [ -z "$description" ]; then
    echo "sync-agent-skills: missing name or description in ${source#$REPO_ROOT/}" >&2
    exit 1
  fi

  {
    printf '%s\n' '---'
    printf 'name: %s\n' "$name"
    printf 'description: %s\n' "$description"
    printf '%s\n\n' '---'
    skill_body "$source" | sed \
      -e 's|/dev-issue|$dev-issue|g' \
      -e 's|/process-pr|$process-pr|g' \
      -e 's|/scrum-master|$scrum-master|g' \
      -e 's|/develop-iteration|$develop-iteration|g'
  } >"$output"
}

render_codex_metadata() {
  local skill="$1" output="$2"
  interface_metadata "$skill"
  {
    printf '%s\n' 'interface:'
    printf '  display_name: "%s"\n' "$DISPLAY_NAME"
    printf '  short_description: "%s"\n' "$SHORT_DESCRIPTION"
    printf '  default_prompt: "%s"\n' "$DEFAULT_PROMPT"
    printf '%s\n' 'policy:'
    printf '%s\n' '  allow_implicit_invocation: false'
  } >"$output"
}

render_opencode_skill() {
  local source="$1" output="$2" name description
  name=$(frontmatter_value name "$source")
  description=$(frontmatter_value description "$source")
  if [ -z "$name" ] || [ -z "$description" ]; then
    echo "sync-agent-skills: missing name or description in ${source#$REPO_ROOT/}" >&2
    exit 1
  fi

  {
    printf '%s\n' '---'
    printf 'name: %s\n' "$name"
    printf 'description: Use ONLY when the user explicitly requests /%s. %s\n' "$name" "$description"
    printf '%s\n' 'compatibility: opencode'
    printf '%s\n' 'metadata:'
    printf '%s\n' '  opencode/autoinvoke: "false"'
    printf '%s\n\n' '---'
    skill_body "$source"
  } >"$output"
}

sync_file() {
  local generated="$1" target="$2"
  if [ "$MODE" = check ]; then
    if ! cmp -s "$generated" "$target"; then
      echo "OUT OF SYNC: ${target#$REPO_ROOT/}" >&2
      status=1
    fi
    return
  fi

  mkdir -p "$(dirname "$target")"
  install -m 0644 "$generated" "$target"
  echo "synced ${target#$REPO_ROOT/}"
}

for skill in "${SKILLS[@]}"; do
  source="$REPO_ROOT/.claude/skills/$skill/SKILL.md"
  if [ ! -f "$source" ]; then
    echo "sync-agent-skills: missing canonical skill: ${source#$REPO_ROOT/}" >&2
    exit 1
  fi

  codex_skill="$TMP_DIR/$skill.codex.SKILL.md"
  codex_metadata="$TMP_DIR/$skill.openai.yaml"
  opencode_skill="$TMP_DIR/$skill.opencode.SKILL.md"

  render_codex_skill "$source" "$codex_skill"
  render_codex_metadata "$skill" "$codex_metadata"
  render_opencode_skill "$source" "$opencode_skill"

  sync_file "$codex_skill" "$REPO_ROOT/.agents/skills/$skill/SKILL.md"
  sync_file "$codex_metadata" "$REPO_ROOT/.agents/skills/$skill/agents/openai.yaml"
  sync_file "$opencode_skill" "$REPO_ROOT/.opencode/skills/$skill/SKILL.md"
done

if [ "$MODE" = check ]; then
  if [ "$status" -ne 0 ]; then
    echo "Run: bash scripts/sync-agent-skills.sh" >&2
    exit "$status"
  fi
  echo "agent skill adapters are in sync"
fi
