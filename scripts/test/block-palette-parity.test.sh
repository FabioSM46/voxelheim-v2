#!/usr/bin/env bash
# Pin the block palette's two halves to each other.
#
# The server decides what a block *is* — `server/internal/world/chunk.go` — and the client
# decides what it *looks like* and whether the mesher may cull against it —
# `client/src/world/palette.rs`. Nothing compiles both, so nothing checked that they agreed,
# and #680 added eight blocks at once, which is exactly when two hand-kept tables drift.
#
# **What makes this cheap is that solidity is not a third table on either side.** Both
# spell it the same way:
#
#     server:  Solid(b)     = b != Air && !IsWater(b) && !Cover(b)
#     client:  is_solid(b)  = b != AIR && !is_water(b) && !is_cover(b)
#     client:  is_opaque(b) = b != AIR && !is_water(b) && !is_shaped(b)
#
# So pinning the **ids**, the **water family** and the **cover family** pins solidity and
# opacity with them: a block cannot be solid on the server and walk-through on the client
# unless one of those three disagrees, and all three are checked below. That is the whole
# design of this test — it deliberately does not try to evaluate two languages' predicates.
#
# It checks:
#
#   1. every server id has a client constant with the same number and the same name
#   2. and the reverse, so a client id the server never declared fails too
#   3. the ids are contiguous from zero, which is what an insertion or a hole looks like
#   4. the water families name the same ids on both sides
#   5. the cover families name the same ids on both sides
#   6. every non-air id has a colour arm, so no block silently draws as UNKNOWN magenta
#   7. every block `schematicLegend` can write is a block the client can draw
#
# An absent workspace is nothing to verify — the repository's standing rule — and here that
# is decided by the workspace root rather than by the file, so a rename is a failure.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

if [ ! -d "$REPO_ROOT/server" ] || [ ! -d "$REPO_ROOT/client" ]; then
  echo "block-palette-parity: server/ or client/ is not scaffolded — nothing to verify"
  exit 0
fi

python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
failures = []


def read(rel):
    path = root / rel
    if not path.exists():
        failures.append(f"{rel} is pinned by this test but no longer exists")
        return ""
    return path.read_text()


chunk = read("server/internal/world/chunk.go")
schematic = read("server/internal/world/schematic.go")
palette = read("client/src/world/palette.rs")
if failures:
    print("\n".join(f"FAIL: {f}" for f in failures))
    sys.exit(1)

# --- the two id tables -------------------------------------------------------------
server_ids = {name: int(num) for name, num in re.findall(r"^\t(\w+)\s+Block = (\d+)$", chunk, re.M)}
client_ids = {name: int(num) for name, num in re.findall(r"^pub const (\w+): BlockId = (\d+);$", palette, re.M)}

if len(server_ids) < 10:
    failures.append(f"only {len(server_ids)} server ids were extracted — the extractor is broken")
if len(client_ids) < 10:
    failures.append(f"only {len(client_ids)} client ids were extracted — the extractor is broken")


def screaming(name):
    """A name in the one form both conventions agree on: letters and digits, upper case.

    **Underscore placement is not a fact about the wire, and pinning it would be pinning
    a taste.** Go writes `WaterCurrentXNeg` and Rust writes `WATER_CURRENT_XNEG`, where a
    mechanical CamelCase split would ask for `WATER_CURRENT_X_NEG` — neither spelling is
    wrong and neither is what a client draws. Dropping the separators keeps the check
    on the thing that matters: the same id declared under the same word on both sides.
    """
    return re.sub(r"[^A-Za-z0-9]", "", name).upper()


client_by_word = {}
for name, number in client_ids.items():
    client_by_word.setdefault(screaming(name), []).append((name, number))

for name, number in sorted(server_ids.items()):
    matches = client_by_word.get(screaming(name))
    if not matches:
        failures.append(f"server block {name} = {number} has no client constant")
    elif len(matches) > 1:
        failures.append(f"{name} matches more than one client constant: {matches}")
    elif matches[0][1] != number:
        failures.append(f"{name} is {number} on the server and {matches[0][1]} on the client")

mirrored = {screaming(name) for name in server_ids}
for name, number in sorted(client_ids.items()):
    if screaming(name) not in mirrored:
        failures.append(f"client block {name} = {number} is not declared by the server")

# --- contiguity: an insertion or a reused number looks like a hole ------------------
numbers = sorted(server_ids.values())
if numbers != list(range(len(numbers))):
    failures.append(f"server ids are not contiguous from 0: {numbers}")

# --- the water family, read from the predicate rather than from a naming convention --
water_fn = re.search(r"func IsWater\(b Block\) bool \{\n\treturn ([^\n]+)\n\}", chunk)
if not water_fn:
    failures.append("could not read the server's IsWater — the extractor is broken")
    server_water = set()
else:
    body = water_fn.group(1)
    server_water = {server_ids[n] for n in re.findall(r"b == (\w+)", body) if n in server_ids}
    for lo, hi in re.findall(r"b >= (\w+) && b <= (\w+)", body):
        if lo in server_ids and hi in server_ids:
            server_water |= set(range(server_ids[lo], server_ids[hi] + 1))

client_water_block = re.search(r"const WATER_FAMILY: \[BlockId; \d+\] = \[([^\]]*)\];", palette)
client_water = set()
if not client_water_block:
    failures.append("could not read the client's WATER_FAMILY — the extractor is broken")
else:
    for name in re.findall(r"\w+", client_water_block.group(1)):
        if name in client_ids:
            client_water.add(client_ids[name])
        else:
            failures.append(f"WATER_FAMILY names {name}, which is not a client block id")

if server_water != client_water:
    failures.append(
        f"the water families differ: server-only {sorted(server_water - client_water)}, "
        f"client-only {sorted(client_water - server_water)}"
    )

# --- the cover family ---------------------------------------------------------------
# **The body may be wrapped over several lines and this must not care.** How a Go
# function body is broken across lines is gofmt's decision, not the author's: this
# return was one line while Cover named four ids and became two the moment #874 added
# a fifth and a sixth. An extractor anchored to `[^\n]+` stopped matching at that
# exact commit, and because it fails closed it said so — which is the only reason the
# breakage was visible at all rather than silently reporting an empty server family.
cover_fn = re.search(r"func Cover\(b Block\) bool \{\n\treturn (.+?)\n\}", chunk, re.S)
if not cover_fn:
    failures.append("could not read the server's Cover — the extractor is broken")
    server_cover = set()
else:
    server_cover = {server_ids[n] for n in re.findall(r"b == (\w+)", cover_fn.group(1)) if n in server_ids}

client_cover_block = re.search(r"const COVER_FAMILY: \[BlockId; \d+\] = \[([^\]]*)\];", palette)
client_cover = set()
if not client_cover_block:
    failures.append("could not read the client's COVER_FAMILY — the extractor is broken")
else:
    for name in re.findall(r"\w+", client_cover_block.group(1)):
        if name in client_ids:
            client_cover.add(client_ids[name])
        else:
            failures.append(f"COVER_FAMILY names {name}, which is not a client block id")

if server_cover != client_cover:
    failures.append(
        f"the cover families differ: server-only {sorted(server_cover - client_cover)}, "
        f"client-only {sorted(client_cover - server_cover)}"
    )

# --- every id a server sends has a colour -------------------------------------------
rgba = re.search(r"pub fn linear_rgba\(block: BlockId\) -> \[f32; 4\] \{(.*?)\n\}", palette, re.S)
if not rgba:
    failures.append("could not read linear_rgba — the extractor is broken")
else:
    coloured = set(re.findall(r"^\s+(\w+) => \w+_LINEAR,$", rgba.group(1), re.M))
    for name, number in sorted(client_ids.items()):
        if number == 0 or number in client_water:
            continue  # air is never meshed; the whole water family is answered above the match
        if name not in coloured:
            failures.append(f"{name} has no colour arm — it would draw as UNKNOWN magenta")

# --- every block a drawing can write is a block the client can draw ------------------
legend = re.search(r"var schematicLegend = map\[rune\]Block\{(.*?)\n\}", schematic, re.S)
if not legend:
    failures.append("could not read schematicLegend — the extractor is broken")
else:
    named = re.findall(r"'(?:\\.|[^'])' *: *(\w+),", legend.group(1))
    if len(named) < 5:
        failures.append(f"only {len(named)} legend runes were extracted — the extractor is broken")
    for block in named:
        if block in ("keepTerrain", "Air"):
            continue  # neither is a material: one is "leave it alone", the other is a room
        if block not in server_ids:
            failures.append(f"the legend writes {block}, which is not a block id")
        elif screaming(block) not in client_by_word:
            failures.append(f"the legend writes {block}, which the client cannot draw")

if failures:
    print("\n".join(f"FAIL: {f}" for f in failures))
    sys.exit(1)

print(
    f"block palette parity — {len(server_ids)} ids mirrored, "
    f"{len(server_water)} water, {len(server_cover)} cover, legend clean"
)
PY
