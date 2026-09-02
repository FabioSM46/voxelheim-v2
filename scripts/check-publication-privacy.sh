#!/usr/bin/env bash
# Reject tracked content that would expose private identity or workstation data.
# Diagnostics name only a location and category; the rejected value is never printed.
#
# This is where the three categories were first defined, and every other privacy check
# reads them: check-commit-privacy.sh for commit identities and messages,
# check-body-privacy.sh for issue and pull-request bodies. Each holds its own copy and
# scripts/test/commit-privacy.test.sh pins the set to each other, so widening a pattern
# here without widening it there fails until they agree. Changing anything below is
# therefore a change to every surface at once — which is the intent.

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

# Extracted separately from the address pattern above, and this is not a refinement — it is
# the difference between the table having two entries and having one. `email_pattern` has no
# bracket in its local-part class, so an address whose login carries one is not merely
# matched imprecisely, it is never extracted at all: the bot login this repository publishes
# alongside the handle was invisible to every message-driven scan, and only the author and
# committer fields, which are read whole and never grepped, could see it. Found in review on
# #797, on the change that introduced the table.
#
# So the rule is driven from this pattern alone on every text surface. Anchoring it to the
# domain is what lets the login part stay permissive: anything that is not whitespace and not
# an angle bracket, which is a trailer's own delimiters and nothing else.
github_noreply_extract_pattern='[^[:space:]<>]+@users\.noreply\.github\.com'

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

# Its own pass, for the reason given beside the pattern: a login carrying a bracket is not
# extracted by the address pattern above, so a second test inside that loop would have been
# reached only by the addresses that were never the problem.
while IFS= read -r match; do
  path=${match%%:*}
  remainder=${match#*:}
  line=${remainder%%:*}
  content=${remainder#*:}
  while IFS= read -r noreply; do
    if is_misattributed_noreply "$noreply"; then
      echo "publication privacy: ${path}:${line} contains a misattributed GitHub account id" >&2
      violations=$((violations + 1))
    fi
  done < <(grep -Eo "$github_noreply_extract_pattern" <<< "$content" || true)
done < <(git grep -I -n -E "$github_noreply_extract_pattern" -- "${scannable_files[@]}" || true)

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
