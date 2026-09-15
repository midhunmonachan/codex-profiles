#!/usr/bin/env python3
"""Test the release-tag helper in an isolated, dependency-free git repository."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

repo = Path(__file__).resolve().parent.parent
(repo / "target").mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="release-tag-test-", dir=repo / "target") as directory:
    work = Path(directory)

    def run(*args):
        return subprocess.check_output(args, cwd=work, text=True, stderr=subprocess.STDOUT)

    (work / "src").mkdir()
    (work / "src/main.rs").write_text("fn main() {}\n")
    (work / "Cargo.toml").write_text('[package]\nname = "codex-profiles"\nversion = "1.2.3"\nedition = "2024"\n')
    (work / "package.json").write_text(json.dumps({
        "name": "codex-profiles", "version": "1.2.3",
        "optionalDependencies": {"codex-profiles-linux-x64": "1.2.3"}}))
    shutil.copy2(repo / "scripts/release-tag", work / "release-tag")
    run("cargo", "generate-lockfile", "--offline")
    run("git", "init", "-q")
    run("git", "config", "user.name", "Release helper test")
    run("git", "config", "user.email", "test@example.invalid")
    run("git", "config", "commit.gpgsign", "false")
    run("git", "config", "tag.gpgsign", "false")
    run("git", "add", ".")
    run("git", "commit", "-qm", "Test fixture")
    run("bash", "release-tag", "--bump", "patch")
    manifest = tomllib.loads((work / "Cargo.toml").read_text())
    lock = tomllib.loads((work / "Cargo.lock").read_text())
    npm = json.loads((work / "package.json").read_text())
    assert manifest["package"]["version"] == lock["package"][0]["version"] == npm["version"] == "1.2.4"
    assert set(npm["optionalDependencies"].values()) == {"1.2.4"}
    assert not run("git", "status", "--porcelain").strip()
    assert run("git", "rev-parse", "v1.2.4^{commit}") == run("git", "rev-parse", "HEAD")
    run("cargo", "metadata", "--locked", "--offline", "--format-version", "1")
print("PASS: release helper synchronizes Cargo, lockfile, npm, and tag")
