#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
CHECK="$REPO_ROOT/scripts/check-publication-privacy.sh"
TEST_ROOT=$(mktemp -d)
trap 'rm -rf -- "$TEST_ROOT"' EXIT

mkdir -p "$TEST_ROOT/scripts"
cp "$CHECK" "$TEST_ROOT/scripts/check-publication-privacy.sh"
git -C "$TEST_ROOT" init -q -b develop

printf '%s\n' \
  'commit=1000+privacy@users.noreply.github.com' \
  'fixture=private-address@example.invalid' \
  'path=<workspace>/voxelheim-v2' > "$TEST_ROOT/safe.txt"
git -C "$TEST_ROOT" add scripts/check-publication-privacy.sh safe.txt
(cd "$TEST_ROOT" && bash scripts/check-publication-privacy.sh >/dev/null)

unsafe_email="person""@""private.test"
printf 'contact=%s\n' "$unsafe_email" > "$TEST_ROOT/contact.txt"
git -C "$TEST_ROOT" add contact.txt
set +e
email_output=$(cd "$TEST_ROOT" && bash scripts/check-publication-privacy.sh 2>&1)
email_status=$?
set -e
if [ "$email_status" -ne 1 ]; then
  echo "expected a private email fixture to exit 1, got $email_status" >&2
  exit 1
fi
grep -Fq 'contact.txt:1 contains a non-public email address' <<< "$email_output"
if grep -Fq "$unsafe_email" <<< "$email_output"; then
  echo "privacy failure: the rejected email reached the diagnostic" >&2
  exit 1
fi

git -C "$TEST_ROOT" rm -f -q contact.txt
internal_path="/""home/private/worktree"
printf 'checkout=%s\n' "$internal_path" > "$TEST_ROOT/path.txt"
git -C "$TEST_ROOT" add path.txt
set +e
path_output=$(cd "$TEST_ROOT" && bash scripts/check-publication-privacy.sh 2>&1)
path_status=$?
set -e
if [ "$path_status" -ne 1 ]; then
  echo "expected an internal path fixture to exit 1, got $path_status" >&2
  exit 1
fi
grep -Fq 'path.txt:1 contains a workstation-specific path' <<< "$path_output"
if grep -Fq "$internal_path" <<< "$path_output"; then
  echo "privacy failure: the rejected path reached the diagnostic" >&2
  exit 1
fi

echo "publication privacy — safe placeholders pass and private values fail without leaking"
