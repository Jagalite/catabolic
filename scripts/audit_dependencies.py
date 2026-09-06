# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Check the public release lock files against OSV; fail closed on API errors."""

import argparse
import json
import re
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def audit():
    packages = {}
    for path in sorted((ROOT / "requirements").glob("*.lock")):
        for name, version in re.findall(
            r"^([A-Za-z0-9_.-]+)==(\S+)", path.read_text(), re.M
        ):
            if name in packages and packages[name] != version:
                raise ValueError(f"conflicting release pins for {name}")
            packages[name] = version
    if not packages:
        raise ValueError("no release pins found")
    entries = [
        {"name": name, "version": version} for name, version in sorted(packages.items())
    ]
    queries = [
        {"package": {"name": p["name"], "ecosystem": "PyPI"}, "version": p["version"]}
        for p in entries
    ]
    request = urllib.request.Request(
        "https://api.osv.dev/v1/querybatch",
        data=json.dumps({"queries": queries}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        raw = response.read(4 * 1024 * 1024 + 1)
    if len(raw) > 4 * 1024 * 1024:
        raise ValueError("OSV response exceeded limit")
    results = json.loads(raw)["results"]
    for package, result in zip(entries, results, strict=True):
        advisories = result.get("vulns", [])
        if not isinstance(advisories, list) or any(
            not isinstance(v.get("id"), str) for v in advisories
        ):
            raise ValueError("invalid OSV result")
        package["advisories"] = advisories
    return {
        "checked_at": datetime.now(timezone.utc).isoformat(),
        "provider": "https://api.osv.dev/v1/querybatch",
        "packages": entries,
        "complete": True,
        "safe": not any(p["advisories"] for p in entries),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    try:
        report = audit()
    except Exception as exc:
        report = {"complete": False, "safe": False, "error": str(exc)}
    text = json.dumps(report, indent=2) + "\n"
    if args.report:
        args.report.write_text(text)
    print(text, end="")
    return 2 if not report["complete"] else 0 if report["safe"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
