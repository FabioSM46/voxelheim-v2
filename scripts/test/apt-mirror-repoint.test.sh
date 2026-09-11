#!/usr/bin/env bash
# Pin the APT mirror repoint every client job runs before installing Bevy's system
# dependencies, by executing it against the runner image's own mirrorlist.
#
# The step exists because the hosted runner's mirrorlist reaches for the Azure mirror
# first, and when that host stops answering every index fetch waits out its timeout.
# Its first version moved the host and kept the scheme, so the entry at priority 1
# became http://archive.ubuntu.com. When that port stopped answering from the runners
# (#1107), the dependency step it guards timed out exactly as it had before the repair,
# while https://archive.ubuntu.com answered one line below. A sed expression reads as
# correct either way; only running it on the image's file says which one it is.
#
# Run: bash scripts/test/apt-mirror-repoint.test.sh

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

python3 - "$REPO_ROOT" "$WORK" <<'PY'
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
work = Path(sys.argv[2])

STEP = "Prefer the canonical Ubuntu archive over the Azure mirror"
MIRRORS = "/etc/apt/apt-mirrors.txt"
WORKFLOWS = ("ci.yml", "integration.yml", "client-cache.yml")


def exactly_one(pattern, text, label):
    matches = re.findall(pattern, text, flags=re.MULTILINE)
    if len(matches) != 1:
        raise AssertionError(f"expected one {label}, found {len(matches)}")
    return matches[0]


def repoint_command(name):
    text = (root / ".github/workflows" / name).read_text()
    step = exactly_one(
        rf"^      - name: {re.escape(STEP)}\s*$\n"
        r"([\s\S]*?)(?=^      - (?:name:|uses:)|^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)",
        text,
        f"{name} mirror step",
    )
    return exactly_one(r"^\s*(sudo sed -i [^\n]*?)\s*$", step, f"{name} sed command")


# The three client jobs must repair the mirrorlist identically: a copy left on the old
# expression is the same outage, deferred to whichever workflow still carries it.
commands = {name: repoint_command(name) for name in WORKFLOWS}
assert len(set(commands.values())) == 1, (
    f"every client job must repoint the mirrors identically; got {commands!r}"
)
command = commands["ci.yml"]
assert command.startswith("sudo ") and command.endswith(" " + MIRRORS), (
    f"expected `sudo ... {MIRRORS}`, got {command!r}"
)
# Executed as written, minus the privilege and with a fixture in place of the file.
local = command[len("sudo "):-len(MIRRORS)]


def repoint(label, content):
    path = work / f"{label}.txt"
    path.write_text(content)
    subprocess.run(["bash", "-c", local + '"$1"', "repoint", str(path)], check=True)
    return path.read_text()


# The file configure-apt-sources.sh writes on the ubuntu images the runners boot.
IMAGE = (
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n"
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n"
)
repaired = repoint("image", IMAGE)
assert repaired == (
    "https://archive.ubuntu.com/ubuntu/\tpriority:1\n"
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n"
), f"the image's mirrorlist must start on https://archive.ubuntu.com; got {repaired!r}"
assert "http://" not in repaired, (
    f"no plain-http mirror may survive the repair of the image's file; got {repaired!r}"
)
assert repoint("again", repaired) == repaired, "the repair must be idempotent"

# An https Azure entry moves too, so the scheme never depends on how the image wrote it.
assert repoint("https-azure", "https://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n") == (
    "https://archive.ubuntu.com/ubuntu/\tpriority:1\n"
), "an https Azure entry must be repointed to https://archive.ubuntu.com"

# Repointed rather than deleted: two URIs on one line both move and none disappears.
assert repoint(
    "one-line",
    "http://azure.archive.ubuntu.com/ubuntu/ http://azure.archive.ubuntu.com/ubuntu/\n",
) == "https://archive.ubuntu.com/ubuntu/ https://archive.ubuntu.com/ubuntu/\n", (
    "every Azure URI on a line must be repointed"
)

# Matched on the Azure host alone: no other mirror is touched, whatever its scheme.
OTHERS = (
    "http://ports.ubuntu.com/ubuntu-ports/\tpriority:1\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:2\n"
    "http://archive.ubuntu.com/ubuntu/\tpriority:3\n"
)
assert repoint("others", OTHERS) == OTHERS, "mirrors other than Azure must be left alone"

automation_job = exactly_one(
    r"^  automation:\s*$\n([\s\S]*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)",
    (root / ".github/workflows/ci.yml").read_text(),
    "ci.yml automation job",
)
invocation = "bash scripts/test/apt-mirror-repoint.test.sh"
assert automation_job.count(invocation) == 1, (
    "automation job must execute the APT mirror repoint test exactly once"
)

print(f"apt mirror repoint — {len(WORKFLOWS)} client jobs agree, image mirrorlist starts on https")
PY
