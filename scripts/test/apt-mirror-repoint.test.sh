#!/usr/bin/env bash
# Pin the APT mirror repair every client job runs before installing Bevy's system
# dependencies, by executing it against the runner image's own mirrorlist.
#
# The step exists because the hosted runner's mirrorlist reaches for the Azure mirror
# first, and when that host stops answering every index fetch waits out its timeout.
# Its first version moved the host and kept the scheme, so the entry at priority 1
# became http://archive.ubuntu.com. When that port stopped answering from the runners
# (#1107), the dependency step timed out exactly as it had before the repair. https to
# the same host was degraded as well, and every Ubuntu entry in the file was that one
# host, so the step now also adds a second host at the lowest priority.
#
# A sed expression reads as correct whichever scheme it writes, and an append reads as
# idempotent whether or not it is. Only running the block on the image's file shows
# what it does.
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
import textwrap
from pathlib import Path

root = Path(sys.argv[1])
work = Path(sys.argv[2])

STEP = "Prefer the canonical Ubuntu archive over the Azure mirror"
MIRRORS = "/etc/apt/apt-mirrors.txt"
WORKFLOWS = ("ci.yml", "integration.yml", "client-cache.yml")
KERNEL = "https://mirrors.edge.kernel.org/ubuntu/\tpriority:4\n"


def exactly_one(pattern, text, label):
    matches = re.findall(pattern, text, flags=re.MULTILINE)
    if len(matches) != 1:
        raise AssertionError(f"expected one {label}, found {len(matches)}")
    return matches[0]


def run_block(name):
    text = (root / ".github/workflows" / name).read_text()
    step = exactly_one(
        rf"^      - name: {re.escape(STEP)}\s*$\n"
        r"([\s\S]*?)(?=^      - (?:name:|uses:)|^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)",
        text,
        f"{name} mirror step",
    )
    body = exactly_one(
        r"^        run: \|\s*$\n((?:^          .*\n?)+)", step, f"{name} mirror run block"
    )
    return textwrap.dedent(body)


# The three client jobs must repair the mirrorlist identically: a copy left behind is
# the same outage, deferred to whichever workflow still carries it.
blocks = {name: run_block(name) for name in WORKFLOWS}
assert len(set(blocks.values())) == 1, (
    f"every client job must repair the mirrors identically; got {blocks!r}"
)
block = blocks["ci.yml"]
assert "sudo sed -i" in block and "sudo tee -a" in block, (
    f"expected the repoint and the append in the run block; got {block!r}"
)


def repair(label, content):
    """Run the block as written, without privilege and with a fixture for the file."""
    path = work / f"{label}.txt"
    if content is not None:
        path.write_text(content)
    script = block.replace("sudo ", "").replace(MIRRORS, str(path))
    subprocess.run(["bash", "-e", "-c", script], check=True, capture_output=True)
    return path.read_text() if path.exists() else None


# The file configure-apt-sources.sh writes on the ubuntu images the runners boot.
IMAGE = (
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n"
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n"
)
repaired = repair("image", IMAGE)
assert repaired == (
    "https://archive.ubuntu.com/ubuntu/\tpriority:1\n"
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n" + KERNEL
), (
    "the image's mirrorlist must start on https://archive.ubuntu.com and end on the "
    f"kernel.org mirror at priority 4; got {repaired!r}"
)
assert "http://" not in repaired, (
    f"no plain-http mirror may survive the repair of the image's file; got {repaired!r}"
)
assert repair("again", repaired) == repaired, (
    "the repair must be idempotent: a second run may not append the mirror again"
)

# An https Azure entry moves too, so the scheme never depends on how the image wrote it.
assert repair("https-azure", "https://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n") == (
    "https://archive.ubuntu.com/ubuntu/\tpriority:1\n" + KERNEL
), "an https Azure entry must be repointed to https://archive.ubuntu.com"

# Repointed rather than deleted: two URIs on one line both move and none disappears.
assert repair(
    "one-line",
    "http://azure.archive.ubuntu.com/ubuntu/ http://azure.archive.ubuntu.com/ubuntu/\n",
) == "https://archive.ubuntu.com/ubuntu/ https://archive.ubuntu.com/ubuntu/\n" + KERNEL, (
    "every Azure URI on a line must be repointed"
)

# Matched on the Azure host alone: no other mirror is touched, whatever its scheme.
OTHERS = (
    "http://ports.ubuntu.com/ubuntu-ports/\tpriority:1\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:2\n"
    "http://archive.ubuntu.com/ubuntu/\tpriority:3\n"
)
assert repair("others", OTHERS) == OTHERS + KERNEL, (
    "mirrors other than Azure must be left alone"
)

# An image without a mirrorlist keeps its own sources: nothing is created.
assert repair("absent", None) is None, "a missing mirrorlist must not be created"

automation_job = exactly_one(
    r"^  automation:\s*$\n([\s\S]*?)(?=^  [A-Za-z_][A-Za-z0-9_-]*:\s*$|\Z)",
    (root / ".github/workflows/ci.yml").read_text(),
    "ci.yml automation job",
)
invocation = "bash scripts/test/apt-mirror-repoint.test.sh"
assert automation_job.count(invocation) == 1, (
    "automation job must execute the APT mirror repoint test exactly once"
)

print(
    f"apt mirror repoint — {len(WORKFLOWS)} client jobs agree, image mirrorlist starts "
    "on https and falls back to a second host"
)
PY
