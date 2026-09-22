#!/usr/bin/env python3
"""Test release completeness, archive contents, and checksum verification."""

from hashlib import sha256
import io
import json
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile
import zipfile


repo = Path(__file__).resolve().parent.parent
version = json.loads((repo / "package.json").read_text(encoding="utf-8"))["version"]
targets = {
    "aarch64-apple-darwin": "codex-profiles-darwin-arm64",
    "aarch64-unknown-linux-gnu": "codex-profiles-linux-arm64",
    "x86_64-apple-darwin": "codex-profiles-darwin-x64",
    "x86_64-pc-windows-msvc": "codex-profiles-win32-x64",
    "x86_64-unknown-linux-gnu": "codex-profiles-linux-x64",
}


def clear_directory(path: Path) -> None:
    for child in path.iterdir():
        if child.is_dir():
            shutil.rmtree(child)
        else:
            child.unlink()


def write_tar(path: Path, mode: int, extra: bool = False, symlink: bool = False) -> None:
    with tarfile.open(path, "w:gz") as archive:
        member = tarfile.TarInfo("codex-profiles")
        member.mode = mode
        if symlink:
            member.type = tarfile.SYMTYPE
            member.linkname = "somewhere-else"
            archive.addfile(member)
        else:
            payload = b"synthetic executable\n"
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))
        if extra:
            extra_member = tarfile.TarInfo("unexpected")
            extra_payload = b"unexpected\n"
            extra_member.size = len(extra_payload)
            archive.addfile(extra_member, io.BytesIO(extra_payload))


def write_zip(path: Path, extra: bool = False, symlink: bool = False) -> None:
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        member = zipfile.ZipInfo("codex-profiles.exe")
        if symlink:
            member.external_attr = (stat.S_IFLNK | 0o777) << 16
            archive.writestr(member, b"somewhere-else")
        else:
            archive.writestr(member, b"synthetic executable\n")
        if extra:
            archive.writestr("unexpected", b"unexpected\n")


def build_fixture(
    out: Path,
    *,
    mode: int = 0o755,
    tar_extra: bool = False,
    tar_symlink: bool = False,
    zip_extra: bool = False,
    zip_symlink: bool = False,
) -> None:
    clear_directory(out)
    artifacts = out / "artifacts"
    release = out / "release"
    npm_packages = out / "npm-packages"
    homebrew = out / "homebrew"
    cargo = out / "cargo"
    checksums = out / "checksums"
    for directory in (artifacts, release, npm_packages, homebrew, cargo, checksums):
        directory.mkdir(parents=True)

    generated = []
    for target, package_name in targets.items():
        artifact_dir = artifacts / f"codex-profiles-{target}"
        artifact_dir.mkdir()
        binary_name = "codex-profiles.exe" if "windows" in target else "codex-profiles"
        (artifact_dir / binary_name).write_bytes(b"synthetic executable\n")
        if "windows" in target:
            archive = release / f"codex-profiles-{target}.exe.zip"
            write_zip(archive, extra=zip_extra, symlink=zip_symlink)
        else:
            archive = release / f"codex-profiles-{target}.tar.gz"
            write_tar(archive, mode, extra=tar_extra, symlink=tar_symlink)
        generated.append(archive)

        package = npm_packages / f"{package_name}-{version}.tgz"
        package.write_bytes(f"synthetic {package_name}\n".encode())
        generated.append(package)

    main_package = npm_packages / f"codex-profiles-{version}.tgz"
    main_package.write_bytes(b"synthetic main package\n")
    generated.append(main_package)

    crate = cargo / f"codex-profiles-{version}.crate"
    crate.write_bytes(b"synthetic cargo crate\n")
    generated.append(crate)

    cask = homebrew / "codex-profiles.rb"
    cask.write_text('cask "codex-profiles" do\nend\n', encoding="utf-8")
    generated.append(cask)

    digests = {path.name: sha256(path.read_bytes()).hexdigest() for path in generated}
    (checksums / "SHA256SUMS").write_text(
        "".join(f"{digest}  {name}\n" for name, digest in digests.items()),
        encoding="utf-8",
    )
    manifest = {
        "version": version,
        "tag": f"v{version}",
        "repository": {
            "slug": "midhunmonachan/codex-profiles",
            "url": "https://github.com/midhunmonachan/codex-profiles",
        },
        "commit": "synthetic",
        "tools": {"python": "test"},
        "provenance": {
            "github_release": "https://example.invalid/release",
            "verification_guide": "https://example.invalid/verification",
            "github_attestations": True,
            "npm_provenance": True,
        },
        "artifacts": [
            {"path": name, "sha256": digest, "category": "synthetic"}
            for name, digest in digests.items()
        ],
    }
    (checksums / "release-manifest.json").write_text(
        json.dumps(manifest) + "\n", encoding="utf-8"
    )


def verify(out: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(repo / "scripts/verify-artifacts.sh"), version, str(out)],
        capture_output=True,
        text=True,
    )


def assert_pass(out: Path) -> None:
    result = verify(out)
    assert result.returncode == 0, result.stderr


def assert_fail(out: Path, message: str) -> None:
    result = verify(out)
    assert result.returncode != 0, "invalid release fixture unexpectedly passed"
    output = result.stdout + result.stderr
    assert message in output, output


target = repo / "target"
target.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="release-artifacts-test-", dir=target) as directory:
    out = Path(directory)

    build_fixture(out)
    assert_pass(out)

    # A changed file must fail even if SHA256SUMS and the manifest were not changed.
    archive = out / "release" / "codex-profiles-x86_64-unknown-linux-gnu.tar.gz"
    archive.write_bytes(archive.read_bytes() + b"tampered")
    assert_fail(out, "Checksum mismatch")

    build_fixture(out, mode=0o644)
    assert_fail(out, "not executable")

    build_fixture(out, tar_extra=True)
    assert_fail(out, "must contain only codex-profiles")

    build_fixture(out, tar_symlink=True)
    assert_fail(out, "not a regular file")

    build_fixture(out, zip_extra=True)
    assert_fail(out, "must contain only codex-profiles.exe")

    build_fixture(out, zip_symlink=True)
    assert_fail(out, "not a regular file")

    build_fixture(out)
    extra = out / "release" / "unexpected-release-file"
    extra.write_bytes(b"missing checksum\n")
    assert_fail(out, "missing or duplicated in checksums")

    build_fixture(out)
    checksums = out / "checksums" / "SHA256SUMS"
    first_line = checksums.read_text(encoding="utf-8").splitlines(keepends=True)[0]
    checksums.write_text(
        checksums.read_text(encoding="utf-8") + first_line,
        encoding="utf-8",
    )
    assert_fail(out, "missing or duplicated in checksums")

    build_fixture(out)
    manifest_path = out / "checksums" / "release-manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["artifacts"].append(manifest["artifacts"][0])
    manifest_path.write_text(json.dumps(manifest) + "\n", encoding="utf-8")
    assert_fail(out, "duplicate artifact path")

    build_fixture(out)
    missing_package = (
        out / "npm-packages" / f"{targets['x86_64-unknown-linux-gnu']}-{version}.tgz"
    )
    missing_package.unlink()
    assert_fail(out, "Missing npm platform package")

print("PASS: release completeness, archive permissions/types, and checksum verification")
