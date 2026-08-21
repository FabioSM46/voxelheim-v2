#!/usr/bin/env bash
# Reject commits that would publish private data through their identity fields OR
# through their message. Both are permanent, world-readable git objects.
#
# Diagnostics name only a commit and a category; the rejected value is never printed.
# That discipline matters more here than anywhere else in the repository: a leak check
# that echoes the leak has published it a second time, into a public CI log, where
# nobody thinks to look for it and nobody can redact it.
#
# The message scan applies the three categories scripts/check-publication-privacy.sh
# already applies to tracked content, plus the one generated shape that made this
# necessary. The two scripts hold those three patterns separately and
# scripts/test/commit-privacy.test.sh pins the pair — the same idiom the repository
# uses wherever a definition must agree in two places.

set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <base-commit> <head-commit>" >&2
  exit 2
fi

base_commit=$1
head_commit=$2

for commit in "$base_commit" "$head_commit"; do
  if ! git rev-parse --verify --quiet "${commit}^{commit}" >/dev/null; then
    echo "commit privacy: required commit is unavailable" >&2
    exit 2
  fi
done

if ! merge_base=$(git merge-base "$base_commit" "$head_commit"); then
  echo "commit privacy: base and head have no shared history" >&2
  exit 2
fi

# The range is what HEAD adds and nothing else, which is load-bearing rather than
# incidental. History already on the base branch is out of reach — the rulesets forbid
# the direct push a rewrite would need — so a scan any wider than this would fail on
# commits nobody can change, and would stay failing for every pull request after it.
mapfile -t commits < <(git rev-list --reverse "${merge_base}..${head_commit}")
if [ "${#commits[@]}" -eq 0 ]; then
  echo "commit privacy: no commits found in the requested range" >&2
  exit 2
fi

# An author or committer field names a person, so only a GitHub noreply address may
# stand there. The message scan below is deliberately more permissive: a reserved
# example domain is legitimate prose, and it is what the file scanner already allows.
is_github_noreply() {
  [[ "$1" =~ ^(noreply@github\.com|[^@]+@users\.noreply\.github\.com)$ ]]
}

is_approved_email() {
  local email=${1,,}
  case "$email" in
    noreply@github.com|*@users.noreply.github.com|*@example.invalid|*@example.com|*@example.org|*@example.net)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

email_pattern='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'

# Assembled from pieces so this file does not itself contain a workstation path prefix.
slash=/
backslash='\'
private_path_prefixes=(
  "${slash}home${slash}"
  "${slash}Users${slash}"
  "${slash}workspace${slash}"
  "${slash}workspaces${slash}"
  "C:${backslash}Users${backslash}"
)

private_network_pattern='(^|[^0-9])(10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}|192\.168\.[0-9]{1,3}\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3})([^0-9]|$)'

# The one generated shape, matched as a shape and not as a substring. "A non-public
# service endpoint" cannot be pattern-matched in general, and trying would fire on every
# URL anyone ever put in a commit message. What can be matched is the trailer that keeps
# arriving because a tool appends it, and the account-scoped URL it carries.
#
# A trailer is a key at the start of a line. A sentence that merely names the trailer is
# not one — this file's own comments are the proof that the distinction has to hold.
session_trailer_pattern='^[[:space:]]*Claude-Session[[:space:]]*:'
session_url_pattern='claude\.ai/code/session_'

violations=0
for commit in "${commits[@]}"; do
  short_commit=$(git rev-parse --short=12 "$commit")
  author_email=$(git show -s --format=%ae "$commit")
  committer_email=$(git show -s --format=%ce "$commit")
  message=$(git show -s --format=%B "$commit")

  if ! is_github_noreply "$author_email"; then
    echo "commit privacy: ${short_commit} exposes its author email" >&2
    violations=$((violations + 1))
  fi
  if ! is_github_noreply "$committer_email"; then
    echo "commit privacy: ${short_commit} exposes its committer email" >&2
    violations=$((violations + 1))
  fi

  # One diagnostic per category per commit: the count of occurrences is itself a hint
  # about the content, and the category plus the commit is everything an author needs
  # to find it in a message they wrote.
  unapproved_email=0
  while IFS= read -r email; do
    if ! is_approved_email "$email"; then
      unapproved_email=1
    fi
  done < <(grep -Eo "$email_pattern" <<< "$message" || true)
  if [ "$unapproved_email" -ne 0 ]; then
    echo "commit privacy: ${short_commit} message contains a non-public email address" >&2
    violations=$((violations + 1))
  fi

  for prefix in "${private_path_prefixes[@]}"; do
    if grep -qF -- "$prefix" <<< "$message"; then
      echo "commit privacy: ${short_commit} message contains a workstation-specific path" >&2
      violations=$((violations + 1))
      break
    fi
  done

  if grep -qE -- "$private_network_pattern" <<< "$message"; then
    echo "commit privacy: ${short_commit} message contains a private-network address" >&2
    violations=$((violations + 1))
  fi

  if grep -qE -- "$session_trailer_pattern" <<< "$message" ||
     grep -qE -- "$session_url_pattern" <<< "$message"; then
    echo "commit privacy: ${short_commit} message contains a generated agent session reference" >&2
    violations=$((violations + 1))
  fi
done

if [ "$violations" -ne 0 ]; then
  echo "commit privacy: rejected ${violations} private-data occurrence(s)" >&2
  echo "Configure Git with the GitHub noreply address shown under Settings → Emails, and" >&2
  echo "amend the message: no workstation path, private-network address or generated" >&2
  echo "session reference belongs in a commit anyone can read forever." >&2
  exit 1
fi

echo "commit privacy — ${#commits[@]} commit(s) use GitHub noreply and carry no private data in their messages"
