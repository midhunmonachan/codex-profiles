#!/usr/bin/env python3
"""Exercise built binaries or npm tarballs before publishing. Uses no real credentials."""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib


def check(command, version, home):
    env = dict(os.environ, CODEX_HOME=str(home), CODEX_PROFILES_SKIP_UPDATE="1",
               CODEX_PROFILES_COMMAND="codex-profiles", NO_COLOR="1")

    def run(args, success=True):
        result = subprocess.run(command + args, env=env, input="", capture_output=True,
                                text=True, timeout=30)
        assert (result.returncode == 0) == success, f"Unexpected exit for {args}: {result}"
        if success:
            assert not result.stderr, f"Unexpected stderr for {args}: {result.stderr}"
        else:
            assert result.stderr and not result.stdout, f"Wrong error stream for {args}: {result}"
        return result.stdout

    # These commands must work even if CODEX_HOME does not exist.
    for flag in ("--version", "-V"):
        assert run([flag]).strip() == f"codex-profiles {version}"
    assert "Usage:" in run([])
    for flag in ("--help", "-h"):
        assert "Usage:" in run([flag])
    commands = [(name,) for name in ("save", "load", "list", "export", "import",
                                     "doctor", "label", "status", "delete")]
    commands += [("label", name) for name in ("set", "clear", "rename")]
    for parts in commands:
        assert "Usage:" in run([*parts, "--help"])
    for args in (["--not-a-flag"], ["not-a-command"], ["load", "--label"]):
        run(args, success=False)
    assert not home.exists(), "Information commands created profile storage"

    home.mkdir()
    (home / "config.toml").write_text('cli_auth_credentials_store = "file"\n')

    def mutation(args):
        value = json.loads(run([*args, "--json"]))
        assert value["success"] is True and value["command"] == args[0]
        return value["profile"]

    for label in ("alpha", "beta"):
        (home / "auth.json").write_text(json.dumps({
            "auth_mode": "apikey", "OPENAI_API_KEY": f"sk-synthetic-smoke-{label}"}))
        mutation(["save", "--label", label])
    profiles = json.loads(run(["list", "--json"]))
    assert profiles, "Missing saved profiles"
    loaded = mutation(["load", "--label", "alpha", "--with-status"])
    assert loaded["label"] == "alpha" and loaded["status"]["is_api_key"] is True
    assert json.loads((home / "auth.json").read_text())["OPENAI_API_KEY"] == "sk-synthetic-smoke-alpha"
    assert json.loads(run(["status", "--json"]))["is_api_key"] is True
    before = (home / "auth.json").read_bytes()
    run(["load", "--label", "missing", "--json"], success=False)
    assert (home / "auth.json").read_bytes() == before
    mutation(["delete", "--label", "beta", "--yes"])


def unpack(package, destination):
    with tarfile.open(package) as archive:
        for member in archive.getmembers():
            if member.isdir():
                continue
            assert member.isfile() and member.name.startswith("package/"), member.name
            target = (destination / member.name[len("package/"):]).resolve()
            assert target.is_relative_to(destination.resolve()), member.name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.extractfile(member).read())
            target.chmod(member.mode & 0o777)


def main():
    repo = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--binary", type=Path)
    group.add_argument("--npm-packages", type=Path)
    parser.add_argument("--expected-version")
    args = parser.parse_args()
    version = args.expected_version or tomllib.loads((repo / "Cargo.toml").read_text())["package"]["version"]
    target = repo / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="release-smoke-", dir=target) as work:
        work = Path(work)
        if args.binary:
            command = [str(args.binary.resolve())]
        else:
            os_name = {"win32": "win32", "linux": "linux", "darwin": "darwin"}[sys.platform]
            arch = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64", "arm64": "arm64"}[platform.machine().lower()]
            native = f"codex-profiles-{os_name}-{arch}"
            modules = work / "node_modules"
            for name in ("codex-profiles", native):
                unpack(args.npm_packages / f"{name}-{version}.tgz", modules / name)
                metadata = json.loads((modules / name / "package.json").read_text())
                assert metadata["version"] == version
            metadata = json.loads((modules / "codex-profiles/package.json").read_text())
            assert metadata["optionalDependencies"][native] == version
            node = shutil.which("node")
            assert node, "Node.js is required for the npm smoke test"
            command = [node, str(modules / "codex-profiles/bin/codex-profiles.js")]
        check(command, version, work / "codex-home")
    print("PASS: version/help exit codes, argument errors, JSON output, and profile switching")


if __name__ == "__main__":
    main()
