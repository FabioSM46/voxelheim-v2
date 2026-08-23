#!/usr/bin/env bash
# Keep the documented account store out of temporary storage (#229).
#
# The provider-to-account record is the only copy of the random AccountID used to
# derive character ownership. A README command that puts `-auth-dir` below `/tmp`
# therefore turns a reboot or age-based cleanup into permanent character loss.
# This guard reads the command people actually copy; it deliberately does not pin
# the replacement path, so another persistent location remains a documentation
# decision rather than a test rewrite.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
README="$REPO_ROOT/README.md"

python3 - "$README" <<'PY'
import re
import sys
from pathlib import Path


def temporary_auth_dir_lines(text: str) -> list[int]:
    """Return lines whose `-auth-dir` argument points below `/tmp`."""
    # A shell continuation is whitespace for this purpose. Normalising it keeps a
    # future two-line command from walking around the guard while preserving line
    # numbers closely enough to identify the command that must be fixed.
    normalised = re.sub(r"\\\r?\n", " ", text)
    pattern = re.compile(
        r"(?:^|[ \t])-auth-dir(?:[ \t]*=[ \t]*|[ \t]+)[\"']?"
        r"/tmp(?:/|(?=[\"' \t\r\n`]|$))",
        re.MULTILINE,
    )
    return [normalised.count("\n", 0, match.start()) + 1 for match in pattern.finditer(normalised)]


# Pin the guard's teeth as well as today's README. Both common flag spellings must
# fail, while the test remains agnostic about which persistent path replaces them.
assert temporary_auth_dir_lines("run -auth-dir /tmp/voxelheim-auth\n") == [1]
assert temporary_auth_dir_lines("run -auth-dir='/tmp/accounts'\n") == [1]
assert temporary_auth_dir_lines("run -auth-dir=/tmp/accounts\n") == [1]
assert not temporary_auth_dir_lines("run -auth-dir $PERSISTENT_DATA/accounts\n")

readme = Path(sys.argv[1]).read_text(encoding="utf-8")
unsafe = temporary_auth_dir_lines(readme)
if unsafe:
    joined = ", ".join(str(line) for line in unsafe)
    raise SystemExit(
        f"README.md points -auth-dir at temporary storage on line(s) {joined}; "
        "the account mapping must survive a reboot"
    )

print("account directory documentation — no -auth-dir command points below /tmp")
PY
