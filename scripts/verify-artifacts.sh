#!/usr/bin/env bash
set -euo pipefail

version="${1:-}"
out_dir="${2:-dist}"

if [[ -z "${version}" ]]; then
  version=$(python3 - <<'PY'
import json
with open("package.json", "r", encoding="utf-8") as fh:
    print(json.load(fh)["version"])
PY
  )
fi

version="${version#v}"

required_targets=(
  aarch64-apple-darwin
  aarch64-unknown-linux-gnu
  x86_64-apple-darwin
  x86_64-pc-windows-msvc
  x86_64-unknown-linux-gnu
)

release_dir="${out_dir}/release"
npm_packages_dir="${out_dir}/npm-packages"
homebrew_dir="${out_dir}/homebrew"
cargo_dir="${out_dir}/cargo"
checksums_file="${out_dir}/checksums/SHA256SUMS"
manifest_file="${out_dir}/checksums/release-manifest.json"

if [[ ! -d "${release_dir}" ]]; then
  echo "Missing release dir: ${release_dir}" >&2
  exit 1
fi

if [[ ! -d "${npm_packages_dir}" ]]; then
  echo "Missing npm packages dir: ${npm_packages_dir}" >&2
  exit 1
fi

if [[ ! -d "${cargo_dir}" ]]; then
  echo "Missing cargo dir: ${cargo_dir}" >&2
  exit 1
fi

if [[ ! -f "${checksums_file}" ]]; then
  echo "Missing checksums file: ${checksums_file}" >&2
  exit 1
fi

for target in "${required_targets[@]}"; do
  artifact_dir="${out_dir}/artifacts/codex-profiles-${target}"
  if [[ ! -d "${artifact_dir}" ]]; then
    echo "Missing build artifact directory: ${artifact_dir}" >&2
    exit 1
  fi
  if [[ "${target}" == *windows* ]]; then
    expected="${release_dir}/codex-profiles-${target}.exe.zip"
  else
    expected="${release_dir}/codex-profiles-${target}.tar.gz"
  fi
  if [[ ! -f "${expected}" ]]; then
    echo "Missing release asset: ${expected}" >&2
    exit 1
  fi
done

shopt -s nullglob
for artifact_dir in "${out_dir}/artifacts"/codex-profiles-*; do
  target="${artifact_dir##*/codex-profiles-}"
  known=0
  for required_target in "${required_targets[@]}"; do
    [[ "${target}" == "${required_target}" ]] && known=1
  done
  if [[ "${known}" -eq 0 ]]; then
    echo "Unsupported build artifact target: ${target}" >&2
    exit 1
  fi
done
shopt -u nullglob

main_pkg="${npm_packages_dir}/codex-profiles-${version}.tgz"
if [[ ! -f "${main_pkg}" ]]; then
  echo "Missing npm main package: ${main_pkg}" >&2
  exit 1
fi

crate="${cargo_dir}/codex-profiles-${version}.crate"
if [[ ! -f "${crate}" ]]; then
  echo "Missing cargo crate: ${crate}" >&2
  exit 1
fi

for package_name in \
  codex-profiles-darwin-arm64 \
  codex-profiles-darwin-x64 \
  codex-profiles-linux-arm64 \
  codex-profiles-linux-x64 \
  codex-profiles-win32-x64; do
  package_path="${npm_packages_dir}/${package_name}-${version}.tgz"
  if [[ ! -f "${package_path}" ]]; then
    echo "Missing npm platform package: ${package_path}" >&2
    exit 1
  fi
done

if [[ ! -f "${homebrew_dir}/codex-profiles.rb" ]]; then
  echo "Missing Homebrew cask: ${homebrew_dir}/codex-profiles.rb" >&2
  exit 1
fi

if [[ ! -s "${checksums_file}" ]]; then
  echo "Checksums file is empty: ${checksums_file}" >&2
  exit 1
fi

if [[ ! -f "${manifest_file}" ]]; then
  echo "Missing release manifest: ${manifest_file}" >&2
  exit 1
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "Missing sha256sum/shasum" >&2
    exit 1
  fi
}

# Resolve each checksum entry to a generated artifact and independently hash
# that file. Comparing only SHA256SUMS with the manifest would let a corrupted
# artifact pass verification when both metadata files still agree.
while read -r expected path; do
  [[ -n "${expected}" && -n "${path}" ]] || continue
  if [[ "${path}" == */* || "${path}" == .* ]]; then
    echo "Checksum entry must name an artifact basename: ${path}" >&2
    exit 1
  fi
  artifact=""
  for directory in "${release_dir}" "${npm_packages_dir}" "${cargo_dir}" "${homebrew_dir}"; do
    candidate="${directory}/${path}"
    if [[ -f "${candidate}" ]]; then
      if [[ -n "${artifact}" ]]; then
        echo "Ambiguous checksum artifact path: ${path}" >&2
        exit 1
      fi
      artifact="${candidate}"
    fi
  done
  if [[ -z "${artifact}" ]]; then
    echo "Checksum entry has no matching artifact: ${path}" >&2
    exit 1
  fi
  actual="$(sha256_file "${artifact}")"
  if [[ "${expected}" != "${actual}" ]]; then
    echo "Checksum mismatch for ${path}: expected ${expected}, got ${actual}" >&2
    exit 1
  fi
done < "${checksums_file}"

for directory in "${release_dir}" "${npm_packages_dir}" "${cargo_dir}" "${homebrew_dir}"; do
  for artifact in "${directory}"/*; do
    [[ -f "${artifact}" ]] || continue
    path="$(basename "${artifact}")"
    count="$(awk -v name="${path}" '$2 == name { count += 1 } END { print count + 0 }' "${checksums_file}")"
    if [[ "${count}" -ne 1 ]]; then
      echo "Generated artifact is missing or duplicated in checksums: ${path}" >&2
      exit 1
    fi
  done
done

# Validate exact archive contents, regular-file types, and executable mode in
# Unix release archives. The release smoke test runs before upload/download, so
# this also catches mode-bit loss in GitHub artifact transport.
python3 - <<'PY' "${release_dir}"
import sys
import tarfile
import zipfile
from pathlib import Path

release_dir = Path(sys.argv[1])
required_targets = {
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
}
for target in required_targets:
    archive = release_dir / f"codex-profiles-{target}.tar.gz"
    try:
        with tarfile.open(archive, "r:gz") as handle:
            members = handle.getmembers()
    except (OSError, tarfile.TarError) as error:
        raise SystemExit(f"Invalid release archive: {archive}: {error}")
    if len(members) != 1 or members[0].name != "codex-profiles":
        raise SystemExit(f"Release archive must contain only codex-profiles: {archive}")
    member = members[0]
    if not member.isfile():
        raise SystemExit(f"Release archive member is not a regular file: {archive}")
    if not member.mode & 0o100:
        raise SystemExit(f"Release archive binary is not executable: {archive}")

archive = release_dir / "codex-profiles-x86_64-pc-windows-msvc.exe.zip"
try:
    with zipfile.ZipFile(archive) as handle:
        members = handle.infolist()
except (OSError, zipfile.BadZipFile) as error:
    raise SystemExit(f"Invalid release archive: {archive}: {error}")
if len(members) != 1 or members[0].filename != "codex-profiles.exe" or members[0].is_dir():
    raise SystemExit(f"Release archive must contain only codex-profiles.exe: {archive}")
member = members[0]
unix_mode = (member.external_attr >> 16) & 0xffff
file_type = unix_mode & 0o170000
if file_type not in (0, 0o100000):
    raise SystemExit(f"Release archive member is not a regular file: {archive}")
PY

python3 - <<'PY' "${version}" "${checksums_file}" "${manifest_file}"
import json
import sys

version, checksums_path, manifest_path = sys.argv[1:]

expected = {}
with open(checksums_path, "r", encoding="utf-8") as fh:
    for line in fh:
        line = line.strip()
        if not line:
            continue
        sha256, path = line.split("  ", 1)
        expected[path] = sha256

with open(manifest_path, "r", encoding="utf-8") as fh:
    manifest = json.load(fh)

if manifest.get("version") != version:
    raise SystemExit(
        f"Manifest version mismatch: {manifest.get('version')} != {version}"
    )

if manifest.get("tag") != f"v{version}":
    raise SystemExit(
        f"Manifest tag mismatch: {manifest.get('tag')} != v{version}"
    )

repository = manifest.get("repository")
if not isinstance(repository, dict) or not repository.get("slug") or not repository.get("url"):
    raise SystemExit("Manifest repository field must include slug and url")

if "commit" not in manifest:
    raise SystemExit("Manifest commit field is missing")

tools = manifest.get("tools")
if not isinstance(tools, dict) or not tools:
    raise SystemExit("Manifest tools field must be a non-empty object")

provenance = manifest.get("provenance")
if not isinstance(provenance, dict):
    raise SystemExit("Manifest provenance field must be an object")

for key in ("github_release", "verification_guide", "github_attestations", "npm_provenance"):
    if key not in provenance:
        raise SystemExit(f"Manifest provenance field is missing {key}")

artifacts = manifest.get("artifacts")
if not isinstance(artifacts, list):
    raise SystemExit("Manifest artifacts field must be a list")

observed = {}
for artifact in artifacts:
    path = artifact.get("path")
    sha256 = artifact.get("sha256")
    if not path or not sha256:
        raise SystemExit("Manifest artifact entries must include path and sha256")
    if path in observed:
        raise SystemExit(f"Manifest contains duplicate artifact path: {path}")
    observed[path] = sha256

if observed != expected:
    raise SystemExit("Manifest artifacts do not match SHA256SUMS")
PY
