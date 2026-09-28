#!/usr/bin/env python3
"""Export controlled descentbot observations; never publish raw process output."""
import json
import os
from pathlib import Path
import re
import sys


def public_value(value):
    """Only scalar observations and controlled prose can leave the runner."""
    if value is None or isinstance(value, (bool, int)):
        return value
    if isinstance(value, str):
        if not re.fullmatch(r"[A-Za-z0-9 .,:;+>\[\]()-]*", value):
            raise ValueError("non-public prose in observations")
        return value
    if isinstance(value, list):
        return [public_value(item) for item in value]
    if isinstance(value, dict):
        if not all(re.fullmatch(r"[A-Za-z]+", key) for key in value):
            raise ValueError("unexpected observation key")
        return {key: public_value(item) for key, item in value.items()}
    raise ValueError("unexpected observation type")


def export(raw, destination, exit_code):
    source = os.environ["HOARD_SOURCE_COMMIT"]
    run_id = os.environ["GITHUB_RUN_ID"]
    attempt = os.environ["GITHUB_RUN_ATTEMPT"]
    server = os.environ["GITHUB_SERVER_URL"]
    repository = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[0-9a-f]{40}", source) or not run_id.isdecimal() or not attempt.isdecimal():
        raise ValueError("invalid public run identity")
    if server != "https://github.com" or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid public repository identity")
    records = [line.removeprefix("HOARD_EVIDENCE ") for line in raw.splitlines()
               if line.startswith("HOARD_EVIDENCE ")]
    observed = None
    status = "no-scenario-report"
    if len(records) == 1:
        try:
            observed = public_value(json.loads(records[0]))
            if not isinstance(observed, dict) or observed.get("Version") != 1 or not isinstance(observed.get("Success"), bool):
                raise ValueError("unsupported evidence")
            status = "reported"
        except (ValueError, TypeError):
            observed = None
            status = "rejected-scenario-report"
    elif records:
        status = "ambiguous-scenario-report"
    success = exit_code == 0 and observed is not None and observed["Success"] is True
    result = {
        "source_commit": source,
        "run_url": f"{server}/{repository}/actions/runs/{run_id}/attempts/{attempt}",
        "invocation": "voxelheim-descentbot -server <built-server> -party 3 -hoard -timeout 75m -seed 1",
        "fixtures": "Original iron sword and rusty armor bootstrap per member; one leader EnchantingTable after normal exit. No earned reagent or silver grants.",
        "travel_assists": "Existing portal and stuck placements; two post-exit overworld placements for table and capital forge. No dungeon skips or injected durability.",
        "exit_code": exit_code,
        "report_status": status,
        "success": success,
        "observations": observed,
        "raw_output": "Withheld: may contain private process paths, credentials or server logs.",
    }
    destination.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return success


if __name__ == "__main__":
    # The raw file stays in runner scratch; this function never prints its contents.
    raw_path, output_path, code = sys.argv[1:]
    passed = export(Path(raw_path).read_text(encoding="utf-8", errors="replace"), Path(output_path), int(code))
    sys.exit(0 if passed else 1)
