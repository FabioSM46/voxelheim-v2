#!/usr/bin/env bash
# Reject tracked content that would expose private identity or workstation data.
# Diagnostics name only a location and category; the rejected value is never printed.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

mapfile -d '' tracked_files < <(git ls-files -z)
if [ "${#tracked_files[@]}" -eq 0 ]; then
  echo "publication privacy: no tracked files found" >&2
  exit 2
fi

violations=0
scannable_files=()
for path in "${tracked_files[@]}"; do
  basename=${path##*/}
  case "$basename" in
    .env)
      echo "publication privacy: ${path} is a tracked secret-bearing environment file" >&2
      violations=$((violations + 1))
      ;;
    .env.*)
      if [ "$basename" = ".env.example" ]; then
        scannable_files+=("$path")
      else
        echo "publication privacy: ${path} is a tracked secret-bearing environment file" >&2
        violations=$((violations + 1))
      fi
      ;;
    *)
      scannable_files+=("$path")
      ;;
  esac
done

is_approved_email() {
  local email=${1,,}
  case "$email" in
    # noreply@anthropic.com is the co-author trailer an agent's harness appends. It names
    # no person, it is a vendor no-reply that is already public, and eleven commits on
    # develop carry it — so refusing it would redden a whole class of legitimate commits
    # while protecting nothing, and a check that fires on a non-leak is one people learn
    # to work around. Approved by name and not as `noreply@*`: a no-reply under a private
    # host would publish an internal hostname, which is the thing these scans exist to
    # catch. No example of one is written here, because the file scan reads this file too
    # — the same problem the assembled-from-pieces path prefixes below solve the same way.
    noreply@github.com|noreply@anthropic.com|*@users.noreply.github.com|*@example.invalid|*@example.com|*@example.org|*@example.net)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

email_pattern='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
while IFS= read -r match; do
  path=${match%%:*}
  remainder=${match#*:}
  line=${remainder%%:*}
  content=${remainder#*:}
  while IFS= read -r email; do
    if ! is_approved_email "$email"; then
      echo "publication privacy: ${path}:${line} contains a non-public email address" >&2
      violations=$((violations + 1))
    fi
  done < <(grep -Eo "$email_pattern" <<< "$content" || true)
done < <(git grep -I -n -E "$email_pattern" -- "${scannable_files[@]}" || true)

slash=/
backslash='\'
private_path_prefixes=(
  "${slash}home${slash}"
  "${slash}Users${slash}"
  "${slash}workspace${slash}"
  "${slash}workspaces${slash}"
  "C:${backslash}Users${backslash}"
)

for prefix in "${private_path_prefixes[@]}"; do
  while IFS= read -r match; do
    path=${match%%:*}
    remainder=${match#*:}
    line=${remainder%%:*}
    echo "publication privacy: ${path}:${line} contains a workstation-specific path" >&2
    violations=$((violations + 1))
  done < <(git grep -I -n -F "$prefix" -- "${scannable_files[@]}" || true)
done

private_network_pattern='(^|[^0-9])(10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|192\.168\.[0-9]{1,3}\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3})([^0-9]|$)'
while IFS= read -r match; do
  path=${match%%:*}
  remainder=${match#*:}
  line=${remainder%%:*}
  echo "publication privacy: ${path}:${line} contains a private-network address" >&2
  violations=$((violations + 1))
done < <(git grep -I -n -E "$private_network_pattern" -- "${scannable_files[@]}" || true)

if [ "$violations" -ne 0 ]; then
  echo "publication privacy: rejected ${violations} private-data occurrence(s)" >&2
  exit 1
fi

echo "publication privacy — tracked content contains only approved public identities and synthetic paths"
