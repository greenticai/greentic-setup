#!/bin/bash
# Verify that every t('ui.*') call in the frontend has a matching entry in i18n/en.json.
# Exit code 0 = all keys covered; 1 = missing keys printed to stderr.

set -euo pipefail

EN_JSON="${EN_JSON:-i18n/en.json}"
FRONTEND_DIR="${FRONTEND_DIR:-assets/setup-ui-v2}"

if [ ! -f "$EN_JSON" ]; then
  echo "missing $EN_JSON" >&2
  exit 2
fi

# Extract all keys used by t() calls in the frontend (HTML + JS)
used_keys=$(grep -rohE "t\((['\"])(ui\.[a-z0-9._]+)\1" "$FRONTEND_DIR" \
  | grep -oE "ui\.[a-z0-9._]+" \
  | sort -u)

# Extract all keys defined in en.json
defined_keys=$(python3 -c "
import json
with open('$EN_JSON') as f:
    d = json.load(f)
for k in sorted(d.keys()):
    if k.startswith('ui.'):
        print(k)
")

missing=""
for key in $used_keys; do
  if ! echo "$defined_keys" | grep -qx "$key"; then
    missing="$missing $key"
  fi
done

if [ -n "$missing" ]; then
  echo "i18n coverage check failed — missing keys:" >&2
  for k in $missing; do
    echo "  $k" >&2
  done
  exit 1
fi

echo "i18n coverage: all used ui.* keys defined in $EN_JSON"
