#!/usr/bin/env python3
"""Report changes to reviewed Codex source paths without changing the baseline."""

import argparse
from datetime import date
import json
from pathlib import Path, PurePosixPath
import re
import sys

from automation_http import fetch_json

REPO = "openai/codex"
API = f"https://api.github.com/repos/{REPO}"
WEB = f"https://github.com/{REPO}"
DEFAULT_BASELINE = Path(__file__).resolve().parents[1] / ".github/codex-compatibility.json"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def validate_baseline(baseline):
    require(baseline["schema_version"] == 1 and baseline["repository"] == REPO
            and baseline["branch"] == "main", "Unsupported compatibility baseline")
    require(re.fullmatch(r"[0-9a-f]{40}", baseline["reviewed_commit"]), "Invalid reviewed commit")
    date.fromisoformat(baseline["reviewed_on"])
    paths = baseline["paths"]
    require(isinstance(paths, list) and paths, "Baseline paths must be a nonempty list")
    for path in paths:
        require(isinstance(path, str) and re.fullmatch(r"[A-Za-z0-9_./-]+", path)
                and not path.startswith("/") and ".." not in PurePosixPath(path).parts
                and str(PurePosixPath(path)) == path, "Invalid monitored path")
    require(len(set(paths)) == len(paths), "Duplicate monitored paths")


def read_tree(commit, paths):
    response = fetch_json(f"{API}/git/trees/{commit}?recursive=1")
    require(isinstance(response, dict) and response.get("truncated") is False
            and isinstance(response.get("tree"), list),
            "Upstream tree is truncated or malformed")
    entries = {}
    seen = set()
    for entry in response["tree"]:
        require(isinstance(entry, dict) and isinstance(entry.get("path"), str),
                "Malformed upstream tree entry")
        path = entry["path"]
        require(path not in seen, "Duplicate upstream tree path")
        seen.add(path)
        if path in paths:
            require(entry.get("type") in ("blob", "tree", "commit")
                    and re.fullmatch(r"[0-7]{6}", entry.get("mode", ""))
                    and re.fullmatch(r"[0-9a-f]{40}", entry.get("sha", "")),
                    "Malformed monitored tree entry")
            entries[path] = {key: entry[key] for key in ("type", "mode", "sha")}
    return entries


def check_compatibility(baseline):
    validate_baseline(baseline)
    head = fetch_json(f"{API}/commits/main")["sha"]
    require(re.fullmatch(r"[0-9a-f]{40}", head), "Invalid upstream commit")
    paths = baseline["paths"]
    reviewed = baseline["reviewed_commit"]
    before = read_tree(reviewed, paths)
    after = read_tree(head, paths)
    results = []
    for path in paths:
        previous = before.get(path)
        require(previous is not None and previous["type"] == "blob"
                and previous["mode"] in ("100644", "100755"),
                f"Reviewed path is not a regular source file: {path}")
        current = after.get(path)
        status = "unchanged" if current == previous else "changed"
        if current is None:
            status = "removed_or_moved"
        results.append({"path": path, "status": status, "reviewed": previous, "observed": current})
    return {
        "status": "review_required" if any(p["status"] != "unchanged" for p in results) else "unchanged",
        "repository": REPO,
        "reviewed_commit": reviewed,
        "reviewed_on": baseline["reviewed_on"],
        "observed_commit": head,
        "compare_url": f"{WEB}/compare/{reviewed}...{head}",
        "paths": results,
    }


def markdown_report(report):
    lines = ["# Codex compatibility monitor", "", f"Status: **{report['status']}**", ""]
    if report["status"] == "incomplete":
        lines.extend(["The check could not complete; no compatibility conclusion is available.",
                      "", report["error"]])
    else:
        lines.extend([
            f"Reviewed: [`{report['reviewed_commit']}`]({WEB}/tree/{report['reviewed_commit']})",
            f"Observed: [`{report['observed_commit']}`]({WEB}/tree/{report['observed_commit']})", "",
            "| Monitored source | Result |", "| --- | --- |",
        ])
        for entry in report["paths"]:
            url = f"{WEB}/blob/{report['observed_commit']}/{entry['path']}"
            lines.append(f"| [`{entry['path']}`]({url}) | {entry['status']} |")
        lines.extend(["", f"[Review upstream changes]({report['compare_url']}).", "",
                      "Changed bytes require review; they do not establish incompatibility. "
                      "Unchanged monitored paths do not establish overall compatibility. "
                      "The reviewed baseline has not been updated."])
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        report = check_compatibility(json.loads(args.baseline.read_text(encoding="utf-8")))
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        report = {"status": "incomplete", "error": str(error)}
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    summary = markdown_report(report)
    (args.output_dir / "report.md").write_text(summary, encoding="utf-8")
    print(summary, end="")
    return int(report["status"] == "incomplete")


if __name__ == "__main__":
    sys.exit(main())
