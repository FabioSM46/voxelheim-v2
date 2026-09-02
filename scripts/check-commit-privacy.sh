#!/usr/bin/env bash
# Reject commits that would publish private data through their identity fields OR
# through their message. Both are permanent, world-readable git objects.
#
# Diagnostics name only a commit and a category; the rejected value is never printed.
# That discipline matters here in a way it does not in an ordinary check: a leak check
# that echoes the leak has published it a second time, into a public CI log, where
# nobody thinks to look for it and nobody can redact it.
#
# The message scan applies the three categories scripts/check-publication-privacy.sh
# already applies to tracked content, plus the one generated shape that made this
# necessary. Every privacy check holds those three patterns separately and
# scripts/test/commit-privacy.test.sh pins the set — the same idiom the repository
# uses wherever a definition must agree in more than one place. A new reader of the
# patterns joins that pin rather than making another copy nothing compares;
# scripts/check-body-privacy.sh was the third (#130).

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
    # noreply@anthropic.com is the co-author trailer an agent's harness appends. It names
    # no person, it is a vendor no-reply that is already public, and much of develop's
    # history already carries it — so refusing it would redden a whole class of legitimate
    # commits while protecting nothing, and a check that fires on a non-leak is one people
    # learn to work around. No commit count is written here on purpose: it moves with every
    # merge, and a number nobody re-measures is how a claim about the world goes stale.
    # Approved by name and not as `noreply@*`: a no-reply under a private host would publish
    # an internal hostname, which is the thing these scans exist to catch. No example of one
    # is written here either, because the file scan reads this file too — the same problem
    # the assembled-from-pieces path prefixes below solve the same way.
    noreply@github.com|noreply@anthropic.com|*@users.noreply.github.com|*@example.invalid|*@example.com|*@example.org|*@example.net)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

# A GitHub noreply address is resolved by the NUMBER in front of the plus, and by nothing
# else. `<id>+<login>@users.noreply.github.com` credits whoever owns `<id>`; the login
# beside it is decoration GitHub never reads. So an address naming the right person with
# the wrong number is well-formed, passes every shape test above — `*@users.noreply…`
# approves it — and silently credits a stranger's account for work they never did.
#
# It happened here three times, in co-author trailers a tool appended, and it was found
# the only way it could be: by a human noticing two accounts nobody recognised in the
# repository's contributor graph, one commit each, weeks later. Nothing was red. This is
# the same family as the generated session trailer — a machine-written value recurs
# *because* it is generated rather than because anybody was careless — and it earns a
# rule for the same reason.
#
# The table is the only claim about the world that can be checked without the network,
# and it is deliberately narrow: for a login it names, the id must be that login's; for a
# login it does not name, it says nothing. That is the exact shape of the observed defect
# — the right name beside the wrong number — and it is what keeps the rule from rejecting
# the synthetic identities the privacy tests are built from. It does not attempt to
# enumerate who may appear; that is the approved-identity rule in AGENTS.md, enforced by
# a reader rather than by a pattern.
github_noreply_ids=(
  "fabiosm46:124870035"
  "github-actions[bot]:41898282"
)

# The id group is optional because the legacy form `<login>@users.noreply.github.com`
# carries no number and still resolves correctly — most of this repository's history uses
# it. A plus with nothing before it is a different thing: it resolves to nobody, and it is
# rejected whatever login follows, because an address that credits no account is a defect
# on any name.
github_noreply_pattern='^(([0-9]*)\+)?([^@+]+)@users\.noreply\.github\.com$'

is_misattributed_noreply() {
  local email=${1,,} plus prefix login entry
  [[ "$email" =~ $github_noreply_pattern ]] || return 1
  plus=${BASH_REMATCH[1]}
  prefix=${BASH_REMATCH[2]}
  login=${BASH_REMATCH[3]}
  if [ -n "$plus" ] && [ -z "$prefix" ]; then
    return 0
  fi
  for entry in "${github_noreply_ids[@]}"; do
    [ "${entry%:*}" = "$login" ] || continue
    [ -z "$prefix" ] && return 1
    [ "$prefix" = "${entry##*:}" ] && return 1
    return 0
  done
  return 1
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

  # Checked separately from the exposure rule above, because these two fail in opposite
  # directions: an exposed address publishes the author, a misattributed one publishes
  # somebody else entirely. A commit can be perfectly private and still credit a stranger.
  if is_misattributed_noreply "$author_email"; then
    echo "commit privacy: ${short_commit} author email credits another account's id" >&2
    violations=$((violations + 1))
  fi
  if is_misattributed_noreply "$committer_email"; then
    echo "commit privacy: ${short_commit} committer email credits another account's id" >&2
    violations=$((violations + 1))
  fi

  # One diagnostic per category per commit: the count of occurrences is itself a hint
  # about the content, and the category plus the commit is everything an author needs
  # to find it in a message they wrote.
  unapproved_email=0
  misattributed_id=0
  while IFS= read -r email; do
    if ! is_approved_email "$email"; then
      unapproved_email=1
    fi
    if is_misattributed_noreply "$email"; then
      misattributed_id=1
    fi
  done < <(grep -Eo "$email_pattern" <<< "$message" || true)
  if [ "$unapproved_email" -ne 0 ]; then
    echo "commit privacy: ${short_commit} message contains a non-public email address" >&2
    violations=$((violations + 1))
  fi
  # This is the one that matters in practice: every occurrence so far arrived in a
  # Co-authored-by trailer, which is a message and not an identity field.
  if [ "$misattributed_id" -ne 0 ]; then
    echo "commit privacy: ${short_commit} message contains a misattributed GitHub account id" >&2
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
  echo "session reference belongs in a commit anyone can read forever. Copy that address" >&2
  echo "rather than typing it — the number in front of the plus is the whole identity, and" >&2
  echo "a wrong one credits a stranger's account instead of failing." >&2
  exit 1
fi

echo "commit privacy — ${#commits[@]} commit(s) use GitHub noreply and carry no private data in their messages"
