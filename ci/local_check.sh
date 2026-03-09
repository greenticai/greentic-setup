#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

step() {
  echo ""
  echo "==> $1"
  echo "------------------------------------------------------"
}

step "1/6  cargo fmt --check"
cargo fmt --all -- --check

step "2/6  cargo clippy"
cargo clippy --all-targets --all-features -- -D warnings

step "3/6  cargo test"
cargo test --all-features

step "4/6  cargo build"
cargo build --all-features

step "5/6  cargo doc"
cargo doc --no-deps --all-features

step "6/6  cargo package (dry-run)"
# Find publishable crates and run package + publish dry-run
for toml in $(find . -name Cargo.toml -not -path '*/target/*'); do
  if grep -q 'publish\s*=\s*false' "$toml" 2>/dev/null; then
    continue
  fi
  crate_name=$(grep '^name\s*=' "$toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
  if [ -z "$crate_name" ]; then
    continue
  fi
  echo "  packaging: $crate_name"
  cargo package -p "$crate_name" --allow-dirty
  cargo publish -p "$crate_name" --dry-run --allow-dirty
done

echo ""
echo "==> All checks passed."
