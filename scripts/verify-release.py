#!/usr/bin/env python3
"""Verify published release assets and registry packages without publishing anything."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from automation_http import fetch_bytes, fetch_json

PRODUCTION_REPO = "midhunmonachan/codex-profiles"
PLATFORMS = ("darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64")
TARGETS = (
    "aarch64-apple-darwin", "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu",
)
MAX_ASSET_BYTES = 64 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def expected_assets(version):
    assets = {f"codex-profiles-{target}.tar.gz": "release" for target in TARGETS}
    assets["codex-profiles-x86_64-pc-windows-msvc.exe.zip"] = "release"
    for name in ("codex-profiles", *(f"codex-profiles-{p}" for p in PLATFORMS)):
        assets[f"{name}-{version}.tgz"] = "npm-packages"
    assets[f"codex-profiles-{version}.crate"] = "cargo"
    assets["codex-profiles.rb"] = "homebrew"
    return assets


def gh(*args):
    result = subprocess.run(
        ["gh", *args], capture_output=True, text=True, timeout=120,
        env={**os.environ, "GH_PROMPT_DISABLED": "1"},
    )
    require(result.returncode == 0, f"GitHub CLI {' '.join(args[:2])} failed")
    return result.stdout


def github_json(endpoint):
    return json.loads(gh("api", "--hostname", "github.com", endpoint))


def resolve_tag(repo, tag):
    obj = github_json(f"repos/{repo}/git/ref/tags/{tag}")["object"]
    for _ in range(5):
        require(re.fullmatch(r"[0-9a-f]{40}", obj["sha"]), "Invalid tag object SHA")
        if obj["type"] == "commit":
            return obj["sha"]
        require(obj["type"] == "tag", "Release tag does not resolve to a commit")
        obj = github_json(f"repos/{repo}/git/tags/{obj['sha']}")["object"]
    raise ValueError("Release tag nesting exceeds limit")


def verify_assets(directory, release, repo, tag, commit):
    version = tag[1:]
    payloads = expected_assets(version)
    names = set(payloads) | {"SHA256SUMS", "release-manifest.json"}
    assets = release["assets"]
    require(len(assets) == len(names) and {a["name"] for a in assets} == names,
            "Published asset names are missing, duplicated, or unexpected")
    require({p.name for p in directory.iterdir()} == names, "Downloaded asset set differs")
    hashes = {}
    for asset in assets:
        path = directory / asset["name"]
        require(path.is_file() and not path.is_symlink(), "Asset is not a regular file")
        require(type(asset["size"]) is int and 0 < asset["size"] <= MAX_ASSET_BYTES,
                "Asset size exceeds the verification limit")
        require(path.stat().st_size == asset["size"], f"Asset size mismatch: {path.name}")
        data = path.read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        require(asset["digest"] == f"sha256:{digest}", f"GitHub digest mismatch: {path.name}")
        hashes[path.name] = digest

    checksums = {}
    for line in (directory / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([^/\\\s]+)", line)
        require(match is not None, "Malformed checksum entry")
        digest, name = match.groups()
        require(name not in checksums, "Duplicate checksum entry")
        checksums[name] = digest
    require(checksums == {name: hashes[name] for name in payloads},
            "Checksums do not match the complete payload set")

    manifest = json.loads((directory / "release-manifest.json").read_text(encoding="utf-8"))
    require(manifest["version"] == version and manifest["tag"] == tag,
            "Manifest release version mismatch")
    require(manifest["commit"] == commit, "Manifest commit differs from the release tag")
    require(manifest["repository"] == {"slug": repo, "url": f"https://github.com/{repo}"},
            "Manifest repository mismatch")
    entries = manifest["artifacts"]
    require(len(entries) == len(payloads), "Manifest artifact count mismatch")
    require({entry["path"] for entry in entries} == set(payloads), "Manifest asset set mismatch")
    for entry in entries:
        name = entry["path"]
        require(entry["sha256"] == hashes[name] and entry["category"] == payloads[name],
                f"Manifest artifact mismatch: {name}")


def verify_attestations(directory, repo, tag, commit):
    gh("release", "verify", tag, "--repo", f"github.com/{repo}")
    for path in sorted(directory.iterdir()):
        gh("attestation", "verify", str(path), "--hostname", "github.com", "--repo", repo,
           "--signer-workflow", f"{repo}/.github/workflows/release.yml",
           "--source-ref", f"refs/tags/{tag}", "--source-digest", commit,
           "--deny-self-hosted-runners")
        print(f"Verified attestation: {path.name}", flush=True)


def verify_registries(directory, repo, tag):
    if repo != PRODUCTION_REPO or "-" in tag:
        print("Registry verification skipped: registries publish only production stable releases.")
        return
    version = tag[1:]
    for name in ("codex-profiles", *(f"codex-profiles-{p}" for p in PLATFORMS)):
        metadata = fetch_json(f"https://registry.npmjs.org/{name}/{version}")
        require(metadata["name"] == name and metadata["version"] == version,
                f"npm version mismatch: {name}")
        data = (directory / f"{name}-{version}.tgz").read_bytes()
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode("ascii")
        require(metadata["dist"]["integrity"] == integrity, f"npm integrity mismatch: {name}")
        require(metadata["dist"]["attestations"]["provenance"]["predicateType"]
                == "https://slsa.dev/provenance/v1", f"npm provenance is missing: {name}")
        published = fetch_bytes(f"https://registry.npmjs.org/{name}/-/{name}-{version}.tgz")
        require(published == data, f"npm tarball differs from release asset: {name}")
        print(f"Verified npm package: {name}@{version}", flush=True)

    metadata = fetch_json(f"https://crates.io/api/v1/crates/codex-profiles/{version}")["version"]
    require(metadata["crate"] == "codex-profiles" and metadata["num"] == version
            and metadata["yanked"] is False, "Crate version is missing, wrong, or yanked")
    data = (directory / f"codex-profiles-{version}.crate").read_bytes()
    require(metadata["checksum"] == hashlib.sha256(data).hexdigest(), "Crate checksum mismatch")
    published = fetch_bytes(f"https://static.crates.io/crates/codex-profiles/codex-profiles-{version}.crate")
    require(published == data, "Registry crate differs from release asset")
    print(f"Verified crate: codex-profiles@{version}", flush=True)


def verify_release(repo, tag, expected_commit=None):
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*", repo),
            "Invalid GitHub repository")
    require(re.fullmatch(r"v\d+\.\d+\.\d+(?:-(?:alpha|beta)(?:\.\d+)?)?", tag),
            "Invalid release tag")
    require(not expected_commit or re.fullmatch(r"[0-9a-f]{40}", expected_commit),
            "Invalid expected commit")
    commit = resolve_tag(repo, tag)
    require(not expected_commit or commit == expected_commit, "Release tag commit mismatch")
    release = github_json(f"repos/{repo}/releases/tags/{tag}")
    require(release["tag_name"] == tag and release["draft"] is False, "Release is not published")
    require(release["prerelease"] == ("-" in tag), "Release prerelease flag mismatch")
    require(release.get("immutable") is True, "Release is not immutable")
    names = set(expected_assets(tag[1:])) | {"SHA256SUMS", "release-manifest.json"}
    require(len(release["assets"]) == len(names)
            and {a["name"] for a in release["assets"]} == names, "Unexpected published asset set")
    require(all(type(a["size"]) is int and 0 < a["size"] <= MAX_ASSET_BYTES for a in release["assets"]),
            "Asset size exceeds the verification limit")
    with tempfile.TemporaryDirectory(prefix="codex-profiles-verify-") as temporary:
        directory = Path(temporary)
        gh("release", "download", tag, "--repo", f"github.com/{repo}", "--dir", str(directory))
        verify_assets(directory, release, repo, tag, commit)
        verify_attestations(directory, repo, tag, commit)
        verify_registries(directory, repo, tag)
    print(f"Verified {len(names)} assets for {repo} {tag} at {commit}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--repo", default=PRODUCTION_REPO)
    parser.add_argument("--expected-commit")
    args = parser.parse_args()
    verify_release(args.repo, args.tag, args.expected_commit)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired) as error:
        sys.exit(f"Release verification failed: {error}")
