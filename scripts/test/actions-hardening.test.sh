#!/usr/bin/env bash
# Keep GitHub Actions immutable and prevent checkout credentials from reaching
# build or test commands. Repository settings enforce the same SHA rule after
# this change reaches develop; this test makes the rule travel with the source.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)

python3 - "$REPO_ROOT" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
workflow_dir = root / ".github" / "workflows"
workflows = sorted(workflow_dir.glob("*.yml")) + sorted(workflow_dir.glob("*.yaml"))
assert workflows, "no GitHub Actions workflows found"

use_re = re.compile(
    r"^(?P<indent>\s*)(?:-\s+)?uses:\s+"
    r"(?P<action>[^@\s#]+)@(?P<ref>[^\s#]+)"
    r"(?:\s+#\s*(?P<label>\S+))?\s*$"
)
sha_re = re.compile(r"[0-9a-f]{40}")
label_re = re.compile(r"(?:v?[0-9]+(?:\.[0-9]+)*)")

remote_uses = 0
checkout_uses = 0
rust_toolchain_uses = []

for path in workflows:
    text = path.read_text()
    lines = text.splitlines()

    assert re.search(r"^permissions:\s*$", text, re.MULTILINE), (
        f"{path.relative_to(root)} must declare top-level permissions explicitly"
    )
    assert not re.search(r"^permissions:\s*write-all\s*$", text, re.MULTILINE), (
        f"{path.relative_to(root)} grants write-all"
    )

    for index, line in enumerate(lines):
        match = use_re.match(line)
        if not match:
            continue
        action = match.group("action")
        if action.startswith(("./", "docker://")):
            continue

        remote_uses += 1
        ref = match.group("ref")
        label = match.group("label")
        assert sha_re.fullmatch(ref), (
            f"{path.relative_to(root)}:{index + 1}: {action} must use a full 40-character SHA, "
            f"got {ref!r}"
        )
        assert label and label_re.fullmatch(label), (
            f"{path.relative_to(root)}:{index + 1}: pinned actions need a trailing release label "
            "such as `# v4` or `# 1.97.1` for manual update auditing"
        )

        if action == "dtolnay/rust-toolchain":
            rust_toolchain_uses.append((ref, label))

        if action != "actions/checkout":
            continue

        checkout_uses += 1
        step_indent = len(match.group("indent"))
        body = []
        for following in lines[index + 1 :]:
            if re.match(rf"^\s{{{step_indent}}}-\s+(?:name:|uses:)", following):
                break
            body.append(following)
        assert any(re.match(r"^\s+persist-credentials:\s*false\s*$", item) for item in body), (
            f"{path.relative_to(root)}:{index + 1}: checkout must set persist-credentials: false"
        )

assert remote_uses > 0, "no remote action references were inspected"
assert checkout_uses > 0, "no checkout steps were inspected"
assert rust_toolchain_uses, "no dtolnay/rust-toolchain references were inspected"

toolchain = (root / "client" / "rust-toolchain.toml").read_text()
channel_match = re.search(r'^channel\s*=\s*"([^"]+)"\s*$', toolchain, re.MULTILINE)
assert channel_match, "client/rust-toolchain.toml must declare an exact channel"
channel = channel_match.group(1)
assert {label for _, label in rust_toolchain_uses} == {channel}, (
    "every dtolnay/rust-toolchain release label must match client/rust-toolchain.toml"
)
assert len({ref for ref, _ in rust_toolchain_uses}) == 1, (
    "every workflow must pin the same dtolnay/rust-toolchain commit"
)

ci = (workflow_dir / "ci.yml").read_text()
assert re.search(
    r"^permissions:\s*$\n(?:^[ \t].*\n)*?^  contents:\s*read\s*$", ci, re.MULTILINE
), "ci.yml must default GITHUB_TOKEN to contents: read"

invocation = "bash scripts/test/actions-hardening.test.sh"
assert ci.count(invocation) == 1, "CI must run the Actions hardening test exactly once"

print(
    f"actions hardening — {remote_uses} remote uses pinned, "
    f"{checkout_uses} checkout steps drop credentials"
)
PY
