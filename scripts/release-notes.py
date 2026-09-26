#!/usr/bin/env python3
"""Extract one reviewed changelog section from the checked-out release tree."""

import argparse
import json
from pathlib import Path
import re
import sys


def release_notes(text, version):
    version = version.removeprefix("v")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-(?:alpha|beta)(?:\.\d+)?)?", version):
        raise ValueError("Invalid release version")
    sections = []
    body = None
    fence = None
    comparison = None
    for line in text.splitlines(keepends=True):
        marker = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
        if marker:
            token = marker[1]
            if fence is None:
                fence = token
            elif (token[0] == fence[0] and len(token) >= len(fence)
                  and not line.strip().strip(token[0])):
                fence = None
            if body is not None:
                body.append(line)
            continue
        heading = None if fence else re.match(r"^## \[([^\]]+)\](?:\s+-\s+.*)?\s*$", line)
        if heading:
            body = [] if heading[1] == version else None
            if body is not None:
                sections.append(body)
        elif not fence and re.match(r"^\[[^\]]+\]:", line):
            body = None
            match = re.match(rf"^\[{re.escape(version)}\]:\s+(https://\S+)\s*$", line)
            if match:
                if comparison is not None:
                    raise ValueError(f"Duplicate changelog comparison link for {version}")
                comparison = match[1]
        elif body is not None:
            body.append(line)
    if fence is not None:
        raise ValueError("Unclosed changelog code fence")
    if len(sections) != 1:
        raise ValueError(f"Expected exactly one changelog section for {version}; found {len(sections)}")
    notes = "".join(sections[0]).strip()
    visible = re.sub(r"<!--.*?-->", "", notes, flags=re.S)
    if not any(re.search(r"\w", line) and not re.match(r"^\s*(?:#{1,6}(?:\s|$)|`{3,}|~{3,})", line)
               for line in visible.splitlines()):
        raise ValueError(f"Changelog section for {version} is empty")
    if comparison:
        notes += f"\n\n**Full changelog:** {comparison}"
    return notes + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?")
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    args = parser.parse_args()
    version = args.version or json.loads(Path("package.json").read_text())["version"]
    print(release_notes(args.changelog.read_text(encoding="utf-8"), version), end="")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        sys.exit(f"Release notes: {error}")
