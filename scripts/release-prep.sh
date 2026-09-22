#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release-prep.sh [--bump patch|minor|major]

Runs checks, bumps version (optional), and creates a release tag.
EOF
}

args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bump|--bump=*)
      args+=("$1")
      if [[ "$1" == "--bump" ]]; then
        shift
        if [[ $# -eq 0 ]]; then
          usage >&2
          exit 1
        fi
        args+=("$1")
      fi
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

scripts/env-check.sh
scripts/check.sh --no-tests
make coverage
./scripts/release-tag "${args[@]}"
