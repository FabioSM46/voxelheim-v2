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

git -C "$TEST_ROOT" rm -f -q path.txt
# The number in front of the plus is the whole identity: this address names the project's
# approved handle and credits somebody else. Nothing above can see it — it is a GitHub
# noreply address, which the approved-email list allows by shape — and that is the point.
wrong_id="9999""999999999"
printf 'Co-authored-by: FabioSM46 <%s+FabioSM46''@users.noreply.github.com>\n' "$wrong_id" \
  > "$TEST_ROOT/trailer.txt"
git -C "$TEST_ROOT" add trailer.txt
set +e
id_output=$(cd "$TEST_ROOT" && bash scripts/check-publication-privacy.sh 2>&1)
id_status=$?
set -e
if [ "$id_status" -ne 1 ]; then
  echo "expected a misattributed account id to exit 1, got $id_status" >&2
  exit 1
fi
grep -Fq 'trailer.txt:1 contains a misattributed GitHub account id' <<< "$id_output"
if grep -Fq "$wrong_id" <<< "$id_output"; then
  echo "privacy failure: the rejected account id reached the diagnostic" >&2
  exit 1
fi

# The bot login carries a bracket, which `email_pattern` cannot extract — so this file is
# invisible to the address scan and only the rule's own pass can refuse it.
git -C "$TEST_ROOT" rm -f -q trailer.txt
bracket_login="github-""actions[bot]"
printf 'Co-authored-by: %s <%s+%s''@users.noreply.github.com>\n' \
  "$bracket_login" "$wrong_id" "$bracket_login" > "$TEST_ROOT/bracket.txt"
git -C "$TEST_ROOT" add bracket.txt
set +e
bracket_output=$(cd "$TEST_ROOT" && bash scripts/check-publication-privacy.sh 2>&1)
bracket_status=$?
set -e
if [ "$bracket_status" -ne 1 ]; then
  echo "expected a bracketed login with another account's id to exit 1, got $bracket_status" >&2
  exit 1
fi
grep -Fq 'bracket.txt:1 contains a misattributed GitHub account id' <<< "$bracket_output"
if grep -Fq "$wrong_id" <<< "$bracket_output"; then
  echo "privacy failure: the rejected account id reached the diagnostic" >&2
  exit 1
fi
git -C "$TEST_ROOT" rm -f -q bracket.txt
git -C "$TEST_ROOT" checkout -q -- . 2>/dev/null || true
printf 'Co-authored-by: FabioSM46 <%s+FabioSM46''@users.noreply.github.com>\n' "$wrong_id" \
  > "$TEST_ROOT/trailer.txt"
git -C "$TEST_ROOT" add trailer.txt

# The same handle with its own id passes, which is what keeps the rule from being a ban on
# the address rather than a check of the number in it.
git -C "$TEST_ROOT" rm -f -q trailer.txt
printf 'Co-authored-by: FabioSM46 <124870035+FabioSM46@users.noreply.github.com>\n' \
  > "$TEST_ROOT/correct.txt"
git -C "$TEST_ROOT" add correct.txt
(cd "$TEST_ROOT" && bash scripts/check-publication-privacy.sh >/dev/null)

echo "publication privacy — safe placeholders pass, private values fail without leaking, and a noreply address carrying another account's id is refused"
