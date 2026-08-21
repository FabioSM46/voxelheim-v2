#!/usr/bin/env bash
# Reject an issue or pull-request body that publishes private identity or workstation data.
#
# Diagnostics name only a line number and a category; the rejected value is never printed.
# That discipline exists in all three privacy checks, and it is worth the most here. This
# one reports into a GitHub comment, which carries notifications and an audience: a finding
# that quotes the leak has mailed it to every watcher and pinned a second, separately
# addressable copy beside the first. A body can be edited; a comment quoting it cannot be
# un-sent, and neither can the notification. Quoting the value would destroy the only
# remedy the author still has.
#
# THIS CHECK CANNOT PREVENT A LEAK, and that is a real difference from the other two rather
# than a caveat. A body exists only once it has been posted, so unlike
# check-publication-privacy.sh (tracked content, before a push) and check-commit-privacy.sh
# (commits, before a push), this one necessarily runs after the fact. Its whole value is the
# speed and clarity of the alarm: it shortens how long nobody knew. It does not shorten how
# long the value was readable, and a clean result here is not evidence that nothing was
# published — only that nothing is published *now*.
#
# The three categories are the ones scripts/check-publication-privacy.sh already owns; this
# script adds none and widens none. All three privacy checks hold those definitions
# separately and scripts/test/commit-privacy.test.sh pins the set to each other — the same
# idiom the repository uses wherever a definition must agree in more than one file. A fourth
# reader joins that pin rather than making a fourth copy nothing compares.
#
# Usage:  check-body-privacy.sh < body
#
#   The body arrives on STDIN and never on the command line, so it cannot be read out of the
#   process table by anything else on the runner.
#
#   STDOUT is the findings and nothing else — one line each, empty when the body is clean.
#   That contract is what lets a caller pipe stdout straight into a public comment without
#   inspecting it, and it is only safe because of the first paragraph.
#   STDERR carries the summary and the caveat above, for the run log.
#
#   Exit 0  the body is clean
#   Exit 1  findings were printed to stdout
#   Exit 2  the script was called wrongly (no input, or arguments it does not take)

set -euo pipefail

if [ "$#" -ne 0 ]; then
  echo "usage: $0 < body" >&2
  exit 2
fi

if [ -t 0 ]; then
  echo "body privacy: the body is read from standard input; nothing was piped in" >&2
  exit 2
fi

body=$(cat)

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

# A finding is a line number and a category. The line number is a location, not a value:
# it says where to look in a body the reader already has open, and it survives the edit
# that removes the value, which is exactly what an author needs to confirm the fix.
findings=()
add_finding() {
  findings+=("$1"$'\t'"$2")
}

# One finding per category per line. A second occurrence on the same line adds no location
# a reader could act on, and a count of occurrences is itself a hint about the content.
while IFS= read -r match; do
  line=${match%%:*}
  content=${match#*:}
  unapproved=0
  while IFS= read -r email; do
    if ! is_approved_email "$email"; then
      unapproved=1
    fi
  done < <(grep -Eo "$email_pattern" <<< "$content" || true)
  if [ "$unapproved" -ne 0 ]; then
    add_finding "$line" "a non-public email address"
  fi
done < <(grep -n -E "$email_pattern" <<< "$body" || true)

# Collected across every prefix and then deduplicated, so a line matching two of them is
# one finding rather than two.
while IFS= read -r line; do
  [ -n "$line" ] || continue
  add_finding "$line" "a workstation-specific path"
done < <(
  for prefix in "${private_path_prefixes[@]}"; do
    grep -n -F -- "$prefix" <<< "$body" || true
  done | cut -d: -f1 | sort -n -u
)

while IFS= read -r match; do
  add_finding "${match%%:*}" "a private-network address"
done < <(grep -n -E -- "$private_network_pattern" <<< "$body" || true)

if [ "${#findings[@]}" -ne 0 ]; then
  # Sorted by line so the report reads in the order of the body. `-u` is keyed on both
  # fields deliberately: keyed on the line number alone it would drop a second category
  # found on the same line, which is the one place a body carries two different leaks and
  # the reader needs to be told about both.
  printf '%s\n' "${findings[@]}" |
    sort -t $'\t' -k1,1n -k2,2 -u |
    while IFS=$'\t' read -r line category; do
      printf 'body privacy: line %s contains %s\n' "$line" "$category"
    done

  echo "body privacy: rejected ${#findings[@]} private-data occurrence(s)" >&2
  echo "This body was already published when this check ran — editing it changes what is" >&2
  echo "read next, not what was already sent. If the value was a credential, rotate it." >&2
  exit 1
fi

echo "body privacy — the body carries no non-public email address, workstation path or private-network address" >&2
