#!/usr/bin/env bash
# Build a tiny local bundle around one .gtpack and launch the setup GUI.
#
# Usage:
#   ./scripts/test_provider.sh [--no-ui] /path/to/provider.gtpack [bundle-dir]
#
# Environment:
#   GREENTIC_SETUP_BIN=/path/to/greentic-setup  Use a specific setup binary instead of cargo run.
#   USE_TARGET_BIN=1                            Use target/debug/greentic-setup if present.
#   GREENTIC_SETUP_RUNTIME_URL=http://127.0.0.1:<port>
#                                               Runtime base URL for provider-owned setup APIs.
#   GREENTIC_SETUP_RUNTIME_PORT=8793            Local setup runtime port when auto-starting one.
#   GREENTIC_SETUP_USE_PYTHON_TESTER=1          Opt into legacy provider Python tester fallback.
#   TENANT=demo                                Setup tenant scope.
#   TEAM=default                               Setup team scope.
#   ENV=dev                                    Setup environment.
#   KEEP_BUNDLE=1                              Leave generated bundle on disk.

set -euo pipefail

cd "$(dirname "$0")/.."

usage() {
  cat <<'EOF'
Usage: ./scripts/test_provider.sh [--no-ui] /path/to/provider.gtpack [bundle-dir]

Creates a temporary Greentic bundle, adds the local provider .gtpack, and
runs setup directly against that bundle. By default this launches the setup
GUI; pass --no-ui to run the non-UI setup flow. For provider setup web
components, --no-ui starts the setup server without opening a browser, prints
the URL to open manually, and waits for the wizard to finish successfully.

Environment:
  GREENTIC_SETUP_BIN=/path/to/greentic-setup
  USE_TARGET_BIN=1
  GREENTIC_SETUP_RUNTIME_URL=http://127.0.0.1:<port>
  GREENTIC_SETUP_RUNTIME_PORT=8793
  GREENTIC_SETUP_USE_PYTHON_TESTER=1
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

NO_UI=false
POSITIONAL=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-ui)
      NO_UI=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      while [[ $# -gt 0 ]]; do
        POSITIONAL+=("$1")
        shift
      done
      ;;
    -*)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      POSITIONAL+=("$1")
      shift
      ;;
  esac
done

if [[ ${#POSITIONAL[@]} -lt 1 || ${#POSITIONAL[@]} -gt 2 ]]; then
  usage >&2
  exit 2
fi

PACK_PATH="${POSITIONAL[0]}"
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

PACK_HAS_SETUP_WEB_COMPONENT=false
SETUP_ROUTES_JSON="$(unzip -p "$PACK_ABS" assets/setup.routes.json 2>/dev/null || true)"
if printf '%s\n' "$SETUP_ROUTES_JSON" | grep -q '"schema_id"[[:space:]]*:[[:space:]]*"greentic.setup.web-component.v1"'; then
  PACK_HAS_SETUP_WEB_COMPONENT=true
fi
PACK_HAS_SETUP_BACKEND_CONTRACT=false
SETUP_BACKEND_CONTRACT_JSON="$(unzip -p "$PACK_ABS" assets/setup/backend-contract.json 2>/dev/null || true)"
if printf '%s\n' "$SETUP_BACKEND_CONTRACT_JSON" | grep -q '"schema_id"[[:space:]]*:[[:space:]]*"greentic.setup.backend-contract.v1"'; then
  PACK_HAS_SETUP_BACKEND_CONTRACT=true
fi
PACK_NEEDS_SETUP_RUNTIME=false
if printf '%s\n' "$SETUP_BACKEND_CONTRACT_JSON" \
  | grep -Eq '"host_capability"[[:space:]]*:|"registration_url_source"[[:space:]]*:[[:space:]]*"host_runtime"|"kind"[[:space:]]*:[[:space:]]*"bot_framework_registration"|"kind"[[:space:]]*:[[:space:]]*"runtime_observation"'; then
  PACK_NEEDS_SETUP_RUNTIME=true
fi
SETUP_PROVIDER_ID="$(
  printf '%s\n' "$SETUP_ROUTES_JSON" \
    | sed -n 's/.*"provider_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n 1
)"

if [[ ${#POSITIONAL[@]} -eq 2 ]]; then
  BUNDLE_DIR="${POSITIONAL[1]}"
  CLEANUP=false
else
  SLUG="$(basename "$PACK_PATH" .gtpack | tr -cs '[:alnum:]_.-' '-')"
  BUNDLE_DIR="/tmp/greentic-provider-${SLUG}-bundle"
  CLEANUP=true
fi

if [[ "${KEEP_BUNDLE:-}" == "1" ]]; then
  CLEANUP=false
fi

if [[ "$PACK_HAS_SETUP_WEB_COMPONENT" == "true" ]]; then
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
  :
fi

SETUP_RUNTIME_PID=""
SETUP_UI_PID=""
cleanup() {
  if [[ -n "${SETUP_UI_PID:-}" ]]; then
    kill "$SETUP_UI_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "${SETUP_RUNTIME_PID:-}" ]]; then
    kill "$SETUP_RUNTIME_PID" >/dev/null 2>&1 || true
  fi
  if [[ "$CLEANUP" == "true" ]]; then
    rm -rf "$BUNDLE_DIR"
  fi
}
trap cleanup EXIT

start_local_setup_runtime_if_available() {
  if [[ "$PACK_HAS_SETUP_WEB_COMPONENT" != "true" || -n "${GREENTIC_SETUP_RUNTIME_URL:-}" ]]; then
    return
  fi
  if [[ "$PACK_HAS_SETUP_BACKEND_CONTRACT" == "true" && "$PACK_NEEDS_SETUP_RUNTIME" != "true" && "${GREENTIC_SETUP_USE_PYTHON_TESTER:-}" != "1" ]]; then
    echo "Setup backend: using greentic-setup backend contract"
    return
  fi
  if [[ "$PACK_HAS_SETUP_BACKEND_CONTRACT" == "true" && "$PACK_NEEDS_SETUP_RUNTIME" == "true" && "${GREENTIC_SETUP_USE_PYTHON_TESTER:-}" != "1" ]]; then
    echo "Setup backend: using greentic-setup backend contract with local runtime capabilities"
  fi

  case "$SETUP_PROVIDER_ID" in
    messaging-teams)
      local runtime_script="../greentic-messaging-providers/scripts/test_teams_bot.sh"
      if [[ ! -x "$runtime_script" ]]; then
        cat >&2 <<EOF
This provider declares a setup web component with provider-owned backend routes.

greentic-setup can serve the component assets, but this local checkout does not
include the Teams setup runtime script at:

  $runtime_script

Start the provider setup runtime, then rerun with:

  GREENTIC_SETUP_RUNTIME_URL=http://127.0.0.1:<runtime-port> scripts/test_provider.sh $PACK_PATH
EOF
        exit 2
      fi

      local runtime_port="${GREENTIC_SETUP_RUNTIME_PORT:-}"
      if [[ -z "$runtime_port" ]]; then
        for candidate in {8793..8893}; do
          if ! curl -fsS "http://127.0.0.1:${candidate}/api/state" >/dev/null 2>&1; then
            runtime_port="$candidate"
            break
          fi
        done
      elif curl -fsS "http://127.0.0.1:${runtime_port}/api/state" >/dev/null 2>&1; then
        cat >&2 <<EOF
Port $runtime_port is already serving a Teams setup runtime.

Stop the old tester first, or choose another port:

  GREENTIC_SETUP_RUNTIME_PORT=<free-port> scripts/test_provider.sh $PACK_PATH
EOF
        exit 2
      fi
      if [[ -z "$runtime_port" ]]; then
        echo "Could not find a free local setup runtime port in 8793..8893" >&2
        exit 1
      fi

      local runtime_work="${TMPDIR:-/tmp}/greentic-teams-bot-test-${runtime_port}"
      local runtime_log="/tmp/greentic-setup-${SETUP_PROVIDER_ID}-${runtime_port}.log"
      rm -rf "$runtime_work"
      echo "Setup runtime: auto-starting $SETUP_PROVIDER_ID on http://127.0.0.1:$runtime_port"
      GREENTIC_TEAMS_BOT_TENANT="$TENANT" \
        GREENTIC_TEAMS_BOT_TEAM="$TEAM" \
        PORT="$runtime_port" \
        "$runtime_script" --no-open >"$runtime_log" 2>&1 &
      SETUP_RUNTIME_PID=$!

      local ready=false
      for _ in {1..100}; do
        if curl -fsS "http://127.0.0.1:${runtime_port}/api/state" >/dev/null 2>&1; then
          ready=true
          break
        fi
        sleep 0.1
      done
      if [[ "$ready" != "true" ]]; then
        echo "Setup runtime did not become ready. Log: $runtime_log" >&2
        exit 1
      fi
      export GREENTIC_SETUP_RUNTIME_URL="http://127.0.0.1:${runtime_port}"
      ;;
    *)
      cat >&2 <<EOF
This provider declares a setup web component with provider-owned backend routes.

greentic-setup can serve the component assets, but no local setup runtime
launcher is known for provider "$SETUP_PROVIDER_ID".

Start the provider setup runtime, then rerun with:

  GREENTIC_SETUP_RUNTIME_URL=http://127.0.0.1:<runtime-port> scripts/test_provider.sh $PACK_PATH
EOF
      exit 2
      ;;
  esac
}

wait_for_browser_setup_success() {
  local setup_log="/tmp/greentic-setup-ui-$$.log"
  GREENTIC_SETUP_NO_OPEN=1 "${SETUP_CMD[@]}" "${SETUP_ARGS[@]}" >"$setup_log" 2>&1 &
  SETUP_UI_PID=$!

  local setup_url=""
  for _ in {1..200}; do
    if ! kill -0 "$SETUP_UI_PID" >/dev/null 2>&1; then
      echo "Setup UI exited before it became ready. Log:" >&2
      cat "$setup_log" >&2 || true
      exit 1
    fi
    setup_url="$(sed -n 's/^Setup UI started at: //p' "$setup_log" | tail -n 1)"
    if [[ -n "$setup_url" ]]; then
      break
    fi
    sleep 0.1
  done

  if [[ -z "$setup_url" ]]; then
    echo "Timed out waiting for setup UI URL. Log:" >&2
    cat "$setup_log" >&2 || true
    exit 1
  fi

  echo
  echo "Open this URL in your browser to complete provider setup:"
  echo "  $setup_url"
  echo
  echo "Waiting for the setup wizard to finish successfully..."

  while true; do
    if ! kill -0 "$SETUP_UI_PID" >/dev/null 2>&1; then
      echo "Setup UI exited before reporting success. Log:" >&2
      cat "$setup_log" >&2 || true
      exit 1
    fi

    local result_json
    result_json="$(curl -fsS "$setup_url/api/result" 2>/dev/null || true)"
    if printf '%s\n' "$result_json" | grep -q '"success"[[:space:]]*:[[:space:]]*true'; then
      echo "Setup wizard completed successfully."
      curl -fsS -X POST "$setup_url/api/shutdown" >/dev/null 2>&1 || true
      wait "$SETUP_UI_PID" || true
      SETUP_UI_PID=""
      return
    fi
    if printf '%s\n' "$result_json" | grep -q '"success"[[:space:]]*:[[:space:]]*false'; then
      echo "Setup wizard finished with an error:" >&2
      printf '%s\n' "$result_json" >&2
      curl -fsS -X POST "$setup_url/api/shutdown" >/dev/null 2>&1 || true
      wait "$SETUP_UI_PID" || true
      SETUP_UI_PID=""
      exit 1
    fi
    sleep 2
  done
}

echo "Provider pack: $PACK_ABS"
echo "Bundle dir:    $BUNDLE_DIR"
echo "Scope:         tenant=$TENANT team=$TEAM env=$ENV"
if [[ "$NO_UI" == "true" && "$PACK_HAS_SETUP_WEB_COMPONENT" == "true" ]]; then
  echo "UI:            manual browser (--no-ui)"
else
  UI_MODE="enabled"
  if [[ "$NO_UI" == "true" ]]; then
    UI_MODE="disabled (--no-ui)"
  fi
  echo "UI:            $UI_MODE"
fi
if [[ "$PACK_HAS_SETUP_WEB_COMPONENT" == "true" ]]; then
  echo "Setup UI:      provider web component"
fi
echo

start_local_setup_runtime_if_available

rm -rf "$BUNDLE_DIR"
"${SETUP_CMD[@]}" bundle init "$BUNDLE_DIR" --name "Provider Test"
"${SETUP_CMD[@]}" bundle add "$PACK_ABS" --bundle "$BUNDLE_DIR" --tenant "$TENANT" --team "$TEAM" --env "$ENV"

echo
SETUP_ARGS=(--tenant "$TENANT" --team "$TEAM" --env "$ENV")
if [[ "$NO_UI" == "true" && "$PACK_HAS_SETUP_WEB_COMPONENT" != "true" ]]; then
  SETUP_ARGS+=(--no-ui)
fi
SETUP_ARGS+=("$BUNDLE_DIR")

if [[ "$NO_UI" == "true" && "$PACK_HAS_SETUP_WEB_COMPONENT" == "true" ]]; then
  echo "Launching setup server without opening a browser..."
elif [[ "$NO_UI" == "true" ]]; then
  echo "Running setup without GUI..."
else
  echo "Launching setup GUI..."
fi
echo "Command: ${SETUP_CMD[*]} ${SETUP_ARGS[*]}"
if [[ "$NO_UI" == "true" && "$PACK_HAS_SETUP_WEB_COMPONENT" == "true" ]]; then
  wait_for_browser_setup_success
else
  "${SETUP_CMD[@]}" "${SETUP_ARGS[@]}"
fi

if [[ "$CLEANUP" == "false" ]]; then
  echo
  echo "Bundle left at: $BUNDLE_DIR"
fi
