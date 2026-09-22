#!/usr/bin/env bash
set -euo pipefail

run_audit=1
run_tests=1

usage() {
  cat <<'EOF'
Usage: scripts/check.sh [--no-audit] [--no-tests]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-audit)
      run_audit=0
      shift
      ;;
    --no-tests)
      run_tests=0
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

if [[ "${run_audit}" -eq 1 ]] && ! command -v cargo-audit >/dev/null 2>&1; then
  echo "Missing required tool: cargo-audit (install it or run scripts/check.sh --no-audit)" >&2
  exit 1
fi

bash install.sh --help >/dev/null
cargo fetch --locked
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
if [[ "${run_tests}" -eq 1 ]]; then
  if command -v cargo-nextest >/dev/null 2>&1; then
    cargo nextest run --tests --locked
  else
    cargo test --tests --locked
  fi
fi
if [[ "${run_audit}" -eq 1 ]]; then
  cargo audit
fi
