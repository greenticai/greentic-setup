#!/usr/bin/env bash
# e2e: the default welcome pack must render its welcome card when the `default`
# flow runs — this is what greentic-start invokes when a user enters a channel
# on a bundle with no custom app (route_messaging_envelopes -> select_app_flow).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"
export PATH="${HOME}/.cargo/bin:${PATH}"

PACK="assets/default-welcome.gtpack"
[ -f "${PACK}" ] || { echo "FAIL: ${PACK} missing"; exit 1; }

echo "running default flow via greentic-runner-cli…"
OUT="$(greentic-runner-cli --pack "${PACK}" --flow default 2>&1)"

python3 - "$OUT" <<'PY'
import sys
out = sys.argv[1]
ok = '"status": "Success"' in out or '"status":"Success"' in out
node_ok = "ai.greentic.component-adaptive-card" in out and ('"status": "Ok"' in out or '"status":"Ok"' in out)
print(f"  flow status Success      : {ok}")
print(f"  welcome card node status : {'Ok' if node_ok else 'NOT Ok'}")
if ok and node_ok:
    print("PASS: default welcome flow runs and the adaptive-card welcome node renders OK")
    sys.exit(0)
print("FAIL: default welcome flow did not run cleanly")
print(out[:800])
sys.exit(1)
PY
