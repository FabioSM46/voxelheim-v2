#!/usr/bin/env bash
# Pin docs/ADDING_AN_ITEM.md to the tree it describes.
#
# That document is a *path* rather than a second copy of the rules: every rule it names is
# enforced somewhere else, and it points at the enforcing copy. That shape is only honest
# while the pointers resolve. A checklist whose file paths have quietly moved is worse than
# no checklist, because somebody follows it and concludes the step does not exist.
#
# So this pins the three things that rot on their own — and deliberately nothing else. It
# does NOT check that the prose is true: no test can, and pretending otherwise would be the
# `Diff: 3 chars` mistake this repository has already paid for once, a diagnostic that
# cannot vary and therefore cannot look wrong.
#
#   1. every repository-relative path the document cites exists
#   2. every test it tells the reader to look for still exists by that name
#   3. every registry column and named symbol it lists still appears where it says
#
# The paths are matched only when they contain a directory separator and end in a known
# extension, which is what keeps a bare `items.go` in running prose out of the check.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
doc_path = root / "docs/ADDING_AN_ITEM.md"
doc = doc_path.read_text()
failures = []


def present(where, failures):
    """The file at `where`, or None once the absence has been judged.

    "An absent workspace is nothing to verify" is this repository's rule, and it is a rule
    about a *workspace* — `server/`, `client/`, `schemas/` — which may legitimately not be
    scaffolded yet. Applying it to an individual file turns a rename into a silent pass,
    which is the rot this script exists to catch happening inside the catcher. So the skip
    is decided by the workspace root and the file's own absence is a failure.
    """
    target = root / where
    if target.exists():
        return target
    if (root / Path(where).parts[0]).exists():
        failures.append(f"{where} is cited by this pin but no longer exists")
    return None

# 1 — cited paths resolve.
cited = sorted(set(re.findall(r"`([A-Za-z0-9_.\-]+(?:/[A-Za-z0-9_.\-]+)+\.(?:go|rs|md|yml|fbs|sh|toml))`", doc)))
if not cited:
    failures.append("the document cites no repository paths at all — the extractor is broken")
for path in cited:
    if not (root / path).exists():
        failures.append(f"cited path does not exist: {path}")

# 2 — the tests it names are still called that. Named rather than extracted, because the
# document's list is a claim about which tests catch which omission and a regex over the
# prose would only ever agree with itself.
TESTS = {
    "TestEveryItemIsRegisteredWithItsOwnStackLimitAndPlacement": "server/internal/game/items_test.go",
    "every_known_item_has_a_name_a_shape_and_a_colour": "client/src/player/items.rs",
    "the_registry_names_every_item_id_this_client_declares": "client/src/player/items.rs",
    "every_shape_has_a_drawing_of_its_own": "client/src/ui/icon.rs",
}
for name, where in TESTS.items():
    if name not in doc:
        failures.append(f"the document no longer names the test {name}; update this pin with it")
        continue
    if not (target := present(where, failures)):
        continue
    if name not in target.read_text():
        failures.append(f"{name} is named by the document but not found in {where}")

# 3 — the registry columns and the symbols the path walks through.
SYMBOLS = {
    "server/internal/game/items.go": [
        "places", "maxStack", "wornAt", "maxDurability", "meleeDamage",
        "repairRestore", "restoresHunger", "launches", "ammunition",
        "itemRegistry", "blockDrops",
    ],
    "server/internal/game/inventory.go": ["starterSlots"],
    "server/internal/game/species.go": ["lootRoll"],
    "client/src/player/items.rs": ["ItemShape", "ItemColour"],
    "client/src/player/combat.rs": ["LEFT_BUTTON_USES"],
}
for where, symbols in SYMBOLS.items():
    if not (target := present(where, failures)):
        continue
    text = target.read_text()
    for symbol in symbols:
        if symbol not in doc:
            failures.append(f"the document no longer mentions {symbol}; update this pin with it")
        elif symbol not in text:
            failures.append(f"{symbol} is named by the document but not found in {where}")

if failures:
    print("docs/ADDING_AN_ITEM.md has stale pointers:", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)

print(f"adding-an-item doc — {len(cited)} cited paths resolve, "
      f"{len(TESTS)} named tests exist, every registry column and symbol is where it says")
PY
