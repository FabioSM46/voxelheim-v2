#!/usr/bin/env bash
# Pin "the client test suite writes nothing into the developer's data directory" (#230).
#
# The leak was not a test doing something exotic. `client/src/net/session.rs` fell back to
# the real process environment whenever no caller named a data directory, and the tests
# that stand up a loopback server on port 0 drive that same session — so every run left one
# more `127.0.0.1_<ephemeral-port>` file behind in `$XDG_DATA_HOME/voxelheim/characters`,
# under a path the client treats as the player's own. On the machine the bug was filed from
# there were 1769 of them. Nothing was ever wrong enough to turn anything red.
#
# The fix is a compile-time one, so the pin below is mostly a compile-time one too:
# `Environment::read` is `#[cfg(not(test))]`, which means a test build has no way to ask
# what `$XDG_DATA_HOME` is, and the fallback it gets instead names nowhere at all. That is
# what makes the *next* test correct without anybody remembering — and it is why the
# structural half of this file is worth more than the dynamic half, rather than being a
# stand-in for it. A redirected-environment run can only report on the tests that exist
# today; the `#[cfg]` answers for the ones nobody has written.
#
# **This test cannot build the client, and that is worth stating rather than hiding.** The
# `automation` job has no Rust toolchain, no Bevy system dependencies, no warm cargo cache
# and a ten-minute budget; building Bevy inside it would spend the whole budget duplicating
# what the `client` job already does. So the suite is run only where it is already built —
# a developer's worktree after the client gate — and skipped, out loud, everywhere else.
#
# Deliberately absent: anything that deletes anything. The 1769 files this issue reports
# are a one-off for a human to remove by hand. A script that tidied
# `$XDG_DATA_HOME/voxelheim/characters` would be deleting a player's remembered characters
# on the strength of a guess about which of them a test wrote, which is a worse bug than
# the one being fixed.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

# An absent workspace is "nothing to verify", never an error — the same rule every other
# script and CI job in this repository keeps while the scaffolds are still arriving.
if [ ! -f client/Cargo.toml ] || [ ! -f client/src/net/session.rs ]; then
  echo "client workspace not scaffolded — nothing to verify"
  exit 0
fi

# ── 1. The structural half: the real environment is unreachable from a test build ──────
python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
client_src = root / "client/src"
session = (client_src / "net/session.rs").read_text()


def strip_doc_comments(text):
    """Drop `///` and `//` lines so prose about a call is never read as one.

    Every claim below is about code. This file's own header would otherwise satisfy
    several of them, which is the failure mode a scan for a string always has."""
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("//")
    )


# The guard itself. Attributes sit between the doc comment and the item, so the check is
# "the nearest attribute above `fn read`", not "somewhere in the file".
read_decl = re.search(
    r"((?:^\s*#\[[^\]]*\]\s*$\n)*)^\s*pub\(super\) fn read\(\)",
    session,
    flags=re.MULTILINE,
)
assert read_decl, "client/src/net/session.rs no longer declares `Environment::read`"
assert "cfg(not(test))" in read_decl.group(1), (
    "`Environment::read` must carry #[cfg(not(test))]. Without it a test build can ask "
    "for the developer's real $XDG_DATA_HOME, which is how #230 happened: the tests that "
    "bind 127.0.0.1:0 drive the same session a player's launch does, and it wrote one "
    "file per ephemeral port into a directory no test owns."
)

# Exactly one caller, and it is the shipped half of the fallback. Two would mean a second
# path into the real data directory, and this is the check that says so before a reviewer
# has to notice it.
callers = []
for path in sorted(client_src.rglob("*.rs")):
    # flatc output is regenerated, never hand-edited, and knows nothing about any of this.
    if "gen" in path.relative_to(client_src).parts:
        continue
    code = strip_doc_comments(path.read_text())
    if "Environment::read()" in code:
        callers.append(str(path.relative_to(root)))

assert callers == ["client/src/net/session.rs"], (
    "`Environment::read()` must be called from exactly one place — the shipped half of "
    f"`default_environment` in client/src/net/session.rs. Called from: {callers}"
)

code = strip_doc_comments(session)
shipped = re.search(
    r"#\[cfg\(not\(test\)\)\]\s*\npub\(super\) fn default_environment\(\) -> Environment \{"
    r"\s*\n\s*Environment::read\(\)\s*\n\}",
    code,
)
assert shipped, (
    "the shipped fallback must be `#[cfg(not(test))] fn default_environment()` returning "
    "`Environment::read()` — the one place the process environment is read"
)

# The test-build half. `Environment::default()` names neither XDG_DATA_HOME nor HOME, so
# `data_home` answers None and every per-server path derived from it is None: a session a
# test forgot to give a directory writes nothing, nowhere. A temporary directory here
# would pass this test and still leave the suite writing files somebody has to think about.
under_test = re.search(
    r"#\[cfg\(test\)\]\s*\npub\(super\) fn default_environment\(\) -> Environment \{"
    r"\s*\n\s*Environment::default\(\)\s*\n\}",
    code,
)
assert under_test, (
    "the test-build fallback must be `#[cfg(test)] fn default_environment()` returning "
    "`Environment::default()`, which names no data directory at all"
)

# The fallback is what `run` reaches for, rather than the environment directly.
assert re.search(r"None => default_environment\(\),", code), (
    "the session's `data_home` fallback must go through `default_environment()`"
)

# And nothing anywhere else in the client asks the process where a home directory is.
# Both names are read exactly once, in the one function a test build cannot call; a second
# reader anywhere would be a second way back into the developer's directory.
reads = {}
for path in sorted(client_src.rglob("*.rs")):
    if "gen" in path.relative_to(client_src).parts:
        continue
    body = strip_doc_comments(path.read_text())
    for name in ("XDG_DATA_HOME", "HOME"):
        found = re.findall(rf"env::var\(\s*(?:\"{name}\"|{name})\s*\)", body)
        if found:
            reads.setdefault(str(path.relative_to(root)), {})[name] = len(found)

assert reads == {"client/src/net/session.rs": {"XDG_DATA_HOME": 1, "HOME": 1}}, (
    "the data directory must be read in exactly one place — `Environment::read`, which a "
    f"test build cannot call. Found: {reads}"
)

print("structural pin: a test build cannot name the developer's data directory")
PY

# ── 2. The dynamic half: run the suite, and let it prove it ────────────────────────────
# Only where it is already built. See the header for why the automation job is not that
# place, and why this skips rather than pretending.
TARGET_DIR=${CARGO_TARGET_DIR:-client/target}
if ! command -v cargo >/dev/null 2>&1; then
  echo "skipped the suite run: no cargo on PATH (the client job is where it is compiled)"
  exit 0
fi
if [ ! -d "$TARGET_DIR" ]; then
  echo "skipped the suite run: the client is not built here — run the client gate first"
  exit 0
fi

WORK=$(mktemp -d)
# Removes only what this script created, and nothing it was pointed at.
cleanup() {
  if [ -n "${WORK:-}" ] && [ -d "$WORK" ]; then
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

# The count of the developer's own directory, taken before and compared after. It is never
# printed and never touched: this is the number the issue's reproduction steps ask for, and
# the whole property is that it does not move.
REAL_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
real_entries() {
  if [ -d "$REAL_DATA_HOME/voxelheim/characters" ]; then
    find "$REAL_DATA_HOME/voxelheim/characters" -mindepth 1 | wc -l
  else
    echo 0
  fi
}
REAL_BEFORE=$(real_entries)

# The pattern `scripts/interop-check.sh` already uses for the real client: a data home the
# harness owns, inside a directory it removes.
mkdir -p "$WORK/clientdata"
(cd client && XDG_DATA_HOME="$WORK/clientdata" cargo test --workspace --locked) >"$WORK/suite.log" 2>&1 || {
  tail -30 "$WORK/suite.log"
  echo "FAIL: the client suite does not pass with XDG_DATA_HOME pointed elsewhere" >&2
  exit 1
}

WROTE=$(find "$WORK/clientdata" -mindepth 1 | wc -l)
if [ "$WROTE" -ne 0 ]; then
  find "$WORK/clientdata" -mindepth 1 | sed -n '1,10p' >&2
  echo "FAIL: the client suite wrote $WROTE entries under \$XDG_DATA_HOME. A test must \
write inside a directory it owns; with the environment redirected these would have been \
files in the developer's own data directory." >&2
  exit 1
fi

REAL_AFTER=$(real_entries)
if [ "$REAL_BEFORE" -ne "$REAL_AFTER" ]; then
  echo "FAIL: the real data directory gained $((REAL_AFTER - REAL_BEFORE)) entries during \
the suite, despite \$XDG_DATA_HOME pointing elsewhere — something reaches the home \
directory without going through it." >&2
  exit 1
fi

PASSED=$(grep -c '^test result: ok\.' "$WORK/suite.log" || true)
echo "suite run under a redirected data home: nothing written, ${PASSED} target(s) green"
