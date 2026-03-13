# Demo Checklist - Greentic Setup

## Pre-Demo Setup

### 1. Merge gtc setup passthrough (jika belum)
```bash
cd greentic
git checkout main
git merge feat/gtc-setup-passthrough
git push
```

### 2. Install latest tools
```bash
# Install greentic-setup
cargo install --path greentic-setup

# Install gtc (dengan setup passthrough)
cargo install --path greentic

# Verify
gtc doctor
```

### 3. Prepare Telegram Bot
- [ ] Buat bot di @BotFather (atau pakai existing)
- [ ] Catat bot token: `123456789:ABCdefGHI...`
- [ ] Test bot bisa diakses: https://t.me/YOUR_BOT

### 4. Setup Tunnel (ngrok/cloudflare)
```bash
# Ngrok
ngrok http 8080

# Atau cloudflare tunnel
cloudflared tunnel --url http://localhost:8080
```
- [ ] Catat public URL: `https://xxx.ngrok.io`

### 5. Prepare Telegram Pack
```bash
# Check pack exists
ls -la ../greentic-messaging-providers/dist/messaging-telegram.gtpack

# Atau build fresh
cd ../greentic-messaging-providers
cargo build --release
```

---

## Demo Flow

### Part 1: Simple Mode (5 menit)

```bash
# 1. Show help
greentic-setup --help

# 2. Initialize bundle
greentic-setup bundle init /tmp/telegram-demo --name "Telegram Demo"

# 3. Add pack
greentic-setup bundle add ../greentic-messaging-providers/dist/messaging-telegram.gtpack \
  --bundle /tmp/telegram-demo

# 4. Generate answers template
greentic-setup --dry-run --emit-answers /tmp/answers.json /tmp/telegram-demo

# 5. Show template
cat /tmp/answers.json

# 6. Fill in credentials (edit file)

# 7. Apply answers
greentic-setup --answers /tmp/answers.json /tmp/telegram-demo

# 8. Show status
greentic-setup bundle status --bundle /tmp/telegram-demo
```

### Part 2: Interactive Wizard (3 menit)

```bash
# Interactive mode (tanpa --answers)
greentic-setup /tmp/telegram-demo

# Shows:
# - Found 1 provider(s) to configure
# - Prompts for each question
```

### Part 3: gtc Passthrough (2 menit)

```bash
# Same commands via gtc
gtc setup --help
gtc setup bundle init /tmp/gtc-demo --name "GTC Demo"
gtc setup bundle status --bundle /tmp/gtc-demo
```

### Part 4: i18n Support (2 menit)

```bash
# Show Indonesian
LANG=id greentic-setup --help

# Show Japanese
LANG=ja greentic-setup --help

# Show Arabic
LANG=ar greentic-setup --help
```

### Part 5: Build & Deploy (3 menit)

```bash
# Build portable .gtbundle
greentic-setup bundle build --bundle /tmp/telegram-demo --out /tmp/telegram-demo.gtbundle

# Show result
ls -lh /tmp/telegram-demo.gtbundle

# Deploy from .gtbundle
greentic-setup --answers /tmp/answers.json /tmp/telegram-demo.gtbundle
```

### Part 6: Run with Operator (5 menit)

```bash
# Start demo with operator
gtc op demo start --bundle /tmp/telegram-demo

# Test Telegram bot
# Send message to bot, see response
```

---

## Demo Script (Automated)

```bash
# Run full demo with pauses
./scripts/demo.sh

# Run without pauses (for recording)
./scripts/demo.sh --no-pause
```

---

## Backup Commands (jika ada masalah)

```bash
# Reset bundle
rm -rf /tmp/telegram-demo
greentic-setup bundle init /tmp/telegram-demo

# Direct binary (tanpa gtc)
./target/release/greentic-setup --help

# Check logs
tail -f /tmp/telegram-demo/logs/*.log
```

---

## Key Points untuk Presentasi

1. **Simple Mode** - Cukup `gtc setup ./bundle`, tidak perlu subcommand
2. **Interactive Wizard** - Tanpa --answers, wizard akan prompt
3. **66 Languages** - Full i18n support
4. **gtc Integration** - Unified CLI experience via passthrough
5. **Portable Bundles** - .gtbundle untuk deployment
6. **CI/CD Ready** - answers.json dari secrets manager
7. **Refactored from Operator** - Setup logic extracted ke standalone crate (69 tests)
8. **Greentic-QA Complex Questions** - Conditional jumps (`visible_if`), secret masking, nested expressions
9. **Admin Endpoint (mTLS)** - Runtime bundle lifecycle: deploy, setup, update, remove via API
10. **Adaptive Card Setup** - Setup via interactive cards di messaging channels (nice-to-have)

### Demo Narrative: Setup Evolution

```
OLD WAY (operator-embedded):
  gtc op demo wizard --answers answers.json --bundle ./bundle --execute
  ↓ tightly coupled to operator, hard to test, no reuse

NEW WAY (standalone greentic-setup):
  gtc setup --answers answers.json ./bundle        # CLI setup
  POST /admin/v1/bundle/deploy                     # API setup (mTLS)
  [Adaptive Card in Teams/WebChat]                 # Card setup (future)
```

### Key Architecture Slides

1. **Setup extracted from operator** → reusable by CLI, admin API, card flows
2. **Greentic-QA FormSpec** → complex question sets with conditional visibility, secret masking
3. **Admin endpoint** → mTLS-secured HTTP for runtime bundle add/update/remove
4. **Hot reload** → diff-based detection, in-place provider swap without restart
5. **Adaptive card setup** → secure one-time token + card rendering in any messaging channel
