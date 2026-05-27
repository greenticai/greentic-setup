#!/usr/bin/env bash
# Build a tiny local bundle around one .gtpack and launch the setup GUI.
#
# Usage:
#   ./scripts/test_provider.sh /path/to/provider.gtpack [bundle-dir]
#
# Environment:
#   GREENTIC_SETUP_BIN=/path/to/greentic-setup  Use a specific setup binary instead of cargo run.
#   USE_TARGET_BIN=1                            Use target/debug/greentic-setup if present.
#   TENANT=demo                                Setup tenant scope.
#   TEAM=default                               Setup team scope.
#   ENV=dev                                    Setup environment.
#   KEEP_BUNDLE=1                              Leave generated bundle on disk.

set -euo pipefail

cd "$(dirname "$0")/.."

usage() {
  cat <<'EOF'
Usage: ./scripts/test_provider.sh /path/to/provider.gtpack [bundle-dir]

Creates a temporary Greentic bundle, adds the local provider .gtpack, and
launches the setup GUI directly against that bundle.

Environment:
  GREENTIC_SETUP_BIN=/path/to/greentic-setup
  USE_TARGET_BIN=1
  TENANT=demo
  TEAM=default
  ENV=dev
  KEEP_BUNDLE=1
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage >&2
  exit 2
fi

PACK_PATH="$1"
if [[ ! -f "$PACK_PATH" ]]; then
  echo "Pack not found: $PACK_PATH" >&2
  exit 1
fi

case "$PACK_PATH" in
  *.gtpack) ;;
  *)
    echo "Expected a .gtpack file: $PACK_PATH" >&2
    exit 1
    ;;
esac

PACK_ABS="$(cd "$(dirname "$PACK_PATH")" && pwd)/$(basename "$PACK_PATH")"
TENANT="${TENANT:-demo}"
TEAM="${TEAM:-default}"
ENV="${ENV:-dev}"

if [[ $# -eq 2 ]]; then
  BUNDLE_DIR="$2"
  CLEANUP=false
else
  SLUG="$(basename "$PACK_PATH" .gtpack | tr -cs '[:alnum:]_.-' '-')"
  BUNDLE_DIR="/tmp/greentic-provider-${SLUG}-bundle"
  CLEANUP=true
fi

if [[ "${KEEP_BUNDLE:-}" == "1" ]]; then
  CLEANUP=false
fi

if [[ -n "${GREENTIC_SETUP_BIN:-}" ]]; then
  SETUP_CMD=("$GREENTIC_SETUP_BIN")
elif [[ "${USE_TARGET_BIN:-}" == "1" && -x target/debug/greentic-setup ]]; then
  SETUP_CMD=("target/debug/greentic-setup")
else
  SETUP_CMD=("cargo" "run" "--quiet" "--")
fi

if [[ "$CLEANUP" == "true" ]]; then
  trap 'rm -rf "$BUNDLE_DIR"' EXIT
fi

echo "Provider pack: $PACK_ABS"
echo "Bundle dir:    $BUNDLE_DIR"
echo "Scope:         tenant=$TENANT team=$TEAM env=$ENV"
echo

rm -rf "$BUNDLE_DIR"
"${SETUP_CMD[@]}" bundle init "$BUNDLE_DIR" --name "Provider Test"
"${SETUP_CMD[@]}" bundle add "$PACK_ABS" --bundle "$BUNDLE_DIR" --tenant "$TENANT" --team "$TEAM" --env "$ENV"

echo
echo "Launching setup GUI..."
echo "Command: ${SETUP_CMD[*]} --tenant $TENANT --team $TEAM --env $ENV $BUNDLE_DIR"
"${SETUP_CMD[@]}" --tenant "$TENANT" --team "$TEAM" --env "$ENV" "$BUNDLE_DIR"

if [[ "$CLEANUP" == "false" ]]; then
  echo
  echo "Bundle left at: $BUNDLE_DIR"
fi
