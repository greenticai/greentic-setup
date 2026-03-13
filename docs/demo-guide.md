# Greentic Bundle Demo Guide

Complete guide for creating, configuring, and running Greentic bundles.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Step-by-Step Guide](#step-by-step-guide)
- [Answers File Format](#answers-file-format)
- [Provider-Specific Setup](#provider-specific-setup)
- [Advanced Usage](#advanced-usage)
- [Troubleshooting](#troubleshooting)

---

## Prerequisites

```bash
# Verify all tools are installed
gtc doctor
```

Expected output:
```
greentic-dev: OK (greentic-dev 0.4.x)
greentic-operator: OK (greentic-operator 0.4.x)
greentic-setup: OK (greentic-setup 0.4.x)
```

---

## Quick Start

### Simple Workflow (Recommended)

```bash
# 1. Create bundle with packs
gtc setup bundle init ./my-demo --name "My Demo"
gtc setup bundle add messaging-telegram.gtpack --bundle ./my-demo
gtc setup bundle add messaging-webchat.gtpack --bundle ./my-demo   # zero-config, instant GUI

# 2. Generate answers template
gtc setup --dry-run --emit-answers answers.json ./my-demo

# 3. Edit answers.json with your credentials

# 4. Run setup
gtc setup --answers answers.json ./my-demo

# 5. Start demo
GREENTIC_ENV=dev gtc op demo start --bundle ./my-demo --cloudflared off --nats off

# 6. Open WebChat GUI in browser (works immediately, no credentials needed)
#    http://localhost:8080/v1/messaging/webchat/demo/_/gui/
```

### One-Liner Examples

```bash
# Preview setup (dry-run)
gtc setup --dry-run ./my-demo

# Generate answers template
gtc setup --dry-run --emit-answers answers.json ./my-demo

# Apply answers and setup
gtc setup --answers answers.json ./my-demo

# For production .gtbundle files
gtc setup --answers answers.json ./my-bundle.gtbundle
```

---

## Step-by-Step Guide

### 1. Initialize Bundle

```bash
gtc setup bundle init ./telegram-demo --name "Telegram Demo"
```

**Bundle Structure:**
```
telegram-demo/
├── greentic.demo.yaml      # Demo configuration
├── packs/                  # Pack storage
├── providers/              # Provider configs
├── seeds/                  # Seed data
├── resolved/               # Resolved manifests
└── tenants/                # Tenant configs
```

### 2. Add Packs

```bash
# From local file
gtc setup bundle add ./greentic-messaging-providers/packs/messaging-telegram/dist/messaging-telegram.gtpack --bundle ./telegram-demo

# From OCI registry
gtc setup bundle add oci://ghcr.io/greentic-ai/packs/messaging-telegram:latest \
  --bundle ./telegram-demo
```

### 3. Generate Answers Template

```bash
gtc setup --dry-run --emit-answers answers.json ./telegram-demo
```

This creates `answers.json`:
```json
{
  "bundle_source": "./telegram-demo",
  "env": "dev",
  "tenant": "demo",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": false,
      "public_surface_policy": "disabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "",
      "public_base_url": ""
    }
  }
}
```

### 4. Configure Answers

Edit `answers.json`:
```json
{
  "bundle_source": "./telegram-demo",
  "env": "dev",
  "tenant": "demo",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": true,
      "public_base_url": "https://abc123.ngrok.io",
      "public_surface_policy": "enabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
      "public_base_url": "https://abc123.ngrok.io"
    }
  }
}
```

### 5. Run Setup

```bash
gtc setup --answers answers.json ./telegram-demo
```

### 6. Verify Bundle

```bash
gtc setup bundle status --bundle ./telegram-demo
```

### 7. Run Demo

```bash
GREENTIC_ENV=dev gtc op demo start --bundle ./telegram-demo --cloudflared off --nats off
```

### 8. Test WebChat (instant, zero-config)

If your bundle includes `messaging-webchat.gtpack`, open in browser:

```
http://localhost:8080/v1/messaging/webchat/demo/_/gui/
```

Type a message and see the response — no credentials or external setup needed. This is the fastest way to verify your bundle is working.

---

## Answers File Format

### JSON Format

```json
{
  "bundle_source": "./my-bundle",
  "env": "dev",
  "tenant": "demo",
  "team": "default",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": false,
      "public_surface_policy": "disabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "your-bot-token",
      "public_base_url": "https://your-domain.com"
    },
    "state-redis": {
      "redis_url": "redis://localhost:6379"
    }
  }
}
```

### YAML Format

```yaml
bundle_source: ./my-bundle
env: dev
tenant: demo
setup_answers:
  messaging-telegram:
    bot_token: "your-bot-token"
    public_base_url: "https://your-domain.com"
```

---

## Provider-Specific Setup

### Telegram

1. Create bot via [@BotFather](https://t.me/BotFather)
2. Get bot token
3. Setup public URL (ngrok/cloudflare tunnel)

```json
{
  "messaging-telegram": {
    "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
    "public_base_url": "https://abc123.ngrok.io"
  }
}
```

### Slack

```json
{
  "messaging-slack": {
    "bot_token": "xoxb-your-bot-token",
    "signing_secret": "your-signing-secret",
    "public_base_url": "https://your-domain.com"
  }
}
```

### Microsoft Teams

```json
{
  "messaging-teams": {
    "app_id": "your-app-id",
    "app_password": "your-app-password",
    "public_base_url": "https://your-domain.com"
  }
}
```

### WebChat (zero-config)

WebChat works out of the box — no credentials, no external services. Just add the pack to your bundle:

```bash
gtc setup bundle add messaging-webchat.gtpack --bundle ./my-demo
```

After `gtc op demo start`, the WebChat GUI is available at:

```
http://localhost:8080/v1/messaging/webchat/{tenant}/{team}/gui/
```

For default tenant/team:

```
http://localhost:8080/v1/messaging/webchat/demo/_/gui/
```

The operator runs a built-in Direct Line server — no Azure Bot Service or external token needed.

### Redis State

```json
{
  "state-redis": {
    "redis_url": "redis://localhost:6379",
    "redis_auth_enabled": false
  }
}
```

---

## Advanced Usage

### Bundle Subcommands

For advanced lifecycle management:

```bash
# Initialize bundle
gtc setup bundle init ./my-bundle --name "My Bundle"

# Add pack
gtc setup bundle add pack.gtpack --bundle ./my-bundle

# Setup specific provider
gtc setup bundle setup messaging-telegram --bundle ./my-bundle --answers telegram.json

# Update provider
gtc setup bundle update messaging-telegram --bundle ./my-bundle --answers telegram.json

# Remove provider
gtc setup bundle remove messaging-telegram --bundle ./my-bundle --force

# Build portable bundle
gtc setup bundle build --bundle ./my-bundle --out ./dist

# List packs
gtc setup bundle list --bundle ./my-bundle --domain messaging

# Show status
gtc setup bundle status --bundle ./my-bundle --format json
```

### Multi-Tenant Setup

```bash
gtc setup --tenant production --team ops --answers prod.json ./my-bundle
```

### CI/CD Integration

```bash
#!/bin/bash
set -e

# Create and setup bundle
gtc setup bundle init ./bundle --name "CI Demo"
gtc setup bundle add oci://ghcr.io/greentic-ai/packs/messaging-telegram:latest --bundle ./bundle

# Setup with environment-specific answers
gtc setup --answers ./config/${ENV}/answers.json ./bundle

# Build artifact
gtc setup bundle build --bundle ./bundle --out ./dist
```

### Demo: QA Complex Questions (Conditional Questions + Secret Masking)

greentic-setup supports full greentic-qa FormSpec features: conditional questions (`visible_if`), secret masking, visibility-aware validation, and visibility-aware persistence. This section shows how to demo these features.

#### What it demonstrates

| Feature | How it works |
|---------|-------------|
| **Conditional jumps** | Questions appear/disappear based on previous answers |
| **Secret masking** | `secret: true` fields use masked input (rpassword) |
| **Visibility-aware validation** | Required fields that are invisible are not validated |
| **Visibility-aware persistence** | Invisible answers are NOT written to secrets store |

#### Example: state-redis QA

The `state-redis` pack uses `visible_if` for auth and TLS questions:

```
redis_url              → always shown (required, secret)
redis_auth_enabled     → always shown (boolean toggle)
  └─ redis_password    → only if redis_auth_enabled = true (required, secret)
redis_tls_enabled      → always shown (boolean toggle)
  ├─ redis_tls_ca_cert   → only if redis_tls_enabled = true (required, secret)
  └─ redis_tls_skip_verify → only if redis_tls_enabled = true
key_prefix             → always shown
default_ttl_seconds    → always shown
connection_pool_size   → always shown
```

QA file (`state-redis-setup.json`):
```json
{
  "id": "redis_password",
  "kind": "String",
  "required": true,
  "secret": true,
  "visible_if": { "field": "redis_auth_enabled", "eq": "true" }
}
```

#### Step 1: Add state-redis pack to bundle

```bash
gtc setup bundle init ./qa-demo --name "QA Demo"
gtc setup bundle add demo-bundle/packs/state-redis.gtpack --bundle ./qa-demo
```

#### Step 2: Interactive wizard (shows conditional jumps + masking)

```bash
gtc setup ./qa-demo
```

Expected behavior:

```
Redis URL: ****                           ← masked (secret: true)
Enable authentication? [false]:           ← boolean toggle

  (if "true") Redis Password: ****        ← conditional + masked
  (if "false") skipped                    ← not asked, not persisted

Enable TLS? [false]:                      ← boolean toggle

  (if "true") TLS CA cert path: ****      ← conditional + masked
  (if "true") Skip TLS verify? [false]:   ← conditional
  (if "false") both skipped               ← not asked, not persisted

Key prefix [greentic]:
Default TTL (seconds) [0]:
Connection pool size [5]:
```

#### Step 3: Answers file (shows visibility-aware persistence)

```bash
# Generate template — ALL questions included
gtc setup --dry-run --emit-answers /tmp/qa-answers.json ./qa-demo
```

With auth disabled:
```json
{
  "setup_answers": {
    "state-redis": {
      "redis_url": "redis://localhost:6379",
      "redis_auth_enabled": "false",
      "redis_tls_enabled": "false",
      "key_prefix": "greentic"
    }
  }
}
```

```bash
gtc setup --answers /tmp/qa-answers.json ./qa-demo
```

Result: `redis_password`, `redis_tls_ca_cert`, `redis_tls_skip_verify` are **not persisted** to secrets store because their `visible_if` conditions are false.

With auth enabled:
```json
{
  "setup_answers": {
    "state-redis": {
      "redis_url": "redis://localhost:6379",
      "redis_auth_enabled": "true",
      "redis_password": "my-secret-pw",
      "redis_tls_enabled": "true",
      "redis_tls_ca_cert": "/etc/ssl/redis-ca.crt",
      "redis_tls_skip_verify": "false",
      "key_prefix": "greentic"
    }
  }
}
```

Result: all answers persisted including conditional ones.

#### Step 4: Verify secrets persistence

```bash
# Check which secrets were actually written
ls ~/.greentic/secrets/dev/demo/default/state-redis/
```

With auth disabled:
```
redis_url          ← written
key_prefix         ← written
```

With auth enabled:
```
redis_url          ← written
redis_password     ← written (conditional, visible)
redis_tls_ca_cert  ← written (conditional, visible)
key_prefix         ← written
```

#### Supported `visible_if` formats

Packs can use three formats for `visible_if`:

**Format 1 — Field equality (most common):**
```json
{ "visible_if": { "field": "redis_auth_enabled", "eq": "true" } }
```

**Format 2 — Truthy (field has any non-empty value):**
```json
{ "visible_if": { "field": "advanced_mode" } }
```

**Format 3 — Full qa-spec Expr (complex logic):**
```json
{
  "visible_if": {
    "op": "and",
    "expressions": [
      { "op": "eq", "left": { "op": "answer", "path": "auth_type" }, "right": { "op": "literal", "value": "oauth" } },
      { "op": "is_set", "path": "client_id" }
    ]
  }
}
```

Supported operators: `and`, `or`, `not`, `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `is_set`, `answer`, `literal`, `var`.

#### Talking points

- "Questions are dynamically shown/hidden based on previous answers — no static forms"
- "Secret fields use masked input — passwords never shown in terminal"
- "Only visible answers are persisted — invisible conditional fields don't leak to secrets store"
- "Pack authors define conditional logic in QA JSON — no code changes needed in the platform"
- "Same QA spec powers interactive wizard, answers file, and adaptive card setup"

---

## Troubleshooting

### Debug Mode

```bash
# Dry run
gtc setup --dry-run ./my-bundle

# With verbose output
gtc setup bundle setup --bundle ./my-bundle --answers answers.json --verbose
```

### Common Errors

| Error | Solution |
|-------|----------|
| `bundle not found` | Check bundle path exists |
| `failed to read answers file` | Check JSON/YAML syntax |
| `missing required field` | Add field to answers file |
| `interactive wizard not yet implemented` | Use `--answers <file>` |

### Reset Bundle

```bash
# Remove provider
gtc setup bundle remove messaging-telegram --bundle ./my-bundle --force

# Full reset
rm -rf ./my-bundle
gtc setup bundle init ./my-bundle --name "Fresh"
```

---

## Command Reference

### Simple Commands

| Command | Description |
|---------|-------------|
| `gtc setup <BUNDLE>` | Interactive wizard |
| `gtc setup --dry-run <BUNDLE>` | Preview without executing |
| `gtc setup --emit-answers <FILE> <BUNDLE>` | Generate answers template |
| `gtc setup --answers <FILE> <BUNDLE>` | Apply answers file |

### Bundle Subcommands

| Command | Description |
|---------|-------------|
| `gtc setup bundle init` | Initialize bundle |
| `gtc setup bundle add` | Add pack |
| `gtc setup bundle setup` | Run setup |
| `gtc setup bundle update` | Update config |
| `gtc setup bundle remove` | Remove provider |
| `gtc setup bundle build` | Build portable |
| `gtc setup bundle list` | List packs |
| `gtc setup bundle status` | Show status |

### Common Flags

| Flag | Description |
|------|-------------|
| `--dry-run` | Preview without changes |
| `--emit-answers <FILE>` | Generate template |
| `--answers <FILE>` | Apply answers |
| `--tenant <NAME>` | Tenant (default: demo) |
| `--team <NAME>` | Team (default: default) |
| `--env <NAME>` | Environment (default: dev) |

---

## Architecture & Design

### What greentic-setup does

greentic-setup is the **refactored setup engine** — extracted from the operator into a standalone crate. The operator no longer handles setup logic directly; it delegates everything to greentic-setup. This separation enables:

1. **CLI-driven setup** — `gtc setup ./bundle` for local development and CI/CD
2. **Admin API setup** — mTLS-secured endpoints for runtime bundle management
3. **Adaptive card setup** — setup via interactive cards in messaging channels

### End-to-end bundle setup flow

greentic-setup handles the complete bundle lifecycle:

```
Bundle Source → Discovery → QA Wizard → Secrets → Webhooks → Validate → Ready
```

1. **Resolve bundle source** — load from path, `.gtbundle` archive, or remote
2. **Discover packs** — scan for app packs, extension packs (messaging, events, state, telemetry, etc.)
3. **Run QA wizard** — interactive or from `--answers` file (reuses greentic-qa FormSpec)
4. **Persist secrets** — encrypted to dev store, with secret-name aliasing
5. **Register webhooks** — automatic for Telegram, Slack, Webex
6. **Validate bundle** — check all providers are loadable

### Bundle sources

Bundles can be loaded from multiple source types:

| Source | Example | Description |
|--------|---------|-------------|
| **Directory** (dev) | `./my-demo` | Local directory for development/testing |
| **Absolute path** | `/opt/bundles/prod.gtbundle` | Absolute path to bundle |
| **Relative path** | `../bundles/demo.gtbundle` | Relative path to bundle |
| **`.gtbundle` file** | `./my-bundle.gtbundle` | Portable archive (squashfs/zip) |
| **`file://`** | `file:///opt/bundles/demo.gtbundle` | Explicit file URI |
| **`oci://`** | `oci://ghcr.io/greentic-ai/bundles/demo:v1` | OCI registry (via greentic-distributor-client) |
| **`repo://`** | `repo://greentic/demo-bundle:latest` | Greentic repository (via greentic-distributor-client) [placeholder] |
| **`store://`** | `store://my-org/prod-bundle:v2` | Greentic store (via greentic-distributor-client) [placeholder] |

```bash
# Local directory (development)
gtc setup ./my-demo

# .gtbundle archive (portable deployment)
gtc setup --answers answers.json ./my-bundle.gtbundle

# OCI registry (CI/CD, production)
gtc setup --answers answers.json oci://ghcr.io/greentic-ai/bundles/demo:v1

# File URI
gtc setup --answers answers.json file:///opt/bundles/prod.gtbundle
```

### Answers workflow (`--dry-run` + `--emit-answers` + `--answers`)

The answers workflow reuses greentic-qa for question generation and validation:

```bash
# 1. Generate answers template (discovers packs → extracts QA questions → writes JSON)
gtc setup --dry-run --emit-answers answers.json ./my-bundle

# 2. Edit answers.json with your credentials

# 3. Apply answers (loads JSON → validates → persists secrets → registers webhooks)
gtc setup --answers answers.json ./my-bundle
```

The generated `answers.json` includes all discoverable questions from all packs:
```json
{
  "bundle_source": "./my-bundle",
  "env": "dev",
  "tenant": "demo",
  "greentic_setup_version": "1.0.0",
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "",
      "public_base_url": "",
      "default_chat_id": ""
    },
    "state-redis": {
      "redis_url": "",
      "redis_auth_enabled": false
    }
  }
}
```

### Operator delegation (greentic-operator → greentic-setup)

greentic-operator is a **passthrough** for all setup logic. It does not handle discovery, QA, or secrets persistence directly.

```
gtc setup ./bundle
  └─ greentic-setup (standalone binary)
       ├─ discovery: scan packs (app, messaging, events, state, telemetry, ...)
       ├─ QA: FormSpec questions (greentic-qa)
       ├─ secrets: persist to dev store (greentic-secrets-lib)
       ├─ webhooks: register with providers
       └─ validation: check bundle is loadable

gtc op demo start --bundle ./bundle
  └─ greentic-operator
       └─ greentic-start (runtime)
            ├─ loads pre-configured bundle (setup already done)
            ├─ starts HTTP ingress
            └─ runs WASM components
```

### Greentic-QA integration

greentic-setup fully supports greentic-qa FormSpec, including:

- **Conditional questions** — `visible_if` expressions with field equality, truthy checks, or full qa-spec `Expr` (And/Or/Not/Eq/Ne/Lt/Gt/IsSet)
- **Secret masking** — questions marked `secret: true` are masked in prompts and stored encrypted
- **Validation** — required field checks, format validation, conditional required (only when visible)
- **Visibility-aware persistence** — invisible/conditional questions whose conditions aren't met are skipped in secrets persistence

Example:
```json
{
  "id": "redis_password",
  "label": "Redis Password",
  "required": true,
  "secret": true,
  "visible_if": {"field": "redis_auth_enabled", "eq": "true"}
}
```

### i18n (greentic-i18n)

All user-facing strings in greentic-setup are fully internationalized via greentic-i18n:

- **66 languages** supported (en, id, ja, ar, de, fr, zh, ko, ...)
- CLI output, error messages, wizard prompts — all translated
- Source strings in `i18n/en.json`, translations auto-generated
- Locale detection: `--locale <BCP47>` flag, `$LANG` env, or system default

```bash
# Run in Indonesian
gtc setup --locale id ./my-bundle

# Translate missing strings (via tools/i18n.sh)
cd greentic-setup
tools/i18n.sh all          # translate + validate + status
tools/i18n.sh translate    # translate missing fields (Codex auto-translates)
```

### Setup capabilities status

| Capability | Status | Description |
|-----------|--------|-------------|
| Refactor setup from operator | Done | Standalone crate, 69 tests. Operator delegates all setup to greentic-setup |
| End-to-end bundle setup via CLI | Done | `gtc setup ./bundle` handles full lifecycle |
| Bundle sources (path, file://, .gtbundle) | Done | Local paths and .gtbundle archives |
| Bundle sources (oci://) | Done | Via greentic-distributor-client |
| Bundle sources (repo://, store://) | Placeholder | Via greentic-distributor-client (future) |
| `--answers` (auto-load answers) | Done | Reuses greentic-qa, JSON/YAML format |
| `--dry-run --emit-answers` | Done | Generates JSON template from discovered packs |
| Greentic-QA complex questions | Done | Conditional jumps, secret masking, visibility-aware persistence |
| Full i18n (greentic-i18n) | Done | 66 languages, `tools/i18n.sh` for auto-translation |
| Operator passthrough | Done | greentic-operator delegates to greentic-setup |
| Discovery of all pack types | Done | App packs, extension packs (messaging, events, state, telemetry, ...) |
| Admin endpoint (mTLS) | Types done | `http-ingress-admin` for runtime add/update/remove |
| Adaptive card setup | Types done | Setup via Adaptive Cards in messaging channels |

### Admin endpoint (runtime lifecycle)

The operator exposes an mTLS-secured admin endpoint for bundle lifecycle management at runtime. Runs on a separate port (default 8443) alongside the main HTTP ingress (8080).

#### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/admin/v1/health` | Health check |
| `GET` | `/admin/v1/status` | Bundle status (pack count, tenant count) |
| `POST` | `/admin/v1/deploy` | Deploy or update a bundle |
| `POST` | `/admin/v1/remove` | Remove providers from a bundle |
| `POST` | `/admin/v1/setup` | Run setup (QA + secrets + webhooks) for providers |

This enables:
- **Hot reload** — update a running operator without restart (diff-based)
- **CI/CD integration** — deploy bundles via API call
- **Multi-bundle** — manage multiple bundles on a single operator

#### Step 1: Generate TLS certificates

Create a CA, server cert, and client cert. All certs go in a single directory (e.g. `certs/`).

```bash
mkdir -p demo-bundle/certs && cd demo-bundle/certs

# 1. Create CA (Certificate Authority)
openssl genrsa -out ca.key 4096
openssl req -new -x509 -key ca.key -sha256 -days 3650 \
  -out ca.crt -subj "/CN=greentic-admin-ca"

# 2. Create server cert (for the admin endpoint)
openssl genrsa -out server.key 2048
openssl req -new -key server.key -out server.csr \
  -subj "/CN=localhost"
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key \
  -CAcreateserial -out server.crt -days 365 -sha256

# 3. Create client cert (for CLI/CI callers)
openssl genrsa -out client.key 2048
openssl req -new -key client.key -out client.csr \
  -subj "/CN=deploy-bot"
openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key \
  -CAcreateserial -out client.crt -days 365 -sha256

# Cleanup CSRs
rm -f *.csr *.srl

cd ../..
```

Expected `certs/` directory:
```
demo-bundle/certs/
├── ca.crt          # CA certificate (shared)
├── ca.key          # CA private key (keep secure)
├── server.crt      # Server certificate
├── server.key      # Server private key
├── client.crt      # Client certificate
└── client.key      # Client private key
```

#### Step 2: Start operator with admin endpoint

```bash
GREENTIC_ENV=dev gtc op demo start \
  --bundle demo-bundle \
  --admin \
  --admin-port 8443 \
  --admin-certs-dir demo-bundle/certs \
  --admin-allowed-clients "deploy-bot" \
  --cloudflared off --nats off
```

Expected log output:
```
admin API listening on https://127.0.0.1:8443 (mTLS)
HTTP ingress ready at http://127.0.0.1:8080
demo start running (config=... tenant=demo team=default); press Ctrl+C to stop
```

**CLI flags:**

| Flag | Default | Description |
|------|---------|-------------|
| `--admin` | off | Enable the mTLS admin endpoint |
| `--admin-port` | `8443` | Port for the admin endpoint |
| `--admin-certs-dir` | `<bundle>/certs` | Directory with `server.crt`, `server.key`, `ca.crt` |
| `--admin-allowed-clients` | (all) | Comma-separated client CNs (empty = allow any valid cert) |

#### Step 3: Call admin API

All requests require client cert + key + CA cert.

**Health check:**

```bash
curl -s --cert demo-bundle/certs/client.crt \
  --key demo-bundle/certs/client.key \
  --cacert demo-bundle/certs/ca.crt \
  https://localhost:8443/admin/v1/health
```

Response:
```json
{"ok": true, "data": "healthy"}
```

**Bundle status:**

```bash
curl -s --cert demo-bundle/certs/client.crt \
  --key demo-bundle/certs/client.key \
  --cacert demo-bundle/certs/ca.crt \
  https://localhost:8443/admin/v1/status | jq
```

Response:
```json
{
  "ok": true,
  "data": {
    "bundle_path": "/path/to/demo-bundle",
    "status": "active",
    "pack_count": 5,
    "tenant_count": 1,
    "provider_count": 5
  }
}
```

**Run setup for a tenant:**

```bash
curl -s --cert demo-bundle/certs/client.crt \
  --key demo-bundle/certs/client.key \
  --cacert demo-bundle/certs/ca.crt \
  -X POST https://localhost:8443/admin/v1/setup \
  -H "Content-Type: application/json" \
  -d '{
    "tenant": "demo",
    "team": "default",
    "answers": {
      "messaging-telegram": {
        "bot_token": "123456:ABC-DEF",
        "public_base_url": "https://example.com"
      }
    }
  }' | jq
```

Response:
```json
{"ok": true, "data": {"setup": true}}
```

**Dry-run setup (preview steps):**

```bash
curl -s --cert demo-bundle/certs/client.crt \
  --key demo-bundle/certs/client.key \
  --cacert demo-bundle/certs/ca.crt \
  -X POST https://localhost:8443/admin/v1/setup \
  -H "Content-Type: application/json" \
  -d '{"tenant": "demo", "dry_run": true}' | jq
```

Response:
```json
{
  "ok": true,
  "data": {
    "dry_run": true,
    "steps": [
      "discover packs in bundle",
      "resolve messaging-telegram",
      "persist secrets for messaging-telegram"
    ]
  }
}
```

**Deploy bundle with packs:**

```bash
curl -s --cert demo-bundle/certs/client.crt \
  --key demo-bundle/certs/client.key \
  --cacert demo-bundle/certs/ca.crt \
  -X POST https://localhost:8443/admin/v1/deploy \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/demo-bundle",
    "pack_refs": ["messaging-telegram.gtpack"],
    "tenants": [{"tenant": "demo", "team": "default", "allow_paths": []}],
    "dry_run": false
  }' | jq
```

**Remove providers:**

```bash
curl -s --cert demo-bundle/certs/client.crt \
  --key demo-bundle/certs/client.key \
  --cacert demo-bundle/certs/ca.crt \
  -X POST https://localhost:8443/admin/v1/remove \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/demo-bundle",
    "providers": ["messaging-telegram"],
    "tenants": [{"tenant": "demo", "team": "default", "allow_paths": []}],
    "dry_run": false
  }' | jq
```

#### Step 4: CI/CD integration example

```bash
#!/bin/bash
set -e

CERTS_DIR="./certs"
ADMIN_URL="https://operator.internal:8443"

# Deploy bundle
curl -sf --cert "$CERTS_DIR/client.crt" \
  --key "$CERTS_DIR/client.key" \
  --cacert "$CERTS_DIR/ca.crt" \
  -X POST "$ADMIN_URL/admin/v1/deploy" \
  -H "Content-Type: application/json" \
  -d @deploy-request.json

# Run setup with answers
curl -sf --cert "$CERTS_DIR/client.crt" \
  --key "$CERTS_DIR/client.key" \
  --cacert "$CERTS_DIR/ca.crt" \
  -X POST "$ADMIN_URL/admin/v1/setup" \
  -H "Content-Type: application/json" \
  -d @setup-request.json

# Verify
curl -sf --cert "$CERTS_DIR/client.crt" \
  --key "$CERTS_DIR/client.key" \
  --cacert "$CERTS_DIR/ca.crt" \
  "$ADMIN_URL/admin/v1/status" | jq '.data.pack_count'
```

#### Security notes

- **mTLS required** — both server and client must present valid certificates signed by the same CA
- **CN-based access control** — `--admin-allowed-clients` restricts which client CNs can connect (empty = any valid cert)
- **Localhost only** — admin endpoint binds to `127.0.0.1` by default (not exposed externally)
- **Separate port** — admin traffic is isolated from application traffic on port 8080
- **No credentials in URL** — all auth is via TLS certificates, no tokens or passwords in requests

### Adaptive card setup (nice-to-have)

Setup can be done via Adaptive Cards rendered in messaging channels (Teams, WebChat, Webex). The flow:

1. Operator generates a **setup link** with a one-time token
2. User opens the link in a messaging channel
3. Adaptive Cards render setup questions with conditional visibility
4. Answers are submitted back to the operator via secure callback
5. Secrets are persisted and webhooks registered

Security considerations:
- One-time token with expiry (prevents replay)
- mTLS on the admin callback endpoint
- Card actions include HMAC signature verification
- Setup link can be scoped to specific providers/tenants
