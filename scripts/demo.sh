#!/bin/bash
# Greentic Setup Demo Script
# Use Case: Deploy WebChat Customer Support Bot
#
# Run: ./scripts/demo.sh
# Or:  ./scripts/demo.sh --no-pause (for recording)

set -e
cd "$(dirname "$0")/.."

DEMO_DIR="/tmp/gtc-demo"
WEBCHAT_PACK="${WEBCHAT_PACK:-../demo-bundle-local/providers/messaging/messaging-webchat.gtpack}"

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# Check for --no-pause flag
NO_PAUSE=false
[[ "$1" == "--no-pause" ]] && NO_PAUSE=true

header() {
    echo ""
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${GREEN}▶ $1${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

cmd() {
    echo -e "${CYAN}\$ $1${NC}"
}

pause() {
    if [[ "$NO_PAUSE" == "false" ]]; then
        echo ""
        echo -e "${YELLOW}Press Enter to continue...${NC}"
        read -r
    fi
}

# ─────────────────────────────────────────────────────────────────────────────
header "USE CASE: Deploy WebChat Customer Support Bot"
echo ""
echo "Scenario: DevOps engineer deploying a chatbot to production"
echo "          with a WebChat widget on the company website."
echo ""
echo "Tools needed: gtc (Greentic CLI)"
pause

# ─────────────────────────────────────────────────────────────────────────────
# Clean up
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"

# ─────────────────────────────────────────────────────────────────────────────
header "Step 1: Initialize Bundle"
cmd 'gtc setup bundle init ./support-bot --name "Support Bot"'
echo ""
gtc setup bundle init "$DEMO_DIR/support-bot" --name "Support Bot"
pause

# ─────────────────────────────────────────────────────────────────────────────
header "Step 2: Add WebChat Provider"
cmd 'gtc setup bundle add ./messaging-webchat.gtpack --bundle ./support-bot'
echo ""
if [[ -f "$WEBCHAT_PACK" ]]; then
    gtc setup bundle add "$WEBCHAT_PACK" --bundle "$DEMO_DIR/support-bot"
else
    echo "(Skipped - pack not found at $WEBCHAT_PACK)"
    echo "Set WEBCHAT_PACK env var to provide a valid .gtpack path"
fi
pause

# ─────────────────────────────────────────────────────────────────────────────
header "Step 3: Generate Answers Template"
cmd 'gtc setup --dry-run --emit-answers answers.json ./support-bot'
echo ""
gtc setup --dry-run --emit-answers "$DEMO_DIR/answers.json" "$DEMO_DIR/support-bot"
echo ""
echo "Generated template:"
cat "$DEMO_DIR/answers.json"
pause

# ─────────────────────────────────────────────────────────────────────────────
header "Step 4: Edit Answers (simulate filling credentials)"
echo ""
cat > "$DEMO_DIR/answers.json" << 'EOF'
{
  "bundle_source": "./support-bot",
  "env": "production",
  "tenant": "acme-corp",
  "team": "support",
  "setup_answers": {
    "messaging-webchat": {
      "public_base_url": "https://support.acme-corp.com",
      "jwt_signing_key": "super-secret-jwt-key-2024"
    }
  }
}
EOF
echo "Filled answers.json:"
cat "$DEMO_DIR/answers.json"
pause

# ─────────────────────────────────────────────────────────────────────────────
header "Step 5: Apply Answers"
cmd 'gtc setup --answers answers.json ./support-bot'
echo ""
gtc setup --answers "$DEMO_DIR/answers.json" "$DEMO_DIR/support-bot"
pause

# ─────────────────────────────────────────────────────────────────────────────
header "Step 6: Build Portable .gtbundle"
cmd 'gtc setup bundle build --bundle ./support-bot --out ./support-bot.gtbundle'
echo ""
gtc setup bundle build --bundle "$DEMO_DIR/support-bot" --out "$DEMO_DIR/support-bot.gtbundle"
echo ""
echo "Result:"
ls -lh "$DEMO_DIR/support-bot.gtbundle"
pause

# ─────────────────────────────────────────────────────────────────────────────
header "Step 7: Deploy to Production Server"
echo ""
echo "On production server, only need 2 files:"
echo "  - support-bot.gtbundle (portable archive)"
echo "  - answers.json (from secrets manager)"
echo ""
cmd 'gtc setup --answers answers.json ./support-bot.gtbundle'
echo ""
gtc setup --answers "$DEMO_DIR/answers.json" "$DEMO_DIR/support-bot.gtbundle"
pause

# ─────────────────────────────────────────────────────────────────────────────
header "DEMO COMPLETE"
echo ""
echo "Summary:"
echo ""
echo "  Developer workflow:"
echo "    1. gtc setup bundle init ./my-bot"
echo "    2. gtc setup bundle add ./provider.gtpack --bundle ./my-bot"
echo "    3. gtc setup --dry-run --emit-answers answers.json ./my-bot"
echo "    4. # Edit answers.json"
echo "    5. gtc setup --answers answers.json ./my-bot"
echo "    6. gtc setup bundle build --out ./my-bot.gtbundle"
echo ""
echo "  CI/CD deployment:"
echo "    gtc setup --answers answers.json ./my-bot.gtbundle"
echo "    gtc op demo start --bundle ./my-bot.gtbundle"
echo ""
echo "Files created in $DEMO_DIR:"
ls -lh "$DEMO_DIR"
