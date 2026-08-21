#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
CHECK="$REPO_ROOT/scripts/check-commit-email-privacy.sh"
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

if (cd "$TEST_ROOT" && bash "$CHECK" >/dev/null 2>&1); then
  echo "expected missing range arguments to fail" >&2
  exit 1
fi

echo "commit email privacy — GitHub noreply identities pass, exposed identities fail without leaking"
