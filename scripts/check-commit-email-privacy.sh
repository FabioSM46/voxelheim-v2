#!/usr/bin/env bash
# Reject commits that would publish an author or committer email address.
# The offending address is deliberately never printed: CI logs are public.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <base-commit> <head-commit>" >&2
  exit 2
fi

base_commit=$1
head_commit=$2

for commit in "$base_commit" "$head_commit"; do
  if ! git rev-parse --verify --quiet "${commit}^{commit}" >/dev/null; then
    echo "commit email privacy: required commit is unavailable" >&2
    exit 2
  fi
done

if ! merge_base=$(git merge-base "$base_commit" "$head_commit"); then
  echo "commit email privacy: base and head have no shared history" >&2
  exit 2
fi

mapfile -t commits < <(git rev-list --reverse "${merge_base}..${head_commit}")
if [ "${#commits[@]}" -eq 0 ]; then
  echo "commit email privacy: no commits found in the requested range" >&2
  exit 2
fi

is_github_noreply() {
  [[ "$1" =~ ^(noreply@github\.com|[^@]+@users\.noreply\.github\.com)$ ]]
}

violations=0
for commit in "${commits[@]}"; do
  short_commit=$(git rev-parse --short=12 "$commit")
  author_email=$(git show -s --format=%ae "$commit")
  committer_email=$(git show -s --format=%ce "$commit")

  if ! is_github_noreply "$author_email"; then
    echo "commit email privacy: ${short_commit} exposes its author email" >&2
    violations=$((violations + 1))
  fi
  if ! is_github_noreply "$committer_email"; then
    echo "commit email privacy: ${short_commit} exposes its committer email" >&2
    violations=$((violations + 1))
  fi
done

if [ "$violations" -ne 0 ]; then
  echo "commit email privacy: rejected ${violations} non-private identity field(s)" >&2
  echo "Configure Git with the GitHub noreply address shown under Settings → Emails." >&2
  exit 1
fi

echo "commit email privacy — ${#commits[@]} commit(s) use GitHub noreply"
