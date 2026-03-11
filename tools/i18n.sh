#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# greentic-i18n repo path (sibling directory)
I18N_REPO="${I18N_REPO:-../greentic-i18n}"

AUTH_MODE="${AUTH_MODE:-auto}"
LOCALE="${LOCALE:-en}"
EN_PATH="${EN_PATH:-i18n/en.json}"

usage() {
  cat <<'USAGE'
Usage: tools/i18n.sh [translate|validate|status|all|seed]

Commands:
  translate   Generate translations for all languages
  validate    Validate placeholder/backtick/newline rules
  status      Check staleness and missing keys
  all         Run translate + validate + status
  seed        Create empty locale files for all target languages

Environment overrides:
  I18N_REPO=...    Path to greentic-i18n repo (default: ../greentic-i18n)
  EN_PATH=...      English source file path (default: i18n/en.json)
  AUTH_MODE=...    Translator auth mode for translate (default: auto)
  LOCALE=...       CLI locale used for output (default: en)

Examples:
  tools/i18n.sh all
  AUTH_MODE=api-key tools/i18n.sh translate
  tools/i18n.sh seed
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

MODE="${1:-all}"

# Target languages (66 total)
LANGUAGES=(
  ar ar-AE ar-DZ ar-EG ar-IQ ar-MA ar-SA ar-SD ar-SY ar-TN
  ay bg bn cs da de el en-GB es et fa fi fr gn gu hi hr ht hu
  id it ja km kn ko lo lt lv ml mr ms my nah ne nl no pa pl pt
  qu ro ru si sk sr sv ta te th tl tr uk ur vi zh
)

check_i18n_repo() {
  if [[ ! -d "$I18N_REPO" ]]; then
    echo "Error: greentic-i18n repo not found at $I18N_REPO" >&2
    echo "Set I18N_REPO to the correct path" >&2
    exit 1
  fi
}

run_translator() {
  local cmd="$1"
  shift
  (cd "$I18N_REPO" && cargo run -p greentic-i18n-translator -- \
    --locale "$LOCALE" \
    "$cmd" --en "$ROOT_DIR/$EN_PATH" "$@")
}

run_translate() {
  echo "==> translate: $EN_PATH"
  check_i18n_repo
  run_translator translate --langs all --auth-mode "$AUTH_MODE"
}

run_validate() {
  echo "==> validate: $EN_PATH"
  check_i18n_repo
  run_translator validate --langs all
}

run_status() {
  echo "==> status: $EN_PATH"
  check_i18n_repo
  run_translator status --langs all
}

run_seed() {
  echo "==> seed: Creating locale files for ${#LANGUAGES[@]} languages"
  mkdir -p "$ROOT_DIR/i18n"

  for lang in "${LANGUAGES[@]}"; do
    local file="$ROOT_DIR/i18n/$lang.json"
    if [[ ! -f "$file" ]]; then
      echo "{}" > "$file"
      echo "  Created: $file"
    fi
  done

  echo "Done. Run 'tools/i18n.sh translate' to generate translations."
}

case "$MODE" in
  translate)
    run_translate
    ;;
  validate)
    run_validate
    ;;
  status)
    run_status
    ;;
  seed)
    run_seed
    ;;
  all)
    run_translate
    run_validate
    run_status
    ;;
  *)
    echo "Unknown mode: $MODE" >&2
    usage
    exit 2
    ;;
esac
