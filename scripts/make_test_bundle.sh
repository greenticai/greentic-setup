#!/usr/bin/env bash
# Build a deployable test bundle for the "default bundle on entry" feature:
# a bundle carrying the fixed default-welcome pack (and optionally a messaging
# provider) so "user enters -> welcome" can be tested end-to-end with greentic-start.
#
# Usage: scripts/make_test_bundle.sh [--provider <pack.gtpack>] [--out <dir>]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "${ROOT}"
export PATH="${HOME}/.cargo/bin:${PATH}"

PROVIDER=""; OUT="/tmp/welcome-test.gtbundle"
while [ $# -gt 0 ]; do case "$1" in
  --provider) shift; PROVIDER="${1:-}";;
  --out) shift; OUT="${1:-}";;
  *) echo "unknown arg: $1" >&2; exit 2;;
esac; shift || true; done

rm -rf "${OUT}"
greentic-setup bundle init "${OUT}" --tenant demo --name welcome-test --non-interactive >/dev/null
# Ship the fixed default-welcome pack explicitly (an installed greentic-setup may
# still embed an older asset); this is the app that greets users on entry.
mkdir -p "${OUT}/packs"; cp -f assets/default-welcome.gtpack "${OUT}/packs/default.gtpack"
[ -n "${PROVIDER}" ] && greentic-setup bundle add "${PROVIDER}" --bundle "${OUT}" --tenant demo --non-interactive >/dev/null

echo "bundle: ${OUT}"
greentic-pack doctor --archive "${OUT}/packs/default.gtpack" 2>/dev/null | grep -iE 'Flows list' -A1 | sed 's/^/  /'
[ -n "${PROVIDER}" ] && echo "  provider: $(basename "${PROVIDER}")" || echo "  provider: (none — add one with --provider)"
echo ""
echo "run it:  greentic-start start --bundle ${OUT} --tenant demo --no-browser"
echo "  then send an entry to the provider ingress -> the welcome flow runs on entry."
