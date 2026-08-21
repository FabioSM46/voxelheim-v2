#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
CHECK="$REPO_ROOT/scripts/check-commit-privacy.sh"
TEST_ROOT=$(mktemp -d)
trap 'rm -rf -- "$TEST_ROOT"' EXIT

git -C "$TEST_ROOT" init -q -b develop
git -C "$TEST_ROOT" -c user.name=PrivacyTest \
  -c user.email=1000+privacy@users.noreply.github.com \
  commit -q --allow-empty -m root
git -C "$TEST_ROOT" branch feature

git -C "$TEST_ROOT" -c user.name=PrivacyTest \
  -c user.email=1000+privacy@users.noreply.github.com \
  commit -q --allow-empty -m base-advanced
base_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)

git -C "$TEST_ROOT" switch -q feature
git -C "$TEST_ROOT" -c user.name=PrivacyTest \
  -c user.email=1000+privacy@users.noreply.github.com \
  commit -q --allow-empty -m private
private_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)

if git -C "$TEST_ROOT" merge-base --is-ancestor "$base_commit" "$private_commit"; then
  echo "test fixture failure: the advanced base unexpectedly precedes the feature" >&2
  exit 1
fi
private_output=$(cd "$TEST_ROOT" && bash "$CHECK" "$base_commit" "$private_commit")
grep -Fq '1 commit(s) use GitHub noreply' <<< "$private_output"

GIT_AUTHOR_NAME=GitHub \
GIT_AUTHOR_EMAIL=noreply@github.com \
GIT_COMMITTER_NAME=GitHub \
GIT_COMMITTER_EMAIL=noreply@github.com \
  git -C "$TEST_ROOT" commit -q --allow-empty -m github-generated
github_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
github_output=$(cd "$TEST_ROOT" && bash "$CHECK" "$private_commit" "$github_commit")
grep -Fq '1 commit(s) use GitHub noreply' <<< "$github_output"

GIT_AUTHOR_NAME=PrivacyTest \
GIT_AUTHOR_EMAIL=private-address@example.invalid \
GIT_COMMITTER_NAME=PrivacyTest \
GIT_COMMITTER_EMAIL=1000+privacy@users.noreply.github.com \
  git -C "$TEST_ROOT" commit -q --allow-empty -m exposed-author
exposed_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)

set +e
exposed_output=$(cd "$TEST_ROOT" && bash "$CHECK" "$github_commit" "$exposed_commit" 2>&1)
exposed_status=$?
set -e

if [ "$exposed_status" -ne 1 ]; then
  echo "expected an exposed author email to exit 1, got $exposed_status" >&2
  exit 1
fi
grep -Fq 'exposes its author email' <<< "$exposed_output"
if grep -Fq 'private-address@example.invalid' <<< "$exposed_output"; then
  echo "privacy failure: the rejected address reached the diagnostic" >&2
  exit 1
fi


# ---------------------------------------------------------------- the message scan
# Everything below drives the half this check gained in #128: a commit message is a
# published surface, and until now nothing read one. Fixture values are assembled from
# pieces at run time so this file does not itself contain a workstation path, a
# private-network address or a non-public email address.

commit_as_noreply() {
  git -C "$TEST_ROOT" -c user.name=PrivacyTest \
    -c user.email=1000+privacy@users.noreply.github.com \
    commit -q --allow-empty "$@"
}

run_check() {
  local status=0
  CHECK_OUTPUT=$(cd "$TEST_ROOT" && bash "$CHECK" "$1" "$2" 2>&1) || status=$?
  CHECK_STATUS=$status
}

assert_absent() {
  if grep -Fq "$1" <<< "$CHECK_OUTPUT"; then
    echo "privacy failure: $2 reached the diagnostic" >&2
    exit 1
  fi
}

# All three categories in one message. The assertions that matter are the last three:
# a check that names the value it rejected has published it a second time, into a public
# CI log, which is a wider audience than the commit it was protecting.
internal_path="/""home/example-user/voxelheim"
private_ip="10.""0.0.7"
unsafe_email="person""@""private.test"
commit_as_noreply -m "chore: probe a message carrying all three categories

Built under ${internal_path} against ${private_ip}, reported by ${unsafe_email}."
triple_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)

run_check "$exposed_commit" "$triple_commit"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a message carrying private data to exit 1, got $CHECK_STATUS" >&2
  exit 1
fi
grep -Fq 'message contains a workstation-specific path' <<< "$CHECK_OUTPUT"
grep -Fq 'message contains a private-network address' <<< "$CHECK_OUTPUT"
grep -Fq 'message contains a non-public email address' <<< "$CHECK_OUTPUT"
assert_absent "$internal_path" "the rejected path"
assert_absent "$private_ip" "the rejected address"
assert_absent "$unsafe_email" "the rejected email"

# The trailer, carrying no URL at all, so only the trailer rule can be what fired.
commit_as_noreply -m "chore: probe the generated trailer" -m "Claude-Session: reference-placeholder"
trailer_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$triple_commit" "$trailer_commit"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a session trailer to exit 1, got $CHECK_STATUS" >&2
  exit 1
fi
grep -Fq 'message contains a generated agent session reference' <<< "$CHECK_OUTPUT"

# The URL mid-sentence, where no trailer key can match it: a URL is the payload wherever
# it sits, so this rule is anchored to the shape rather than to the line.
session_url="https://claude.ai/code/session_0123456789abcdef"
commit_as_noreply -m "chore: probe the session URL in prose

Context for this change is recorded at ${session_url}, which is account-scoped."
url_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$trailer_commit" "$url_commit"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected a session URL to exit 1, got $CHECK_STATUS" >&2
  exit 1
fi
grep -Fq 'message contains a generated agent session reference' <<< "$CHECK_OUTPUT"
assert_absent "$session_url" "the rejected session URL"

# Merely naming the trailer is prose. The rule is about the shape a tool appends, not
# about a substring — a check that could not tell them apart would reject the commit
# that documents it, and every commit discussing this issue.
commit_as_noreply -m "docs: probe a message that only names the trailer

This commit deliberately omits the Claude-Session trailer, and says so in prose."
mention_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$url_commit" "$mention_commit"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected a mere mention of the trailer to pass, got $CHECK_STATUS" >&2
  exit 1
fi

# The message scan and the file scan agree on what an address may be. They hold the
# three patterns separately and are pinned to each other at the bottom of this file;
# this is the same claim from the outside, where a disagreement would be visible.
commit_as_noreply -m "chore: probe the identities both scans allow

Co-authored-by: Fixture <1000+fixture@users.noreply.github.com>
Reported-by: fixture@example.invalid"
approved_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$mention_commit" "$approved_commit"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected approved identities in a message to pass, got $CHECK_STATUS" >&2
  exit 1
fi

# The range is honoured, and this is the assertion that keeps the check usable at all.
# Three commits already on develop carry the trailer above; the rulesets forbid the
# direct push a rewrite would need. A scan wider than BASE..HEAD would fail the very next
# pull request on history nobody can change and stay failing forever. Four violating
# commits sit in this base's ancestry and none of them is in the range.
commit_as_noreply -m "chore: probe a clean successor of violating history"
clean_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$url_commit" "$clean_commit"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected violations outside BASE..HEAD to be out of reach, got $CHECK_STATUS" >&2
  echo "$CHECK_OUTPUT" >&2
  exit 1
fi
grep -Fq '3 commit(s) use GitHub noreply' <<< "$CHECK_OUTPUT"

# The co-author trailer an agent's harness appends is approved. Placed after the range
# case deliberately: the violating commit below must not land between that case's base
# and its head, or it would fail on a violation the range was written to exclude.
commit_as_noreply -m "chore: probe the agent co-author trailer

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
coauthor_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$clean_commit" "$coauthor_commit"
if [ "$CHECK_STATUS" -ne 0 ]; then
  echo "expected the agent co-author trailer to pass, got $CHECK_STATUS" >&2
  echo "$CHECK_OUTPUT" >&2
  exit 1
fi

# And the address beside it on the same domain is not approved. The rule is the literal
# address rather than the domain, because a no-reply is only harmless when it belongs to
# a service everybody can already see.
other_anthropic="someone""@""anthropic.com"
commit_as_noreply -m "chore: probe a second address on the approved address's domain

Reported-by: ${other_anthropic}"
domain_commit=$(git -C "$TEST_ROOT" rev-parse HEAD)
run_check "$coauthor_commit" "$domain_commit"
if [ "$CHECK_STATUS" -ne 1 ]; then
  echo "expected an unapproved address on the same domain to exit 1, got $CHECK_STATUS" >&2
  exit 1
fi
grep -Fq 'message contains a non-public email address' <<< "$CHECK_OUTPUT"
assert_absent "$other_anthropic" "the rejected email"

# An empty range still exits as it did before the message scan existed.
run_check "$clean_commit" "$clean_commit"
if [ "$CHECK_STATUS" -ne 2 ]; then
  echo "expected an empty range to exit 2, got $CHECK_STATUS" >&2
  exit 1
fi
grep -Fq 'no commits found in the requested range' <<< "$CHECK_OUTPUT"

if (cd "$TEST_ROOT" && bash "$CHECK" >/dev/null 2>&1); then
  echo "expected missing range arguments to fail" >&2
  exit 1
fi

# -------------------------------------------------------------------- the pinned set
# Every privacy check applies the same three categories, and each holds those definitions
# separately. That duplication is only safe while something compares them: a widened path
# prefix or a corrected email pattern that lands in one script and not the others would
# leave the surfaces disagreeing about what is private, silently and in the direction that
# publishes. Same idiom as the FULL_REVIEW_MARKER pair — the definitions may live in
# several files, the agreement may not be left to hand.
#
# The set is a list rather than a pair because it grew (#130 added the body scan). A fourth
# reader joins by adding one line here, which is the whole point: the alternative is a
# fourth copy nothing compares, and nothing would go red on the day it drifted.
PINNED_CHECKS=(
  "$CHECK"
  "$REPO_ROOT/scripts/check-publication-privacy.sh"
  "$REPO_ROOT/scripts/check-body-privacy.sh"
)

# Each definition is compared against the first script's copy, and a definition missing
# from any of them fails just as loudly as one that differs — a check that has quietly
# stopped carrying a pattern is not a check that agrees.
pin_extracted() {
  local label=$1 reference="" reference_file="" script value
  shift
  for script in "${PINNED_CHECKS[@]}"; do
    value=$("$@" "$script") || value=""
    if [ -z "$value" ]; then
      echo "pin failure: the ${label} was not found in ${script##*/}" >&2
      exit 1
    fi
    if [ -z "$reference_file" ]; then
      reference=$value
      reference_file=$script
      continue
    fi
    if [ "$value" != "$reference" ]; then
      echo "pin failure: the ${label} differs between ${reference_file##*/} and ${script##*/}" >&2
      exit 1
    fi
  done
}

extract_line() {
  grep -E -- "$1" "$2"
}

extract_path_prefix_block() {
  awk '/^slash=\/$/{f=1} f{print} f&&/^\)$/{exit}' "$1"
}

pin_extracted "email pattern" extract_line '^email_pattern='
pin_extracted "private-network pattern" extract_line '^private_network_pattern='
pin_extracted "approved-email list" extract_line '^ +noreply@github\.com\|'
pin_extracted "workstation path prefixes" extract_path_prefix_block

echo "commit privacy — noreply identities and clean messages pass; exposed identities, private message content and generated session references fail without leaking; ${#PINNED_CHECKS[@]} privacy checks agree on the three patterns"
